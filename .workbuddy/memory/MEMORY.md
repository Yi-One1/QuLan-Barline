---
description: 曲栏 Barline 长期记忆总纲——构建/版本/交付纪律、架构不变量、已验证踩坑、下一轮候选。接手只需读这一份；逐日细节见 .workbuddy/memory/2026-11-0X.md。
tags: [曲栏Barline, 项目总纲, 架构不变量, 构建纪律, 踩坑, 下一轮候选]
category: 曲栏Barline
---

# 曲栏 Barline 长期记忆

Windows 任务栏媒体小组件：Rust(MSVC) + `windows` 0.62 + Direct2D/DWM/SMTC + WebView2。当前 **v1.9.2**，测试 594（593 passed + 1 ignored 联网），clippy 0。仓库 github.com/Yi-One1/QuLan-Barline（main，@ `27d8208` 已推；G8-② 提交 `41b36a8` 本地待推）。**G1~G7 全部闭环；G8 分组迭代中：① 已真人验收（v1.9.1）、② 已实现+测试全绿+出包待真人验收（v1.9.2）、③ 待办**。分组与全部拍板见 progress.md §2.5。

## 铁律
- 构建只走 `scripts/build.ps1 -Task check|clippy|test|build`（PowerShell 工具跑）；禁裸 cargo（Git Bash 抢 link.exe）；卡顿/陈旧锁告警设 `$env:CARGO_INCREMENTAL="0"`。
- 测试汇总直跑 `target/<profile>/deps/barline-*.exe`（windows_subsystem 吞 stdout）。
- `deliverables/` 只增不删；每次改动 bump 版本（**5 处**：Cargo.toml 1 + barline.rc 4）+ 出包；`Cargo.lock` 自动同步 1 行一并提交。
- 版本策略：改可见行为/新能力 → 升第二位；纯修正/文案/重构 → 升第三位。
- 禁用 reg.exe/Add-Type/cmd.exe；工程师不启动 exe、不截屏、不动 deliverables、不 commit（主理人统一提交）。
- 节奏：一组 → 全绿 → 单提交 → 出包 → 真人验收 → 才更文档进下一组。多成员协作解除冻结须告知全员 + 新 SHA256，否则整轮作废。

