//! 实时速度实测：轮询 ZCode CLI 进程的 IO 写字节计数（往桌面 UI 管道的流式渲染数据），
//! 在模型流式输出期间该计数会以数十 KB/s 持续增长，是真实的实时信号。
//!
//! - 进程发现：Toolhelp 枚举 + NtQueryInformationProcess 读命令行，过滤 `zcode.cjs`
//! - 落盘扣除：rollout/日志/WAL 的增长从字节增量中减去（完成/flush 瞬间的尖峰来源）。
//!   扣除允许单拍为负（flush 与写入错位时由区间总和收敛），只在窗口汇总时钳非负——
//!   若逐拍钳 0，错位的落盘增量会被永久吞掉（实测读数塌缩到真值的 1/5 就是这个原因）
//! - 噪声底：BASE_NOISE + 每进程自适应心跳底（封顶，防止持续流式期间分位数被
//!   流式增量"毒化"，把自己的输出当噪声扣掉）
//! - 突发剔除：单拍原始增量超过阈值（请求体上传 ~190KB/拍）整拍丢弃，不进积分
//! - 字节→token 换算【一致性校准】：调用完成后用 与显示路径完全相同的清洗流
//!   在 [first_token, completed] 区间的积分字节 ÷ 真实 output_tokens 做滑动自校准。
//!   校准分子与显示分子同源，任何系统性扣除（噪声底/落盘/错位）都会被系数抵消，
//!   显示值收敛到真实 t/s。历史教训：校准用未清洗的总字节流、显示用清洗后的流，
//!   两条链路口径不一致曾导致系数被抬高 2~3 倍、读数系统性偏低。

use crate::metrics::Call;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// 当前速度统计窗口（显示幅度）
const WINDOW_MS: i64 = 30_000;
/// 进程/文件采样环容量（~3min @700ms，覆盖校准回溯区间）
const RING_CAP: usize = 260;
/// 进程列表刷新周期
const REFRESH_EVERY: Duration = Duration::from_secs(30);
/// 流式判定阈值（清洗后速率）
const STREAMING_BPS: f64 = 4_000.0;
/// 启停由调用门控决定；该短窗仅用于定位幅度锚点（首字节拍）
const DETECT_MS: i64 = 2_500;
/// 待机心跳底噪的粗略上界（B/s），叠加每进程自适应底噪（封顶后）过滤心跳
const BASE_NOISE_BPS: f64 = 3_000.0;
/// 每进程自适应心跳底的单拍封顶（字节/拍）。实测流式可到 ~75KB/s（52KB/拍），
/// 分位数底噪若不封顶，持续流式期间会被流式增量抬高，把自己的输出当噪声扣掉
const FLOOR_CAP_BYTES: f64 = 2_000.0;
/// 单拍原始增量的剔除阈值：请求体上传实测 ~190KB/拍（拆分也 >90KB），
/// 真实流式最大 ~52KB/拍，取中间值整拍丢弃（字节与时长都不进积分）
const BURST_TICK_BYTES: f64 = 100_000.0;
/// 初始字节/token 系数（清洗流实测约 350~900，取偏保守值，校准后快速收敛）
const DEFAULT_BPT: f64 = 600.0;
/// 校准样本的合理区间（防异常样本污染中位数）。一致性校准下系数会吸收系统性
/// 扣除（落盘镜像/噪声底/突发拍剔除），区间必须足够宽以免破坏收敛；
/// 垃圾样本主要由 CAL_MIN_TOKENS 的调用规模门槛拦截
const CAL_MIN: f64 = 100.0;
const CAL_MAX: f64 = 6_000.0;
/// 校准样本的调用规模下限：小调用的 UI 固定帧开销占比大，禁止入样本
const CAL_MIN_TOKENS: u64 = 300;

#[derive(Default, Clone)]
pub struct LiveNow {
    /// 是否成功发现了 CLI 进程（false 时前端回退到窗口/估算显示）
    pub available: bool,
    pub streaming: bool,
    /// 流式已开始但 30s 滑窗尚未填满（读数来自已活跃区间，前端显示"统计中"）
    pub ramping: bool,
    pub tps: f64,
    /// 清洗后的管道字节率（B/s），调试日志/对账用
    pub pipe_bps: f64,
}

/// 一次调用完成后的校准与对账事件（调试日志用）
#[derive(Clone, Debug, serde::Serialize)]
pub struct CalEvent {
    pub id: String,
    pub session: String,
    pub completed_ms: i64,
    /// 调用真值（落盘 output+reasoning ÷ 生成时长）
    pub true_tps: f64,
    pub gen_ms: i64,
    pub eff: u64,
    /// 流式区间原始写字节（未清洗，全进程求和；归属判定与对账基线）
    pub raw_bytes: f64,
    /// 与显示同口径的清洗流在 [first_token, completed] 的积分字节（本调用系数分子）
    pub clean_bytes: f64,
    /// 本调用清洗流样本 B/token（0 = 未入校准）
    pub bpt_sample: f64,
    /// 事件后生效的系数
    pub bpt_now: f64,
    /// 未入校准（调用过短 / 无有效字节）
    pub cal_skipped: bool,
}

