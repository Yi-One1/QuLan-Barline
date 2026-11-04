# G6 清理清单（v1.8.1）—— 死码与未引用资源盘点

> 盘点范围：`src/`（59 个 `.rs` / 33,436 行 / 523 个 `#[test]`）、`assets/`、`docs/`、`scripts/`、`deliverables/`。
> 方法：对每个声明符号做整词全文匹配，并用**花括号深度**区分「生产区 / `#[cfg(test)]` 区」（`src/app.rs` 有两个 `#[cfg(test)] mod`，中间夹着生产函数 `marquee_frame`，按「测试区到文件尾」的粗切法会把生产函数误判成测试代码——本清单已按精确切法重算）。
> 纪律：本清单**只盘点，未改动任何源码 / 测试 / 资源**。所有结论都附出现次数，没有「可能没人用」。

---

## 1. 结论摘要

**能删的确定死代码 = 29 项约 235 行（占 `src/` 的 0.7%），另有 17 项「仅测试引用」中真正该删的 11 项约 110 行需连测试一起改（另 6 项因充当契约锚点 / 观测窗口归 C 档保留）；行为零变化（编译器 + 523 条测试全程可验证）。** 体积上的大头不在代码而在资源：**81 MB 的 74 张 BMP 截图**与 8 份零入链历史报告（`docs/` 共 82 MB），这部分必须用户点头，且 `deliverables/` 目录**未被 git 跟踪**（`.gitignore: /deliverables/`），删那里的文件无法用 git 恢复。

---

## 2. A 档 · 确定死（零引用、无契约、可直接删）

### A-1 零引用（删掉不需要动任何测试）

判定标准：全仓（含测试区）除声明行外**零次**出现 + 不是 trait 实现方法 + 不是被测试锁定的契约常量 + 不是 Win32 回调 / COM 接口要求的签名。逐项已用整词匹配复核。

