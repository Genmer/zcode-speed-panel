use chrono::{Datelike, Local, NaiveTime, Utc};
use rusqlite::OpenFlags;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// 一次已完成的模型调用（来自 ZCode usage 数据库 model_usage 表）
#[derive(Clone, Debug)]
pub struct Call {
    /// model_usage 主键
    #[allow(dead_code)]
    pub id: String,
    pub started_ms: i64,
    #[allow(dead_code)]
    pub first_token_ms: Option<i64>,
    pub completed_ms: i64,
    /// 纯生成时长：completed_at - first_token_at（缺失时退化为 duration_ms）
    pub gen_ms: i64,
    pub output: u64,
    pub reasoning: u64,
    pub input: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub session: String,
}

impl Call {
    /// 速率分子：输出 token + 思考 token（思考内容同样是流式输出）
    pub fn effective_out(&self) -> u64 {
        self.output + self.reasoning
    }
}

/// 推送给前端的指标快照
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub current_tps: f64,
    pub avg_tps: f64,
    pub total_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub calls_today: u64,
    pub sessions_today: u64,
    pub is_live: bool,
    /// 调用尚未落盘但按调用间隔推断仍在生成，速度为窗口回退值
    pub is_estimating: bool,
    /// 实测流式已开始但 30s 滑窗未填满（读数来自已活跃区间，前端显示"统计中"）
    pub ramping: bool,
    /// 调用已开始但尚未输出首字节（TTFT/管道未观测到增量）：显示"统计中…"提示
    /// 而不是误导性的估算值，前端表盘/桌宠显示 …
    pub is_starting: bool,
    /// 近 10 分钟已完成调用的真实速度（落盘口径，与速度曲线同源）。
    /// 部分调用期间 UI 管道无增量字节（IO 实测不可用），用它做回退显示
    pub window_tps: f64,
    /// 当前速度来源："io"=进程流实测 / "window"=窗口回退 / "idle"=待机
    pub live_source: String,
    pub last_activity_ms: i64,
    pub now_ms: i64,
    pub rollout_dir: String,
    pub spark: Vec<f64>,
}

/// 当前速度统计窗口
const LIVE_WINDOW_MS: i64 = 10 * 60 * 1000;
/// 距离最近一次调用完成超过该时长视为待机，当前速度归零
/// 估算窗口：按今日调用间隔中位数推断“仍在生成”，超出则待机
const ESTIMATE_MIN_MS: i64 = 20 * 1000;
const ESTIMATE_MAX_MS: i64 = 240 * 1000;
const ESTIMATE_DEFAULT_MS: i64 = 60 * 1000;
/// 极短生成时长的下限，避免除零/极端尖峰
const MIN_DUR_MS: i64 = 50;
/// 速度曲线：15 分钟，10 秒一档（对齐墙钟边界，便于前端平滑滚动）
const SPARK_BUCKETS: usize = 90;
const SPARK_BUCKET_MS: i64 = 10_000;

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn usage_db_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".zcode").join("cli").join("db").join("db.sqlite"))
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn local_midnight_utc_ms() -> i64 {
    let now = Local::now();
    let tz = now.timezone();
    now.date_naive()
        .and_time(NaiveTime::MIN)
        .and_local_timezone(tz)
        .single()
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
        .unwrap_or_else(|| now.timestamp_millis() - 86_400_000)
}

/// 当日聚合器：持有今日全部调用，计算所有指标（纯函数，便于测试）
pub struct Aggregator {
    pub calls: Vec<Call>,
    pub today_ymd: (i32, u32, u32),
}

