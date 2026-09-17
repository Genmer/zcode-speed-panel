// 桌宠宠物包（Codex Pet 格式，来自 dsh-desk 项目，MIT）
// 精灵图：1536 宽 × 8 列，每帧 192×cellHeight；行序对应动画

import { SPEED_TIERS, fmtTps, speedColor } from "./gauges";

const FONT = `"Segoe UI", "Microsoft YaHei", sans-serif`;

/** 头顶气泡排版（随悬停进度在单行/两行之间插值） */
interface BubbleLayout {
  rows: Array<{ label: string; value: string; color: string }>;
  /** 悬停进度 0~1：文字与尺寸的插值系数 */
  t: number;
  fs: number;
  labelFs: number;
  lineH: number;
  gapY: number;
  padX: number;
  padY: number;
  colGap: number;
  /** 已乘悬停进度：过标签列随之滑入，避免数值被挤出框 */
  labelW: number;
  boxW: number;
  boxH: number;
  /** 相对单行气泡多占的高度：精灵按此缩小让位 */
  grow: number;
  /** 气泡可见度（待机为 0）：整块淡入淡出 */
  vis: number;
}

export interface PetPack {
  id: string;
  displayName: string;
  sheet: string;
  sheetW: number;
  cellW: number;
  cellH: number;
  rows: number;
  /** 行序 → 动画名与帧数 */
  anims: Record<string, { row: number; frames: number }>;
  frameMs: number;
}

export const PET_PACKS: PetPack[] = [
  {
    id: "yuexinmiao",
    displayName: "月薪喵",
    sheet: "pets/yuexinmiao/spritesheet.webp",
    sheetW: 1536,
    cellW: 192,
    cellH: 208,
    rows: 9,
    anims: {
      idle: { row: 0, frames: 6 },
      running_right: { row: 1, frames: 8 },
      running_left: { row: 2, frames: 8 },
      waving: { row: 3, frames: 4 },
      jumping: { row: 4, frames: 5 },
      failed: { row: 5, frames: 8 },
      waiting_permission: { row: 6, frames: 6 },
      running: { row: 7, frames: 6 },
      review: { row: 8, frames: 6 },
    },
    frameMs: 160,
  },
  {
    id: "maid-deepseek-whale",
    displayName: "鲸鱼女仆",
    sheet: "pets/maid-deepseek-whale/spritesheet.webp",
    sheetW: 1536,
    cellW: 192,
    cellH: 208,
    rows: 11,
    anims: {
      idle: { row: 0, frames: 7 },
      running_right: { row: 1, frames: 8 },
      running_left: { row: 2, frames: 8 },
      waving: { row: 3, frames: 4 },
      jumping: { row: 4, frames: 5 },
      failed: { row: 5, frames: 8 },
      waiting_permission: { row: 6, frames: 6 },
      running: { row: 7, frames: 6 },
      review: { row: 8, frames: 6 },
    },
    frameMs: 160,
  },
];

export function packById(id: string): PetPack {
  return PET_PACKS.find((p) => p.id === id) ?? PET_PACKS[0];
}

/** 桌宠画布：精灵动画 + 状态切换 + 头顶速度气泡 */
export class PetWidget {
  private canvas: HTMLCanvasElement;
  private pack: PetPack;
  private img: HTMLImageElement | null = null;
  private anim = "idle";
  private frame = 0;
  private lastFrameAt = 0;
  private tps = "";
  /** 上轮均速（最近一次已完成调用，落盘口径）；0 = 今日尚无已完成调用 */
  private lastTps = 0;
  /** 鼠标悬停：气泡由单行实时速度变两行（实时速度 / 上轮均速） */
  private hover = false;
  /** 悬停进度 0~1（平滑过渡，同时驱动气泡尺寸与精灵让位） */
  private hoverT = 0;
  /** 气泡可见度 0~1：待机时不显示（悬停除外），淡入淡出同时驱动精灵让位 */
  private visT = 0;
  private lastDrawAt = 0;
  private running = false;
  private est = false;
  private raf = 0;
  private expandBtn: HTMLElement | null = null;
  private cycleBtn: HTMLElement | null = null;

  constructor(canvas: HTMLCanvasElement, packId: string, onPackSwitch?: () => void) {
    this.canvas = canvas;
    this.pack = packById(packId);
    this.load();
    // 双击切换下一只宠物
    canvas.addEventListener("dblclick", () => {
      const i = PET_PACKS.findIndex((p) => p.id === this.pack.id);
      this.pack = packById(PET_PACKS[(i + 1) % PET_PACKS.length].id);
      this.load();
      onPackSwitch?.();
    });
    // 悬停监听挂在整块悬浮窗上（按钮是画布兄弟节点，挂画布会在移到按钮上时误判离开）
    const hoverTarget = canvas.parentElement ?? canvas;
    hoverTarget.addEventListener("mouseenter", () => {
      this.hover = true;
    });
    hoverTarget.addEventListener("mouseleave", () => {
      this.hover = false;
    });
  }

