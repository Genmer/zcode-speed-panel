import "./style.css";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ArcGauge, BadgeGauge, MiniGauge, SPEED_TIERS, drawSpark, fmtClock, fmtTokens, fmtTps, speedColor } from "./gauges";
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
  tiers: SPEED_TIERS, // 六档（0–40/40–80/80–160/160–240/240–320/320+），随当前速度换色
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

// 当前速度卡右上角小表：最近一轮已完成调用的速度（落盘口径，非实时）
const gLast = new BadgeGauge($("g-last"), { tiers: SPEED_TIERS });

const miniGauge = new MiniGauge($("mini-gauge"), { tiers: SPEED_TIERS });
// 仪表悬浮窗右上角的上轮小环（与完整面板角标同款，只是尺寸更小）
const miniLast = new BadgeGauge($("mini-last"), { tiers: SPEED_TIERS });
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
  if (!styleDropdown.contains(e.target as Node)) setStyleDropdownOpen(false);
});
window.addEventListener("blur", () => {
  hideFloatMenu();
  setStyleDropdownOpen(false);
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") setStyleDropdownOpen(false);
});
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
const floatLast = $("float-last");

let lastSpark: number[] = [];
let lastNowMs = 0;
let sparkColor = "#22d3ee";

function redrawSpark() {
  if (lastSpark.length) drawSpark(sparkCanvas, lastSpark, sparkColor, lastNowMs);
}

function statusClass(s: Snapshot): string {
  if (s.isLive || s.isStarting) return "dot live";
  if (s.isEstimating) return "dot est";
  return "dot idle";
}

/** 缓存命中率 = cache_read ÷ input（usage 库的 input 本身就是全部提示 token，
 *  缓存命中的部分已含其中，分母再加 cache_read 会重复计数；cache_creation 全库
 *  恒为 0，防御性保留在分母以兼容将来单列它的 provider） */
const cacheHitRate = (s: Snapshot): string => {
  const prompt = s.inputTokens + s.cacheCreationTokens;
  if (prompt <= 0) return "0%";
  return ((s.cacheReadTokens / prompt) * 100).toFixed(1) + "%";
};

