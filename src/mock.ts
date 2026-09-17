// 浏览器预览模式：模拟 ZCode 的 model-io 调用流，便于无 Tauri 环境下预览 UI

export interface Snapshot {
  currentTps: number;
  avgTps: number;
  totalTokens: number;
  outputTokens: number;
  inputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  callsToday: number;
  sessionsToday: number;
  isLive: boolean;
  isEstimating: boolean;
  /** 调用已开始但首字节未到（TTFT）：显示"统计中…"提示而非估算值 */
  isStarting: boolean;
  /** 实测流式已开始但 30s 滑窗未填满（显示"统计中"） */
  ramping: boolean;
  /** 近 10 分钟已完成调用的真实速度（落盘口径） */
  windowTps: number;
  /** 最近一次已完成调用的真实速度（落盘口径），当前速度卡右上角小表用 */
  lastCallTps: number;
  liveSource: string;
  lastActivityMs: number;
  nowMs: number;
  rolloutDir: string;
  spark: number[];
}

interface MockCall {
  completed: number;
  duration: number;
  output: number;
  input: number;
  cache: number;
  session: string;
  /** 管道静默调用：整段无增量字节，走 ≈ 估算显示 */
  silent: boolean;
}

const MIN_DUR = 50;
const WINDOW = 10 * 60 * 1000;
const BUCKETS = 90;
const BUCKET = 10_000;

const rnd = (a: number, b: number) => a + Math.random() * (b - a);

let calls: MockCall[] = [];
let sessionNo = 1;

function newCall(now: number): MockCall {
  if (Math.random() < 0.18) sessionNo++;
  const duration = Math.exp(rnd(Math.log(12000), Math.log(180000)));
  const tps = rnd(18, 70);
  const output = Math.max(60, Math.round((duration / 1000) * tps));
  // usage 库语义:cache_read 是 input 的子集,命中率常态 90%+
  const input = Math.round(rnd(15000, 60000));
  return {
    completed: now + duration,
    duration,
    output,
    input,
    cache: Math.round(input * rnd(0.8, 0.99)),
    session: `mock-sess-${sessionNo}`,
    silent: Math.random() < 0.22,
  };
}

function seedHistory(now: number) {
  let t = now - 3 * 3600 * 1000;
  while (t < now) {
    if (Math.random() < 0.72) {
      const burst = Math.round(rnd(3, 14));
      for (let i = 0; i < burst && t < now; i++) {
        const c = newCall(t);
        if (c.completed > now) break;
        calls.push(c);
        t = c.completed + rnd(300, 2500);
      }
      t += rnd(20000, 240000);
    } else {
      t += rnd(60000, 300000);
    }
  }
  sessionNo = Math.max(sessionNo, 6);
}

function snapshot(now: number, pending: MockCall | null): Snapshot {
  let out = 0,
    input = 0,
    cache = 0,
    dur = 0,
    wOut = 0,
    wDur = 0,
    last = 0,
    lastTps = 0;
  const sessions = new Set<string>();
  const buckets = new Array<number>(BUCKETS).fill(0);
  const bucketDur = new Array<number>(BUCKETS).fill(0);
  const nowSlot = Math.floor(now / BUCKET);
  for (const c of calls) {
    const d = Math.max(MIN_DUR, c.duration);
    out += c.output;
    input += c.input;
    cache += c.cache;
    dur += d;
    if (c.completed >= last) {
      last = c.completed;
      lastTps = c.output / (d / 1000); // 与后端一致：取完成时刻最晚一条的 eff ÷ 纯生成时长
    }
    sessions.add(c.session);
    if (c.completed >= now - WINDOW) {
      wOut += c.output;
      wDur += d;
    }
    const slot = nowSlot - Math.floor(c.completed / BUCKET);
    if (slot >= 0 && slot < BUCKETS) {
      buckets[BUCKETS - 1 - slot] += c.output;
      bucketDur[BUCKETS - 1 - slot] += d;
    }
  }
  // 门控模型与后端一致：以进行中的调用（pending）为准。
  // 模拟 TTFT ~2.5s：启动期显示"统计中…"；约 1/5 的调用为管道静默（整段 ≈ 估算）
  const pendingStart = pending ? pending.completed - pending.duration : 0;
  const ageSec = pending ? (now - pendingStart) / 1000 : Infinity;
  const isStarting = !!pending && ageSec < 2.5 && !pending.silent;
  const isLive = !!pending && ageSec >= 2.5 && !pending.silent;
  const isEstimating = !!pending && pending.silent && wDur > 0;
  const currentTps = isStarting
    ? 0
    : isLive || isEstimating
      ? wDur > 0
        ? wOut / (wDur / 1000)
        : 0
      : 0;
  const spark = buckets.map((o, i) => (bucketDur[i] > 0 ? o / (bucketDur[i] / 1000) : 0));
  if (isEstimating && spark[BUCKETS - 1] <= 0) {
    spark[BUCKETS - 1] = currentTps;
  }
  return {
    currentTps,
    avgTps: dur > 0 ? out / (dur / 1000) : 0,
    totalTokens: out + input,
    outputTokens: out,
    inputTokens: input,
    cacheCreationTokens: 0,
    cacheReadTokens: cache,
    callsToday: calls.length,
    sessionsToday: sessions.size,
    isLive,
    isEstimating,
    isStarting,
    ramping: isLive && ageSec < 30,
    windowTps: wDur > 0 ? wOut / (wDur / 1000) : 0,
    lastCallTps: lastTps,
    liveSource: isStarting || isLive ? "io" : isEstimating ? "window" : "idle",
    lastActivityMs: last,
    nowMs: now,
    rolloutDir: "（浏览器预览 · 模拟数据）",
    spark,
  };
}

export function startMock(onData: (s: Snapshot) => void) {
  const now = Date.now();
  seedHistory(now);
  let pending: MockCall | null = null;
  let nextStart = now + rnd(1000, 4000);

  const tick = () => {
    const t = Date.now();
    if (pending && t >= pending.completed) {
      calls.push(pending);
      // 只保留最近 30 分钟
      const cutoff = t - 30 * 60 * 1000;
      calls = calls.filter((c) => c.completed >= cutoff);
      pending = null;
      nextStart = t + rnd(200, 2500);
    }
    if (!pending && t >= nextStart) {
      pending = newCall(t);
    }
    onData(snapshot(t, pending));
  };

  tick();
  setInterval(tick, 400);
}