## 架构不变量（保命要点）
- 高度恒定 48px；信息区宽 = max(歌名+歌手自然宽,160)，上限 340。`platform::data_dir()` 唯一数据路径入口；config.json/.tmp 永留默认根。
- 背景：模糊已永久删除；`Theme::apply_bg_config` 两态背景唯一写入口；透明态必须携带 on_light 基色。
- 亮度判定（无 EMA）：死区 + 连续 2 次确认 + 大幅跳变快通道 + 反向冷却 2 帧。
- **SMTC**：禁全量轮询（分层 500/1000/2000ms + 网易云(0,0)×3 退避 5s）；主通道=事件（三事件→脏位→60ms 下限；回调只置 AtomicBool）。**网易云恒报 (0,0)** → 进度/时长不可用，歌词钟靠锚点 `position_at` 外推（绝不用陈旧 timeline_at）。
- **歌词链**：Mem→Disk→Local→负缓存→在线并发（lrclib+网易云 8s 硬截止）；换歌抢占（TrackId 代次守卫）。`MIN_PICK_SCORE=60`、时长差>10s 淘汰。
- **歌词落盘**：`src/lyric/writeback.rs` 只新建绝不覆盖；落点 `local_dirs[0]/Barline/`（空则零 IO）；文件名 **`locate::candidates(track)[0]+".lrc"`**（非法字符用**空格不是 _**）；保留名 CON/PRN/... 拦截；超长放弃不截断；`create_new(true)` 原子不覆盖（禁 .tmp+rename）。**三条触发路径**（mem/disk/online）都落盘（v1.7.0 只挂 online → 缓存里的歌永不落盘）。`Cache::get_mem/get_disk` 回填内存**必须带 source**（丢=首播对二播错）。`render_lrc` 只读 `start_ms`/`text`，绝不读 `words`；不写 `[offset:]`；UTF-8 无 BOM + CRLF。
- **歌词层序（v1.8.0）**：`lyrics.local_first` 默认 false → [Mem,Disk,Local]；true → [Local,Mem,Disk]（本地在内存前，手改 lrc 即时生效）。单 `Step` 数组 + 单循环，禁两份逻辑。`maybe_write_back` 唯一收口。设置页缓存统计走 `SettingsService::post()`（**非 `push()`**，push 会覆写 last_state 致控件空白）。
- **G3.5-A 全屏遮挡（v1.5.6）**：top-level popup（非任务栏子窗）；纯通知型 appbar 只 ABM_NEW/REMOVE、不占工作区、注册在 widget 主窗口（TaskbarCreated 是广播，HWND_MESSAGE 收不到）；ABM_REMOVE 在 host.destroy() 前。`ABN_FULLSCREENAPP` → HWND_BOTTOM（**禁用 HWND_NOTOPMOST，它≠降到底**）→ 退出 HWND_TOPMOST。**永不 SW_HIDE**：frozen 只降 Z+降载；z_demoted 双份幂等、仅 SetWindowPos 成功后翻转；退出先清 frozen/z_demoted 再恢复 Z。1s 心跳仅兜底。`occlusion_want(AppbarOpen)=!(fs && !same_monitor(fg,wm))`。`fs=false` 路径 monitor 全 0 不冻结（安全方向放宽）；v1.5.7 加固：`fg/wm==RECT::default()` 回退 frozen_decision。
- **G4 悬停（v1.6.1）**：四按钮共用 ButtonVisual+glow；`glow_params` **禁加 HitTarget/match target**（四 target 同值结构性保证）；`GLOW_FLOOR_ALPHA=0x06` 是曲线下限截距非 × 底色；悬停 0x42/0x1C、按下 0x60/0x2E；按下=外圈收紧0.5+下沉1.0s+图标94%；封面恒暗蒙+恒亮双箭头（**不用 theme.text**）。视觉参数集中 limits。
- **G6 清理（v1.8.1）**：`src/main.rs` 全局 `#![allow(dead_code)]` → 死码编译器发现不了，清理只能人工逐符号盘点（**且先验证 rg 真能跑**）。`Effect` 死变体"删一半也能编译"（app.rs `other=>` 兜底吞漏删）→ 变体+臂+ARCHITECTURE.md §8 三处同批。`Config` 是 `#[serde(default)]` 无 deny_unknown_fields → 删配置字段老 config 不报错；**加 deny_unknown_fields 即破坏性变更**。清理后 AppEvent 15/Effect 9 变体。
- **G7 黑名单（v1.9.0）**：判定三级回退「缓存→静态表→AUMID 末段」**纯内存**（禁 IO/COM，先 clone 快照）；常量在 limits。`config.media.app_names`（BTreeMap，上限64 淘汰字典序最小键）。枚举只许「进黑名单分区(detectedLoaded 守卫)+手点刷新」两处，零定时器。**`execute()` 入口也拦播控**（命中 return、不发假 CommandResult）。`Refresh/Quit/SetPaused/EnumerateApps` 在拦截前不误拦。ignored 专用轮次（IGNORE_RECHECK_MS=1000+Cadence::consume）**不污染退避状态机**。`normalize_entry` **先剥引号再判非法字符**。`apply_blacklist` 唯一写入入口（apply_set 拒 media.blacklist* 三条）。解绑后补 `Effect::Media(Refresh)` ≤1s 恢复。`IVectorView` 取大小 `Size()`、取元素 `GetAt(i)`（windows-rs 0.62，需 Foundation_Collections）。
- **重绘判据（v1.6.1）**：on_down/up/capture_lost 改 pressed 后返回空 Vec；`Interaction::animating()` 不含 pressed → `App::dispatch()` 用 `interaction_needs_redraw(prev_hover,prev_pressed,state)` 判，**空输入早退 + 正常 reduce 两分支都要用**，`prev_*` 在 `sync_interaction()` 前捕获；`frame_hash` 含 pressed（app.rs:2524）。**凡不产生 WidgetInput 的交互态变化须显式进 dispatch 判据**。`on_capture_lost` **不得清 pending**（否则单击失效）。
- **G8-① 多会话（v1.9.1）**：`tick()` 原本只 `GetCurrentSession()` 一个会话，命中黑名单就 `SessionCleared` 藏整个小组件。现在命中时过 1s 低频闸 `rescan_sessions()` 枚举 `GetSessions()`，`choose_session()` 纯函数优先 Playing 的正常会话（全暂停取系统顺序第一个），锁定 `PollState::display_session` 显示；`switch_display_session()` **整体复位 parts/cadence/CoverMemo/session_events/last** 杜绝串歌；`execute()` 播控改打显示会话；「当前应用」= `state.playing.session`（`displayed_session_hit` 纯函数）。`scan_gate(ignored_now, has_displayed_session)` 纯函数是**常规播放零新增跨进程调用**的唯一闸（4 格真值表锁死，`(false,false)→false`）；`release_needs_ignored_clear()` + `apply_release()` 保证 Release 时清 `last_ignored` 并发 `IgnoredSession(None)`（否则托盘残留「恢复 A」，点它误删黑名单）。**G7 的 `tick_ignored()`/`Cadence::consume` 一行未改**。已知限制（用户拍板不修）：隐藏态 ≤1 次/秒低频枚举、UI 同帧隐藏 ≤50ms 瞬时、`plan.props=false` 过渡轮次 ≤2s 混合快照（G7 固有）。
- 热键 `HOTKEYS_VERSION=2`，默认 Ctrl+Alt+P/←/→/L/M/H；单实例 `Local\Barline.SingleInstance.v1`。性能埋点 `perf::Span`（默认 50ms / smtc.* 8ms 才记）。

