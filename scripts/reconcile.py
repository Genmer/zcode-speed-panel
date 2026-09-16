# -*- coding: utf-8 -*-
"""事后对账：verify-log.jsonl（面板同款引擎的实时显示 + 原始每进程写字节）
× usage DB（对话结束后落盘的真实 token），评估：
  1) 实时速度显示的准确度（逐调用 + 全窗积分）
  2) 字节→token 自校准系数受多进程噪声污染的程度
  3) 总量链路：面板最终值 vs 落盘重算
"""
import json, sqlite3, sys, datetime, zoneinfo
from pathlib import Path

TZ = zoneinfo.ZoneInfo("Asia/Shanghai")
LOG = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("src-tauri/target/verify-log.jsonl")
DB = Path.home() / ".zcode/cli/db/db.sqlite"
MY_SESS = sys.argv[2] if len(sys.argv) > 2 else None

ticks = [json.loads(l) for l in LOG.read_text(encoding="utf-8").splitlines() if l.strip()]
t0, t1 = ticks[0]["t"], ticks[-1]["t"]
dur_s = (t1 - t0) / 1000
print(f"采样窗: {datetime.datetime.fromtimestamp(t0/1000, TZ):%H:%M:%S} → "
      f"{datetime.datetime.fromtimestamp(t1/1000, TZ):%H:%M:%S}  ({dur_s:.0f}s, {len(ticks)} 拍)")

# ---- 原始序列 ----
raw_pids = {}   # pid -> [(t, bytes)]
files = []      # [(t, total)]
for tk in ticks:
    files.append((tk["t"], tk["files"]))
    for pid, b in tk["raw"].items():
        raw_pids.setdefault(int(pid), []).append((tk["t"], b))
print(f"观测到 CLI 进程数: {len(raw_pids)}")

def series_delta(series, w0, w1):
    """与面板相同的窗口重叠规则: ct>=w0 且 pt<=w1 的增量求和"""
    acc = 0.0
    for (pt, pv), (ct, cv) in zip(series, series[1:]):
        if ct >= w0 and pt <= w1:
            acc += max(0, cv - pv)
    return acc

# ---- 落盘的真实调用（窗口内完成）----
con = sqlite3.connect(f"file:{DB.as_posix()}?mode=ro", uri=True)
cur = con.cursor()
cur.execute("""SELECT id, session_id, completed_at,
               CASE WHEN first_token_at IS NOT NULL AND completed_at > first_token_at
                    THEN completed_at - first_token_at ELSE duration_ms END,
               output_tokens, reasoning_tokens, input_tokens
               FROM model_usage WHERE status='completed' AND completed_at > ? AND completed_at <= ?
               ORDER BY completed_at""", (t0 - 1000, t1 + 1000))
calls = cur.fetchall()
print(f"窗内落盘调用: {len(calls)} 次, 真实生成 token(out+rea) 合计 {sum(c[4]+c[5] for c in calls)}")

# ---- 逐调用对账 ----
print("\n[逐调用] 完成时刻  会话(尾4)  out+rea   gen_s  真实tps | 面板bpt  干净bpt  污染占比 | 显示均tps(覆盖率)")
rows = []
for cid, sess, comp, gen, out, rea, inp in calls:
    eff = out + rea
    gen_s = max(gen, 50) / 1000
    true_tps = eff / gen_s
    w0 = comp - min(gen, 300_000)
    w1 = comp
    per_pid = {p: series_delta(s, w0, w1) for p, s in raw_pids.items()}
    total_b = sum(per_pid.values())
    file_b = series_delta(files, w0, w1)
    top_pid, top_b = max(per_pid.items(), key=lambda kv: kv[1]) if per_pid else (None, 0.0)
    # 面板口径 bpt（全进程求和 − 落盘）
    panel_bpt = (total_b - file_b) / eff if eff and total_b > file_b else None
    if panel_bpt is not None:
        panel_bpt = min(max(panel_bpt, 400), 8000)
    # 干净口径 bpt（只算最大字节进程 − 落盘）
    clean_bpt = (top_b - file_b) / eff if eff and top_b > file_b else None
    others_b = total_b - top_b
    poll = others_b / total_b if total_b else 0
    # 该调用流式区间内面板显示的速度均值与覆盖率
    disp = [(tk["t"], tk["live"]["tps"]) for tk in ticks
            if w0 <= tk["t"] <= w1 and tk["live"]["stream"]]
    cov = len(disp) / max(1, sum(1 for tk in ticks if w0 <= tk["t"] <= w1))
    disp_mean = sum(v for _, v in disp) / len(disp) if disp else 0.0
    rows.append((cid, sess[-4:], eff, gen_s, true_tps, panel_bpt, clean_bpt, poll, disp_mean, cov))
    print(f"  {datetime.datetime.fromtimestamp(comp/1000, TZ):%H:%M:%S}  {sess[-4:]}"
          f"  {eff:>7}  {gen_s:>6.1f}  {true_tps:>7.1f} | "
          f"{panel_bpt if panel_bpt is None else round(panel_bpt)}  "
          f"{clean_bpt if clean_bpt is None else round(clean_bpt)}  {poll:>5.0%} | "
          f"{disp_mean:>7.1f} ({cov:.0%})")

