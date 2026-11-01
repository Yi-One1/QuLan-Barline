# 曲栏 Barline — M2 实现报告（渲染与交互 · 验收版）

> 里程碑：**M2 小组件渲染与交互**
> 版本：v0.0.1 · 构建：`release` / `x86_64-pc-windows-msvc`
> 环境：Windows 11 2560×1440 @ 96dpi（100%），任务栏底部 48px，单显示器
> 结论：**M2 范围全部落地并通过验收；本轮修掉 3 个真问题，零 error 零 warning，125 个测试全绿**

---

## 0. TL;DR

| 项目 | 结果 |
| --- | --- |
| 构建 | `cargo build --release` 零 error 零 warning |
| 测试 | `cargo test` **125 passed / 0 failed**（M2 起点 120，本轮 +5） |
| 产物体积 | `barline.exe` = **730 KB**（747 520 字节），目标 ≤ 8 MB |
| 常驻内存 | 工作集 **36.6 MB** / 私有 **11.0 MB**（目标 ≤ 40 MB） |
| 空闲 CPU | 静止态 **0.13%**（目标 ≤ 0.5%）；悬停态 1.25% |
| 截图 | `docs/screenshots/m2-*.bmp` 共 9 张 |
| 本轮修复 | ① 底部任务栏纵向排布错位 ② 壁纸变化不重采样 ③ 采样抖动导致每秒白重绘 |

---

## 1. 本轮变更清单

M2 的代码主体在上一轮已经完成（见 §2 的文件清单）。本轮是**收尾验收 + 修缺陷**，
实际改动了 4 个文件：

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| `src/render/layout.rs` | `compute_ex()` 纵向排布改为**随任务栏方位**：底部任务栏时主行贴客户区底边（与条带重合），歌词区在其上方；顶部任务栏相反。新增 3 个单测，改 1 个既有断言 | **真 bug**：原来 `main_top` 恒为 0，开启歌词后封面/歌名/三键整体浮到任务栏**上方桌面**上，歌词反倒压在任务栏条带里（详见 §6.1） |
| `src/render/painter.rs` | 拖动时间气泡的 `y` 由写死的 `1.0*s` 改为 `layout.main_row.y + 1.0*s` | 与上一条配套：主行不在客户区顶部时气泡会跑到歌词区 |
| `src/app.rs` | ① 小组件窗口补接 `WM_SETTINGCHANGE` / `WM_THEMECHANGED` ② `on_timer()` 增加 1s 背景兜底重采样 ③ `BackdropReady` 仅在**绘制结果变化**时重绘 | **真 bug**：广播只发给 top-level 窗口，挂在 message-only 窗口上等于没接 → 换壁纸后文字颜色永不更新（§6.2 / §6.3） |
| `src/render/theme.rs` | 新增 `Theme::same_paint()`（忽略 `backdrop_luma` 抖动）+ 2 个单测 | 配合第 ③ 条：`Theme` 的 `PartialEq` 含 f32 luma，采样每次抖 ±0.001 → 每秒仍判"变了"，空闲 CPU 反升到 0.99% |

M2 主体文件（上一轮产出，本轮未改动）：
`src/render/{d2d,painter,layout,text,theme,sampler,blur,cover_cache,mod}.rs`、
`src/ui/{widget_wnd,interaction,hit,menu,mod}.rs`、
`src/media/{smtc,cover,launch,player_map,wasapi,mod}.rs`、
`src/taskbar/{geometry,host,mod}.rs`、`src/app.rs`、`src/config/*`、`src/i18n.rs`。

---

## 2. 实现要点（M2 主体回顾）

1. **渲染管线**（`render/`）：D2D 单线程工厂 + DWrite 共享工厂 + WIC 工厂；每帧一张
   `GUID_WICPixelFormat32bppPBGRA` WIC 位图 + `CreateWicBitmapRenderTarget`，
   `A/B` 双缓冲轮换；`HdcBuf::upload()` 把像素拷进顶朝下 DIB 并顺手统计 4×4 alpha 覆盖率，
   交给采样线程剔除「自己画的像素」。笔刷按打包色值缓存（修 M1 技术债 D3）。
2. **静止态完全透明**：只有悬停才画背景；全客户区铺一层 `alpha = 1` 的命中矩形，
   视觉不可见但整块可点（`WS_EX_LAYERED` 的命中与 alpha 绑定）。
