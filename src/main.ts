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
  minScale: 60, // 最小量程 60 t/s，常见速度落在弧形中段更好读
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
// 存储键升级到 v2：让老用户也拿到一次新默认（鲸鱼女仆），之后的选择照常记住
const PET_PACK_KEY = "petPack.v2";
let currentPetPack = localStorage.getItem(PET_PACK_KEY) ?? "maid-deepseek-whale";
const petWidget = new PetWidget($<HTMLCanvasElement>("pet-canvas"), currentPetPack, () => {
  currentPetPack = petWidget.packId;
  localStorage.setItem(PET_PACK_KEY, currentPetPack);
});
petWidget.start();

// ---- 桌宠滚轮缩放：上下滚动调整悬浮窗大小（后端记忆，重启后保持） ----
const PET_BASE_SIZE = 200;
const PET_SIZE_MIN = 100;
const PET_SIZE_MAX = 480;
let petSize = Math.min(PET_SIZE_MAX, Math.max(PET_SIZE_MIN, Number(localStorage.getItem("petSize.v1")) || PET_BASE_SIZE));
$("float-pet").addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const next = petSize * (e.deltaY < 0 ? 1.08 : 1 / 1.08);
    const clamped = Math.min(PET_SIZE_MAX, Math.max(PET_SIZE_MIN, next));
    if (Math.round(clamped) === Math.round(petSize)) return;
    petSize = clamped;
    localStorage.setItem("petSize.v1", String(Math.round(clamped)));
    if (hasTauri) {
      tauriInvoke("set_float_size", { size: Math.round(clamped) }).catch(() => {});
    }
  },
  { passive: false }
);

// ---- 悬浮窗/桌宠右键菜单：恢复窗体 / 退出 ----
const floatMenu = $("float-menu");
const showFloatMenu = (x: number, y: number) => {
  floatMenu.style.display = "flex";
  const mw = floatMenu.offsetWidth || 110;
  const mh = floatMenu.offsetHeight || 60;
  floatMenu.style.left = `${Math.max(0, Math.min(x, window.innerWidth - mw - 2))}px`;
  floatMenu.style.top = `${Math.max(0, Math.min(y, window.innerHeight - mh - 2))}px`;
};
const hideFloatMenu = () => {
  floatMenu.style.display = "none";
};
for (const id of ["float-pet", "float-gauge", "float-pill"]) {
  $(id).addEventListener("contextmenu", (e) => {
    e.preventDefault();
    showFloatMenu(e.clientX, e.clientY);
  });
}
window.addEventListener("mousedown", (e) => {
  if (!floatMenu.contains(e.target as Node)) hideFloatMenu();
});
window.addEventListener("blur", hideFloatMenu);
$("float-menu-restore").addEventListener("click", () => {
  hideFloatMenu();
  requestMode("full");
});
$("float-menu-quit").addEventListener("click", () => {
  hideFloatMenu();
  tauriInvoke("quit_app");
});

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

/** 缓存命中率 = cache_read ÷ 全部提示 token（input + cache_creation + cache_read） */
const cacheHitRate = (s: Snapshot): string => {
  const prompt = s.inputTokens + s.cacheCreationTokens + s.cacheReadTokens;
  if (prompt <= 0) return "0%";
  return ((s.cacheReadTokens / prompt) * 100).toFixed(1) + "%";
};

function onSnapshot(s: Snapshot) {
  gCurrent.setTarget(s.currentTps, s.isEstimating);
  gAvg.setTarget(s.avgTps);
  gTotal.setTarget(s.totalTokens);
  miniGauge.setTarget(s.currentTps, s.isEstimating);

  subCurrent.textContent =
    s.liveSource === "io"
      ? s.ramping
        ? "实时实测 · 统计中…（30s 滑窗建立中）"
        : "实时实测 · 进程流式输出（30s 滑窗实测）"
      : s.isEstimating
        ? "生成中 · 此段无增量字节，按近期真实速度估算 ≈"
        : "待机 · 已无生成任务";
  subAvg.textContent = `Σ输出 ÷ Σ生成时长 · 今日 ${s.callsToday} 次调用`;
  subTotal.textContent = `输出 ${fmtTokens(s.outputTokens)} · 输入 ${fmtTokens(s.inputTokens)} · 缓存命中率 ${cacheHitRate(s)}`;

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
  localStorage.setItem(PET_PACK_KEY, currentPetPack);
});

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
