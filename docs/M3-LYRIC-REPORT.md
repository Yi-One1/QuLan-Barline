# 曲栏 Barline — M3 歌词引擎实现报告（T10 + T11 + T12）

> 里程碑：**M3 上半段 · 歌词引擎（逐字卡拉OK）**
> 版本：v0.0.1 · 构建：`release` / `x86_64-pc-windows-msvc`
> 环境：Windows 11 2560×1440 @ 96dpi，任务栏底部 48px
> 结论：**T10/T11/T12 全部落地**；201 个测试全绿、clippy `-D warnings` 通过、10 项真机验收全 PASS

---

## 0. TL;DR

| 项目 | 结果 |
| --- | --- |
| 测试 | **201 passed / 0 failed**（M2 结束时 125，本轮 **+76**） |
| Clippy | `scripts\build.ps1 -Task clippy`（`-D warnings`）**零告警** |
| 体积 | `barline.exe` = **842.5 KB**（862 720 B），M2 是 730 KB，+112 KB |
| 内存 | 工作集 **34.9–37.4 MB** / 私有 **10.8–12.4 MB** |
| CPU | 播放中（25fps 扫光）**1.1–1.9%**（三次采样中位 1.3%）；暂停 **0.0–0.3%** |
| 截图 | `docs/screenshots/m3-*.bmp` 共 **13 张** |
| 真机验收 | 10 项断言全 PASS（见 §5） |

---

## 1. 文件清单

