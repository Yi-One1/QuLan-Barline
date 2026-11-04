---
description: 曲栏 Barline 项目长期记忆总纲：构建/版本/交付纪律、架构不变量（含 v1.4 歌词并发、SMTC 分层轮询/事件/锚点、v1.5.6 G3.5-A 全屏遮挡）、已验证的踩坑清单、当前留档待办。接手前只需读这一份即可恢复上下文。
tags: [曲栏Barline, 项目总纲, 架构不变量, 构建纪律, 踩坑, SMTC分层轮询, 卡顿专档, 全屏遮挡]
category: 曲栏Barline
---

# 曲栏 Barline 项目长期记忆

Windows 任务栏媒体小组件：Rust 1.99.1(MSVC) + `windows` 0.62.2 + Direct2D/DWM/SMTC + WebView2。
当前版本 **v1.6.1**（G4 悬停反馈 + 验收反馈修复，**已出包待真人验收**），测试 **472 项**（471 passed + 1 ignored 联网），clippy 0 警告。仓库 `https://github.com/Yi-One1/QuLan-Barline.git`（main）。G1 歌词提速、G2 卡顿缓解（专档 `progress.md §3.1`）、G3 全链（v1.5.0~v1.5.4）、**G3.5 全屏遮挡（v1.5.6 方案 A）** 均已闭环；**G4 v1.6.0 验收报 2 项 → v1.6.1 已交付待验收**；下一组 G5 歌词落盘；G7 黑名单（v1.8.0）PRD/ARCH 已入库待实现。

## 铁律（违反必踩坑）
- 构建只走 `scripts/build.ps1 -Task check|clippy|test|build`（用 PowerShell 工具跑），**禁止裸 cargo**（Git Bash 抢 link.exe）。
- 测试汇总要直跑 `target/<profile>/deps/barline-*.exe`——`windows_subsystem` 会吞 stdout。
- `deliverables/` 的 exe **只增不删**；每次改动 bump 版本（**5 处**：Cargo.toml 1 + barline.rc 4）并出包；`Cargo.lock` 会由 cargo 自动同步 1 行，一并提交。
- 版本策略（用户定）：改可见行为/新增能力 → 升第二位；纯修正/文案/内部重构 → 升第三位。
- 禁用 `reg.exe` / `Add-Type` / `cmd.exe`；工程师不启动 exe、不截屏、不动 deliverables、不 commit（主理人统一提交）。
- 节奏（用户定）：一次一组 → 全绿测试 → 单提交 → 出包 → 用户真人验收 → 才更新文档并进下一组。
- **多成员协作时，解除代码冻结必须同步告知所有在跑成员并给新的源码 SHA256 指纹**，否则整轮验证作废（v1.5.6 教训）。

