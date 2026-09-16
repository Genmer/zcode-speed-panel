//! 验证工具：真实引擎（Engine+LiveIo，与面板同代码）+ 独立的原始每进程写字节采样，
//! 输出 JSONL 供事后与 usage DB 的真实 token 对账，评估实时速度与总量口径的偏差。
//! 用法：cargo run --example verify -- [秒数] [输出文件]
#[path = "../src/metrics.rs"]
mod metrics;
#[path = "../src/liveio.rs"]
mod liveio;

use metrics::Engine;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write;

#[cfg(windows)]
mod raw {
    use super::*;
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
    pub struct IoCounters {
        pub read_ops: u64,
        pub write_ops: u64,
        pub other_ops: u64,
        pub read_bytes: u64,
        pub write_bytes: u64,
        pub other_bytes: u64,
    }
    #[repr(C)]
    pub struct ProcessEntry32W {
        pub size: u32,
        pub usage: u32,
        pub process_id: u32,
        pub default_heap_id: usize,
        pub module_id: u32,
        pub threads: u32,
        pub parent_process_id: u32,
        pub pri_class_base: i32,
        pub flags: u32,
        pub exe_file: [u16; 260],
    }
    pub const PROCESS_QUERY_LIMITED: u32 = 0x1410;
    pub const TH32CS_SNAPPROCESS: u32 = 2;

    pub unsafe fn open(pid: u32) -> isize {
        OpenProcess(PROCESS_QUERY_LIMITED, 0, pid) as isize
    }
    pub unsafe fn write_bytes(h: isize) -> Option<u64> {
        let mut io = IoCounters {
            read_ops: 0,
            write_ops: 0,
            other_ops: 0,
            read_bytes: 0,
            write_bytes: 0,
            other_bytes: 0,
        };
        if GetProcessIoCounters(h as *mut c_void, &mut io) != 0 {
            Some(io.write_bytes)
        } else {
            None
        }
    }
    pub unsafe fn close(h: isize) {
        CloseHandle(h as *mut c_void);
    }

    pub fn command_line(pid: u32) -> Option<String> {
        unsafe {
            let h = open(pid);
            if h == 0 {
                return None;
            }
            let rd = |addr: usize, buf: &mut [u8]| -> bool {
                let mut n = 0usize;
                ReadProcessMemory(h as *mut c_void, addr as *const c_void, buf.as_mut_ptr().cast(), buf.len(), &mut n) != 0
            };
            let mut pbi = [0u8; 48];
            let mut ret: u32 = 0;
            if NtQueryInformationProcess(h as *mut c_void, 0, pbi.as_mut_ptr().cast(), 48, &mut ret) != 0 {
                close(h);
                return None;
            }
            let peb = usize::from_ne_bytes(pbi[8..16].try_into().ok()?);
            if peb == 0 {
                close(h);
                return None;
            }
            let mut pp_ptr = [0u8; 8];
            if !rd(peb + 0x20, &mut pp_ptr) {
                close(h);
                return None;
            }
            let pp = usize::from_ne_bytes(pp_ptr.try_into().ok()?);
            if pp == 0 {
                close(h);
                return None;
            }
            let mut us = [0u8; 16];
            if !rd(pp + 0x70, &mut us) {
                close(h);
                return None;
            }
            let len = u16::from_ne_bytes([us[0], us[1]]) as usize;
            let buf_ptr = usize::from_ne_bytes(us[8..16].try_into().ok()?);
            if len == 0 || buf_ptr == 0 {
                close(h);
                return None;
            }
            let mut wbuf = vec![0u8; len];
            if !rd(buf_ptr, &mut wbuf) {
                close(h);
                return None;
            }
            close(h);
            let u16s: Vec<u16> = wbuf.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect();
            Some(String::from_utf16_lossy(&u16s))
        }
    }