| # | 路径:行号 | 符号 | 引用证据 | 建议动作 | 风险 |
|---|---|---|---|---|---|
| 1 | `src/settings/mod.rs:93` | `SettingsService::is_open()` | 1 次（仅定义） | 删函数 | 无。调用方一律用 `hwnd()` 判空 |
| 2 | `src/settings/bridge.rs:331` | `lang_str()` | 1 次（仅定义） | 删函数 | 无。语言字段走 serde 直接进 state JSON |
| 3 | `src/settings/bridge.rs:170` | `"lyrics.local_dirs_add"` 白名单分支 | 3 次（定义 + 2 条自带测试） | 删分支 + 删 `bridge.rs:601-604` 两条断言 | 低。「添加目录」实际走 `addLyricsDirBrowse` 动作，在 `app.rs:1896` 直接 push，从不发这条 set |
| 4 | `src/ui/menu.rs:236` | `cursor_pos()` | 1 次（仅定义） | 删函数 | 无。没有「弹出点兜底」调用方 |
| 5 | `src/ui/menu.rs:261` | `ABOUT_OK` | 1 次（仅定义） | 删常量 | 无。`show_about()` 自带 OK 文案 |
| 6 | `src/media/launch.rs:726` | `open_log_dir()` | 1 次（仅定义） | 删函数 | 无。设置页 `openLogDir` 在 `app.rs:1736` 内联 `ShellExecuteW` |
| 7 | `src/render/d2d.rs:283` | `current_slot()` | 1 次（仅定义） | 删函数 | 无。封面缓存不按槽分桶 |
| 8 | `src/taskbar/geometry.rs:82` | `target_size()` | 1 次（仅定义） | 删函数 | 无。宽高在各处直接相减 |
| 9 | `src/taskbar/host.rs:126` | `Host::is_visible()` | 1 次（仅定义） | 删访问器 | 无。`visible` 字段在 `host.rs:298/308` 内部自用 |
| 10 | `src/platform/dpi.rs:28` | `get_dpi_for_window()` | 1 次（仅定义） | 删函数 | 无。生产路径的 DPI 来自任务栏几何，不经此函数 |
| 11 | `src/platform/perf.rs:344` | `Span::with_note()` | 1 次（仅定义） | 删函数 | 低。附带后果：`Span.note` 恒为空，可一并删 `note` 字段与 `Debug` 里的输出 |
| 12 | `src/log.rs:275` | `debug_enabled()` | 1 次（仅定义） | 删函数（或改宏真用它，见 D-2） | 无。`log_debug!` 宏从不调用它 |
| 13 | `src/hotkey.rs:213` | `Hotkey::active_count()` | 1 次（仅定义） | 删函数 | 无 |
| 14 | `src/hotkey.rs:249` | `raw_code()` | 1 次（仅定义） | 删函数 | 无 |
| 15 | `src/app.rs:3071` | `App::debug_summary()` | 1 次（仅定义） | 删函数（18 行） | 低。诊断辅助，删后排查需临时重写 |
| 16 | `src/event.rs:284` | `type Sink` | 1 次（仅定义） | 删类型别名 | 无。全仓统一写 `EventSink<AppEvent>` |
| 17 | `src/config/mod.rs:67` | `HotkeyBinding::NONE` | 1 次（仅定义） | 删关联常量 | 无。未绑定的判定用 `is_bound()` |
| 18 | `src/config/defaults.rs:44` | `LYRIC_PAD` | 1 次（仅定义） | 删常量 | 无 |
| 19 | `src/config/defaults.rs:62` | `SEEK_DRAG_MS` | 1 次（仅定义） | 删常量 | 无。拖动判定只用 `SEEK_DRAG_PX` |
| 20 | `src/config/defaults.rs:75` | `TASKBAR_POLL_MS` | 1 次（仅定义） | 删常量 | 无。实际周期在 `app.rs` 的 `TIMER_GEOM` 上 |
| 21 | `src/config/defaults.rs:106` | `SEEK_BUBBLE_MS` | 1 次（仅定义） | 删常量 | 无。气泡停留时长另有出处 |
| 22 | `src/config/defaults.rs:317-319` | `TEXT_DARK` / `TEXT_LIGHT`（+ 317 行的孤立注释） | 各 1 次（仅定义） | 删两个常量 + 删 317 行注释 | 无。已被 `Theme::DARK_TEXT` / `LIGHT_TEXT`（`Rgb`）取代 |
| 23 | `src/config/defaults.rs:360` | `mods::NOREPEAT` | 1 次（仅定义） | 删常量 | 无。`hotkey.rs:143` 用的是 windows-rs 的 `MOD_NOREPEAT` |
| 24 | `src/render/theme.rs:148` | `Theme::DIM_ALPHA` | 1 次（仅定义） | 删常量 | 无 |
| 25 | `assets/ui/index.html:449` / `:499` | i18n 键 `hotkeys.reset1` | 2 次（zh/en 字典各 1） | 删两条词条 | 无。逐行重置用的是 `hotkeys.reset_one`（`:450`/`:500`，JS 在 `:788` 取用） |
| 26 | `assets/ui/index.html:462` / `:512` | i18n 键 `toast.saved` | 2 次（zh/en 字典各 1） | 删两条词条 | 无。toast 文案来自 Rust 的 `msg.payload.text`，或 `:832` 的内联双语串 |
| 27 | `src/i18n.rs`（12 条） | `app.name` / `app.tagline` / `btn.play` / `btn.pause` / `btn.prev` / `btn.next` / `menu.separator` / `state.no_session` / `state.playing` / `state.paused` / `state.fallback` / `state.no_cover` | 每条 2 次（zh/en 各 1，**调用点 0**） | 删 24 行 | 无。全仓只活 `state.unknown_track` / `state.unknown_artist`；无「遍历全部词条」的测试，删除不影响 523 条 |
| 28 | `src/event.rs:192/194/196/198/206/209` | `Effect::SetVisible` / `MoveWindow` / `ResizeTarget` / `RequestBackdrop` / `SettingsPush(HostMsg)` / `CloseSettings` | `MoveWindow` 2 次、`RequestBackdrop` 2 次、`SetVisible` 2 次、`CloseSettings` 2 次；`ResizeTarget`、`SettingsPush` **各 1 次（仅定义）** | 6 个变体 + `app.rs:2245/2271/2292/2298` 4 个死臂一起删；`HostMsg`（`event.rs:266`）与 `ToastLevel`（`:277`）随之整体删除 | **低但有前提**：变体与臂必须**同一次提交**删。只删臂留变体（或反之）会静默落进 `app.rs:2311` 的 `other =>` 兜底，以后有人构造就变成「只打一条 debug 日志、功能不生效」 |
| 29 | `src/event.rs:171/172` | `WidgetInput::SetBackgroundMode` / `ShowAbout` | 各 2 次（定义 + 臂） | 2 个变体 + `app.rs:2166/2180` 2 个死臂一起删 | 低。两者都另有活入口：背景模式走托盘菜单 `MenuCommand`，关于走 `MenuCommand::ShowAbout` → `app.rs:2965` |

