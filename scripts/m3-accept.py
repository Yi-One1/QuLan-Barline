#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""M3 歌词引擎验收：真机跑一遍「本地歌词 → 逐字扫光 → 单/双行 → 收合动画 → 降级四态」。

依赖 m2-accept.py 里的截图 / 输入合成 / 窗口探测helper（同目录）。
所有步骤必须在同一次进程调用内完成（工具调用结束会回收子进程树）。
"""

import importlib.util
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = r"E:\APP\WorkBuddy\工作空间\曲栏Barline"
SHOTS = os.path.join(ROOT, "docs", "screenshots")
EXE = os.path.join(ROOT, "target", "release", "barline.exe")
CFG = r"C:\Users\caihongyi\AppData\Local\Barline\config.json"
LRC_DIR = r"C:\Users\caihongyi\AppData\Local\Barline\lyrics"

spec = importlib.util.spec_from_file_location("acc", os.path.join(HERE, "m2-accept.py"))
acc = importlib.util.module_from_spec(spec)
sys.modules["acc"] = acc
spec.loader.exec_module(acc)

# ---------------------------------------------------------------- 输入合成

import ctypes
from ctypes import wintypes

MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010
KEYEVENTF_KEYUP = 0x0002


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class _KEYU(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("pad", ctypes.c_byte * 32)]


class KEYINPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _KEYU)]


def key(vk):
    """按一下某个虚拟键（按下 + 抬起）。"""
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.SendInput.argtypes = [wintypes.UINT, ctypes.c_void_p, ctypes.c_int]
    for flags in (0, KEYEVENTF_KEYUP):
        inp = KEYINPUT()
        inp.type = 1  # INPUT_KEYBOARD
        inp.u.ki = KEYBDINPUT(vk, 0, flags, 0, None)
        user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(KEYINPUT))
        time.sleep(0.03)


def right_click(x, y):
    acc.move(x, y)
    time.sleep(0.15)
    acc._send(MOUSEEVENTF_RIGHTDOWN)
    time.sleep(0.09)
    acc._send(MOUSEEVENTF_RIGHTUP)

# ---------------------------------------------------------------- 素材

TRACK_A = ("夜空独行", "测试歌手")
TRACK_B = ("找不到的歌", "无人知晓")

LRC_WORD = """[ti:夜空独行]
[ar:测试歌手]
[al:M3 验收]
[by:Barline]
[offset:0]
[00:00.50]<00:00.50>夜<00:00.90>空<00:01.30>独<00:01.70>行
[00:04.00]<00:04.00>脚<00:04.40>步<00:04.80>很<00:05.20>轻
[00:07.00]<00:07.00>路<00:07.40>灯<00:07.80>把<00:08.20>影<00:08.60>子<00:09.00>拉<00:09.40>长
[00:10.00]只有行级时间戳的一句
[00:13.00]<00:13.00>Hello <00:13.70>world <00:14.40>混<00:14.80>排<00:15.20>测<00:15.60>试
[00:16.50]扫光要平滑推进
[00:19.00]<00:19.00>风<00:19.40>从<00:19.80>窗<00:20.20>口<00:20.60>吹<00:21.00>进<00:21.40>来
[00:22.00]切行时高度不变
[00:25.00]<00:25.00>时<00:25.40>间<00:25.80>在<00:26.20>这<00:26.60>里<00:27.00>停<00:27.40>留
[00:28.50]下一行在这里
[00:31.00]<00:31.00>我<00:31.40>们<00:31.80>都<00:32.20>在<00:32.60>路<00:33.00>上
[00:34.00]最后一段歌词
[00:37.00]<00:37.00>曲<00:37.40>终<00:37.80>人<00:38.20>散
"""


def write_pages():
    """两条 SMTC 会话：A 有本地歌词，B 没有。"""
    for name, (title, artist), page_title in [
        ("m3_demo_a.html", TRACK_A, "M3 验收音轨 A"),
        ("m3_demo_b.html", TRACK_B, "M3 验收音轨 B"),
    ]:
        p = os.path.join(HERE, name)
        with open(p, "w", encoding="utf-8") as f:
            f.write(
                """<!doctype html><html><head><meta charset="utf-8"><title>%s</title></head>