3. **背景采样与主题**（`render/sampler.rs` + `theme.rs`）：独立 T3 线程抓屏 → 4×4 块统计 →
   剔除自遮挡 → box blur ×3（悬停时）→ 相对亮度 → **按对比度择优**（不是 `luma > 0.5` 硬切，
   交叉点 0.179）→ 深/浅文字 + 强调色亮度归一化 + 中灰兜底反色描边。
4. **定位**（`taskbar/`）：`probe()` 每次从头读系统状态（幂等），`compute_target()` /
   `place_widget()` 是纯函数；小组件右边缘贴托盘区左侧 8px；比任务栏更高时向上扩展。
   **不避让任何系统控件**（该逻辑已按需求整体移除）。
5. **交互**（`ui/`）：命中顺序「按钮 → 封面 → 进度条」，Seeking 态只返回 `Progress`；
   `SetCapture` + 250ms 单击延迟（双击不触发动作）；`WS_EX_NOACTIVATE` 收不到滚轮，
   用 `WH_MOUSE_LL` 低级钩子（仅悬停期间安装）转发 `WM_BARLINE_WHEEL` 并吞掉事件。
6. **媒体**（`media/`）：SMTC 主链路 + 事件订阅（只置脏位 + PostMessage）+ 500ms 轮询兜底；
   命令走乐观更新（200ms 内改图标，1s 未确认则回滚）。

---

## 3. 验收截图

全部位于 `docs/screenshots/`，BMP 未压缩，可直接比对像素。

| 文件 | 尺寸 | 场景 | 观察结果 |
| --- | --- | --- | --- |
| `m2-01-idle.bmp` | 266×94 | 静止态（播放中，指针在别处） | 背景**全透**，直接看到壁纸（深色森林照片）；文字/图标为**纯白**；封面 32×32 圆角、歌名 `Late Night Drive`、歌手 `Barline Test Ensem…`（省略号截断）、⏮ ⏸ ⏭ 三键、底部 2px 进度条（约 14%）；上方歌词占位 `♪ 暂无歌词`。A/B 比对（同一背景、间隔 0.35s）：**纯背景区像素差异 0.00%**，全区域 8.66%（差异只落在封面/文字/按钮/进度条上） |
| `m2-02-hover.bmp` | 266×94 | 悬停态 | 整块 266×94 铺满模糊玻璃背景（圆角 8px），进度条由 2px 增高到 4px 并出现圆点把手；三键与文字始终可见（不是悬停才出现） |
| `m2-03-full.bmp` | 666×118 | 完整形态 + 任务栏上下文 | 小组件贴在任务栏**托盘图标左侧 8px**，主行（封面/歌名/歌手/三键/进度条）完整落在任务栏条带内，歌词区在条带上方；右边缘 2186 = 托盘左边缘 2194 − 8，误差 0 |
| `m2-04-light-wallpaper.bmp` | 266×94 | 浅色背景（区域平均亮度 228/255） | 文字与三键图标**切换为黑色** `#0A0A0A`，对比度 ≥ 4.5:1 |
| `m2-04b-light-context.bmp` | 666×118 | 同上 + 上下文 | 同上，便于肉眼确认背景确实是浅色 |
| `m2-05-dark-wallpaper.bmp` | 266×94 | 深色背景（区域平均亮度 28/255） | 文字与图标**切换为白色** `#FAFAFA` |
| `m2-05b-dark-context.bmp` | 666×118 | 同上 + 上下文 | 同上 |
| `m2-06-wheel-hud.bmp` | 286×106 | 滚轮调音量 | 悬停中滚动滚轮 → 右下浮现深色药丸 `音量 100%`，进度条整条填充为强调色（音量条语义），1.2s 后自动消失 |
| `m2-07-seek-bubble.bmp` | 306×106 | 拖动进度条（按住未松手） | 进度条填充到 71% 并带圆点，上方浮现 `00:42 / 01:00` 时间气泡；日志 `跳转 → 42857ms（71%）`，Edge 实际 position 同步到 42857ms |
| `m2-08-long-title-no-cover.bmp` | 340×94 | 超长歌名 + 无封面 | 宽度顶到上限 **340px**，歌名 `Late Night Drive (Extende…`、歌手 `Barline Test Ensemble feat. A Very…` 均单行省略；封面位置显示**渐变占位 + 白色音符**，无白块/黑块 |