## 架构不变量
- 小组件高度**恒定 48px**；信息区宽 = max(歌名+歌手自然宽, 160px)，上限 340px。
- `platform::data_dir()` 是唯一数据路径入口；`config.json` / `.tmp` **永远留默认根**且删净时豁免。
- 背景：模糊已**永久删除**；`Theme::apply_bg_config` 是两态背景唯一写入口；透明态**必须携带 on_light 基色**。
- 亮度判定（无 EMA）：死区 + 连续 2 次确认 + 大幅跳变快速通道 + 反向冷却 2 帧。
- **歌词链（v1.4.0）**：内存→磁盘→本地→负缓存→**在线并发**（lrclib+网易云抢答，8s 硬截止）；换歌抢占（独立线程 + TrackId 代次守卫）；重试最多 1 次/3s、超时不重试；缓存键 `cache_title_key` 摘平台后缀与首尾装饰但**版本标记不摘**；`MIN_PICK_SCORE=60`、时长差 >10s 淘汰。
- **SMTC 轮询（v1.4.2/1.4.3，卡顿的根）**：**禁止全量轮询**（14 次/秒会拖卡网易云）；分层 500/1000/2000ms + 网易云 (0,0)×3 退避 5s；**主通道=事件**（MediaPropertiesChanged 等三事件→置脏位→60ms 下限 forced；回调只许置 AtomicBool）。
- **位置锚点 `position_at`**（歌词同步命门）：读了时间线→读取时刻；没读但切歌→检测时刻；其余沿用上一份。绝不许用陈旧 `timeline_at`（网易云恒报 (0,0)）。
- **G3.5-A 全屏遮挡（v1.5.6）**：曲栏是 top-level popup（**非任务栏子窗口**），全屏盖不住的根因是 `WS_EX_TOPMOST` + 每秒 `ensure_on_top()`。纯通知型 appbar（只 `ABM_NEW/ABM_REMOVE`、**不发 `ABM_QUERYPOS/SETPOS`、不占工作区**）注册在 **widget 主窗口**（`TaskbarCreated` 是广播，`HWND_MESSAGE` 收不到）；`ABM_REMOVE` 必须在 `host.destroy()` 之前。`ABN_FULLSCREENAPP` 事件驱动 → `HWND_BOTTOM`（**禁用 `HWND_NOTOPMOST`**）→ 退出 `HWND_TOPMOST`。**永不 `SW_HIDE`**：`frozen` 只降 Z 序 + 静止降载，`compute_visible()` 不含 frozen；`z_demoted` 双份状态幂等、只在 `SetWindowPos` 成功后翻转；退出须**先清 `frozen/z_demoted` 逻辑门控再恢复 Z 序**。1s 心跳只作兜底对账。`occlusion_want(AppbarOpen) = !(fs && !same_monitor(fg,wm))`（信任事件、仅明确异屏还原）。**坑：`||` 短路使 `fs=false` 时 monitor 完全不参与判定 → 「monitor 全 0 不冻结」安全方向在事件路径被放宽，只有 `fs=true` 路径 100% 保住；v1.5.7 应对 `fg/wm == RECT::default()` 回退 `frozen_decision`**。
- **G4 悬停反馈（v1.6.1）**：四按钮（Prev/Toggle/Next/Dismiss）共用 `ButtonVisual` + `glow_params` + `glow_colors` + `draw_button_feedback`；`glow_params(hovering,pressed,hover_bg.a)` **禁止加 `HitTarget` 参数 / `match target`**（四 target 同值的结构性保证）；Q1 保底 `GLOW_FLOOR_ALPHA=0x06` 是曲线下限截距（透明档恒 `(P,P)`、无态仍 0），不是 × 专属底色；**悬停标称 `0x42/0x1C`、按下 `0x60/0x2E`**（默认 70% 档实测 18%/7.8%、26.3%/12.5%）；按下 = 外圈收紧 0.5 + 下沉 `1.0*s` + 图标 94%（所有内部偏移同乘 k）；封面恒暗蒙层 + 恒亮双箭头（**不用 `theme.text`**），朝向由 `COVER_ARROW_ORIENTATION` 派生；旧 `DISMISS_BG_*` 符号保留（layout 契约）但不得进绘制路径。所有视觉参数集中在 `limits`。
- **重绘判据（v1.6.1 教训，重要）**：`on_down`/`on_up`/`on_capture_lost` 改变 `pressed` 后**返回空 `Vec<WidgetInput>`**，而 `Interaction::animating()` 不含 `pressed` → 必须在 `App::dispatch()` 里用 `interaction_needs_redraw(prev_hover, prev_pressed, state)` 判断，**两条分支（空输入早退 + 正常 reduce）都要用**，`prev_*` 必须在 `sync_interaction()` **之前**捕获。`frame_hash` 含 `pressed`（`app.rs:2524`），补的帧不会被去重吃掉。**凡是不产生 `WidgetInput` 的交互态变化都必须显式进 dispatch 判据**，否则就是「状态变了但画面没变」。`src/ui/interaction.rs` 不得为此改动（重绘决策属 App 层）。
- **松手帧依赖两条互补路径**：`WM_LBUTTONUP` 里 `ReleaseCapture()` 同步触发嵌套 `WM_CAPTURECHANGED` → `on_capture_lost` → `dispatch(vec![])` 落地松手帧，外层 `dispatch` 的 `prev_pressed=Some→None` 兜底。**依赖 `on_capture_lost` 不清 `pending`** —— 若将来加「清 pending」，单击会失效，必须同步补测。真机「偶尔单击没反应」优先查这条。
- 热键：`HOTKEYS_VERSION=2`，默认 `Ctrl+Alt+P/←/→/L/M/H`。单实例 `Local\Barline.SingleInstance.v1`。
- 性能埋点（仍在）：`perf::Span`，默认 50ms / `smtc.*` 8ms 才记 `[perf]`；统计行每 60s 一条。

