#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod liveio;
mod metrics;

use liveio::LiveIo;
use metrics::{home_dir, Engine, ModelStatsPayload, Snapshot};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WindowEvent};

/// 窗口显示模式：完整面板 / 悬浮窗
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Full,
    Float,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Full => "full",
            Mode::Float => "float",
        }
    }
    fn parse(s: &str) -> Mode {
        if s.trim() == "float" {
            Mode::Float
        } else {
            Mode::Full
        }
    }
}

/// 悬浮窗样式：迷你仪表盘 / 速度胶囊 / 桌宠
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FloatStyle {
    Gauge,
    Pill,
    Pet,
}

impl FloatStyle {
    fn as_str(self) -> &'static str {
        match self {
            FloatStyle::Gauge => "gauge",
            FloatStyle::Pill => "pill",
            FloatStyle::Pet => "pet",
        }
    }
    fn parse(s: &str) -> FloatStyle {
        match s.trim() {
            "pill" => FloatStyle::Pill,
            "pet" => FloatStyle::Pet,
            _ => FloatStyle::Gauge,
        }
    }
}

/// 持久化状态：模式、样式与两种模式各自记住的窗口位置/桌宠尺寸。
/// 桌宠位置与完整面板位置互相独立——收起为桌宠时桌宠回到自己上次的位置
/// （无记忆时锚定窗体中心，而不是窗体左上角），展开时窗体回到自己的老位置。
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct Persisted {
    mode: String,
    style: String,
    /// 完整面板上次位置（物理像素）
    #[serde(default)]
    full_pos: Option<(i32, i32)>,
    /// 悬浮窗上次位置（物理像素）
    #[serde(default)]
    float_pos: Option<(i32, i32)>,
    /// 桌宠悬浮窗边长（逻辑像素）
    #[serde(default)]
    pet_size: Option<f64>,
}

struct AppState {
    engine: Mutex<Engine>,
    mode: Mutex<Mode>,
    style: Mutex<FloatStyle>,
    live: Mutex<LiveIo>,
    debug: Mutex<DebugLog>,
    persist: Mutex<Persisted>,
    /// 位置落盘节流（拖动期间每 2s 一次，关闭/退出立即落盘）
    last_pos_save: Mutex<Option<std::time::Instant>>,
    /// 托盘菜单顶部的状态项（disabled，仅展示生成状态）
    tray_status: Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    /// 上次写入状态项/托盘 tooltip 的状态文本（变化才更新，避免每拍 churn）
    tray_status_last: Mutex<String>,
    /// mac 启动引导提示是否待领取（一次性）：setup 在事件循环前执行，
    /// 此时 emit 必然早于页面加载被丢弃，改为前端就绪后 invoke 领取
    tray_hint_pending: Mutex<bool>,
    /// macOS 无边框多屏安全最大化记忆：(还原物理坐标, 还原物理尺寸)
    saved_max_rect: Mutex<Option<(PhysicalPosition<i32>, PhysicalSize<u32>)>>,
}

/// 调试日志：记录实时显示值、统计值与每轮调用完成后的真值，
/// 供"实时读数 vs 落盘统计"的偏差分析。JSONL 追加写，超限轮转保留一代；
/// 轮转出的旧文件超过 7 天在启动时自动清理。
struct DebugLog {
    file: Option<fs::File>,
    written: u64,
    last_heartbeat: std::time::Instant,
}

const DEBUG_LOG_MAX: u64 = 8 * 1024 * 1024;
/// 轮转旧日志的保留时长
const DEBUG_LOG_KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 86400);

impl DebugLog {
    fn new() -> Self {
        let mut log = DebugLog { file: None, written: 0, last_heartbeat: std::time::Instant::now() };
        log.cleanup_rotated();
        log.reopen();
        log
    }

    fn path() -> Option<PathBuf> {
        home_dir().map(|h| h.join(".zcode").join("speed-panel-debug.jsonl"))
    }

