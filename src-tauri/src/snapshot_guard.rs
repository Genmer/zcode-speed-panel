//! 快照防护：阻断 ZCode 工作区快照的静默上传（chflags uchg 目录不可变锁）。
//!
//! ## 背景（2026-09 本机验证）
//!
//! ZCode 登录后会把**整个工作区**（含 `.git/` 全历史）打成加密 tar.gz 写入
//! `~/.zcode/v2/checkpoints/<工作区hash>/pending/*.tar.gz.enc`，再经
//! zcode.z.ai 拿凭证直传阿里云 OSS；设置开关无效，且凭证 API 与模型 API
//! 同域，**不能靠封网络解决**。清空该目录后对目录本身 `chflags uchg`
//! （macOS 用户级不可变标志，用户自有目录无需 sudo）即可让 ZCode 写不进
//! 去——快照链路死亡，而模型对话/补全/工具调用完全正常；唯一损失是
//! 「检查点回滚 / 时间线」。`chflags nouchg` 随时可逆，目录留空时 ZCode
//! 会自动重建内容。（机制来源：ferster 博客《ZCode 静默上传工作区快照》）
//!
//! ## 实现要点
//!
//! - **不碰网络、不碰进程**：只做目录文件系统操作（remove/create/chflags，
//!   `std::process::Command` 调系统 chflags），对运行中的 ZCode 无侵入；
//! - **锁定检测 = 写入探测**：在目录里 create+delete 临时文件，创建失败
//!   即已锁。纯 std 实现，比解析 `ls -lO` / libc `st_flags` 干净；
//! - **知情同意在前端**（`#guard-confirm` 确认弹窗必须明示损失检查点回滚，
//!   见 key-rules #16）；apply/release 收到调用即执行、不再二次确认；
//! - **防护计数**：锁定时刻与 calls_today 基线持久化在
//!   `~/.zcode/speed-panel-guard.json`；poller 每拍按 calls 增量累计
//!   `blocked_rounds`（calls_today 跨天回退按 0 增量重置基准拍）；
//! - **目录已锁但无记录**（用户看过文档后手动 chflags / 重装面板）：首拍
//!   探测到即补记基线，从该时刻起算轮次；
//! - **仅 macOS**：Windows 无等价的用户级不可变标志，`supported=false`，
//!   apply/release 返回中文错误，前端按钮禁用并如实标注。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// chflags 仅 macOS（其他平台 apply/release 拒绝执行）
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// checkpoints 目录扫描节流（poller ~700ms 一拍，不必每拍走文件系统）
const SCAN_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// 每拍随 metrics payload 推送前端的防护状态（serde camelCase）
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotGuardStatus {
    /// 本平台是否支持文件锁（chflags 仅 macOS；false 时前端禁用按钮）
    pub supported: bool,
    /// checkpoints 目录当前是否被锁定（写入探测失败）
    pub locked: bool,
    /// 锁定时刻（guard.json，epoch ms）；目录已锁但无记录时由 poller 补记
    pub locked_since_ms: Option<i64>,
    /// 防护开启后经过的对话轮次（poller 按 calls_today 增量累计）
    pub blocked_rounds: u64,
    /// 已积累工件数（`**/pending/*.enc`）
    pub artifact_count: u64,
    /// 工件总体积（字节）
    pub artifact_bytes: u64,
    /// 工作区目录数
    pub workspace_count: u64,
    /// Σ failureCount（ZCode 自己记录的上传失败计数）
    pub failure_count: u64,
}

/// 单工作区 state.json 里防护关心的字段（解析纯函数的输出）
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StateSummary {
    pub failure_count: u64,
}

/// checkpoints 目录扫描摘要
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ScanSummary {
    pub artifact_count: u64,
    pub artifact_bytes: u64,
    pub workspace_count: u64,
    pub failure_count: u64,
}

/// state.json → 摘要（纯函数，可测）：损坏 JSON / 非对象返回 None（调用方
/// 跳过该工作区的 failure 计数，工件与目录数仍如实统计）；failureCount
/// 缺失按 0
pub(crate) fn parse_state_summary(json: &str) -> Option<StateSummary> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if !v.is_object() {
        return None;
    }
    Some(StateSummary {
        failure_count: v.get("failureCount").and_then(|x| x.as_u64()).unwrap_or(0),
    })
}

