# 曲栏 Barline — 交接文档（progress.md）

> **用途**：新开对话 / 接手者的唯一入口。只读这一份即可恢复上下文；细节再查 `docs/`（PRD / ARCHITECTURE / QA 报告）。
> **最后更新**：2026-11-04 23:16 · **G3 全链闭环（v1.5.0~v1.5.4 已真人验收）** · **G3.5 全屏遮挡 v1.5.6 已真人验收 ✅** · G2 闭环 · **G4 悬停反馈 v1.6.1 已真人验收 ✅** · **G5 歌词落盘 v1.7.0 已出包（待真人验收）** · G7 黑名单待实现（v1.8.0） · 全量回归绿（494 测试：493 passed + 1 ignored）· 工作区干净（已单提交）。

---

## 0. 项目速览

Windows 任务栏「媒体小组件」：**Rust + Direct2D 逐像素透明分层窗口**，内嵌任务栏条带（贴托盘区左侧 8px），通过 **SMTC** 读取/控制媒体会话，含封面 / 播控三键 / 进度条 / 音量 HUD / 逐字卡拉OK歌词 / 壁纸自适应对比度 / 托盘菜单 / 全局快捷键 / WebView2 设置页。

| 项 | 值 |
|---|---|
| 栈 | Rust 1.99.1 (MSVC) + `windows` 0.62.2 + Direct2D / DWM / SMTC + WebView2 |
| 仓库 | `E:\AI工作空间\曲栏Barline`（2026-11-03 迁入，旧路径 `E:\APP\WorkBuddy\工作空间\曲栏Barline` 已废弃）· 远端 `https://github.com/Yi-One1/QuLan-Barline.git` · 分支 **main** |
| 当前版本 | **v1.4.4**（`Cargo.toml` 1 处 + `assets/barline.rc` 4 处：`FILEVERSION`/`PRODUCTVERSION` 数字版 + `FileVersion`/`ProductVersion` 字符串，**共 5 处必须同步**） |
| 质量 | 测试 **390**（389 passed + 1 ignored 联网用例）· clippy **0 警告** · release 包 1066 KB |
| 构建 | `powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task check\|clippy\|test\|build`（**禁止裸 cargo**） |
| 自测 | 根目录 **`启动曲栏.bat`**（增量构建 + 跑未打包 exe）；新包在 `deliverables/` |
| 交付包 | `deliverables/曲栏_Barline_vX.Y.Z.exe`，**只增不删** |
| 版本策略 | 改可见行为/新增能力 → 升**第二**位；纯修正/文案/内部重构 → 升**第三**位 |
| 节奏 | 一次一组 → 全绿测试 → 单提交 → 出包 → **用户真人验收** → 才更新文档 → 下一组 |

---

## 1. 已完成事项

### 第一轮：M1~M5 → v1.0.0（历史，详见 `docs/`）
骨架 → 完整小组件 → 歌词引擎（逐字卡拉OK）→ 托盘/快捷键/lrclib → WebView2 设置页 → 打包。四份 QA 独立报告全通过。

### 第二轮：12 项问题，批次①~⑤ → v1.1.0 ~ v1.1.21（全部闭环）
歌词入任务栏条带 / 数据目录与歌词目录自定义 / 音量步进 f64 / 两态背景删模糊 / 显示设置接通 / 会话音量禁用（待办 A）/ 封面激活（汽水搁置，待办 D）/ 循环随机整体删除。

### 第三轮：12 项问题，6 组 → v1.1.22 ~ v1.3.1（全部闭环）
菜单设置 / 开机自启 / 悬停下拉 / 透明态带基色 + 亮度四道闸 + 截图冻结 / 最小化还原 / 删界面缩放 / 快捷键回 Ctrl+Alt / lrclib 空 artist 400 修复 + 网易云兜底 / 托盘勾选态反向。顺手修 4 个既有缺陷（版本号写死、WM_SIZE 0×0、空 artist 400、勾选态取反点）。

### 第四轮（2026-11-03，用户 6 项问题）：G1/G2 已闭环，v1.4.0 ~ v1.4.3

**G1 · 问题1 歌词查询慢（25~84s）→ v1.4.0 ✅ 已验收**
- 根因（日志实证）：单线程 worker 串行查询 + 整链重试 ×3（5/15/60s）+ lrclib 失败立刻重试 → 一条冷门查询最坏 83.8s；「查过的歌也要 10s+」是**排队**不是缓存失效。
- 修复：两源**并发**（`query_parallel`，首个命中即返回）+ **8s 硬截止**（单源 5.5s / receive 4s）+ **换歌抢占**（主循环只做缓存/本地/负缓存等毫秒级步骤，在线查询丢独立线程，`TrackId` 代次守卫丢弃旧结果）+ 重试收敛（**最多 1 次 / 3s，超时不重试**）+ 缓存键归一化 `cache_title_key`（摘 `_哔哩哔哩_bilibili` 与首尾 `【】「」` 装饰；版本标记 Live/伴奏/圆括号**不摘**，防串歌）。
- 顺手修：`barline.rc` 数字版本号 `1,1,21,0` → 同步 Cargo 版本（v1.1.22 起一直错）。

**G2 · 问题2 随机卡顿 → v1.4.1~v1.4.3 ✅ 用户验收：切歌跟手 ✓、歌词同步 ✓**
- v1.4.1 加分段埋点 → 用户对照实验锁定 SMTC 轮询 → v1.4.2 分层降频（14→5.5 次跨进程调用/秒，卡顿消失但引入切歌延迟与歌词漂移）→ v1.4.3 锚点三规则 + SMTC 三事件订阅 → 切歌跟手 ✓ 歌词同步 ✓。

### 第四轮 G2 残余卡顿续查 → v1.4.4（**已出包 + 2026-11-03 真人验收通过**）

> ✅ 状态：**闭环**。用户实测「网易云播放一段时间未再卡顿」，日志侧同步印证（详见 §3.1）。

**日志实证的归因（v1.4.4 之前的历史日志）**
1. **反证（假阳性）**：`msg.mouse` 的 `843ms / 882ms / 450001ms` 三条，毫秒级时间戳**全部与 `[app] 收到 Quit` / `右键菜单：Quit` 同一毫秒**。450001ms = 用户开着托盘右键菜单放了 **7.5 分钟**（菜单模态循环被算进 span）。**不是卡顿**。
2. **实锤（唯一真实越阈段）**：`sampler.capture` 8 条 51/55/63/70/80/111/132/188ms，集中在 01:05–01:08（6 条）与 02:16（2 条），其余时间静默。运行时长约 50 分钟 → **约每 6 分钟一次**，频率与量级都和「偶尔卡、间隔大」吻合。段内是 `GetDC(None)` + `BitBlt` 读已合成桌面 → 强制 GPU→CPU 同步、和 DWM 抢锁。
3. **盲区**：`tick.capture / tick.probe / tick.ontop / tick.backdrop` 全程**零记录** —— 不是不慢，是每秒跑一次、单次 20~40ms 永远越不过 50ms 阈值。

**v1.4.4 改动（commit `5108192` + 记忆 `28f9ac3`）**
- **A1** `WM_RBUTTONUP` 在弹菜单**前**显式 `sp.take()→finish()`，菜单模态时间不再计入；`is_mouse_message` 注释写明三条历史假阳性。
- **A2** 新增 `UI_SLOW_MS=SAMPLER_SLOW_MS=16`，**只**给 `tick.capture/probe/ontop/backdrop` + `sampler.capture` 五段；`Span::new` 走 `threshold_for(name)` 自动套用，其余段保持 50ms / smtc 8ms。
- **A3** `RateLimiter`：同段 60s ≤10 条 + 每窗口一次提示；只在「要写日志」时拿锁（`admit_record` 返回即释放，**无锁嵌套 → 不会死锁**）。
- **A4** `CaptureStats`（AtomicU64，取走即清零），60s 统计行追加 `capture=N 慢M capture最慢=Xms`。
- **B1** 静止态心跳 1s → **3s**（`limits::BACKDROP_IDLE_MS=3000`），`last_backdrop` 只在**真投递**后刷新；悬停/换壁纸/几何变化/退出截图态仍立即采样。
- **B2** 前台全屏跳过心跳采样；`is_fullscreen_window` 纯函数（3px 容差），复用 `sync_capture_freeze` 里本来那次 `foreground_info()` → **零新增跨进程调用**。
- **补丁**：`should_skip_idle_backdrop` 加「同屏」条件（副屏全屏不再误停主屏心跳）；`widget_monitor` 在 `relayout_with` 的 `apply_geometry` **之后**刷新。

**验证**：测试 363 → **390**（389 passed / 1 ignored），clippy 0 警告；主理人独立复跑两轮；QA 两轮全 PASS，抓到并修掉「多显示器误伤」真实缺口。

---

## 2. 关键决策

### 2.1 铁律（不可违反）

1. **构建**：只走 `scripts/build.ps1`（PowerShell 工具里跑），禁裸 cargo；测试汇总需直跑 `target\<profile>\deps\barline-*.exe`（`windows_subsystem` 吞 stdout）。
2. **交付**：每次改动 bump 版本（**5 处**）+ 出包；`deliverables/` 只增不删；历史包永久保留。
3. **本机安全约束**：禁 `reg.exe` / `Add-Type` / `cmd.exe`；注册表读取用 Rust API；结束进程用 `Stop-Process -Name barline -Force`；工程师不启动 exe、不截屏。
4. **文档纪律**：每次验收通过后同步 `progress.md` + 受影响的 PRD / README / ARCHITECTURE。
5. **验收纪律**：用户真人验收为准，工程师自测只是第一层；不要用「测试通过」代替「行为正确」。