    /// 自动清理：删除超过保留期的轮转日志（speed-panel-debug.jsonl.N）
    fn cleanup_rotated(&mut self) {
        let Some(p) = DebugLog::path() else { return };
        let Some(dir) = p.parent() else { return };
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if !e.file_name().to_string_lossy().starts_with("speed-panel-debug.jsonl.") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if let Ok(mtime) = meta.modified() {
                if mtime < std::time::SystemTime::now() - DEBUG_LOG_KEEP {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    fn reopen(&mut self) {
        if let Some(p) = DebugLog::path() {
            if let Ok(meta) = fs::metadata(&p) {
                self.written = meta.len();
            }
            self.file = fs::OpenOptions::new().create(true).append(true).open(&p).ok();
        }
    }

    fn write(&mut self, value: serde_json::Value) {
        use std::io::Write;
        if self.written > DEBUG_LOG_MAX {
            self.file = None;
            if let Some(p) = DebugLog::path() {
                let _ = fs::rename(&p, p.with_extension("jsonl.1"));
            }
            self.written = 0;
            self.reopen();
        }
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{}", value);
            self.written += value.to_string().len() as u64 + 1;
        }
    }
}

const FULL_SIZE: (f64, f64) = (1000.0, 700.0);
const FLOAT_GAUGE_SIZE: (f64, f64) = (116.0, 116.0);
const FLOAT_PILL_SIZE: (f64, f64) = (224.0, 78.0);
/// 桌宠默认边长（逻辑像素），滚轮缩放范围 [100, 480]
const FLOAT_PET_SIZE: f64 = 200.0;
const PET_SIZE_MIN: f64 = 100.0;
const PET_SIZE_MAX: f64 = 480.0;

fn mode_file() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".zcode").join("speed-panel-mode.txt"))
}

fn load_persisted() -> Persisted {
    let raw = mode_file().and_then(|p| fs::read_to_string(p).ok());
    match raw {
        Some(s) => match serde_json::from_str::<Persisted>(&s) {
            Ok(p) => p,
            // 旧格式：纯文本 "full"/"float"
            Err(_) => Persisted {
                mode: s,
                ..Default::default()
            },
        },
        None => Persisted::default(),
    }
}

fn save_all(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mode = *state.mode.lock().unwrap();
    let style = *state.style.lock().unwrap();
    let p = state.persist.lock().unwrap().clone();
    if let Some(path) = mode_file() {
        let json = serde_json::json!({
            "mode": mode.as_str(),
            "style": style.as_str(),
            "full_pos": p.full_pos,
            "float_pos": p.float_pos,
            "pet_size": p.pet_size,
        });
        let _ = fs::write(path, json.to_string());
    }
}

/// 把窗口完整拉回它所在显示器的可见区域（多屏时以窗口当前点定位）
fn clamp_to_screen(window: &tauri::WebviewWindow, x: i32, y: i32, w: u32, h: u32) -> (i32, i32) {
    let monitor = window
        .monitor_from_point(x as f64, y as f64)
        .ok()
        .flatten()
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(m) = monitor else {
        return (x, y);
    };
    let mp = m.position();
    let ms = m.size();
    let max_x = (mp.x + ms.width as i32 - w as i32).max(mp.x);
    let max_y = (mp.y + ms.height as i32 - h as i32).max(mp.y);
    (x.clamp(mp.x, max_x), y.clamp(mp.y, max_y))
}