**A-1 合计：29 项 ≈ 235 行。** 其中第 28 项与 ARCHITECTURE.md §8.2 的枚举清单直接冲突（该处仍列着 `SetVisible / MoveWindow / ResizeTarget / RequestBackdrop / SettingsPush / CloseSettings`，还列着代码里早已不存在的 `DropBackdrop`），**本轮禁改 `docs/ARCHITECTURE.md`，建议与 D-16 一并排到 G6 之后**。

### A-2 仅测试引用（删它要连测试一起删 —— 单独一档，别顺手删）

| # | 路径:行号 | 符号 | 测试引用数 | 删它的连带代价 |
|---|---|---|---|---|
| 1 | `src/render/layout.rs:266` + `:278` | `compute()` / `compute_ex()` | 30 / 1 | 生产路径是 `app.rs:2369` 直接调 `compute_full`。这两个包装层**只服务测试**，删则需把 ~28 处 `compute(...)` 调用改成 `compute_full(..., form.lyric_w())` |
| 2 | `src/lyric/writeback.rs:112` | `file_name_for()` | 18 | 与私有 `build_file_name()`（`:236`，`save` 在 `:350` 真用）是**同一行的两份实现**（仅 `Option`/`Result` 之差）。删则 W1–W5、W11、W12、W15、s1 共 18 处断言要改 |
| 3 | `src/app.rs:196` | `State::progress_fraction()` | 3 | 绘制侧另有一份内联实现（见 D-6）。**建议优先改成唯一出处，而不是删** |
| 4 | `src/render/layout.rs:237` | `Layout::fixed_width()` | 8 | 被 `fixed_width_matches_computed_width` 当契约锚点用 → 改判 **C 档** |
| 5 | `src/render/layout.rs:257` | `Layout::dismiss_bg_rect()` | 3 | 注释已说明「做成纯函数就是为了能单测几何契约」→ 改判 **C 档** |
| 6 | `src/ui/interaction.rs:125` / `:130` | `has_pending_click()` / `is_seeking()` | 11 / 7 | 状态机内部状态只能靠这两个访问器观测 → 改判 **C 档** |
| 7 | `src/config/defaults.rs:230/240/242/248` | `DISMISS_ALPHA` / `DISMISS_BG_HOVER` / `DISMISS_BG_PRESSED` / `DISMISS_BG_RADIUS` | 2/3/2/3 | 布局契约常量，被 `layout` 测试用来钉 × 底色的数值 → 改判 **C 档** |
| 8 | `src/event.rs:134` | `HotkeyAction::ALL` | 3 | 枚举完整性遍历用（`ALL.len()==6` 是防漏加动作的闸） → **C 档** |
| 9 | `src/taskbar/host.rs:30` | `WM_BARLINE_WIDGET_BASE` | 2 | 窗口消息编号契约，`platform/wake.rs:107/123` 的测试引用 → **C 档** |
| 10 | `src/platform/perf.rs:92` / `:378` | `is_slow()` / `Span::cancel()` | 6 / 7 | 生产代码从不取消 span（`report()` 才用 `over()`）。删则 perf 的 6+7 条测试要删 |
| 11 | `src/media/mod.rs:126` | `Rgb::parse_hex()` | 1 | 唯一调用点是 `config/mod.rs:479` 的测试，而它读的正是死配置 `accent_custom`（见 B-6）。两者一起删才干净 |
| 12 | `src/platform/wide.rs:22` | `from_wide_full()` | 1 | 只有 `from_wide_full_keeps_content` 一条测试 |
| 13 | `src/render/sampler.rs:114` | `Area::overall()` | 1 | 只有 1 条测试 |
| 14 | `src/render/theme.rs:59` | `Rgba::with_alpha_scale()` | 1 | 只有 1 条测试 |
| 15 | `src/hotkey.rs:241` | `error_name()` | 1 | 只有 1 条测试；活路径用 `raw_code`…而 `raw_code` 也已死（A-1 #14），两者互为孤儿 |
| 16 | `src/lyric/mod.rs:100` | `Lyrics::word_progress()` | 2 | 扫光进度现在由 `render` 侧算，此函数只剩 2 条测试 |
| 17 | `src/event.rs:230` + `src/media/smtc.rs:1041` | `MediaCommand::Mute` | 1（`smtc.rs:1636`） | **重点**：生产代码从不发送 `Mute`（`app.rs:2089` 的静音走 WASAPI 音量），`smtc.rs:1041` 的分支与 `smtc.rs:1636` 的测试是唯一使用者 |