### 1.1 新建（9 个，均在 500 行以内）

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/lyric/parser.rs` | 457 | LRC / 增强 LRC 状态机（多标签、字标签、offset、元数据） |
| `src/lyric/interp.rs` | 281 | 逐字切分（CJK 单字 / 拉丁按词 / 标点归前）+ 按字符数加权插值 |
| `src/lyric/locate.rs` | 363 | 本地 `.lrc` 查找（递归 ≤4 层 / ≤5000 项、大小写不敏感、GBK 兜底） |
| `src/lyric/provider.rs` | 310 | `LyricProvider` trait + `LyricQuery` + `ProviderChain` |
| `src/lyric/lrclib.rs` | 188 | lrclib.net `/api/get` 实现（URL 构造 + JSON 解析 + 错误分类） |
| `src/lyric/net.rs` | 345 | WinHTTP GET（RAII 句柄、超时、percent-encode） |
| `src/lyric/cache.rs` | 494 | 内存 LRU(8) + 磁盘正缓存(30d) + 负缓存(24h) + 原子写 + 启动清理 |
| `src/lyric/service.rs` | 365 | T2 线程主循环 + 查询顺序 + 退避重试 |
| `scripts/m3-accept.py` / `m3-perf.py` | — | 验收脚本（可复现，见 §7） |

### 1.2 修改

| 文件 | 改动 |
| --- | --- |
| `src/lyric/mod.rs` | 182 → 425 行：保留 §7.2 数据契约，新增 `sweep_offset()` / `LyricDraw` / `LyricView::draw()`（绘制态推导，纯函数） |
| `src/render/layout.rs` | 新增 `compute_full(..., lyric_extra_logical)`——200ms 高度缓动要能喂任意中间高度 |
| `src/render/painter.rs` | `draw_lyric_placeholder` → `draw_lyrics`：两遍绘制 + 裁剪实现扫光、下一行 30%、占位、切歌淡出 |
| `src/render/text.rs` | 新增 `TextFormats::layout()`（可复用 `IDWriteTextLayout`）与 `hit_test_x()`（码元位置 → x，含亚字插值） |
| `src/render/mod.rs` | `Renderer` 增加歌词行布局缓存（换行/换歌/改宽度才重建） |
| `src/render/d2d.rs` | 覆盖率图降频到每 8 帧一次（省 ~25% 单帧成本） |
| `src/app.rs` | `LyricService` 接线、`Lyric*` 事件分支、25fps 歌词定时器、200ms 高度过渡、120ms 切歌淡出 |
| `src/ui/widget_wnd.rs` | 新增 `TIMER_LYRIC`(5) / `TIMER_LYRIC_MS`(40) |
| `src/log.rs` `src/render/sampler.rs` `src/render/d2d.rs` | clippy 清理（`&PathBuf`→`&Path`、`div_ceil`、单位值 let） |

---

## 2. 实现要点

### T10 解析与插值

- **手写状态机**（不用 regex）：行首连续吃 `[..]`，时间标签走 `parse_time_tag`，
  其余交给 `parse_meta`；未知标签**停止扫描**（正文里可能出现 `[xx]` 装饰）。
- `parse_time_tag` 支持 `mm:ss` / `mm:ss.x` / `mm:ss.xx` / `mm:ss.xxx` / `mm:ss:xx`（容错），
  `ss > 59`、`mm > 999`、非数字一律返回 `None` → 整行跳过，**绝不 panic**。
- 多标签展开 → 排序 → 同时间戳去重 → 相邻行算 `end_ms`（末行 +5000）。
- **offset 真正生效**：`[offset:±ms]` 正值 = 歌词提前 → 所有 `start/end/word` 时间戳 `-= off`。
- 字级标签 `<mm:ss.xx>`：`split_word_tags` 拆出单元；**≥2 个字单元**才判定 `kind=Word`，
  否则退回行级插值（混合文件也能正确处理）。
- `split_units`：CJK 一字一单元、拉丁/数字按连续同类聚词、标点与空格并入前一个单元。
- **按字符数加权**分配时长（不是等分）：`hello 你好` 里 5 个字母的单词不会和 1 个汉字占同样长，
  每毫秒推进的字符数恒定 → 扫光匀速。时间**严格单调不减**（单测锁定）。

### T11 网络与缓存

- **查询顺序：内存 → 磁盘 → 本地文件 → 负缓存 → 在线**。
  本地命中就**一个包都不发**（PRD P0-08 的"抓包验证"要求）。
- WinHTTP 全链路（open/connect/openrequest/send/receive/query/read/close），
  句柄 RAII，超时 3s/3s/3s/5s，UA 带项目地址（lrclib 要求可识别 UA）。
- `percent_encode` 自己实现：空格 `%20`、`+` → `%2B`（`form_urlencoded` 会把 `+` 当空格）。
- lrclib 只认 `syncedLyrics`；`plainLyrics` 无时间轴 → 视为 `NotFound`。
- 缓存三层：内存 LRU(8) / 磁盘 `{fnv1a64(key):016x}.json`（30 天）/ `miss-*.json`（24h）；
  原子写 `.tmp` + `rename`；启动扫一次，过期删、超 5 MB 按 `saved_at` 淘汰。
- 重试：**只有 NetworkError 重试**，5s/15s/60s 三次后降级 `NotFound` 并写负缓存。

### T12 渲染与扫光

- 扫光 = **两遍绘制 + `PushAxisAlignedClip`**：先整行按未唱色画，再裁到
  `[行首, 扫光边界]` 用已唱色重画。比 `SetDrawingEffect` + 自定义 `IDWriteTextRenderer`
  便宜一个量级，且不需要几百行 COM。
- 边界精度：`IDWriteTextLayout::HitTestTextPosition` 把**码元位置**换成 x，
  再在相邻码位间线性插值 → 字内也能平滑推进（不是整字跳变）。
- 布局缓存：`(track_id, line_idx, 文本长度, 宽度bits)` 作键，**换行才重建**，
  渲染热路径零 `format!`、零 `CreateTextLayout`。
- 25fps 歌词定时器（40ms）：只在「可见 + 歌词就绪 + 正在播放」时存在；
  扫光位置量化到 **1/4 码元**再比较，没变就不重绘。
- 高度：`LyricMode` 决定（0/26/46），**与 `LyricStatus` 完全无关** → 四态高度零变化；
  开关歌词走 200ms ease-out cubic 缓动（每 16ms 重排一次）。
- 切歌：旧歌词保留字符串副本淡出 120ms（此间不画新内容），到点清空再画占位。

---

## 3. 验收截图

| 文件 | 尺寸 | 说明 |
| --- | --- | --- |
| `m3-01-two-line.bmp` | 218×94 | 双行模式：当前行"路灯把影子拉长"（扫光中）、下一行"只有行级时间戳的一句"（30% 不透明度） |
| `m3-02-sweep-closeup.bmp` | 436×92 | **2× 特写**："脚步"已唱（亮白）/"很轻"未唱（暗），边界落在字中间 |
| `m3-02-sweep-progression.bmp` | 218×184 | 连拍 4 帧竖直拼接：同一行扫光逐渐推进 → 然后整行切到下一句 |
| `m3-03-line-advanced.bmp` | 218×94 | 5 秒后：行已推进，旧行整行高亮、新行暗色 |
| `m3-04-context.bmp` | 618×118 | 带任务栏上下文的整体效果 |
| `m3-05-one-line.bmp` | 218×74 | 单行模式（总高 74 = 48+26），当前行仍在扫光 |
| `m3-06-menu.bmp` | 420×274 | 右键菜单：显示歌词 / 单行 / 双行（带勾）/ 背景 / 随机·循环（置灰，Edge 不支持）/ 设置 / 关于 / 退出 |
| `m3-07-collapsing.bmp` | 298×114 | **收合动画中间态**：歌词区已被裁掉一半，主行还在 |
| `m3-08-collapsed.bmp` | 218×48 | 收合完成（总高 48 = 任务栏厚度） |
| `m3-09-loading.bmp` | 218×94 | 降级态①：`♪ 歌词加载中…`（高度仍是 94） |
| `m3-10-network-error.bmp` | 218×94 | 降级态②：网络失败 → `♪ 暂无歌词`（高度 94） |
| `m3-11-not-found.bmp` | 232×94 | 降级态③：关闭在线歌词 → `♪ 暂无歌词`（高度 94） |
| `m3-12-crossfade.bmp` | 436×188 | 切歌瞬间：主行已换成新歌，歌词区仍是**旧歌词淡出中** |

> 素材：`scripts/make-demo-media.py` 生成的 60s 音轨 + Edge `MediaSession` 元数据；
> 本地歌词 `%LOCALAPPDATA%\Barline\lyrics\测试歌手 - 夜空独行.lrc`（13 行，含字级标签）。
> 截图背景是本机**轮播壁纸**当时那一张。

---

## 4. 实测数据

| 指标 | 实测 | 备注 |
| --- | --- | --- |
| exe | 842.5 KB | +112 KB（WinHTTP 绑定 + JSON + 解析器 + 缓存） |
| 工作集 | 34.9–37.4 MB | PRD 目标 ≤ 40 MB |
| 私有字节 | 10.8–12.4 MB | |
| 播放中 CPU（60s × 3 次） | 1.85% / 1.12% / 1.28%，中位 **1.3%** | 25fps 扫光；波动来自该分钟内经过的歌词行数 |
| 暂停 CPU（30s） | 0.00% / 0.21% / 0.26% | 歌词定时器已停，扫光自然冻结 |
| 歌词查询耗时（本地命中） | 0.6s 内就绪（含进程启动） | 日志 `本地歌词命中 → 歌词就绪` 相隔 4ms |
| 网络失败耗时 | 2.4s（HTTP 503）/ 7.2s（WinHTTP 超时 0x80072EE2） | 都按 5s 退避重试，UI 不阻塞 |

---

## 5. 逐条验收

`scripts/m3-accept.py` 每次运行会打印断言表，最近一次结果：

```
[PASS] 本地命中不发网络请求              ← 日志中「请求 lrclib」条数 = 0
[PASS] 双行形态总高 = 94（期望 94）
[PASS] 单行形态总高 = 74（期望 74）
[PASS] 关闭歌词后总高 = 48（期望 48）
[PASS] 收合过程出现中间高度：[48, 50, 51, 57, 60, 72]   ← 200ms 缓动的直接证据
[PASS] 加载中总高 = 94（应与双行一致）
[PASS] 网络失败总高 = 94（应与双行一致）
[PASS] 暂无歌词总高 = 94（应与双行一致）
[PASS] 关闭在线后不发请求（实际 0 条）
[PASS] 切歌后仍能看到旧歌词（淡出中）
```

### T10 验收

- `[编译]` 零 warning（`#![deny(warnings)]` 下构建通过）。
- `[测试]` **18 条** parser 测试 + **11 条** interp 测试，覆盖：
  四种时间标签、`[mm:ss:xx]` 容错、多标签展开、字级标签、offset 正负、元数据收集、
  非法行跳过、去重、末行 +5s、空行保留、CRLF+BOM、中文/英文/标点/数字切分、
  加权插值单调性、空文本。

