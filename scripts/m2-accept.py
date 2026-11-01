#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""M2 全流程验收：真实 SMTC 会话 + 截图 + 合成鼠标交互 + 明暗背景。

必须在同一次进程调用内跑完：工具调用结束后子进程树会被回收。

关于「明 / 暗背景」：本机 `SPI_SETDESKWALLPAPER` 返回成功但桌面不变（壁纸被
系统策略锁住），所以改用「最大化一个浅色 / 深色页面」的方式改变任务栏与其上方
桌面区域的真实像素亮度——采样线程看到的就是屏幕上真实的背景，验证链路等价。
"""

import ctypes
import importlib.util
import os
import struct
import subprocess
import sys
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = r"E:\APP\WorkBuddy\工作空间\曲栏Barline"
SHOTS = os.path.join(ROOT, "docs", "screenshots")
EXE = os.path.join(ROOT, "target", "release", "barline.exe")
LOG = r"C:\Users\caihongyi\AppData\Local\Barline\barline.log"

user32 = ctypes.WinDLL("user32", use_last_error=True)

_spec = importlib.util.spec_from_file_location(
    "grabscreen", os.path.join(ROOT, "scripts", "grab-screen.py")
)
grabscreen = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(grabscreen)

user32.FindWindowW.restype = wintypes.HWND
user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetSystemMetrics.argtypes = [ctypes.c_int]
user32.SendInput.argtypes = [wintypes.UINT, ctypes.c_void_p, ctypes.c_int]

SW = user32.GetSystemMetrics(0)
SH = user32.GetSystemMetrics(1)

MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_ABSOLUTE = 0x8000
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_WHEEL = 0x0800


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", wintypes.LONG),
        ("dy", wintypes.LONG),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class _U(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("pad", ctypes.c_byte * 32)]


class INPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _U)]


def _send(flags, dx=0, dy=0, data=0):
    inp = INPUT()
    inp.type = 0
    inp.u.mi = MOUSEINPUT(dx, dy, data, flags, 0, None)
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT))


def move(x, y):
    _send(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, int(x * 65536 / SW), int(y * 65536 / SH))
    time.sleep(0.06)


def click(x, y):
    move(x, y)
    time.sleep(0.20)
    _send(MOUSEEVENTF_LEFTDOWN)
    time.sleep(0.09)
    _send(MOUSEEVENTF_LEFTUP)
    time.sleep(0.45)


def drag(x1, y1, x2, y2, steps=10):
    move(x1, y1)
    time.sleep(0.12)
    _send(MOUSEEVENTF_LEFTDOWN)
    time.sleep(0.15)
    for i in range(1, steps + 1):
        move(int(x1 + (x2 - x1) * i / steps), int(y1 + (y2 - y1) * i / steps))
        time.sleep(0.05)
    time.sleep(0.15)
    _send(MOUSEEVENTF_LEFTUP)
    time.sleep(0.45)


def wheel(x, y, notches):
    move(x, y)
    time.sleep(0.25)
    for _ in range(abs(notches)):
        _send(MOUSEEVENTF_WHEEL, 0, 0, 120 if notches > 0 else -120)
        time.sleep(0.10)


def shot(name, x, y, w, h):
    path = os.path.join(SHOTS, name)
    grabscreen.grab(x, y, w, h, path)
    print("  [shot] %-30s %dx%d @ (%d,%d)" % (name, w, h, x, y))
    return path


def read_bmp(path):
    with open(path, "rb") as f:
        data = f.read()
    w, h = struct.unpack("<ii", data[18:26])
    off = struct.unpack("<I", data[10:14])[0]
    stride = w * 4
    rows = [data[off + r * stride : off + (r + 1) * stride] for r in range(h)]
    rows.reverse()
    return w, h, b"".join(rows)


def write_bmp(path, w, h, px):
    rows = [px[r * w * 4 : (r + 1) * w * 4] for r in range(h)]
    rows.reverse()
    body = b"".join(rows)
    hdr = struct.pack("<2sIHHI", b"BM", 14 + 40 + len(body), 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 32, 0, len(body), 0, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(hdr + info + body)


def crop(src, sx, sy, x, y, w, h, dst):
    sw, sh, px = read_bmp(src)
    ox, oy = x - sx, y - sy
    out = bytearray()
    for r in range(h):
        s = ((oy + r) * sw + ox) * 4
        out += px[s : s + w * 4]
    write_bmp(dst, w, h, bytes(out))
    return dst


def region_diff(a, b, x0, y0, x1, y1, thresh=8):
    w, h, pa = read_bmp(a)
    _, _, pb = read_bmp(b)
    diff = 0
    n = 0
    for y in range(y0, min(y1, h)):
        for x in range(x0, min(x1, w)):
            i = (y * w + x) * 4
            n += 1
            if (
                abs(pa[i] - pb[i]) > thresh
                or abs(pa[i + 1] - pb[i + 1]) > thresh
                or abs(pa[i + 2] - pb[i + 2]) > thresh
            ):
                diff += 1
    return diff / max(1, n)


def avg_luma(path):
    _, _, px = read_bmp(path)
    tot = 0
    n = len(px) // 4
    for i in range(0, len(px), 4):
        tot += 0.0722 * px[i] + 0.7152 * px[i + 1] + 0.2126 * px[i + 2]
    return tot / max(1, n)



SW_MAXIMIZE = 3
SW_MINIMIZE = 6


def find_window_by_title(sub):
    user32.EnumWindows.restype = wintypes.BOOL
    found = []

    WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def cb(h, _l):
        buf = ctypes.create_unicode_buffer(512)
        user32.GetWindowTextW(h, buf, 512)
        if sub in buf.value and user32.IsWindowVisible(h):
            found.append(h)
        return True

    user32.EnumWindows(WNDENUMPROC(cb), 0)
    return found[0] if found else None


def maximize_window_with_title(sub, timeout=8.0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        h = find_window_by_title(sub)
        if h:
            user32.ShowWindow(h, SW_MAXIMIZE)
            time.sleep(0.8)
            user32.SetForegroundWindow(h)
            return h
        time.sleep(0.3)
    print("  [WARN] 没找到标题含 %r 的窗口" % sub)
    return None


def minimize_window_with_title(sub, timeout=6.0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        h = find_window_by_title(sub)
        if h:
            user32.ShowWindow(h, SW_MINIMIZE)
            return h
        time.sleep(0.3)
    print("  [WARN] 没找到标题含 %r 的窗口（无法最小化）" % sub)
    return None


def pixel_luma(x, y):
    path = os.path.join(HERE, "_px.bmp")
    grabscreen.grab(x, y, 1, 1, path)
    return avg_luma(path)


def wait_bg_luma(x, y, want_light, timeout=10.0):
    t0 = time.time()
    while time.time() - t0 < timeout:
        v = pixel_luma(x, y)
        if (want_light and v > 190) or ((not want_light) and v < 60):
            return v
        time.sleep(0.4)
    return pixel_luma(x, y)


def kill_isolated_edge():
    """结束占用隔离 profile 的 Edge 进程（绝不碰用户自己的浏览器）。"""
    script = (
        "Get-CimInstance Win32_Process -Filter \"Name='msedge.exe'\" | "
        "Where-Object { $_.CommandLine -like '*barline_edge_profile*' } | "
        "ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }"
    )
    subprocess.run(
        ["powershell", "-NoProfile", "-NonInteractive", "-Command", script],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def broadcast_setting_change():
    """模拟「壁纸/主题变化」：向所有顶层窗口广播 WM_SETTINGCHANGE。"""
    user32.SendMessageTimeoutW.argtypes = [
        wintypes.HWND,
        wintypes.UINT,
        wintypes.WPARAM,
        ctypes.c_void_p,
        wintypes.UINT,
        wintypes.UINT,
        ctypes.POINTER(ctypes.c_size_t),
    ]
    res = ctypes.c_size_t()
    user32.SendMessageTimeoutW(
        0xFFFF, 0x001A, 0, ctypes.c_wchar_p("ImmersiveColorSet"), 0x0002, 3000, ctypes.byref(res)
    )


def set_theme_light(light):
    """切换 Windows 浅色 / 深色主题（任务栏底色跟着变），并广播。"""
    import winreg

    path = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
    k = winreg.OpenKey(winreg.HKEY_CURRENT_USER, path, 0, winreg.KEY_SET_VALUE)
    winreg.SetValueEx(k, "AppsUseLightTheme", 0, winreg.REG_DWORD, 1 if light else 0)
    winreg.SetValueEx(k, "SystemUsesLightTheme", 0, winreg.REG_DWORD, 1 if light else 0)
    winreg.CloseKey(k)
    broadcast_setting_change()
    print("  主题切换 → %s" % ("浅色" if light else "深色"))
    time.sleep(2.0)


def read_theme():
    import winreg

    path = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as k:
            a = winreg.QueryValueEx(k, "AppsUseLightTheme")[0]
            s = winreg.QueryValueEx(k, "SystemUsesLightTheme")[0]
            return bool(a), bool(s)
    except OSError:
        return True, True


class WNDCLASSW(ctypes.Structure):
    _fields_ = [
        ("style", wintypes.UINT),
        ("lpfnWndProc", ctypes.c_void_p),
        ("cbClsExtra", ctypes.c_int),
        ("cbWndExtra", ctypes.c_int),
        ("hInstance", wintypes.HINSTANCE),
        ("hIcon", wintypes.HICON),
        ("hCursor", wintypes.HANDLE),
        ("hbrBackground", wintypes.HBRUSH),
        ("lpszMenuName", wintypes.LPCWSTR),
        ("lpszClassName", wintypes.LPCWSTR),
    ]


_OVERLAY_PROC = None
_OVERLAY_SEQ = [0]


def make_overlay(x, y, w, h, rgb):
    """建一个 topmost 纯色窗口垫在小组件后面，模拟「浅色 / 深色壁纸」。

    本机 `SPI_SETDESKWALLPAPER` 被策略锁住（返回成功但桌面不变），任务栏又是
    半透明、始终透出壁纸，所以没法用系统壁纸做明暗对比。这里用一个 topmost
    的纯色 popup 覆盖「任务栏 + 任务栏上方桌面」那一小块——采样线程抓到的就是
    屏幕上真实像素，链路与真实壁纸完全一致。小组件自己是 topmost，会在下一次
    1s 心跳里 `ensure_on_top` 升到这个垫片之上。
    """
    global _OVERLAY_PROC
    gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
    gdi32.CreateSolidBrush.restype = wintypes.HANDLE
    gdi32.CreateSolidBrush.argtypes = [wintypes.DWORD]
    brush = gdi32.CreateSolidBrush((rgb[2] << 16) | (rgb[1] << 8) | rgb[0])

    WNDPROC = ctypes.WINFUNCTYPE(
        ctypes.c_ssize_t, wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t
    )
    user32.DefWindowProcW.restype = ctypes.c_ssize_t
    user32.DefWindowProcW.argtypes = [
        wintypes.HWND,
        wintypes.UINT,
        ctypes.c_size_t,
        ctypes.c_ssize_t,
    ]

    @WNDPROC
    def proc(hwnd, msg, wp, lp):
        return user32.DefWindowProcW(hwnd, msg, wp, lp)

    _OVERLAY_PROC = proc
    _OVERLAY_SEQ[0] += 1
    cls_name = "BarlineBgProbe%d" % _OVERLAY_SEQ[0]
    wc = WNDCLASSW(
        style=0,
        lpfnWndProc=ctypes.cast(proc, ctypes.c_void_p),
        cbClsExtra=0,
        cbWndExtra=0,
        hInstance=None,
        hIcon=None,
        hCursor=None,
        hbrBackground=brush,
        lpszMenuName=None,
        lpszClassName=cls_name,
    )
    user32.RegisterClassW.argtypes = [ctypes.POINTER(WNDCLASSW)]
    user32.RegisterClassW(ctypes.byref(wc))

    WS_POPUP = 0x80000000
    WS_EX_TOPMOST = 0x00000008
    WS_EX_TOOLWINDOW = 0x00000080
    WS_EX_NOACTIVATE = 0x08000000
    user32.CreateWindowExW.restype = wintypes.HWND
    hwnd = user32.CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        cls_name,
        "BarlineBgProbe",
        WS_POPUP,
        x,
        y,
        w,
        h,
        None,
        None,
        None,
        None,
    )
    user32.ShowWindow(hwnd, 4)  # SW_SHOWNOACTIVATE
    return hwnd


def destroy_overlay(hwnd):
    if hwnd:
        user32.DestroyWindow(hwnd)


def widget_rect():
    h = user32.FindWindowW("BarlineWidgetWnd", None)
    if not h:
        return None
    r = wintypes.RECT()
    user32.GetWindowRect(h, ctypes.byref(r))
    return (r.left, r.top, r.right, r.bottom)


def tray_rect():
    h = user32.FindWindowW("Shell_TrayWnd", None)
    r = wintypes.RECT()
    user32.GetWindowRect(h, ctypes.byref(r))
    return (r.left, r.top, r.right, r.bottom)


def log_text():
    try:
        with open(LOG, "r", encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def lines_since(mark, *pats):
    out = []
    for ln in log_text()[mark:].splitlines():
        if not pats or any(p in ln for p in pats):
            out.append(ln.strip())
    return out


def wait_stable_rect(timeout=20):
    """等窗口矩形连续 1.2s 不变（宽度会随歌名测量结果变化）。"""
    last = None
    stable_since = None
    t0 = time.time()
    while time.time() - t0 < timeout:
        r = widget_rect()
        if r != last:
            last = r
            stable_since = time.time()
        elif r and time.time() - stable_since > 1.2:
            return r
        time.sleep(0.15)
    return widget_rect()


def write_page(name, bg, fg, title):
    p = os.path.join(HERE, name)
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            """<!doctype html><html><head><meta charset="utf-8"><title>%s</title>
<style>html,body{margin:0;height:100%%;background:%s;}</style></head>
<body><div style="font:18px/1.7 Segoe UI;color:%s;padding:48px">%s</div></body></html>"""
            % (title, bg, fg, title)
        )
    return p


def write_demo2(path):
    """第二条会话：超长歌名 + 无封面 → 验证省略号与渐变占位。"""
    with open(path, "w", encoding="utf-8") as fp:
        fp.write(
            """<!doctype html>