> 说明：上表 #4–#9 虽同为「仅测试引用」，但它们的**存在理由就是给测试当观测窗口**，删了等于把契约检查一起删掉，所以归 C 档保留。真正该删的是 #1/#2/#10–#17。

---

## 3. B 档 · 需用户点头（资源文件 / 配置契约 / 用户可能想留的东西）

| # | 文件或符号 | 大小 | 判断依据 | 删除后的影响 |
|---|---|---|---|---|
| 1 | `docs/screenshots/`（74 个 `.bmp`） | **81 MB** | 被 **7 个文件**以**通配/区间**引用（`M1-REPORT.md:70` 写 `docs/screenshots/*.bmp`；`QA-M4-REPORT.md:258` 写 `m4-qa2-10..15`），`README.md:5` 正文按精确名引用 1 张（`m4-qa-10-appearance.bmp`）。**除这 1 张外，没有任何一张是「被精确路径引用」的**，但也没有一张能说「完全没被提到」 | 74 个文件均被 git 跟踪（`git ls-files docs/screenshots`=74），可用 `git checkout` 全量恢复；但 6 份报告的「证据」会失效，`README.md` 首屏图会变红叉 |
| 2 | `docs/M1-REPORT.md`、`M2-REPORT.md`、`M3-LYRIC-REPORT.md`、`M4-REPORT.md`、`QA-M2-REPORT.md`、`QA-M3P2-REPORT.md`、`QA-M4-REPORT.md`、`QA-M5-REPORT.md`（8 份） | 共 127 KB | **零入链**：`grep -rl` 全仓无任何文件引用它们的文件名。`progress.md` 只用散文转述结论、不引文件名 | 删掉等于抹掉 M1–M5 的验收证据链；`progress.md` 里「QA 结论 PASS / 14 项全 PASS」就没了原始依据。**建议保留**，若要删请在 B 档单独裁决 |
| 3 | `deliverables/LICENSE` | 1,076 B | 与根 `LICENSE` **字节完全相同**（md5 `9d3f8e8f…`）。`scripts/build.ps1` 不拷贝它，即删除后不会「下次构建又回来」 | 无。**注意：`/deliverables/` 在 `.gitignore` 里，git 未跟踪，删了无法用 git 恢复**（但内容与根 `LICENSE` 一致，可从根拷回） |
| 4 | `deliverables/README.md` | 3,861 B | 与根 `README.md`（4,974 B）**内容不同**（md5 `21a9efb5…` vs `fc868236…`），是 11 月 2 日的**过期副本**，里面还写着旧的包体/版本数据 | 无功能影响，但它是**会误导人的旧文档**（比单纯重复更值得删）。同样不在 git 里，删前请先另存 |
| 5 | `src/event.rs:33/50/61/63/65/87/74/89/90` 共 9 个 `AppEvent` 变体 | ≈ 55 行（变体 + `app.rs:1594/1600/1604/1608/1612/1616/1626/1630` 的 8 个臂） | **零发射点**：`FallbackSession`（全仓 0 次构造）、`LyricRetryDue`、`TaskbarGeometry`、`TaskbarCreated`、`DisplayChanged`、`SecondInstanceLaunched`、`ConfigChanged`、`Tick`、`Quit` 各自只出现在自己的 `reduce` 臂里。功能没丢——对应行为已改成直接方法调用（如 `app.rs:641` 直接调 `tick_optimistic()`、`widget_wnd` 直接调 `on_taskbar_created()`） | 删掉后 `AppEvent` 从 24 个变体降到 15 个，比 `Effect` 好删（有 `other =>` 兜底，编译不报错）。但 `event.rs` 头部自称「模块之间**唯一**的耦合点」，删事件变体属于**架构决定**而非机械清理，且 `docs/ARCHITECTURE.md §8.1` 把它们列为对外契约 → 必须用户点头 |
| 6 | `Config.window`（`config/mod.rs:225`）+ `WindowStateConfig` 整棵（`:185-192`，6 个字段） | ≈ 35 行（结构 + `defaults.rs:598-607` 的 `Default` + 测试 4 处） | **生产代码零读写**：`settings_w/h/x/y`、`lyrics_visible`、`lyrics_two_line` 从未被读过，只有 serde 每次 `save()` 原样写进 `config.json`。对应的真实功能早已改名（`lyrics.enabled`）或下线（歌词行数 v1.1.1 固定单行） | 用户 `config.json` 里少 6 个键（`#[serde(default)]` 已开，读旧文件不报错）。风险是「以后想恢复设置窗位置」要从零写 |
| 7 | `AppearanceConfig.accent_source`（`:90`）+ `accent_custom`（`:92`）+ `AccentSource` 枚举（`:23`） | ≈ 22 行 | **生产零读**：`accent_source` 全仓只在 `defaults.rs:415` 赋默认值；`accent_custom` 唯一的「读」是 `config/mod.rs:479` 的测试。强调色实际取自系统（`render/theme`） | 同上：`config.json` 少 2 个键。若将来做「自定义强调色」，需重建 3 处 |
| 8 | `LyricsConfig.mode`（`:102`）+ `LyricModeSerde`（`:33`） | ≈ 12 行 | 唯一「读」是 `app.rs:408` 的一行**启动日志**；设置页早已不能改（`bridge.rs:134` 注释：v1.1.1 从白名单删除）。等于「为一行日志活的字段」 | 删时同步删 `app.rs:408` 那个 `{:?}` 参数。`config/mod.rs:342-353` 的向后兼容测试要改 |
| 9 | `scripts/probe-widgets.py`（4.1 KB） | 4.1 KB | 零入链（只有自己文档里写了用法）。是 M2 期用 ctypes 枚举 explorer 窗口找「Win11 任务栏媒体控件」类名的工具 | 删掉以后要重新排查「与系统媒体控件共存」需重写；零第三方依赖，留着成本几乎为零。**倾向保留** |
| 10 | `Cargo.toml` 的 `Win32_UI_Accessibility`、`Foundation_Collections` 两个 feature | 编译期 | 两者**零 import**（`grep -rn` 全仓 0 命中）。但 feature 可能是 `webview2-com-sys` 的传递依赖 | **必须实测**：单独摘掉后跑 `build.ps1 -Task build`，绿才能删。本清单不把它计入「确定死」 |