## 已验证的坑（环境/通用）
- `AppState::geometry_epoch` 每秒变（探测序号）；稳定标识用 `area_epoch`。`SW_SHOW` 对最小化窗口 no-op → `IsIconic` 后 `SW_RESTORE`。
- 日志 relocate 到自定义 data_dir（本机 `E:\APP\曲栏Barline\数据目录`）。lrclib 空 artist → HTTP 400，空 artist 改走搜索。
- git worktree 检出 .ps1 转码失败；重建历史包用 detached checkout。windows-rs 0.62 SMTC 事件名不带会话前缀、反注册 PascalCase `Remove*`。
- **`git grep` 不含 untracked**；`#[ignore]` 注释字面串误计 → 计数用 ripgrep 全量 + --list。
- **靠外部命令做判定先验证命令能跑**：v1.8.1 QA 用 `rg` 查死码，环境无 rg → 全面假绿（与"测试全绿但功能没做"同类失效）。
- **测试可"全绿但功能没做"**：v1.7.0 的 `s5` 注释把 bug 锁成契约（"防人塞落盘钩子到快路径"），改此类测试须重写名/注释表新意图，不许改到能过就算。
- **动态 i18n 文本不能挂 `data-i18n`**：render() 会重置成模板串，须放无该属性的元素自渲染。
- **`copy-item` 还原源码复原 mtime** → cargo 复用旧二进制、测试数字虚高；还原探针后 `touch` 源强制重编。
- 卡顿类方法学：埋点→用户两阶段对照→日志按时间点对齐→只对共同动作定罪。睡眠时长须超被测周期。
- **PS 5.1 跑 `build.ps1` 会假失败**：脚本第 42 行 `$ErrorActionPreference='Stop'` 会把 cargo 写 stderr 的 `Compiling/Finished` 升级成 `NativeCommandError`（红字），可能中断 cargo 得到假 `EXIT=-1`。解法：读脚本原文、仅把该行**在内存中**放宽为 `Continue`、用 `[scriptblock]::Create` 执行，脚本文件一字不改，取真实 `$LASTEXITCODE` + `Finished`。**坑中坑**：转 scriptblock 会丢 `$PSScriptRoot` → `-Task build -Profile release` 的体积报告 `Join-Path` 拿到空路径抛异常；须把 `$PSScriptRoot` 替换成脚本目录字面量。或用不合并 stderr 的直接调用规避。
- **「有效测试」要能锁住接线，不只锁纯函数**：G8-① 第 1 轮 QA 把主循环性能闸改恒 `true`（常规播放也允许枚举）时 **570 条测试全绿** —— 红线只靠代码走查。修复方式是抽 `scan_gate()` 纯函数 + 真值表单测，并要求「复刻改坏必须变红」才算出包。反向验证要覆盖**事件内容**（故意发 `SessionCleared` 冒充，测试须打印「必须恰好一条 IgnoredSession(None)」），不能只断言「有事件/文件存在」。