/// 聚合（纯函数，可测）：逐工作区（state 解析结果 + pending 工件大小列表）
/// → 总摘要。state 损坏（None）的工作区仍计入工作区数/工件数/体积，
/// 只是不贡献 failureCount
pub(crate) fn summarize_scans(scans: &[(Option<StateSummary>, Vec<u64>)]) -> ScanSummary {
    let mut s = ScanSummary::default();
    for (state, enc_sizes) in scans {
        s.workspace_count += 1;
        for sz in enc_sizes {
            s.artifact_count += 1;
            s.artifact_bytes += *sz;
        }
        if let Some(st) = state {
            s.failure_count += st.failure_count;
        }
    }
    s
}

/// guard.json（~/.zcode/speed-panel-guard.json）：锁定时刻 + calls 基线 +
/// 累计轮次。锁定三字段缺省即"未防护"，不落盘多余键
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct GuardFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    locked_since_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calls_baseline: Option<u64>,
    #[serde(default)]
    blocked_rounds: u64,
}

fn checkpoints_dir() -> Option<PathBuf> {
    crate::metrics::home_dir().map(|h| h.join(".zcode").join("v2").join("checkpoints"))
}

fn guard_file_path() -> Option<PathBuf> {
    crate::metrics::home_dir().map(|h| h.join(".zcode").join("speed-panel-guard.json"))
}