---

## 4. C 档 · 保留（看似死但必须留 / 刻意留档）

| 符号 | 为什么留 |
|---|---|
| `src/media/launch.rs:390` `tray_click_activate()` | 有 `#[allow(dead_code)]`，注释写明「**当前未启用**（用户裁决 2026-11-02）」+ 三条续修方向；`progress.md §3.4 待办 D` 明确「搁置但留档」。删了等于抹掉一次已验证有效的实测实现 |
| `src/app.rs:2813` / `2822` `start_msg_timer()` / `kill_msg_timer()` | 注释明确声明「保留一对幂等入口，供后续需要在消息窗上重启 `TIMER_GEOM` 时使用，**不新增任何轮询频率**」——是刻意的成对预留，不是漏删 |
| `src/render/layout.rs:237` `fixed_width()` | 契约测试锚点：`fixed_width_matches_computed_width`（`:1070`）用 8 条断言钉住「布局侧固定件宽度与 `compute_full` 同源」，删函数=删契约 |
| `src/render/layout.rs:257` `dismiss_bg_rect()` | 注释写明它是把「× 底色不许越界」做成纯函数的唯一途径（painter 需要 D2D 上下文，单测跑不了）。3 条断言依赖 |
| `src/ui/interaction.rs:125` / `130` `has_pending_click()` / `is_seeking()` | 状态机内部字段只能通过访问器观测，11+7 条断言依赖它们 |
| `src/config/defaults.rs:230/240/242/248` `DISMISS_*` 4 个常量 | 布局契约数值，`layout` 测试用它把「× 底色 6px 字形 / 0x14 底 / 圆角」钉成数值，删了测试就没锚点 |
| `src/event.rs:134` `HotkeyAction::ALL` | `ALL.len() == 6` 是「防漏加动作」的闸（`event.rs:297`），也是 6 动作双向一致性测试的枚举源 |
| `src/taskbar/host.rs:30` `WM_BARLINE_WIDGET_BASE` | 窗口消息编号契约，`platform/wake.rs:107/123` 用来断言消息号段不撞车 |
| `src/runtime` 的 `HostMsg` 之外的 `pub` 面 | 见 A-1 #28：`HostMsg` 确实整体死了（`Effect::SettingsPush` 是唯一引用者），可以删；但**先确认**没有在 `settings/bridge.rs` 里留同类协议注释要一起改（`bridge.rs:7-13` 的协议注释描述的是 JS→Rust 方向，不受影响） |