import statistics as st
def med(xs):
    xs = [x for x in xs if x is not None]
    return st.median(xs) if xs else float("nan")

print(f"\n[校准系数] 面板bpt中位数={med([r[5] for r in rows]):.0f}  "
      f"干净bpt中位数={med([r[6] for r in rows]):.0f}  (默认1600, 钳位[400,8000])")
print(f"[串扰] 非归属进程字节占比中位数={med([r[7] for r in rows]):.0%}")

# ---- 全窗积分：显示速度 × 时间 vs 落盘真实 ----
integ = sum(tk["live"]["tps"] * 0.5 for tk in ticks[1:] if tk["live"]["stream"])
true_sum = sum(r[2] for r in rows)
print(f"\n[积分对账] 显示速度积分≈{integ:.0f} tok  vs 窗内落盘真实 {true_sum} tok  "
      f"偏差 {(integ-true_sum)/true_sum if true_sum else 0:+.1%}")
mine = [r for r in rows if MY_SESS and r[1] == MY_SESS[-4:]]
if mine:
    print(f"  仅本会话({MY_SESS[-4:]}): {len(mine)} 次 {sum(r[2] for r in mine)} tok")

# ---- 总量链路：最后一拍面板值 vs 此刻落盘重算 ----
last = ticks[-1]["snap"]
cur.execute("""SELECT COALESCE(SUM(output_tokens),0), COALESCE(SUM(reasoning_tokens),0),
               COALESCE(SUM(input_tokens),0), COALESCE(SUM(cache_creation_input_tokens),0),
               COALESCE(SUM(cache_read_input_tokens),0), COUNT(*) FROM model_usage
               WHERE status='completed' AND completed_at >= (
                 SELECT MIN(completed_at) FROM model_usage WHERE completed_at >= ?
                 )""", (t0 - 86_400_000,))
# 用与面板一致的"今日零点"口径
import time
local_mid = int(datetime.datetime.combine(datetime.datetime.fromtimestamp(t1/1000, TZ).date(),
                                          datetime.time.min, tzinfo=TZ).timestamp() * 1000)
cur.execute("""SELECT COALESCE(SUM(output_tokens),0), COALESCE(SUM(reasoning_tokens),0),
               COALESCE(SUM(input_tokens),0), COALESCE(SUM(cache_creation_input_tokens),0),
               COALESCE(SUM(cache_read_input_tokens),0), COUNT(*) FROM model_usage
               WHERE status='completed' AND completed_at >= ? AND completed_at <= ?""", (local_mid, t1))
o, r, i, cc, cr, n = cur.fetchone()
panel_total = last["out"] + last["rea"] + last["inp"] + last["cc"]
disk_total = o + r + i + cc
print(f"\n[总量链路] 采样末拍(引擎聚合) = {panel_total}  vs 落盘重算(同口径SQL) = {disk_total}  "
      f"差 {panel_total - disk_total}  ({(panel_total-disk_total)/disk_total:+.4%})")
print(f"  明细: 引擎 out={last['out']} rea={last['rea']} inp={last['inp']} calls={last['calls']}"
      f"  | 磁盘 out={o} rea={r} inp={i} calls={n}")
cur.execute("""SELECT SUM(computed_total_tokens) FROM model_usage
               WHERE status='completed' AND completed_at >= ? AND completed_at <= ?""", (local_mid, t1))
print(f"  官方 computed_total(不含思考) = {cur.fetchone()[0]}  面板多计的思考 = {last['rea']}")
con.close()
