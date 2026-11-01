#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""P1 验收：lrclib 真实端到端请求（超时是否够、状态码 / 字节数 / 耗时）。

用法：python scripts\\m4-lrclib-check.py
"""

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
CACHE = r"C:\Users\caihongyi\AppData\Local\Barline\lyrics-cache"

spec = importlib.util.spec_from_file_location("acc", os.path.join(HERE, "m2-accept.py"))
acc = importlib.util.module_from_spec(spec)
sys.modules["acc"] = acc
spec.loader.exec_module(acc)

TITLE = "Yellow"
ARTIST = "Coldplay"


def main():
    page = os.path.join(HERE, "m4_demo_lrclib.html")
    with open(page, "w", encoding="utf-8") as f:
        f.write(
            """<!doctype html><html><head><meta charset="utf-8"><title>M4 lrclib 验收</title></head>
<body style="background:#101014;color:#ddd;font-family:Segoe UI">
<h2>lrclib 在线歌词验收（请勿关闭）</h2>
<audio id="a" src="barline_demo.wav" autoplay loop controls></audio>
<script>
const a = document.getElementById('a');
if ('mediaSession' in navigator) {
  navigator.mediaSession.metadata = new MediaMetadata({title: %s, artist: %s, album: 'Parachutes'});
}
a.play().catch(e => document.title = 'blocked: ' + e);
</script></body></html>
"""
            % (json.dumps(TITLE, ensure_ascii=False), json.dumps(ARTIST, ensure_ascii=False))
        )

    # 清掉这首歌可能存在的缓存，保证真的联网
    if os.path.isdir(CACHE):
        for name in os.listdir(CACHE):
            try:
                os.remove(os.path.join(CACHE, name))
            except OSError:
                pass
        print("已清空歌词缓存")

    with open(CFG, "r", encoding="utf-8") as f:
        cfg = json.load(f)
    cfg["lyrics"]["online_enabled"] = True
    with open(CFG, "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False)

    acc.kill_isolated_edge()
    time.sleep(0.8)
    srv = subprocess.Popen(
        [sys.executable, "-m", "http.server", "8769", "--bind", "127.0.0.1"],
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
            "http://127.0.0.1:8769/m4_demo_lrclib.html",
        ]
    )
    time.sleep(9)
    acc.minimize_window_with_title("M4 lrclib 验收")

    mark = len(acc.log_text())
    app = subprocess.Popen([EXE])
    print("等待歌词查询（最多 40s，lrclib 本机实测 6.6s/次）…")
    t0 = time.time()
    while time.time() - t0 < 40:
        if acc.lines_since(mark, "lrclib 响应", "歌词就绪", "歌词缺失"):
            break
        time.sleep(0.5)
    time.sleep(0.5)

    print("--- 日志 ---")
    for ln in acc.lines_since(mark, "lrclib", "歌词"):
        print("   " + ln)

    app.kill()
    time.sleep(0.6)
    ep.kill()
    acc.kill_isolated_edge()
    srv.kill()
    print("done")
    return 0


if __name__ == "__main__":
    sys.exit(main())