  private load() {
    this.img = null;
    const img = new Image();
    img.src = this.pack.sheet;
    img.onload = () => {
      this.img = img;
    };
  }

  setLive(tps: number, state: "idle" | "running" | "estimating" | "starting") {
    // 启动等待（首字节未到）显示 "…"，与表盘的统计中提示一致
    this.tps = state === "starting" ? "…" : state === "estimating" ? "≈" + tps.toFixed(1) : tps.toFixed(1);
    this.est = state === "estimating";
    this.running = state === "running" || state === "starting";
    // 只有实测到流式输出（或刚启动等待中）才播放跑步动画；估算回退时保持站立
    this.anim = this.running ? "running" : "idle";
  }

  /** 上轮均速（最近一次已完成调用速度，落盘口径）：悬停气泡第二行用 */
  setLast(tps: number) {
    this.lastTps = isFinite(tps) && tps > 0 ? tps : 0;
  }

  get packId(): string {
    return this.pack.id;
  }

  /** 切换到下一只宠物（自动保存由调用方处理） */
  cyclePack(): string {
    const i = PET_PACKS.findIndex((p) => p.id === this.pack.id);
    this.pack = packById(PET_PACKS[(i + 1) % PET_PACKS.length].id);
    this.load();
    return this.pack.id;
  }

  start() {
    const loop = (now: number) => {
      this.draw(now);
      this.raf = requestAnimationFrame(loop);
    };
    this.raf = requestAnimationFrame(loop);
  }

  stop() {
    cancelAnimationFrame(this.raf);
  }