function onSnapshot(s: Snapshot) {
  gCurrent.setTarget(s.currentTps, s.isEstimating, s.isStarting);
  gAvg.setTarget(s.avgTps);
  gLast.setTarget(s.lastCallTps);
  gTotal.setTarget(s.totalTokens);
  miniGauge.setTarget(s.currentTps, s.isEstimating, s.isStarting);
  miniLast.setTarget(s.lastCallTps);

  subCurrent.textContent = s.isStarting
    ? "生成已启动 · 等待模型输出（统计中…）"
    : s.liveSource === "io"
      ? s.ramping
        ? "实时实测 · 统计中…（30s 滑窗建立中）"
        : "实时实测 · 进程流式输出（30s 滑窗实测）"
      : s.isEstimating
        ? "生成中 · 此段无增量字节，按近期真实速度估算 ≈"
        : "待机 · 已无生成任务";
  subAvg.textContent = `Σ输出 ÷ Σ生成时长 · 今日 ${s.callsToday} 次调用`;
  subTotal.textContent = `输出 ${fmtTokens(s.outputTokens)} · 输入 ${fmtTokens(s.inputTokens)} · 缓存命中率 ${cacheHitRate(s)}`;

  document.body.classList.toggle("live", s.isLive || s.isStarting);
  document.body.classList.toggle("est", s.isEstimating);
  const petState: "idle" | "running" | "estimating" | "starting" = s.isStarting
    ? "starting"
    : s.liveSource === "io"
      ? "running"
      : "idle";
  petWidget.setLive(s.currentTps, petState);
  liveDot.className = statusClass(s);
  liveText.textContent = s.isLive || s.isStarting ? "生成中" : s.isEstimating ? "估算中" : "待机";
  updatedAt.textContent = `更新于 ${fmtClock(s.nowMs)}`;
  floatDot.className = statusClass(s);
  floatTps.textContent = s.isStarting
    ? "…"
    : (s.isEstimating && s.liveSource !== "io" ? "≈" : "") + fmtTps(s.currentTps);
  petWidget.setLast(s.lastCallTps);
  // 胶囊第二行：上轮均速（落盘口径），按速度分档着色，无数据时显示 --
  floatLast.textContent = s.lastCallTps > 0 ? fmtTps(s.lastCallTps) : "--";
  floatLast.style.color = speedColor(s.lastCallTps, SPEED_TIERS);

  // 窗口标题同步实时速度，任务栏/Alt+Tab 可直接看到
  const title = `${s.isLive || s.isStarting ? "▶" : s.isEstimating ? "≈" : "⏸"} ${s.isStarting ? "…" : fmtTps(s.currentTps)} t/s · ${s.callsToday} 次 · ZCode 速度仪表盘`;
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

// ---- 模式与悬浮窗样式（自绘下拉，替代原生 select：WebView2 弹层在浅色系统主题下看不清） ----
function applyModeUi(mode: string) {
  document.body.classList.toggle("float-mode", mode === "float");
}

const STYLE_LABELS: Record<string, string> = {
  pet: "桌宠",
  gauge: "仪表悬浮窗",
  pill: "胶囊悬浮窗",
};

let currentStyle = localStorage.getItem("floatStyle") ?? "gauge";
const styleDropdown = $("float-style");
const styleOptions = Array.from(
  $<HTMLElement>("float-style-list").querySelectorAll<HTMLButtonElement>("button[data-value]"),
);

function applyStyleUi(style: string) {
  currentStyle = style;
  document.body.classList.toggle("style-pet", style === "pet");
  document.body.classList.toggle("style-gauge", style === "gauge");
  document.body.classList.toggle("style-pill", style === "pill");
  $("float-style-label").textContent = STYLE_LABELS[style] ?? STYLE_LABELS.gauge;
  for (const opt of styleOptions) {
    opt.classList.toggle("selected", opt.dataset.value === style);
  }
}

function setStyleDropdownOpen(open: boolean) {
  styleDropdown.classList.toggle("open", open);
  $<HTMLButtonElement>("float-style-btn").setAttribute("aria-expanded", String(open));
}

function selectFloatStyle(style: string) {
  setStyleDropdownOpen(false);
  localStorage.setItem("floatStyle", style);
  applyStyleUi(style);
  if (document.body.classList.contains("float-mode")) {
    tauriInvoke("set_float_style", { style }).catch(() => {});
  }
}

function requestMode(mode: "full" | "float") {
  if (!hasTauri) {
    applyModeUi(mode);
    return;
  }
  tauriInvoke("set_mode", { mode, style: currentStyle }).catch(() => {});
}

$("btn-float").addEventListener("click", () => requestMode("float"));
$("float-gauge-expand").addEventListener("click", () => requestMode("full"));
$("float-pill-expand").addEventListener("click", () => requestMode("full"));
$("float-pet-expand").addEventListener("click", () => requestMode("full"));
$("float-pet-cycle").addEventListener("click", () => {
  currentPetPack = petWidget.cyclePack();
  localStorage.setItem(PET_PACK_KEY, currentPetPack);
});

// ---- 重新校准（当前速度卡左上角 ⟳）：丢弃字节→token 系数样本回到先验 ----
const btnRecal = $<HTMLButtonElement>("btn-recal");
if (!hasTauri) btnRecal.style.display = "none"; // 浏览器预览无真实校准
let recalTimer = 0;
const flashRecal = () => {
  btnRecal.classList.add("done");
  window.clearTimeout(recalTimer);
  recalTimer = window.setTimeout(() => btnRecal.classList.remove("done"), 1500);
};
btnRecal.addEventListener("click", () => {
  tauriInvoke("recalibrate").catch((err) => console.warn("recalibrate 失败:", err));
});

$("float-style-btn").addEventListener("click", () => {
  setStyleDropdownOpen(!styleDropdown.classList.contains("open"));
});
for (const opt of styleOptions) {
  opt.addEventListener("click", () => selectFloatStyle(opt.dataset.value!));
}

// ---- mac 引导提示：主窗口从隐藏→显示时后端发 "tray-hint"（Windows 不发，前端永不显示）----
const trayHint = $("tray-hint");
let trayHintTimer = 0;
const showTrayHint = () => {
  trayHint.classList.add("show");
  window.clearTimeout(trayHintTimer);
  trayHintTimer = window.setTimeout(() => trayHint.classList.remove("show"), 6000);
};
trayHint.addEventListener("click", () => {
  window.clearTimeout(trayHintTimer);
  trayHint.classList.remove("show");
});

// 顶栏/悬浮窗拖动：mousedown 调 startDragging（按钮、下拉框除外）。
// 目标自身带 data-tauri-drag-region 时由 Tauri 内核直接处理（跳过，避免双重拖动）
function enableDrag(el: HTMLElement) {
  el.addEventListener("mousedown", (e) => {
    const target = e.target as HTMLElement;
    if (target.closest("button, select, input, .dropdown")) return;
    if (target.hasAttribute("data-tauri-drag-region")) return;
    e.preventDefault();
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow: g }) => g().startDragging().catch(() => {}))
      .catch(() => {});
  });
}
enableDrag($("app-header"));
enableDrag($("float-gauge"));
enableDrag($("float-pill"));
enableDrag($("float-pet"));