---

## 5. D 档 · 需要改写而非删除

| # | 路径:行号 | 现状问题 | 建议改法 |
|---|---|---|---|
| 1 | `src/event.rs:203` | **注释说**：`/// 保存配置（带 300ms 去抖）`<br>**代码实际**：`Effect::SaveConfig` → `app.rs:2287` 直接 `cfg.save()` → `config/mod.rs:291-304` 同步 `fs::write` + `fs::rename`；全仓 `grep 去抖` 无实现（另一处 300ms 是 `SAMPLE_THROTTLE_MS`，与落盘无关） | 改注释为「保存配置（**同步**写盘：tmp + 原子 rename；不做去抖）」。**同一句话在 `docs/ARCHITECTURE.md §8.2` 也有一份**，本轮禁改，见 #16 |
| 2 | `src/log.rs:274` | **注释说**：「宏用它避免无谓的 `format_args!` 求值」<br>**代码实际**：`log_debug!` 宏（`:280`）直接调 `log::write(Level::Debug, …, format_args!(...))`，从不判 `debug_enabled()`；该函数零调用 | 二选一：①删注释与函数（推荐，见 A-1 #12）；②真的让宏先判 `if debug_enabled()`——但那会改变 `format_args!` 的求值时机，属行为变更，本轮不做 |
| 3 | `src/lyric/writeback.rs:661`（W13） | **恒真断言**：`let dir = tmp("w13");` 建了个临时目录，但 `save(&track, &[], &lyrics())` 的第二个参数是**空切片**，`dir` 从未进入 `save()`。于是 `assert!(!dir.join(SUB_DIR).exists(), "不该创建任何目录")` 永远成立，改坏 `save()` 也不会红 | 改成有鉴别力的写法：把 `dir` 传进去但断言「全目录仍为空」——`assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0)`；或明确写成「`local_dirs` 为空时 `save` 连 `dirs_of()` 都不碰」并删掉无关的 `dir` |
| 4 | `src/lyric/writeback.rs:675`（W14） | **弱断言**：`match read_dir(&sub) { Ok(rd) => rd.count(), Err(_) => 0 }` 把「目录不存在」和「目录空」合并成同一个 0，于是「建了个空目录」这种回归抓不到（而 W13 的主张正是「连目录都不建」） | 拆成两条显式断言：`assert!(!sub.exists(), "空歌词不该建 Barline 目录")`（若确定口径允许建空目录，则写成 `assert_eq!(count, 0)` 并在注释里写明「允许建空目录」） |
| 5 | `src/lyric/service.rs:1130`（s1） | **近平行实现比对**：`let want = writeback::file_name_for(&req.track); assert_eq!(files[0], want, …)` —— 落盘侧真正用的是私有 `build_file_name()`（`writeback.rs:236`），`file_name_for` 是它的一行孪生兄弟。断言成了「两份同样的实现互相印证」，任一份单独改另一条都测不出「写出去的名字查找侧命中不了」 | 改为**跨侧闭环**：`assert!(locate::locate_in(&dir, &req.track).is_some(), "落盘文件名必须能被查找侧命中")`；或退一步用字面量 `assert_eq!(files[0], "测试曲目α - 无名歌手α.lrc")`。W15 已经在做闭环，s1 这里可以只留字面量 |
| 6 | `src/app.rs:196` vs `src/render/painter.rs:911-923` | **两份进度实现**：`State::progress_fraction()` 与 `draw_progress()` 里的内联 `frac` 是同一公式的两个副本（painter 多了 `hud` 分支）。`progress_fraction` 只有 2 条测试调用，**屏幕上画的从来不是它算的** | 把 `hud`/`seeking`/常规三态收进 `progress_fraction`（或新增 `progress_fraction_with_hud`），让 painter 只调用它；否则删 `progress_fraction`，把测试改成对 painter 的纯函数断言。**这是本轮里「测试全绿但没测到生产代码」的第二个活样本**（第一个是 v1.7.0 的 `s5`） |
| 7 | `src/config/mod.rs:181` | **注释说**：`/// 记忆的窗口 / 交互状态，重启后恢复。`<br>**代码实际**：6 个字段生产零读写，`settings_w/h/x/y` 从未被恢复过 | 与 B-6 一起处置：要么删整棵；要么保留但把注释改成「**预留**：当前未持久化设置窗几何」，别让读者以为重启会恢复 |
| 8 | `src/render/layout.rs:263-265` | **注释说**：「保留这个 4 参数签名是为了让既有单测**与调用点**零改动；正式路径用 `compute_ex`」<br>**代码实际**：正式路径是 `app.rs:2369` 直调 `compute_full`，`compute` 与 `compute_ex` 都只剩测试调用，「调用点」已归零 | 改注释为「仅供单测使用的 4/6 参数便利包装；正式路径是 `compute_full`」 |
| 9 | `src/media/launch.rs:725` | **注释说**：`/// 打开本地日志目录（设置页 / 关于页用）。`<br>**代码实际**：零调用；设置页走 `app.rs:1736` 的内联 `ShellExecuteW` | 随 A-1 #6 一起删 |
| 10 | `src/ui/menu.rs:235` | **注释说**：「取鼠标当前位置（**菜单弹出点兜底用**）」<br>**代码实际**：零调用，没有这条兜底路径 | 随 A-1 #4 一起删 |
| 11 | `src/render/d2d.rs:282` | **注释说**：「当前槽位（0 = A，1 = B），**用作封面缓存的分桶键**」<br>**代码实际**：零调用，封面缓存不按槽分桶 | 随 A-1 #7 一起删 |
| 12 | `src/event.rs:9-11` | **注释说**：「M1 阶段只有 7 个变体被真正处理，其余变体是 **M2–M5 的接口预留**——`reduce()` 里统一 `log_debug!` 后忽略」<br>**代码实际**：M2–M5 全部已发布（v1.8.0），而 9 个变体至今**零发射点**——它们不是「预留」，是「已被方法直调取代的残骸」 | 改成实话：「以下 9 个变体当前无发射点，保留与否见 G6 清单 B-5」。若 B-5 决定删，整段一起删 |
| 13 | `src/event.rs:261-264` | **注释说**：`HostMsg` 是「**M1 占位**……到 T13（M3）会整体搬到 `settings::bridge`，`Effect::SettingsPush` 的签名不变」<br>**代码实际**：M3 早已完成，`HostMsg` 仍留在 `event.rs`，且整个类型零引用（`ToastLevel` 同理） | 随 A-1 #28 整体删除；若要留，注释必须改成「当前无使用者」 |
| 14 | `src/config/defaults.rs:317` | **孤立注释**：`/// 与系统媒体控件之间的间距` 挂在下方的 `TEXT_DARK` 上，语义完全无关（原常量被删后注释残留） | 随 A-1 #22 一起删 |
| 15 | `src/i18n.rs`（`about_text_credits_both_lyric_sources` 上方） | **注释说**：「契约回归（v1.1.22 / **为 G6 铺路**）：……否则 **G6 接入网易云兜底后**还得再回来改一次文案」<br>**代码实际**：网易云兜底在 v1.3.0 就已接入并验收，这里说的「G6」与当前的 G6（清理专项）是两个不同含义，会误导 | 把「为 G6 铺路」改成「v1.3.0 网易云兜底接入前铺路」 |
| 16 | `docs/ARCHITECTURE.md §8.2` | 仍列着 `Effect::DropBackdrop`（代码里**不存在**）、`BackdropReady { theme, blur }`（代码里 `blur` 已删）、`SaveConfig（带 300ms 去抖）`、以及 A-1 #28 的 6 个死变体 | **本轮禁改**。建议在 G6 收尾时单独一批同步，否则下一轮又会被当成「契约」引用 |