### 2.2 架构不变量

6. **布局**：高度**恒定 48px**（=2+22+22+2）；信息区宽 = max(紧凑总宽, 160px)，上限 340px。
7. **存储**：`platform::data_dir()` 是唯一数据路径入口；`config.json`/`.tmp` **永远留默认根**且删净时豁免。
8. **背景体系**：模糊已永久删除；`idle_mode`/`background_mode` 各自 `Translucent|Transparent` + alpha 1..=100；`Theme::apply_bg_config` 是唯一写入口；透明态必须携带 on_light 基色；淡入 120ms / 淡出 200ms。
9. **亮度判定（无 EMA）**：死区 + 连续 2 次确认 + 大幅跳变快速通道（越过对侧阈值 ≥0.05 当帧翻） + 反向冷却 2 帧；1px 描边已删除。
10. **歌词链（v1.4.0 起）**：内存 LRU → 磁盘缓存 → 本地目录 → 负缓存（`v2:` 盐）→ **在线并发**（lrclib 与网易云各起线程，首个命中即返回）。单源预算 5.5s、单请求 receive 4s、全局**硬截止 8s**；**换歌即抢占**——在线查询绝不在主循环里同步跑，结果回主线程先过 `TrackId` 代次守卫，旧代结果直接丢弃（不推事件、不写负缓存）；网络失败最多重试 **1 次（3s）**，**超时不重试**。缓存键走 `query::cache_key`：摘平台后缀与首尾装饰，**版本标记一律不摘**。`MIN_PICK_SCORE=60`、时长差 >10s 淘汰；只认带时间轴歌词。
11. **SMTC 媒体轮询（v1.4.2/1.4.3 起，卡顿的根）**：**禁止改回全量轮询**——每 500ms 全量读一次 ≈14 次跨进程调用/秒会把网易云拖卡（v1.4.2 实测）。分层常量（`config::defaults::limits`）：快 500ms（状态）/ 中 1000ms（时间线）/ 慢 2000ms（元数据+controls）/ 网易云 `(0,0)`×3 退避 5000ms。**主通道是事件**：会话级 `MediaPropertiesChanged / PlaybackInfoChanged / TimelinePropertiesChanged` → 回调只置 `AtomicBool` 脏位（回调跑在 WinRT 线程池，严禁 COM 重活）→ 60ms 下限 forced 全量读；订阅跟随 AUMID 重挂/摘除，token 存 `SessionSubscriptions`（Drop 时 PascalCase `Remove*`）。
12. **位置锚点 `position_at`（歌词同步的命门）**：三规则（`smtc.rs::anchor_of`）——本轮读了时间线 → 用读取时刻；没读但 `track_id` 变了 → 用检测时刻；其余 → **沿用上一份锚点**。绝不许退回陈旧的 `timeline_at`（网易云恒报 `(0,0)`，歌词钟靠锚点起走表；v1.4.2 就因挂到陈旧锚点导致歌词快最多 5s）。
13. **快捷键**：`HOTKEYS_VERSION=2`，默认 `Ctrl+Alt+P/←/→/L/M/H`；vk 字面量唯一出处 `bindings_with()`；迁移只认「6 组全等于上一版默认」。
14. **单实例**：互斥体 `Local\Barline.SingleInstance.v1`。
15. **性能口径（v1.4.4 更新）**：`perf::Span` 埋点仍在；默认 `smtc.*` **8ms**、`paint.frame/cover.decode/msg.mouse` 等**50ms**；`tick.capture/probe/ontop/backdrop` + `sampler.capture` 这 **5 段 16ms**（一帧 16.7ms，UI 线程上 16ms 已可感知，旧 50ms 口径会吃掉 20~40ms 尖峰）；**同段 60s 限流 ≤10 条**；统计日志每 60s 一条（段名 `smtc 轮询统计`，已追加 `capture=N 慢M capture最慢=Xms`）。
16. **亮度采样降载（v1.4.4）**：静止态背景采样心跳 **3s**（原 1s），前台**同屏**全屏跳过；用户能感知的采样路径（悬停/换壁纸/几何变化/退出截图态）一律立即采样、不受影响。注意：3s 心跳下**边际**亮度变化最坏 6s 才翻（2 样本 × 3s），大幅跳变走快速通道仍是 1 样本（≤3s，换壁纸即时）。

### 2.3 已废弃 / 明确禁止（别重犯）

17. **不许把在线歌词查询放回主循环同步执行**（v1.4.0 教训）。
18. **不许叠第三个抗抖器**（EMA+死区+二次确认=延迟相乘）；**不许右缘渐隐、阴影描边兜底、自动降级快捷键**；`ui_scale` 已删除。
19. 绘制守卫：任何「状态不满足就整帧跳过」的守卫，扩展新状态时必须重审。
20. 改外观必须**即时生效**：`apply_bg_config` 就地同步 + Redraw。
21. **调 SMTC 的节奏永远优先事件、其次降频轮询**：对第三方播放器的跨进程调用次数要当「预算」管（v1.4.2 教训）。
22. `src/event.rs:201` 注释声称「保存配置带 300ms 去抖」实际是同步写盘——待修（**G6 一并处理**：真去抖或改注释）。
23. `Effect::RequestBackdrop`（`event.rs:196`）与 `Effect::MoveWindow`（`app.rs:1919`）**全仓零发射点**，是死 effect/死分支，G6 清理候选。

---

## 3. 未完成的待办

### 3.1 ✅ G2 残余卡顿续查（v1.4.4）—— **2026-11-03 22:00 用户真机验收通过，已闭环**

**用户结论**：用网易云播放一段时间，**未出现卡顿**。

**日志侧独立印证**（v1.4.4 会话 03:07:43 → 13:47:55，连续 10h40m / 639 条分钟统计）：
- `sampler.capture` 慢记录：旧版 8 条（51/55/63/70/80/111/132/188ms）→ **v1.4.4 全程 0 条**；`capture=15~17 慢=0 capture最慢=4~9ms`。**注意这不是阈值放宽洗掉的**：阈值反而从 50ms 收紧到 16ms，仍零记录。
- 全种类慢记录仅 2 条：`tick.probe 24ms`（12:29:59）、`smtc.props 79ms`（13:15:01，媒体线程，不阻塞 UI）。`msg.mouse` 6 条假阳性**全部在 v1.4.4 之前**，新版期间为零 → 菜单模态埋点修正有效。
- **覆盖空洞**：03:16:56 → 09:00:02 前台全屏 2560×1440 持续 5h43m，心跳按设计跳过（`capture=0` 那 342 行），有效采样约 4.4h。
- **唯一还活着的 UI 线程嫌疑**：`tick.probe`（每秒 `SHAppBarMessage`）偶发 24ms，暂不动、留档。

**本轮已做的（v1.4.4）**
- 抓屏 `sampler.capture` 从 1 次/秒降到 **1 次/3 秒**（BitBlt 是头号嫌疑段）。
- 修了 `msg.mouse` 菜单假阳性埋点 + 把 tick.* 盲区从 50ms 降到 16ms + 60s 统计行加 `capture=N 慢M capture最慢=Xms`。

**（原验收路线已执行完毕，保留备查）**：`capture≈15~17/min`，落在「3s 心跳 + 同屏全屏跳过」的预期区间，`慢=0` → 降载确认生效，无需回查 `limits::BACKDROP_IDLE_MS`。

**仍存的残余嫌疑（按优先级，证据都在代码里）**
- ★★★ `render/sampler.rs` BitBlt 读已合成桌面——v1.4.4 已降到 1/3 频率，若还卡，下一步是「关屏幕采样看是否消失」（`request_backdrop` 开头 `return;`）。
- ★★★ `app.rs:613 detect_capture()` → `launch.rs:279 process_image_name()`：UI 线程**每秒**对前台进程 `OpenProcess`+查询镜像名，被安全软件挂钩时随机几~几十 ms。**下一步杠杆：按 PID 缓存镜像名**（前台 PID 不变时复用，零额外查询）。
- ★★★ `taskbar/geometry.rs:147 probe()`：UI 线程每秒 `SHAppBarMessage`（同步跨进程发 Explorer）。可考虑 2s 或更省。
- ★★ `taskbar/host.rs:143`：UI 线程每秒 `SetWindowPos` 置顶。
- ★★ `log.rs:142 write()`：全局 Mutex + 每行 flush 同步落盘，慢盘/杀软扫描时跨线程排队。
- ★★ `ui/widget_wnd.rs:214` 全局低层鼠标钩子（仅悬停期安装）。
- 已排除：`CAPTUREBLT`、25fps 定时器、歌词网络线程（独立线程）。

**二分杠杆（一次只关一项，每项 ≥10 分钟、换歌 ≥10 次，「不卡了」以用户主观 + 日志零 `[perf]` 双确认）**
- `limits::MEDIA_POLL_SLOW_MS` 2000→4000（元数据压力再降一半）；`FORCED_FLOOR_MS` 60→300（事件风暴时用）；`request_backdrop()` 开头 `return;`（关屏幕采样）；`detect_capture` 加 PID 缓存（关每秒 OpenProcess）；`ensure_on_top()` 调用点注释（关每秒置顶）。

