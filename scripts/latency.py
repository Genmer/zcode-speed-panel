# -*- coding: utf-8 -*-
"""启停响应与精度验证：verify-log2.jsonl（新逻辑）× DB 真实调用时间
验证：①起步延迟（首字节后判定开始的滞后）②停止延迟（completed → 持续归零）
     ③起步段无 0 读数 ④逐调用/积分精度 ⑤静默期误判（streaming 应为 false）
"""
import json, sqlite3, datetime, zoneinfo, statistics as st
from pathlib import Path

TZ = zoneinfo.ZoneInfo("Asia/Shanghai")
LOG = Path(__file__).parent.parent / "src-tauri/target/verify-log4.jsonl"
ticks = [json.loads(l) for l in LOG.read_text(encoding="utf-8").splitlines() if l.strip()]
t0, t1 = ticks[0]["t"], ticks[-1]["t"]
TICK = 0.7
print(f"采样窗: {datetime.datetime.fromtimestamp(t0/1000, TZ):%H:%M:%S} → "
      f"{datetime.datetime.fromtimestamp(t1/1000, TZ):%H:%M:%S}  ({(t1-t0)/1000:.0f}s, {len(ticks)} 拍)")

con = sqlite3.connect(f"file:{(Path.home()/'.zcode/cli/db/db.sqlite').as_posix()}?mode=ro", uri=True)
cur = con.cursor()
cur.execute("""SELECT session_id, completed_at, first_token_at,
               CASE WHEN completed_at > first_token_at THEN completed_at-first_token_at ELSE duration_ms END,
               output_tokens+reasoning_tokens
               FROM model_usage WHERE status='completed' AND first_token_at IS NOT NULL
                 AND completed_at > ? AND completed_at <= ? ORDER BY completed_at""", (t0 - 1000, t1))
calls = [c for c in cur.fetchall() if c[2] >= t0 - 5000]
print(f"窗内调用: {len(calls)} 次")

states = [(tk["t"], tk["live"]["stream"], tk["live"]["tps"], tk["live"]["ramp"]) for tk in ticks]

def turn_on_after(ts):
    """ts 之后第一个 stream=true 的时刻（要求此前是 false）"""
    for i, (t, s, _, _) in enumerate(states):
        if t < ts: continue
        if s and (i == 0 or not states[i-1][1]):
            return t
        if s:
            return t  # 已处于 on
    return None

def stop_lag_after(comp):
    """comp 之后第一个 ≥2 拍 false 段的起点 − 之前最后的 true 拍"""
    for i, (t, s, _, _) in enumerate(states):
        if t < comp: continue
        if not s and i > 0 and not states[i-1][1]:
            # 找到 false 段起点 i-1，其前最后一个 true
            j = i - 1
            while j > 0 and states[j-1][1]:
                j -= 1
            return states[j][0], t
    return None, None

print("\n[逐调用] 会话   完成时刻  起步延迟  停止拖尾 | 起步段0读数 | 显示均tps  真实tps  比值")
start_lats, stop_lags, zeros, ratios = [], [], [], []
for k, (sess, comp, ftok, gen, eff) in enumerate(calls):
    on_t = turn_on_after(ftok - 300)
    lat_s = (on_t - ftok) / 1000 if on_t else None
    last_true, false_start = stop_lag_after(comp)
    lat_e = (last_true - comp) / 1000 if last_true else 0.0
    ramp_ticks = [tk for tk in ticks if ftok <= tk["t"] <= min(ftok + 30_000, t1) and tk["live"]["stream"]]
    z = sum(1 for tk in ramp_ticks if tk["live"]["tps"] <= 0.01)
    zeros.append(z)
    win = [tk for tk in ticks if ftok <= tk["t"] <= comp and tk["live"]["stream"]]
    disp = st.mean([tk["live"]["tps"] for tk in win]) if win else 0.0
    true_tps = eff / (gen / 1000)
    r = None
    if eff > 200 and win:
        r = disp / true_tps
        ratios.append(r)
    if lat_s is not None:
        start_lats.append(lat_s)
    if last_true:
        stop_lags.append(lat_e)
    print(f"  {sess[-4:]}  {datetime.datetime.fromtimestamp(comp/1000, TZ):%H:%M:%S}  "
          f"{lat_s if lat_s is None else f'{lat_s:5.1f}s'}   "
          f"{lat_e:5.1f}s | {z}/{len(ramp_ticks)} 拍 | {disp:7.1f}  {true_tps:7.1f}  "
          f"{'' if r is None else f'{r:.2f}x'}")

print(f"\n[汇总] 起步延迟: {['%.1f' % x for x in start_lats]}")
print(f"       停止拖尾(完成→最后true拍): {['%.1f' % x for x in stop_lags]}")
print(f"       起步段 0 读数: {sum(zeros)} 拍 (共 {len(zeros)} 次调用)")
if ratios:
    print(f"       逐调用 显示/真值: 中位 {st.median(ratios):.2f}x  范围 {min(ratios):.2f}~{max(ratios):.2f}x  平均绝对偏差 {st.mean([abs(r-1) for r in ratios]):.0%}")
integ = sum(tk["live"]["tps"] * TICK for tk in ticks[1:] if tk["live"]["stream"])
true_sum = sum(c[4] for c in calls)
print(f"       积分: {integ:.0f} tok vs 真实 {true_sum} tok ({(integ-true_sum)/true_sum:+.1%})" if true_sum else "")
stream_n = sum(1 for tk in ticks if tk["live"]["stream"])
ramp_n = sum(1 for tk in ticks if tk["live"]["stream"] and tk["live"]["ramp"])
print(f"       时间结构: 流式 {stream_n} 拍 ({stream_n/len(ticks):.0%}), 其中统计中 {ramp_n} 拍")
con.close()