---

## 6. 不建议在本轮做的

- **「清空歌词缓存」按钮**：`lyrics.cache.desc` 文案已经说「这个文件夹可以安全删除」，但做个按钮是**新功能**（要走 UI + 动作 + 白名单 + i18n），不是清理。
- **`assets/` 的 4 个文件**：`barline.rc`、`icon.ico`、`app.manifest`（`build.rs` 三行 `rerun-if-changed` + `embed_resource::compile`）、`ui/index.html`（`settings/assets.rs:7` 的 `include_str!`）**全部在用，零孤儿**，不用动。
- **`deliverables/` 的 35 个 exe**：用户口径「exe 永不删」，本轮不碰（`progress.md:256` 写的「现存 24 个」是 v1.4.x 时的快照，现已增至 35）。
- **`progress.md` / `docs/ARCHITECTURE.md`**：本轮禁改（D-16 因此只能登记）。
- **把 painter 的进度 / 跑马灯计算抽成统一纯函数**：那是重构（D-6 的最小版只要求「让测试锁住真代码」），改错的风险高于清理收益，留给下一轮。
- **全 523 条测试的恒真断言普查**：本轮做了两类自动扫描（`assert!` 里含 `||` 的：**0 命中**；两侧调用同一函数的：**0 命中**），并逐条读了已点名的 3 处。剩下的是需要人读语义的活，按 D-3/D-4/D-5 的口径逐批补强更稳。
- **`scripts/barline_edge_profile/`（90 MB）**：是 WebView2 运行期产生的隔离 profile，**已在 `.gitignore` 里、不在仓库中**，属于本地垃圾，与仓库清理无关（想省磁盘可以直接删，不需要走 G6 流程）。

