# 曲栏 Barline — 交接文档（progress.md）

> **用途**：新开对话 / 接手者的唯一入口。只读这一份即可恢复上下文；细节再查 `docs/`（PRD / ARCHITECTURE / QA 报告）。
> **最后更新**：2026-11-04 03:00 · **G3 右上角 × 已出包 v1.5.4（待真人验收）** · **G3.5 全屏遮挡修复已立项（v1.5.5，插在 G4 前）** · G2 闭环 · G7 黑名单待实现（v1.8.0） · 全量回归绿（428 测试）· 工作区干净。

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
- **G3.5 全屏遮挡修复（用户 2026-11-04 追加）→ v1.5.5**：全屏应用（电影/播放器/浏览器 F11/游戏）时小组件**必须消失**，不得压在全屏窗口上方；退出全屏自动恢复。
  - **现象（用户原话）**：「其他窗口全屏时，小组件会左移，且小组件会盖在全屏的窗口上方」——两件事：① z-order 压过全屏窗口；② 同时发生**位置左移**。
  - **已存在的信号（零新增跨进程调用）**：`platform::capture::foreground_info()` 每次都返回 `fullscreen`（`is_fullscreen_window(rect, monitor)`）与 `monitor`，v1.4.4（G2-B2）起**每秒随心跳刷新**，当前**仅**用于「同屏全屏时暂停静止态背景采样」。`AppState::foreground_fullscreen` / `AppState::foreground_monitor` 已就绪，`compute_visible()` 却没用它们。
  - **「左移」根因已定位（本机日志实证，2026-11-04 02:47~02:48）**：全屏开始时任务栏**托盘区变宽 28px**（`tray_left 2130 → 2102`、`tray 宽 430 → 458`），`geometry::place_widget` 按 `tray_left` 定位 → `target x 1922 → 1894`（左移 28px）；退出全屏后几何自动恢复（`2102 → 2130`、`1894 → 1922`）。**跟随逻辑本身没 bug，问题是全屏时不该显示**。因此本轮只需解决"全屏时隐藏"，不需要额外修正定位。
  - **修复方向（待架构确认）**：`compute_visible()` 增加「同屏全屏 → false」；全屏翻转时触发 `sync_visibility()`；`ensure_on_top()` 在全屏期间跳过（否则每秒仍会把窗口顶到最前）；`widget.raise()` 同理。**不得新增轮询频率**（全屏信号随既有 1s 心跳免费刷新；1s 延迟可接受）。
  - **不可破**：副屏全屏**不得**隐藏主屏小组件（沿用 v1.4.4「同屏才生效」判据）；不得新增轮询频率。
- **G4 悬停/按下反馈（问题 3 + 5）→ v1.6.0**：封面悬停＝变暗蒙层 + 左上/右下双箭头（点击整块封面＝打开播放器，沿用 `ClickCover`）；四按钮（含 ×）用户选「**柔和光晕 + 下沉**」——悬停：白底黑影/黑底白光双层低透明度光晕；按下：光晕收紧、图标下沉 1px 并缩到 94%。**× 与四按钮共用同一套悬停/按下状态机与绘制函数**（G3 起设计好复用）。
- **G5 在线歌词落盘（用户追加）→ v1.7.0**：写到「歌词目录」**第一个**目录下的 `Barline/` 子目录；文件名 `歌手 - 歌名.lrc`（非法字符替换 `_`）；**已存在一律跳过、绝不覆盖**；只写**行级** LRC + `[ti:]/[ar:]/[al:]`，**不写逐字标签**。副作用：导出后走「本地文件命中」，优先级高于缓存，清缓存/换机不丢。
- **G6 清理（问题 6）→ v1.7.1**：范围＝**死码 + 未引用资源**。已确认在列：`hotkeys.reset1` 死键（`assets/ui/index.html:418/462`）、`SettingsService::is_open()`（`src/settings/mod.rs:93` 零调用）、`event.rs:201` 去抖注释失实、`Effect::RequestBackdrop`/`Effect::MoveWindow` 零发射点、`deliverables/` 多余的 LICENSE/README 副本；删 `assets/`/`docs/` 资源前**必须先把清单给用户确认**。`deliverables/` 的 exe 永不动。

- **G7 媒体会话黑名单（用户 2026-11-03 追加）→ v1.8.0**：`docs/PRD-G7-黑名单.md` + `docs/ARCH-G7-黑名单.md`（已入库，**待实现**）。
  - **四项已拍板决策**：① 排最后（不打断 G3~G6）② 按**进程名**匹配 ③ 语义 = **完全忽略**（不渲染/不查歌词/不响应播控；唯一会话被拉黑 → `NoSession` 空态）④ 入口三处（设置页分区含检测列表 + 托盘「拉黑/恢复当前应用」+ 清单增删）。
  - **U1=B 的实现口径（已定稿）**：反查结果**落盘**到 `config.json` 的 `media.app_names`（`BTreeMap<aumid_lower, process_name>`，**上限 64 条**，满时淘汰字典序最小键 = 确定性 + 自愈 + 零额外状态，不做负缓存，`blacklist_clear` 不清它）；判定三级回退「缓存 → 静态表 → AUMID 末段」全内存；预算常量 `ENUM_LOOKUP_BUDGET_MS=120` / `ENUM_TOTAL_BUDGET_MS=200` / `ENUM_MAX_LOOKUPS=16` / `ENUM_MAX_AUDIO_SESSIONS=64`（单点定义在 `config::defaults::limits`）；静态表已命中的会话**不反查**（典型机器反查 0 次）。
  - **已知残留**：从没出现过、且不在静态表的进程名，手输时无法验证 → 温和提示 + 引导用「检测到的应用」，**不许静默失效**。
  - **硬约束（性能红线）**：判定必须纯内存字符串比对；应用枚举只允许「进入黑名单分区」与「点刷新」两处，禁止定时器/后台轮询/切导航枚举；枚举只读会话列表 + AUMID，禁读元数据/时间线/controls。
  - **关键修正（架构核实）**：项目**没有 WASAPI 兜底媒体链路**（`wasapi.rs` 只有音量、`FallbackSession` 零发射点），只过滤 SMTC 单链路；`player_map::resolve()` 仅 22 条静态表、表外进程名为空，故需 U1=B 的缓存；`app.rs:1911` 是**同步写盘**（非 300ms 去抖）。
  - 任务分解 **T01~T05** 见 ARCH 文档，规模约 **9~10 人·天**（T01 数据层 M/偏重、T02 过滤链 L、T03 设置页 **XL**、T04 托盘+i18n M、T05 回归 S+）；**验收 10 条**（7 条功能 + W1「反查拿到真实名」/ W2「缓存跨重启仍命中」/ W2b「手改 json 不炸」/ W3「失败不静默三档对照」）；**另需注意 execute() 入口也要拦播控命令**（PRD 最初漏了，不拦热键仍能控制被拉黑应用）。

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
6. **当前状态**：G3 右上角 × 已出包 **v1.5.4**（字形 6px / 命中 14 / 笔画 1.2 / 与三键墨色一致），**等用户真人验收**；验收通过即闭环 G3。**下一组是 G3.5 全屏遮挡修复（v1.5.5）**——范围与根因见 §3.2；`foreground_fullscreen` 信号已存在（每秒免费刷新），不要新造轮询。G4 悬停反馈（v1.6.0）排其后；G7 黑名单文档已就绪但排在最后，别提前开工。
7. 收尾：全部完成后跑全量 `check/clippy/test`，同步文档，写一条记忆到 `.workbuddy/memory/`。