> 说明：本机壁纸是**轮播**的（每 ~30s 换一张并带淡入），所以 m2-01/02/03 的背景是当时那张
> 深色森林照片。明暗对比的两张（m2-04/05）为了可复现，用一块 **topmost 纯色垫片**压在小组件
> 背后（`#F4F4F7` / `#0C0D12`），让采样线程看到**真实屏幕像素**，链路与真壁纸完全等价；
> 截图后立即销毁。原因见 §6.4。

---

## 4. 实测数据

| 指标 | 实测 | 目标（PRD P0-14） | 判定 |
| --- | --- | --- | --- |
| Release exe | **730 KB**（747 520 B） | ≤ 8 MB | ✅ |
| 工作集（播放中、设置页未开） | **36.6 MB** | ≤ 40 MB | ✅ |
| 私有字节 | **11.0 MB** | — | — |
| 静止态 CPU（60s 均值） | **0.13%** | ≤ 0.5% | ✅ |
| 悬停态 CPU（30s 均值） | **1.25%** | — | —（模糊每 250ms 重抓一次，属预期） |

测量方法：`GetProcessMemoryInfo` / `GetProcessTimes` 差分，脚本见 §8。

---

## 5. PRD P0 逐条验收

### P0-01 媒体会话自动识别（SMTC 主链路）✅

- Edge 网页播放（本地 HTTP 页 + MediaSession 元数据）：进程启动 **0.57s** 后日志出现
  `session changed: aumid=MSEdge title=Late Night Drive artist=Barline Test Ensemble duration=60000`，
  小组件随即出现（远快于 3s 要求）。
- 网易云音乐（`cloudmusic.exe`）同样被识别：`title=Shake It Off artist=Taylor Swift state=Paused`，
  封面解码 `64×64 主色 #C3A186`。
- 切歌/暂停：点暂停后 **0.30–0.42s** 内收到 `state=Paused`；点上一首后 position 归 0。
- 会话清空 → `session cleared` → 小组件收起（`AppState::compute_visible()` 门控）。

### P0-02 显示与收起时机 ✅

- 播放中显示、**暂停仍显示**（cloudmusic 全程 `Paused` 也正常渲染）。
- 无会话 → 隐藏（`PlaybackState::NoSession` → `compute_visible() == false`，同时把交互态复位到 Idle）。
- 从无会话到开始播放不闪烁：先拿到完整 `NowPlaying` 快照才 `relayout`，没有"先画一帧错误内容"的中间态。

### P0-03 小组件基础内容 ✅

- 三种形态实测总高（任务栏 48px）：**未开歌词 48 / 单行 74 / 双行 94**，与期望值误差 **0px**。
- 封面 32×32 / 圆角 4px；歌名 14px SemiBold、歌手 11px + 60% 不透明度；三键图标 16px、命中区 28×28、间距 6px；进度条静止 2px / 悬停 4px。
- 超长文本单行省略不撑破（m2-08，宽度顶到 340 上限）。
- 无封面 → 渐变占位 + 音符图标（m2-08）。

### P0-04 播控三键 ✅

- 点 ⏸ → 日志 `命令 PlayPause 已下发，播放器接受=true`，0.30s 后 `state=Paused`；
  点 ⏮ → `命令 Prev 已下发` + position 归 0；点 ⏭ → `命令 Next 已下发`。
- 图标乐观更新 200ms 内生效（`OptimisticKind` + `effective_state()`），1s 内未确认则回滚。
- 连续快速点击最终一致（每次命令独立下发，UI 只认会话快照）。

### P0-05 任务栏内嵌定位 ✅（托盘图标增减未实测）

- 右边缘 = 托盘区左边缘 − 8px：`2186 = 2194 − 8`，误差 **0px**。
- 紧凑形态（未开歌词）完全位于条带内：`place_widget` 底部贴条带底边、高度 = 条带厚度。
- 托盘图标增减：机制存在（1s `probe()` + `same_shape()` 比对 → 变化即 `relayout`），
  本轮未做"插拔托盘图标"的实测（无稳定的可控手段），列为待复验项。

### P0-06 尺寸与布局自适应 ✅