// ============ 纯计算部分（跨平台，可单测）：拍清洗 / 区间积分 / 中位数 ============

/// 清洗后的单拍管道字节。bytes 可为负：落盘 flush 与 IO 计数错位时，
/// 由区间积分求和收敛（Σ bytes = Σ 原始增量 − Σ 落盘 − Σ 噪声底），不能逐拍钳 0
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TickRow {
    /// 拍时长
    pub dt_ms: i64,
    /// 清洗后字节（原始写入 − 落盘增长 − 噪声底，可能为负）
    pub bytes: f64,
    /// 拍结束时刻（墙钟 ms）
    pub end_ms: i64,
}

/// 由累计写字节序列与 tracked 文件总量序列构建清洗后的拍序列。
/// samples / files 均为 (时刻 ms, 累计值)，通常同一轮询循环成对采样（时间戳一致）。
pub(crate) fn build_rows(
    samples: &[(i64, u64)],
    files: &[(i64, u64)],
    min_delta: f64,
) -> Vec<TickRow> {
    let floor_static = min_delta.min(FLOOR_CAP_BYTES);
    let mut rows = Vec::with_capacity(samples.len());
    for w in samples.windows(2) {
        let (t0, w0) = w[0];
        let (t1, w1) = w[1];
        let dt_ms = t1 - t0;
        if dt_ms <= 0 {
            continue;
        }
        let raw = w1.saturating_sub(w0) as f64;
        // 请求体上传等单拍突发：整拍剔除（既不积分字节也不计时长）
        if raw > BURST_TICK_BYTES {
            continue;
        }
        // [t0, t1) 区间内 tracked 文件的落盘增长（边界半开，避免相邻区间重复计入）
        let mut fg = 0f64;
        for f in files.windows(2) {
            let (ft0, fv0) = f[0];
            let (ft1, fv1) = f[1];
            if ft1 > t0 && ft0 < t1 {
                fg += fv1.saturating_sub(fv0) as f64;
            }
        }
        let dt_s = dt_ms as f64 / 1000.0;
        rows.push(TickRow {
            dt_ms,
            bytes: raw - fg - floor_static - BASE_NOISE_BPS * dt_s,
            end_ms: t1,
        });
    }
    rows
}

/// 按时间比例积分 [from_ms, to_ms] 区间：跨界拍按重叠时长分摊。
/// 返回 (字节, 秒)。被剔除的突发拍不贡献时长，不稀释速率
pub(crate) fn integrate(rows: &[TickRow], from_ms: i64, to_ms: i64) -> (f64, f64) {
    let (mut bytes, mut secs) = (0f64, 0f64);
    for r in rows {
        let lo = (r.end_ms - r.dt_ms).max(from_ms);
        let hi = r.end_ms.min(to_ms);
        if hi <= lo {
            continue;
        }
        let ov = (hi - lo) as f64;
        let dt = r.dt_ms as f64;
        bytes += r.bytes * (ov / dt);
        secs += ov / 1000.0;
    }
    (bytes, secs)
}

/// 校准系数维护：滑动窗口样本的中位数（纯函数便于测试）
pub(crate) fn median_bpt(samples: &mut VecDeque<f64>, sample: f64, cap: usize) -> f64 {
    samples.push_back(sample);
    while samples.len() > cap {
        samples.pop_front();
    }
    let mut sorted: Vec<f64> = samples.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted[sorted.len() / 2]
}

/// 校准样本准入：管道积分须有实质贡献（≥原始字节的 20%），且 B/token 未钳位
/// 就落在合理区间。管道静默调用（字节全在完成瞬间落盘，实测样本可低至
/// ~7 B/token）与异常比例样本整条拒绝，防止中位数系数被污染。
/// 返回 (是否入样, 样本值)
pub(crate) fn cal_sample(eff: u64, clean_bytes: f64, raw_bytes: f64) -> (bool, f64) {
    if eff < CAL_MIN_TOKENS || clean_bytes <= 0.0 || raw_bytes <= 0.0 {
        return (false, 0.0);
    }
    let ratio = clean_bytes / eff as f64;
    let usable = clean_bytes / raw_bytes >= 0.2;
    (usable && ratio >= CAL_MIN && ratio <= CAL_MAX, ratio)
}

