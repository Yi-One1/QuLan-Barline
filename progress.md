# 曲栏 Barline — 交接文档（progress.md）

> **用途**：新开对话 / 接手者的唯一入口。只读这一份即可恢复上下文；细节再查 `docs/`（PRD / ARCHITECTURE / QA 报告）。
> **最后更新**：2026-11-03 00:25 · 第三轮 12 项全部验收闭环（v1.3.1）· 全量回归绿 · 工作区干净。

---

## 0. 项目速览

Windows 任务栏「媒体小组件」：**Rust + Direct2D 逐像素透明分层窗口**，内嵌任务栏条带（贴托盘区左侧 8px），通过 **SMTC** 读取/控制媒体会话，含封面 / 播控三键 / 进度条 / 音量 HUD / 逐字卡拉OK歌词 / 壁纸自适应对比度 / 托盘菜单 / 全局快捷键 / WebView2 设置页。

| 项 | 值 |
|---|---|
| 栈 | Rust 1.99.1 (MSVC) + `windows` 0.62.2 + Direct2D / DWM / SMTC + WebView2 |
| 仓库 | `E:\APP\WorkBuddy\工作空间\曲栏Barline` · 远端 `https://github.com/Yi-One1/QuLan-Barline.git` · 分支 **main** · 63 提交 |
| 当前版本 | **v1.3.1**（`Cargo.toml` / `assets/barline.rc` 两处 3 个字段） |
| 质量 | 测试 **319**（318 passed + 1 ignored 联网用例）· clippy **0 警告** · release 包 1033 KB |
| 构建 | `powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task check\|clippy\|test\|build`（**禁止裸 cargo**） |
| 自测 | 根目录 **`启动曲栏.bat`**（增量构建 + 跑未打包 exe） |
| 交付包 | `deliverables/曲栏_Barline_vX.Y.Z.exe`，**只增不删** |
| 版本策略 | 改可见行为/新增能力 → 升**第二**位；纯修正/文案/内部重构 → 升**第三**位 |
| 节奏 | 一次一组 → 全绿测试 → 单提交 → 出包 → **用户真人验收** → 才更新文档 → 下一组 |

---

## 1. 已完成事项

### 第一轮：M1~M5 → v1.0.0（历史，详见 `docs/`）
骨架 → 完整小组件 → 歌词引擎（逐字卡拉OK）→ 托盘/快捷键/lrclib → WebView2 设置页 → 打包。四份 QA 独立报告全通过（`docs/QA-M2 / QA-M4 / QA-M3P2 / QA-M5-REPORT.md`）。

### 第二轮：12 项问题，批次①~⑤ → v1.1.0 ~ v1.1.21（全部闭环）

| 批次 | 内容 | 结果 |
|---|---|---|
| 问题1 | 歌词移入任务栏条带（四版迭代 + 跑马灯） | v1.1.0~v1.1.4 ✅ |
| ① 问题3/4 | 数据目录/歌词目录可自定义 + 迁移/删除弹窗自选 | v1.1.5~v1.1.9 ✅ |
| ② 问题5 | 音量「调节步进」恒空白（f32 序列化拓宽 f64） | v1.1.10 ✅ |
| ③ 问题2/10 | 删模糊背景、两态背景 + 可调透明度；歌名有空间被省略 | v1.1.11~v1.1.13 ✅ |
| ④ 问题8/11/12 | 「显示设置」接通、设置页永久置顶、菜单被小组件遮 | v1.1.14 ✅ |
| ⑤ 问题6/7/9 | 会话音量（**裁决禁用**）、封面激活（网易云/Edge 修好，汽水**搁置**）、循环/随机（**整体删除**） | v1.1.15~v1.1.21 ✅ |

### 第三轮：12 项问题，分 6 组 → v1.1.22 ~ v1.3.1（全部闭环）

| 组 | 问题 | 版本 | 要点 |
|---|---|---|---|
| G1 | 5 / 9 / 11 / 12 | v1.1.22 | 菜单「设置」、「开机自启动」、悬停下拉透明在上；歌词关闭时子项与目录区整组置灰 |
| G5 | 1 / 3 / 2 | v1.2.0 | 透明态背景带基色（修「先灰后白」）；删描边；亮度判定四道闸；截图遮罩冻结采样与几何 |
| G2 | 4 / 6 | v1.2.1 | 设置窗口最小化还原（`SW_RESTORE`）；菜单背景扩 4 项（常规/悬停各透明→半透明） |
| G3 | 8 | v1.2.2 | 删除死配置「界面缩放」整项 |
| G4 | 10 | v1.2.3 | 默认回到 `Ctrl+Alt+*`；录制→「更改」；逐行 `↺`；旧配置按矩阵迁移 |
| G6 | 7 | v1.3.0 | 修 lrclib 空 artist 400；标题清洗 + 模糊择优 + 网易云兜底；负缓存换 key |
| 补丁 | — | v1.3.1 | 托盘「显示/隐藏小组件」勾选态反向 |