**相关代码落点**：`src/platform/perf.rs`（`Span`/`SLOW_MS=50`/`UI_SLOW_MS=16`/`SMTC_SLOW_MS=8`/`RateLimiter`/`CaptureStats`）、`src/media/smtc.rs`（`Cadence` 分层 / `SessionSubscriptions` 事件 / `schedule` / `flush_if_due`）、`src/config/defaults.rs::limits`（`BACKDROP_IDLE_MS=3000` + 500/1000/2000/5000ms 常量）、`src/app.rs`（`sync_capture_freeze` / `relayout_with` 的 `widget_monitor` 刷新 / 心跳门控）、`src/render/sampler.rs`（埋点 2 段）。
**埋点去留**：v1.4.4 已决定——**保留 `[perf]` 慢日志**（阈值 16/50/8ms）与 60s 统计行（含 capture 字段），只做限流；不删。

### 3.2 待做组（G3~G7，按序执行；G3.5 为 2026-11-04 追加、G7 为 2026-11-03 追加）

> ⚠️ 版本号已重排（按下表，覆盖旧 progress 里的 v1.5.1/v1.6.0/v1.6.1 写法）。

- **G3 右上角 ×（问题 4）→ v1.5.0**：用户定「**常驻**、尽量小但要清晰、位置＝**整个小组件右上角**」；点击＝隐藏到托盘。
  - **布局方案（已拍板）：独占右上角**——右边缘为 × 预留约 16px，三键整体左移，窗口总宽 +≈16px（已确认与 `btn_next` 右对齐垂直居中的 28×28 命中区重叠问题）。新增 `HitTarget::Dismiss`。
  - **交付实况（v1.5.0~v1.5.4，用户逐轮验收微调）**：字形 16 → **6px**、命中区 24 → **14px**、笔画 1.6 → 1.2、常态 100% 不透明（`DISMISS_ALPHA=0xFF`，与三键 enabled 同源）；`DISMISS_GAP` 8 → **2**、`DISMISS_RIGHT_MARGIN=4`（字形右缘距窗口边缘 8px，与左侧 `CONTENT_PAD` 对称）；窗口相对 v1.4.4 为 **+12\*s**；「下一首」图标右缘 → × 字形左缘 = **12px**。
- **G3.5 全屏遮挡（用户 2026-11-04 追加）→ v1.5.5 ❌ 被用户否定 → v1.5.6 · ✅ 已真人验收（用户 21:13「这一项没问题，下一项」）· 包 `deliverables/曲栏_Barline_v1.5.6.exe`**：全屏应用（电影/播放器/浏览器 F11/游戏）时小组件**直接被盖住且静止**，不得压在全屏窗口上方、不得占后台；退出全屏自动恢复。
  - **v1.5.5 验收否定（用户原话）**：「切换全屏时，会有1秒的延迟……我要的效果是像 fluentflyout 那样，全屏时直接被盖住，退出全屏时还在……去查一下这个软件是怎么做的」。根因：v1.5.5 靠 1s `foreground_info()` 轮询发现进出 + 隐藏 + 400ms 解冻兜底，进出各约 1s。用户拍板 **方案 A：事件驱动 + 只降层级不隐藏 + 保留冻结**。
  - **现象（用户原话）**：「其他窗口全屏时，小组件会左移，且小组件会盖在全屏的窗口上方」；追加要求「全屏时让本软件静止或冻结，避免后台占用（例如全屏游戏）」。
  - **已存在的信号（零新增跨进程调用）**：`platform::capture::foreground_info()` 返回 `fullscreen` + `monitor`，v1.4.4（G2-B2）起每秒随心跳刷新；`frozen_decision()`（`app.rs:2971`）**直接复用** `should_skip_idle_backdrop`，不复制同屏判据。
  - **「左移」根因（本机日志实证，02:47~02:48）**：全屏时任务栏托盘区变宽 28px（`tray_left 2130→2102`），`place_widget` 按 `tray_left` 定位 → `target x 1922→1894`；退出后自动恢复。**跟随逻辑没 bug，不改**；退出全屏那一 tick 仍跑 `geometry::probe()`，位置自然拉回。
  - **冻结内容**：`widget.set_visible(false)` + 停 `TIMER_ANIM/LYRIC`（`start_widget_timer` 有 frozen 守卫）+ 媒体线程 `SetPaused(true)` 走**阻塞 `recv()`**（0 唤醒 / 0 跨进程）+ `redraw`/`ensure_on_top`/`request_backdrop`/几何探测/`detect_capture_with` 全跳过。**只保留 `TIMER_GEOM 1000ms` 的 `foreground_info()`**（纯本地）作为发现退出的唯一途径；热键仍可用；不新增轮询频率、不用 WinEventHook、不动 `SetThreadExecutionState`。
  - **解冻顺序（不闪旧帧）**：`SetPaused(false)+Refresh` → `resume_pending=true` + `sync_visibility` 加 `&& !resume_pending` 保持隐藏 → 收第一份 `NowPlaying`（`Effect::ResumeFromFreeze`）或 400ms `TIMER_RESUME=6` 兜底 → `finish_resume` 在隐藏态 `relayout+redraw` → 才 `set_visible(true)`；门控是 `geometry.is_some() && compute_visible()`（用户冻结期主动隐藏时兜底不得强行弹出）。`PollState::reset_for_resume()` 保证解冻首轮即 `Plan::full()`。
  - **QA 结论 PASS-WITH-NOTES**（450 测试，12 条独立回归）：主理人疑点 `tick_optimistic()` 冻结期仍在 `frozen` 判断之前执行 —— **证伪为非问题**（只读一个 `Option`，`redraw()` 首行 frozen 守卫挡住整帧）。另发现工程师 `paused` 用例只用 `sleep(30ms)` < 50ms 轮询周期、无法证伪阻塞语义，QA 补 150ms 真阻塞测试。
  - **留档观察（不阻断）**：`AppEvent::Tick` 全仓无发送者（未来启用会多一个 500ms UI 唤醒）；`relayout_with` 与 `sync_visibility` 的 resume 门控写法不一致；`TIMER_CLICK`(3) 冻结期理论上仍可挂（实际不可达）；快速「退出→400ms 内再全屏」会多一次无效 relayout（门控保证不误显示）；`reset_for_resume` 的 `checked_sub(60ms)` 在进程极早期可能退化（实测未触发）。
  - **不可破**：副屏全屏**不得**冻结主屏小组件；monitor 全 0 不冻结（安全方向）；既有 428 条断言一条未放宽。
  - **v1.5.6 返工实况（方案 A，已真人验收 ✅）**：
    - **根因再确认**：曲栏**不是任务栏子窗口**（`src/taskbar/host.rs` `parent=None` 的 top-level popup），`widget_ex_style()` 含 `WS_EX_TOPMOST` + `ensure_on_top()` 每秒置顶 → 全屏窗口盖不住它。FluentFlyout（`TaskbarWindow.xaml.cs`）用 `SetParent(Shell_TrayWnd)` 变任务栏子窗口、全屏时天然跟随被盖；其 `SHQueryUserNotificationState` 只用于判断是否弹 flyout。本轮按用户拍板**不改 `SetParent`**。
    - **机制**：新增 `src/platform/appbar.rs`「纯通知型 appbar」（只 `ABM_NEW`/`ABM_REMOVE`，**故意不发 `ABM_QUERYPOS/SETPOS`、不占工作区**），注册在 **widget 主窗口**（`TaskbarCreated` 是广播，`HWND_MESSAGE` 收不到），`WM_BARLINE_APPBAR = WM_APP+5` 在 `widget_proc` 唯一派发。`ABN_FULLSCREENAPP(TRUE)` → 先 `SetWindowPos(HWND_BOTTOM)` 预降 → 同一次 WndProc 里一次本地 `foreground_info()` 对账 → `frozen_decision()` 判同屏；`(FALSE)` → `AppbarClose` 强制解冻、立即 `HWND_TOPMOST`。1s `TIMER_GEOM` 的 `foreground_info()` 只作兜底对账（appbar 事件漏报最多 1s 纠错，非主路径）。
    - **为什么是 `HWND_BOTTOM`**：`HWND_NOTOPMOST` 只是「置于所有非 topmost 窗口之上」，对 Chrome F11 / 电影和电视 / 无边框全屏游戏**仍会盖上去**，原 bug 不修；MS 对 `ABN_FULLSCREENAPP` 的要求本就是 drop to bottom。`HWND_BOTTOM` 一次调用同时清 topmost 状态并落到 Z 序底部。「插到 `taskbar_hwnd` 之后」候选亦被否决：顶层窗口之间无 Z 序跟随（只有子窗口才有），且曲栏窗口矩形完全落在任务栏矩形内部，低于任务栏常态就不可见。
    - **语义重定义**：`frozen` = 全屏遮挡态（窗口**仍 visible**，只降 Z 序 + 静止降载），`compute_visible()` 删 frozen 分支；新增 `z_demoted`（`AppState` 期望态 + `TaskbarHost` 已应用态；幂等、**只在 `SetWindowPos` 成功后**翻转）；`apply_geometry` / `raise` / `set_visible` / `ensure_on_top` 全按态门控。**删除整套 resume 机制**（`resume_pending` / `TIMER_RESUME=6` / `RESUME_FALLBACK_MS` / `Effect::ResumeFromFreeze` / `finish_resume`）。退出顺序：先清 `frozen/z_demoted` → `HWND_TOPMOST` → `ensure_on_top()` → `sync_visibility()` → `SetPaused(false)+Refresh` → `sync_lyric_timer()`+`request_backdrop()`；窗口全程未 hide，静止旧帧连续存在，不空白、不闪、不等 `NowPlaying`。
    - **R1 竞态修复（QA 提出、出包前补）**：`occlusion_want(AppbarOpen)` 由 `frozen_decision(...)` 改为 `!(fs && !same_monitor(fg, wm))` —— **信任事件，仅「本地确实看到全屏窗口且明确异屏」才还原**。否则「通知早于前台窗口切换」时 `fs=false` 会把刚降的 BOTTOM 在同一 WndProc 内还原、退回 1s。`AppbarClose` 强制 false、`Timer => frozen_decision` 与三个纯函数逐字未动；真值表扩为 30 格 + 独立推导 + 6 组命名竞态用例。
    - **QA 结论 `PASS-WITH-NOTES`**：14/14 PASS、路由 `NoOne`；`check/clippy/test` 全绿、**461 个 `#[test]` / 1 ignored / 460 passed**、clippy 0 警告、`git diff --check` 干净。基线 `src/app.rs` SHA256 `b56a6841…` 构建前后一致（并发写曾两次作废旧轮次，最终以冻结指纹为准）。`deliverables/曲栏_Barline_v1.5.6.exe` 1073 KB、FileVersion/ProductVersion 1.5.6。
    - **留档观察（不阻塞，按 QA 最终增量报告给精确口径）**：① **副屏/全 0 不变量只在 `fs=true` 路径 100% 保住**：`AppbarOpen => !fs || same_monitor(...)` 的 `||` 短路使 `fs=false` 时 monitor **完全不参与判定**，因此 **「monitor 全 0 不冻结」这条 v1.4.4 起的安全方向在事件路径被有意识放宽**。`widget_monitor` 在启动早期 / `geometry::probe()` 失败 / `WM_DISPLAYCHANGE` 瞬间为 `RECT::default()`，此时一条 TRUE 广播会完整冻结（含停 ANIM/LYRIC、`SetPaused(true)`、跳过采样）≤1s，由 1s 心跳按严格的 `frozen_decision` 自动恢复；窗口始终 visible、永不 `SW_HIDE`、无残留状态。真值表把 `fs=false × 全 0 → true` 明钉为契约，属**契约重定义**（有注释与等强度反向替代断言，非静默删）。② `fs=false` 是**完整进入遮挡/降载**，不只是「仅 Z 序预降」，误判时用户看到内容停更 1s。③ **R2 退出抖动**：`AppbarClose` 后 1s 心跳若本地仍报全屏会补冻；且新分支下迟到/重复的 TRUE 广播（close/open 乱序、Explorer 重注册补发）在 `fs=false` 时可**立即**补冻、不再等心跳。建议 v1.5.7 加 1–2s 纯函数抑制（只抑制 Timer、不抑制 AppbarOpen）。④ **v1.5.7 加固首选（QA 建议，不引入新状态/定时器）**：`AppbarOpen` 分支先判 `fg == RECT::default() || wm == RECT::default()` 则回退 `frozen_decision(fs, fg, wm)`，恢复 monitor 全 0 安全方向；正常竞态下 monitor 已知，不牺牲 R1 修复效果。⑤ `ensure_on_top()` 与 `set_z_demoted(false)` 各发一次 `HWND_TOPMOST`（冗余无害，别删）。⑥ **真机验收看日志 `trigger=appbar|timer`：F11 进入应立刻出现 `trigger=appbar` 的「进入」行**，若先出现 `trigger=timer` 说明事件路径没生效；出现 `trigger=appbar` 进入 → 约 1s 后 `trigger=timer` 退出且用户未退出全屏，即为误冻样本。
