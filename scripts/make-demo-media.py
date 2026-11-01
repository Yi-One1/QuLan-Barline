#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""验收辅助：造一个「有真实时长」的媒体会话。

1. 生成 60s 的 WAV（轻音量琶音，避免打扰）；
2. 生成 300×300 PNG 封面（无第三方库，手写 PNG）；
3. 写一个 HTML 页面，用 <audio> 播放并设置 MediaSession 元数据；
4. 用 Edge 打开它 → 系统出现一条 duration 已知的 SMTC 会话。
"""

import math
import os
import struct
import sys
import zlib

OUT = os.path.dirname(os.path.abspath(__file__))
WAV = os.path.join(OUT, "barline_demo.wav")
PNG = os.path.join(OUT, "barline_demo.png")
HTML = os.path.join(OUT, "barline_demo.html")

DURATION_S = 60
RATE = 22050


def gen_wav(path):
    n = DURATION_S * RATE
    frames = bytearray()
    # 简单的 A 小调琶音：A3 C4 E4 A4，每音 0.5s，弱音量
    notes = [220.00, 261.63, 329.63, 440.00]
    for i in range(n):
        t = i / RATE
        idx = int(t / 0.5) % len(notes)
        f = notes[idx]
        env = 1.0 - (t % 0.5) / 0.5
        v = 0.22 * env * math.sin(2 * math.pi * f * t)
        frames += struct.pack("<h", int(max(-1.0, min(1.0, v)) * 32767))
    with open(path, "wb") as fp:
        data = bytes(frames)
        fp.write(b"RIFF")
        fp.write(struct.pack("<I", 36 + len(data)))
        fp.write(b"WAVEfmt ")
        fp.write(struct.pack("<IHHIIHH", 16, 1, 1, RATE, RATE * 2, 2, 16))
        fp.write(b"data")
        fp.write(struct.pack("<I", len(data)))
        fp.write(data)


def gen_png(path, size=300):
    rows = bytearray()
    for y in range(size):
        rows.append(0)  # filter type 0
        for x in range(size):
            u = x / (size - 1)
            v = y / (size - 1)
            r = int(255 * (0.25 + 0.55 * u))
            g = int(255 * (0.15 + 0.45 * (1.0 - v) * u + 0.20 * v))
            b = int(255 * (0.55 + 0.40 * (1.0 - u)))
            # 右下角画一个浅色圆，模拟唱片
            cx, cy = 0.72 * size, 0.30 * size
            d = math.hypot(x - cx, y - cy)
            if d < size * 0.26:
                k = 1.0 - d / (size * 0.26)
                r = int(min(255, r + 90 * k))
                g = int(min(255, g + 70 * k))
                b = int(min(255, b + 40 * k))
            rows += bytes((r, g, b))

    def chunk(tag, payload):
        out = struct.pack(">I", len(payload)) + tag + payload
        return out + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as fp:
        fp.write(png)


def gen_html(path):
    with open(path, "w", encoding="utf-8") as fp:
        fp.write(
            """<!doctype html>
<html><head><meta charset="utf-8"><title>Barline 验收音轨</title></head>
<body style="background:#101014;color:#ddd;font-family:Segoe UI">
<h2>Barline 验收音轨（请勿关闭本页）</h2>
<audio id="a" src="barline_demo.wav" autoplay loop controls></audio>
<script>
const a = document.getElementById('a');
if ('mediaSession' in navigator) {
  navigator.mediaSession.metadata = new MediaMetadata({
    title: 'Late Night Drive',
    artist: 'Barline Test Ensemble',
    album: 'M2 Acceptance',
    artwork: [
      { src: 'barline_demo.png', sizes: '300x300', type: 'image/png' }
    ]
  });
  navigator.mediaSession.setActionHandler('play', () => a.play());
  navigator.mediaSession.setActionHandler('pause', () => a.pause());
  navigator.mediaSession.setActionHandler('previoustrack', () => { a.currentTime = 0; });
  navigator.mediaSession.setActionHandler('nexttrack', () => { a.currentTime = 0; });
  navigator.mediaSession.setActionHandler('seekto', (d) => {
    if (d.seekTime != null) a.currentTime = d.seekTime;
  });
}
a.play().catch(e => document.title = 'autoplay blocked: ' + e);
</script>
</body></html>
"""
        )


if __name__ == "__main__":
    gen_wav(WAV)
    gen_png(PNG)
    gen_html(HTML)
    print(WAV)
    print(PNG)
    print(HTML)
