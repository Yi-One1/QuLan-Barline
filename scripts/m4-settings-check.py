#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""M4 设置页验收：打开 → 六组截图 → 语言切换 → 明暗主题 → 关闭后内存回落。

用法：python scripts\\m4-settings-check.py
"""

import ctypes
import importlib.util
import os
import subprocess
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = r"E:\APP\WorkBuddy\工作空间\曲栏Barline"
EXE = os.path.join(ROOT, "target", "release", "barline.exe")
SHOTS = os.path.join(ROOT, "docs", "screenshots")

spec = importlib.util.spec_from_file_location("acc", os.path.join(HERE, "m2-accept.py"))
acc = importlib.util.module_from_spec(spec)
sys.modules["acc"] = acc
spec.loader.exec_module(acc)

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)

WM_BARLINE_TRAY = 0x8000 + 2
WM_RBUTTONUP = 0x0205
SETTINGS_CLASS = "BarlineSettingsWnd"


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


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class _KU(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("pad", ctypes.c_byte * 32)]


class KINPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _KU)]


def _key(vk):
    """按一下虚拟键（按下 + 抬起）。"""
    for up in (False, True):
        inp = KINPUT()
        inp.type = 1
        inp.u.ki = KEYBDINPUT(vk, 0, 0x0002 if up else 0, 0, None)
        user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(KINPUT))
        time.sleep(0.05)


def win_rect(hwnd):
    r = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return (r.left, r.top, r.right, r.bottom)


def main():
    acc.kill_isolated_edge()
    time.sleep(0.5)
    mark = len(acc.log_text())
    app = subprocess.Popen([EXE])
    time.sleep(2.5)

    tray = user32.FindWindowW("BarlineTrayWnd", None)
    print("托盘窗口 = %s" % (tray,))
    if not tray:
        print("!! 托盘没起来")
        app.kill()
        return 1

    # 打开设置页：再启动一个实例（= 用户双击 exe，走单实例唤起路径）
    CURSOR = (2000, 1299)
    second = subprocess.Popen([EXE])
    second.wait(timeout=5)
    print("已启动第二个实例（应在 1s 内退出并唤起设置页），等 WebView2 初始化…")

    # 等设置窗口 + WebView2 就绪
    hwnd = 0
    t0 = time.time()
    while time.time() - t0 < 25:
        hwnd = user32.FindWindowW(SETTINGS_CLASS, None)
        if hwnd and acc.lines_since(mark, "WebView2 就绪"):
            break
        time.sleep(0.4)
    time.sleep(2.0)
    print("设置窗口 = %s" % (hwnd,))
    for ln in acc.lines_since(mark, "设置", "WebView2"):
        print("   " + ln)
    if not hwnd:
        app.kill()
        return 1

    # 用客户区截图（不含标题栏与边框），窗口矩形包含非客户区会切掉左侧
    cr = wintypes.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(cr))
    pt = wintypes.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(pt))
    r = (pt.x, pt.y, pt.x + cr.right, pt.y + cr.bottom)
    print("客户区 = %s" % (r,))
    w, h = cr.right, cr.bottom

    # 六个分组：点左侧导航并截图
    nav_y = r[1] + 82  # 第一项中心（brand 之后）
    step = 39
    names = ["appearance", "lyrics", "volume", "hotkeys", "general", "about"]
    for i, name in enumerate(names):
        acc.click(r[0] + 120, nav_y + i * step)
        time.sleep(0.6)
        acc.shot("m4-1%d-%s.bmp" % (i, name), r[0], r[1], w, h)

    # ---------------- 深色主题 ----------------
    print("=== 切到深色主题 ===")
    import winreg

    key_path = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key_path) as k:
        orig = winreg.QueryValueEx(k, "AppsUseLightTheme")[0]
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key_path, 0, winreg.KEY_SET_VALUE) as k:
        winreg.SetValueEx(k, "AppsUseLightTheme", 0, winreg.REG_DWORD, 0)
        winreg.SetValueEx(k, "SystemUsesLightTheme", 0, winreg.REG_DWORD, 0)
    acc.broadcast_setting_change()
    time.sleep(2.5)
    # 回「歌词」页截图（深色）
    acc.click(r[0] + 120, nav_y + 1 * step)
    time.sleep(0.8)
    acc.shot("m4-16-dark.bmp", r[0], r[1], w, h)

    # ---------------- 英文 ----------------
    print("=== 切到 English ===")
    # 点「通用」→ 语言下拉 → 选 English
    acc.click(r[0] + 120, nav_y + 4 * step)
    time.sleep(0.6)
    sel_x = r[0] + w - 130
    sel_y = r[1] + 203
    acc.click(sel_x, sel_y)
    time.sleep(0.7)
    # 原生下拉：Down×2 到 English，回车确认（比点坐标稳）
    for _ in range(2):
        _key(0x28)
    _key(0x0D)
    time.sleep(1.5)
    acc.shot("m4-17-english.bmp", r[0], r[1], w, h)
    for ln in acc.lines_since(mark, "配置变更", "设置页"):
        print("   " + ln)

    # 还原主题
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key_path, 0, winreg.KEY_SET_VALUE) as k:
        winreg.SetValueEx(k, "AppsUseLightTheme", 0, winreg.REG_DWORD, orig)
        winreg.SetValueEx(k, "SystemUsesLightTheme", 0, winreg.REG_DWORD, orig)
    acc.broadcast_setting_change()
    time.sleep(1.5)

    # ---------------- 内存：关窗回落 + 连续开关 5 次 ----------------
    print("=== 内存 ===")
    ws, priv_open = mem(app.pid)
    print("   打开时：工作集 %.1f MB / 私有 %.1f MB" % (ws / 1048576, priv_open / 1048576))
    for i in range(5):
        user32.PostMessageW(hwnd, 0x0010, 0, 0)  # WM_CLOSE
        time.sleep(1.2)
        if i == 0:
            ws, priv = mem(app.pid)
            print("   关闭后：工作集 %.1f MB / 私有 %.1f MB" % (ws / 1048576, priv / 1048576))
        # 重新打开：再启动一个实例（真实「双击 exe」路径，同时验证单实例唤起）
        second = subprocess.Popen([EXE])
        second.wait(timeout=5)
        time.sleep(3.5)
        hwnd = user32.FindWindowW(SETTINGS_CLASS, None)
    ws, priv = mem(app.pid)
    print("   开关 5 次后（仍打开）：工作集 %.1f MB / 私有 %.1f MB" % (ws / 1048576, priv / 1048576))
    user32.PostMessageW(hwnd, 0x0010, 0, 0)
    time.sleep(1.2)
    ws, priv = mem(app.pid)
    print("   最终关闭：工作集 %.1f MB / 私有 %.1f MB" % (ws / 1048576, priv / 1048576))

    app.kill()
    time.sleep(0.6)
    print("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