- **G4 悬停/按下反馈（问题 3 + 5）→ v1.6.1 · ✅ 已真人验收通过（2026-11-04 22:41，用户「没问题，继续下一个」）**：封面悬停＝变暗蒙层 + 左上/右下双箭头（点击整块封面＝打开播放器，沿用 `ClickCover`）；四按钮（含 ×）用户选「**柔和光晕 + 下沉**」——悬停：白底黑影/黑底白光双层低透明度光晕；按下：光晕收紧、图标下沉 1px 并缩到 94%。**× 与四按钮共用同一套悬停/按下状态机与绘制函数**（G3 起设计好复用）。
  - **流程（专家团队 SOP）**：PM 许清楚增量 PRD（含 5 个待拍板问题）→ 架构师高见远增量设计（独立评审主理人越权起草的 WIP 原型）→ 工程师寇豆码实现 → QA 严过关独立回归。主理人只做编排 / 汇编 / 出包 / 提交。
  - **用户三项拍板**：① **Q1 = 保留极淡保底**（与 PM 推荐相反）：`hover_bg.a==0` 时四 target 不归零；② **Q2 = 不做动画**，即时反馈，不新增定时器 / 帧驱动 / 轮询；③ **Q4 = 封面双箭头 ↖+↘ 对角外扩**。Q3（× 是否更淡）、Q5（蒙层暗度）留真人验收微调。
  - **Q1 落地口径（架构师裁定）**：`GLOW_FLOOR_ALPHA=0x06` 是四 target 共享的 `glow_params(hovering, pressed, hover_bg.a)` 强度曲线在 `a→0` 处的**非零下限截距**，不是 × 专属底色；实现 `round(nom*k).max(FLOOR).min(nom)` 逐分量；透明档悬停 `(0x06,0x06,1.0)`、按下 `(0x06,0x06,0.5)`；普通档 70% 悬停 `(0x16,0x08,1.0)`、按下 `(0x20,0x0E,0.5)`；无态恒 `(0,0,1.0)`。`glow_params` **签名无 `HitTarget`、函数体无 `match target`** → 四 target 同值是结构性保证。R6/A5 原「透明档 alpha 全 0」已作废。
  - **共用一套实现**：`draw_buttons`（三键循环）与 `draw_dismiss`（×）是 `draw_button_feedback` 仅有的两个调用点；旧 `theme.text@0x14` 淡底绘制路径已移除（`painter.rs` 内 `DISMISS_BG*` / `dismiss_bg_rect` grep 为空）；`DISMISS_BG_HOVER/PRESSED/RADIUS` 符号**保留**（`layout.rs` 契约测试锁死，删则违反 461 零删改 + `deny(warnings)`），注释标注「历史契约常量、v1.6.0 起不参与绘制」。
  - **架构师两处推翻**：① 封面箭头**不用 `theme.text`**（浅壁纸是深色，叠黑蒙层不可见），改「恒暗蒙层 + 恒亮箭头」；② `glow_params` 的 WIP 线性归零实现与 Q1 冲突，推翻重写。
  - **交付数据**：`470 个 #[test] / 469 passed / 0 failed / 1 ignored`（基线 461 → +9，零删改零放宽）；check/clippy/test/build 四连全绿、clippy 0 警告；版本 5 处 + `Cargo.lock:7` 同步 1.6.0；包体 1,101,312 B、FileVersion/ProductVersion 1.6.0；历史 32 包只增不删。
  - **QA 结论 `PASS-WITH-NOTES` / 路由 `NoOne` / 可出包**：12 项核查全 PASS；独立复核 4 处工程师偏差（`center` 元组合并 8→7 参行为等价、`fill_arrow` 泛化 `arrow_axis` 使 orientation=0 逐像素不变、`Option<bitmap>` 语义等价、常量具名化无内联魔法数）；零触碰清单（theme/layout/ui/taskbar/main/platform/media）diff 全空，appbar 全屏遮挡 grep 为空。
  - **参数集中（`limits`，便于逐轮微调）**：`GLOW_FLOOR_ALPHA∈[0x01,0x06]`、`COVER_HOVER_DIM=0x5A`（建议区间 `[0x40,0x80]`）、`COVER_ARROW_ORIENTATION`（0=↖+↘ / 1=↗+↙ / 其它兜底 0）、光晕 alpha/半径/扩张/按下缩放/下沉/箭头尺寸线宽内边距全部具名。
  - **留档观察（不阻塞，QA）**：① 透明档保底 2.4% 是否可辨、双层同 alpha 的观感（只剩 RGB+半径区分）；② 100% DPI 下 × 与「下一首」光晕净距 2.5px、三键相邻两圆相切（快速横移可能「光圈串」）；③ **disabled 三键悬停仍出光晕**（`draw_button_feedback` 不读 `on`），可能与 `text@0x55` 的半透明图标组合成「看似可点」，真机判断是否需要 `enabled` 门控；④ `fill_arrow` 是 `COVER_ARROW_ORIENTATION` 的第二读取点（与 `cover_arrow_specs` 同源同兜底，当前无错配调用，建议下一轮把 orientation 变入参）；⑤ 本轮零运行时验证，hover/press 像素合成、DPI 抗锯齿、封面蒙层观感只能真人验收。
  - **v1.6.1 真人验收反馈 2 项（2026-11-04 22:2x，用户原话）**：「① 按下反馈时而出现，时而没有，并不稳定；② 悬停反馈不够明显」。
    - **① 根因（确定性 Bug，主理人逐行定位）**：`WM_LBUTTONDOWN` → `Interaction::on_down` 设置 `pressed` 后**返回空 `Vec<WidgetInput>`**；`App::dispatch` 的 `inputs.is_empty()` 分支**只比较 `hover_target`、不比较 `pressed`**；而 `Interaction::animating()` 只由 `hover_alpha` 差值与 `hud_left_ms` 决定，**按下不会让它为 true** → `sync_interaction()` 把 `TIMER_ANIM` kill 掉。结果：悬停淡入未结束（≈150ms 内）时按下能画出按下态，淡入结束后按下**一次重绘都不发生**。`WM_LBUTTONUP` / `WM_CAPTURECHANGED` 同理（松手/失焦也不重绘，按下态可能残留）。
    - **① 修复**：新增纯函数 `interaction_needs_redraw(prev_hover, prev_pressed, state)`（`app.rs` 模块级），`dispatch()` **两条分支（空输入早退 + 正常 reduce）都用它**；`prev_*` 必须在 `sync_interaction()` **之前**捕获（否则判据恒 false）。`src/ui/interaction.rs` **零改动**（重绘决策属 App 层，不下沉进纯交互状态机）。`frame_hash` 自 v1.5.0 起就含 `pressed`，故补的帧不会被哈希去重吃掉。
    - **② 处置**：用户四档中选「**中等**」→ `GLOW_INNER_HOVER 0x20→0x42`、`GLOW_OUTER_HOVER 0x0C→0x1C`、`GLOW_INNER_PRESSED 0x2E→0x60`、`GLOW_OUTER_PRESSED 0x14→0x2E`。默认 70% 档实测 **18.0% / 7.8%**（原 8.8% / 3.3%）、按下 **26.3% / 12.5%**；`GLOW_FLOOR_ALPHA=0x06`、半径/扩张/缩放 0.94/下沉 1px/蒙层 0x5A/箭头常量**全部未动**。
    - **交付数据**：`472 个 #[test] / 471 passed / 0 failed / 1 ignored`（v1.6.0 基线 470 → +2，既有断言零删改，仅 5 条 `glow_params_truth_table` 期望值按新常量重算并注明理由）；check/clippy/test/build 全绿、clippy 0 警告；版本 5 处 + `Cargo.lock:7` 同步 1.6.1；包体 1,101,312 B、FileVersion/ProductVersion 1.6.1；历史 33 包只增不删。
    - **QA 结论 `PASS` / 路由 `NoOne` / 可出包**：10 项全 PASS。重点复核：`frame_hash` 确含 `pressed`（`:2524`，修复不形同虚设）、`prev_*` 捕获在 `sync_interaction()` 之前、down/up/capture_lost **三条路径都出帧**、`AppState.pressed` **唯一写入点** `sync_interaction()`、按住不动不重复重绘（`Interaction::animating()` 未被污染 → 空闲 CPU≈0 完好）、`dispatch()` 早退三条件不误吞按下帧。
    - **留档观察（不阻塞）**：① `dispatch()` 本体与 `frame_hash` 的 `pressed` 敏感性**无自动化测试**（需 App+hwnd，单测不可行），靠逐行审查把守，日后重构 `dispatch` 需人工复看 `app.rs:2832-2853` 与 `:2524`；② 松手帧实际由 `ReleaseCapture` 触发的嵌套 `WM_CAPTURECHANGED` 落地、外层 `dispatch` 兜底（双保险），**依赖 `on_capture_lost` 不清 `pending`**——若将来给它加「清 pending」，单击会失效，必须同步补测；③ 若真机看到「偶尔单击没反应」，优先查 `ReleaseCapture` 路径而非本轮改动。