// 悬浮窗双击 = 恢复完整面板。桌宠不参与：双击已用于换宠物（pet.ts），
// 其恢复走 ⤢ 按钮 / 右键菜单 / 托盘。按钮上的双击不触发（click 已处理）
for (const id of ["float-gauge", "float-pill"]) {
  $(id).addEventListener("dblclick", (e) => {
    if ((e.target as HTMLElement).closest("button, select, input, .dropdown")) return;
    requestMode("full");
  });
}

// ---- 自绘标题栏：拖动移动、双击最大化，— / ▢ / ✕ 窗口控制 ----
const currentWindow = () => import("@tauri-apps/api/window").then((m) => m.getCurrentWindow());
$("app-header").addEventListener("dblclick", (e) => {
  if ((e.target as HTMLElement).closest("button, select, input, .dropdown")) return;
  if (hasTauri) currentWindow().then((w) => w.toggleMaximize()).catch(() => {});
});
if (hasTauri) {
  $("wc-min").addEventListener("click", () => {
    currentWindow().then((w) => w.minimize()).catch(() => {});
  });
  $("wc-max").addEventListener("click", () => {
    currentWindow().then((w) => w.toggleMaximize()).catch(() => {});
  });
  $("wc-close").addEventListener("click", () => requestMode("float"));
} else {
  // 浏览器预览无窗口控制
  ($("win-controls") as HTMLElement).style.display = "none";
}

applyStyleUi(localStorage.getItem("floatStyle") ?? "gauge");

if (hasTauri) {
  (async () => {
    const { listen } = await import("@tauri-apps/api/event");
    await listen<SnapshotPayload>("metrics", (e) => {
      onSnapshot({ ...e.payload.snapshot, rolloutDir: e.payload.rolloutDir });
    });
    await listen<string>("mode", (e) => applyModeUi(e.payload));
    await listen("tray-hint", () => showTrayHint());
    // 重新校准完成（手动或漂移自动触发）：按钮闪 ✓ 反馈
    await listen("recalibrated", flashRecal);
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
    // mac 启动引导（一次性）：页面就绪后主动领取，避免 setup 内 emit 早于加载被丢弃
    if (await tauriInvoke<boolean>("tray_hint_once")) showTrayHint();
  })().catch((err) => {
    document.title = `初始化失败 · ZCode 速度仪表盘`;
    subCurrent.textContent = `Tauri 初始化失败：${err}`;
  });
} else {
  startMock(onSnapshot);
}