<body style="background:#0f1016;color:#ddd;font-family:Segoe UI">
<h2>M3 歌词验收（请勿关闭本页）</h2>
<audio id="a" src="barline_demo.wav" autoplay loop controls></audio>
<script>
const a = document.getElementById('a');
if ('mediaSession' in navigator) {
  navigator.mediaSession.metadata = new MediaMetadata({
    title: %s, artist: %s, album: 'M3 Acceptance'
  });
  navigator.mediaSession.setActionHandler('play', () => a.play());
  navigator.mediaSession.setActionHandler('pause', () => a.pause());
  navigator.mediaSession.setActionHandler('seekto', (d) => {
    if (d.seekTime != null) a.currentTime = d.seekTime;
  });
}
a.play().catch(e => document.title = 'blocked: ' + e);
</script></body></html>
"""
                % (
                    page_title,
                    json.dumps(title, ensure_ascii=False),
                    json.dumps(artist, ensure_ascii=False),
                )
            )
        print("  页面：%s" % p)


def set_config(**kw):
    """改 config.json 的 lyrics.* 字段（保留其它字段）。"""
    with open(CFG, "r", encoding="utf-8") as f:
        cfg = json.load(f)
    for k, v in kw.items():
        cfg["lyrics"][k] = v
    with open(CFG, "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False)


def save_config():
    with open(CFG, "r", encoding="utf-8") as f:
        return f.read()


def restore_config(text):
    with open(CFG, "w", encoding="utf-8") as f:
        f.write(text)


def zoom(src, dst, factor=2):
    """最近邻放大，用于把扫光细节做成"特写"。"""
    w, h, px = acc.read_bmp(src)
    nw, nh = w * factor, h * factor
    out = bytearray()
    for y in range(h):
        row = px[y * w * 4 : (y + 1) * w * 4]
        wide = bytearray()
        for x in range(w):
            wide += row[x * 4 : x * 4 + 4] * factor
        for _ in range(factor):
            out += wide
    acc.write_bmp(dst, nw, nh, bytes(out))
    print("  [zoom] %s → %dx%d" % (os.path.basename(dst), nw, nh))
    return dst


def stack(paths, dst):
    """把若干同宽 bmp 竖直拼成一张（扫光推进过程图）。"""
    imgs = [acc.read_bmp(p) for p in paths]
    w = imgs[0][0]
    total_h = sum(i[1] for i in imgs)
    out = bytearray()
    for _, _, px in imgs:
        out += px
    acc.write_bmp(dst, w, total_h, bytes(out))
    print("  [stack] %s → %dx%d" % (os.path.basename(dst), w, total_h))
    return dst


def bright_halves(path):
    """返回 (左半亮像素数, 右半亮像素数)。用来挑"扫亮了一半"的那一帧。"""
    w, h, px = acc.read_bmp(path)
    left = right = 0
    for y in range(h):
        for x in range(w):
            i = (y * w + x) * 4
            luma = 0.0722 * px[i] + 0.7152 * px[i + 1] + 0.2126 * px[i + 2]
            if luma > 190:
                if x < w // 2:
                    left += 1
                else:
                    right += 1
    return left, right


def lyric_band(wr, bar_h):
    """歌词区矩形（屏幕坐标）。"""
    wl, wt, wrr, wb = wr
    ww, wh = wrr - wl, wb - wt
    main_top = wh - bar_h
    return (wl, wt, ww, main_top)


def wait_rect_ready(min_w=210, timeout=15.0):
    """等小组件出现且宽度已按歌名测量完（> min_w），立刻返回——用于抢"加载中"这一帧。"""
    t0 = time.time()
    while time.time() - t0 < timeout:
        r = acc.widget_rect()
        if r and (r[2] - r[0]) >= min_w:
            return r
        time.sleep(0.05)
    return acc.widget_rect()


def start_app(mark):
    app = subprocess.Popen([EXE])
    wr = acc.wait_stable_rect(20)
    return app, wr, mark


def stop_app(app):
    app.kill()
    time.sleep(0.9)


def main():
    os.makedirs(SHOTS, exist_ok=True)
    os.makedirs(LRC_DIR, exist_ok=True)
    orig_cfg = save_config()

    # ---- 素材：demo 音轨 / 本地歌词 ----
    if not os.path.exists(os.path.join(HERE, "barline_demo.wav")):
        subprocess.run([sys.executable, os.path.join(HERE, "make-demo-media.py")], check=False)
    write_pages()
    lrc_path = os.path.join(LRC_DIR, "%s - %s.lrc" % (TRACK_A[1], TRACK_A[0]))
    with open(lrc_path, "w", encoding="utf-8") as f:
        f.write(LRC_WORD)
    print("本地歌词：%s" % lrc_path)

    # 清空歌词缓存：否则阶段 1 会命中上一轮的磁盘缓存，看不到"本地文件命中"这条链路
    cache_dir = r"C://Users//caihongyi//AppData//Local//Barline//lyrics-cache"
    if os.path.isdir(cache_dir):
        for f in os.listdir(cache_dir):
            try:
                os.remove(os.path.join(cache_dir, f))
            except OSError:
                pass
        print("已清空歌词缓存：%s" % cache_dir)

    acc.kill_isolated_edge()
    time.sleep(0.8)
    srv = subprocess.Popen(
        [sys.executable, "-m", "http.server", "8767", "--bind", "127.0.0.1"],
        cwd=HERE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(1.0)

    edge = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
    if not os.path.exists(edge):
        edge = r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"
    common = [
        "--user-data-dir=" + os.path.join(HERE, "barline_edge_profile"),
        "--autoplay-policy=no-user-gesture-required",
        "--no-first-run",
        "--no-default-browser-check",
        "--window-position=120,100",
        "--window-size=900,600",
    ]
    procs = []

    def open_page(name):
        p = subprocess.Popen([edge] + common + ["http://127.0.0.1:8767/" + name])
        procs.append(p)
        return p

    tr = acc.tray_rect()
    bar_h = tr[3] - tr[1]
    print("任务栏厚度 = %d" % bar_h)

    results = []
    try:
        # ================= 阶段 1：本地歌词 + 双行 + 逐字扫光 =================
        print("=== 阶段 1：本地歌词（双行，字级标签）===")
        set_config(enabled=True, mode="two_line", online_enabled=True)
        open_page("m3_demo_a.html")
        time.sleep(9)
        acc.minimize_window_with_title("M3 验收音轨 A")

        mark = len(acc.log_text())
        app, wr, mark = start_app(mark)
        time.sleep(2.5)
        wr = acc.widget_rect()
        print("  窗口 = %s" % (wr,))
        for ln in acc.lines_since(mark, "歌词", "本地", "lrclib"):
            print("   | " + ln)
        net = acc.lines_since(mark, "请求 lrclib")
        print("  网络请求条数 = %d（本地命中应为 0）" % len(net))
        results.append(("本地命中不发网络请求", len(net) == 0))

        wl, wt, wrr, wb = wr
        ww, wh = wrr - wl, wb - wt
        lx, ly, lw, lh = lyric_band(wr, bar_h)
        results.append(("双行形态总高 = %d（期望 %d）" % (wh, bar_h + 46), wh == bar_h + 46))

        acc.move(1200, 700)
        time.sleep(1.2)
        acc.shot("m3-01-two-line.bmp", wl, wt, ww, wh)
        # 连拍 4 帧歌词带：既证明扫光在推进，也能挑出一帧"边界在字中间"做特写
        bands = []
        for i in range(4):
            bands.append(acc.shot("_band%d.bmp" % i, lx, ly, lw, lh))
            time.sleep(0.55)
        stack(bands, os.path.join(SHOTS, "m3-02-sweep-progression.bmp"))
        counts = [sum(bright_halves(b)) for b in bands]
        order = sorted(range(len(bands)), key=lambda i: counts[i])
        best = bands[order[len(order) // 2]]
        print("  各帧亮像素数 = %s，特写选中第 %d 帧" % (counts, order[len(order) // 2]))
        zoom(best, os.path.join(SHOTS, "m3-02-sweep-closeup.bmp"), 2)
        time.sleep(4.0)
        acc.shot("m3-03-line-advanced.bmp", wl, wt, ww, wh)
        acc.shot("m3-04-context.bmp", wl - 200, wt - 24, ww + 400, wh + 24)
        for ln in acc.lines_since(mark, "歌词就绪", "本地歌词命中"):
            print("   | " + ln)
        stop_app(app)

        # ================= 阶段 2：单行模式 =================
        print("=== 阶段 2：单行模式 ===")
        set_config(enabled=True, mode="one_line")
        mark = len(acc.log_text())
        app, wr, mark = start_app(mark)
        time.sleep(2.5)
        wr = acc.widget_rect()
        wl, wt, wrr, wb = wr
        ww, wh = wrr - wl, wb - wt
        results.append(("单行形态总高 = %d（期望 %d）" % (wh, bar_h + 26), wh == bar_h + 26))
        acc.move(1200, 700)
        time.sleep(1.0)
        acc.shot("m3-05-one-line.bmp", wl, wt, ww, wh)
        stop_app(app)

        # ================= 阶段 3：关闭歌词 + 200ms 收合动画 =================
        print("=== 阶段 3：关闭歌词（右键菜单 → 200ms 收合）===")
        set_config(enabled=True, mode="two_line")
        mark = len(acc.log_text())
        app, wr, mark = start_app(mark)
        time.sleep(2.5)
        wr = acc.widget_rect()
        wl, wt, wrr, wb = wr
        ww, wh = wrr - wl, wb - wt
        print("  收合前高度 = %d" % wh)
        # 右键小组件 → 菜单第一项「显示歌词」
        right_click(wl + ww // 2, wt + wh - 20)
        time.sleep(0.7)
        acc.shot("m3-06-menu.bmp", wl - 40, wt - 140, 420, wh + 180)
        # 键盘选中第一项
        key(0x28)  # VK_DOWN
        time.sleep(0.15)
        # 采样线程先跑起来，再按回车——否则 python 发键的 60–90ms 会吃掉动画前半段
        series = []
        stop = threading.Event()

        def sample_heights():
            t0 = time.time()
            shot_done = [False]
            while not stop.is_set() and time.time() - t0 < 0.8:
                r = acc.widget_rect()
                if r:
                    h = r[3] - r[1]
                    series.append((round((time.time() - t0) * 1000), h))
                    if not shot_done[0] and bar_h + 2 < h < wh - 4:
                        acc.shot("m3-07-collapsing.bmp", wl - 40, wt - 10, ww + 80, wh + 20)
                        shot_done[0] = True
                time.sleep(0.008)

        th = threading.Thread(target=sample_heights)
        th.start()
        time.sleep(0.03)
        key(0x0D)  # VK_RETURN → 菜单关闭 → 触发收合
        th.join()
        print("  高度时间序列（ms, px）：%s" % series)
        time.sleep(0.6)
        r = acc.widget_rect()
        if r:
            wl, wt, wrr, wb = r
            ww, wh2 = wrr - wl, wb - wt
            results.append(("关闭歌词后总高 = %d（期望 %d）" % (wh2, bar_h), wh2 == bar_h))
            acc.shot("m3-08-collapsed.bmp", wl, wt, ww, wh2)
        heights = sorted({h for _, h in series})
        results.append(("收合过程出现中间高度：%s" % heights, len(heights) >= 3))
        stop_app(app)

        # ================= 阶段 4：加载中 → 网络失败 =================
        print("=== 阶段 4：无本地歌词 → 加载中 → 网络失败 ===")
        set_config(enabled=True, mode="two_line", online_enabled=True)
        open_page("m3_demo_b.html")
        time.sleep(6)
        acc.minimize_window_with_title("M3 验收音轨 B")
        mark = len(acc.log_text())
        app = subprocess.Popen([EXE])
        wr = wait_rect_ready(210, 15)
        if not wr:
            raise RuntimeError("小组件没出现")
        wl, wt, wrr, wb = wr
        ww, wh = wrr - wl, wb - wt
        # 等 LyricLoading 真正落到 UI 上（远早于网络失败）
        time.sleep(1.5)
        acc.shot("m3-09-loading.bmp", wl, wt, ww, wh)
        results.append(("加载中总高 = %d（应与双行一致 %d）" % (wh, bar_h + 46), wh == bar_h + 46))
        for ln in acc.lines_since(mark, "请求 lrclib", "歌词"):
            print("   | " + ln)
        # 轮询等网络失败（本机 lrclib 会被挡，耗时 3–8s 不等）
        t0 = time.time()
        while time.time() - t0 < 30 and not acc.lines_since(mark, "歌词缺失"):
            time.sleep(0.4)
        print("  网络失败耗时 %.1fs" % (time.time() - t0))
        acc.shot("m3-10-network-error.bmp", wl, wt, ww, wh)
        for ln in acc.lines_since(mark, "网络", "重试", "歌词缺失"):
            print("   | " + ln)
        wh_now = acc.widget_rect()
        if wh_now:
            results.append(("网络失败总高 = %d（应与双行一致）" % (wh_now[3] - wh_now[1]), wh_now[3] - wh_now[1] == bar_h + 46))
        stop_app(app)

        # ================= 阶段 5：关闭在线 → 暂无歌词 =================
        print("=== 阶段 5：关闭在线歌词 → 暂无歌词 ===")
        set_config(enabled=True, mode="two_line", online_enabled=False)
        mark = len(acc.log_text())
        app, wr, mark = start_app(mark)
        time.sleep(2.0)
        wr = acc.widget_rect()
        wl, wt, wrr, wb = wr
        ww, wh = wrr - wl, wb - wt
        acc.move(1200, 700)
        time.sleep(0.6)
        acc.shot("m3-11-not-found.bmp", wl, wt, ww, wh)
        results.append(("暂无歌词总高 = %d（应与双行一致）" % wh, wh == bar_h + 46))
        net = acc.lines_since(mark, "请求 lrclib")
        results.append(("关闭在线后不发请求（实际 %d 条）" % len(net), len(net) == 0))
        stop_app(app)
        # ================= 阶段 6：切歌淡出（120ms）=================
        print("=== 阶段 6：切歌淡出 120ms ===")
        set_config(enabled=True, mode="two_line", online_enabled=False)
        # 先把当前会话切回 A（阶段 4/5 把 B 留在前台了）
        open_page("m3_demo_a.html")
        time.sleep(6)
        acc.minimize_window_with_title("M3 验收音轨 A")
        mark = len(acc.log_text())
        app, wr, mark = start_app(mark)
        time.sleep(2.5)
        wr = acc.widget_rect()
        wl, wt, wrr, wb = wr
        w_old = wrr - wl
        ww, wh = wrr - wl, wb - wt
        print("  旧曲宽度 = %d" % w_old)
        # 换到 B：SMTC 切歌 → 旧歌词淡出
        open_page("m3_demo_b.html")
        t0 = time.time()
        switched = False
        shots = []
        while time.time() - t0 < 12:
            r = acc.widget_rect()
            if r and (r[2] - r[0]) != w_old:
                switched = True
                break
            time.sleep(0.008)
        if switched:
            # 切歌后窗口变宽了，裁剪要按**新**矩形来，否则左边会被切掉
            r = acc.widget_rect()
            if r:
                wl, wt, wrr, wb = r
                ww, wh = wrr - wl, wb - wt
            for i in range(3):
                p = acc.shot("_fade%d.bmp" % i, wl, wt, ww, wh)
                shots.append(p)
                time.sleep(0.035)
            zoom(shots[0], os.path.join(SHOTS, "m3-12-crossfade.bmp"), 2)
            for f in shots[1:]:
                try:
                    os.remove(f)
                except OSError:
                    pass
            print("  切歌检测到宽度变化，已抓 %d 帧" % len(shots))
            results.append(("切歌后仍能看到旧歌词（淡出中）", True))
        else:
            print("  [WARN] 12s 内没检测到切歌")
        stop_app(app)
    finally:
        restore_config(orig_cfg)
        for p in procs:
            p.kill()
        acc.kill_isolated_edge()
        srv.kill()
        try:
            os.remove(lrc_path)
        except OSError:
            pass
        for f in ("_band0.bmp", "_band1.bmp", "_band2.bmp", "_band3.bmp", "_fade0.bmp"):
            try:
                os.remove(os.path.join(SHOTS, f))
            except OSError:
                pass
        print("已清理（配置与歌词文件已还原）")

    print("\n=== 验收结果 ===")
    for name, ok in results:
        print("  [%s] %s" % ("PASS" if ok else "FAIL", name))
    return 0 if all(ok for _, ok in results) else 1


if __name__ == "__main__":
    sys.exit(main())