impl Aggregator {
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            today_ymd: {
                let n = Local::now();
                (n.year(), n.month(), n.day())
            },
        }
    }

    pub fn ingest(&mut self, call: Call) {
        self.calls.push(call);
    }

    /// 跨天清理：清空今日累计
    pub fn rollover_if_needed(&mut self) {
        let n = Local::now();
        let ymd = (n.year(), n.month(), n.day());
        if ymd != self.today_ymd {
            self.today_ymd = ymd;
            self.calls.clear();
        }
    }

    pub fn calls(&self) -> &[Call] {
        &self.calls
    }

    pub fn snapshot(&self) -> Snapshot {
        let now = now_ms();
        let mut out_total = 0u64;
        let mut reason_total = 0u64;
        let mut input_total = 0u64;
        let mut cc_total = 0u64;
        let mut cr_total = 0u64;
        let mut dur_total = 0i64;
        let mut w_out = 0u64;
        let mut w_dur = 0i64;
        let mut last_completed = 0i64;
        let mut sessions: HashSet<&str> = HashSet::new();
        // 桶对齐墙钟 10s 边界：桶序号 = 完成时刻所属槽 与 当前槽 的差
        let now_slot = now.div_euclid(SPARK_BUCKET_MS);
        let mut buckets = vec![(0u64, 0i64); SPARK_BUCKETS];

        for c in &self.calls {
            out_total += c.output;
            reason_total += c.reasoning;
            input_total += c.input;
            cc_total += c.cache_creation;
            cr_total += c.cache_read;
            dur_total += c.gen_ms.max(MIN_DUR_MS);
            if !c.session.is_empty() {
                sessions.insert(c.session.as_str());
            }
            last_completed = last_completed.max(c.completed_ms);
            if c.completed_ms >= now - LIVE_WINDOW_MS {
                w_out += c.effective_out();
                w_dur += c.gen_ms.max(MIN_DUR_MS);
            }
            let slot = (now_slot - c.completed_ms.div_euclid(SPARK_BUCKET_MS)) as usize;
            if slot < SPARK_BUCKETS {
                let b = &mut buckets[SPARK_BUCKETS - 1 - slot];
                b.0 += c.effective_out();
                b.1 += c.gen_ms.max(MIN_DUR_MS);
            }
        }

        // 估算窗口：今日相邻完成时刻间隔的中位数（夹在 20s~240s），
        // 用于长思考/长输出期间（调用尚未落盘）继续按最近速度显示回退值
        let mut comps: Vec<i64> = self.calls.iter().map(|c| c.completed_ms).collect();
        comps.sort_unstable();
        comps.dedup();
        let mut gaps: Vec<i64> = comps
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|g| *g > 0 && *g < 600_000)
            .collect();
        let grace_ms = if gaps.len() >= 3 {
            let start = gaps.len().saturating_sub(10);
            let tail = &mut gaps[start..];
            tail.sort_unstable();
            (tail[tail.len() / 2]).clamp(ESTIMATE_MIN_MS, ESTIMATE_MAX_MS)
        } else {
            ESTIMATE_DEFAULT_MS
        };

        let since = now - last_completed;
        // is_live 只由实时 IO 实测决定（main 中覆写）；此处按调用间隔给出窗口回退
        let is_estimating = last_completed > 0 && since <= grace_ms && w_dur > 0;
        let current_tps = if is_estimating && w_dur > 0 {
            w_out as f64 / (w_dur as f64 / 1000.0)
        } else {
            0.0
        };
        let avg_tps = if dur_total > 0 {
            (out_total + reason_total) as f64 / (dur_total as f64 / 1000.0)
        } else {
            0.0
        };
        let mut spark: Vec<f64> = buckets
            .iter()
            .map(|(o, d)| {
                if *d > 0 {
                    *o as f64 / (*d as f64 / 1000.0)
                } else {
                    0.0
                }
            })
            .collect();
        // 估算期把最右一档（当前未落盘的调用）临时填成回退值，完成后被真实数据替换
        if is_estimating {
            if let Some(last) = spark.last_mut() {
                if *last <= 0.0 {
                    *last = current_tps;
                }
            }
        }
        let window_tps = if w_dur > 0 {
            w_out as f64 / (w_dur as f64 / 1000.0)
        } else {
            0.0
        };

        // 总量口径与 ZCode 官方统计一致：input + output + reasoning + cache_creation，
        // 缓存命中（cache_read）是提示复用、不是新增用量，单独展示不计入
        Snapshot {
            current_tps,
            avg_tps,
            total_tokens: out_total + reason_total + input_total + cc_total,
            output_tokens: out_total,
            reasoning_tokens: reason_total,
            input_tokens: input_total,
            cache_creation_tokens: cc_total,
            cache_read_tokens: cr_total,
            calls_today: self.calls.len() as u64,
            sessions_today: sessions.len() as u64,
            is_live: false,
            is_estimating,
            ramping: false,
            is_starting: false,
            window_tps,
            live_source: if is_estimating {
                "window".to_string()
            } else {
                "idle".to_string()
            },
            last_activity_ms: last_completed,
            now_ms: now,
            rollout_dir: String::new(),
            spark,
        }
    }
}