**顺手修掉的既有缺陷（4 个，均不在原始清单里）**
1. 「关于」对话框版本号写死 `v0.0.1` → 改 `concat!` + `env!(CARGO_PKG_VERSION)`。
2. 设置窗口最小化时 `WM_SIZE` 派发 0×0 → 原样灌给 WebView2，加 `w<=0||h<=0` 守卫。
3. lrclib 收到空 `artist_name` 直接 HTTP 400（Edge 播 B 站视频必现）→ 空 artist 不拼该参数并跳过精确接口。
4. 托盘菜单勾选态反向 → 抽 `tray::toggle_checked()` 为唯一取反点。

---

## 2. 关键决策

### 2.1 铁律（不可违反）

1. **构建**：只走 `scripts/build.ps1`（PowerShell 工具里跑），禁裸 cargo；测试汇总需直跑 `target\<profile>\deps\barline-*.exe`（`windows_subsystem` 吞 stdout）。
2. **交付**：每次改动 bump 版本 + 出包；`deliverables/` 只增不删；历史包永久保留。
3. **本机安全约束**：禁 `reg.exe` / `Add-Type` / `cmd.exe`；注册表读取用 Rust API；结束进程用 `Stop-Process -Name barline -Force`；工程师不启动 exe、不截屏。
4. **文档纪律**：每次验收通过后同步 `progress.md` + 受影响的 PRD / README / ARCHITECTURE。
5. **验收纪律**：用户真人验收为准，工程师自测只是第一层；不要用「测试通过」代替「行为正确」。

### 2.2 架构不变量

6. **布局**：高度**恒定 48px**（=2+22+22+2），任何形态不出任务栏；上行=歌名+歌手紧凑同排（间隔 6px），下行=单行歌词（11px SemiBold，歌名 12px / 歌手 10px）；信息区宽 = max(紧凑总宽, 160px)，上限 340px。
7. **存储**：`platform::data_dir()` 是唯一数据路径入口；`config.storage.data_dir`（空=默认）+ `active_data_dir`（上次生效，迁移源）+ `pending_migrate/pending_delete_old`（弹窗选项，重启执行）；`config.json`/`.tmp` **永远留默认根**且删净时豁免。
8. **背景体系**：模糊已**永久删除**；`idle_mode` / `background_mode` 各自 `Translucent|Transparent` + `idle_alpha`/`hover_alpha`（1..=100，70%→0xB3）；`Theme::apply_bg_config` 是两态背景**唯一写入口**；**透明态必须携带当前 on_light 基色**（只 alpha=0），否则淡入中点是中灰；淡入 120ms / 淡出 200ms。
9. **亮度判定（无 EMA）**：raw → 死区（≤0.12 判深色 / ≥0.25 判浅色，中间保持现状）+ 连续 2 次确认 + 大幅跳变快速通道（越过对侧阈值 ≥0.05 当帧翻）+ 反向冷却 2 帧（防视频壁纸 1Hz 频闪）。**1px 反色描边已删除**，中灰带 4.36:1 可接受。
10. **歌词链**：内存 LRU → 磁盘缓存 → 本地目录 → 负缓存（key 带 `v2:` 盐）→ lrclib（精确 `/api/get` → 模糊 `/api/search` 择优）→ 网易云兜底；`MIN_PICK_SCORE=60`、时长差 >10s 直接淘汰（**宁可不中也不串歌**）；只认带时间轴的歌词。
11. **快捷键**：`HOTKEYS_VERSION=2`，默认 `Ctrl+Alt+P / ← / → / L / M / H`；vk 字面量唯一出处 `bindings_with()`；迁移只认「6 组全等于上一版默认」。
12. **单实例**：互斥体 `Local\Barline.SingleInstance.v1`，句柄存 `OnceLock` 永不释放。
13. **性能口径**：扫光 25fps（降 fps 有可见台阶）；扫光开 CPU ≤2%、扫光关 ≤ 扫光开（同环境相对判定）；关窗后私有内存 3 轮增量<1MB 且峰值<60MB 为 PASS。

### 2.3 已废弃 / 明确禁止（别重犯）

