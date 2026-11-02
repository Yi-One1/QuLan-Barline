---
description: 曲栏 Barline 项目长期记忆总纲：构建/版本/交付纪律、架构不变量、已验证的踩坑清单、当前留档待办。接手前只需读这一份即可恢复上下文。
tags: [曲栏Barline, 项目总纲, 架构不变量, 构建纪律, 踩坑]
category: 曲栏Barline
---

# 曲栏 Barline 项目长期记忆

Windows 任务栏媒体小组件：Rust 1.99.1(MSVC) + `windows` 0.62.2 + Direct2D/DWM/SMTC + WebView2。
当前版本 **v1.3.1**，测试 **319 项**（1 ignored 联网），clippy 0 警告。仓库 `https://github.com/Yi-One1/QuLan-Barline.git`（main）。

## 铁律（违反必踩坑）
- 构建只走 `scripts/build.ps1 -Task check|clippy|test|build`（用 PowerShell 工具跑），**禁止裸 cargo**（Git Bash 抢 link.exe）。
- 测试汇总要直跑 `target/<profile>/deps/barline-*.exe`——`windows_subsystem` 会吞 stdout。
- `deliverables/` 的 exe **只增不删**；每次改动 bump 版本并出包。
- 版本策略（用户定）：改可见行为/新增能力 → 升第二位；纯修正/文案/内部重构 → 升第三位。
- 禁用 `reg.exe` / `Add-Type` / `cmd.exe`；工程师不启动 exe、不截屏、不动 deliverables、不 commit（主理人统一提交）。
- 节奏（用户定）：一次一组 → 全绿测试 → 单提交 → 出包 → 用户真人验收 → 才更新文档并进下一组。

## 架构不变量
- 小组件高度**恒定 48px**（=2+22+22+2），任何形态不出任务栏；信息区宽 = max(歌名+歌手自然宽, 160px)，上限 340px。
- `platform::data_dir()` 是唯一数据路径入口；`config.json` / `.tmp` **永远留默认根**且删净时豁免。
- 背景：模糊已**永久删除**；`idle_mode` / `background_mode` 各自 Transparent|Translucent + `idle_alpha`/`hover_alpha`(1..=100)；`Theme::apply_bg_config` 是两态背景唯一写入口；透明态**必须携带 on_light 基色**（只 alpha=0）。
- 亮度判定（无 EMA）：raw → 死区(≤0.12 深 / ≥0.25 浅，中间保持) + 连续 2 次确认 + 大幅跳变快速通道(越过对侧阈值 ≥0.05 当帧翻) + 反向冷却 2 帧。1px 反色描边已删除。
- 歌词链：内存 LRU → 磁盘缓存 → 本地目录 → 负缓存(`v2:` 盐) → lrclib(精确→`/api/search` 模糊择优) → 网易云兜底；`MIN_PICK_SCORE=60`、时长差 >10s 直接淘汰（宁可不中也不串歌）。
- 热键：`HOTKEYS_VERSION=2`，默认 `Ctrl+Alt+P/←/→/L/M/H`；迁移只认「6 组全等于上一版默认」。
- 单实例互斥体 `Local\Barline.SingleInstance.v1`。

## 已验证的坑
- `AppState::geometry_epoch` **每秒都变**（probe 每次 +1），是"探测序号"不是"代次"；要稳定标识请用采样矩形指纹 `area_epoch`。
- `SW_SHOW` 对**最小化**窗口是 no-op → 必须 `IsIconic` 后 `SW_RESTORE`。
- 日志会 `relocate` 到自定义 data_dir（本机当前 `E:\APP\曲栏Barline\数据目录`）；排查前先确认日志在哪个目录。
- lrclib 的 `/api/get` 收到**空 `artist_name`** 会直接 HTTP 400 → 空 artist 不拼该参数、改走搜索。
- 抗抖**不要叠三层**（EMA+死区+二次确认=延迟相乘，实测白→黑 7 秒）。
- CEF/Electron 应用的"卡死"必须用**行为验证**（点 × 能否隐藏），前台/激活状态查询全是假阳性。
- git worktree 检出 `.ps1` 会触发 working-tree-encoding 转码失败；重建历史包用「detached checkout → build → 拷 exe → 回 main」。

## 留档待办（不阻塞）
- 待办 A：会话音量（v1.1.17 起禁用，续修入口 `media/wasapi.rs::session_simple_volume`）。
- 待办 D：汽水音乐托盘态激活（搁置：任何外部 ShowWindow 都会让 CEF 输入挂起）。
- 小尾巴：`hotkeys.reset1` 死词条；`SettingsService::is_open()` 零调用；「冲突一键换组」未做。