fn apply_mode(window: &tauri::WebviewWindow, mode: Mode, style: FloatStyle, p: &Persisted) {
    let scale = window.scale_factor().unwrap_or(1.0);
    match mode {
        Mode::Full => {
            let _ = window.set_min_size(Some(LogicalSize::new(720.0, 520.0)));
            let _ = window.set_size(LogicalSize::new(FULL_SIZE.0, FULL_SIZE.1));
            // 无边框：顶栏为前端自绘（拖动/双击最大化/— ▢ ✕），不再恢复系统装饰
            let _ = window.set_decorations(false);
            let _ = window.set_resizable(true);
            let _ = window.set_always_on_top(false);
            let _ = window.set_skip_taskbar(false);
            let _ = window.set_shadow(true);
            // 回到完整面板自己的老位置（无记忆时保持当前左上角，钳回可见区域）
            if let Some((x, y)) = p.full_pos {
                let (px, py) = clamp_to_screen(
                    window,
                    x,
                    y,
                    (FULL_SIZE.0 * scale) as u32,
                    (FULL_SIZE.1 * scale) as u32,
                );
                let _ = window.set_position(PhysicalPosition::new(px, py));
            }
        }
        Mode::Float => {
            let (w, h) = match style {
                FloatStyle::Gauge => FLOAT_GAUGE_SIZE,
                FloatStyle::Pill => FLOAT_PILL_SIZE,
                FloatStyle::Pet => {
                    let s = p.pet_size.unwrap_or(FLOAT_PET_SIZE).clamp(PET_SIZE_MIN, PET_SIZE_MAX);
                    (s, s)
                }
            };
            let _ = window.set_min_size(None::<LogicalSize<f64>>);
            let _ = window.set_size(LogicalSize::new(w, h));
            let _ = window.set_decorations(false);
            let _ = window.set_resizable(false);
            // 悬浮窗：置顶、不占任务栏、无原生阴影（阴影会盖住圆角外透明区）
            let _ = window.set_always_on_top(true);
            let _ = window.set_skip_taskbar(true);
            let _ = window.set_shadow(false);
            // 位置：桌宠/悬浮窗自己上次的位置；无记忆时锚定当前窗体中心
            //（而不是跟随左上角——旧版收起后桌宠总落在原窗体左上角的问题）
            let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
            let target = match p.float_pos {
                Some((x, y)) => clamp_to_screen(window, x, y, pw, ph),
                None => {
                    let cur = window.outer_position().unwrap_or_default();
                    let sz = window.outer_size().unwrap_or_default();
                    let cx = cur.x + sz.width as i32 / 2;
                    let cy = cur.y + sz.height as i32 / 2;
                    clamp_to_screen(window, cx - pw as i32 / 2, cy - ph as i32 / 2, pw, ph)
                }
            };
            let _ = window.set_position(PhysicalPosition::new(target.0, target.1));
        }
    }
}

fn switch_mode(app: &AppHandle, mode: Mode) {
    let state = app.state::<AppState>();
    let style = *state.style.lock().unwrap();
    let prev = *state.mode.lock().unwrap();
    if mode == Mode::Float {
        *state.saved_max_rect.lock().unwrap() = None;
    }
    // 记住旧模式下窗口的位置（两种模式各自独立记忆）
    if let Some(win) = app.get_webview_window("main") {
        if let Ok(pos) = win.outer_position() {
            let mut p = state.persist.lock().unwrap();
            match prev {
                Mode::Full => p.full_pos = Some((pos.x, pos.y)),
                Mode::Float => p.float_pos = Some((pos.x, pos.y)),
            }
        }
    }
    // 先更新模式再应用新尺寸/位置：应用过程触发的 Moved 事件按新模式回写
    *state.mode.lock().unwrap() = mode;
    let p = state.persist.lock().unwrap().clone();
    if let Some(window) = app.get_webview_window("main") {
        apply_mode(&window, mode, style, &p);
    }
    save_all(app);
    let _ = app.emit("mode", mode.as_str());
}