14. **不许叠第三个抗抖器**：最初给「死区 + 二次确认」加了 EMA(α=0.25)，结果白→黑壁纸要 **7 秒**、黑→中灰 8 秒——EMA 与死区在干同一件事，延迟相乘。**抗抖只需要「死区 + 连续确认」两件**。
15. **不许再引入「把最后一个字变淡」的绘制效果**（右缘渐隐带 v1.1.4 已删）。
16. **不许用阴影/底色替代描边兜底**（v1.2.0 已判定描边是视觉噪音）。
17. **不许自动降级快捷键**（注册失败就标红让用户改，禁止偷偷加 Shift）。
18. `ui_scale` 已整项删除，不做「往小缩」也不改字号档位。
19. 绘制守卫教训：任何「状态不满足就整帧跳过」的守卫，扩展新状态时必须重审（v1.1.12 的 `idle_bg` 就被旧守卫静默吞掉一整版）。
20. 改外观必须**即时生效**：`apply_bg_config` 就地同步 + Redraw，不许只靠异步采样回填。

---

## 3. 未完成的待办

### 3.1 待办 A：会话音量（问题 6）—— 用户裁决「暂时禁用，以后有需要再改」

- **现象**：v1.1.15/v1.1.16 在网易云/汽水/Edge 上滚轮均无效；日志显示**连网易云在播都报「没有匹配的音频会话」**——失败在**音频会话枚举/匹配**环节，比 GUID 解析更靠前，且**枚举环节无日志**（被静默吞掉）。
- **已排除**：会话 GUID 来源（已改 `GetSessionIdentifier` 尾段）、循环内 `?` 提前返回（已改 `continue`）、进程映射（SodaMusic.exe 已入表）。
- **未排查嫌疑**：① `IMMDevice::Activate` 拿到的 `IAudioSessionManager2` 枚举为空；② `process_image_name` 对会话 pid 返回空串；③ 设备归属（是否走非默认 endpoint）；④ COM apartment 影响。
- **续修入口**：`src/media/wasapi.rs::session_simple_volume`（枚举处补逐会话 `log_info` 诊断）；`app.rs` on_wheel 的 CurrentPlayer 分支（实现完整保留）；禁用点 = `Config::load` 归一化 + bridge 强制 `volume.scope=System` + 设置页 select 置灰。

### 3.2 待办 D：汽水音乐托盘态封面激活 —— 用户裁决「搁置」

- **实测根因**：汽水（Electron/CEF）Hide 到托盘后，**任何外部 `ShowWindow`（SW_SHOW/SW_RESTORE）+ 任何前台技巧（SFW/ALT 解锁/AttachThreadInput/SwitchToThisWindow）都会让 CEF 输入管线挂起**——窗口显示、前台与 GUITHREADINFO 激活态全部正常，但内容完全点不动（冻结帧）。
- **验证方法学**：「卡死」必须用**行为验证**（点 × 能否隐藏窗口），前台/激活状态查询全是假阳性。
- **有效但被否**：托盘图标点击（`Shell_NotifyIconGetRect` 精确定位 + `SendInput`），恢复与行为验证全通，但**抢鼠标**，用户不启用。
- **已排除**：AUMID `ActivateApplication`（全候选 E_INVALIDARG）；`ShellExecuteW` 开始菜单 lnk（触发汽水重启接管、中断播放）。
- **现行行为（v1.1.21+）**：窗口可见时正常前台激活；托盘隐藏态**不做任何激活**（日志「激活暂禁用（缺陷搁置，待办 D）」），不制造卡死窗口、不抢鼠标。
- **续修入口**：`src/media/launch.rs::tray_click_activate`（`#[allow(dead_code)]` 留档，注释含方向）、`TRAY_RECOVERY_APPS` / `is_tray_recovery_app`、`activate_player` 开头 tray_recovery 分支。**诊断工具 `examples/diag_soda.rs` 已删除**，复现需按当时五种模式（enum/activate/click/fix/tray/fg）重建。
- **续修方向**：① 逆向 Electron NotifyIconHost 托盘回调消息号直接 `PostMessage`（零鼠标移动）；② 探测 deep-link/second-instance 唤醒；③ 仅光标静止时模拟点击 + 光标恢复。

### 3.3 小尾巴（本轮留档，均不阻塞）

- `hotkeys.reset1`（旧「重置」词条）已无引用，可删。
- `SettingsService::is_open()` 全仓零调用，是死 API（可删或让 `open()` 复用）。
- 「冲突一键换组」未做（用户定的每行只有「更改 + ↺」两个按钮，加第三个会违背该决定）。
- 歌词择优阈值 `MIN_PICK_SCORE=60` 偏保守；若出现「明明有歌词却不显示」，可降到 40（串歌风险上升）。

### 3.4 存量与可选（历史遗留，不阻塞）

