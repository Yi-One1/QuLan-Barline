# 曲栏 Barline — 开发进度交接文档（progress.md）

> 用途：供「新开对话 / 接手者」快速恢复上下文。本文档与 `docs/` 下的 PRD / ARCHITECTURE / 各里程碑报告互补，但本文件是**唯一需要通读就能上手**的索引。
> 最后更新：M4 第一批已提交（be7f8d9），M4 第二批（设置页）代码已落工作树但未提交未验证。

---

## 0. 项目一句话定位

Windows 任务栏「媒体小组件」：**Rust + Direct2D 逐像素透明分层窗口**，内嵌任务栏条带（贴托盘区左侧 8px），通过 **SMTC** 读取/控制媒体会话，支持**封面/歌名/歌手/三按钮/进度条/音量 HUD**、**壁纸亮度自适应黑白文字**、**悬停毛玻璃**、**逐字卡拉OK歌词**，以及**托盘菜单 / 全局快捷键 / WebView2 设置页**。

| 项 | 值 |
|---|---|
| 语言/栈 | Rust 1.99.1 (MSVC) + `windows` 0.62.2 + Direct2D/DWM/SMTC |
| 构建 | `powershell -ExecutionPolicy Bypass -File scripts/build.ps1`（**禁止直接 `cargo build`**，见 §2 环境约束） |
| 仓库 | `E:\APP\WorkBuddy\工作空间\曲栏Barline`（git，master，4 个里程碑提交） |
| 团队 | 交付总监（齐活林/我）、许清楚（PM）、高见远（架构）、寇豆码（工程师）、严过关（QA） |
| 关键语境 | 用户另有同名自研软件 `BarlineWidget`（类名 `BarlineWidget`/`BarlineBackdrop`），**不是系统控件，小组件不做任何避让/共存逻辑** |

---

## 1. 已完成事项（按里程碑）

### 提交序列（git log --oneline）
```
be7f8d9  v0.0.5  M4 第一批：托盘 / 全局快捷键 / lrclib 超时修复 / 退出卡死修复
b9d204b  v0.0.4  M2 QA 六条缺陷全部清零（clippy / frame_hash / 对比度 / LRU）
1f66500  v0.0.3  M3 歌词引擎（逐字卡拉OK）
b29e619  v0.0.2  M1 骨架 + M2 完整小组件（初始大提交）
```
> 注：M1、M2 在第一个大提交里一起进了仓库；之后每个改进独立提交。

### M1 — 任务栏胶囊骨架
- Rust 工程骨架、`#![deny(warnings)]`、单实例、日志、配置、i18n、托盘雏形
- 任务栏几何探测（四方位 / 多 DPI / 自动隐藏 / Explorer 重启重挂）
- SMTC 媒体会话读取；Direct2D 逐像素透明分层窗口
- 交付：`docs/M1-REPORT.md` + 4 张截图

### M2 — 完整小组件（静止/悬停/完整形态）
- 封面 WIC 解码 + 圆角裁切；歌名/歌手省略号截断（结果进布局缓存）
- 播放/上一首/下一首、进度条拖动跳转、循环/随机
- 悬停背景：自研 box blur 毛玻璃 / 半透明，两模式可切换
- 壁纸亮度自适应黑白文字（alpha 掩码剔除自身像素，防自强化）
- 封面取色强调色、音量 HUD、右键菜单、点击封面激活播放器
- 修复：底部任务栏纵向排布反转、WM_SETTINGCHANGE 挂在 message-only 窗口收不到广播、采样抖动导致每秒白重绘
- 真机验收 10/10；9 张截图 `docs/screenshots/m2-*`
- `docs/M2-REPORT.md`

### M2 QA（严过关，独立验收）
- 报告 `docs/QA-M2-REPORT.md`：结论 **有条件通过**
- 独立跑出的正面证据：release 零 warning、125 test 全绿、右边缘贴合 2154=2162−8（0px）、静止态**真透明**（像素差 5.19≤噪声 7.45）、悬停**真模糊**（梯度 −88%）、深色对比度 6.90:1、单实例退出 0.09s、5 分钟无泄漏
- 挑出 **6 条缺陷**（见 §3）

