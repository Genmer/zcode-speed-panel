//! 实时速度实测：轮询 ZCode CLI 进程的 IO 写字节计数（往桌面 UI 管道的流式渲染数据），
//! 在模型流式输出期间该计数会以数十 KB/s 持续增长，是真实的实时信号。
//!
//! - 进程发现：Toolhelp 枚举 + NtQueryInformationProcess 读命令行，过滤 `zcode.cjs`
//! - 噪声底：每个进程按自身近窗口最小增量自适应扣除（待机心跳 1-2KB/s）
//! - 尖峰扣除：调用完成瞬间 rollout/日志/WAL 的落盘写入从字节增量中减去
//! - 字节→token 换算：每次调用完成后用真实 output_tokens ÷ 该调用区间实测字节
//!   做滑动自校准（初始系数来自实测约 1600 B/token）

use crate::metrics::Call;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

const WINDOW_MS: u64 = 10_000;
const RING_CAP: usize = 260; // ~130s @500ms
const REFRESH_EVERY: Duration = Duration::from_secs(30);
/// 流式判定阈值（扣除噪声底后）
const STREAMING_BPS: f64 = 4_000.0;
/// 待机心跳底噪的粗略上界（B/s），叠加每进程自适应底噪后足以过滤心跳
const BASE_NOISE_BPS: f64 = 3_000.0;
const DEFAULT_BPT: f64 = 1_600.0;
const CAL_MIN: f64 = 400.0;
const CAL_MAX: f64 = 8_000.0;

#[derive(Default, Clone)]
pub struct LiveNow {
    /// 是否成功发现了 CLI 进程（false 时前端回退到窗口/估算显示）
    pub available: bool,
    pub streaming: bool,
    pub tps: f64,
}