/// 折叠为悬浮窗：完整面板 → 切换悬浮窗模式；已在悬浮窗 → 唤起并聚焦。
/// CloseRequested / mac 菜单栏 Cmd+Q / ExitRequested 兜底共用
fn collapse_to_float(app: &AppHandle) {
    let mode = *app.state::<AppState>().mode.lock().unwrap();
    if mode == Mode::Full {
        switch_mode(app, Mode::Float);
    } else {
        show_main(app);
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPayload {
    snapshot: Snapshot,
    rollout_dir: String,
    mode: String,
    float_style: String,
}

fn build_payload(app: &AppHandle) -> SnapshotPayload {
    let state = app.state::<AppState>();
    let mode = *state.mode.lock().unwrap();
    let style = *state.style.lock().unwrap();
    let rollout_dir;
    let new_calls;
    let engine_calls: Vec<metrics::Call>;
    let inflight: Option<(String, i64)>;
    let mut snapshot;
    {
        let mut engine = state.engine.lock().unwrap();
        new_calls = engine.poll();
        snapshot = engine.snapshot();
        rollout_dir = engine.data_source_label();
        engine_calls = engine.calls().to_vec();
        inflight = engine.call_in_flight();
    }
    // 实时实测：进程 IO 写字节流（真实值），只统计当前活跃会话对应的 CLI 进程
    let now_ms = snapshot.now_ms;
    let cal_event;
    let bpt_now;
    let pipe_bps;
    {
        let mut live = state.live.lock().unwrap();
        if !live.history_done() {
            live.ingest_history(engine_calls.as_slice());
        }
        live.observe(&new_calls);
        live.set_inflight(inflight.clone());
        let live_now = live.measure(now_ms);
        cal_event = live.take_calibration();
        bpt_now = live.bytes_per_token();
        pipe_bps = live_now.pipe_bps;
        let ever_saw = live.ever_saw_procs();
        if live_now.available {
            if live_now.streaming {
                snapshot.is_live = true;
                snapshot.is_estimating = false;
                snapshot.ramping = live_now.ramping;
                snapshot.is_starting = live_now.awaiting;
                snapshot.live_source = "io".into();
                if live_now.awaiting {
                    // 启动期（门控已开、首字节未到）：显示"统计中…"提示，
                    // 不显示误导性的估算值
                    snapshot.current_tps = 0.0;
                } else if live_now.tps < 1.0 && snapshot.window_tps > 0.0 {
                    // 部分调用期间 UI 管道无增量字节（IO 实测为 0）：回退到近期
                    // 已完成调用的真实速度（与速度曲线同口径），标记 ≈ 估算
                    snapshot.current_tps = snapshot.window_tps;
                    snapshot.is_estimating = true;
                    snapshot.live_source = "window".into();
                } else {
                    snapshot.current_tps = live_now.tps;
                }
                if let Some(last) = snapshot.spark.last_mut() {
                    *last = snapshot.current_tps;
                }
            } else {
                // IO 可用但门控判定无调用 → 如实待机（真实值优先，不用估算掩盖）
                snapshot.is_estimating = false;
                snapshot.ramping = false;
                snapshot.current_tps = 0.0;
                snapshot.live_source = "idle".into();
                if let Some(last) = snapshot.spark.last_mut() {
                    *last = 0.0;
                }
            }
        } else if inflight.is_some() && !ever_saw {
            // IO 从未可用（IO 探测环境不可用 / 面板刚启动进程未发现）：
            // 按 message 门控决定，而不是按调用间隔盲估——有调用进行中才显示
            // （近期有真值则估算 ≈，否则"统计中…"提示），门控已停立即归零。
            // 旧口径按间隔中位数推断，调用结束后还会空转"估算中"最长 240s
            if !(snapshot.is_estimating && snapshot.current_tps > 0.0) {
                snapshot.is_estimating = false;
                snapshot.is_starting = true;
                snapshot.current_tps = 0.0;
                snapshot.live_source = "window".into();
                if let Some(last) = snapshot.spark.last_mut() {
                    *last = 0.0;
                }
            }
        } else {
            // 发现过进程但当前不可用（CLI 已全部退出），或门控已停 → 如实待机
            snapshot.is_live = false;
            snapshot.is_estimating = false;
            snapshot.ramping = false;
            snapshot.current_tps = 0.0;
            snapshot.live_source = "idle".into();
            if let Some(last) = snapshot.spark.last_mut() {
                *last = 0.0;
            }
        }

    // ---- 调试日志：实时显示值 / 统计值 / 每轮完成后的真值 ----
    {
        let state = app.state::<AppState>();
        let mut log = state.debug.lock().unwrap();
        for c in &new_calls {
            log.write(serde_json::json!({
                "kind": "call",
                "t": now_ms,
                "id": c.id,
                "sess": &c.session[c.session.len().saturating_sub(8)..],
                "done": c.completed_ms,
                "gen_ms": c.gen_ms,
                "eff": c.effective_out(),
                "true_tps": (c.effective_out() as f64) / (c.gen_ms.max(50) as f64 / 1000.0),
            }));
        }
        if let Some(cal) = &cal_event {
            // 对账：清洗流按本调用区间积分 ÷ 生成长秒 ÷ 当前系数 = 该调用期间
            // 显示口径的平均 t/s 预测，与落盘真值 true_tps 对比即可评估实时准确性
            let pred_tps = if cal.gen_ms > 0 && cal.bpt_now > 0.0 {
                cal.clean_bytes / (cal.gen_ms as f64 / 1000.0) / cal.bpt_now
            } else {
                0.0
            };
            log.write(serde_json::json!({
                "kind": "cal",
                "t": now_ms,
                "id": cal.id,
                "gen_ms": cal.gen_ms,
                "eff": cal.eff,
                "true_tps": (cal.true_tps * 10.0).round() / 10.0,
                "raw_kb": (cal.raw_bytes / 1024.0 * 10.0).round() / 10.0,
                "clean_kb": (cal.clean_bytes / 1024.0 * 10.0).round() / 10.0,
                "attr_pid": cal.attr_pid,
                "top_pid": cal.top_pid,
                "bpt_sample": (cal.bpt_sample * 10.0).round() / 10.0,
                "bpt_now": (cal.bpt_now * 10.0).round() / 10.0,
                "pred_tps": (pred_tps * 10.0).round() / 10.0,
                "skipped": cal.cal_skipped,
            }));
        }
        let active = snapshot.is_live || snapshot.is_estimating || snapshot.is_starting;
        let heartbeat = log.last_heartbeat.elapsed() > std::time::Duration::from_secs(30);
        if active || heartbeat {
            log.last_heartbeat = std::time::Instant::now();
            let tail: Vec<f64> = snapshot
                .spark
                .iter()
                .rev()
                .take(3)
                .rev()
                .map(|v| (v * 10.0).round() / 10.0)
                .collect();
            log.write(serde_json::json!({
                "kind": "tick",
                "t": now_ms,
                "src": snapshot.live_source,
                "tps": (snapshot.current_tps * 10.0).round() / 10.0,
                "pipe": (pipe_bps / 10.0).round() * 10.0,
                "stream": snapshot.is_live,
                "ramp": snapshot.ramping,
                "start": snapshot.is_starting,
                "est": snapshot.is_estimating,
                "bpt": (bpt_now * 10.0).round() / 10.0,
                "avg": (snapshot.avg_tps * 10.0).round() / 10.0,
                "spark_tail": tail,
                "calls": snapshot.calls_today,
            }));
        }
    }

    }
    SnapshotPayload {
        rollout_dir,
        snapshot,
        mode: mode.as_str().to_string(),
        float_style: style.as_str().to_string(),
    }
}

#[tauri::command]
fn snapshot(app: AppHandle) -> SnapshotPayload {
    build_payload(&app)
}

/// 模型速度趋势：只读查询 usage 库按模型 × 桶聚合（详情弹窗打开期间前端每 5s 拉取）。
/// 聚合在 Engine 内现算完成，零本地存储、不写入 usage 库
#[tauri::command]
fn model_stats(app: AppHandle, window_min: i64) -> ModelStatsPayload {
    let state = app.state::<AppState>();
    let engine = state.engine.lock().unwrap();
    engine.model_stats(window_min)
}

#[tauri::command]
fn set_mode(app: AppHandle, mode: String, style: Option<String>) {
    if let Some(s) = style {
        let st = FloatStyle::parse(&s);
        *app.state::<AppState>().style.lock().unwrap() = st;
    }
    switch_mode(&app, Mode::parse(&mode));
}

#[tauri::command]
fn set_float_style(app: AppHandle, style: String) {
    let st = FloatStyle::parse(&style);
    {
        let state = app.state::<AppState>();
        *state.style.lock().unwrap() = st;
        let mode = *state.mode.lock().unwrap();
        if mode == Mode::Float {
            if let Some(window) = app.get_webview_window("main") {
                let p = state.persist.lock().unwrap().clone();
                apply_mode(&window, mode, st, &p);
            }
        }
    }
    save_all(&app);
    let _ = app.emit("float-style", st.as_str());
}

/// 桌宠滚轮缩放：调整悬浮窗边长（逻辑像素）并持久化
#[tauri::command]
fn set_float_size(app: AppHandle, size: f64) {
    let size = size.clamp(PET_SIZE_MIN, PET_SIZE_MAX);
    {
        let state = app.state::<AppState>();
        state.persist.lock().unwrap().pet_size = Some(size);
        let mode = *state.mode.lock().unwrap();
        let style = *state.style.lock().unwrap();
        if mode == Mode::Float && style == FloatStyle::Pet {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_size(LogicalSize::new(size, size));
            }
        }
    }
    save_all(&app);
}

/// 悬浮窗右键菜单"退出"：保存状态后退出应用
#[tauri::command]
fn quit_app(app: AppHandle) {
    save_all(&app);
    app.exit(0);
}

/// mac 启动引导提示（一次性）：由前端页面就绪后主动 invoke 领取——
/// setup 内 emit 会早于 WKWebView 加载被丢弃。非 mac 恒返回 false
#[tauri::command]
fn tray_hint_once(app: AppHandle) -> bool {
    let state = app.state::<AppState>();
    let mut guard = state.tray_hint_pending.lock().unwrap();
    let pending = *guard;
    *guard = false;
    pending
}

fn toggle_window_maximize(window: &tauri::WebviewWindow) {
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

/// 多屏安全最大化/还原：macOS 无边框窗口原生 toggle_maximize 会跳回主屏，
/// 此处按窗口中心点所在显示器铺满（避让菜单栏）；Windows 直接调用系统最大化
#[tauri::command]
fn toggle_maximize_safe(window: tauri::WebviewWindow, state: tauri::State<'_, AppState>) {
    #[cfg(windows)]
    {
        toggle_window_maximize(&window);
    }
    #[cfg(target_os = "macos")]
    {
        let mut saved = state.saved_max_rect.lock().unwrap();
        if let Some((pos, size)) = saved.take() {
            // 已最大化，执行还原
            let _ = window.set_size(size);
            let _ = window.set_position(pos);
        } else {
            // 未最大化，执行安全最大化
            let cur_pos = window.outer_position().unwrap_or_default();
            let cur_size = window.outer_size().unwrap_or_default();
            *saved = Some((cur_pos, cur_size));

            let cx = cur_pos.x + cur_size.width as i32 / 2;
            let cy = cur_pos.y + cur_size.height as i32 / 2;
            let monitor = window
                .monitor_from_point(cx as f64, cy as f64)
                .ok()
                .flatten()
                .or_else(|| window.current_monitor().ok().flatten())
                .or_else(|| window.primary_monitor().ok().flatten());

            if let Some(m) = monitor {
                let scale = m.scale_factor();
                let mp = m.position();
                let ms = m.size();
                // 避让 macOS 顶部菜单栏高度约 28pt
                let top_margin = (28.0 * scale) as i32;
                let target_x = mp.x;
                let target_y = mp.y + top_margin;
                let target_w = ms.width;
                let target_h = ms.height.saturating_sub(top_margin as u32);

                let _ = window.set_position(PhysicalPosition::new(target_x, target_y));
                let _ = window.set_size(PhysicalSize::new(target_w, target_h));
            } else {
                toggle_window_maximize(&window);
            }
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        toggle_window_maximize(&window);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        // mac：窗口从隐藏→显示时提示"应用常驻菜单栏"（无 Dock 图标，用户
        // 关掉窗口后靠提示找回入口）；已可见（如重复启动唤起）不打扰
        #[cfg(target_os = "macos")]
        let was_hidden = !win.is_visible().unwrap_or(true);
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        #[cfg(target_os = "macos")]
        if was_hidden {
            let _ = app.emit("tray-hint", ());
        }
    }
}

fn hide_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

fn toggle_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) && !win.is_minimized().unwrap_or(false) {
            let _ = win.hide();
        } else {
            show_main(app);
        }
    }
}

