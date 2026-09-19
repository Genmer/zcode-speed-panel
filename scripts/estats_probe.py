# -*- coding: utf-8 -*-
"""ESTATS 每连接字节计数复验脚本（key-rules #14 的翻案/坐实实验）。

背景：docs/key-rules.md #14 记载 2026-09-18 实验——Windows 唯一的每连接
字节公开 API `Set/GetPerTcpConnectionEStats` 返回 ERROR_NOT_SUPPORTED(50)，
但当年实验代码未留档：调的 ESTATS 类、结构长度、Set→Get 顺序、以及是否对
**他人进程的连接**（面板的真实场景——目标连接属于 zcode.exe）试过启用，
均不可考。本脚本把实验重新做严谨，输出判定矩阵。

测试矩阵（全部普通权限，面板不以管理员为前提）：
  1. 导出符号解析：4 个 EStats 函数（v4/v6 × Set/Get，注意导出名大写 S）；
  2. 自有 v4 连接：先 Get（验证默认是否采集）→ Set 启用 → 传输数据 → 再
     Get，验证计数随传输增长；
  3. 他人连接（zcode.exe 优先，任一非自身 ESTAB 作对照）：Get → Set 启用 →
     间隔采样两次，看计数是否增长（CLI 会话连接对流时应当可见）；
  4. v6 同族 API（任一 v6 ESTAB 行）。

判定标准：他人连接的 Get 返回 NO_ERROR 且字节计数随时间增长 → ESTATS 可用
（按进程真实速度可行）；否则坐实 key-rules #14。

用法：python scripts/estats_probe.py
"""

import ctypes
import os
import socket
import struct
import subprocess
import sys
import time

try:
    sys.stdout.reconfigure(encoding="utf-8")
except Exception:
    pass

iphlpapi = ctypes.WinDLL("iphlpapi")

TCP_TABLE_OWNER_PID_ALL = 5
AF_INET = 2
AF_INET6 = 23
MIB_TCP_STATE_ESTAB = 5
TCP_ESTATS_DATA = 0  # TcpConnectionEstatsType 枚举第 0 项 = Data（字节/分段计数）
ROD_LEN = 32  # TCP_ESTATS_DATA_ROD_v0 = 4 × ULONG64
RW_LEN = 1  # TCP_ESTATS_DATA_RW_v0 = 1 × BOOLEAN(EnableCollection)

ERR_NAMES = {
    0: "NO_ERROR",
    5: "ERROR_ACCESS_DENIED",
    13: "ERROR_INVALID_DATA",
    50: "ERROR_NOT_SUPPORTED",
    87: "ERROR_INVALID_PARAMETER",
    122: "ERROR_INSUFFICIENT_BUFFER",
    1168: "ERROR_NOT_FOUND",
}


def err_str(rc):
    return ERR_NAMES.get(rc, "code_%d" % rc)


# ============ EStats 函数指针（导出名大写 S，直接链接会 LNK2019——key-rules #108） ============

class MIB_TCPROW(ctypes.Structure):
    _fields_ = [
        ("state", ctypes.c_uint),
        ("local_addr", ctypes.c_uint),
        ("local_port", ctypes.c_uint),
        ("remote_addr", ctypes.c_uint),
        ("remote_port", ctypes.c_uint),
    ]


class MIB_TCP6ROW(ctypes.Structure):
    _fields_ = [
        ("local_addr", ctypes.c_ubyte * 16),
        ("local_scope", ctypes.c_uint),
        ("local_port", ctypes.c_uint),
        ("remote_addr", ctypes.c_ubyte * 16),
        ("remote_scope", ctypes.c_uint),
        ("remote_port", ctypes.c_uint),
        ("state", ctypes.c_uint),
    ]


def resolve_estats_fns():
    """返回 {名字: 函数或 None}。ctypes 的属性访问走 GetProcAddress，取不到抛 AttributeError"""
    names = [
        "SetPerTcpConnectionEStats",
        "GetPerTcpConnectionEStats",
        "SetPerTcp6ConnectionEStats",
        "GetPerTcp6ConnectionEStats",
    ]
    out = {}
    for n in names:
        try:
            fn = getattr(iphlpapi, n)
            fn.restype = ctypes.c_uint
            fn.argtypes = [
                ctypes.c_void_p,  # Row
                ctypes.c_uint,  # EstatsType
                ctypes.POINTER(ctypes.c_ubyte),  # Rw
                ctypes.c_uint,  # RwVersion
                ctypes.c_uint,  # RwSize
                ctypes.POINTER(ctypes.c_ubyte),  # Rod
                ctypes.c_uint,  # RodVersion
                ctypes.c_uint,  # RodSize
            ]
            out[n] = fn
        except AttributeError:
            out[n] = None
    return out