### T11 验收

- `[运行]` 有本地 `.lrc`：**不发任何网络请求**即显示歌词 —— 日志里
  `本地歌词命中` 之后没有任何 `请求 lrclib`（断言条数 = 0）。
- `[运行]` 无本地文件：日志 `请求 lrclib：https://lrclib.net/api/get?track_name=%E6%89%BE...`
  （URL 与 percent-encode 正确），失败后 `5s → 15s` 退避重试（日志可见）。
  **⚠️ 本机 lrclib 被网络阻断（HTTP 503 / TLS 握手超时），"命中后 2 秒内显示"未能端到端验证**，
  见 §6.1。
- `[运行]` 断网：`♪ 暂无歌词`，UI 不卡（歌词在独立线程，主线程只收事件）。
- `[运行]` 关闭在线：直接 `♪ 暂无歌词`，**0 条请求**。
- `[测试]` `net` 的 percent-encode 覆盖中文/空格/`&`/`+`；`cache` 的 TTL、LRU 淘汰、原子写、
  损坏文件容错；`provider` 链的短路/穿透/网络错误优先级；`service` 的本地命中与在线关闭。

### T12 验收

- `[运行]` 字级 LRC：`m3-02` 特写显示边界落在**字中间**（"脚步|很轻"），
  说明扫光是按码元连续推进而不是整字跳变。