/// 窗口移动：按当前模式回写位置到内存，节流落盘（拖动每 2s 最多一次）
fn on_window_moved(app: &AppHandle, pos: PhysicalPosition<i32>) {
    let state = app.state::<AppState>();
    let mode = *state.mode.lock().unwrap();
    {
        let mut p = state.persist.lock().unwrap();
        match mode {
            Mode::Full => p.full_pos = Some((pos.x, pos.y)),
            Mode::Float => p.float_pos = Some((pos.x, pos.y)),
        }
    }
    let due = {
        let mut last = state.last_pos_save.lock().unwrap();
        let due = last.map_or(true, |t| t.elapsed() > Duration::from_secs(2));
        if due {
            *last = Some(std::time::Instant::now());
        }
        due
    };
    if due {
        save_all(app);
    }
}

/// 托盘状态：菜单顶部状态项文本 + 托盘 tooltip。按快照状态生成
///（生成中/估算中/待机），文本变化才写（避免每 700ms 重复设置）
fn update_tray_status(app: &AppHandle, s: &Snapshot) {
    let state_word = if s.is_live || s.is_starting {
        "生成中"
    } else if s.is_estimating {
        "估算中"
    } else {
        "待机"
    };
    let text = if s.is_live || s.is_starting {
        format!("生成中 {:.1} t/s", s.current_tps)
    } else if s.is_estimating {
        format!("估算中 ≈{:.1} t/s", s.current_tps)
    } else {
        "待机".to_string()
    };
    let state = app.state::<AppState>();
    {
        let mut last = state.tray_status_last.lock().unwrap();
        if *last == text {
            return;
        }
        *last = text.clone();
    }
    if let Some(item) = state.tray_status.lock().unwrap().as_ref() {
        let _ = item.set_text(text);
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(&format!("ZCode 速度仪表盘 · {state_word}")));
    }
}