- **G5 在线歌词落盘（用户追加）→ v1.7.0 · ✅ 已出包 `deliverables/曲栏_Barline_v1.7.0.exe`（待真人验收）**：在线歌词命中后落盘到「歌词目录」**第一个**目录下的 `Barline/` 子目录；文件名 `歌手 - 歌名.lrc`；**已存在一律跳过、绝不覆盖**；只写**行级** LRC + `[ti:]/[ar:]/[al:]`，**不写逐字标签**。
  - **⚠️ 需求口径的两处实际修正（与用户原话不同，均已确认）**：① 非法字符**替换为空格**（不是 `_`）——`locate::normalize_name` 把 `\ / : * ? " < > |` 换成空格再比对，用 `_` 会导致「写了永远命中不了」；② **不改查询顺序**（用户第二次拍板「还是不改查询顺序吧」），已知限制：**手改 `.lrc` 不会立刻生效**，要等磁盘缓存 30 天过期或手动清缓存。落盘收益在「缓存失效 / 清缓存 / 换机后仍能从本地命中」。
  - **流程（专家团队 SOP）**：PM 许清楚增量 PRD（8 个待确认问题，含 3 个「写了搜不到」的致命发现）→ 架构师高见远设计（**R2/R3 两次打回**：第一版仍带「本地优先」，用户改口径后整章作废）→ 工程师寇豆码实现 → QA 严过关独立回归（发现尾随点缺陷）→ 工程师定点修 → 主理人复核出包。
  - **新增模块 `src/lyric/writeback.rs`（~580 行，含 15 条测试）**：`Target` / `Skip{NoDir,NoName,Reserved,TooLong,Empty,Exists}` / `Outcome{Written,Skipped,Failed}`；`resolve_target_dir` / `file_name_for` / `render_lrc` / `save` 全为可单测纯函数（IO 只在 `save` 里）。
  - **落盘规则**：目录 = `local_dirs[0].trim()` 非空 + 绝对路径 + `is_dir()` → `d0/Barline`，**惰性 `create_dir_all`**（`local_dirs` 空则零 IO、连目录都不建）；文件名 = `locate::candidates(track)[0] + ".lrc"`（**机械复用，禁止自造命名**）；保留名 `CON/PRN/AUX/NUL/COM1-9/LPT1-9` 拦截；stem > 120 UTF-16 或全路径 > 259 → **放弃不截断**（截断后与候选名不等，等于白写）。
  - **写入原子性**：`OpenOptions::create_new(true)`（唯一能原子表达「不存在才创建」的原语）；`AlreadyExists → Skipped(Exists)` 静默；**不用 `.tmp` + rename**（`std::fs::rename` 在 Windows 是 `MOVEFILE_REPLACE_EXISTING`，必覆盖）；写一半失败**只删自己刚建的那个**。
  - **内容纯净**：`render_lrc` **只读 `line.start_ms` 与 `line.text`，绝不读 `line.words`**（`LyricKind::Line` 的 `words` 是插值产物，写出去等于造假的逐字时间戳）；不写 `[offset:]`（`parser.rs` 已把 offset 内联进 `start_ms`）；负 `start_ms` 钳 0；元数据空则整行省略、值内 `]`/CR/LF 换空格；UTF-8 **无 BOM** + CRLF；空行保留。
  - **触发点**：`on_online` 命中分支，**`emit(LyricReady)` 之后**（先显示后落盘）、**代次守卫之后**（换歌绝不写到上一首）、用 `out.req.track` 而非「当前播放」；失败只 `log_warn!`，**不改事件、不碰缓存**；`Worker.wrote`（≤`WRITE_BACK_DEDUP_CAP=64`）做本次运行去重，跨进程幂等靠 `create_new`。
  - **配置与设置页**：`LyricsConfig.write_back: bool` 默认 `true`（靠容器 `#[serde(default)]`，旧配置自动得 true）；`LyricRequest.write_back` 随请求传给歌词线程；`settings/bridge.rs` 白名单加 `lyrics.write_back`；设置页开关 + 中文静态说明（含「30 天缓存优先、手改需等过期/清缓存」）+ zh/en 双语 + 置灰联动。
  - **交付数据**：`494 个 #[test] / 493 passed / 0 failed / 1 ignored`（v1.6.1 基线 472 → +22；**既有 472 条零放宽**，全仓代码删除行仅 9 行：6 版本号 + 1 JS 数组 + 1 import + 1 `Arc::new`→`Arc::clone`）；check/clippy/test/build 全绿、clippy 0 警告；版本 5 处 + `Cargo.lock:7` 同步 1.7.0；包体 1,113,600 B、FileVersion/ProductVersion 1.7.0；历史 34 包只增不删。
  - **QA 结论 `PASS-WITH-NOTES` / 路由 `NoOne` / 可出包，14 项全 PASS**：三条红线实证守住——① **`query_fast_path`（原 `service.rs:288-377`）逐字节零改动**（新旧 SHA256 同为 `e9b50990…`，2877 字符）；② `locate/provider/cache/parser/query/interp` + `render/platform/media/main` **零 diff**；③ 绝不覆盖（`create_new` + 唯一 `remove_file` 在「刚建即写失败」分支 + `create_dir_all` 在所有 Skip 检查之后）。
  - **QA 发现并已修（出包前）的缺陷**：`build_file_name` 原有 `trim_end_matches('.')`，理由是「Windows 会吃掉尾随点」——**实测证伪**：我们总加 `.lrc`，文件名末尾是 `c`，Windows 只裁**整个文件名末尾**的点，`abc....lrc` 的点全保留、`file_stem` 恰等于候选名。裁剪后 `file_stem` 变 `abc` ≠ `abc...` → **写了永远搜不到**，且该死文件可能被「歌名恰好 `abc`」的另一首歌命中 → **显示错歌词**。已删除裁剪，并把 W5 从「断言名字被裁掉」改为**闭环断言**（落盘后立刻 `locate::find` 必须命中同一路径）。
  - **留档（v1.8.1 待办）**：① `W13`/`W14` 断言恒真、无鉴别力（真正保证来自 `save` 的执行顺序），补强为「传真实目录但 Empty/NoDir 跳过 → 目录不存在」；② 测试脚手架 `service.rs` 的 `worker()` 把 cache 指向真实 `%LOCALAPPDATA%\Barline\lyrics-cache`（**pre-existing**，非本轮引入），应改指 `%TEMP%`（**v1.8.0 已解决**：QA 复核 `worker()` → `cache(tag)` → `std::env::temp_dir()/barline_service_*`，且 `%LOCALAPPDATA%\Barline\lyrics-cache` 在此机器上根本不存在，本条关闭）；③ 「清空歌词缓存」按钮可作为 R1 的根治手段，建议纳入 v1.8。
  - **已知取舍（用户已接受）**：落盘同步 IO 跑在歌词线程（`emit` 已先发，UI 无感）；`wrote` 在 `save` **之前**记账（一次 Failed 后本次运行不重试）；落盘文件**只增不减**（绝不删用户目录文件，故无法做配额，听 1000 首约几十 MB）。
