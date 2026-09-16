#!/usr/bin/env python3
"""实时 t/s vs 调用真值 对账分析。

数据源：~/.zcode/speed-panel-debug.jsonl（面板调试日志）
  - kind=cal  ：每次调用完成后的校准/对账事件（v2 起含 true_tps / pred_tps / clean_kb）
  - kind=tick ：面板实际显示值（tps / pipe / src）
  - kind=call ：调用真值（eff / gen_ms / true_tps）

用法：
  python scripts/live_vs_true.py                 # 分析默认日志
  python scripts/live_vs_true.py <日志路径>       # 分析指定日志
  python scripts/live_vs_true.py --ticks         # 追加：tick 级显示值分布

评估口径：
  pred_tps = 清洗流在 [first_token, completed] 的积分字节 ÷ 生成长(s) ÷ 当前 bpt
  —— 即"该调用期间显示口径的平均 t/s 预测"。pred/true 越接近 1 越准。
旧格式日志（cal 无 pred_tps 字段）自动退化为 tick 回放法：取调用区间内
io 来源 tick 显示值的均值与真值对比。
"""
from __future__ import annotations

import json
import statistics
import sys
from pathlib import Path

DEFAULT_LOG = Path.home() / ".zcode" / "speed-panel-debug.jsonl"


def load(path: Path):
    calls, ticks, cals = [], [], []
    with open(path, encoding="utf-8") as f:
        for line in f:
            try:
                d = json.loads(line)
            except (json.JSONDecodeError, UnicodeDecodeError):
                continue
            kind = d.get("kind")
            if kind == "call":
                calls.append(d)
            elif kind == "tick":
                ticks.append(d)
            elif kind == "cal":
                cals.append(d)
    ticks.sort(key=lambda x: x["t"])
    return calls, ticks, cals


def recon_new(cals, calls):
    """v2 口径：cal 事件自带积分对账字段"""
    by_id = {c["id"]: c for c in calls}
    rows = []
    for cal in cals:
        if cal.get("pred_tps") is None:
            continue
        c = by_id.get(cal["id"], {})
        rows.append({
            "done": cal.get("t", 0),
            "gen_s": cal.get("gen_ms", c.get("gen_ms", 0)) / 1000,
            "eff": cal.get("eff", c.get("eff", 0)),
            "true": cal.get("true_tps", c.get("true_tps", 0.0)),
            "pred": cal["pred_tps"],
            "bpt_sample": cal.get("bpt_sample", 0.0),
            "bpt_now": cal.get("bpt_now", 0.0),
            "clean_kb": cal.get("clean_kb", 0.0),
            "skipped": cal.get("skipped", True),
        })
    return rows


def recon_legacy(calls, ticks):
    """旧口径：调用区间内 io 来源 tick 显示值均值 vs 真值"""
    rows = []
    for c in calls:
        t0 = c["done"] - c["gen_ms"]
        t1 = c["done"]
        win = [tk for tk in ticks
               if t0 + 1000 <= tk["t"] <= t1 - 500
               and tk.get("tps", 0) > 0 and tk.get("src") == "io"]
        if len(win) < 3:
            continue
        avg = statistics.mean(tk["tps"] for tk in win)
        rows.append({
            "done": t1,
            "gen_s": c["gen_ms"] / 1000,
            "eff": c["eff"],
            "true": c["true_tps"],
            "pred": avg,
            "bpt_sample": 0.0,
            "bpt_now": win[-1].get("bpt", 0.0),
            "clean_kb": 0.0,
            "skipped": False,
        })
    return rows


def main():
    path = Path(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else DEFAULT_LOG
    show_ticks = "--ticks" in sys.argv
    if not path.exists():
        print(f"日志不存在: {path}")
        sys.exit(1)

    calls, ticks, cals = load(path)
    rows = recon_new(cals, calls)
    mode = "v2 积分对账"
    if not rows:
        rows = recon_legacy(calls, ticks)
        mode = "旧版 tick 回放（建议升级面板后重新采样）"

    qualified = [r for r in rows if r["eff"] >= 300 and not r["skipped"] and r["true"] > 0]
    print(f"日志: {path}")
    print(f"口径: {mode} | 调用 {len(calls)} 次，对账样本 {len(rows)}，达标(≥300 tok 且入校准) {len(qualified)}\n")
    if not qualified:
        print("暂无达标对账样本（需 ≥300 token 的调用完成后生成）。")
        return

    print(f"{'完成时刻':>9} {'gen_s':>6} {'eff':>6} {'true':>7} {'pred':>7} {'ratio':>6} {'bpt样本':>8} {'bpt生效':>8}")
    ratios = []
    for r in qualified[-40:]:
        ratio = r["pred"] / r["true"] if r["true"] else 0.0
        ratios.append(ratio)
        t = r["done"] / 1000 % 86400
        hh, rem = divmod(int(t), 3600)
        mm, ss = divmod(rem, 60)
        print(f"{hh:02d}:{mm:02d}:{ss:02d}   {r['gen_s']:6.1f} {r['eff']:6d} "
              f"{r['true']:7.1f} {r['pred']:7.1f} {ratio:6.2f} {r['bpt_sample']:8.0f} {r['bpt_now']:8.0f}")

    med = statistics.median(ratios)
    within = sum(1 for x in ratios if 0.8 <= x <= 1.25) / len(ratios)
    print(f"\npred/true 中位数 = {med:.2f}（1.00 为准） | ±20% 内占比 = {within:.0%}")
    if med < 0.8:
        print("→ 实时读数仍系统性偏低：把本表连同 tick 日志反馈")
    elif med > 1.25:
        print("→ 实时读数系统性偏高：系数样本可能被异常调用污染")
    else:
        print("→ 实时读数与真值一致 ✓")

    if show_ticks:
        io_ticks = [t for t in ticks if t.get("src") == "io" and t.get("stream")]
        if io_ticks:
            src_count = {}
            for t in ticks:
                s = t.get("src")
                src_count[s] = src_count.get(s, 0) + 1
            print(f"\ntick 来源分布: {src_count}")
            tps = [t["tps"] for t in io_ticks if t.get("tps")]
            print(f"io 流式 tick: n={len(tps)} 中位 {statistics.median(tps):.1f} t/s "
                  f"p90 {sorted(tps)[int(len(tps)*0.9)]:.1f} 最大 {max(tps):.1f}")
            pipes = [t.get("pipe", 0) for t in io_ticks]
            if any(pipes):
                print(f"清洗管道字节率: 中位 {statistics.median(pipes):.0f} B/s "
                      f"最大 {max(pipes):.0f} B/s")


if __name__ == "__main__":
    main()