### M3 — 歌词引擎（逐字卡拉OK）
- `src/lyric/` 9 文件（均 ≤500 行）：LRC/增强LRC 解析、`[offset:]` 生效、CJK 逐字/拉丁按词/标点归前的按字符数加权插值
- 查询顺序：内存LRU(8) → 磁盘缓存(30d) → 本地目录 → 负缓存(24h) → 在线；**自研 WinHTTP 客户端**（无 reqwest）
- 两遍绘制+裁剪扫光，边界用 `HitTestTextPosition` 换算 + 亚字插值
- 25fps 定时器仅在「可见+就绪+播放中」存在；降级四态高度零变化；歌词开关 200ms 收合；切歌 120ms 淡出
- 201 测试全绿、clippy 清零、exe 842.5 KB；13 张截图 `docs/screenshots/m3-*`
- `docs/M3-LYRIC-REPORT.md`

### M2 QA 六条缺陷清零（v0.0.4）
- clippy 用**聚合结构体真解**（painter::Ctx / TextDraw / Pill / sampler::BlockGrid），`src/` 内**零 `#[allow]`**
- `frame_hash` 呈现去重落地（12 项指纹，进度按1s、扫光按1/60行量化）
- 歌手名对比度：同色 + 动态 alpha 下限 78%（浅底 9.90:1 / 中灰 4.54:1 / 深底 11.17:1）
- 歌词 LRU 改为按字节封顶 512KB（附单测）
- 203 测试全绿

### M4 第一批（v0.0.5，已提交 be7f8d9）
- **lrclib 在线歌词修复**（根因是超时设太低，不是被墙）：`/api/get` receive 5s→**12s**、`/api/search`→15s，resolve/connect/send 各 5s 分离；超时/5xx **重试 1 次**；加 **25s 总闸**（单次 receive 超时非总时长，实测 404 能滴 22.4s）
- **系统托盘**（`src/ui/tray.rs`）：独立隐藏顶层窗口作菜单宿主（小组件 NOACTIVATE、消息窗 HWND_MESSAGE 都不能承载菜单）；三项菜单「显示设置 / 显示·隐藏小组件 / 退出」+ 勾选态；TaskbarCreated 重加图标；退出 NIM_DELETE；截图 `m4-01-tray-menu.bmp`
- **全局快捷键**（`src/hotkey.rs`）：6 组 `RegisterHotKey`；`WM_HOTKEY`→`AppEvent`→`reduce`；冲突(1409)记 `Conflict` 不阻止启动；`RebindHotkeys` 重绑；`suspend/resume` 留给录制
- **P0 退出卡死修复**：原来退出 join 歌词线程最坏卡 25s，改断开通道+detach → **20ms 干净退出**
- 209 测试全绿、clippy 零告警、exe 853 KB

---

## 2. 关键决策（拍板记录，避免推翻重来）

1. **系统控件避让逻辑整体删除**：用户澄清 `BarlineWidget` 是其自有另一软件，明确「不用管它」。已从 5 模块移除（detect_system_widget / reserve_for / compute_target_ex / 3 配置字段 / 2 i18n 词条 / open_taskbar_settings）。小组件就是「贴托盘区左侧 8px」，不避让任何东西。
2. **PRD P0-05 修正**：原要求「完全在任务栏条带内」与双行歌词 218×94 > 条带 48 矛盾。裁决：**主行必须完全在条带内，歌词区向桌面方向扩展**（底部任务栏向上、顶部向下）。已写入 `docs/PRD.md`。
3. **CPU 指标口径修正**：原「播放中 ≤0.5%」与「25fps 逐字扫光」物理矛盾。改为：**无扫光动画时 ≤0.5%；扫光源运行期间允许 ≤2%**（实测中位 1.3%）。**保持 25fps**（降到 10fps 有可见台阶、缓存底图口径改动 150 行，不划算）。
4. **歌手名对比度**：同色 + 动态 alpha（下限 78%），满足 WCAG 4.5:1。
5. **默认快捷键改前缀**：工程师实测 `Ctrl+Alt+P/←/→/L/M` 在本机**全被占用(1409)**，仅 `Ctrl+Alt+H` 空闲。改成 **`Ctrl+Alt+Shift+`** 前缀；加 `hotkeys_version` 迁移逻辑——**只迁移从未改过默认的老配置**，用户自定义过的绝不覆盖。
6. **首启冲突提示用托盘气泡**，不弹 MessageBox 打断。
7. **构建纪律**：统一 `scripts/build.ps1`（自动注入 MSVC 环境）。**禁止直接 `cargo build`**——Git Bash 的 `usr/bin/link.exe` 会抢占 MSVC 链接器报 `link: extra operand`。
8. **本机安全约束（必须照做）**：禁用 `reg.exe`（黑名单）、`Add-Type`（编译 .NET 被拦）、`rm` 清理 `%LOCALAPPDATA%` 下文件（安全钩子）。截屏统一 `scripts/grab-screen.py`（ctypes+GDI）。启动 exe 必须后台起，结束 `Stop-Process -Name barline -Force`。
9. **验收纪律**：QA 必须在**独立 git worktree** 验收（`git worktree add ... <commit>`），不信任工程师自述，一切以自跑证据为准。
10. **进程回收陷阱**：exe 作为工具调用的子进程会被进程树回收，验收界面前必须让父进程保活（工程师在 scripts 里有可复用做法）。