struct ProcRing {
    handle: isize,
    samples: VecDeque<(i64, u64)>,
    /// 该进程最小的每拍增量（自适应心跳噪声底，使用时封顶）
    min_delta: f64,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetProcessIoCounters(h: *mut c_void, counters: *mut IoCounters) -> i32;
        fn ReadProcessMemory(
            h: *mut c_void,
            addr: *const c_void,
            buf: *mut c_void,
            size: usize,
            read: *mut usize,
        ) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Process32FirstW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snap: isize, entry: *mut ProcessEntry32W) -> i32;
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            h: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
    }

    #[repr(C)]
    struct IoCounters {
        read_ops: u64,
        write_ops: u64,
        other_ops: u64,
        read_bytes: u64,
        write_bytes: u64,
        other_bytes: u64,
    }

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        pri_class_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    const PROCESS_QUERY_LIMITED: u32 = 0x1410; // QUERY_INFORMATION | QUERY_LIMITED | VM_READ
    const TH32CS_SNAPPROCESS: u32 = 2;

    fn process_command_line(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED, 0, pid);
            if h.is_null() {
                return None;
            }
            // 经典方案：ProcessBasicInformation → PEB → ProcessParameters → CommandLine
            let rd = |addr: usize, buf: &mut [u8]| -> bool {
                let mut n = 0usize;
                ReadProcessMemory(h, addr as *const c_void, buf.as_mut_ptr().cast(), buf.len(), &mut n)
                    != 0
            };
            let mut pbi = [0u8; 48];
            let mut ret: u32 = 0;
            if NtQueryInformationProcess(h, 0, pbi.as_mut_ptr().cast(), 48, &mut ret) != 0 {
                CloseHandle(h);
                return None;
            }
            #[cfg(target_pointer_width = "64")]
            {
                let peb = usize::from_ne_bytes(pbi[8..16].try_into().ok()?);
                if peb == 0 {
                    CloseHandle(h);
                    return None;
                }
                let mut pp_ptr = [0u8; 8];
                if !rd(peb + 0x20, &mut pp_ptr) {
                    CloseHandle(h);
                    return None;
                }
                let pp = usize::from_ne_bytes(pp_ptr.try_into().ok()?);
                if pp == 0 {
                    CloseHandle(h);
                    return None;
                }
                // RTL_USER_PROCESS_PARAMETERS.CommandLine (UNICODE_STRING) @ 0x70
                let mut us = [0u8; 16];
                if !rd(pp + 0x70, &mut us) {
                    CloseHandle(h);
                    return None;
                }
                let len = u16::from_ne_bytes([us[0], us[1]]) as usize;
                let buf_ptr = usize::from_ne_bytes(us[8..16].try_into().ok()?);
                if len == 0 || buf_ptr == 0 {
                    CloseHandle(h);
                    return None;
                }
                let mut wbuf = vec![0u8; len];
                if !rd(buf_ptr, &mut wbuf) {
                    CloseHandle(h);
                    return None;
                }
                let u16s: Vec<u16> = wbuf
                    .chunks_exact(2)
                    .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                    .collect();
                CloseHandle(h);
                return Some(String::from_utf16_lossy(&u16s));
            }
            #[cfg(not(target_pointer_width = "64"))]
            {
                CloseHandle(h);
                None
            }
        }
    }

    fn discover_cli_pids() -> Vec<u32> {
        let mut pids = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == -1 {
                return pids;
            }
            let mut entry = ProcessEntry32W {
                size: std::mem::size_of::<ProcessEntry32W>() as u32,
                usage: 0,
                process_id: 0,
                default_heap_id: 0,
                module_id: 0,
                threads: 0,
                parent_process_id: 0,
                pri_class_base: 0,
                flags: 0,
                exe_file: [0; 260],
            };
            let ok = Process32FirstW(snap, &mut entry);
            if ok != 0 {
                loop {
                    let exe = String::from_utf16_lossy(
                        &entry.exe_file[..entry.exe_file.iter().position(|c| *c == 0).unwrap_or(260)],
                    );
                    if exe.eq_ignore_ascii_case("zcode.exe") {
                        if let Some(cmd) = process_command_line(entry.process_id) {
                            if cmd.contains("zcode.cjs") {
                                pids.push(entry.process_id);
                            }
                        }
                    }
                    if Process32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap as *mut c_void);
        }
        pids
    }

    fn io_write_bytes(handle: isize) -> Option<u64> {
        let mut io = IoCounters {
            read_ops: 0,
            write_ops: 0,
            other_ops: 0,
            read_bytes: 0,
            write_bytes: 0,
            other_bytes: 0,
        };
        unsafe {
            if GetProcessIoCounters(handle as *mut c_void, &mut io) != 0 {
                Some(io.write_bytes)
            } else {
                None
            }
        }
    }

    pub struct LiveIo {
        procs: HashMap<u32, ProcRing>,
        last_refresh: Option<Instant>,
        /// (时刻 ms, tracked 文件累计字节)
        files_hist: VecDeque<(i64, u64)>,
        pending: VecDeque<Call>,
        cal: VecDeque<f64>,
        bytes_per_token: f64,
        last_result: LiveNow,
        /// 会话 → 最近一次为其生成输出的 CLI 进程
        session_pid: HashMap<String, u32>,
        /// 当前关注的会话 = 最近完成调用的会话
        current_session: Option<String>,
        attributed: HashSet<String>,
        history_done: bool,
        /// 进行中的调用（Engine 由 message 表判定）：(会话, 调用开始时刻)
        inflight: Option<(String, i64)>,
        /// 本段调用的幅度锚点（首个达到流式阈值的时刻，墙钟 ms）
        active_since: Option<i64>,
        /// 锚点所属的会话进程（变化时重置）
        active_pid: Option<u32>,
        /// 最近一次校准事件（供调试日志取用）
        pending_cal: Option<CalEvent>,
    }

    impl LiveIo {
        pub fn new() -> Self {
            // 系数队列预置默认值为先验样本：冷启动阶段单个异常样本无法独占中位数，
            // 需要 2 个真实样本才能推动系数；满 5 个样本后先验自然被挤出
            let mut cal = VecDeque::with_capacity(5);
            cal.push_back(DEFAULT_BPT);
            Self {
                procs: HashMap::new(),
                last_refresh: None,
                files_hist: VecDeque::new(),
                pending: VecDeque::new(),
                cal,
                bytes_per_token: DEFAULT_BPT,
                last_result: LiveNow::default(),
                session_pid: HashMap::new(),
                current_session: None,
                attributed: HashSet::new(),
                history_done: false,
                inflight: None,
                active_since: None,
                active_pid: None,
                pending_cal: None,
            }
        }

        /// 启动时注入今日已有调用，用于确定当前会话
        pub fn ingest_history(&mut self, calls: &[Call]) {
            if let Some(latest) = calls.iter().max_by_key(|c| c.completed_ms) {
                self.current_session = Some(latest.session.clone());
            }
            self.history_done = true;
        }

        pub fn history_done(&self) -> bool {
            self.history_done
        }

        fn tracked_files_total(&self) -> u64 {
            // rollout 目录全部 jsonl + CLI 日志目录 + db WAL（落盘写入的尖峰来源）
            let mut total = 0u64;
            if let Some(home) = crate::metrics::home_dir() {
                for dir in [
                    home.join(".zcode/cli/rollout"),
                    home.join(".zcode/cli/log"),
                ] {
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for e in rd.flatten() {
                            if let Ok(m) = e.metadata() {
                                total += m.len();
                            }
                        }
                    }
                }
                if let Ok(m) = std::fs::metadata(home.join(".zcode/cli/db/db.sqlite-wal")) {
                    total += m.len();
                }
            }
            total
        }

        pub fn observe(&mut self, new_calls: &[Call]) {
            for c in new_calls {
                self.pending.push_back(c.clone());
                // 最近完成调用的会话 = 当前关注的会话
                self.current_session = Some(c.session.clone());
            }
            while self.pending.len() > 8 {
                self.pending.pop_front();
            }
        }

        /// 每拍更新"调用进行中"信号（Engine 由 message 表与完成行比较得出）
        pub fn set_inflight(&mut self, inflight: Option<(String, i64)>) {
            self.inflight = inflight;
        }

        /// 当前生效的字节→token 系数（调试日志用）
        pub fn bytes_per_token(&self) -> f64 {
            self.bytes_per_token
        }

        /// 取走最近一次校准事件（如有）
        pub fn take_calibration(&mut self) -> Option<CalEvent> {
            self.pending_cal.take()
        }

        /// 每个轮询周期调用一次。now_ms 为墙钟毫秒（与 Engine 快照同源）
        pub fn measure(&mut self, now_ms: i64) -> LiveNow {
            let now = Instant::now();
            // 周期性刷新 CLI 进程集合
            if self
                .last_refresh
                .map_or(true, |t| now.duration_since(t) > REFRESH_EVERY)
            {
                self.last_refresh = Some(now);
                let found = discover_cli_pids();
                self.procs.retain(|pid, _| found.contains(pid));
                for pid in found {
                    self.procs.entry(pid).or_insert_with(|| ProcRing {
                        handle: unsafe { OpenProcess(PROCESS_QUERY_LIMITED, 0, pid) } as isize,
                        samples: VecDeque::new(),
                        min_delta: f64::MAX,
                    });
                }
            }

            // 采样本轮写字节与 tracked 文件总量（同拍成对，时间戳一致）
            self.procs.retain(|_, ring| {
                match io_write_bytes(ring.handle) {
                    Some(w) => {
                        ring.samples.push_back((now_ms, w));
                        while ring.samples.len() > RING_CAP {
                            ring.samples.pop_front();
                        }
                        true
                    }
                    None => false, // 进程已退出
                }
            });
            let ft = self.tracked_files_total();
            self.files_hist.push_back((now_ms, ft));
            while self.files_hist.len() > RING_CAP {
                self.files_hist.pop_front();
            }
            let files: Vec<(i64, u64)> = self.files_hist.iter().copied().collect();

            // 清洗后的拍序列（显示与校准共用同一条流，保证口径一致）
            let mut rows_by_pid: HashMap<u32, Vec<TickRow>> = HashMap::new();
            for (pid, ring) in self.procs.iter_mut() {
                let samples: Vec<(i64, u64)> = ring.samples.iter().copied().collect();
                // 每拍最小增量的低分位 × 2 作为该进程的心跳噪声底（字节/拍），
                // 样本太少时不用自适应底噪（避免冷启动吃掉起步信号）。
                // 使用时在 build_rows 内封顶，防持续流式期间被自身增量毒化
                let mut deltas: Vec<f64> = samples
                    .windows(2)
                    .map(|w| w[1].1.saturating_sub(w[0].1) as f64)
                    .collect();
                if !deltas.is_empty() {
                    deltas.sort_by(|a: &f64, b: &f64| a.partial_cmp(b).unwrap());
                    ring.min_delta = if deltas.len() >= 20 {
                        deltas[deltas.len() / 10] * 2.0
                    } else {
                        0.0
                    };
                }
                rows_by_pid.insert(*pid, build_rows(&samples, &files, ring.min_delta));
            }

            // ---- 校准 + 会话→进程归属（调用完成后处理）----
            // 归属用原始字节（未清洗）：落盘错位/噪声扣除不影响"哪个进程在写"的判断。
            // 系数分子用与显示完全相同的清洗流在 [first_token, completed] 的积分，
            // 系统性扣除被系数抵消，显示收敛到真实 t/s
            while let Some(call) = self.pending.front().cloned() {
                if now_ms - call.completed_ms > 120_000 {
                    self.pending.pop_front();
                    continue;
                }
                let stream_start_ms = (call.completed_ms - call.gen_ms.min(300_000)).max(0);
                let mut raw_by_pid: HashMap<u32, u64> = HashMap::new();
                for (pid, ring) in self.procs.iter() {
                    let mut acc = 0u64;
                    for (a, b) in ring.samples.iter().zip(ring.samples.iter().skip(1)) {
                        if b.0 >= stream_start_ms && a.0 <= call.completed_ms {
                            acc += b.1.saturating_sub(a.1);
                        }
                    }
                    raw_by_pid.insert(*pid, acc);
                }
                let raw_total = raw_by_pid.values().sum::<u64>() as f64;
                if !self.attributed.contains(&call.id) {
                    self.attributed.insert(call.id.clone());
                    if self.attributed.len() > 4_000 {
                        self.attributed.clear();
                    }
                    if raw_total > 20_000.0 {
                        if let Some((pid, _)) =
                            raw_by_pid.iter().max_by(|a, b| a.1.cmp(b.1))
                        {
                            self.session_pid.insert(call.session.clone(), *pid);
                        }
                    }
                }
                // 清洗积分：优先归属进程（与显示路径一致），未归属时退化为全进程求和
                let clean_bytes = match self.session_pid.get(&call.session) {
                    Some(pid) if rows_by_pid.contains_key(pid) => {
                        integrate(&rows_by_pid[pid], stream_start_ms, call.completed_ms).0
                    }
                    _ => rows_by_pid
                        .values()
                        .map(|r| integrate(r, stream_start_ms, call.completed_ms).0)
                        .sum::<f64>(),
                };
                let eff = call.effective_out();
                let true_tps = eff as f64 / (call.gen_ms.max(50) as f64 / 1000.0);
                let (in_cal, bpt_sample) = cal_sample(eff, clean_bytes, raw_total);
                if in_cal {
                    self.bytes_per_token = median_bpt(&mut self.cal, bpt_sample, 5);
                }
                self.pending_cal = Some(CalEvent {
                    id: call.id.clone(),
                    session: call.session.clone(),
                    completed_ms: call.completed_ms,
                    true_tps,
                    gen_ms: call.gen_ms,
                    eff,
                    raw_bytes: raw_total,
                    clean_bytes,
                    bpt_sample: if in_cal { bpt_sample } else { 0.0 },
                    bpt_now: self.bytes_per_token,
                    cal_skipped: !in_cal,
                });
                self.pending.pop_front();
            }

            // 只统计当前会话对应进程的写字节流，避免后台会话污染状态
            let live_pid = self
                .current_session
                .as_ref()
                .and_then(|s| self.session_pid.get(s))
                .copied();
            // 会话进程变化时重置幅度锚点，避免跨会话残留
            if live_pid != self.active_pid {
                self.active_pid = live_pid;
                self.active_since = None;
            }

            // 区间积分辅助：归属进程的流，或全部进程的流（时间轴相同，分子分母分别求和）
            let span = |from_ms: i64| -> (f64, f64) {
                match live_pid {
                    Some(pid) => integrate(
                        rows_by_pid.get(&pid).map(|v| &v[..]).unwrap_or(&[]),
                        from_ms,
                        now_ms,
                    ),
                    None => rows_by_pid
                        .values()
                        .map(|r| integrate(r, from_ms, now_ms))
                        .fold((0.0, 0.0), |(b, s), (bb, ss)| (b + bb, s + ss)),
                }
            };

            // 启停判定（调用门控）：message 表的 assistant 行在调用开始瞬间提交、
            // 完成行在调用结束落盘，"开始时间 > 完成时间"即调用进行中——
            // 开始当拍生效（首 token 前即亮"统计中"），完成行落盘当拍归零。
            // 工具执行/待机期间管道同样有 UI 状态突发，门控将其可靠排除。
            // 门控不可用时（尚无任何已完成调用做基线）退化为纯字节判定。
            let model_active = self
                .inflight
                .as_ref()
                .map_or(false, |(s, _)| Some(s) == self.current_session.as_ref());
            let (det_b, det_s) = span(now_ms - DETECT_MS);
            let detect_bps = if det_s > 0.0 { det_b / det_s } else { 0.0 };
            let gate_on = if self.inflight.is_some() {
                model_active
            } else {
                self.current_session.is_none() && detect_bps > STREAMING_BPS
            };
            if gate_on {
                // 幅度锚点：本段调用内首个清洗流速达到流式阈值的时刻
                if self.active_since.is_none() && detect_bps > STREAMING_BPS {
                    self.active_since = Some(now_ms);
                }
            } else {
                self.active_since = None;
            }

            // 幅度：30s 滑窗 ∩ [首字节拍, now] 的清洗流积分。首字节当拍即有真实读数
            // （此前为 TTFT，显示"统计中"）；稳态覆盖满 30s（平滑）；停止当拍归零。
            // 落盘 flush 错位产生的负拍在窗口内对消，仅在汇总处钳非负
            let pipe_bps = if gate_on {
                match self.active_since {
                    Some(anchor) => {
                        let from = anchor.max(now_ms - WINDOW_MS);
                        let (b, s) = span(from);
                        if s > 0.0 {
                            (b / s).max(0.0)
                        } else {
                            0.0
                        }
                    }
                    None => 0.0, // 首字节未到（TTFT），显示统计中
                }
            } else {
                0.0
            };
            let streaming = gate_on;
            let ramping = streaming
                && (self.active_since.is_none()
                    || self
                        .active_since
                        .map_or(false, |a| now_ms - a < WINDOW_MS));

            let result = LiveNow {
                available: !self.procs.is_empty(),
                streaming,
                ramping,
                tps: if streaming {
                    pipe_bps / self.bytes_per_token
                } else {
                    0.0
                },
                pipe_bps,
            };
            self.last_result = result.clone();
            result
        }
    }
}

