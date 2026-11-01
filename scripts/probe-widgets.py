#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""开发辅助：枚举 explorer.exe 的顶层窗口，找出 Win11 内建「任务栏媒体控件」。

用法：
    python scripts/probe-widgets.py

用途：M2 的「与系统媒体控件共存」需求需要准确知道系统控件的**窗口类名**与**矩形**，
否则 `FindWindowW` 探测会落空。本脚本用 ctypes 直接调 user32，零第三方依赖
（本机 `Add-Type` 被禁，不能走 PowerShell + .NET）。
"""

import ctypes
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.FindWindowW.restype = wintypes.HWND
user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.FindWindowExW.restype = wintypes.HWND
user32.FindWindowExW.argtypes = [wintypes.HWND, wintypes.HWND, wintypes.LPCWSTR, wintypes.LPCWSTR]

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


def class_of(hwnd):
    buf = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(hwnd, buf, 256)
    return buf.value


def title_of(hwnd):
    buf = ctypes.create_unicode_buffer(512)
    user32.GetWindowTextW(hwnd, buf, 512)
    return buf.value


def rect_of(hwnd):
    r = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return (r.left, r.top, r.right, r.bottom)


def pid_of(hwnd):
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def process_name(pid):
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    h = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return ""
    try:
        size = wintypes.DWORD(260)
        buf = ctypes.create_unicode_buffer(260)
        if kernel32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size)):
            return buf.value
        return ""
    finally:
        kernel32.CloseHandle(h)


def main():
    rows = []

    def cb(hwnd, _lp):
        cls = class_of(hwnd)
        if not cls:
            return True
        pid = pid_of(hwnd)
        exe = process_name(pid).lower()
        # 只关心 explorer.exe 与类名里带 barline / media / tray 的窗口
        interesting = (
            "explorer.exe" in exe
            or "barline" in cls.lower()
            or "media" in cls.lower()
            or "tray" in cls.lower()
        )
        if interesting:
            rows.append(
                (
                    cls,
                    title_of(hwnd),
                    rect_of(hwnd),
                    bool(user32.IsWindowVisible(hwnd)),
                    exe.rsplit("\\", 1)[-1],
                    hwnd,
                )
            )
        return True

    user32.EnumWindows(WNDENUMPROC(cb), 0)

    print("%-28s %-6s %-14s %-26s %s" % ("class", "vis", "proc", "rect(l,t,r,b)", "title"))
    print("-" * 110)
    for cls, title, (l, t, r, b), vis, exe, hwnd in sorted(rows, key=lambda x: x[0]):
        print(
            "%-28s %-6s %-14s %-26s %s"
            % (cls, "Y" if vis else "N", exe, "(%d,%d,%d,%d) %dx%d" % (l, t, r, b, r - l, b - t), title[:40])
        )

    print()
    print("--- 定向探测 ---")
    for cls in ("Shell_TrayWnd", "TrayNotifyWnd", "BarlineWidget", "BarlineBackdrop", "BarlineSettings"):
        if cls == "TrayNotifyWnd":
            tray = user32.FindWindowW("Shell_TrayWnd", None)
            h = user32.FindWindowExW(tray, None, "TrayNotifyWnd", None)
        else:
            h = user32.FindWindowW(cls, None)
        if h:
            print("%-18s hwnd=0x%X vis=%s rect=%s" % (cls, h, bool(user32.IsWindowVisible(h)), rect_of(h)))
        else:
            print("%-18s (not found)" % cls)


if __name__ == "__main__":
    main()
