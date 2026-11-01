#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""M3 性能实测：播放中（逐字扫光 25fps 重绘）vs 暂停（应回落）。

用法：python scripts\\m3-perf.py
"""

import importlib.util
import json
import os
import subprocess
import sys
import time
from ctypes import wintypes
import ctypes

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = r"E:\APP\WorkBuddy\工作空间\曲栏Barline"
EXE = os.path.join(ROOT, "target", "release", "barline.exe")
LRC_DIR = r"C:\Users\caihongyi\AppData\Local\Barline\lyrics"

spec = importlib.util.spec_from_file_location("acc", os.path.join(HERE, "m2-accept.py"))
acc = importlib.util.module_from_spec(spec)
sys.modules["acc"] = acc
spec.loader.exec_module(acc)

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)


class PMC(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


def mem(pid):
    h = kernel32.OpenProcess(0x1000 | 0x0400, False, pid)
    if not h:
        return (0, 0)
    c = PMC()
    c.cb = ctypes.sizeof(c)
    psapi.GetProcessMemoryInfo(h, ctypes.byref(c), ctypes.sizeof(c))
    kernel32.CloseHandle(h)
    return (c.WorkingSetSize, c.PrivateUsage)


def cpu(pid):
    h = kernel32.OpenProcess(0x1000 | 0x0400, False, pid)
    if not h:
        return 0
    c, e, k, u = (wintypes.FILETIME() for _ in range(4))
    kernel32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(k), ctypes.byref(u))
    kernel32.CloseHandle(h)
    f = lambda t: (t.dwHighDateTime << 32) | t.dwLowDateTime
    return f(k) + f(u)


def sample(pid, seconds, label):
    t0, w0 = cpu(pid), time.time()
    time.sleep(seconds)
    t1, w1 = cpu(pid), time.time()
    m = mem(pid)
    pct = (t1 - t0) / 1e7 / (w1 - w0) * 100.0
    print(
        "%-22s CPU %.3fs/%.0fs = %5.2f%%   工作集 %5.1f MB  私有 %5.1f MB"
        % (label, (t1 - t0) / 1e7, w1 - w0, pct, m[0] / 1048576, m[1] / 1048576)
    )
    return pct


LRC = """[ti:夜空独行]
[ar:测试歌手]
[00:00.50]<00:00.50>夜<00:00.90>空<00:01.30>独<00:01.70>行
[00:04.00]<00:04.00>脚<00:04.40>步<00:04.80>很<00:05.20>轻
[00:07.00]<00:07.00>路<00:07.40>灯<00:07.80>把<00:08.20>影<00:08.60>子<00:09.00>拉<00:09.40>长
[00:10.00]只有行级时间戳的一句
[00:13.00]<00:13.00>Hello <00:13.70>world <00:14.40>混<00:14.80>排
[00:16.50]扫光要平滑推进
[00:19.00]<00:19.00>风<00:19.40>从<00:19.80>窗<00:20.20>口<00:20.60>吹<00:21.00>进
[00:22.00]切行时高度不变
[00:25.00]<00:25.00>时<00:25.40>间<00:25.80>在<00:26.20>这<00:26.60>里
[00:28.50]下一行在这里
[00:31.00]<00:31.00>我<00:31.40>们<00:31.80>都<00:32.20>在<00:32.60>路<00:33.00>上
[00:34.00]最后一段歌词
[00:37.00]<00:37.00>曲<00:37.40>终<00:37.80>人<00:38.20>散
"""


def main():
    os.makedirs(LRC_DIR, exist_ok=True)
    lrc = os.path.join(LRC_DIR, "测试歌手 - 夜空独行.lrc")
    with open(lrc, "w", encoding="utf-8") as f:
        f.write(LRC)
    page = os.path.join(HERE, "m3_demo_a.html")
    if not os.path.exists(page):
        print("缺 m3_demo_a.html，先跑一次 m3-accept.py")
        return 1

    acc.kill_isolated_edge()
    time.sleep(0.8)
    srv = subprocess.Popen(
        [sys.executable, "-m", "http.server", "8768", "--bind", "127.0.0.1"],
        cwd=HERE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(1.0)
    edge = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
    if not os.path.exists(edge):
        edge = r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"
    ep = subprocess.Popen(
        [
            edge,
            "--user-data-dir=" + os.path.join(HERE, "barline_edge_profile"),
            "--autoplay-policy=no-user-gesture-required",
            "--no-first-run",
            "--no-default-browser-check",
            "--window-position=120,100",
            "--window-size=900,600",
            "http://127.0.0.1:8768/m3_demo_a.html",
        ]
    )
    time.sleep(9)
    acc.minimize_window_with_title("M3 验收音轨 A")

    app = subprocess.Popen([EXE])
    wr = acc.wait_stable_rect(20)
    print("窗口 = %s" % (wr,))
    time.sleep(3)
    tr = acc.tray_rect()
    wl, wt, wrr, wb = wr
    ww, wh = wrr - wl, wb - wt
    main_top = wh - (tr[3] - tr[1])

    acc.move(1200, 700)
    time.sleep(1.0)
    sample(app.pid, 60, "播放中（歌词滚动）")
    # 点暂停 → 扫光应停住、定时器应停
    acc.click(wl + ww - 56, wt + main_top + 24)
    # 指针必须移开：悬停会启动 250ms 的模糊刷新定时器（那是 M2 的「悬停态」成本）
    acc.move(1200, 700)
    time.sleep(2.5)
    sample(app.pid, 30, "暂停（歌词静止）")

    app.kill()
    time.sleep(0.6)
    ep.kill()
    acc.kill_isolated_edge()
    srv.kill()
    try:
        os.remove(lrc)
    except OSError:
        pass
    print("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