FNS = resolve_estats_fns()


# ============ 连接表枚举（GetExtendedTcpTable, OWNER_PID_ALL） ============

class MIB_TCPROW_OWNER_PID(ctypes.Structure):
    _fields_ = MIB_TCPROW._fields_ + [("pid", ctypes.c_uint)]


class MIB_TCP6ROW_OWNER_PID(ctypes.Structure):
    _fields_ = MIB_TCP6ROW._fields_ + [("pid", ctypes.c_uint)]


def ipv4_str(v):
    return "%d.%d.%d.%d" % (v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF, (v >> 24) & 0xFF)


def port_str(p):
    return "%d" % (((p & 0xFF) << 8) | ((p >> 8) & 0xFF))


def ipv6_str(b):
    return ":".join("%02x%02x" % (b[i * 2], b[i * 2 + 1]) for i in range(8))


def enum_v4():
    size = ctypes.c_uint(0)
    iphlpapi.GetExtendedTcpTable(None, ctypes.byref(size), 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0)
    buf = ctypes.create_string_buffer(size.value)
    rc = iphlpapi.GetExtendedTcpTable(buf, ctypes.byref(size), 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0)
    if rc != 0:
        print("GetExtendedTcpTable(v4) 失败: %s" % err_str(rc))
        return []
    n = struct.unpack_from("<I", buf, 0)[0]
    rows = (MIB_TCPROW_OWNER_PID * n).from_buffer_copy(buf, 4)
    return [
        {
            "row": MIB_TCPROW(
                state=r.state, local_addr=r.local_addr, local_port=r.local_port,
                remote_addr=r.remote_addr, remote_port=r.remote_port),
            "pid": r.pid,
            "remote": "%s:%s" % (ipv4_str(r.remote_addr), port_str(r.remote_port)),
            "local": "%s:%s" % (ipv4_str(r.local_addr), port_str(r.local_port)),
        }
        for r in rows
    ]


def enum_v6():
    size = ctypes.c_uint(0)
    iphlpapi.GetExtendedTcpTable(None, ctypes.byref(size), 0, AF_INET6, TCP_TABLE_OWNER_PID_ALL, 0)
    buf = ctypes.create_string_buffer(size.value)
    rc = iphlpapi.GetExtendedTcpTable(buf, ctypes.byref(size), 0, AF_INET6, TCP_TABLE_OWNER_PID_ALL, 0)
    if rc != 0:
        print("GetExtendedTcpTable(v6) 失败: %s" % err_str(rc))
        return []
    n = struct.unpack_from("<I", buf, 0)[0]
    rows = (MIB_TCP6ROW_OWNER_PID * n).from_buffer_copy(buf, 4)
    out = []
    for r in rows:
        # MIB_TCP6ROW 是 OWNER_PID 行的前缀，按字节截取构造
        row = MIB_TCP6ROW.from_buffer_copy(bytes(r)[: ctypes.sizeof(MIB_TCP6ROW)])
        out.append({
            "row": row,
            "pid": r.pid,
            "remote": "[%s]:%s" % (ipv6_str(r.remote_addr), port_str(r.remote_port)),
        })
    return out


# ============ ESTATS 读写 ============

def estats_get_v4(row):
    rod = (ctypes.c_ubyte * ROD_LEN)()
    rc = FNS["GetPerTcpConnectionEStats"](ctypes.byref(row), TCP_ESTATS_DATA,
                                          None, 0, 0, rod, 0, ROD_LEN)
    if rc != 0:
        return rc, None
    bytes_out, segs_out, bytes_in, segs_in = struct.unpack_from("<4Q", rod, 0)
    return rc, (bytes_out, segs_out, bytes_in, segs_in)


def estats_set_v4(row):
    rw = (ctypes.c_ubyte * RW_LEN)(1)  # EnableCollection = TRUE
    return FNS["SetPerTcpConnectionEStats"](ctypes.byref(row), TCP_ESTATS_DATA,
                                            rw, 0, RW_LEN, None, 0, 0)


def estats_get_v6(row):
    rod = (ctypes.c_ubyte * ROD_LEN)()
    rc = FNS["GetPerTcp6ConnectionEStats"](ctypes.byref(row), TCP_ESTATS_DATA,
                                           None, 0, 0, rod, 0, ROD_LEN)
    if rc != 0:
        return rc, None
    return rc, struct.unpack_from("<4Q", rod, 0)