<html><head><meta charset="utf-8"><title>Barline 长标题用例</title></head>
<body style="background:#12131a;color:#ddd;font-family:Segoe UI">
<h2>超长歌名 / 无封面用例</h2>
<audio id="a" src="barline_demo.wav" autoplay loop controls></audio>
<script>
const a = document.getElementById('a');
if ('mediaSession' in navigator) {
  navigator.mediaSession.metadata = new MediaMetadata({
    title: 'Late Night Drive (Extended Club Mix) [2026 Remastered Deluxe Edition]',
    artist: 'Barline Test Ensemble feat. A Very Long Artist Name',
    album: 'M2 Acceptance'
  });
}
a.play().catch(e => document.title = 'blocked: ' + e);
</script></body></html>
"""
        )
    return path


def main():
    os.makedirs(SHOTS, exist_ok=True)
    edge = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
    if not os.path.exists(edge):
        edge = r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"

    write_page("barline_bg_light.html", "#F4F4F7", "#1A1A1A", "浅色背景（对应 m2-04）")
    write_page("barline_bg_dark.html", "#0C0D12", "#E8E8E8", "深色背景（对应 m2-05）")
    write_demo2(os.path.join(HERE, "barline_demo2.html"))

    kill_isolated_edge()
    time.sleep(1.0)

    srv = subprocess.Popen(
        [sys.executable, "-m", "http.server", "8765", "--bind", "127.0.0.1"],
        cwd=HERE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(1.2)

    profile = os.path.join(HERE, "barline_edge_profile")
    common = [
        "--user-data-dir=" + profile,
        "--autoplay-policy=no-user-gesture-required",
        "--no-first-run",
        "--no-default-browser-check",
    ]
    procs = []

    def open_window(url, maximize=False, pos=(120, 100)):
        args = [edge] + common
        if maximize:
            args += ["--new-window", "--start-maximized"]
        elif pos:
            args += ["--window-position=%d,%d" % pos, "--window-size=980,680"]
        args.append(url)
        p = subprocess.Popen(args)
        procs.append(p)
        return p

    open_window("http://127.0.0.1:8765/barline_demo.html", pos=(120, 100))
    print("Edge 已启动，等待 10s 建立 SMTC 会话…")
    time.sleep(10)
    # 把播放窗口最小化：桌面恢复干净（只剩壁纸 + 任务栏），截图才有代表性
    minimize_window_with_title("Barline 验收音轨")
    time.sleep(1.5)

    tr = tray_rect()
    bar_h = tr[3] - tr[1]
    print("任务栏 = %s（厚度 %d）" % (tr, bar_h))
    print("任务栏左侧桌面像素亮度 = %.0f/255（应 > 150，否则有窗口挡着）"
          % pixel_luma(2000, tr[1] - 30))

    ref_x, ref_y = 1700, tr[1] - 130
    ref = shot("_ref_off.bmp", ref_x, ref_y, 900, 130 + bar_h)

    mark = len(log_text())
    app = subprocess.Popen([EXE])
    wr = wait_stable_rect()
    if not wr:
        print("!! 小组件没出现")
        return 1
    time.sleep(1.0)
    wl, wt, wrr, wb = wr
    ww, wh = wrr - wl, wb - wt
    main_top = wh - bar_h
    print("小组件 %dx%d @ (%d,%d)  main_top=%d" % (ww, wh, wl, wt, main_top))
    for ln in lines_since(mark, "session changed", "封面已解码", "布局"):
        print("   " + ln)

    cx_prev = wl + ww - 104 + 14
    cx_toggle = wl + ww - 70 + 14
    cx_next = wl + ww - 22
    cy_btn = wt + main_top + 24
    cy_prog = wt + wh - 4

    # ---- m2-01 静止态 ----
    move(1200, 700)
    time.sleep(1.0)
    idle = shot("m2-01-idle.bmp", wl, wt, ww, wh)
    print("  静止态平均亮度 = %.0f/255（本机壁纸是轮播的，逐帧会变）" % avg_luma(idle))

    # ---- m2-02 悬停态 ----
    move(wl + ww // 2, cy_btn)
    time.sleep(1.2)
    hover = shot("m2-02-hover.bmp", wl, wt, ww, wh)
    print("  悬停态 vs 静止态：全区域 %.2f%%（背景已浮现）"
          % (region_diff(hover, idle, 0, 0, ww, wh) * 100))

    # ---- m2-03 完整形态（带任务栏上下文）----
    move(1200, 700)
    time.sleep(1.0)
    shot("m2-03-full.bmp", wl - 200, wt - 24, ww + 400, wh + 24)

    # ---- 交互 ----
    print("--- 交互 ---")
    m = len(log_text())
    click(cx_toggle, cy_btn)
    time.sleep(0.8)
    for ln in lines_since(m, "命令", "session changed"):
        print("   [toggle] " + ln)

    m = len(log_text())
    click(cx_prev, cy_btn)
    time.sleep(1.0)
    for ln in lines_since(m, "命令", "session changed"):
        print("   [prev]   " + ln)

    m = len(log_text())
    click(cx_next, cy_btn)
    time.sleep(1.0)
    for ln in lines_since(m, "命令", "session changed"):
        print("   [next]   " + ln)

    m = len(log_text())
    # 拖动中截图：时间气泡只在按住期间 + 松开后 500ms 内出现，必须先拍再松手
    move(wl + int(ww * 0.25), cy_prog)
    time.sleep(0.15)
    _send(MOUSEEVENTF_LEFTDOWN)
    time.sleep(0.18)
    for i in range(1, 11):
        move(wl + int(ww * (0.25 + 0.047 * i)), cy_prog)
        time.sleep(0.05)
    time.sleep(0.30)
    shot("m2-07-seek-bubble.bmp", wl - 20, wt - 12, ww + 40, wh + 12)
    _send(MOUSEEVENTF_LEFTUP)
    time.sleep(0.6)
    for ln in lines_since(m, "跳转", "session changed"):
        print("   [seek]   " + ln)

    m = len(log_text())
    move(wl + ww // 2, cy_btn)
    time.sleep(0.5)
    wheel(wl + ww // 2, cy_btn, 3)
    shot("m2-06-wheel-hud.bmp", wl - 10, wt - 12, ww + 20, wh + 12)
    for ln in lines_since(m, "滚轮"):
        print("   [wheel]  " + ln)
    time.sleep(1.3)
    wheel(wl + ww // 2, cy_btn, -3)
    time.sleep(1.6)

    m = len(log_text())
    click(wl + 24, wt + main_top + 24)
    time.sleep(1.0)
    for ln in lines_since(m, "激活"):
        print("   [cover]  " + ln)

    # ---- 明 / 暗背景：用 topmost 纯色垫片改变小组件背后的真实像素 ----
    print("--- 明暗背景 ---")
    move(1200, 700)
    ov_x, ov_y = wl - 40, wt - 40
    ov_w, ov_h = ww + 80, wh + 40

    ov = make_overlay(ov_x, ov_y, ov_w, ov_h, (0xF4, 0xF4, 0xF7))
    time.sleep(2.5)  # 等 1s 心跳把小组件 ensure_on_top 到垫片之上
    print("  垫片后像素亮度 = %.0f/255" % pixel_luma(wl + ww // 2, wt + 6))
    light = shot("m2-04-light-wallpaper.bmp", wl, wt, ww, wh)
    print("  浅色背景：区域平均亮度 = %.0f/255" % avg_luma(light))
    shot("m2-04b-light-context.bmp", wl - 200, wt - 24, ww + 400, wh + 24)
    destroy_overlay(ov)
    time.sleep(1.0)

    ov = make_overlay(ov_x, ov_y, ov_w, ov_h, (0x0C, 0x0D, 0x12))
    time.sleep(2.5)
    print("  垫片后像素亮度 = %.0f/255" % pixel_luma(wl + ww // 2, wt + 6))
    dark = shot("m2-05-dark-wallpaper.bmp", wl, wt, ww, wh)
    print("  深色背景：区域平均亮度 = %.0f/255" % avg_luma(dark))
    shot("m2-05b-dark-context.bmp", wl - 200, wt - 24, ww + 400, wh + 24)
    destroy_overlay(ov)
    time.sleep(1.5)

    # ---- 长标题 + 无封面 ----
    print("--- 长标题 / 无封面 ---")
    open_window("http://127.0.0.1:8765/barline_demo2.html")
    time.sleep(3.0)
    minimize_window_with_title("Barline 长标题用例")
    time.sleep(4.0)
    r2 = wait_stable_rect(15)
    print("  新窗口矩形 = %s" % (r2,))
    for ln in lines_since(mark, "session changed")[-2:]:
        print("   " + ln)
    if r2:
        wl2, wt2, wrr2, wb2 = r2
        ww2, wh2 = wrr2 - wl2, wb2 - wt2
        shot("m2-08-long-title-no-cover.bmp", wl2, wt2, ww2, wh2)
        print("  长标题形态 %dx%d" % (ww2, wh2))

    # ---- 静止态透明度的严格验证（垫一块静态纯色 → A/B 间隔 0.35s）----
    print("--- 静止态透明度 A/B ---")
    ov = make_overlay(ov_x, ov_y, ov_w, ov_h, (0xF4, 0xF4, 0xF7))
    time.sleep(2.5)
    move(1200, 700)
    time.sleep(1.2)
    on = shot("_transp_on.bmp", wl, wt, ww, wh)
    app.kill()
    time.sleep(0.35)
    off = shot("_transp_off.bmp", wl, wt, ww, wh)
    q = region_diff(on, off, int(ww * 0.62), 4, ww - 12, main_top - 6)
    w_all = region_diff(on, off, 0, 0, ww, wh)
    print("  纯背景区差异 %.2f%% / 全区域差异 %.2f%%（A/B 间隔 0.35s）" % (q * 100, w_all * 100))
    destroy_overlay(ov)

    time.sleep(0.8)
    for p in procs:
        p.kill()
    kill_isolated_edge()
    srv.kill()
    print("已清理进程")
    return 0


if __name__ == "__main__":
    sys.exit(main())