/// 损坏/缺失回默认（未防护），不报错——状态探测按目录实际锁定为准
fn load_guard_file() -> GuardFile {
    let Some(path) = guard_file_path() else { return GuardFile::default() };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_guard_file(f: &GuardFile) {
    if let Some(path) = guard_file_path() {
        if let Err(e) = std::fs::write(&path, serde_json::to_string(f).unwrap_or_default()) {
            eprintln!("[zcode-speed-panel] guard 状态落盘失败: {e}");
        }
    }
}

/// chflags uchg/nouchg（std::process::Command，用户自有目录无需 sudo）。
/// 仅在 SUPPORTED 平台被调用
fn set_immutable(dir: &Path, lock: bool) -> Result<(), String> {
    let flag = if lock { "uchg" } else { "nouchg" };
    let st = std::process::Command::new("chflags")
        .arg(flag)
        .arg(dir)
        .status()
        .map_err(|e| format!("执行 chflags 失败: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("chflags {flag} 未成功（exit {:?}）", st.code()))
    }
}

/// 写入探测：目录存在且无法在其中创建临时文件 = 已锁（uchg 阻止在目录内
/// 新建条目）。探测文件随即删除；目录不存在 = 未锁
fn probe_locked(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(".speed-panel-lock-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

/// 扫描 checkpoints：每工作区目录读 state.json（损坏跳过）+ pending/*.enc
/// 文件大小，聚合走纯函数 summarize_scans
fn scan_checkpoints(dir: &Path) -> ScanSummary {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return ScanSummary::default();
    };
    let mut scans: Vec<(Option<StateSummary>, Vec<u64>)> = Vec::new();
    for e in rd.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let ws = e.path();
        let state = std::fs::read_to_string(ws.join("state.json"))
            .ok()
            .and_then(|t| parse_state_summary(&t));
        let mut enc_sizes = Vec::new();
        if let Ok(rd2) = std::fs::read_dir(ws.join("pending")) {
            for f in rd2.flatten() {
                if f.file_name().to_string_lossy().ends_with(".enc") {
                    if let Ok(m) = f.metadata() {
                        enc_sizes.push(m.len());
                    }
                }
            }
        }
        scans.push((state, enc_sizes));
    }
    summarize_scans(&scans)
}

/// 快照防护状态机：guard.json 内存镜像 + 上一拍 calls + 节流的扫描缓存。
/// poller 每拍 `tick`；apply/release 由 Tauri 命令调用（前端已过确认弹窗）
pub struct SnapshotGuard {
    file: GuardFile,
    /// 上一拍 calls_today（增量累计；跨天回退按 0 增量重置基准拍）
    last_calls: u64,
    scan: ScanSummary,
    last_scan: Option<std::time::Instant>,
}

impl SnapshotGuard {
    pub fn new() -> Self {
        SnapshotGuard {
            file: load_guard_file(),
            last_calls: 0,
            scan: ScanSummary::default(),
            last_scan: None,
        }
    }

    /// 供独立 status 命令取当前 calls 口径（不推进计数）
    pub fn last_calls_seen(&self) -> u64 {
        self.last_calls
    }

    fn status(&self, locked: bool) -> SnapshotGuardStatus {
        SnapshotGuardStatus {
            supported: SUPPORTED,
            locked,
            locked_since_ms: self.file.locked_since_ms,
            blocked_rounds: self.file.blocked_rounds,
            artifact_count: self.scan.artifact_count,
            artifact_bytes: self.scan.artifact_bytes,
            workspace_count: self.scan.workspace_count,
            failure_count: self.scan.failure_count,
        }
    }

    /// poller 每拍：写入探测定锁定态 → 维护 blocked_rounds（变化才落盘）→
    /// 节流扫描（5s）→ 组装 status
    pub fn tick(&mut self, calls_today: u64, now_ms: i64) -> SnapshotGuardStatus {
        let Some(dir) = checkpoints_dir() else {
            return SnapshotGuardStatus { supported: SUPPORTED, ..Default::default() };
        };
        let locked = probe_locked(&dir);
        if locked {
            if self.file.locked_since_ms.is_none() || self.file.calls_baseline.is_none() {
                // 目录已锁但没有记录（用户手动 chflags / 面板重装丢档）：
                // 从现在起补记基线
                self.file.locked_since_ms = Some(now_ms);
                self.file.calls_baseline = Some(calls_today);
                self.last_calls = calls_today;
                save_guard_file(&self.file);
            } else {
                // calls_today 当日只增；跨天回退（saturating 后为 0 增量）
                // 并重置基准拍，从新一天的计数继续累加
                let delta = calls_today.saturating_sub(self.last_calls);
                if delta > 0 {
                    self.file.blocked_rounds += delta;
                    save_guard_file(&self.file);
                }
                self.last_calls = calls_today;
            }
        } else {
            self.last_calls = calls_today;
            if self.file.locked_since_ms.is_some() {
                // 有记录但目录已解锁（外部解除/手工 nouchg）：清档如实反映
                self.file = GuardFile::default();
                save_guard_file(&self.file);
            }
        }
        if self.last_scan.map_or(true, |t| t.elapsed() > SCAN_EVERY) {
            self.last_scan = Some(std::time::Instant::now());
            self.scan = scan_checkpoints(&dir);
        }
        self.status(locked)
    }

    /// 开启防护（前端已过确认弹窗）：先解锁（幂等，重复开启时才能清空）→
    /// 清空 checkpoints → 重建空目录 → uchg 锁定 → 写入探测校验 → 记录
    /// guard.json（锁定时刻 + calls 基线）
    pub fn apply(&mut self, calls_today: u64, now_ms: i64) -> Result<SnapshotGuardStatus, String> {
        if !SUPPORTED {
            return Err("文件锁仅支持 macOS（chflags）".into());
        }
        let dir = checkpoints_dir().ok_or("无法定位用户目录")?;
        if probe_locked(&dir) {
            set_immutable(&dir, false)?;
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("清空 checkpoints 失败: {e}"))?;
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("重建 checkpoints 目录失败: {e}"))?;
        set_immutable(&dir, true)?;
        if !probe_locked(&dir) {
            return Err("锁定未生效（写入探测仍成功），请检查目录权限".into());
        }
        self.file = GuardFile {
            locked_since_ms: Some(now_ms),
            calls_baseline: Some(calls_today),
            blocked_rounds: 0,
        };
        self.last_calls = calls_today;
        self.scan = ScanSummary::default();
        self.last_scan = Some(std::time::Instant::now());
        save_guard_file(&self.file);
        Ok(self.status(true))
    }

    /// 解除防护：nouchg 解锁（目录留空，ZCode 检测不到内容会自动重建
    /// state.json/pending 等）；清空 guard.json 计数
    pub fn release(&mut self, calls_today: u64) -> Result<SnapshotGuardStatus, String> {
        if !SUPPORTED {
            return Err("文件锁仅支持 macOS（chflags）".into());
        }
        let dir = checkpoints_dir().ok_or("无法定位用户目录")?;
        if probe_locked(&dir) {
            set_immutable(&dir, false)?;
        }
        self.file = GuardFile::default();
        self.last_calls = calls_today;
        save_guard_file(&self.file);
        let locked = probe_locked(&dir);
        Ok(self.status(locked))
    }
}

// ============ 测试 ============
#[cfg(test)]
mod tests {
    use super::*;

    /// state.json 解析 + 聚合：failureCount 求和、工件数/体积累计、
    /// 损坏 JSON 容错（跳过的工作区仍计工件与目录数，只是不贡献 failure）
    #[test]
    fn state_summary_parse_and_aggregate() {
        let ok = parse_state_summary(
            r#"{"workspacePath":"/Users/x/proj","failureCount":3,
                "lastCompressedSize":{"encryptedSizeBytes":100,"workspaceSizeBytes":200}}"#,
        )
        .expect("应解析成功");
        assert_eq!(ok.failure_count, 3);
        // failureCount 缺失按 0
        assert_eq!(parse_state_summary(r#"{"workspacePath":"x"}"#).unwrap().failure_count, 0);
        // 损坏 JSON / 非对象 → None（调用方跳过）
        assert!(parse_state_summary("{oops").is_none());
        assert!(parse_state_summary("[]").is_none());

        let scans = vec![
            (Some(ok), vec![100, 50]),                        // 2 个工件 150B，failure 3
            (None, vec![549_000_000]),                        // state 损坏：failure 不计
            (Some(StateSummary { failure_count: 7 }), vec![]), // 无工件的工作区
        ];
        let s = summarize_scans(&scans);
        assert_eq!(s.workspace_count, 3);
        assert_eq!(s.artifact_count, 3);
        assert_eq!(s.artifact_bytes, 549_000_150);
        assert_eq!(s.failure_count, 10);
        assert_eq!(summarize_scans(&[]), ScanSummary::default());
    }

    /// 防护状态字段的序列化契约：camelCase 键名 + guard.json 往返
    /// （前端 SnapshotPayload.guard 依赖键名；guard.json 是跨启动唯一持久化）
    #[test]
    fn guard_status_serializes_locked_fields() {
        let st = SnapshotGuardStatus {
            supported: true,
            locked: true,
            locked_since_ms: Some(1_788_000_000_000),
            blocked_rounds: 42,
            artifact_count: 302,
            artifact_bytes: 302_000_000,
            workspace_count: 23,
            failure_count: 11,
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["supported"], true);
        assert_eq!(json["locked"], true);
        assert_eq!(json["lockedSinceMs"], 1_788_000_000_000i64);
        assert_eq!(json["blockedRounds"], 42);
        assert_eq!(json["artifactCount"], 302);
        assert_eq!(json["artifactBytes"], 302_000_000);
        assert_eq!(json["workspaceCount"], 23);
        assert_eq!(json["failureCount"], 11);
        // 未防护默认值：locked=false、时刻为 null（前端按空隐藏"防护后"行）
        let def = serde_json::to_value(SnapshotGuardStatus::default()).unwrap();
        assert_eq!(def["locked"], false);
        assert_eq!(def["lockedSinceMs"], serde_json::Value::Null);

        // guard.json 往返：锁定三字段保留；空对象全缺省；默认实例不落多余键
        let f = GuardFile { locked_since_ms: Some(123), calls_baseline: Some(456), blocked_rounds: 7 };
        let round: GuardFile = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(round, f);
        let empty: GuardFile = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, GuardFile::default());
        assert!(!serde_json::to_string(&GuardFile::default()).unwrap().contains("lockedSinceMs"));
    }
}