- **QA 无真机证据项**（M2 时代，勿视为通过）：播控真实改状态 / 拖动跳转 / 封面激活前台 / 滚轮音量 HUD（进程外读 SMTC 被安全策略阻）/ 多显示器 / DPI / 四方位 / 自动隐藏。
- GitHub Release 发布（挂 deliverables zip）、LICENSE 版权人实名（现为 `The Barline Authors` 占位）、设置页标题栏明暗动态跟随、热键日志冗余清理。
- 已知未覆盖的歌词场景：两源都没有的冷门曲 / UP 主原创 / 纯音乐 BGM；只有 `plainLyrics` 无时间轴的条目；中外文歌名差异过大（`周杰伦` vs `Jay Chou`）。

---

## 4. 其他需记录

### 4.1 本机环境（踩过的坑）

- **数据目录已被用户改到** `E:\APP\曲栏Barline\数据目录`；日志跟在那里（**不是** `%LOCALAPPDATA%\Barline`，那份只剩启动前几行）。排查前先确认日志在哪。
- 构建必须用 **PowerShell 工具**跑 `build.ps1`；Bash 里调 powershell 会被拦。
- `git worktree` 检出 `.ps1` 会触发 working-tree-encoding 转码失败；**重建历史版本包**用「主仓库 detached checkout `<hash>` → `build.ps1 -Task build` → 拷 exe → checkout main」。
- push 遇会话代理 502 时走本机代理：`git -c http.proxy=http://127.0.0.1:7890 -c https.proxy=http://127.0.0.1:7890 push`。
- 用户日常用**网易云 / 汽水音乐 / Edge（B 站视频）**测试；验收反馈常附截图，现象以截图为准。

### 4.2 诊断日志关键字（排查时直接 grep）

| 关键字 | 含义 |
|---|---|
| `热键已注册：` / `热键注册失败：` | 逐组注册结果；失败带「哪一组 + 组合 + 错误码」（1409=被占用） |
| `热键触发：` | WM_HOTKEY 到达，说明注册与分发都通（用户报「无反应」时先查这行） |
| `widget rect 变化：old=… new=… reason=…` | 窗口位移归因（TaskbarGeometry / EnsureOnTop / Resize / DpiChange / Unknown） |
| `widget 实际位置漂移：` | 非我方改动导致的位置偏差（`ensure_on_top` 对账） |
| `几何变化（字段=tray_rect,dpi）` | 任务栏几何重算，字段名直接指出是哪个 rect 变了 |
| `文字色翻转：` / `文字色快速翻转（…fast=true…）` | 黑白切换走的是慢路径还是快速通道 |
| `歌词查询：X - Y → 源=? 命中/全部未命中` | 歌词命中率与来源（含候选关键词与耗时） |
| `截图态` | 截图遮罩进入/退出（冻结生效范围） |

### 4.3 交付包现状（`deliverables/`，只增不删）

现存 **18 个 exe**：`v1.1.10` ~ `v1.1.22`、`v1.2.0` ~ `v1.2.3`、`v1.3.0`、`v1.3.1`（另有多余的 LICENSE/README 副本）。
⚠️ **`v1.0.0.exe`、`v1.0.0.zip`、`v1.1.0` ~ `v1.1.9` 已不在磁盘**（疑似早前清理误删，`deliverables/` 在 `.gitignore` 内无法从 git 恢复）。如需补齐，用 4.1 的 detached checkout 方式逐版本重建；不补也不影响使用。

### 4.4 测试与验收口径

- 单测覆盖的是**纯逻辑**（布局计算、主题/亮度状态机、歌词清洗与择优、配置迁移、表单契约）。渲染像素、真实热键、截图行为必须**用户真机看**。
- 每组交付前我自己跑 `check` / `clippy` / `test` 三项并读测试二进制的汇总（不信工程师自述）。
- 有 1 个 `#[ignore]` 的联网用例（lrclib），不联网时属正常跳过。

---

## 5. 接手须知（新对话从这里开始）

1. `git status` / `git log --oneline -8` 确认干净与进度（以本文档头部「最后更新」为准）。
2. 团队：`software-barline-12`（主理人 + `software-engineer`）。新会话需 `TeamCreate` 重建；派发时 prompt **必须自包含**（工作目录、任务、§2.1 铁律、版本 bump 三处、`build.ps1` 全链、单提交、汇报格式）。
3. 每项完成：主理人抽验 diff → 出包 `deliverables/曲栏_Barline_vX.Y.Z.exe`（不删旧包）→ 播报「任务清单 + 进度」→ **停下等验收**。
4. 工程师不启动 exe、不动 `deliverables/`、不 commit（提交与出包归主理人）；用户自测用 `启动曲栏.bat`。
5. 用户报「没反应」类问题时：**先读日志**（§4.2），别先改代码——本轮就有一例是「没打开启用快捷键开关」而非缺陷。
6. 收尾：全部完成后跑全量 `check/clippy/test`，同步文档，写一条记忆到 `.workbuddy/memory/`。