/// ZCode usage 数据库（只读 WAL）轮询引擎
pub struct Engine {
    conn: Option<rusqlite::Connection>,
    agg: Aggregator,
    ingested: HashSet<String>,
    pub db_path: Option<PathBuf>,
}

impl Engine {
    pub fn new() -> Self {
        let db_path = usage_db_path();
        let conn = db_path.as_ref().and_then(|p| {
            match rusqlite::Connection::open_with_flags(
                p,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("[zcode-speed-panel] usage DB open failed: {e}");
                    None
                }
            }
        });
        Self {
            conn,
            agg: Aggregator::new(),
            ingested: HashSet::new(),
            db_path,
        }
    }

    pub fn data_source_label(&self) -> String {
        self.db_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "(未找到 ~/.zcode/cli/db/db.sqlite)".into())
    }

    /// 轮询 usage 数据库，增量摄取今日完成的调用。返回本轮新增（供实时 IO 模块校准）。
    pub fn poll(&mut self) -> Vec<Call> {
        self.agg.rollover_if_needed();
        let today_start_ms = local_midnight_utc_ms();
        // 跨天时重置已摄取集合
        if self.agg.calls.is_empty() && !self.ingested.is_empty() {
            self.ingested.clear();
        }
        let Some(conn) = &self.conn else {
            return Vec::new();
        };
        let mut new_calls = Vec::new();
        let sql = concat!(
            "SELECT id, started_at, first_token_at, completed_at, duration_ms, ",
            "output_tokens, reasoning_tokens, input_tokens, ",
            "cache_creation_input_tokens, cache_read_input_tokens, session_id ",
            "FROM model_usage WHERE status='completed' AND completed_at >= ?1 ",
            "ORDER BY completed_at ASC"
        );
        let mut stmt = match conn.prepare_cached(sql) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[zcode-speed-panel] usage DB prepare failed: {e}");
                return Vec::new();
            }
        };
        let rows = stmt
            .query_map([today_start_ms], |r| {
            let id: String = r.get(0)?;
            let started: i64 = r.get(1)?;
            let ft: Option<i64> = r.get(2)?;
            let completed: i64 = r.get(3)?;
            let dur: Option<i64> = r.get(4)?;
            // rusqlite 不支持 u64 列读取，按 i64 取再转
            let out: i64 = r.get::<_, Option<i64>>(5)?.unwrap_or(0);
            let reason: i64 = r.get::<_, Option<i64>>(6)?.unwrap_or(0);
            let input: i64 = r.get::<_, Option<i64>>(7)?.unwrap_or(0);
            let cc: i64 = r.get::<_, Option<i64>>(8)?.unwrap_or(0);
            let cr: i64 = r.get::<_, Option<i64>>(9)?.unwrap_or(0);
            let session: String = r.get(10)?;
            Ok((
                id,
                started,
                ft,
                completed,
                dur,
                out.max(0) as u64,
                reason.max(0) as u64,
                input.max(0) as u64,
                cc.max(0) as u64,
                cr.max(0) as u64,
                session,
            ))
        });
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[zcode-speed-panel] usage DB query failed: {e}");
                return Vec::new();
            }
        };
        for row in rows.flatten() {
            let (id, started, ft, completed, dur, out, reason, input, cc, cr, session) = row;
            if self.ingested.contains(&id) {
                continue;
            }
            self.ingested.insert(id.clone());
            let gen_ms = match ft {
                Some(f) if completed > f => completed - f,
                _ => dur.filter(|d| *d > 0).unwrap_or(MIN_DUR_MS),
            };
            self.agg.ingest(Call {
                id,
                started_ms: started,
                first_token_ms: ft,
                completed_ms: completed,
                gen_ms,
                output: out,
                reasoning: reason,
                input,
                cache_creation: cc,
                cache_read: cr,
                session,
            });
            new_calls.push(self.agg.calls.last().unwrap().clone());
        }
        // 今日聚合只保留今日数据（摄取集合在跨天 rollover 时重置）
        self.agg.calls.retain(|c| c.started_ms >= today_start_ms);
        new_calls
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut s = self.agg.snapshot();
        s.rollout_dir = self.data_source_label();
        s
    }

    /// 今日已摄取的全部调用（供实时模块确定当前会话）
    pub fn calls(&self) -> &[Call] {
        &self.agg.calls()
    }

    /// 是否有调用正在进行：看最近活跃会话的最新 assistant 消息行。
    /// message 行在调用开始瞬间即提交（≤200ms 可读），行内 data 的 time 对象在
    /// 调用结束（含取消/出错）时补写 completed 字段——比 model_usage 完成行更快、
    /// 且覆盖 status='cancelled'/'error'（这两种调用永远没有 completed 状态行，
    /// 旧口径下会卡"生成中"直到 10 分钟兜底）。
    /// 返回 (会话, 调用开始时刻)。10 分钟上限兜底崩溃后无人补写 completed 的行。
    pub fn call_in_flight(&self) -> Option<(String, i64)> {
        let conn = self.conn.as_ref()?;
        // 最近活跃会话（session 表 ~1k 行，按 time_updated 倒序小表扫描可接受）；
        // message 表缺 time_created 单列索引，不能全局 ORDER BY（实测 ~200ms/次）
        let mut stmt = match conn.prepare_cached(
            "SELECT id FROM session ORDER BY time_updated DESC LIMIT 6",
        ) {
            Ok(s) => s,
            Err(_) => return None,
        };
        let sessions: Vec<String> = match stmt.query_map([], |r| r.get::<_, String>(0)) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => return None,
        };
        drop(stmt);

        let mut cands: Vec<(String, i64, bool)> = Vec::new();
        for sess in &sessions {
            // 每会话只看最新一条 assistant 行（走 (session_id, time_created) 复合索引）
            let Ok(mut stmt) = conn.prepare_cached(
                "SELECT time_created, substr(data,1,120) FROM message \
                 WHERE session_id = ?1 ORDER BY time_created DESC LIMIT 8",
            ) else {
                continue;
            };
            let Ok(rows) = stmt.query_map([sess], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            }) else {
                continue;
            };
            for (created, prefix) in rows.flatten() {
                if !prefix.contains("\"assistant\"") {
                    continue;
                }
                let done = prefix.contains("\"completed\"");
                cands.push((sess.clone(), created, done));
                break;
            }
        }
        inflight_from_rows(&cands, Utc::now().timestamp_millis())
    }
}