---

## 3. 未完成的待办（含已知缺陷）

### 高优先
- **M4 第二批：设置页（WebView2）—— 代码已完成但未提交未验证**
  - 文件：`src/settings/{mod,webview,bridge,assets}.rs`（共 ~1007 行）+ `assets/ui/index.html` + 截图 `m4-10`~`m4-17`（appearance/lyrics/volume/hotkeys/general/about/dark/english）
  - 实现点：Mica 标题栏 + 圆角 + 明暗跟随；WebView2 环境**异步回调**创建（不堵消息泵）；`bridge.rs` JSON 双向协议 + 配置白名单写入；关窗 = `DestroyWindow` + 彻底 drop WebView2（顺序：controller.Close→drop controller→drop webview→drop env→DestroyWindow），保证内存回落
  - `Cargo.toml` 已加 `webview2-com = "0.39.1"`
  - **编译已验证**：`cargo check` 退出码 **0**（2026-11-01 实测，零编译错误；之前一次 exit 1 是 PowerShell `2>&1 | Select-Object` 管道误报，非真失败）。**尚未跑** `cargo test` / `clippy` / release 构建 / 运行时验收。
  - **待办**：`cargo test` 全绿 → `clippy -D warnings` 零 → `cargo build --release` 零 warning → 提交 → QA 复验
  - ⚠️ 当前状态：这批代码在**主仓库工作树（未提交）**，且与默认快捷键改前缀的 `Cargo.toml`/`src/*` 改动混在一起。接手时先 `git diff` 看清，确认 test/clippy 通过再提交。

### 中优先（M3 遗留 P2）
- **歌词长行处理**：当前 >15 字行**硬裁**（无省略号，否则扫光码元映射错位）。计划改为**右侧渐隐**（保留码元映射）。
- **行切换 150ms 上移动画**：未做（切歌目前是 120ms 淡出，无上移）。

### 低优先 / 后续
- **M5 打包发布**：正式图标、版本资源、便携版打包 `曲栏_Barline_v1.0.0.exe`、README / LICENSE、GitHub 推送提交序列。
- **QA 未拿到证据的验收项**（来自 QA-M2-REPORT，勿视为通过）：播控三键真实改状态、拖动 50% 跳转误差、点击封面激活前台、滚轮音量真实变化+HUD（本机无「时长已知且可控」会话 + 进程外读 SMTC/音量被安全策略阻断）；切歌 1s 更新、关播放器 3s 收起、配置损坏恢复、运行中托盘增减、任务栏自动隐藏、多显示器/DPI/四方位——均未实测。

---

## 4. 当前团队状态 / 阻塞（接手须知）

- **子代理撞 429 `Credits exhausted`**：派发工程师/QA 子代理会失败。**本地 Bash 不受影响**，可亲自跑 build/test。
- **寇豆码（工程师）子代理**：M4 第二批进行中时因 429 失败，但设置页代码已留工作树（见 §3）。
- **严过关-2（QA，M4 第一批复验）**：任务被暂停（`be7f8d9` 的独立 worktree `barline-qa-m4` 已建），**复验报告未产出**。
- **工作树不干净**：`git status` 显示设置页相关文件未提交 + `Cargo.lock`/`Cargo.toml` 改动。**任何接手动作前先 `git status` / `git diff`**，勿在脏树上直接叠加。
- 已建 worktree：`E:\APP\WorkBuddy\工作空间\barline-qa-m4`（基于 be7f8d9，QA 用，可 `git worktree remove ... --force` 清理）。

