#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""G9 组① Go/No-Go 探针：枚举 Shell_TrayWnd 子树，判定任务栏的绘制形态。

用法：
    python scripts/g9_taskbar_probe.py               # 真实探测，输出 JSON
    python scripts/g9_taskbar_probe.py --self-test   # 纯逻辑自检，不碰任何系统 API

设计约束（对应 G9 组① 验收口径）：
  1. **只读**：只枚举窗口、读类名/矩形/可见性，不改任何窗口、不注入、不写内存。
  2. **判定逻辑必须独立可测**：分类函数只吃「类名列表」，与 Win32 调用解耦，
     所以 --self-test 能在没有任何任务栏的环境里跑。
  3. **反向验证**：故意把判定表改坏，分类结果必须跟着变；否则说明测试锁不住接线
     （项目 G8 血泪教训：「测试全绿但功能没做」）。
  4. 零第三方依赖，继承 scripts/probe-widgets.py 的 ctypes 写法（本机禁 Add-Type）。

输出 JSON 字段：
  environment / tray_root / descendants[] / classification / limits
"""

import ctypes
import json
import sys
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

user32.FindWindowW.restype = wintypes.HWND
user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetParent.restype = wintypes.HWND
user32.GetParent.argtypes = [wintypes.HWND]
user32.EnumChildWindows.restype = wintypes.BOOL
user32.EnumChildWindows.argtypes = [wintypes.HWND, ctypes.c_void_p, wintypes.LPARAM]

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

# ---- 判定表（改坏这里的任何一组，--self-test 的反向验证必须失败） ----
CLASSIC_CLOCK_MARKERS = (
    "TrayClockWClass", "ClockButton", "TrayNotifyWnd",
    "SysPager", "ToolbarWindow32", "TrayClockWClass2",
)
XAML_MARKERS = (
    "Windows.UI.Composition", "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow", "TopLevelWindowForOverflowXamlIsland",
    "Xaml_WindowedPopupClass", "DesktopWindowXamlSource",
)
TASKLIST_MARKERS = ("MSTaskListWClass", "MSTaskSwWClass", "ReBarWindow32")

# 自检样本：模拟三种可能的世界，全部与真实系统无关
SAMPLE_CLASSIC = ["Shell_TrayWnd", "TrayNotifyWnd", "TrayClockWClass", "SysPager", "ToolbarWindow32"]
SAMPLE_XAML = ["Shell_TrayWnd", "Windows.UI.Composition.DesktopWindowContentBridge",
               "Windows.UI.Core.CoreWindow", "XamlExplorerHostIslandWindow"]
SAMPLE_MIXED = ["Shell_TrayWnd", "TrayNotifyWnd", "TrayClockWClass",
                "Windows.UI.Composition.DesktopWindowContentBridge", "MSTaskListWClass"]
SAMPLE_EMPTY = ["Shell_TrayWnd"]


def classify(classes):
    """纯函数：给定窗口类名列表，判定任务栏是否存在「经典可子类化绘制面」。

    verdict:
      classic_hookable —— 有经典时钟/托盘窗口，用户态 WNDPROC 子类化有机会
      mixed             —— 经典窗口与 XAML island 并存，需分别验证
      xaml_only         —— 只有 XAML/Composition 视觉树，经典子类化基本无效
      unknown           —— 信息不足
    """
    cs = [str(c) for c in classes]
    clock = sorted({c for c in cs if any(m.lower() in c.lower() for m in CLASSIC_CLOCK_MARKERS)})
    xaml = sorted({c for c in cs if any(m.lower() in c.lower() for m in XAML_MARKERS)})
    tasklist = sorted({c for c in cs if any(m.lower() in c.lower() for m in TASKLIST_MARKERS)})

    if xaml and not clock:
        verdict = "xaml_only"
    elif clock and xaml:
        verdict = "mixed"
    elif clock:
        verdict = "classic_hookable"
    else:
        verdict = "unknown"

    return {
        "verdict": verdict,
        "classic_clock_windows": clock,
        "xaml_windows": xaml,
        "tasklist_windows": tasklist,
        "classic_surface_count": len(clock),
        "xaml_surface_count": len(xaml),
    }


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
    return [r.left, r.top, r.right, r.bottom]


def pid_of(hwnd):
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def enumerate_tray():
    """只读枚举 Shell_TrayWnd 及全部后代；EnumChildWindows 本身递归遍历后代。"""
    root = user32.FindWindowW("Shell_TrayWnd", None)
    if not root:
        return None, [], "Shell_TrayWnd 不存在（Explorer 可能正在重启）"

    root_row = {
        "class": class_of(root), "title": title_of(root),
        "visible": bool(user32.IsWindowVisible(root)), "rect": rect_of(root),
        "pid": pid_of(root), "parent_class": "", "hwnd": "0x%X" % root,
    }

    rows = []

    def cb(hwnd, _lp):
        parent = user32.GetParent(hwnd)
        rows.append({
            "class": class_of(hwnd),
            "title": title_of(hwnd),
            "visible": bool(user32.IsWindowVisible(hwnd)),
            "rect": rect_of(hwnd),
            "pid": pid_of(hwnd),
            "parent_class": class_of(parent) if parent else "",
            "hwnd": "0x%X" % hwnd,
        })
        return True

    # 显式传 WNDENUMPROC 实例，避免 ctypes 把回调当成 c_void_p 后丢失生命周期
    proc = WNDENUMPROC(cb)
    user32.EnumChildWindows.argtypes = [wintypes.HWND, WNDENUMPROC, wintypes.LPARAM]
    ok = user32.EnumChildWindows(root, proc, 0)
    note = "" if ok else "EnumChildWindows 返回 FALSE（部分子窗口可能未枚举）"
    return root_row, rows, note


def probe():
    root, rows, note = enumerate_tray()
    classes = ([root["class"]] if root else []) + [r["class"] for r in rows]
    result = {
        "probe": "g9-taskbar-probe/1.0",
        "mode": "live-readonly",
        "tray_root": root,
        "descendant_count": len(rows),
        "descendants": rows,
        "classification": classify(classes),
        "environment_note": "系统版本/TranslucentTB 档位由主理人另行核对，见 docs/G9-可行性结论.md",
        "limits": [
            "本探针只枚举窗口树，不注入、不写内存、不改窗口属性、不验证绘制消息",
            "窗口类名不能直接证明绘制路径：存在经典 HWND 不等于 WM_PAINT 会经过它",
            "XAML/Composition 视觉树内的文字与图标没有独立 HWND，本探针无法枚举",
        ],
        "note": note,
    }
    return result


def self_test():
    """有效测试：4 个正向用例 + 2 组反向验证（改坏判定表必须让结果变化）。"""
    checks = []

    def check(name, cond):
        checks.append((name, bool(cond)))

    check("classic 树判为 classic_hookable",
          classify(SAMPLE_CLASSIC)["verdict"] == "classic_hookable")
    check("xaml 树判为 xaml_only",
          classify(SAMPLE_XAML)["verdict"] == "xaml_only")
    check("混合树判为 mixed",
          classify(SAMPLE_MIXED)["verdict"] == "mixed")
    check("空树判为 unknown",
          classify(SAMPLE_EMPTY)["verdict"] == "unknown")
    check("classic 树能列出时钟窗口",
          "TrayClockWClass" in classify(SAMPLE_CLASSIC)["classic_clock_windows"])
    check("xaml 树能列出 XAML 窗口",
          len(classify(SAMPLE_XAML)["xaml_windows"]) >= 2)
    check("大小写不敏感",
          classify(["trayclockwclass"])["verdict"] == "classic_hookable")

    # 反向验证 ①：把 XAML 判定表清空，xaml_only 必须消失
    global XAML_MARKERS, CLASSIC_CLOCK_MARKERS
    saved_xaml = XAML_MARKERS
    XAML_MARKERS = ()
    broken_xaml = classify(SAMPLE_XAML)["verdict"]
    XAML_MARKERS = saved_xaml
    check("反向验证①：XAML 判定表改坏后 xaml_only 必须变化", broken_xaml != "xaml_only")

    # 反向验证 ②：把经典判定表清空，classic_hookable 必须消失
    saved_classic = CLASSIC_CLOCK_MARKERS
    CLASSIC_CLOCK_MARKERS = ()
    broken_classic = classify(SAMPLE_CLASSIC)["verdict"]
    CLASSIC_CLOCK_MARKERS = saved_classic
    check("反向验证②：经典判定表改坏后 classic_hookable 必须变化",
          broken_classic != "classic_hookable")

    failed = [name for name, ok in checks if not ok]
    print("G9 组① 探针自检：%d 项，通过 %d，失败 %d" % (
        len(checks), len(checks) - len(failed), len(failed)))
    for name, ok in checks:
        print("  [%s] %s" % ("PASS" if ok else "FAIL", name))
    if failed:
        print("\n反向验证说明：判定表被改坏时分类结果必须改变，否则测试锁不住接线。")
        return 1
    print("\n结论：分类逻辑与反向验证均通过（未接触任何系统 API）。")
    return 0


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
        return self_test()
    if len(sys.argv) > 1:
        print(__doc__)
        return 2
    print(json.dumps(probe(), ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