- **G5.1 查询顺序可切换 + 落盘补齐 + 缓存可见（用户验收 v1.7.0 时反馈）→ v1.8.0 · ✅ 已真人验收通过（2026-11-05 00:34，用户「没问题，继续下一个」）**：用户验收时提四问，拍板为：① **默认仍磁盘缓存优先**，新增设置项让用户自己切「本地文件优先」；② 设置页显示歌词磁盘缓存的真实路径；③ 「只要查到歌词、本地目录没有就存一份」；④ 落盘名改「**歌名 - 歌手**」（带空格），英文**保留原始大小写**。
  - **根因（用户真实痛点）**：v1.7.0 的落盘只挂在 `on_online` 一处，内存/磁盘命中都是「命中即 return」→ 用户机器 `本地歌词目录\Barline\` 始终为空、`lyrics-cache` 已 93 个文件（第二首起全被缓存拦住）。这不是"没实现"，是**触发面只覆盖了三条路径里的一条**。
  - **顺序（用户二次裁决，推翻了 PRD 初稿）**：`local_first=false` → `[Mem, Disk, Local]`（v1.7.0 原序）；`true` → `[Local, Mem, Disk]`——**本地排在内存之前**，手改 `.lrc` 立刻生效、**含正在播放的那一首**（若内存在前，正在播的曲子会一直吃旧副本）。一个 `Step` 数组 + 单循环表达两条顺序，**禁止两份逻辑**；本地命中后 `cache.put` 同时刷内存与磁盘缓存。
  - **落盘收口**：`maybe_write_back` 是唯一出口，`source == "local"` 与 `!write_back` 两个判定各只存在一份（QA 用 `grep` 复核 `if !req.write_back` 全仓仅 1 处）；`get_mem`/`get_disk` 改返回 `Hit{lyrics, source}`，**磁盘回填内存必须带 source**（否则「第一次播放对、第二次播放错」，只有 `C2` 能锁住，`s8` 锁不住——QA 反向实验实证）。
  - **命名**：`display_stem` = `sanitize(歌名) - sanitize(歌手)`（保留原始大小写与全角；Windows 非法字符 `\ / : * ? " < > |` 与控制字符 U+0000–1F → 空格；空白折叠 + trim）；`check_stem` 统一做「保留设备名 / >120 码元 / `n(stem) ∈ candidates`」三段校验，主/回退共用。主路径因 `Unsearchable` 失败且 artist 非空时**回退 title-only**（写 `歌名.lrc`，即 v1.7.0 既有行为、`candidates` 第 3 项），回退仍不过则 Skip —— **绝不写永远命中不了的死文件**。`locate.rs` 本轮**零 diff**。
  - **设置页**：`lyrics.local_first` 开关（默认关；文案写明两种顺序的代价）+ 只读缓存卡片（路径 / 文件数 / 大小 / 「点击打开文件夹」，**进入歌词分区读一次**，零定时器、零轮询）；新动作 `lyricsCacheStats` + 新消息 `cacheStats` + `SettingsService::post()`——**绝不能用 `push()`**（它会覆写 `last_state`，窗口重建后补推 cacheStats、设置页控件全空白，且只在关窗再开时复现）；动态占用量元素**不带 `data-i18n`**（否则被 `render()` 打回模板串）。
  - **交付数据**：`523 个 #[test] / 522 passed / 0 failed / 1 ignored`（v1.7.0 基线 494 → 净增 29）；check/clippy/test/build 四连全绿、clippy **0 警告**；版本 5 处（`Cargo.toml:3` + `barline.rc:15/16/28/33`）+ `Cargo.lock:7` 同步 1.8.0；包体 **1,125,376 B**、FileVersion/ProductVersion 1.8.0；历史 35 包只增不删。
  - **QA 结论 `PASS` / 路由 `NoOne` / 14 项全 PASS / P0–P2 零缺陷**（4 条 P3 为文档同步）：独立写 9 条探针 + **2 条反向验证**（把层序改回 `[Mem,Disk,Local]` → `s9` 立刻红；把 `get_disk` 回填 source 清空 → `C2` 红），均按 SHA256 字节级还原，工作区零残留。结构红线 `locate/provider/parser/query/interp/render/platform/media/storage` 全部零 diff。
  - **v1.7.0 的「`query_fast_path` 逐字节零改动」承诺本轮主动作废**：替代判据 = E1–E6 行为等价（事件序列、三条命中日志逐字符不变、同一 `t0`、`LyricLoading` 位置、最多一次 `LyricReady`、`false` 分支数组字面量 `[Mem,Disk,Local]`）+ S1–S3 结构红线（ARCH §2.4）。旧 `s5`（意图「防止有人把落盘钩子塞进 query_fast_path」）**语义反转**并改名 `s7_disk_cache_hit_does_write_back`，注释写明「旧意图正是 v1.7.0 漏掉 A6.2/A6.3 的根因」——**这正是"测试全绿但功能没做"的活样本**。
  - **留档（不阻塞）**：① `wrote` 同会话去重 → 「删掉 `Barline/` 后同会话重播」不重写（幂等靠 `create_new` 兜底；真机验 A6.2 需重启后重播，或听满 65 首触发 `clear`）；② `local_first=true` 每切歌付一次 `index_dir`（深度 ≤4、每目录 ≤5000 条目），超大歌词目录下歌词显示会推迟；③ `source=local` 的缓存条目在用户删掉 `.lrc` 后不自愈，等 30 天 TTL；④ 缓存优先时若 `local_dirs[1]` 已有 `.lrc`，仍会在 `local_dirs[0]/Barline/` 写一份副本（F1/F3，开本地优先即消失，用户已接受）；⑤ 占用行无静态「已用空间」标签（与 i18n 模板设计一致）；⑥ `settings/assets.rs` 新增 1 条计划外测试（把 ARCH 的读码项变成断言：`lyrics-cache` 零字面量、零定时器、动态元素无 `data-i18n`）；⑦ **artist 只含标点（`／`、`?`）时 title-only 回退会与「歌名真的就叫 X」的另一首歌同键**：两首不同曲争同一个 `歌名.lrc`，`create_new` 后到者 `Skipped(Exists)`、先到者内容胜出。**v1.7.0 同样会撞**（当时也只写 `candidates[0]`），非本轮回归——以后若见「显示错歌词」，先查这条，别当新 bug 追。