## 已验证的坑
- `AppState::geometry_epoch` 每秒都变，是"探测序号"；稳定标识用 `area_epoch`。
- `SW_SHOW` 对最小化窗口是 no-op → 必须 `IsIconic` 后 `SW_RESTORE`。
- 日志会 `relocate` 到自定义 data_dir（本机 `E:\APP\曲栏Barline\数据目录`）。
- lrclib 空 `artist_name` 直接 HTTP 400 → 空 artist 不拼参数、改走搜索。
- 抗抖不要叠三层（延迟相乘）；CEF/Electron"卡死"必须行为验证。
- git worktree 检出 `.ps1` 触发转码失败；重建历史包用 detached checkout。
- **网易云 SMTC 恒报 `(duration=0, position=0)`**：进度条/时长对它不可用，歌词钟靠锚点外推。
- windows-rs 0.62 SMTC 事件：EventArgs 名不带会话前缀；反注册是 PascalCase `Remove*`。
- 卡顿类问题方法学：埋点（慢才记）→ 用户两阶段对照 → 日志按时间点对齐 → 只对"共同动作"定罪。
- **`HWND_NOTOPMOST` ≠ 降到底**：它只是「置于所有非 topmost 窗口之上」，对 F11 / 无边框全屏游戏仍会盖上去；顶层窗口之间也无 Z 序跟随，只有 `SetParent` 子窗口才跟随任务栏。
- `git grep` **不含 untracked 文件**（新增文件里的测试会漏数）；`#[ignore]` 注释字面串会误计 → 计数用 ripgrep 全量 + `--list`。
- `check` 遇 `target/debug/incremental` 陈旧锁文件告警用 `$env:CARGO_INCREMENTAL="0"` 跑同一条 build.ps1，别删缓存；测试 exe 直跑需 Git Bash 管道取 stdout。
- 睡眠时长必须超过被测周期，否则测试是橡皮图章。

## 留档待办（不阻塞）
- **G2 残余卡顿**：低频、间隔大，用户接受；续查路线与二分杠杆全在 `progress.md §3.1`（埋点仍在，别重搭）。
- 待办 A：会话音量（禁用中，入口 `media/wasapi.rs::session_simple_volume`）。
- 待办 D：汽水音乐托盘态激活（搁置：外部 ShowWindow 让 CEF 输入挂起）。
- 小尾巴：`hotkeys.reset1` 死词条；`SettingsService::is_open()` 零调用；`event.rs:201` "300ms 去抖"注释失实（实为同步写盘）；「冲突一键换组」未做。
- **v1.5.6 观察项（QA 最终报告，均不阻塞出包）**：① R2 退出抖动（`AppbarClose` 后 1s 心跳可能补冻，可加 1–2s 纯函数抑制，只抑制 Timer）；② R1 新分支 `AppbarOpen × fs=false` 会**完整冻结含降载** ≤1s、可自愈、窗口不 hide，且 monitor 全 0 安全方向在该路径被放宽；③ 迟到/重复的 TRUE 广播可立即补冻；④ `ensure_on_top()` 与 `set_z_demoted(false)` 各发一次 `HWND_TOPMOST`（冗余无害，别删）。**v1.5.7 首选加固**：`AppbarOpen` 遇 `fg/wm == RECT::default()` 回退 `frozen_decision`。真机验收看日志 `trigger=appbar|timer`：F11 进入应立刻出现 `trigger=appbar` 的「进入」行。
- **当前**：G4 v1.6.0 验收报「按下反馈不稳定 + 悬停不够明显」→ **v1.6.1 已交付待验收**（按下重绘判据补 `pressed`、悬停提至中等档）。验收通过后 → G5 歌词落盘（`歌词目录/Barline/歌手 - 歌名.lrc`，已存在跳过，行级 LRC）→ G6 清理（死码+未引用资源，删前列清单）→ G7 黑名单（v1.8.0，PRD/ARCH 已入库）。
