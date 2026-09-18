// 模型速度趋势详情弹窗：按模型分类的速度折线（统一 60 桶）与窗口统计。
// 数据来自后端 model_stats 命令（只读查询 usage 库 model_usage 表现算聚合，
// 零本地存储）；弹窗打开期间每 5s 拉取一次，关闭即停。
import { fmtClock, fmtTokens, fmtTps } from "./gauges";

const FONT = `"Segoe UI", "Microsoft YaHei", sans-serif`;

/** 折线/图例配色（按 series 顺序循环） */
const PALETTE = ["#22d3ee", "#a78bfa", "#34d399", "#fbbf24", "#f87171", "#60a5fa", "#f472b6", "#4ade80"];

/** 统计窗口选项（分钟），与后端 clamp 档位一致 */
const WINDOW_OPTIONS = [
  { value: 10, label: "最近 10 分钟" },
  { value: 60, label: "最近 1 小时" },
  { value: 360, label: "最近 6 小时" },
];

/** 图例与统计行里模型名的截断长度（超过加 …，完整名放 title） */
const MODEL_NAME_MAX = 18;

interface ModelBucket {
  tps: number;
  calls: number;
  tokens: number;
}

interface ModelSeries {
  model: string;
  buckets: ModelBucket[];
  totalCalls: number;
  totalTokens: number;
  avgTps: number;
  peakTps: number;
  share: number;
}

interface ModelStatsPayload {
  windowMin: number;
  bucketMs: number;
  nowMs: number;
  series: ModelSeries[];
}

type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T | undefined>;

const $ = <T extends HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing #${id}`);
  return el as T;
};

const shortModel = (name: string): string => (name.length > MODEL_NAME_MAX ? name.slice(0, MODEL_NAME_MAX) + "…" : name);