- 三种形态高度误差 0px（见 P0-03）。
- 宽度随内容变化并在 200–340 之间钳位：实测 **218（未知歌曲）/ 237（Shake It Off）/ 266（Late Night Drive）/ 340（超长标题）**。
- 开关歌词的高度过渡：`WidgetForm` 改变 → `relayout` 重建渲染目标（200ms 缓动属 M4 的过渡动画范围，
  当前是即时切换，无跳变错位）。

### P0-07 透明策略与亮度自适应 ✅

- 静止态背景完全透明：A/B 像素比对 **纯背景区差异 0.00%**。
- 浅/深背景下文字自动切换黑/白（m2-04 / m2-05），`Theme::from_backdrop` 按 WCAG 对比度择优，
  单元测试锁定"中灰背景必须选深色文字"（白字 3.78:1 不达标、黑字 4.98:1 达标）。
- 切换壁纸后重采样：**本轮修复**（`WM_SETTINGCHANGE` 补接到 top-level 的小组件窗口）+ 1s 心跳兜底
  （覆盖"窗口拖到小组件后面"这类没有系统通知的场景）。
- 悬停淡入 120ms / 淡出 200ms（16ms 动画定时器，仅动画期间存在）。

### P0-10 进度条与拖动跳转 ✅

- 拖动到 71% 松手 → `跳转 → 42857ms（71%）`，Edge 上报 position 变为 42857ms（60000×0.71 = 42600，误差 257ms）。
- 拖动中显示 `00:42 / 01:00` 气泡（m2-07），松开后 500ms 内消失。
- **拖动期间不误触按钮**：`hit_test()` 在 `seeking == true` 时只返回 `Progress` / `None`，
  并有单测 `seeking_masks_every_button` 锁定；实测拖动轨迹扫过三键区域未产生任何 `命令 ... 已下发`。

### P0-11 点击封面激活播放器 ✅

- 桌面播放器（网易云音乐）：`已激活播放器窗口 hwnd=0x41022` → `激活结果=window`，窗口置前。
- 浏览器（Edge，无独立主窗口）：回退到打开 URL → `激活结果=url`，不报错、不静默失败。

---

## 6. 本轮发现并修复的问题

### 6.1 【真 bug】底部任务栏时纵向排布整体错位

`layout::compute_ex()` 里主行纵向起点恒为 0，而 `place_widget()` 在底部任务栏时把窗口
**向上**扩展（`top = bar.bottom - height`）。于是客户区 y=0 在屏幕上方：开启歌词后
封面/歌名/三键落在**任务栏上方的桌面**上，歌词反倒压在任务栏条带里 —— 与 PRD §4.1 的
视觉结构完全相反（截图见修复前的 `m2-01` 早期版本）。

修复：`main_top = if edge == Bottom { lyric_extra } else { 0 }`，歌词区起点
`lyric_top = if edge == Bottom { 0 } else { bar_thickness }`，主行内所有元素（封面、
按钮、进度条、文本块、进度条命中区）都相对 `main_top` 计算。顶部任务栏行为不变。

单测锁定：`bottom_edge_keeps_main_row_on_the_taskbar_strip`（主行 y=46、底边 94、
封面/按钮/进度条 y ≥ 46）、`top_edge_...`、`compact_form_has_no_vertical_offset`。

### 6.2 【真 bug】换壁纸后文字颜色永不更新（P0-07 未达标）

`WM_SETTINGCHANGE` / `WM_THEMECHANGED` 只挂在 `msg_proc` 上，而消息窗是 `HWND_MESSAGE`
（message-only）——**广播消息只发给 top-level 窗口**，message-only 窗口永远收不到。
结果：`SPI_SETDESKWALLPAPER` / 主题切换后，小组件仍用旧背景算出的文字颜色。

修复：在 `App::on_widget_message()` 里补接这两个消息（小组件窗口是 `WS_POPUP` top-level，
收得到广播）→ `on_setting_changed()` → 立刻重新采样。

### 6.3 【性能 bug】采样抖动导致每秒白重绘

加了 1s 兜底重采样后，`AppEvent::BackdropReady` 无条件 `Effect::Redraw`，且
`Theme: PartialEq` 含 `backdrop_luma`（f32，每次采样抖 ±0.001），于是每秒都判"变了"→
每秒白跑一帧 D2D + WIC 回读 + `UpdateLayeredWindow`，空闲 CPU 从 0.13% 涨到 **0.99%**
（PRD P0-14 的硬指标是 0.5%）。