def estats_set_v6(row):
    rw = (ctypes.c_ubyte * RW_LEN)(1)
    return FNS["SetPerTcp6ConnectionEStats"](ctypes.byref(row), TCP_ESTATS_DATA,
                                             rw, 0, RW_LEN, None, 0, 0)


def fmt_counters(c):
    if c is None:
        return "-"
    return "out=%dB/%dseg in=%dB/%dseg" % c


# ============ 测试用例 ============

def find_zcode_pids():
    try:
        out = subprocess.run(
            ["tasklist", "/FI", "IMAGENAME eq zcode.exe", "/FO", "CSV", "/NH"],
            capture_output=True, text=True, timeout=10).stdout
    except Exception as e:
        print("tasklist 失败: %s" % e)
        return set()
    pids = set()
    for line in out.splitlines():
        parts = [p.strip('"') for p in line.split('","')]
        if len(parts) >= 2 and parts[0].lower() == "zcode.exe" and parts[1].isdigit():
            pids.add(int(parts[1]))
    return pids


HTTP_HOSTS = ["www.baidu.com", "www.qq.com", "mirrors.aliyun.com"]


def test_own_v4():
    """自有连接：Get(未启用) → Set 启用 → 传输 → Get，验证计数增长"""
    print("\n== 测试 2：自有 v4 连接 ==")
    sock = None
    host = None
    for h in HTTP_HOSTS:
        try:
            sock = socket.create_connection((h, 80), timeout=10)
            host = h
            break
        except OSError:
            continue
    if sock is None:
        print("跳过：无可用 HTTP 端点")
        return None
    try:
        local_ip, local_port = sock.getsockname()
        want_addr = int.from_bytes(socket.inet_aton(local_ip), "little")
        want_port = ((local_port & 0xFF) << 8) | ((local_port >> 8) & 0xFF)
        row = None
        for r in enum_v4():
            if r["pid"] == os.getpid() and r["row"].local_addr == want_addr and r["row"].local_port == want_port:
                row = r["row"]
                break
        if row is None:
            print("连接表中找不到自己的行（%s:%d）" % (local_ip, local_port))
            return None

        rc1, c1 = estats_get_v4(row)
        print("  Get（未启用，验证默认采集）: %s  %s" % (err_str(rc1), fmt_counters(c1)))
        rc2 = estats_set_v4(row)
        print("  Set（启用 Data 采集）      : %s" % err_str(rc2))
        # 调用姿势变体：排除参数形态导致的误判
        rwq = (ctypes.c_ubyte * RW_LEN)()
        rodq = (ctypes.c_ubyte * ROD_LEN)()
        rcA = FNS["GetPerTcpConnectionEStats"](ctypes.byref(row), TCP_ESTATS_DATA,
                                               rwq, 0, RW_LEN, rodq, 0, ROD_LEN)
        print("  变体 A：Get 附带 Rw 缓冲   : %s  %s" % (err_str(rcA), fmt_counters(
            struct.unpack_from("<4Q", rodq, 0) if rcA == 0 else None)))
        rod_set = (ctypes.c_ubyte * ROD_LEN)()
        rcB = FNS["SetPerTcpConnectionEStats"](ctypes.byref(row), TCP_ESTATS_DATA,
                                               (ctypes.c_ubyte * RW_LEN)(1), 0, RW_LEN,
                                               rod_set, 0, ROD_LEN)
        print("  变体 B：Set 附带 Rod 缓冲  : %s" % err_str(rcB))
        rc3, c3 = estats_get_v4(row)
        print("  Get（启用后）              : %s  %s" % (err_str(rc3), fmt_counters(c3)))

        total = 0
        req = ("GET / HTTP/1.1\r\nHost: %s\r\nUser-Agent: estats-probe\r\n"
               "Connection: close\r\n\r\n" % host).encode()
        sock.sendall(req)
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            total += len(chunk)
        print("  传输完成：HTTP 下载 %d 字节" % total)

        rc4, c4 = estats_get_v4(row)
        print("  Get（传输后）              : %s  %s" % (err_str(rc4), fmt_counters(c4)))
        if c4 is not None and c3 is not None:
            print("  增量：in +%dB / out +%dB" % (c4[2] - c3[2], c4[0] - c3[0]))
        ok = rc3 == 0 and c3 is not None and rc4 == 0 and c4 is not None and (c4[2] > c3[2] or c4[0] > c3[0])
        print("  => 自有连接判定：%s" % ("可用（计数随传输增长）" if ok else "不可用"))
        return ok
    finally:
        sock.close()


