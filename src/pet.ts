// 桌宠宠物包（Codex Pet 格式，来自 dsh-desk 项目，MIT）
// 精灵图：1536 宽 × 8 列，每帧 192×cellHeight；行序对应动画

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
  private stateText = "待机";
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
  }

  private load() {
    this.img = null;
    const img = new Image();
    img.src = this.pack.sheet;
    img.onload = () => {
      this.img = img;
    };
  }

  setLive(tps: number, state: "idle" | "running" | "estimating") {
    this.tps = state === "estimating" ? "≈" + tps.toFixed(1) : tps.toFixed(1);
    this.est = state === "estimating";
    this.stateText = state === "idle" ? "待机" : state === "estimating" ? "估算中" : "生成中";
    // 只有 IO 实测到流式输出才播放跑步动画；估算回退时保持站立
    this.anim = state === "running" ? "running" : "idle";
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

    const anim = this.pack.anims[this.anim] ?? this.pack.anims.idle;
    if (now - this.lastFrameAt >= this.pack.frameMs) {
      this.lastFrameAt = now;
      this.frame = (this.frame + 1) % anim.frames;
    }
    const img = this.img;
    if (img) {
      const sx = this.frame * this.pack.cellW;
      const sy = anim.row * this.pack.cellH;
      // 底部贴边居中：精灵图单元自带透明边距，放大并紧贴下缘，避免脚下留大片空白
      const scale = Math.min((w * 0.94) / this.pack.cellW, (h * 0.92) / this.pack.cellH);
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

    // 头顶速度气泡（贴顶）
    ctx.textAlign = "center";
    ctx.font = `600 15px "Segoe UI", "Microsoft YaHei", sans-serif`;
    const label = `${this.tps} t/s`;
    const bw = ctx.measureText(label).width + 22;
    const bx = w / 2 - bw / 2;
    const by = 2;
    ctx.fillStyle = "rgba(13,20,36,0.88)";
    ctx.strokeStyle = this.est
      ? "rgba(251,191,36,0.75)"
      : this.stateText === "生成中"
        ? "rgba(34,211,238,0.75)"
        : "rgba(255,255,255,0.22)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.roundRect(bx, by, bw, 26, 13);
    ctx.fill();
    ctx.stroke();
    // 气泡小尾巴
    ctx.beginPath();
    ctx.moveTo(w / 2 - 5, by + 25);
    ctx.lineTo(w / 2 + 5, by + 25);
    ctx.lineTo(w / 2, by + 33);
    ctx.closePath();
    ctx.fillStyle = "rgba(13,20,36,0.88)";
    ctx.fill();
    ctx.fillStyle = this.est ? "#fbbf24" : this.stateText === "生成中" ? "#22d3ee" : "#8b93a7";
    ctx.fillText(label, w / 2, by + 18);
    ctx.font = `10px "Segoe UI", sans-serif`;
    ctx.fillStyle = "rgba(139,147,167,0.9)";
    ctx.fillText(this.stateText, w / 2, by + 44);
  }
}