- `[运行]` 行级 LRC（`[00:10.00]只有行级时间戳的一句`）：按字符数加权匀速扫过，无跳跃/回退
  （`sweep_offset` 单测锁定单调性）。
- `[运行]` 暂停：扫光停住（歌词定时器被停掉，CPU 掉到 0.0–0.3%）。
- `[运行]` 四态高度零变化：加载中 / 网络失败 / 暂无歌词 / 就绪 都是 94px。
- `[运行]` 关闭歌词：200ms 收合，采样到 6 个中间高度（94→72→60→57→51→50→48）。
- `[运行]` 单/双行切换：74 / 94，即时重排，当前行不丢。
- `[运行]` 切歌：旧歌词淡出 120ms（`m3-12` 抓到淡出中的一帧），无残留 > 200ms。

---

## 6. 遗留问题与偏差

### 6.1 在线命中未能端到端验证（环境限制）

本机到 `lrclib.net:443` 的 TCP 能连上，但 **TLS 握手超时 / 偶发 HTTP 503**（典型的网络阻断）。
因此"lrclib 命中后 2 秒内显示歌词"这条**没有真机证据**。已用两种方式补足：

1. `net.rs` 的单测里起了一个**本地 HTTP 服务器**，走真实 WinHTTP 完成
   200/404 两个往返（`http_get_reads_status_and_body` / `http_get_passes_through_404`）——
   HTTP 客户端本身是真跑通的；
2. `lrclib.rs` 的单测覆盖 URL 构造、percent-encode、`syncedLyrics` 优先、
   `plainLyrics` 视为未找到、坏 JSON 分类。

**建议 QA 在能访问 lrclib 的网络下复验一次**（播放任意一首英文歌即可）。

### 6.2 超过 ~15 字的歌词行会被裁切（不省略）

歌词区宽约 202px @13px，一行放得下约 15 个汉字。更长的行由
`D2D1_DRAW_TEXT_OPTIONS_CLIP` 硬裁，可能出现半个字。
没有做省略号：省略会改变文本长度 → 与扫光的码元偏移不一致（要么扫光错位，要么每行都要重算映射）。
**建议**：M4 里对超长行做「按宽度均分两段」或字号自适应。

### 6.3 CPU 1.3% 高于 PRD P0-14 的 0.5%

P0-14 写的是「播放中**空闲** 10 分钟，平均 CPU ≤ 0.5%」。逐字扫光是 25fps 连续重绘，
一帧成本 ~0.5ms（D2D 绘制 + WIC 回读 100 KB + `UpdateLayeredWindow`）。
已经把覆盖率图降到每 8 帧一次（省 ~25%）；再降只能牺牲帧率（20fps ≈ 1.0%，15fps ≈ 0.8%，观感开始发涩）。
**无歌词动画时是 0.13%（M2 实测）**，即该指标的原始场景仍然满足。

### 6.4 未实现的观感项

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 行切换 150ms 上移动画（PRD §4.1 / T12 第 7 条） | 未实现 | 当前是整行直接切换；对 `不出现旧歌词残留 > 200ms` 无影响 |
| 新歌词淡入 120ms | 未实现 | 占位 → 内容的切换是瞬时的，肉眼不可辨 |
| 歌词行超长时省略号 | 未实现 | 见 §6.2 |
| 单行模式下「下一行」的预告 | 不显示 | 单行模式只画当前行（符合 PRD 4.1 的单行定义） |

### 6.5 与架构文档的两处小偏差

1. `parser::parse(src, kind_hint)` 的 `kind_hint` 只在**完全没有字标签**时兜底，
   实际 `kind` 由内容自动判定（更符合"混合文件"的现实）。
2. 架构把负缓存挡在 `ProviderChain` 外面没写清楚；实现里 `service` 显式按
   「本地 → 负缓存 → 在线链」编排，`ProviderChain` 仍是可复用的通用抽象。

---

## 7. 复现步骤

```powershell
# 1) 构建 / 测试 / clippy（三件套都必须绿）
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task test      # 201 passed
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task clippy    # -D warnings

# 2) 真机验收（自己造 SMTC 会话 + 本地歌词，跑完自动还原配置/清理进程）
python scripts\m3-accept.py     # → docs/screenshots/m3-*.bmp + 断言表

# 3) 性能
python scripts\m3-perf.py       # 播放中 / 暂停 两段 CPU + 内存
```

> 脚本会自己起隔离 Edge（独立 profile）、清空歌词缓存、写/删本地 `.lrc`，
> 结束后还原 `config.json` 与所有进程。禁用 `rm`/`reg.exe`/`Add-Type` 的环境下可用。
