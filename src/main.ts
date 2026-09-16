import "./style.css";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ArcGauge, MiniGauge, drawSpark, fmtClock, fmtTokens, fmtTps } from "./gauges";
import { PetWidget } from "./pet";
import { startMock, type Snapshot } from "./mock";

interface SnapshotPayload {
  snapshot: Snapshot;
  rolloutDir: string;
  mode: string;
  floatStyle: string;
}

const $ = <T extends HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing #${id}`);
  return el as T;
};

const hasTauri = typeof (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ !== "undefined";

async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  if (!hasTauri) return undefined;
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

const gCurrent = new ArcGauge($("g-current"), {
  label: "当前输出速度",
  unit: "token / s",
  color: "#22d3ee",
  color2: "#0ea5e9",
  kind: "speed",
});

const gAvg = new ArcGauge($("g-avg"), {
  label: "今日平均速度",
  unit: "token / s",
  color: "#a78bfa",
  color2: "#8b5cf6",
  kind: "speed",
});

const gTotal = new ArcGauge($("g-total"), {
  label: "今日总 Token",
  unit: "今日累计",
  color: "#34d399",
  color2: "#10b981",
  kind: "tokens",
});

const miniGauge = new MiniGauge($("mini-gauge"));
let currentPetPack = localStorage.getItem("petPack") ?? "yuexinmiao";
const petWidget = new PetWidget($<HTMLCanvasElement>("pet-canvas"), currentPetPack, () => {
  currentPetPack = petWidget.packId;
  localStorage.setItem("petPack", currentPetPack);
});
petWidget.start();

const sparkCanvas = $<HTMLCanvasElement>("spark");
const liveDot = $("live-dot");
const liveText = $("live-text");
const updatedAt = $("updated-at");
const subCurrent = $("sub-current");
const subAvg = $("sub-avg");
const subTotal = $("sub-total");
const stDir = $("st-dir");
const stCalls = $("st-calls");
const stSessions = $("st-sessions");
const stLast = $("st-last");
const chartMax = $("chart-max");
const floatTps = $("float-tps");
const floatDot = $("float-dot");

let lastSpark: number[] = [];
let lastNowMs = 0;
let sparkColor = "#22d3ee";

function redrawSpark() {
  if (lastSpark.length) drawSpark(sparkCanvas, lastSpark, sparkColor, lastNowMs);
}

function statusClass(s: Snapshot): string {
  if (s.isLive) return "dot live";
  if (s.isEstimating) return "dot est";
  return "dot idle";
}

function onSnapshot(s: Snapshot) {
  gCurrent.setTarget(s.currentTps, s.isEstimating);
  gAvg.setTarget(s.avgTps);
  gTotal.setTarget(s.totalTokens);
  miniGauge.setTarget(s.currentTps, s.isEstimating);

  subCurrent.textContent =
    s.liveSource === "io"
      ? `实时实测 · 进程流式输出（10s 真实测量）`
      : s.isEstimating
        ? "长任务估算中 · 调用完成后自动校正"
        : "待机 · 已无生成任务";
  subAvg.textContent = `Σ输出 ÷ Σ生成时长 · 今日 ${s.callsToday} 次调用`;
  subTotal.textContent = `输出 ${fmtTokens(s.outputTokens)} · 输入 ${fmtTokens(s.inputTokens)} · 缓存命中 ${fmtTokens(s.cacheReadTokens)}（未计入）`;

  document.body.classList.toggle("live", s.isLive);
  document.body.classList.toggle("est", s.isEstimating);
  const petState: "idle" | "running" | "estimating" = s.liveSource === "io"
    ? "running"
    : "idle";
  petWidget.setLive(s.currentTps, petState);
  liveDot.className = statusClass(s);
  liveText.textContent = s.isLive ? "生成中" : s.isEstimating ? "估算中" : "待机";
  updatedAt.textContent = `更新于 ${fmtClock(s.nowMs)}`;
  floatDot.className = statusClass(s);
  floatTps.textContent =
    (s.isEstimating && s.liveSource !== "io" ? "≈" : "") + fmtTps(s.currentTps);

  // 窗口标题同步实时速度，任务栏/Alt+Tab 可直接看到
  const title = `${s.liveSource === "io" ? "▶" : s.isEstimating ? "≈" : "⏸"} ${fmtTps(s.currentTps)} t/s · ${s.callsToday} 次 · ZCode 速度仪表盘`;
  document.title = title;
  try {
    getCurrentWindow().setTitle(title).catch(() => {});
  } catch {
    // 浏览器预览模式无 Tauri API
  }

  stDir.textContent = `监控 ${s.rolloutDir}`;
  stCalls.textContent = `今日调用 ${s.callsToday} 次`;
  stSessions.textContent = `${s.sessionsToday} 个会话`;
  stLast.textContent = `最近活动 ${fmtClock(s.lastActivityMs)}`;

  lastSpark = s.spark;
  lastNowMs = s.nowMs;
  sparkColor = s.isLive ? "#22d3ee" : s.isEstimating ? "#fbbf24" : "#64748b";
  const peak = Math.max(10, ...s.spark);
  chartMax.textContent = `峰值 ${fmtTps(peak)} t/s`;
  redrawSpark();
}

window.addEventListener("resize", redrawSpark);

// ---- 模式与悬浮窗样式 ----
function applyModeUi(mode: string) {
  document.body.classList.toggle("float-mode", mode === "float");
}

function applyStyleUi(style: string) {
  document.body.classList.toggle("style-pet", style === "pet");
  document.body.classList.toggle("style-gauge", style === "gauge");
  document.body.classList.toggle("style-pill", style === "pill");
  const sel = $<HTMLSelectElement>("float-style");
  sel.value = style === "pill" || style === "pet" ? style : "gauge";
}

function requestMode(mode: "full" | "float") {
  if (!hasTauri) {
    applyModeUi(mode);
    return;
  }
  const style = $<HTMLSelectElement>("float-style").value;
  tauriInvoke("set_mode", { mode, style }).catch(() => {});
}

$("btn-float").addEventListener("click", () => requestMode("float"));
$("float-gauge-expand").addEventListener("click", () => requestMode("full"));
$("float-pill-expand").addEventListener("click", () => requestMode("full"));
$("float-pet-expand").addEventListener("click", () => requestMode("full"));
$("float-pet-cycle").addEventListener("click", () => {
  currentPetPack = petWidget.cyclePack();
  localStorage.setItem("petPack", currentPetPack);
});
$("float-pet-expand").addEventListener("click", () => requestMode("full"));

$("float-style").addEventListener("change", () => {
  const style = $<HTMLSelectElement>("float-style").value;
  localStorage.setItem("floatStyle", style);
  applyStyleUi(style);
  if (document.body.classList.contains("float-mode")) {
    tauriInvoke("set_float_style", { style }).catch(() => {});
  }
});

// 悬浮窗拖动：mousedown 调 startDragging（按钮除外）
function enableDrag(el: HTMLElement) {
  el.addEventListener("mousedown", (e) => {
    if ((e.target as HTMLElement).closest("button")) return;
    e.preventDefault();
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow: g }) => g().startDragging().catch(() => {}))
      .catch(() => {});
  });
}
enableDrag($("float-gauge"));
enableDrag($("float-pill"));
enableDrag($("float-pet"));

applyStyleUi(localStorage.getItem("floatStyle") ?? "gauge");

if (hasTauri) {
  (async () => {
    const { listen } = await import("@tauri-apps/api/event");
    await listen<SnapshotPayload>("metrics", (e) => {
      onSnapshot({ ...e.payload.snapshot, rolloutDir: e.payload.rolloutDir });
    });
    await listen<string>("mode", (e) => applyModeUi(e.payload));
    await listen<string>("float-style", (e) => {
      localStorage.setItem("floatStyle", e.payload);
      applyStyleUi(e.payload);
    });
    const p = await tauriInvoke<SnapshotPayload>("snapshot");
    if (p) {
      applyModeUi(p.mode);
      if (p.floatStyle) {
        localStorage.setItem("floatStyle", p.floatStyle);
        applyStyleUi(p.floatStyle);
      }
      onSnapshot({ ...p.snapshot, rolloutDir: p.rolloutDir });
    }
  })().catch((err) => {
    document.title = `初始化失败 · ZCode 速度仪表盘`;
    subCurrent.textContent = `Tauri 初始化失败：${err}`;
  });
} else {
  startMock(onSnapshot);
}
