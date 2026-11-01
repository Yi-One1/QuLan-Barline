#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""M2 性能实测：内存 / CPU（静止态 + 悬停态）。"""

import ctypes
import os
import subprocess
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = r"E:\APP\WorkBuddy\工作空间\曲栏Barline"
EXE = os.path.join(ROOT, "target", "release", "barline.exe")

user32 = ctypes.WinDLL("user32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)


class PROCESS_MEMORY_COUNTERS_EX(ctypes.Structure):
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
        return None
    c = PROCESS_MEMORY_COUNTERS_EX()
    c.cb = ctypes.sizeof(c)
    psapi.GetProcessMemoryInfo(h, ctypes.byref(c), ctypes.sizeof(c))
    kernel32.CloseHandle(h)
    return c.WorkingSetSize, c.PrivateUsage


def cpu_times(pid):
    h = kernel32.OpenProcess(0x1000 | 0x0400, False, pid)
    if not h:
        return None
    c = wintypes.FILETIME()
    e = wintypes.FILETIME()
    k = wintypes.FILETIME()
    u = wintypes.FILETIME()
    kernel32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(k), ctypes.byref(u))
    kernel32.CloseHandle(h)

    def f(t):
        return (t.dwHighDateTime << 32) | t.dwLowDateTime

    return f(k) + f(u)


def sample(pid, seconds, label):
    t0 = cpu_times(pid)
    w0 = time.time()
    time.sleep(seconds)
    t1 = cpu_times(pid)
    w1 = time.time()
    m = mem(pid)
    cpu = (t1 - t0) / 1e7  # 100ns -> s
    wall = w1 - w0
    print(
        "%-14s  CPU %.3fs / %.1fs = %.2f%%   工作集 %.1f MB   私有 %.1f MB"
        % (label, cpu, wall, cpu / wall * 100.0, m[0] / 1048576, m[1] / 1048576)
    )
    return cpu / wall * 100.0


def main():
    edge = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
    if not os.path.exists(edge):
        edge = r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"

    # 复用验收脚本里的辅助（起会话 / 找窗口）
    spec = __import__("importlib.util").util.spec_from_file_location("acc", os.path.join(HERE, "m2_accept.py"))
    acc = __import__("importlib.util").util.module_from_spec(spec)
    sys.modules["acc"] = acc
    spec.loader.exec_module(acc)

    acc.kill_isolated_edge()
    time.sleep(0.8)
    srv = subprocess.Popen(
        [sys.executable, "-m", "http.server", "8766", "--bind", "127.0.0.1"],
        cwd=HERE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(1.0)
    profile = os.path.join(HERE, "barline_edge_profile")
    edge_p = subprocess.Popen(
        [
            edge,
            "--user-data-dir=" + profile,
            "--autoplay-policy=no-user-gesture-required",
            "--no-first-run",
            "--no-default-browser-check",
            "--window-position=120,100",
            "--window-size=900,600",
            "http://127.0.0.1:8766/barline_demo.html",
        ]
    )
    time.sleep(9)
    acc.minimize_window_with_title("Barline 验收音轨")

    app = subprocess.Popen([EXE])
    wr = acc.wait_stable_rect(20)
    print("小组件窗口 = %s" % (wr,))
    time.sleep(3)

    tr = acc.tray_rect()
    bar_h = tr[3] - tr[1]
    wl, wt, wrr, wb = wr
    ww, wh = wrr - wl, wb - wt
    main_top = wh - bar_h

    acc.move(1200, 700)
    time.sleep(1.5)
    sample(app.pid, 60, "静止态 60s")

    acc.move(wl + ww // 2, wt + main_top + 24)
    time.sleep(1.5)
    sample(app.pid, 30, "悬停态 30s")

    acc.move(1200, 700)
    time.sleep(2)
    m = mem(app.pid)
    print("峰值（工作集/私有）= %.1f / %.1f MB" % (m[0] / 1048576, m[1] / 1048576))

    app.kill()
    time.sleep(0.6)
    edge_p.kill()
    acc.kill_isolated_edge()
    srv.kill()
    print("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
