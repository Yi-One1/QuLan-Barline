#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""开发辅助：抓取屏幕指定区域并保存为 BMP（零第三方依赖，纯 ctypes + GDI）。

用法：
    python scripts/grab-screen.py out.bmp <x> <y> <w> <h>

为什么不用 PowerShell + System.Drawing：本机安全策略禁止 `Add-Type`（运行期编译 .NET）。
GDI 的 BitBlt 走 ctypes 即可，且抓到的是「已合成」的桌面，正好用来验证分层窗口的
逐像素透明（我们的窗口在 BitBlt 结果里是已合成的）。
"""

import ctypes
import struct
import sys
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

SRCCOPY = 0x00CC0020
BI_RGB = 0
DIB_RGB_COLORS = 0


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wintypes.DWORD * 3)]


def _declare():
    """显式声明签名：默认 ctypes 会按 C int 转换参数，64 位 GDI 句柄会 Overflow。"""
    user32.GetDC.restype = wintypes.HDC
    user32.GetDC.argtypes = [wintypes.HWND]
    user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]

    gdi32.CreateCompatibleDC.restype = wintypes.HDC
    gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
    gdi32.CreateDIBSection.restype = wintypes.HANDLE
    gdi32.CreateDIBSection.argtypes = [
        wintypes.HDC,
        ctypes.POINTER(BITMAPINFO),
        wintypes.UINT,
        ctypes.POINTER(ctypes.c_void_p),
        wintypes.HANDLE,
        wintypes.DWORD,
    ]
    gdi32.SelectObject.restype = wintypes.HANDLE
    gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HANDLE]
    gdi32.BitBlt.restype = wintypes.BOOL
    gdi32.BitBlt.argtypes = [
        wintypes.HDC, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
        wintypes.HDC, ctypes.c_int, ctypes.c_int, wintypes.DWORD,
    ]
    gdi32.DeleteObject.restype = wintypes.BOOL
    gdi32.DeleteObject.argtypes = [wintypes.HANDLE]
    gdi32.DeleteDC.restype = wintypes.BOOL
    gdi32.DeleteDC.argtypes = [wintypes.HDC]


def grab(x, y, w, h, out_path):
    _declare()
    screen_dc = user32.GetDC(None)
    mem_dc = gdi32.CreateCompatibleDC(screen_dc)

    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = w
    bmi.bmiHeader.biHeight = -h  # 顶朝下
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = BI_RGB
    bmi.bmiHeader.biSizeImage = w * h * 4

    bits = ctypes.c_void_p()
    dib = gdi32.CreateDIBSection(
        mem_dc, ctypes.byref(bmi), DIB_RGB_COLORS, ctypes.byref(bits), None, 0
    )
    if not dib:
        raise OSError("CreateDIBSection failed: %d" % ctypes.get_last_error())

    gdi32.SelectObject(mem_dc, dib)
    if not gdi32.BitBlt(mem_dc, 0, 0, w, h, screen_dc, x, y, SRCCOPY):
        raise OSError("BitBlt failed: %d" % ctypes.get_last_error())

    raw = ctypes.string_at(bits, w * h * 4)

    # 写 BMP：BITMAPFILEHEADER + BITMAPINFOHEADER + 像素（自下而上、BGRA）
    row_stride = w * 4
    pixels = bytearray()
    for row in range(h - 1, -1, -1):
        pixels += raw[row * row_stride : (row + 1) * row_stride]

    header = struct.pack("<2sIHHI", b"BM", 14 + 40 + len(pixels), 0, 0, 14 + 40)
    info = struct.pack(
        "<IiiHHIIiiII", 40, w, h, 1, 32, BI_RGB, len(pixels), 0, 0, 0, 0
    )
    with open(out_path, "wb") as f:
        f.write(header)
        f.write(info)
        f.write(pixels)

    gdi32.DeleteObject(dib)
    gdi32.DeleteDC(mem_dc)
    user32.ReleaseDC(None, screen_dc)
    return out_path


if __name__ == "__main__":
    if len(sys.argv) != 6:
        print(__doc__)
        sys.exit(2)
    path = sys.argv[1]
    x, y, w, h = (int(v) for v in sys.argv[2:6])
    print(grab(x, y, w, h, path), w, h)