#[cfg(windows)]
pub use imp::LiveIo;

#[cfg(not(windows))]
pub struct LiveIo;

#[cfg(not(windows))]
impl LiveIo {
    pub fn new() -> Self {
        LiveIo
    }
    pub fn ingest_history(&mut self, _calls: &[Call]) {}
    pub fn history_done(&self) -> bool {
        true
    }
    pub fn observe(&mut self, _new_calls: &[Call]) {}
    pub fn set_inflight(&mut self, _inflight: Option<(String, i64)>) {}
    pub fn bytes_per_token(&self) -> f64 {
        DEFAULT_BPT
    }
    pub fn take_calibration(&mut self) -> Option<CalEvent> {
        None
    }
    pub fn measure(&mut self, _now_ms: i64) -> LiveNow {
        LiveNow::default()
    }
}

// ============ 测试 ============
#[cfg(test)]
mod tests {
    use super::*;

    const TICK: i64 = 700;

    /// 构造 (时刻, 累计字节) 采样序列：每拍增量由 deltas 给出
    fn series(start_ms: i64, deltas: &[f64]) -> Vec<(i64, u64)> {
        let mut out = Vec::with_capacity(deltas.len() + 1);
        let mut acc = 0u64;
        out.push((start_ms, acc));
        for (i, d) in deltas.iter().enumerate() {
            acc += *d as u64;
            out.push((start_ms + (i as i64 + 1) * TICK, acc));
        }
        out
    }