/// 后台轮询线程：增量解析 model-io 文件并推送快照
fn poller(app: AppHandle) {
    loop {
        let payload = build_payload(&app);
        update_tray_status(&app, &payload.snapshot);
        let _ = app.emit("metrics", &payload);
        std::thread::sleep(Duration::from_millis(700));
    }
}

fn main() {
    tauri::Builder::default()
        // 重复启动时唤起已有窗口
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .manage(AppState {
            engine: Mutex::new(Engine::new()),
            mode: Mutex::new(Mode::Full),
            style: Mutex::new(FloatStyle::Gauge),
            live: Mutex::new(LiveIo::new()),
            debug: Mutex::new(DebugLog::new()),
            persist: Mutex::new(Persisted::default()),
            last_pos_save: Mutex::new(None),
            tray_status: Mutex::new(None),
            tray_status_last: Mutex::new(String::new()),
            tray_hint_pending: Mutex::new(cfg!(target_os = "macos")),
            saved_max_rect: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            snapshot,
            model_stats,
            set_mode,
            set_float_style,
            set_float_size,
            quit_app,
            tray_hint_once,
            toggle_maximize_safe
        ])
        .setup(|app| {
            // mac：Accessory 模式——无 Dock 图标、不进 Cmd+Tab，常驻菜单栏托盘；
            // 必须在跑起来之前尽早设置（真退出只有托盘"退出"与悬浮窗右键"退出程序"）
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // mac：自定义应用菜单拦截 Cmd+Q 为"折叠为悬浮窗"（不注册系统
            // 退出项），并附编辑菜单保住 WebView 的 Cmd+C/V/X/A 快捷键
            #[cfg(target_os = "macos")]
            {
                macos_ui::install(app)?;
                app.on_menu_event(|app, ev| {
                    if ev.id().as_ref() == "collapse-to-float" {
                        save_all(app);
                        collapse_to_float(app);
                    }
                });
            }

            // ---- 系统托盘 ----
            // 顶部状态项（disabled 不可点，poller 每拍按快照刷新文本）
            let status = MenuItem::with_id(app, "status", "待机", false, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "隐藏到托盘", true, None::<&str>)?;
            let toggle_float =
                MenuItem::with_id(app, "toggle-float", "悬浮窗 / 完整面板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[&status, &sep, &show, &hide, &toggle_float, &quit],
            )?;
            app.state::<AppState>().tray_status.lock().unwrap().replace(status);

            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;
            TrayIconBuilder::with_id("main-tray")
                .icon(icon)
                .tooltip("ZCode 速度仪表盘")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "show" => show_main(app),
                    "hide" => hide_main(app),
                    "toggle-float" => {
                        let cur = *app.state::<AppState>().mode.lock().unwrap();
                        switch_mode(app, if cur == Mode::Float { Mode::Full } else { Mode::Float });
                    }
                    "quit" => {
                        save_all(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        toggle_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // ---- 关闭按钮 = 收起为悬浮窗；移动时记忆位置（两种模式各自独立） ----
            let win_handle = app.handle().clone();
            app.get_webview_window("main")
                .unwrap()
                .on_window_event(move |event| match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        // 点关闭 = 立刻变悬浮窗（不藏托盘；完全退出走托盘/右键菜单）
                        collapse_to_float(&win_handle);
                    }
                    WindowEvent::Moved(pos) => on_window_moved(&win_handle, *pos),
                    _ => {}
                });

            // ---- 恢复上次显示模式、样式与位置后再亮出窗口，避免闪一下完整尺寸 ----
            let persisted = load_persisted();
            let mode = Mode::parse(&persisted.mode);
            let style = FloatStyle::parse(&persisted.style);
            {
                let state = app.state::<AppState>();
                *state.mode.lock().unwrap() = mode;
                *state.style.lock().unwrap() = style;
                *state.persist.lock().unwrap() = persisted;
            }
            let window = app.get_webview_window("main").unwrap();
            let p = app.state::<AppState>().persist.lock().unwrap().clone();
            apply_mode(&window, mode, style, &p);
            let _ = window.show();
            // mac 启动引导提示不在此 emit：setup 早于事件循环/WKWebView 加载，
            // 发即被弃——改为前端就绪后 invoke `tray_hint_once` 领取（一次性）

            // ---- 启动轮询线程 ----
            let poll_handle = app.handle().clone();
            std::thread::spawn(move || poller(poll_handle));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building zcode-speed-panel")
        .run(|app, event| match event {
            // 兜底防线：非显式 exit(0) 的退出请求（如 mac 上最后的窗口关闭、
            // 系统注销前的退出）一律阻止并折叠为悬浮窗——真退出只有托盘
            // "退出"与悬浮窗右键"退出程序"两条路（app.exit 时 code=Some，放行）
            tauri::RunEvent::ExitRequested { code: None, api, .. } => {
                api.prevent_exit();
                save_all(app);
                collapse_to_float(app);
            }
            // 真退出前再保存一次（best-effort）
            tauri::RunEvent::Exit => {
                save_all(app);
            }
            _ => {}
        });
}

/// mac 专属 UI：应用菜单栏。Cmd+Q 被拦截为"折叠为悬浮窗"（Accessory 模式下
/// 应用没有 Dock/Cmd+Tab 入口，直接退出会让用户以为应用没了）；菜单中不注册
/// 任何系统退出项，保证退出只走托盘与悬浮窗右键。编辑 submenu 保留
/// Cmd+C/V/X/A，否则 WebView 的文本编辑快捷键会失灵
#[cfg(target_os = "macos")]
mod macos_ui {
    use super::*;
    use tauri::menu::{MenuItem, PredefinedMenuItem, Submenu};

    pub fn install(app: &tauri::App) -> tauri::Result<()> {
        let collapse = MenuItem::with_id(
            app,
            "collapse-to-float",
            "隐藏为悬浮窗",
            true,
            Some("CmdOrCtrl+Q"),
        )?;
        let app_menu = Submenu::with_id_and_items(
            app,
            "app",
            "zcode-speed-panel",
            true,
            &[&collapse],
        )?;
        let edit_menu = Submenu::with_id_and_items(
            app,
            "edit",
            "编辑",
            true,
            &[
                &PredefinedMenuItem::cut(app, None)?,
                &PredefinedMenuItem::copy(app, None)?,
                &PredefinedMenuItem::paste(app, None)?,
                &PredefinedMenuItem::select_all(app, None)?,
            ],
        )?;
        let menu = Menu::with_items(app, &[&app_menu, &edit_menu])?;
        app.set_menu(menu)?;
        Ok(())
    }
}
