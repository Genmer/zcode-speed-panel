// Canvas 弧形仪表盘、迷你悬浮仪表与速度曲线渲染

const FONT = `"Segoe UI", "Microsoft YaHei", sans-serif`;

export function niceCeil(x: number): number {
  if (!isFinite(x) || x <= 0) return 10;
  const exp = Math.floor(Math.log10(x));
  const base = Math.pow(10, exp);
  const frac = x / base;
  const nice = frac <= 1 ? 1 : frac <= 2 ? 2 : frac <= 2.5 ? 2.5 : frac <= 5 ? 5 : 10;
  return nice * base;
}

export function fmtTps(v: number): string {
  if (v >= 100) return Math.round(v).toString();
  if (v >= 10) return v.toFixed(1);
  return v.toFixed(1);
}

export function fmtTokens(n: number): string {
  if (n < 0) return "0";
  if (n < 10000) return Math.round(n).toLocaleString("en-US");
  if (n < 1e8) {
    const w = n / 1e4;
    return (w >= 100 ? w.toFixed(0) : w.toFixed(1)) + " 万";
  }
  return (n / 1e8).toFixed(2) + " 亿";
}

export function fmtClock(ms: number): string {
  if (!ms) return "--:--:--";
  const d = new Date(ms);
  const p = (x: number) => x.toString().padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

const TAU = Math.PI * 2;
/** 弧形起止角（270° 扫过，缺口朝下） */
const A0 = Math.PI * 0.75;
const SWEEP = Math.PI * 1.5;
const EST_COLOR = "#fbbf24";

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

function progressGradient(
  ctx: CanvasRenderingContext2D,
  cx: number,
  cy: number,
  frac: number,
  c1: string,
  c2: string,
): string | CanvasGradient {
  const anyCtx = ctx as unknown as {
    createConicGradient?: (a: number, x: number, y: number) => CanvasGradient;
  };
  if (typeof anyCtx.createConicGradient === "function") {
    const g = anyCtx.createConicGradient(A0, cx, cy);
    g.addColorStop(0, c1);
    g.addColorStop(Math.min(0.749, 0.75 * frac), c2);
    g.addColorStop(0.75, c2);
    g.addColorStop(1, c1);
    return g;
  }
  return c1;
}

/** 统一的动画帧循环：量程跟随峰值平滑变化，避免归零时“先跑满再落下” */
const animItems: Array<{ frame(dt: number): void }> = [];
let rafStarted = false;
let lastFrame = 0;

function startLoop() {
  if (rafStarted) return;
  rafStarted = true;
  lastFrame = performance.now();
  const frame = (now: number) => {
    const dt = Math.min(0.1, (now - lastFrame) / 1000);
    lastFrame = now;
    for (const g of animItems) g.frame(dt);
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
}

interface GaugeOptions {
  color: string;
  color2?: string;
  minScale: number;
}

abstract class BaseGauge {
  protected canvas: HTMLCanvasElement;
  protected opts: GaugeOptions;
  protected value = 0;
  protected target = 0;
  protected max: number;
  protected est = false;

  constructor(canvas: HTMLCanvasElement, opts: GaugeOptions) {
    this.canvas = canvas;
    this.opts = opts;
    this.max = opts.minScale;
    animItems.push(this);
    startLoop();
  }

  setTarget(v: number, est = false) {
    if (!isFinite(v) || v < 0) v = 0;
    this.target = v;
    this.est = est;
  }

  frame(dt: number) {
    const k = 1 - Math.exp(-dt * 7);
    this.value += (this.target - this.value) * k;
    if (Math.abs(this.target - this.value) < 0.005) this.value = this.target;
    // 量程跟随：峰值上涨立刻放大，回落时缓慢收缩（收缩速度跟不上指针下落就会“先满后落”）
    const peak = Math.max(this.value, this.target);
    const desired = Math.max(this.opts.minScale, niceCeil(peak * 1.2));
    if (desired > this.max) {
      this.max = desired;
    } else if (desired < this.max) {
      this.max += (desired - this.max) * (1 - Math.exp(-dt * 1.2));
    }
    this.draw();
  }

  protected abstract draw(): void;
}

export class ArcGauge extends BaseGauge {
  private label: string;
  private unit: string;
  private kind: "speed" | "tokens";

  constructor(canvas: HTMLCanvasElement, opts: { label: string; unit: string; color: string; color2?: string; kind: "speed" | "tokens" }) {
    super(canvas, { color: opts.color, color2: opts.color2, minScale: opts.kind === "speed" ? 10 : 10000 });
    this.label = opts.label;
    this.unit = opts.unit;
    this.kind = opts.kind;
  }

  protected draw() {
    const fit = fitCanvas(this.canvas);
    if (!fit) return;
    const { ctx, w, h } = fit;
    const cx = w / 2;
    const cy = h * 0.56;
    const r = Math.min(w * 0.38, h * 0.4);

    ctx.fillStyle = "#8b93a7";
    ctx.font = `500 12px ${FONT}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(this.label, cx, 16);

    ctx.lineWidth = 13;
    ctx.lineCap = "round";
    ctx.strokeStyle = "rgba(255,255,255,0.07)";
    ctx.beginPath();
    ctx.arc(cx, cy, r, A0, A0 + SWEEP);
    ctx.stroke();

    const frac = Math.max(0.0001, Math.min(1, this.value / this.max));
    const color = this.est ? EST_COLOR : this.opts.color;
    const color2 = this.est ? "#d97706" : (this.opts.color2 ?? this.opts.color);
    ctx.save();
    ctx.shadowColor = color;
    ctx.shadowBlur = 14;
    ctx.strokeStyle = progressGradient(ctx, cx, cy, frac, color, color2);
    ctx.beginPath();
    ctx.arc(cx, cy, r, A0, A0 + SWEEP * frac);
    ctx.stroke();
    ctx.restore();

    ctx.strokeStyle = "rgba(255,255,255,0.16)";
    ctx.lineWidth = 2;
    ctx.lineCap = "butt";
    for (let i = 0; i <= 6; i++) {
      const a = A0 + (SWEEP * i) / 6;
      const r1 = r - 22;
      const r2 = r - 16;
      ctx.beginPath();
      ctx.moveTo(cx + Math.cos(a) * r1, cy + Math.sin(a) * r1);
      ctx.lineTo(cx + Math.cos(a) * r2, cy + Math.sin(a) * r2);
      ctx.stroke();
    }

    ctx.fillStyle = "rgba(139,147,167,0.75)";
    ctx.font = `10px ${FONT}`;
    if (this.kind === "speed") {
      const la = A0 + 0.22;
      const ra = A0 + SWEEP - 0.22;
      ctx.textAlign = "left";
      ctx.fillText("0", cx + Math.cos(la) * (r - 40), cy + Math.sin(la) * (r - 40));
      ctx.textAlign = "right";
      ctx.fillText(fmtTps(this.max), cx + Math.cos(ra) * (r - 44), cy + Math.sin(ra) * (r - 44));
    }

    ctx.textAlign = "center";
    ctx.textBaseline = "alphabetic";
    const main = this.kind === "speed" ? (this.est ? "≈" + fmtTps(this.value) : fmtTps(this.value)) : fmtTokens(this.value);
    let fs = Math.round(r * 0.44);
    ctx.font = `600 ${fs}px ${FONT}`;
    const maxW = (r - 24) * 2;
    while (fs > 14 && ctx.measureText(main).width > maxW) {
      fs -= 2;
      ctx.font = `600 ${fs}px ${FONT}`;
    }
    ctx.fillStyle = "#e6e9f0";
    ctx.fillText(main, cx, cy + 2);
    ctx.fillStyle = "#8b93a7";
    ctx.font = `11px ${FONT}`;
    ctx.fillText(this.unit, cx, cy + r * 0.46);
  }
}

/** 悬浮窗用的迷你仪表盘 */
export class MiniGauge extends BaseGauge {
  constructor(canvas: HTMLCanvasElement, opts?: { color?: string; color2?: string }) {
    super(canvas, { color: opts?.color ?? "#22d3ee", color2: opts?.color2 ?? "#0ea5e9", minScale: 10 });
  }

  protected draw() {
    const fit = fitCanvas(this.canvas);
    if (!fit) return;
    const { ctx, w, h } = fit;
    const cx = w / 2;
    const cy = h * 0.52;
    const r = Math.min(w, h) * 0.36;

    ctx.lineWidth = 7;
    ctx.lineCap = "round";
    ctx.strokeStyle = "rgba(255,255,255,0.08)";
    ctx.beginPath();
    ctx.arc(cx, cy, r, A0, A0 + SWEEP);
    ctx.stroke();

    const frac = Math.max(0.0001, Math.min(1, this.value / this.max));
    const color = this.est ? EST_COLOR : this.opts.color;
    const color2 = this.est ? "#d97706" : (this.opts.color2 ?? this.opts.color);
    ctx.save();
    ctx.shadowColor = color;
    ctx.shadowBlur = 9;
    ctx.strokeStyle = progressGradient(ctx, cx, cy, frac, color, color2);
    ctx.beginPath();
    ctx.arc(cx, cy, r, A0, A0 + SWEEP * frac);
    ctx.stroke();
    ctx.restore();

    ctx.textAlign = "center";
    ctx.textBaseline = "alphabetic";
    ctx.fillStyle = this.value > 0 ? "#e6e9f0" : "#8b93a7";
    ctx.font = `600 ${Math.round(r * 0.5)}px ${FONT}`;
    ctx.fillText((this.est ? "≈" : "") + fmtTps(this.value), cx, cy + r * 0.12);
    ctx.fillStyle = "#8b93a7";
    ctx.font = `9px ${FONT}`;
    ctx.fillText("t/s", cx, cy + r * 0.55);
  }
}

/** 近 15 分钟速度曲线（10 秒一档）；x 轴为真实墙钟时刻，整条曲线随时间连续左移 */
const SPARK_BUCKET_MS = 10_000;
const SPARK_GRID_MS = 5 * 60_000;

export function drawSpark(
  canvas: HTMLCanvasElement,
  values: number[],
  color: string,
  nowMs: number,
) {
  const fit = fitCanvas(canvas);
  if (!fit) return;
  const { ctx, w, h } = fit;
  const padL = 34;
  const padR = 10;
  const padT = 8;
  const padB = 18;
  const iw = w - padL - padR;
  const ih = h - padT - padB;
  const peak = Math.max(10, niceCeil(Math.max(...values, 0) * 1.25));
  const n = values.length;

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
    ctx.fillText(fmtTps((peak * (2 - i)) / 2), padL - 6, y);
  }

  if (n < 2) return;
  const phase = (((nowMs % SPARK_BUCKET_MS) + SPARK_BUCKET_MS) % SPARK_BUCKET_MS) / SPARK_BUCKET_MS;
  const dx = iw / n;
  const x = (i: number) => padL + iw - ((n - 1 - i) + phase) * dx;
  const y = (v: number) => padT + ih - (Math.min(v, peak) / peak) * ih;

  // ---- x 轴真实时刻刻度（5 分钟整分）：与数据点同一时间映射反解 x，可直接对表验证。
  // 最新桶结束时刻 = 下一个 10s 边界；右缘即"现在"（差 ≤1 档，肉眼不可辨）
  const tLastEnd = Math.floor(nowMs / SPARK_BUCKET_MS) * SPARK_BUCKET_MS + SPARK_BUCKET_MS;
  const xAt = (t: number) => padL + iw - ((tLastEnd - t) / SPARK_BUCKET_MS) * dx;
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  let t = Math.ceil((tLastEnd - n * SPARK_BUCKET_MS) / SPARK_GRID_MS) * SPARK_GRID_MS;
  for (; t <= tLastEnd; t += SPARK_GRID_MS) {
    const gx = xAt(t);
    if (gx < padL || gx > padL + iw) continue;
    ctx.strokeStyle = "rgba(255,255,255,0.05)";
    ctx.beginPath();
    ctx.moveTo(gx, padT);
    ctx.lineTo(gx, padT + ih);
    ctx.stroke();
    const d = new Date(t);
    const hh = d.getHours().toString().padStart(2, "0");
    const mm = d.getMinutes().toString().padStart(2, "0");
    ctx.fillText(`${hh}:${mm}`, gx, h - padB + 4);
  }
  // 右缘：当前时刻（靠右对齐避免溢出）
  ctx.textAlign = "right";
  ctx.fillStyle = "rgba(139,147,167,0.9)";
  ctx.fillText(`现在 ${fmtClock(nowMs).slice(0, 5)}`, padL + iw, h - padB + 4);

  const grad = ctx.createLinearGradient(0, padT, 0, padT + ih);
  grad.addColorStop(0, color + "52");
  grad.addColorStop(1, color + "00");
  ctx.fillStyle = grad;
  ctx.beginPath();
  ctx.moveTo(x(0), y(values[0]));
  for (let i = 1; i < n; i++) ctx.lineTo(x(i), y(values[i]));
  ctx.lineTo(x(n - 1), padT + ih);
  ctx.lineTo(x(0), padT + ih);
  ctx.closePath();
  ctx.fill();

  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.lineJoin = "round";
  ctx.beginPath();
  ctx.moveTo(x(0), y(values[0]));
  for (let i = 1; i < n; i++) ctx.lineTo(x(i), y(values[i]));
  ctx.stroke();

  const last = n - 1;
  ctx.save();
  ctx.shadowColor = color;
  ctx.shadowBlur = 8;
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.arc(x(last), y(values[last]), 3, 0, TAU);
  ctx.fill();
  ctx.restore();
}