  private draw(now: number) {
    const canvas = this.canvas;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (w < 8 || h < 8) return;
    const dpr = window.devicePixelRatio || 1;
    const pw = Math.round(w * dpr);
    const ph = Math.round(h * dpr);
    if (canvas.width !== pw || canvas.height !== ph) {
      canvas.width = pw;
      canvas.height = ph;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    // 悬停进度（平滑过渡）：0 = 单行实时速度，1 = 两行（实时速度 / 上轮均速）
    const dt = Math.min(0.1, Math.max(0, (now - this.lastDrawAt) / 1000));
    this.lastDrawAt = now;
    const want = this.hover ? 1 : 0;
    this.hoverT += (want - this.hoverT) * (1 - Math.exp(-dt * 12));
    if (Math.abs(want - this.hoverT) < 0.002) this.hoverT = want;
    // 气泡可见度：待机（无生成任务）时不显示气泡；生成/估算/启动等待时显示，
    // 悬停也强制显示（待机时移上去仍能看上一轮均速）
    const wantVis = this.hover || this.running || this.est ? 1 : 0;
    this.visT += (wantVis - this.visT) * (1 - Math.exp(-dt * 12));
    if (Math.abs(wantVis - this.visT) < 0.002) this.visT = wantVis;

    const anim = this.pack.anims[this.anim] ?? this.pack.anims.idle;
    if (now - this.lastFrameAt >= this.pack.frameMs) {
      this.lastFrameAt = now;
      this.frame = (this.frame + 1) % anim.frames;
    }

    // 先排气泡：精灵要让出悬停时多出的那一行高度（否则第二行压住脑袋）
    const bubble = this.layoutBubble(ctx, w);
    const img = this.img;    if (img) {
      const sx = this.frame * this.pack.cellW;
      const sy = anim.row * this.pack.cellH;
      // 底部贴边居中：精灵图单元自带透明边距，放大并紧贴下缘，避免脚下留大片空白。
      // 悬停时从可用高度里扣掉气泡多占的部分 —— 精灵等比缩小，可见部分被遮挡的量
      // 与单行气泡时完全一致（气泡始终只压住精灵图上缘那段透明边距）
      const availH = Math.max(this.pack.cellH * 0.15, h * 0.92 - bubble.grow);
      const scale = Math.min((w * 0.94) / this.pack.cellW, availH / this.pack.cellH);
      const dw = this.pack.cellW * scale;
      const dh = this.pack.cellH * scale;
      const dx = (w - dw) / 2;
      ctx.drawImage(img, sx, sy, this.pack.cellW, this.pack.cellH, dx, h - dh - 2, dw, dh);

      // 按钮贴到精灵脚部右侧（跟随实际绘制宽度）
      if (!this.expandBtn) this.expandBtn = document.getElementById("float-pet-expand");
      if (!this.cycleBtn) this.cycleBtn = document.getElementById("float-pet-cycle");
      const rightGap = Math.max(4, w - (dx + dw) + 2);
      if (this.expandBtn) this.expandBtn.style.right = `${rightGap}px`;
      if (this.cycleBtn) this.cycleBtn.style.right = `${rightGap + 26}px`;
    }

    // 待机且未悬停：整块不画（连尾巴也不留）
    if (bubble.vis > 0.01) this.paintBubble(ctx, w, bubble);
  }

  /** 气泡排版：随悬停进度在"单行实时速度"与"两行（实时速度 / 上轮均速）"之间插值。
   *  桌宠窗口可缩到 100px，字号随窗口缩放并按可用宽度收缩，保证气泡不超宽 */
  private layoutBubble(ctx: CanvasRenderingContext2D, w: number): BubbleLayout {
    const padX = 11;
    // 内边距/行高取到与旧版单行气泡等高（12 + 15×1.2 ≈ 原 26px），
    // 否则单行状态也会平白多压住精灵一点
    const padY = 5;
    const colGap = 8;
    const gapY = 3;
    const t = this.hoverT;
    const liveColor = this.est ? "#fbbf24" : this.running ? "#22d3ee" : "#8b93a7";
    const rows = [
      { label: "实时速度", value: `${this.tps} t/s`, color: liveColor },
      {
        label: "上轮均速",
        value: this.lastTps > 0 ? `${fmtTps(this.lastTps)} t/s` : "--",
        color: speedColor(this.lastTps, SPEED_TIERS),
      },
    ];

    let fs = Math.max(11, Math.min(15, w * 0.075));
    let labelFs = Math.max(8, fs * 0.78);
    let lineH = fs * 1.2;
    let labelW = 0;
    let valueW = 0;
    const maxBoxW = Math.max(40, w - 6);
    const twoRows = t > 0.01; // 过渡中按两行宽度排（否则字会先溢出再收缩）
    for (;;) {
      ctx.font = `600 ${fs}px ${FONT}`;
      valueW = Math.max(
        ctx.measureText(rows[0].value).width,
        twoRows ? ctx.measureText(rows[1].value).width : 0,
      );
      ctx.font = `500 ${labelFs}px ${FONT}`;
      labelW = twoRows
        ? Math.max(ctx.measureText(rows[0].label).width, ctx.measureText(rows[1].label).width)
        : 0;
      if (padX * 2 + labelW + colGap + valueW <= maxBoxW || fs <= 8) break;
      fs -= 1;
      labelFs = Math.max(8, fs * 0.78);
      lineH = fs * 1.2;
    }

    const boxW1 = padX * 2 + valueW;
    const boxW2 = padX * 2 + labelW + colGap + valueW;
    const boxH1 = padY * 2 + lineH;
    const boxH2 = padY * 2 + lineH * 2 + gapY;
    const vis = this.visT;
    return {
      rows,
      t,
      vis,
      fs,
      labelFs,
      lineH,
      gapY,
      padX,
      padY,
      colGap,
      labelW: labelW * t,
      boxW: boxW1 + (boxW2 - boxW1) * t,
      boxH: boxH1 + (boxH2 - boxH1) * t,
      grow: (boxH2 - boxH1) * t * vis,
    };
  }

  private paintBubble(ctx: CanvasRenderingContext2D, w: number, b: BubbleLayout) {
    const bx = w / 2 - b.boxW / 2;
    const by = 2;
    const bg = "rgba(13,20,36,0.88)";

    // 底板与尾巴随可见度淡入淡出（待机时整块消失）
    ctx.globalAlpha = b.vis;
    ctx.fillStyle = bg;
    ctx.strokeStyle = this.est
      ? "rgba(251,191,36,0.75)"
      : this.running
        ? "rgba(34,211,238,0.75)"
        : "rgba(255,255,255,0.22)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.roundRect(bx, by, b.boxW, b.boxH, Math.min(13, b.boxH / 2));
    ctx.fill();
    ctx.stroke();
    // 气泡小尾巴
    ctx.beginPath();
    ctx.moveTo(w / 2 - 5, by + b.boxH - 1);
    ctx.lineTo(w / 2 + 5, by + b.boxH - 1);
    ctx.lineTo(w / 2, by + b.boxH + 7);
    ctx.closePath();
    ctx.fillStyle = bg;
    ctx.fill();

    // 按气泡当前高度裁剪：过渡时第二行随气泡长高而露出，不会先画到框外
    ctx.save();
    ctx.beginPath();
    ctx.rect(bx, by, b.boxW, b.boxH);
    ctx.clip();
    ctx.textAlign = "left";
    ctx.textBaseline = "middle";
    b.rows.forEach((r, i) => {
      if (i > 0 && b.t <= 0.01) return;
      const cy = by + b.padY + b.lineH * (i + 0.5) + b.gapY * i;
      let x = bx + b.padX;
      if (r.label && b.t > 0.01) {
        ctx.globalAlpha = b.t * b.vis;
        ctx.font = `500 ${b.labelFs}px ${FONT}`;
        ctx.fillStyle = "#8b93a7";
        ctx.fillText(r.label, x, cy);
        x += b.labelW + b.colGap;
      }
      ctx.globalAlpha = (i > 0 ? b.t : 1) * b.vis;
      ctx.font = `600 ${b.fs}px ${FONT}`;
      ctx.fillStyle = r.color;
      ctx.fillText(r.value, x, cy);
    });
    ctx.restore();
    ctx.globalAlpha = 1;
  }
}