def probe_foreign_row_v4(tag, row, remote, sample_s=4):
    """他人连接三连：Get → Set → 间隔两次采样看增长"""
    rc1, c1 = estats_get_v4(row)
    print("  [%s] %s" % (tag, remote))
    print("    Get（未启用）: %s  %s" % (err_str(rc1), fmt_counters(c1)))
    rc2 = estats_set_v4(row)
    print("    Set（启用）  : %s" % err_str(rc2))
    rc3, c3 = estats_get_v4(row)
    print("    Get（t0）    : %s  %s" % (err_str(rc3), fmt_counters(c3)))
    time.sleep(sample_s)
    rc4, c4 = estats_get_v4(row)
    print("    Get（t+%ds） : %s  %s" % (sample_s, err_str(rc4), fmt_counters(c4)))
    grew = c3 is not None and c4 is not None and (c4[2] > c3[2] or c4[0] > c3[0])
    if grew:
        print("    增量：in +%dB / out +%dB  ** 计数在增长 **" % (c4[2] - c3[2], c4[0] - c3[0]))
    return {"get": rc3, "set": rc2, "grew": grew, "remote": remote}


def test_foreign_v4():
    print("\n== 测试 3：他人连接（面板真实场景）==")
    estab = [r for r in enum_v4() if r["pid"] != os.getpid() and r["row"].state == MIB_TCP_STATE_ESTAB]
    if not estab:
        print("跳过：无 ESTABLISHED 的他人连接")
        return None
    zcode = find_zcode_pids()
    print("zcode.exe pid 数：%d" % len(zcode))
    zrows = [r for r in estab if r["pid"] in zcode][:5]
    results = []
    for r in zrows:
        results.append(probe_foreign_row_v4("zcode pid=%d" % r["pid"], r["row"], r["remote"]))
    other = next((r for r in estab if r["pid"] not in zcode), None)
    if other is not None:
        results.append(probe_foreign_row_v4("对照 pid=%d" % other["pid"], other["row"], other["remote"]))
    return results


def test_v6():
    print("\n== 测试 4：v6 同族 API ==")
    if FNS["SetPerTcp6ConnectionEStats"] is None or FNS["GetPerTcp6ConnectionEStats"] is None:
        print("跳过：v6 导出符号缺失")
        return None
    estab = [r for r in enum_v6() if r["row"].state == MIB_TCP_STATE_ESTAB]
    if not estab:
        print("跳过：无 v6 ESTABLISHED 连接")
        return None
    r = estab[0]
    print("  [%s] pid=%d" % (r["remote"], r["pid"]))
    rc1 = estats_set_v6(r["row"])
    print("    Set（启用）  : %s" % err_str(rc1))
    rc2, c2 = estats_get_v6(r["row"])
    print("    Get          : %s  %s" % (err_str(rc2), fmt_counters(c2)))
    return {"set": rc1, "get": rc2}


def main():
    win = sys.getwindowsversion()
    print("ESTATS 复验脚本（key-rules #14）")
    print("Windows: %s (build %d)  进程 pid=%d  管理员=%s" % (
        win.platform_version if hasattr(win, "platform_version") else str(win), win.build,
        os.getpid(), bool(ctypes.windll.shell32.IsUserAnAdmin())))

    print("\n== 测试 1：导出符号解析 ==")
    for n, fn in FNS.items():
        print("  %-28s %s" % (n, "已解析" if fn else "缺失!"))

    own_ok = test_own_v4()
    foreign = test_foreign_v4()
    test_v6()

    print("\n== 判定汇总 ==")
    foreign_ok = None
    if foreign:
        for r in foreign:
            print("  他人连接 %s：Set=%s Get=%s 增长=%s" % (
                r["remote"], err_str(r["set"]), err_str(r["get"]), "是" if r["grew"] else "否"))
        foreign_ok = any(r["set"] == 0 and r["get"] == 0 for r in foreign)
        grew = any(r["grew"] for r in foreign)
        print("  自有连接：%s" % ("可用" if own_ok else "不可用"))
        verdict = "可行：按进程真实速度可以实现" if (foreign_ok and grew) else (
            "部分可行（他人连接可读写但未见增长，需结合流量复核）" if foreign_ok else "不可行：坐实 key-rules #14")
        print("  最终判定：%s" % verdict)


if __name__ == "__main__":
    main()