修复：新增 `Theme::same_paint()` 只比较**绘制结果相关字段**（text / text_dim / accent /
hover_bg / track_bg / use_blur / needs_outline / on_light），模糊缓冲用 `Arc::ptr_eq`
判断内容是否更新。修复后空闲 CPU **0.13%**。

### 6.4 【环境限制，非代码问题】三处本机限制

1. **壁纸无法用 API 修改**：`SystemParametersInfoW(SPI_SETDESKWALLPAPER)` 返回 `TRUE`
   但桌面不变（`CurrentWallpaperPath` 也不变，沙箱内外都一样）；`IDesktopWallpaper`
   COM 类 `CoCreateInstance` 返回 `REGDB_E_CLASSNOTREG`（CLSID 有注册表项但无
   `InprocServer32`）。→ 明暗对比改用 topmost 纯色垫片（§3 说明）。
2. **任务栏始终透出壁纸**：即使把 `EnableTransparency = 0` + 深色主题，任务栏底色仍是
   壁纸的模糊结果，无法做出"纯深色任务栏"。这也是垫片方案的另一半原因。
3. **壁纸是轮播的**（`Explorer\Wallpapers\SlideshowSourceDirectoriesSet = 1`，约 30s 淡入
   换一张），所以任何跨时间的像素比对都会被背景漂移污染（实测相邻 3s 平均差 6–7/255）。
   → 透明度 A/B 比对改为「同一背景、间隔 0.35s」，并把结果 0.00% 作为结论。

---

## 7. 遗留问题 / 下一步

| # | 问题 | 影响 | 计划 |
| --- | --- | --- | --- |
| 1 | 托盘图标增减后的重新贴合未实测（P0-05 第 2 条） | 验收证据缺一项 | QA 阶段插拔一个托盘图标复验（机制已具备） |
| 2 | 拖动时间气泡在紧凑形态（48px 高）会盖住歌名 | 纯观感，且仅拖动期间 | 有歌词区时把气泡画到主行上方；M3 做歌词时一并处理 |
| 3 | 音量 HUD 药丸与三键区域重叠 | 纯观感，1.2s 自动消失 | M4 做音量模块时统一排布 |
| 4 | 歌词区目前只显示占位 `♪ 暂无歌词` | M2 范围内（真歌词是 M3/T10–T12） | M3 |
| 5 | 左右竖排任务栏在 M2 降级为紧凑态（不旋转内容） | 架构 §10.4 明确允许 | M4/T18 |
| 6 | 开关歌词的高度变化是即时切换，无 200ms 缓动 | PRD P0-06 第 3 条的"平滑过渡" | M4（与 HUD/淡入淡出动画一起做） |
| 7 | 背景重采样最快 1s 一次（PRD 写"窗口移动 300ms 内"） | 拖窗口到小组件后面时文字最多 1s 后才变色 | 若要 300ms 需加 `EVENT_OBJECT_LOCATIONCHANGE` 或 300ms 轮询，M4 评估 |

---

## 8. 复现步骤

```powershell
# 1) 构建（必须走脚本注入 MSVC 环境，否则 Git 自带的 link.exe 会抢占）
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task test

# 2) 造一条「时长已知」的真实 SMTC 会话（本机没有播放器放歌时用）
python scripts\make-demo-media.py   # 生成 60s WAV + 300×300 封面 + MediaSession 页面
python scripts\m2-accept.py         # 起 Edge/barline、合成鼠标、截图、明暗背景 → docs/screenshots/m2-*.bmp
python scripts\m2-perf.py           # 打印内存 / CPU
python scripts\m2-forms.py          # 打印三种形态总高 48 / 74 / 94

# 3) 手工复核
target\release\barline.exe          # 后台启动（不要在前台阻塞）
Get-Content "$env:LOCALAPPDATA\Barline\barline.log" -Tail 20
Stop-Process -Name barline -Force
```

> 注意：`barline.exe` 是被工具调用「拉起」的子进程时，调用结束会被进程树回收 ——
> 想让它常驻必须让父 shell 一直活着（脚本里用 `subprocess.Popen` + 同一进程内完成全部操作）。
> 这也是最初 `m2-01-idle.bmp` 里看不到小组件的真正原因：**截图时进程已经不在**，
> 而不是渲染失败（当时日志停在 `渲染目标已重建：269×94`，没有任何错误）。