/// message 门控纯判定：候选 (会话, assistant 行创建时刻, 是否已带 completed)。
/// 每会话只认最新一条 assistant 行（更老的未完成行是崩溃残留，已被更新行覆盖），
/// 其中任一会话的最新行未完成且新鲜 → 有调用进行中，取创建时刻最新的一条
pub(crate) fn inflight_from_rows(
    cands: &[(String, i64, bool)],
    now_ms: i64,
) -> Option<(String, i64)> {
    let mut newest: HashMap<&str, &(String, i64, bool)> = HashMap::new();
    for row in cands {
        match newest.get(row.0.as_str()) {
            Some(prev) if prev.1 >= row.1 => {}
            _ => {
                newest.insert(row.0.as_str(), row);
            }
        }
    }
    newest
        .values()
        .filter(|(_, created, done)| !done && now_ms - *created <= 600_000)
        .max_by_key(|(_, created, _)| *created)
        .map(|(s, c, _)| (s.clone(), *c))
}

// ============ 测试 ============
#[cfg(test)]
mod tests {
    use super::*;

    /// message 门控：未带 completed 的最新 assistant 行 → 进行中；
    /// 已完成的行、超龄的僵尸行（崩溃兜底）、以及"同会话更新的已完成行"都不算
    #[test]
    fn inflight_from_rows_gating() {
        let now = 1_000_000i64;
        // 新会话首条调用：行未完成 → 进行中
        let r = inflight_from_rows(&[("new".into(), now - 3_000, false)], now);
        assert_eq!(r, Some(("new".to_string(), now - 3_000)));
        // 同会话有更新的已完成 assistant 行（旧僵尸行在上）→ 不算
        assert_eq!(
            inflight_from_rows(
                &[
                    ("a".into(), now - 60_000, false),      // 崩溃残留
                    ("a".into(), now - 30_000, true),       // 会话 a 最新 assistant 行
                ],
                now
            ),
            None
        );
        // 多会话并发：取最新未完成行（子 agent 会话 b 晚于主会话 a 开始）
        let r = inflight_from_rows(
            &[
                ("a".into(), now - 40_000, true),
                ("b".into(), now - 5_000, false),
            ],
            now,
        );
        assert_eq!(r, Some(("b".to_string(), now - 5_000)));
        // 未完成但超过 10 分钟兜底 → 判停
        assert_eq!(
            inflight_from_rows(&[("z".into(), now - 601_000, false)], now),
            None
        );
        assert_eq!(inflight_from_rows(&[], now), None);
    }