- **G6 清理（问题 6）→ v1.8.1 · ✅ 已真人验收通过（2026-11-05 01:29，用户「没问题，继续下一项」）**：范围＝死代码 + 死配置 + 死事件变体 + 未引用资源 + 失实注释。**先盘点、后用户确认、再动手**。
  - **流程**：架构师全仓盘点 → `docs/CLEANUP-G6-清单.md`（176 行，A 确定死 29 项 / A-2 仅测试引用 10 项 / B 需用户点头 10 项 / C 必须留 9 组 / D 改写 16 项）→ 用户拍板四类（截图全删 / 死配置删 / 死事件变体删 / deliverables 只删副本留报告）→ 工程师执行 → QA 独立回归 14 项。
  - **删除量**：A-1 29 项 + A-2 10 项 + **级联 4 项**（`open_url`、`cover_stats`、`CoverCache.hits/misses`、`FallbackInfo` —— 唯一调用者被本轮删掉，不删就是新一轮死码）；`AppEvent` **24 → 15** 变体、`Effect` **15 → 9**；配置删 `Config.window` 整棵（6 字段）+ `accent_source`/`accent_custom`/`AccentSource` + `LyricsConfig.mode`；`docs/screenshots` **74 张 BMP（81 MB）**；`deliverables/` 的 LICENSE/README 两个副本；Cargo feature 摘 `Win32_UI_Accessibility` + `Foundation_Collections`（**实测非传递依赖**：先摘一个 build 绿、再摘第二个仍绿；证据面是全仓零 `SetWinEventHook`、零 `GetSessions`）。净 **-918 / +584 行**，`src/` 33,436 → 32,910。
  - **保留（C 档 9 组，一个没删）**：`tray_click_activate`（用户裁决留档）、`start_msg_timer`/`kill_msg_timer`（声明预留）、`fixed_width` / `dismiss_bg_rect`（布局契约锚点）、`has_pending_click`/`is_seeking`（状态机唯一观测窗口）、`DISMISS_*` 四常量、`HotkeyAction::ALL`、`WM_BARLINE_WIDGET_BASE`；另保留 35 个 exe、8 份历史报告、`scripts/probe-widgets.py`、`Effect::RefreshTrayMenu`（修复挂点）。
  - **A-2 的处置修正（工程师判断正确，我采纳）**：`compute`/`compute_ex`（44 处调用点）与 `file_name_for`（16 处测试引用）**不删测试，改迁调用点到正式路径 `compute_full(...)` / 私有 `build_file_name()`** —— 那些测试锁的是布局几何契约与落盘命名契约，按字面「删符号连带删测试」会一次抹掉两块核心覆盖。符号确实删干净（零残留），测试反而更强（不再有"只服务测试"的入口）。
  - **D 档改写 15 处**：`event.rs:203` 的「300ms 去抖」（实际是同步写盘 + 原子 rename）、`config/mod.rs:181` 的「重启后恢复」（6 字段零读写）、`event.rs:9-11` 的「M2–M5 预留」（实为已被方法直调取代的残骸）、`HostMsg` 已搬迁、`layout.rs` 的「与调用点零改动」等。**W13 从恒真断言改为哨兵文件 + 目录条目数**、**W14 把「目录不存在/目录空」拆成两条**、**s1 从自指改为字面量** `"测试曲目α - 无名歌手α.lrc"`。**D-6 不做**（`State::progress_fraction` 与 `painter.rs:911-923` 两份进度实现合并属行为重构、会改像素，留档下轮）。
  - **交付数据**：`515 个 #[test] / 514 passed / 0 failed / 1 ignored`（v1.8.0 基线 523 → -8，**消失的 8 条逐条对上被删符号，零放宽，5 处断言增强**）；check/clippy/test 全绿、clippy **0 警告**；版本 5 处 + `Cargo.lock`；包体 **1,092,096 B**（比 v1.8.0 小 32.5 KB）；exe **36 个只增不删**。
  - **QA `PASS-WITH-NOTES` / 路由 `NoOne` / 14 项全 PASS**：最硬的一条是**用用户真实的 `config.json`（带全部已删键）实跑**——`serde_json::from_slice::<Config>` 成功，`local_dirs`（`E:\APP\曲栏Barline\本地歌词目录`）/ `storage.data_dir` / `hotkeys` / `background_mode` / `write_back` **全部值都对**，回写后不再出现已删键；反向验证加 `#[serde(deny_unknown_fields)]` → 兼容性测试立刻变红（证明它真被锁住）。另用 `dead_code` 探针做 **HEAD vs 当前树的集合对比：当前死码是 HEAD 的严格子集、零新增**。
  - **方法论教训（重要）**：QA 第一轮用 `rg` 查残留，**环境里没有 `rg`**，命令报错后所有项都打印 0 —— **一次全面的假绿**。它自己发现并换成 Grep 引擎重做了全部检查。**「工具不存在 → 检查静默通过」与「测试全绿但功能没做」是同一类失效**，凡是靠外部命令做判定，必须验证命令本身能跑通（`rg --version`）。
  - **留档（不阻塞）**：① **pre-existing 死码 30 项**（HEAD 就存在，不属本轮清单，下轮候选：`app::sink/msg_hwnd/is_enabled`、`LOG_MAX_BYTES`、`net::as_str/get/get_with`、`provider::{new,query,query_with_source}`、`media::LyricSource::Wasapi` + `Track.source`、`layout::{inset,lyric_w,edge}` 等）；② **语言切换需重启**（设置页改 `general.language` 后 `App::lang` 不更新、托盘不刷新，`Effect::RefreshTrayMenu` 是挂点）；③ D-6 两份进度实现；④ 截图删除后 `README.md` 与 6 份报告的图片引用已失效（README 已换成「截图归档于 git 历史」的说明，`git show 890c07f^:docs/screenshots/` 可取回）。
  - **`#![allow(dead_code)]` 的真相**：`src/main.rs` 的注释原写「M5 收尾时应移除」，实际 M1–M5 全发布后它仍在 → **死码不会被编译器发现，G6 这类清理只能靠人工逐符号盘点**。已把注释改成如实表述并列出三条真正被兜住的留档项。

- **G7 媒体会话黑名单（用户 2026-11-03 追加）→ v1.9.0 · ✅ 已出包 `deliverables/曲栏_Barline_v1.9.0.exe`（待真人验收）**：`docs/PRD-G7-黑名单.md` + `docs/ARCH-G7-黑名单.md`（已入库，**待实现**）。
  - **四项已拍板决策**：① 排最后（不打断 G3~G6）② 按**进程名**匹配 ③ 语义 = **完全忽略**（不渲染/不查歌词/不响应播控；唯一会话被拉黑 → `NoSession` 空态）④ 入口三处（设置页分区含检测列表 + 托盘「拉黑/恢复当前应用」+ 清单增删）。
  - **U1=B 的实现口径（已定稿）**：反查结果**落盘**到 `config.json` 的 `media.app_names`（`BTreeMap<aumid_lower, process_name>`，**上限 64 条**，满时淘汰字典序最小键 = 确定性 + 自愈 + 零额外状态，不做负缓存，`blacklist_clear` 不清它）；判定三级回退「缓存 → 静态表 → AUMID 末段」全内存；预算常量 `ENUM_LOOKUP_BUDGET_MS=120` / `ENUM_TOTAL_BUDGET_MS=200` / `ENUM_MAX_LOOKUPS=16` / `ENUM_MAX_AUDIO_SESSIONS=64`（单点定义在 `config::defaults::limits`）；静态表已命中的会话**不反查**（典型机器反查 0 次）。
  - **已知残留**：从没出现过、且不在静态表的进程名，手输时无法验证 → 温和提示 + 引导用「检测到的应用」，**不许静默失效**。
  - **硬约束（性能红线）**：判定必须纯内存字符串比对；应用枚举只允许「进入黑名单分区」与「点刷新」两处，禁止定时器/后台轮询/切导航枚举；枚举只读会话列表 + AUMID，禁读元数据/时间线/controls。
  - **关键修正（架构核实）**：项目**没有 WASAPI 兜底媒体链路**（`wasapi.rs` 只有音量、`FallbackSession` 零发射点），只过滤 SMTC 单链路；`player_map::resolve()` 仅 22 条静态表、表外进程名为空，故需 U1=B 的缓存；`app.rs:1911` 是**同步写盘**（非 300ms 去抖）。
  - 任务分解 **T01~T05** 见 ARCH 文档，规模约 **9~10 人·天**（T01 数据层 M/偏重、T02 过滤链 L、T03 设置页 **XL**、T04 托盘+i18n M、T05 回归 S+）；**验收 10 条**（7 条功能 + W1「反查拿到真实名」/ W2「缓存跨重启仍命中」/ W2b「手改 json 不炸」/ W3「失败不静默三档对照」）；**另需注意 execute() 入口也要拦播控命令**（PRD 最初漏了，不拦热键仍能控制被拉黑应用）。
  - **v1.9.0 实现实况（2026-11-05，已出包待验收）**：
    - **流程**：开工前先做两路核对（ARCH-G7 写于 v1.7.0 时代，中间隔了 v1.8.0/v1.8.1 两轮大改）——架构师对齐审计（`docs/ARCH-G7-对齐审计.md`，26 条偏差，**内容级失效仅 2 处**：Config 示例含已删的 `window` 字段、`GetSession(i)` 笔误应为 `GetAt(i)`，其余全是行号漂移，设计主体 95% 可用）+ PM 界面复核（`docs/PRD-G7-现状复核.md`，10 处调整：标题计数不挂 `data-i18n`、刷新按钮挪出 `h2`、行结构叠 `.label`/`.desc` 等）。**因限流，首轮审计由主理人亲自完成一半**（发现 feature 缺失与 `push_transient` 重复），再交架构师补全成文。
    - **前置 5 项（全部并入 T01–T03）**：① Cargo.toml **加回 `Foundation_Collections`**（枚举走 `GetSessions()` → `IVectorView::Size()`/`GetAt()`，G6 摘掉了它）；② 文档提议的 `SettingsService::push_transient()` **不做**——v1.8.0 的 `post()` 语义完全相同（不碰 `last_state`），直接复用；③ `blacklist.hint.match` 用 PRD 版文案；④ 清空确认用 `window.confirm()`（ARCH 专项分析：原生弹窗在 WebView2 STA 会嵌套消息泵）；⑤ 性能基线 v1.8.1。
    - **交付数据**：`556 个 #[test] / 555 passed / 0 failed / 1 ignored`（v1.8.1 基线 515 → **+41**，既有零放宽零删改）；check/clippy/test 全绿、clippy 0 警告；`+1944 / −68` 行；新文件 `src/media/blacklist.rs`（~700 行含 20 测试）；版本 5 处 + lock 1.9.0；包体 **1,159,680 B**；exe **37 个只增不删**。
    - **四条红线 grep 证据（QA 独立复核通过）**：① 判定纯内存——`blacklist.rs` 零 `fs::`/`OpenProcess`/`CoCreateInstance`/`RequestAsync`，判定先 clone 快照再算、锁不嵌套；② 枚举两处触发——`requestDetect()` 恰 2 个调用点（activateNav 带 `detectedLoaded` 守卫 + 刷新钮）、`setTimeout` 全页仍只 1 处、`setInterval` 0；③ `execute()` 拦播控——命中即 return **不发 CommandResult**（不发假成功），`Refresh`/`Quit`/`SetPaused`/`EnumerateApps` 在拦截点之前不误拦；④ ignored 轮次——`Cadence::consume` 与 `tick_ignored` 不碰 `zero_streak`/`timeline_backoff`/`prev_*`，专测连续 200 轮 consume 后退避状态机逐字段不变。
    - **QA `PASS-WITH-NOTES` / 路由 `NoOne` / 14 项全 PASS / P0–P1 零缺陷**：反向验证 2 条（`consume()` 注入 `zero_streak=0` → 红线测试红；`ENUM_LOOKUP_BUDGET_MS` 120→0 → 常量钉死测试红），均 SHA256 字节级还原。
    - **工程师与审计的 11 条偏差（全部有据，主理人复核采纳）**：`Size()` 不是 `GetCount()`（审计也漏了，编译器抓出）；`ENUM_MAX_AUDIO_SESSIONS=64`（裁决口径，ARCH 原文 32）；`apply_blacklist` 加第 4 参 `&MediaIndex`（KeyRecognition 需要）；`normalize_entry` 先剥引号再判非法字符（否则 PRD 自己的路径粘贴示例会被拒）；`MediaIndex::new/set` 输入先归一化（未归一化缓存键大小写查不到）；解绑后补一次 `Effect::Media(Refresh)`（否则恢复最坏 ~2s，保证 ≤1s 验收）；`tick_ignored` 的 probe-Err 分支保持忽略态不发事件（从紧执行）；托盘中英括号全/半角分支；`clamp_label` 总长 ≤40 字符含省略号；检测列表空态双文案细分（未检测过 vs 检测过但空）。
    - **QA 留档 P3（均不阻塞）**：D2 空身份会话不显示灰态行（PRD P0-04 要灰态，实现选择不出现——少一条不可用行，语义无害，**验收时知会用户**）；D3 命中日志文案与 PRD 示例形态不同（信息量等价）；D4 托盘 toast 用 `push()` 覆写 `last_state`（既有模式非本轮引入，`settings.open()` 自愈）；D5 媒体线程死亡时刷新按钮不复位（程序级故障场景）；D6 `BadPath` 变体无专测。
    - **真机走查清单（验收重点）**：① 拉黑当前应用 → 小组件立即消失、热键/托盘播控失效、歌词不查；② 解绑 → 小组件重现（机制保证 ≤1s，工程师标注最坏可能 ~2s，**掐秒表**）；③ 唯一会话被拉黑 → NoSession 空态；④ 进入黑名单分区 → 检测列表出现且 `smtc.props` 60s 计数下降；⑤ 反复点刷新 → `config.json` mtime 不变（仅学到新名字才落盘）；⑥ 表外 UWP 应用 → W1/W2/W3 三条走查（反查真实名、重启后仍命中、手改 json 不炸）；⑦ 全部拉黑后 60s 统计行看 CPU。