---

## 7. 风险与回退

**分层提交，每层单独可回退**（建议 3 个 commit）：

| 批次 | 内容 | 编译/测试能否抓住误删 |
|---|---|---|
| C1 | A-1 全部（含 6 个 `Effect` 变体 + 2 个 `WidgetInput` 变体 + 12 条 i18n + 2 条 HTML 词条） | **能**。删多了编译器直接报 `cannot find`；删漏了 `?`。唯一抓不住的是「变体与臂不同步删」→ 所以二者必须同 commit |
| C2 | A-2 里真正该删的（`compute`/`compute_ex`、`file_name_for`、`perf::is_slow`/`cancel`、`Mute` 等） | **能**。删了符号测试编译失败，逼着同 commit 改断言——这正是我们要的（**测试总数会变**：A-1 一条不动仍是 523；A-2 按「改断言」口径基本持平，按「连测试一起删」口径会少 6–10 条。提交信息里写清 before/after 实测值，不要照抄本清单的估算） |
| C3 | B 档资源（截图 / 历史报告 / deliverables 副本） | **不能**。没有任何自动化能发现资源删多了 → 只走用户点头 + 逐项确认 |

**回退手段**

1. **代码（A 档）**：全部在 git 跟踪内，`git revert <commit>` 或 `git checkout <sha> -- <path>` 单文件精确还原。
2. **`docs/` 资源（B-1/B-2）**：74 张截图与 8 份报告**都被 git 跟踪**（`git ls-files docs` = 91），删错了 `git checkout -- docs/screenshots` 全量回来。
3. **`deliverables/` 的两个副本（B-3/B-4）**：**不在 git 里**（`.gitignore` 有 `/deliverables/`，`git ls-files deliverables` = 0）。删之前必须：
   ```bash
   mkdir -p ../g6-backup && cp deliverables/LICENSE deliverables/README.md ../g6-backup/
   ```
   - `deliverables/LICENSE` 与根 `LICENSE` 字节一致，可从根拷回，实际零损失；
   - `deliverables/README.md` 是**唯一的**过期副本，删了就没有第二份——这是本轮唯一「不可 git 恢复」的删除项。
4. **配置 schema（B-6/B-7/B-8）**：老用户 `config.json` 里会残留多余键，`#[serde(default)]` + serde 默认忽略未知键 → 读不炸。若要回到旧 schema，`git revert` 代码即可，用户文件不用手改。
5. **最危险的一处（结论）**：**A-1 #28 的 `Effect` 死变体**。它是唯一「删一半也编译通过」的地方——`app.rs:2311` 的 `other =>` 兜底会把任何漏删/漏留的组合吞成一条 debug 日志。若只删 arm 不删变体、日后有人构造它，表现是「功能静默失效、日志里只有一句『暂未实现的 Effect』」，qa 与用户都不会察觉。**因此必须变体 + arm + ARCHITECTURE.md §8.2 的对应行三处同批处理。**

---

### 附：本次盘点的证据脚本（跑完即删，不进仓库）

统计口径 = 整词匹配 + 花括号深度切分测试区。7 个临时脚本 `scripts/_g6_*.py` 仅用于生成以上数字，收尾时已从工作区清除。
