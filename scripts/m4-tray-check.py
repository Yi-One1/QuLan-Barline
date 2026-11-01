#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""M4 验收：托盘图标（三项菜单 / 勾选态 / 双击）+ 全局快捷键。

用法：python scripts\\m4-tray-check.py
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

spec = importlib.util.spec_from_file_location("acc", os.path.join(HERE, "m2-accept.py"))
acc = importlib.util.module_from_spec(spec)
sys.modules["acc"] = acc
spec.loader.exec_module(acc)

MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010
CFG = r"C://Users//caihongyi//AppData//Local//Barline//config.json"

shell32 = ctypes.WinDLL("shell32", use_last_error=True)
user32 = ctypes.WinDLL("user32", use_last_error=True)


class GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", wintypes.DWORD),
        ("Data2", wintypes.WORD),
        ("Data3", wintypes.WORD),
        ("Data4", ctypes.c_byte * 8),
    ]


class NOTIFYICONIDENTIFIER(ctypes.Structure):
    _fields_ = [
        ("cbSize", wintypes.DWORD),
        ("hWnd", wintypes.HWND),
        ("uID", wintypes.UINT),
        ("guidItem", GUID),
    ]


def tray_icon_rect():
    """Shell_NotifyIconGetRect：拿到托盘图标的屏幕矩形。"""
    hwnd = user32.FindWindowW("BarlineTrayWnd", None)
    if not hwnd:
        return None
    nid = NOTIFYICONIDENTIFIER()
    nid.cbSize = ctypes.sizeof(NOTIFYICONIDENTIFIER)
    nid.hWnd = hwnd
    nid.uID = 1
    r = wintypes.RECT()
    hr = shell32.Shell_NotifyIconGetRect(ctypes.byref(nid), ctypes.byref(r))
    if hr != 0:
        return None
    return (r.left, r.top, r.right, r.bottom)


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class _U(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("pad", ctypes.c_byte * 32)]


class KINPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _U)]


def key_event(vk, up=False):
    user32.SendInput.argtypes = [wintypes.UINT, ctypes.c_void_p, ctypes.c_int]
    inp = KINPUT()
    inp.type = 1
    inp.u.ki = KEYBDINPUT(vk, 0, 0x0002 if up else 0, 0, None)
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(KINPUT))
    time.sleep(0.04)


def press(mods, vk):
    """按一次组合键：mods = [VK_CONTROL, VK_MENU, ...]"""
    for m in mods:
        key_event(m)
    key_event(vk)
    key_event(vk, up=True)
    for m in reversed(mods):
        key_event(m, up=True)


VK_CONTROL, VK_MENU, VK_SHIFT = 0x11, 0x12, 0x10


def is_widget_visible():
    """小组件窗口是否可见（GetWindowRect 对隐藏窗口仍然有效，必须用 IsWindowVisible）。"""
    h = user32.FindWindowW("BarlineWidgetWnd", None)
    if not h:
        return False
    return bool(user32.IsWindowVisible(h))


def main():
    acc.kill_isolated_edge()
    time.sleep(0.5)
    mark = len(acc.log_text())
    app = subprocess.Popen([EXE])
    time.sleep(3.0)

    print("=== 托盘 ===")
    for ln in acc.lines_since(mark, "托盘"):
        print("   " + ln)
    rect = tray_icon_rect()
    print("   托盘图标矩形 = %s" % (rect,))

    print("=== 快捷键注册 ===")
    for ln in acc.lines_since(mark, "热键"):
        print("   " + ln)

    # 1) Ctrl+Alt+H（本机唯一空闲的默认组合）→ 显示/隐藏小组件
    print("=== 触发 Ctrl+Alt+H（默认组合里本机唯一可注册的）===")
    m2 = len(acc.log_text())
    press([VK_CONTROL, VK_MENU], ord("H"))
    time.sleep(0.8)
    for ln in acc.lines_since(m2, "热键触发", "托盘：", "小组件"):
        print("   " + ln)
    print("   小组件窗口 = %s（隐藏后应为 None）" % (acc.widget_rect(),))
    press([VK_CONTROL, VK_MENU], ord("H"))
    time.sleep(0.8)

    # 3) 弹菜单并截图。
    #
    # 注意：新加的托盘图标在 Win11 默认被折叠进「隐藏图标」浮出层，直接点托盘区
    # 点到的是那个「^」按钮（实测回调根本不触发）。所以这里直接给托盘窗口投递
    # shell 的回调消息，等价于「用户右键了图标」——菜单本身的代码路径完全一致。
    WM_BARLINE_TRAY = 0x8000 + 2
    WM_RBUTTONUP = 0x0205
    tray_hwnd = user32.FindWindowW("BarlineTrayWnd", None)
    print("   托盘窗口 = %s" % (tray_hwnd,))
    CURSOR = (2000, 1299)
    # 菜单相对光标的偏移（实测：菜单左上 ≈ 光标 + (40, 6)，行高约 24px，分隔线约 9px）
    MENU_DX = 135
    ROW1_DY = 21
    ROW2_DY = 45
    ROW3_DY = 78

    def open_menu():
        acc.move(*CURSOR)
        time.sleep(0.2)
        user32.PostMessageW(tray_hwnd, WM_BARLINE_TRAY, 0, WM_RBUTTONUP)
        time.sleep(0.8)

    if tray_hwnd:
        open_menu()
        acc.shot("m4-01-tray-menu.bmp", 1800, 1150, 560, 300)
        key_event(0x1B)
        time.sleep(0.5)

    # 4) 选「显示/隐藏小组件」→ 可见性必须翻转
    if tray_hwnd:
        m4 = len(acc.log_text())
        vis_before = is_widget_visible()
        open_menu()
        acc.click(CURSOR[0] + MENU_DX, CURSOR[1] + ROW2_DY)
        time.sleep(0.9)
        vis_after = is_widget_visible()
        for ln in acc.lines_since(m4, "托盘"):
            print("   " + ln)
        print("   小组件可见性：%s → %s（应翻转）" % (vis_before, vis_after))

    # 5) 再选一次恢复显示，然后选「退出」
    if tray_hwnd:
        open_menu()
        acc.click(CURSOR[0] + MENU_DX, CURSOR[1] + ROW2_DY)
        time.sleep(0.8)
        print("   恢复显示后可见 = %s" % is_widget_visible())

        m5 = len(acc.log_text())
        open_menu()
        # 「退出」用键盘选更稳（分隔线不参与导航，Down×3 一定落到第三项）
        for _ in range(3):
            key_event(0x28)
            time.sleep(0.12)
        key_event(0x0D)
        time.sleep(1.5)
        for ln in acc.lines_since(m5, "托盘", "退出"):
            print("   " + ln)
        print("   进程是否已退出 = %s" % (app.poll() is not None,))

    if app.poll() is None:
        print("（进程仍在，强制结束）")
        app.kill()
    time.sleep(0.8)
    print("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
