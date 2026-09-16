# -*- coding: utf-8 -*-
"""三源对比：仪表盘实时显示(live.tps) vs 统计图口径(落盘桶速度) vs 每轮调用结束后真值
用法: python scripts/compare3.py [verify-log6.jsonl]
数据源: verify 示例的三类日志 kind=tick/call/cal
"""
import json, sys, datetime, zoneinfo, statistics as st
from pathlib import Path

TZ = zoneinfo.ZoneInfo("Asia/Shanghai")
LOG = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent.parent / "src-tauri/target/verify-log6.jsonl"
recs = [json.loads(l) for l in LOG.read_text(encoding="utf-8").splitlines() if l.strip()]
ticks = [r for r in recs if r.get("kind") == "tick"]
t_win0, t_win1 = ticks[0]["t"], ticks[-1]["t"]
# 第一拍会摄取今日全部历史调用，只保留窗内完成的
calls = [r for r in recs if r.get("kind") == "call" and t_win0 - 5000 <= r["done"] <= t_win1]
cals = {r["id"]: r for r in recs if r.get("kind") == "cal"}
f = lambda t: datetime.datetime.fromtimestamp(t/1000, TZ).strftime('%H:%M:%S')
t0, t1 = t_win0, t_win1
print(f"采样窗 {f(t0)} → {f(t1)} ({(t1-t0)/1000:.0f}s)  调用 {len(calls)} 次\n")

print("[逐调用三源对比]")
print("完成时刻  eff  gen_s | 真实tps | 仪表盘均(覆盖) | 图表桶均 | 校准样本bpt(生效bpt) | 仪表/真实  图表/真实")
gauge_ratios, chart_ratios = [], []
for c in calls:
    done, gen, eff, true_tps = c["done"], c["gen_ms"], c["eff"], c["true_tps"]
    w0 = done - max(gen, 0)
    # 仪表盘:该调用区间内 stream 拍的显示均值
    on = [tk for tk in ticks if w0 <= tk["t"] <= done and tk["live"]["stream"]]
    gauge = st.mean([tk["live"]["tps"] for tk in on]) if on else 0.0
    cover = len(on) / max(1, sum(1 for tk in ticks if w0 <= tk["t"] <= done))
    # 图表: spark_tail 只是尾3桶,改由落盘桶重建——用 call 真值聚合到 10s 桶后取区间均值
    # (这里直接用同窗调用的真实 tps 按生成时长加权 = 图表口径)
    same = [x for x in calls if not (x["done"] < w0 or x["done"] - x["gen_ms"] > done)]
    chart = (sum(x["eff"] for x in same) / (sum(x["gen_ms"] for x in same) / 1000.0)) if same else 0.0
    cal = cals.get(c["id"])
    cal_desc = f"{cal['bpt_sample']:.0f}({cal['bpt_now']:.0f})" if cal else "-"
    gr = gauge / true_tps if true_tps > 3 and on else None
    chr_ = chart / true_tps if true_tps > 3 else None
    if gr: gauge_ratios.append(gr)
    if chr_: chart_ratios.append(chr_)
    print(f"  {f(done)}  {eff:>5}  {gen/1000:>4.1f} | {true_tps:>6.1f} | {gauge:>6.1f} ({cover:>3.0%}) | {chart:>6.1f} | {cal_desc:>14} | "
          f"{f'{gr:.2f}x' if gr else '-':>7}  {f'{chr_:.2f}x' if chr_ else '-':>7}")

if gauge_ratios:
    print(f"\n[汇总] 仪表盘/真实: 中位 {st.median(gauge_ratios):.2f}x  均偏 {st.mean(gauge_ratios):.2f}x  范围 {min(gauge_ratios):.2f}~{max(gauge_ratios):.2f}x")
if chart_ratios:
    print(f"       图表/真实:   中位 {st.median(chart_ratios):.2f}x  均偏 {st.mean(chart_ratios):.2f}x")
# 全窗
integ = sum(tk["live"]["tps"] * 0.5 for tk in ticks[1:] if tk["live"]["stream"])
true_sum = sum(c["eff"] for c in calls if t0 <= c["done"] <= t1)
if true_sum:
    print(f"       积分: 仪表 {integ:.0f} vs 真实 {true_sum} tok ({(integ-true_sum)/true_sum:+.1%})")