    fn call(completed: i64, gen_ms: i64, out: u64, reason: u64, input: u64, session: &str) -> Call {
        Call {
            id: format!("{}-{}", completed, out),
            started_ms: completed - gen_ms - 1000,
            first_token_ms: Some(completed - gen_ms),
            completed_ms: completed,
            gen_ms,
            output: out,
            reasoning: reason,
            input,
            cache_creation: 0,
            cache_read: 0,
            session: session.into(),
        }
    }

    #[test]
    fn snapshot_computes_speeds() {
        let now = now_ms();
        let mut agg = Aggregator::new();
        agg.ingest(call(now - 10_000, 10_000, 500, 40, 100, "a"));
        agg.ingest(call(now - 1_000, 8_000, 240, 60, 100, "a"));
        let s = agg.snapshot();
        // 纯生成速率：(500+40 + 240+60) / 18s = 46.7
        assert!((s.avg_tps - 840.0 / 18.0).abs() < 1e-9);
        assert!((s.current_tps - 840.0 / 18.0).abs() < 1e-9);
        assert!(!s.is_live); // is_live 仅由实时 IO 实测在 main 中覆写
        // 总 token = 输出 740 + 思考 100 + 输入 200
        assert_eq!(s.total_tokens, 1040);
        assert_eq!(s.output_tokens, 740);
        assert_eq!(s.reasoning_tokens, 100);
        assert_eq!(s.sessions_today, 1);
        assert_eq!(s.spark.len(), SPARK_BUCKETS);
        assert_eq!(s.live_source, "window"); // 无 IO 探测时 is_live=false → 窗口回退
    }

    #[test]
    fn speed_excludes_time_before_first_token() {
        let now = now_ms();
        let mut agg = Aggregator::new();
        agg.ingest(call(now - 5_000, 5_000, 500, 0, 0, "a"));
        let s1 = agg.snapshot();
        assert!((s1.avg_tps - 100.0).abs() < 1e-9);
        // 调用 B：30s 前就发出请求（长 TTFT/排队），但纯生成为 5s、输出 500
        agg.ingest(Call {
            id: "b".into(),
            started_ms: now - 30_000,
            first_token_ms: Some(now - 6_000),
            completed_ms: now - 1_000,
            gen_ms: 5_000,
            output: 500,
            reasoning: 0,
            input: 0,
            cache_creation: 0,
            cache_read: 0,
            session: "b".into(),
        });
        let s2 = agg.snapshot();
        // 分母用 completed - first_token（不含首 token 前的等待），仍是 100 t/s
        assert!((s2.avg_tps - 100.0).abs() < 1e-9);
    }
}
