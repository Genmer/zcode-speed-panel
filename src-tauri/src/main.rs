#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod liveio;
mod metrics;

use liveio::LiveIo;
use metrics::{home_dir, Engine, Snapshot};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, WindowEvent};

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

struct AppState {
    engine: Mutex<Engine>,
    mode: Mutex<Mode>,
    style: Mutex<FloatStyle>,
    live: Mutex<LiveIo>,
    debug: Mutex<DebugLog>,
}

/// 调试日志：记录实时显示值、统计值与每轮调用完成后的真值，
/// 供"实时读数 vs 落盘统计"的偏差分析。JSONL 追加写，超限轮转保留一代。
struct DebugLog {
    file: Option<fs::File>,
    written: u64,
    last_heartbeat: std::time::Instant,
}

const DEBUG_LOG_MAX: u64 = 8 * 1024 * 1024;

impl DebugLog {
    fn new() -> Self {
        let mut log = DebugLog { file: None, written: 0, last_heartbeat: std::time::Instant::now() };
        log.reopen();
        log
    }

    fn path() -> Option<PathBuf> {
        home_dir().map(|h| h.join(".zcode").join("speed-panel-debug.jsonl"))
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
const FLOAT_PET_SIZE: (f64, f64) = (200.0, 200.0);

fn mode_file() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".zcode").join("speed-panel-mode.txt"))
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Persisted {
    mode: String,
    style: String,
}

fn load_state() -> (Mode, FloatStyle) {
    let raw = mode_file().and_then(|p| fs::read_to_string(p).ok());
    match raw {
        Some(s) => match serde_json::from_str::<Persisted>(&s) {
            Ok(p) => (Mode::parse(&p.mode), FloatStyle::parse(&p.style)),
            // 旧格式：纯文本 "full"/"float"
            Err(_) => (Mode::parse(&s), FloatStyle::Gauge),
        },
        None => (Mode::Full, FloatStyle::Gauge),
    }
}

fn save_state(mode: Mode, style: FloatStyle) {
    if let Some(p) = mode_file() {
        let json = serde_json::json!({ "mode": mode.as_str(), "style": style.as_str() });
        let _ = fs::write(p, json.to_string());
    }
}

fn apply_mode(window: &tauri::WebviewWindow, mode: Mode, style: FloatStyle) {
    match mode {
        Mode::Full => {
            let _ = window.set_min_size(Some(LogicalSize::new(720.0, 520.0)));
            let _ = window.set_size(LogicalSize::new(FULL_SIZE.0, FULL_SIZE.1));
            let _ = window.set_decorations(true);
            let _ = window.set_resizable(true);
            let _ = window.set_always_on_top(false);
            let _ = window.set_skip_taskbar(false);
            let _ = window.set_shadow(true);
        }
        Mode::Float => {
            let (w, h) = match style {
                FloatStyle::Gauge => FLOAT_GAUGE_SIZE,
                FloatStyle::Pill => FLOAT_PILL_SIZE,
                FloatStyle::Pet => FLOAT_PET_SIZE,
            };
            let _ = window.set_min_size(None::<LogicalSize<f64>>);
            let _ = window.set_size(LogicalSize::new(w, h));
            let _ = window.set_decorations(false);
            let _ = window.set_resizable(false);
            // 悬浮窗：置顶、不占任务栏、无原生阴影（阴影会盖住圆角外透明区）
            let _ = window.set_always_on_top(true);
            let _ = window.set_skip_taskbar(true);
            let _ = window.set_shadow(false);
        }
    }
}

fn switch_mode(app: &AppHandle, mode: Mode) {
    let style = *app.state::<AppState>().style.lock().unwrap();
    if let Some(window) = app.get_webview_window("main") {
        apply_mode(&window, mode, style);
    }
    save_state(mode, style);
    *app.state::<AppState>().mode.lock().unwrap() = mode;
    let _ = app.emit("mode", mode.as_str());
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
    // 实时实测：进程 IO 写字节流（真实值），只统计当前会话对应的 CLI 进程
    let now_ms = snapshot.now_ms;
    let cal_event;
    let bpt_now;
    {
        let mut live = state.live.lock().unwrap();
        if !live.history_done() {
            live.ingest_history(engine_calls.as_slice());
        }
        live.observe(&new_calls);
        live.set_inflight(inflight);
        let live_now = live.measure(now_ms);
        cal_event = live.take_calibration();
        bpt_now = live.bytes_per_token();
        if live_now.available {
            if live_now.streaming {
                snapshot.is_live = true;
                snapshot.is_estimating = false;
                snapshot.ramping = live_now.ramping;
                snapshot.live_source = "io".into();
                // 部分调用期间 UI 管道无增量字节（IO 实测为 0）：回退到近期
                // 已完成调用的真实速度（与速度曲线同口径），标记 ≈ 估算
                if live_now.tps < 1.0 && snapshot.window_tps > 0.0 {
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
            // IO 可用但当前会话无流式输出 → 如实待机（真实值优先，不用估算掩盖）
            snapshot.is_estimating = false;
            snapshot.ramping = false;
            snapshot.current_tps = 0.0;
            snapshot.live_source = "idle".into();
            if let Some(last) = snapshot.spark.last_mut() {
                *last = 0.0;
            }
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
            log.write(serde_json::json!({
                "kind": "cal",
                "t": now_ms,
                "id": cal.id,
                "bytes_kb": (cal.bytes / 1024.0 * 10.0).round() / 10.0,
                "bpt_sample": (cal.bpt_sample * 10.0).round() / 10.0,
                "bpt_now": (cal.bpt_now * 10.0).round() / 10.0,
                "skipped": cal.cal_skipped,
            }));
        }
        let active = snapshot.is_live || snapshot.is_estimating;
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
                "stream": snapshot.is_live,
                "ramp": snapshot.ramping,
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
                apply_mode(&window, mode, st);
            }
        }
    }
    let mode = *app.state::<AppState>().mode.lock().unwrap();
    save_state(mode, st);
    let _ = app.emit("float-style", st.as_str());
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
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

/// 后台轮询线程：增量解析 model-io 文件并推送快照
fn poller(app: AppHandle) {
    loop {
        let payload = build_payload(&app);
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
        })
        .invoke_handler(tauri::generate_handler![snapshot, set_mode, set_float_style])
        .setup(|app| {
            // ---- 系统托盘 ----
            let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "隐藏到托盘", true, None::<&str>)?;
            let toggle_float =
                MenuItem::with_id(app, "toggle-float", "悬浮窗 / 完整面板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&show, &hide, &toggle_float, &sep, &quit])?;

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
                    "quit" => app.exit(0),
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

            // ---- 关闭窗口 = 隐藏到托盘 ----
            let win_handle = app.handle().clone();
            app.get_webview_window("main")
                .unwrap()
                .on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Some(win) = win_handle.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    }
                });

            // ---- 恢复上次显示模式与悬浮窗样式后再亮出窗口，避免闪一下完整尺寸 ----
            let (mode, style) = load_state();
            {
                let state = app.state::<AppState>();
                *state.mode.lock().unwrap() = mode;
                *state.style.lock().unwrap() = style;
            }
            let window = app.get_webview_window("main").unwrap();
            apply_mode(&window, mode, style);
            let _ = window.show();

            // ---- 启动轮询线程 ----
            let poll_handle = app.handle().clone();
            std::thread::spawn(move || poller(poll_handle));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running zcode-speed-panel");
}