## 留档待办 / 下一轮候选（均不阻塞）
- 待办 A 会话音量（禁用，入口 `media/wasapi.rs::session_simple_volume`）；待办 D 汽水音乐托盘态激活（搁置：外部 ShowWindow 让 CEF 输入挂起）。
- 下一轮候选：① pre-existing 死码 30 项（HEAD 就有，QA 已列）；② **语言切换需重启**（`general.language` 改 `App::lang` 不更新、托盘不刷新，挂点 `Effect::RefreshTrayMenu`）；③ D-6 两份进度实现（`State::progress_fraction` 与 painter.rs 内联 frac，屏幕上画的不是被测那个）；④ 「清空歌词缓存」一键按钮；⑤ G2 残余卡顿续查路线在 progress.md §3.1。
- v1.5.6 观察项：R2 退出抖动（AppbarClose 后 1s 心跳可能补冻）；R1 `AppbarOpen×fs=false` 完整冻结≤1s 可自愈；迟到/重复 TRUE 广播可补冻。首选项：`AppbarOpen` 遇 `fg/wm==RECT::default()` 回退 frozen_decision。
- 小尾巴待办：设置页标题栏明暗跟随、热键日志冗余清理、「冲突一键换组」未做。
- **版权人已统一实名 `Yi One`**：LICENSE + `assets/barline.rc` 的 `CompanyName`/`LegalCopyright`（2026-11-06 补齐，此前 exe 资源面板仍显示旧占位 `The Barline Authors`）。README 已内嵌设置页/小组件截图（assets/screenshots/）。GitHub Release（tag+exe）仍需用户到 Releases 页手动发（本机无 `gh` CLI / token）；v1.9.0/v1.9.1 未发，现有 v1.9.2 包 39 个 exe。
- **G8-② 技术前提（开工前必读）**：用户只修「网易云暂停→继续歌词跳回第一句」，seek 明确不修（网易云 SMTC 恒报 `(0,0)` 读不到新位置）。`NowPlaying::position_now()`（media/mod.rs:166）非 Playing 直接返回 `position_ms`（暂停冻结）；根因线索在 `smtc.rs` 的 `note_playback` 状态跃迁会置 `plan.timeline=true` → 读到 (0,0) + `read_timeline_now=true` → `anchor_of` 规则 1 把锚点重置为 now，`position_ms=0`。**歌词钟靠锚点外推，绝不能碰 G7/G8-① 的退避状态机与拦截点**。
- **G8-② 已实现（v1.9.2）**：`LyricClockAction{Carry}` + `tick_inputs()/TickInputs` 单入口（派生 `read_timeline_now` 杜绝双来源），`apply_timeline_read()` 见 (0,0) 不覆盖 `parts.position_ms`，Carry 用 `lyric_clock_at(last,now)` 外推续接。**锁接线手段（防「测试全绿但功能没做」）**：`tick_wires_state_jump_into_snapshot_inputs` 源码静态断言（读 `smtc.rs` 自身，断言 `tick` 含 `state_jumped = true` 且经 `tick_inputs(...)` 收拢、无 `tick_inputs(false`、无 `read_timeline_now =`）；把 `state_jumped = true` 改 `= false` 必须恰好 1 条测试变红（RV-C3）。
