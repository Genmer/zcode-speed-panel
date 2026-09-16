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
}

const MIN_DUR = 50;
const WINDOW = 10 * 60 * 1000;
const CUTOFF = 90 * 1000;
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
  return {
    completed: now + duration,
    duration,
    output,
    input: Math.round(rnd(15000, 60000)),
    cache: Math.round(rnd(10000, 250000)),
    session: `mock-sess-${sessionNo}`,
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

function snapshot(now: number): Snapshot {
  let out = 0,
    input = 0,
    cache = 0,
    dur = 0,
    wOut = 0,
    wDur = 0,
    last = 0;
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
    last = Math.max(last, c.completed);
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
  const comps = [...new Set(calls.map((c) => c.completed))].sort((a, b) => a - b);
  const gaps: number[] = [];
  for (let i = 1; i < comps.length; i++) {
    const g = comps[i] - comps[i - 1];
    if (g > 0 && g < 600000) gaps.push(g);
  }
  let grace = 60000;
  if (gaps.length >= 3) {
    const tail = gaps.slice(Math.max(0, gaps.length - 10)).sort((a, b) => a - b);
    grace = Math.min(240000, Math.max(20000, tail[Math.floor(tail.length / 2)]));
  }
  const since = now - last;
  const isLive = last > 0 && since <= CUTOFF;
  const isEstimating = !isLive && last > 0 && since <= grace && wDur > 0;
  const currentTps = isLive || isEstimating ? (wDur > 0 ? wOut / (wDur / 1000) : 0) : 0;
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
    liveSource: isLive ? "io" : isEstimating ? "window" : "idle",
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
    onData(snapshot(t));
  };

  tick();
  setInterval(tick, 400);
}