struct ProcRing {
    handle: isize,
    samples: VecDeque<(Instant, u64)>,
    /// 该进程最小的每拍增量（自适应心跳噪声底）
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
        files_hist: VecDeque<(Instant, u64)>,
        files_last_total: u64,
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
    }

    impl LiveIo {
        pub fn new() -> Self {
            Self {
                procs: HashMap::new(),
                last_refresh: None,
                files_hist: VecDeque::new(),
                files_last_total: 0,
                pending: VecDeque::new(),
                cal: VecDeque::new(),
                bytes_per_token: DEFAULT_BPT,
                last_result: LiveNow::default(),
                session_pid: HashMap::new(),
                current_session: None,
                attributed: HashSet::new(),
                history_done: false,
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

        /// 每个轮询周期调用一次
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

            // 采样本轮写字节
            let mut sample: HashMap<u32, u64> = HashMap::new();
            self.procs.retain(|pid, ring| {
                let w = io_write_bytes(ring.handle);
                match w {
                    Some(w) => {
                        ring.samples.push_back((now, w));
                        while ring.samples.len() > RING_CAP {
                            ring.samples.pop_front();
                        }
                        sample.insert(*pid, w);
                        true
                    }
                    None => false, // 进程已退出
                }
            });

            // 落盘文件总量历史（用于扣尖峰）
            let ft = self.tracked_files_total();
            self.files_hist.push_back((now, ft));
            while self.files_hist.len() > RING_CAP {
                self.files_hist.pop_front();
            }
            self.files_last_total = ft;
            let files: Vec<(Instant, u64)> = self.files_hist.iter().copied().collect();
            // [t0,t1] 区间内的落盘字节（rollout/日志/WAL），完成尖峰的来源
            let file_growth = |t0: Instant, t1: Instant| -> f64 {
                let mut acc = 0f64;
                for w in files.windows(2) {
                    let (pt, pv) = w[0];
                    let (ct, cv) = w[1];
                    if ct >= t0 && pt <= t1 {
                        acc += cv.saturating_sub(pv) as f64;
                    }
                }
                acc
            };

            // 10s 窗口字节速率：逐拍计算，先扣该拍的落盘字节再扣噪声底，
            // 避免调用完成瞬间的落盘尖峰在窗口内形成 ~10s 的假速度激增
            let mut clean: HashMap<u32, f64> = HashMap::new();
            let window_start = now - Duration::from_millis(WINDOW_MS as u64);
            for (pid, ring) in self.procs.iter_mut() {
                let pairs: Vec<(Instant, u64)> = ring.samples.iter().copied().collect();
                // 每拍最小增量的低分位 × 2 作为该进程的心跳噪声底（字节/拍）
                let mut deltas: Vec<f64> = pairs
                    .windows(2)
                    .map(|w| w[1].1.saturating_sub(w[0].1) as f64)
                    .collect();
                if !deltas.is_empty() {
                    deltas.sort_by(|a: &f64, b: &f64| a.partial_cmp(b).unwrap());
                    ring.min_delta = deltas[deltas.len() / 10] * 2.0;
                }
                let mut clean_sum = 0f64;
                let mut time_sum = 0f64;
                for w in pairs.windows(2) {
                    let (pt, pw) = w[0];
                    let (ct, cw) = w[1];
                    if ct < window_start {
                        continue;
                    }
                    let dt = ct.duration_since(pt).as_secs_f64();
                    if dt <= 0.0 {
                        continue;
                    }
                    // 该拍真实的 UI 管道写入 = 总写入 − 落盘写入
                    let pipe = (cw.saturating_sub(pw)) as f64 - file_growth(pt, ct);
                    let floor = ring.min_delta + BASE_NOISE_BPS * dt;
                    clean_sum += (pipe - floor).max(0.0);
                    time_sum += dt;
                }
                let rate = if time_sum > 1.0 {
                    clean_sum / time_sum
                } else {
                    0.0
                };
                clean.insert(*pid, rate);
            }

            // 流式判定与换算（先做归属与校准，再判定，保证同拍生效）
            // 自校准 + 会话→进程归属：用刚完成的调用（真实 output_tokens）÷ 流式区间实测写字节
            while let Some(call) = self.pending.front().cloned() {
                if now_ms - call.completed_ms > 120_000 {
                    self.pending.pop_front();
                    continue;
                }
                let stream_start_ms = (call.completed_ms - call.gen_ms.min(300_000)).max(0);
                let mut per_pid: HashMap<u32, f64> = HashMap::new();
                for (pid, ring) in self.procs.iter() {
                    let pairs: Vec<(Instant, u64)> = ring.samples.iter().copied().collect();
                    let mut acc = 0f64;
                    for w in pairs.windows(2) {
                        let (pt, pw) = w[0];
                        let (ct, cw) = w[1];
                        let pt_ms = now_ms - now.duration_since(pt).as_millis() as i64;
                        let ct_ms = now_ms - now.duration_since(ct).as_millis() as i64;
                        // 采样对落入流式区间即计入
                        if ct_ms >= stream_start_ms && pt_ms <= call.completed_ms {
                            acc += cw.saturating_sub(pw) as f64;
                        }
                    }
                    per_pid.insert(*pid, acc);
                }
                let bytes: f64 = per_pid.values().sum();
                // 扣除该区间内文件的落盘字节
                let mut file_bytes = 0f64;
                let fpairs: Vec<(Instant, u64)> = self.files_hist.iter().copied().collect();
                for w in fpairs.windows(2) {
                    let (pt, pv) = w[0];
                    let (ct, cv) = w[1];
                    let pt_ms = now_ms - now.duration_since(pt).as_millis() as i64;
                    let ct_ms = now_ms - now.duration_since(ct).as_millis() as i64;
                    if ct_ms >= stream_start_ms && pt_ms <= call.completed_ms {
                        file_bytes += cv.saturating_sub(pv) as f64;
                    }
                }
                // 会话归属：流式区间内写字节最多的进程即为该会话的 CLI 进程
                if !self.attributed.contains(&call.id) {
                    self.attributed.insert(call.id.clone());
                    if self.attributed.len() > 4_000 {
                        self.attributed.clear();
                    }
                    if bytes > 20_000.0 {
                        if let Some((pid, _)) =
                            per_pid.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                        {
                            self.session_pid.insert(call.session.clone(), *pid);
                        }
                    }
                }
                if call.effective_out() > 0 && bytes > file_bytes {
                    let bpt = ((bytes - file_bytes) / call.effective_out() as f64)
                        .clamp(CAL_MIN, CAL_MAX);
                    self.cal.push_back(bpt);
                    while self.cal.len() > 5 {
                        self.cal.pop_front();
                    }
                    let mut sorted: Vec<f64> = self.cal.iter().copied().collect();
                    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    self.bytes_per_token = sorted[sorted.len() / 2];
                }
                self.pending.pop_front();
            }

            // 只统计当前会话对应进程的写字节流，避免后台会话/ bots 污染状态
            let live_pid = self
                .current_session
                .as_ref()
                .and_then(|s| self.session_pid.get(s))
                .copied();
            let (clean_total, had_pid) = match live_pid {
                Some(pid) => (clean.get(&pid).copied().unwrap_or(0.0), true),
                None => (clean.values().sum(), false), // 无法归属时退化为全局
            };
            let streaming = clean_total > STREAMING_BPS;
            let tps = if streaming {
                clean_total / self.bytes_per_token
            } else {
                0.0
            };

            let result = LiveNow {
                available: !self.procs.is_empty(),
                streaming,
                tps,
            };
            let _ = had_pid;
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
    pub fn observe(&mut self, _new_calls: &[Call]) {}
    pub fn measure(&mut self, _now_ms: i64) -> LiveNow {
        LiveNow::default()
    }
}