    #[test]
    fn burst_tick_dropped_entirely() {
        // 190KB 请求体突发拍被整拍丢弃（字节与时长都不进积分）；52KB 真实流式拍保留
        let s = series(0, &[190_000.0, 52_000.0, 52_000.0]);
        let files = s.iter().map(|(t, _)| (*t, 0u64)).collect::<Vec<_>>();
        let rows = build_rows(&s, &files, 0.0);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.bytes < 52_000.0));
        // 突发拍连时长都不贡献：两个保留拍的 dt 合计 1.4s
        let secs = integrate(&rows, i64::MIN, i64::MAX).1;
        assert!((secs - 1.4).abs() < 1e-9);
    }

    #[test]
    fn floor_capped_against_ring_poisoning() {
        // 自适应底噪被毒化到 50KB/拍（持续流式期间分位数抬高），
        // 封顶后每拍最多扣 FLOOR_CAP + BASE_NOISE×dt，52KB 流式拍仍保留大头
        let s = series(0, &[52_000.0; 4]);
        let files = s.iter().map(|(t, _)| (*t, 0u64)).collect::<Vec<_>>();
        let rows = build_rows(&s, &files, 50_000.0);
        let floor = FLOOR_CAP_BYTES + BASE_NOISE_BPS * (TICK as f64 / 1000.0);
        for r in &rows {
            assert!((r.bytes - (52_000.0 - floor)).abs() < 1e-6);
        }
    }

    #[test]
    fn flush_mismatch_yields_negative_not_clamped() {
        // 落盘 flush 与 IO 计数错位：本拍只写了 20KB 但文件可见增长 60KB
        // → 该拍为负（-40KB），由前后正拍在区间总和中对消，而不是被钳成 0 丢失
        let s = series(0, &[20_000.0, 60_000.0, 20_000.0]);
        // 文件增长集中在第二拍：0, 0, 60_000, 60_000
        let files = vec![
            (s[0].0, 0u64),
            (s[1].0, 0u64),
            (s[2].0, 60_000u64),
            (s[3].0, 60_000u64),
        ];
        let rows = build_rows(&s, &files, 0.0);
        let floor = BASE_NOISE_BPS * (TICK as f64 / 1000.0);
        assert!(rows[1].bytes < 0.0, "flush 拍应为负: {}", rows[1].bytes);
        // 区间总和 = Σraw − Σfg − Σfloor = 100_000 − 60_000 − 3×floor
        let (b, _) = integrate(&rows, i64::MIN, i64::MAX);
        assert!((b - (100_000.0 - 60_000.0 - 3.0 * floor)).abs() < 1e-6);
    }

    #[test]
    fn integrate_prorates_boundary_ticks() {
        let rows = vec![TickRow { dt_ms: 1000, bytes: 1000.0, end_ms: 10_000 }];
        // 只取该拍的后半 [9_500, 10_000]：字节与时长各计一半
        let (b, s) = integrate(&rows, 9_500, 10_000);
        assert!((b - 500.0).abs() < 1e-9);
        assert!((s - 0.5).abs() < 1e-9);
        // 完全不重叠的区间不贡献
        let (b, _) = integrate(&rows, 10_500, 11_000);
        assert_eq!(b, 0.0);
    }

    #[test]
    fn median_bpt_sliding() {
        let mut s = VecDeque::new();
        assert!((median_bpt(&mut s, 600.0, 5) - 600.0).abs() < 1e-9);
        median_bpt(&mut s, 800.0, 5);
        median_bpt(&mut s, 400.0, 5);
        assert_eq!(s.len(), 3);
        // 奇数个样本取正中；偶数个取上中位元素（[400,500,600,800] → 600）
        assert!((median_bpt(&mut s, 500.0, 5) - 600.0).abs() < 1e-9);
    }

    #[test]
    fn prior_sample_prevents_single_sample_takeover() {
        // 冷启动：预置先验后，单个异常样本（如 bpt=179）不能独占系数
        let mut s = VecDeque::new();
        s.push_back(DEFAULT_BPT);
        let b1 = median_bpt(&mut s, 179.0, 5);
        assert!((b1 - DEFAULT_BPT).abs() < 1e-9, "单个样本不应撼动先验: {b1}");
        // 两个真实样本开始推动中位数（[179, 552, 600] → 552）
        let b2 = median_bpt(&mut s, 552.0, 5);
        assert!((b2 - 552.0).abs() < 1e-9);
    }

    /// 样本准入用例取自真实调试日志（2026-09-17 现场）：
    /// 管道静默调用会产生 ~7 B/token 的垃圾样本，必须整条拒绝而不是钳位后入队
    #[test]
    fn cal_sample_rejects_silent_pipe_calls() {
        // 静默调用：887 token，管道积分仅 5.9KB，原始字节 ~1MB → 拒绝
        let (ok, _) = cal_sample(887, 5_939.0, 1_048_576.0);
        assert!(!ok);
        // 正常调用：1028 token，清洗 526.5KB / 原始 ~900KB → 接受，样本 ≈524
        let (ok, v) = cal_sample(1028, 526.5 * 1024.0, 900.0 * 1024.0);
        assert!(ok);
        assert!((v - 524.0).abs() < 15.0);
        // 小调用：64 token → 拒绝
        assert!(!cal_sample(64, 50_000.0, 80_000.0).0);
        // 偏瘦但真实：449 token，清洗 67.5KB（比例 ~150 B/token，占原始 52%）→ 接受
        let (ok, v) = cal_sample(449, 67.5 * 1024.0, 130.0 * 1024.0);
        assert!(ok);
        assert!((v - 150.0).abs() < 5.0);
        // 超界比例（>6000）→ 拒绝
        assert!(!cal_sample(500, 500.0 * 6000.0 * 1.1, 500.0 * 6000.0 * 1.2).0);
    }

    /// 合成端到端：按 measure() 的口径驱动清洗/积分/校准，
    /// 断言「校准后显示值 ≈ 真值」。
    ///
    /// 场景对齐实测现场：30s 调用、真值 50 t/s、UI 管道 600 B/token、
    /// 落盘镜像 50% 流式字节、心跳底噪、被毒化的自适应底噪、首拍请求突发。
    #[test]
    fn synthetic_call_converges_to_true_tps() {
        const TRUE_TPS: f64 = 50.0;
        const BPT_TRUE: f64 = 600.0;
        const FLUSH_RATIO: f64 = 0.5; // 落盘镜像一半流式字节
        const NOISE: f64 = 1_500.0; // 心跳/日志底噪（并入写字节）

        let gen_ms = 30_000i64;
        let n = (gen_ms / TICK) as usize; // 43 拍
        let stream_per_tick = TRUE_TPS * BPT_TRUE * (TICK as f64 / 1000.0); // 21_000 B
        let mut deltas = Vec::with_capacity(n + 1);
        deltas.push(190_000.0); // 请求体上传（首拍突发，应被整拍剔除）
        for _ in 0..n {
            deltas.push(stream_per_tick + NOISE);
        }
        let t0 = 1_000_000i64;
        let samples = series(t0, &deltas);
        // 文件增长：与写入同拍可见
        let mut files = vec![(samples[0].0, 0u64)];
        for (i, d) in deltas.iter().enumerate() {
            let prev = files[i].1;
            files.push((samples[i + 1].0, prev + (*d * FLUSH_RATIO) as u64));
        }

        let rows = build_rows(&samples, &files, 40_000.0 /* 毒化的底噪 */);
        let call_end = t0 + (deltas.len() as i64) * TICK;
        let stream_start = call_end - gen_ms;

        // 一致性校准：分子 = 同一条清洗流在调用区间的积分
        let (clean, cov_s) = integrate(&rows, stream_start, call_end);
        let eff = (TRUE_TPS * (gen_ms as f64 / 1000.0)) as u64; // 1500 tok
        let bpt = (clean / eff as f64).clamp(CAL_MIN, CAL_MAX);
        assert!(
            cov_s > (gen_ms as f64 / 1000.0) * 0.9,
            "积分应覆盖调用区间: {cov_s}"
        );

        // 稳态显示：30s 滑窗满窗
        let (wb, ws) = integrate(&rows, call_end - WINDOW_MS, call_end);
        let pipe_bps = (wb / ws).max(0.0);
        let shown = pipe_bps / bpt;
        assert!(
            (shown - TRUE_TPS).abs() / TRUE_TPS < 0.05,
            "校准后显示 {shown:.1} 应接近真值 {TRUE_TPS}"
        );
    }

    /// 落盘 flush 延迟成大块（错位最恶劣情形）：正负拍在区间总和对消，
    /// 一致性校准仍收敛到真值
    #[test]
    fn delayed_flush_still_converges() {
        const TRUE_TPS: f64 = 50.0;
        const BPT_TRUE: f64 = 600.0;
        let gen_ms = 30_000i64;
        let n = (gen_ms / TICK) as usize;
        let stream_per_tick = TRUE_TPS * BPT_TRUE * (TICK as f64 / 1000.0);
        let deltas: Vec<f64> = std::iter::once(190_000.0)
            .chain(std::iter::repeat(stream_per_tick).take(n))
            .collect();
        let t0 = 1_000_000i64;
        let samples = series(t0, &deltas);
        // 全部落盘字节延迟到最后一拍一次性可见（单块大 flush）
        let total_flush: u64 = (deltas.iter().sum::<f64>() * 0.5) as u64;
        let mut files = Vec::with_capacity(deltas.len() + 1);
        for (i, (t, _)) in samples.iter().enumerate() {
            let v = if i == samples.len() - 1 { total_flush } else { 0 };
            files.push((*t, v));
        }
        let rows = build_rows(&samples, &files, 0.0);
        let call_end = t0 + (deltas.len() as i64) * TICK;
        let (clean, _) = integrate(&rows, call_end - gen_ms, call_end);
        let eff = (TRUE_TPS * (gen_ms as f64 / 1000.0)) as u64;
        let bpt = (clean / eff as f64).clamp(CAL_MIN, CAL_MAX);
        let (wb, ws) = integrate(&rows, call_end - WINDOW_MS, call_end);
        let shown = ((wb / ws).max(0.0)) / bpt;
        assert!(
            (shown - TRUE_TPS).abs() / TRUE_TPS < 0.05,
            "延迟 flush 场景显示 {shown:.1} 应接近真值 {TRUE_TPS}"
        );
    }
}
