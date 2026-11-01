#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""P0-03 / P0-06 补充验收：三种歌词形态下的总高（未开 / 单行 / 双行）。"""

import importlib.util
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = r"E:\APP\WorkBuddy\工作空间\曲栏Barline"
EXE = os.path.join(ROOT, "target", "release", "barline.exe")
CFG = r"C:\Users\caihongyi\AppData\Local\Barline\config.json"

spec = importlib.util.spec_from_file_location("acc", os.path.join(HERE, "m2_accept.py"))
acc = importlib.util.module_from_spec(spec)
sys.modules["acc"] = acc
spec.loader.exec_module(acc)


def run_form(enabled, mode):
    with open(CFG, "r", encoding="utf-8") as f:
        cfg = json.load(f)
    cfg["lyrics"]["enabled"] = enabled
    cfg["lyrics"]["mode"] = mode
    with open(CFG, "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False)
    app = subprocess.Popen([EXE])
    wr = acc.wait_stable_rect(20)
    time.sleep(1.2)
    wr = acc.widget_rect()
    app.kill()
    time.sleep(0.7)
    if not wr:
        return None
    w = wr[2] - wr[0]
    h = wr[3] - wr[1]
    return w, h


def main():
    with open(CFG, "r", encoding="utf-8") as f:
        orig = f.read()
    tr = acc.tray_rect()
    bar_h = tr[3] - tr[1]
    print("任务栏厚度 = %d" % bar_h)
    try:
        for enabled, mode, label, expect in [
            (False, "two_line", "未开歌词", bar_h),
            (True, "one_line", "单行歌词", bar_h + 26),
            (True, "two_line", "双行歌词", bar_h + 46),
        ]:
            r = run_form(enabled, mode)
            if r:
                print(
                    "%-8s 宽=%d 高=%d  期望高=%d  误差=%d"
                    % (label, r[0], r[1], expect, r[1] - expect)
                )
            else:
                print("%-8s 窗口未出现" % label)
    finally:
        with open(CFG, "w", encoding="utf-8") as f:
            f.write(orig)
        print("配置已还原")
    return 0


if __name__ == "__main__":
    sys.exit(main())