/** 与 gauges.drawSpark 同规则的画布按 DPR 适配 */
function fitCanvas(
  canvas: HTMLCanvasElement,
): { ctx: CanvasRenderingContext2D; w: number; h: number } | null {
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (w < 8 || h < 8) return null;
  const dpr = window.devicePixelRatio || 1;
  const pw = Math.round(w * dpr);
  const ph = Math.round(h * dpr);
  if (canvas.width !== pw || canvas.height !== ph) {
    canvas.width = pw;
    canvas.height = ph;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  return { ctx, w, h };
}

/** 每模型一条 tps 折线：y 轴 0~峰值×1.15 自适应 + 3 条横网格线，x 轴 5 个真实时刻刻度 */
function drawModelChart(canvas: HTMLCanvasElement, p: ModelStatsPayload) {
  const fit = fitCanvas(canvas);
  if (!fit) return;
  const { ctx, w, h } = fit;
  const padL = 40;
  const padR = 10;
  const padT = 10;
  const padB = 20;
  const iw = w - padL - padR;
  const ih = h - padT - padB;
  const n = p.series[0]?.buckets.length ?? 60;
  const spanMs = n * p.bucketMs;
  // 桶 i（0 = 最新）的中心时刻：右缘 ≈ 现在
  const tStart = p.nowMs - spanMs;
  const xAt = (t: number) => padL + (iw * (t - tStart)) / spanMs;
  const yMax = Math.max(10, Math.max(0, ...p.series.flatMap((s) => s.buckets.map((b) => b.tps))) * 1.15);
  const yAt = (v: number) => padT + ih - (Math.min(v, yMax) / yMax) * ih;

  // 3 条横网格线 + 刻度文字（顶 = 峰值档、中 = 半档、底 = 0）
  ctx.strokeStyle = "rgba(255,255,255,0.06)";
  ctx.fillStyle = "rgba(139,147,167,0.7)";
  ctx.font = `10px ${FONT}`;
  ctx.lineWidth = 1;
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  for (let i = 0; i <= 2; i++) {
    const y = padT + (ih * i) / 2;
    ctx.beginPath();
    ctx.moveTo(padL, y);
    ctx.lineTo(w - padR, y);
    ctx.stroke();
    ctx.fillText(fmtTps((yMax * (2 - i)) / 2), padL - 6, y);
  }

  // x 轴 5 个时间刻度（HH:MM），竖向细网格线便于对表
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  for (let k = 0; k <= 4; k++) {
    const t = tStart + (spanMs * k) / 4;
    const gx = xAt(t);
    ctx.strokeStyle = "rgba(255,255,255,0.04)";
    ctx.beginPath();
    ctx.moveTo(gx, padT);
    ctx.lineTo(gx, padT + ih);
    ctx.stroke();
    ctx.fillText(fmtClock(t).slice(0, 5), gx, h - padB + 4);
  }
  if (p.series.length === 0) return;

  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  p.series.forEach((s, j) => {
    ctx.strokeStyle = PALETTE[j % PALETTE.length];
    ctx.beginPath();
    s.buckets.forEach((b, i) => {
      const x = xAt(tStart + (n - i - 0.5) * p.bucketMs);
      const y = yAt(b.tps);
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
  });
}

/** 绑定弹窗全部交互：入口按钮、窗口下拉、5s 轮询、绘制与关闭清理 */
export function initModelStats(invoke: InvokeFn): void {
  const modal = $("model-modal");
  const box = $("model-modal-box");
  const openBtn = $("btn-model-stats");
  const closeBtn = $("model-modal-close");
  const dropdown = $("model-window");
  const dropdownBtn = $<HTMLButtonElement>("model-window-btn");
  const dropdownLabel = $("model-window-label");
  const legend = $("model-legend");
  const canvas = $<HTMLCanvasElement>("model-chart");
  const empty = $("model-empty");
  const summary = $("model-summary");
  const options = Array.from(dropdown.querySelectorAll<HTMLButtonElement>("button[data-value]"));

  let isOpen = false;
  let timer = 0;
  let windowMin = 60; // 默认 1 小时
  let lastPayload: ModelStatsPayload | null = null;

  const setDropdownOpen = (open: boolean) => {
    dropdown.classList.toggle("open", open);
    dropdownBtn.setAttribute("aria-expanded", String(open));
  };

  /** 渲染一次 payload：图例、统计行与折线（无数据时显示空状态） */
  const render = (p: ModelStatsPayload) => {
    lastPayload = p;
    const has = p.series.length > 0;
    empty.style.display = has ? "none" : "flex";
    legend.style.display = has ? "flex" : "none";
    summary.style.display = has ? "flex" : "none";
    legend.replaceChildren();
    summary.replaceChildren();
    p.series.forEach((s, i) => {
      const color = PALETTE[i % PALETTE.length];
      const item = document.createElement("span");
      item.className = "model-legend-item";
      const dot = document.createElement("span");
      dot.className = "model-dot";
      dot.style.background = color;
      const name = document.createElement("span");
      name.textContent = shortModel(s.model);
      name.title = s.model;
      item.append(dot, name);
      legend.append(item);

      const row = document.createElement("span");
      row.className = "model-stat-row";
      const rdot = document.createElement("span");
      rdot.className = "model-dot";
      rdot.style.background = color;
      rdot.title = s.model;
      const text = document.createElement("span");
      text.textContent = `${shortModel(s.model)} · 均速 ${fmtTps(s.avgTps)} t/s · 峰值 ${fmtTps(s.peakTps)} · ${s.totalCalls} 次 · ${fmtTokens(s.totalTokens)} token (${(s.share * 100).toFixed(1)}%)`;
      text.title = s.model;
      row.append(rdot, text);
      summary.append(row);
    });
    if (has) drawModelChart(canvas, p);
  };

  const fetchNow = () => {
    // 收起为悬浮窗时弹窗已被 CSS 隐藏（窗口太小放不下）：停表关闭，不再空转拉取
    if (document.body.classList.contains("float-mode")) {
      close();
      return;
    }
    invoke<ModelStatsPayload>("model_stats", { windowMin })
      .then((p) => {
        if (p && isOpen) render(p);
      })
      .catch((err) => console.warn("[model_stats] invoke failed:", err));
  };

  const open = () => {
    if (isOpen) return;
    isOpen = true;
    modal.style.display = "flex";
    fetchNow();
    timer = window.setInterval(fetchNow, 5000);
  };

  const close = () => {
    if (!isOpen) return;
    isOpen = false;
    window.clearInterval(timer);
    modal.style.display = "none";
    setDropdownOpen(false);
  };

  openBtn.addEventListener("click", () => (isOpen ? close() : open()));
  closeBtn.addEventListener("click", close);
  // 点弹窗内容之外关闭（入口按钮自身除外，由上面的 click 切换开关；
  // 与 main.ts 的 float-menu / 样式下拉 mousedown 监听各自独立，互不影响）
  window.addEventListener("mousedown", (e) => {
    if (!isOpen) return;
    const t = e.target as Node;
    if (box.contains(t) || openBtn.contains(t)) return;
    close();
  });
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && isOpen) close();
  });
  window.addEventListener("resize", () => {
    if (isOpen && lastPayload && lastPayload.series.length) drawModelChart(canvas, lastPayload);
  });

  dropdownBtn.addEventListener("click", () => setDropdownOpen(!dropdown.classList.contains("open")));
  for (const opt of options) {
    opt.addEventListener("click", () => {
      setDropdownOpen(false);
      windowMin = Number(opt.dataset.value) || 60;
      dropdownLabel.textContent = WINDOW_OPTIONS.find((w) => w.value === windowMin)?.label ?? `最近 ${windowMin} 分钟`;
      for (const o of options) o.classList.toggle("selected", o === opt);
      if (isOpen) fetchNow(); // 切窗口立即拉一次（定时器继续按新窗口拉取）
    });
  }
}