    /// 独立实现：枚举含 zcode.cjs 的 CLI 进程 → pid → 累计写字节
    pub fn sample_cli_writes() -> HashMap<u32, u64> {
        let mut out = HashMap::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == -1 {
                return out;
            }
            let mut e = ProcessEntry32W {
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
            if Process32FirstW(snap, &mut e) != 0 {
                loop {
                    let exe = String::from_utf16_lossy(
                        &e.exe_file[..e.exe_file.iter().position(|c| *c == 0).unwrap_or(260)],
                    );
                    if exe.eq_ignore_ascii_case("zcode.exe") {
                        let pid = e.process_id;
                        if let Some(cmd) = command_line(pid) {
                            if cmd.contains("zcode.cjs") {
                                let h = open(pid);
                                if h != 0 {
                                    if let Some(w) = write_bytes(h) {
                                        out.insert(pid, w);
                                    }
                                    close(h);
                                }
                            }
                        }
                    }
                    if Process32NextW(snap, &mut e) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap as *mut c_void);
        }
        out
    }
}

#[cfg(not(windows))]
mod raw {
    use super::*;
    pub fn sample_cli_writes() -> HashMap<u32, u64> {
        HashMap::new()
    }
}

fn tracked_files_total() -> u64 {
    let mut total = 0u64;
    if let Some(home) = metrics::home_dir() {
        for dir in [home.join(".zcode/cli/rollout"), home.join(".zcode/cli/log")] {
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

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(600);
    let out_path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "target/verify-log.jsonl".into());
    let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path).expect("create log"));

    let mut e = Engine::new();
    let mut li = liveio::LiveIo::new();
    let start = std::time::Instant::now();
    eprintln!("verify: 采样 {}s → {}", secs, out_path);

    while start.elapsed().as_secs() < secs {
        let tick = std::time::Instant::now();
        let new_calls = e.poll();
        let inflight = e.call_in_flight();
        let s = e.snapshot();
        let live = {
            li.observe(&new_calls);
            li.set_inflight(inflight);
            li.measure(s.now_ms)
        };
        let cal = li.take_calibration();
        let raw = raw::sample_cli_writes();
        let files = tracked_files_total();

        // 每轮调用完成后的真值统计（落盘数据反推）
        for c in &new_calls {
            let line = serde_json::json!({
                "kind": "call",
                "t": s.now_ms,
                "id": c.id,
                "sess": &c.session[c.session.len().saturating_sub(8)..],
                "done": c.completed_ms,
                "gen_ms": c.gen_ms,
                "eff": c.effective_out(),
                "true_tps": (c.effective_out() as f64) / (c.gen_ms.max(50) as f64 / 1000.0),
            });
            writeln!(out, "{}", line).ok();
        }
        // 校准事件（字节侧对单次调用的估计与样本）
        if let Some(cal) = &cal {
            let line = serde_json::json!({
                "kind": "cal",
                "t": s.now_ms,
                "id": cal.id,
                "bytes_kb": (cal.bytes / 1024.0 * 10.0).round() / 10.0,
                "bpt_sample": (cal.bpt_sample * 10.0).round() / 10.0,
                "bpt_now": (cal.bpt_now * 10.0).round() / 10.0,
                "skipped": cal.cal_skipped,
            });
            writeln!(out, "{}", line).ok();
        }

        let calls_json: Vec<serde_json::Value> = new_calls
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id, "session": c.session,
                    "completed_ms": c.completed_ms, "gen_ms": c.gen_ms,
                    "eff_out": c.effective_out(), "out": c.output, "rea": c.reasoning,
                    "inp": c.input, "cc": c.cache_creation, "cr": c.cache_read,
                })
            })
            .collect();
        let spark_tail: Vec<f64> = s.spark.iter().rev().take(3).rev().copied().collect();
        let line = serde_json::json!({
            "kind": "tick",
            "t": s.now_ms,
            "files": files,
            "raw": raw,
            "calls": calls_json,
            "snap": {
                "total": s.total_tokens, "out": s.output_tokens, "rea": s.reasoning_tokens,
                "inp": s.input_tokens, "cc": s.cache_creation_tokens, "cr": s.cache_read_tokens,
                "calls": s.calls_today, "tps": s.current_tps, "avg": s.avg_tps,
                "is_live": s.is_live, "est": s.is_estimating, "src": s.live_source, "win": (s.window_tps*10.0).round()/10.0,
            },
            "spark_tail": spark_tail,
            "bpt": (li.bytes_per_token() * 10.0).round() / 10.0,
            "live": { "avail": live.available, "stream": live.streaming, "ramp": live.ramping, "tps": live.tps },
        });
        writeln!(out, "{}", line).ok();
        out.flush().ok();

        let dt = tick.elapsed();
        if dt < std::time::Duration::from_millis(500) {
            std::thread::sleep(std::time::Duration::from_millis(500) - dt);
        }
    }
    eprintln!("verify: 完成");
}