---

## 5. 建议的接手步骤（顺序）

1. **先验证设置页**：`powershell -ExecutionPolicy Bypass -File scripts/build.ps1 -Task check`（或 `build`），修任何编译错误；再 `test` + `clippy`。
2. **提交 M4 第二批**：把设置页相关改动单独提交（与默认快捷键改动合并也行，但要 review diff）。
3. **QA 复验**：让 QA 在包含设置页的新快照上复验（含 M4 第一批回归 + 设置页新功能）。
4. **处理 M3 遗留 P2**：歌词长行右侧渐隐、行切换 150ms 上移动画。
5. **M5 打包发布**：图标 / 版本资源 / 便携版 / README+LICENSE / GitHub 推送。

---

## 6. 关键文件索引

| 路径 | 作用 |
|---|---|
| `docs/PRD.md` | 需求（30 条 / P0 16 条 / 验收标准 §8.1）⚠️ 已按决策修正 P0-05、CPU 指标 |
| `docs/ARCHITECTURE.md` | 架构（3038 行；§4.4 内存预算、§10 任务栏内嵌、§11 亮度/模糊、§13 设置页） |
| `docs/TASKS.md` | 任务分解（20 任务 / 5 里程碑） |
| `docs/M1/M2/M3/M4-REPORT.md` | 各里程碑报告（工程师自述，作线索非结论） |
| `docs/QA-M2-REPORT.md` | 严过关 M2 独立验收（有条件通过 + 6 缺陷） |
| `src/render/` | d2d / painter / layout / text / theme / sampler / blur / cover_cache |
| `src/ui/` | widget_wnd / interaction / hit / menu / tray |
| `src/media/` | smtc / cover / launch / player_map / wasapi |
| `src/taskbar/` | geometry / host |
| `src/lyric/` | 歌词解析/查询/扫光/缓存（9 文件） |
| `src/settings/` | 设置页（mod/webview/bridge/assets，未提交） |
| `src/config/` | 配置（mod/defaults）；注意已删 avoid_system_widget 等字段 |
| `src/platform/` | dpi / wake / 其他平台封装 |
| `scripts/build.ps1` | **唯一**构建入口（MSVC 环境注入；-Task check/clippy/test/build） |
| `scripts/grab-screen.py` | 截屏（ctypes+GDI） |
| `scripts/m3-accept.py` / `scripts/m4-settings-check.py` | 真机验收脚本 |
| `docs/screenshots/` | m1-* / m2-* / m3-* / m4-* 全部验收截图 |

---

## 7. 已验证硬指标（截至 be7f8d9，来自 QA 自跑 + 工程师自述，已交叉核对）

| 指标 | 值 |
|---|---|
| release 构建 | 零 error、零 rustc warning |
| clippy `-D warnings` | 零告警（src/ 内无 `#[allow]`） |
| 单元测试 | 209 passed（M4 第一批）/ 203（v0.0.4）/ 125（M2） |
| exe 体积 | 853 KB（v0.0.5，预算 8 MB） |
| 内存 | WS 35.5–36.6 MB / Private 11.8–12.7 MB（预算 WS 40 MB，余量 ~3 MB） |
| CPU | 暂停 0.10% / 无歌词 0.13% / **播放+扫光 1.25%**（指标口径见 §2-3） |
| 退出耗时 | 25s → **20ms**（退出卡死已修） |
| 几何 | 右边缘贴合托盘区 = 托盘左 - 8px，误差 0px |
| 透明 | 静止态真透明（空区域像素差 5.19 ≤ 噪声 7.45） |
| 悬停 | 真模糊（边缘梯度 −88%） |
| 对比度 | 深色背景文字 6.90:1、歌手名浅/中/深 9.90/4.54/11.17:1 |
| 歌词 | 高度四态零变化（双行94/单行74/关闭48）；200ms 收合实测 6 中间帧；25fps 扫光 |
| 单实例 | 二次启动 0.09s 退出，退出码 0 |
| 长稳 | 5 分钟无泄漏无抖动（CPU 0.245%） |