### 3.3 待办 A：会话音量（第二轮问题 6）—— 用户裁决「暂时禁用，以后有需要再改」
- 失败在音频会话枚举/匹配环节且无日志；未排嫌疑 4 项：`IMMDevice::Activate` 枚举为空 / `process_image_name` 空串 / 设备归属非默认 endpoint / COM apartment。
- 入口：`src/media/wasapi.rs::session_simple_volume`；禁用点 = `Config::load` 归一化 + bridge 强制 `volume.scope=System` + 设置页 select 置灰。

### 3.4 待办 D：汽水音乐托盘态封面激活 —— 用户裁决「搁置」
- CEF 隐藏后任何外部 `ShowWindow`/前台技巧都会让输入管线挂起；有效但抢鼠标的方案被否。现行：托盘隐藏态不激活。
- 入口 `src/media/launch.rs::tray_click_activate`（`#[allow(dead_code)]` 留档）；续修方向：PostMessage 逆向 NotifyIconHost / deep-link / 光标静止模拟点击。

### 3.5 小尾巴与存量（不阻塞）
- `MIN_PICK_SCORE=60` 偏保守；若「明明有歌词却不显示」可降 40。「冲突一键换组」未做（与用户决定冲突，倾向不做）。
- M2 时代 8 项无真机证据（勿视为通过）：播控真实改状态 / 拖动跳转 / 封面激活前台 / 滚轮 HUD / 多显示器 / DPI / 四方位 / 自动隐藏。
- GitHub Release 发布、LICENSE 版权人实名（现为占位）、设置页标题栏明暗跟随、热键日志冗余清理。
- 歌词未覆盖：冷门曲/UP 主原创/BGM、只有 `plainLyrics` 无时间轴、中外文歌名差异过大。
- **待验证项**：v1.4.4 多显示器 `widget_monitor` 在「窗口被第三方跨屏搬走」场景下仍可能短暂陈旧（自愈，可焊死在 `ensure_on_top` 漂移分支，零成本）。

---

## 4. 其他需记录

### 4.1 本机环境（踩过的坑）
- **数据目录已被用户改到** `E:\APP\曲栏Barline\数据目录`；日志跟在那里（不是 `%LOCALAPPDATA%\Barline`）。排查前先确认日志在哪。
- 构建必须用 **PowerShell 工具**跑 `build.ps1`；Bash 里调 powershell 会被拦。
- `git worktree` 检出 `.ps1` 会触发 working-tree-encoding 转码失败；重建历史包用「detached checkout → build → 拷 exe → 回 main」。
- push 遇代理 502 时走本机代理：`git -c http.proxy=http://127.0.0.1:7890 -c https.proxy=http://127.0.0.1:7890 push`。
- 用户日常用**网易云 / 汽水音乐 / Edge（B 站）**测试；网易云 SMTC 恒报 `(duration=0, position=0)`，进度条/时长对它不可用。

### 4.2 诊断日志关键字（排查时直接 grep）
| 关键字 | 含义 |
|---|---|
| `[perf] <段> 耗时=NNNms` | 分段慢记录（tick.capture/probe/ontop/backdrop 与 sampler.capture ≥16ms；paint.frame/cover.decode/msg.mouse ≥50ms；smtc.* ≥8ms）。段名：`tick.capture/probe/ontop/backdrop`、`paint.frame`、`msg.mouse`、`sampler.capture/analyze`、`cover.decode`、`smtc.tick/playback/props/timeline/controls` |
| `Xs 内已记满 10 条，后续丢弃` | 该段 60s 限流触发 |
| `[perf] smtc 轮询统计 60s：…` | 每分钟调用量（playback≈120/min 正常；暴增=事件风暴）；**新增 `capture=N 慢M capture最慢=Xms`**（抓屏次数/慢次数/最慢 ms，验证降载是否生效：正常静止态 ≈20/min） |
| `前台全屏 WxH（rect=…）→ 暂停静止态心跳采样` | 同屏全屏命中，心跳跳过（退出时另一条 `退出前台全屏 → 恢复 3s 心跳背景采样`） |
| `歌词查询：… 耗时=Xms 超时=?` | 在线歌词结果与耗时；`丢弃过期在线结果` = 代次守卫生效 |
| `歌词内存/磁盘命中：… 耗时=Xms` | 缓存命中耗时（应毫秒级） |
| `时间线连续 3 次为 (0,0)…退避` | 网易云式退避生效 |
| `热键已注册/失败/触发` | 热键链路（1409=被占用） |
| `widget rect 变化 / 位置漂移 / 几何变化` | 窗口位移归因 |
| `文字色翻转` | 黑白切换路径 |
| `截图态` | 截图遮罩冻结生效范围 |

### 4.3 交付包现状（`deliverables/`，只增不删）
现存 **24 个 exe**：`v1.1.10` ~ `v1.1.22`、`v1.2.0` ~ `v1.2.3`、`v1.3.0`、`v1.3.1`、`v1.4.0` ~ `v1.4.4`（另有多余的 LICENSE/README 副本，G6 清理候选）。
⚠️ `v1.0.0`、`v1.1.0` ~ `v1.1.9` 已不在磁盘（早前清理误删，目录在 `.gitignore` 内无法恢复）；如需补齐用 4.1 的 detached checkout 方式，不补也不影响使用。

### 4.4 测试与验收口径
- 单测覆盖**纯逻辑**（布局、主题/亮度状态机、歌词清洗与择优、配置迁移、SMTC 分层节奏/退避/锚点、perf 阈值、全屏判定 `is_fullscreen_window`、限流器 `RateLimiter`、抓屏统计 `CaptureStats`、心跳门控 `idle_backdrop_due`/`should_skip_idle_backdrop`/`same_monitor`）。渲染像素、真实热键、卡顿体感必须**用户真机看**。
- 每组交付前主理人自己跑 `check`/`clippy`/`test` 并读测试二进制汇总（不信工程师自述）。
- 1 个 `#[ignore]` 联网用例（lrclib），不联网时属正常跳过。

---

## 5. 接手须知（新对话从这里开始）

1. `git status` / `git log --oneline -8` 确认干净与进度（以本文档头部「最后更新」为准）。
2. 团队：`software-barline-14`（主理人 + `software-engineer` + `software-qa-engineer`）。新会话需 `TeamCreate` 重建；派发时 prompt **必须自包含**（工作目录、任务、§2.1 铁律、版本 bump 5 处、`build.ps1` 全链、单提交、汇报格式）。
3. 每项完成：主理人抽验 diff → 出包 `deliverables/曲栏_Barline_vX.Y.Z.exe`（不删旧包）→ 播报「任务清单 + 进度」→ **停下等验收**。
4. 工程师不启动 exe、不动 `deliverables/`、不 commit（提交与出包归主理人）；用户自测用 `启动曲栏.bat` 或包。
5. 用户报「没反应/卡顿」类问题时：**先读日志**（§4.2）+ 设计对照实验，别先改代码；卡顿问题直接进 **§3.1 专档**。
6. **当前状态**：G3 右上角 × **已真人验收通过**（v1.5.4 定稿：字形 6px / 命中 14 / 笔画 1.2 / 与三键同墨色）。**G3.5 全屏冻结 v1.5.5 已出包，等用户真人验收**（重点：全屏进出、后台占用、热键切歌、退出不闪旧帧），详见 §3.2。**下一组是 G4 悬停/按下反馈统一（v1.6.0）**——含 v1.5.1 QA 遗留的 × 悬停底色口径（`theme.text@0x14` 与全窗 `hover_bg`/`on_light` 不同源，透明背景下全局悬停消失但 × 仍亮）。G5 歌词落盘、G6 清理、G7 黑名单依次排队，别提前开工。
7. 收尾：全部完成后跑全量 `check/clippy/test`，同步文档，写一条记忆到 `.workbuddy/memory/`。
