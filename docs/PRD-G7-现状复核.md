# G7 媒体会话黑名单 · PRD 现状复核

复核对象：`docs/PRD-G7-黑名单.md`（2026-11-03，478 行）**§5 交互与界面稿（309–380）/ §8 验收标准（424–441）**
对照基线：`assets/ui/index.html`（925 行）、`src/ui/tray.rs`、`src/i18n.rs`、`src/app.rs`、`src/settings/`、`progress.md` §2.2
性质：**只复核、不改 PRD 正文、不改源码**。行号均为当前工作树实际值（`rg` 不可用，全部用 Grep 工具核对）。

## 1. 一句话结论

**决策层零改动，可直接实现；界面层不能直接当依据，必须按 §2 的 10 处调整。** 四项拍板口径、方案 B、三点拦截、"进入分区才枚举"全部与现状代码相容；但 §5 界面稿有 6 处画法与 v1.8.0/v1.8.1 之后的实际 HTML 结构对不上（卡片头右对齐、动态计数标题、行结构、二次确认、输入行、托盘状态来源），另有 2 处版本号（v1.8.0 / v1.7.1）已过期——G7 实际是 **v1.9.0**（`progress.md:216`）。

## 2. 必须调整项

| # | PRD 位置 | 现在的问题（依据） | 建议改法 |
| --- | --- | --- | --- |
| 1 | 头部表 L6 + §8 条6 L434 | v1.8.0 已被 G5.1 用掉、v1.8.1 是 G6（`progress.md:194/204`）；「回滚到 v1.7.1」跨两代 | 全文 v1.8.0 → **v1.9.0**；回滚验收改「v1.9.0 → v1.8.1，`media` 整组被老 serde 丢弃」 |
| 2 | §8 条4 L432 | 性能基线写「v1.7.1 ±10%」，现基线是 v1.8.1 | 基线改 **v1.8.1** |
| 3 | §5.1 L335「检测到的应用　尚未检测　[刷新]」同处标题行 | `.card > h2` 是块级 14px 标题、无 flex（`index.html:95`），右对齐刷新只能新增 CSS，而 P0-03（L188）明令不新增类 | 改为卡片内首行 `.row`：左 `.label`，右 `.right` = `.desc`(上次检测) + `.btn`(刷新)——零新类 |
| 4 | §5.1 L320/322「已加入黑名单（2）」 | `render()` 每次 state 都重写所有 `[data-i18n]` 的 `textContent`（`index.html:634-636`）→ 计数被打回模板串（v1.8.0 在 `#lyrics-cache-usage` 已踩过，`index.html:297-299`） | `#blacklist-title` **不带** `data-i18n`，JS 写 `t("blacklist.card.title").replace("{n}", n)`（替换先例 `index.html:577-580`） |
| 5 | §5.1 L321-326 行结构（显示名 + 进程名两行） | `.list .item` 是单行 flex 且写死 `font-family:Consolas`（`index.html:164-169`），只能承载「一行文本 + 右侧按钮」 | `.item > span` 内叠 `.label`(显示名) + `.desc`(进程名) + `.btn.danger`(×)；中文显示名会继承 Consolas（可接受）；要正文字体只许**行内 style** |
| 6 | §5.1 L357「清空 → 弹 `confirm`」 | 全页零 `confirm(` / `alert(`（Grep 无匹配）；项目二次确认一律 Rust 原生 `platform::confirm_dialog`（`app.rs:1904`） | 改 action → Rust `confirm_dialog`，与数据目录 / 歌词目录弹窗同款 |
| 7 | §5.1 L327 输入行 | `input[type=text]` 有 `min-width:132px` 且不伸缩（`index.html:111-115`），`.row` 是 space-between（`96-99`）→ 输入框不会占满左侧 | 输入 + 添加 + 清空一起放 `.right`，左侧放 `.desc` 提示；或行内 `style="flex:1"`。不改 CSS |
| 8 | §6 L399 `mediaApps` 通道 | 方向对，漏两步：① 必须走 `SettingsService::post()`，**不能 `push()`**（会覆写 `last_state`，窗口重建后补推错内容，`progress.md:199`）；② `settings/assets.rs:219-223` 已断言全页 `setTimeout` 仅 1 处（toast）→ 枚举**不得**用 setTimeout 做置灰超时 | 补这两句；新动作名字面量要进 `assets.rs` 断言（先例 `226-231`） |
| 9 | §5.2 L375「打开菜单时才读取一次状态（不缓存、不订阅）」 | 易被读成「弹菜单时现查 SMTC」，撞 §2.1 目标 2 的零跨进程红线；正确来源是 `App` 内存态的当前会话身份 | 改写为「读 `App` 内存态里的当前会话身份，**不调 SMTC**」；并在 P0-05 注明 `show_menu(lang, widget_visible)`（`tray.rs:202`）需扩参、`append()`（`tray.rs:288`）4 参 → 5 参 |
| 10 | §5.2 L368 位置 / §1 L35 导航位 | **无问题，保留**：托盘现状 = 设置 / 显示·隐藏小组件 / 分隔线 / 退出（`tray.rs:206-215`），新项插在「显示/隐藏小组件之后、分隔线之前」仍成立；导航现状 6 项、顺序 外观·歌词·音量·快捷键·通用·关于（`index.html:196-201`），「音量之后、快捷键之前」= 第 3、4 项之间 | 不动原决定；`data-group="group-blacklist"`、`ico=⊘`（ARCH:1159 已给，与现有按钮同构） |

## 3. 界面稿 → 现有类名映射建议（目标：新增 CSS 类 = 0）

| 界面稿元素 | 复用的既有结构 | 依据 |
| --- | --- | --- |
| 分区外壳 | `<section id="group-blacklist">` + `h1` + `.sub` | `index.html:86-89, 206-208` |
| 两张卡片 | `.card`（卡 A 可带 id 便于 JS 定位） | `91-95` |
| 卡 A 计数标题 | `<h2 id="blacklist-title">`（**无** `data-i18n`） | `95` + `634-636` |
| 黑名单列表 | `.list > .item > span(.label + .desc) + .btn.danger(×)` | `163-173`；`renderDirs` 先例 `697-717` |
| 空态 | `.list .empty` | `173, 693` |
| 输入行 | `.row` + `input[type=text]` + `.btn.primary` + `.btn.danger` | `96-99, 111-116, 137-146` |
| 两条静态小字 | `.desc`（12px / `--text-2`） | `102` |
| 检测卡片头（上次检测 + 刷新） | `.row` + `.label` + `.right`(`.desc` + `.btn`) | `96-103`；**不改用 `h2`** |
| 检测行 / 不可加入行 | `.list .item` + `.btn`（加入）+ `.dimmed` | `107-108, 163-173` |
| 「未能验证」小字 | `.desc`；悬停解释走 `title`（先例 `#lyrics-cache-dir` / `.btn.icon`） | `293, 788` |

**与 v1.8.0 缓存卡片的共存**：缓存卡是纯 `.row` 组合的**只读**卡（`index.html:289-302`），黑名单是**可编辑列表**，两者在不同分区、互不影响，**不需要任何额外样式**；唯一代价是 `.item` 的 Consolas 会作用于中文显示名。现有 CSS 里最接近的可复用列表就是 `renderDirs` 那套（`.list/.item/.empty/.btn.danger`），已够用。另注：`section` 有 `max-width:720px`（`86`），界面稿的「导航 260px + 宽 main」只是示意，实际按单列 720px 排。

## 4. i18n 键名清单（建议最终键名）

**风格核对**：HTML 字典是「分区前缀 + 点分层级」（`lyrics.cache.usage.more`，`index.html:436`）；`src/i18n.rs` 是「命名空间 + snake_case 段」（`settings.webview_missing_title`，`i18n.rs:63`）。ARCH §8.1/§8.2 的两组键名**与现有风格一致、零冲突**——现字典无任何 `blacklist.*`；`src/i18n.rs` 此前**完全没有 `toast.*` 命名空间**（现只有 state./lyric./tray./settings./menu./hud./about.），新增可以，但双语非空契约测试必须覆盖（`t()` 未知键原样返回，`i18n.rs:128`）。`t()` 返回 `&'static str` → 带应用名的托盘两项必须 Rust 侧 `format!`（ARCH:1337-1349 已如此）。

> 结论：**36 个键全部沿用 ARCH §8.1/§8.2 现名，零改名**；英文逐条照该表，本轮复核无改动。中英全文见 ARCH 表格，此处只列键名 + 中文口径，避免第三份数字漂移。

- **`index.html` 的 `I18N.zh`/`I18N.en`（23 条）**：`nav.blacklist` 黑名单 · `blacklist.sub` 分区副标题 · `blacklist.card.title` 已加入黑名单（{n}） · `blacklist.card.title.empty` 黑名单 · `blacklist.empty` 空态 · `blacklist.add` 添加 · `blacklist.add.placeholder` 输入进程名，如 CloudMusic.exe · `blacklist.remove` 移除 · `blacklist.clear` 清空黑名单 · `blacklist.clear.confirm` 清空确认 · `blacklist.hint.match` 匹配规则小字 · `blacklist.hint.browser` 浏览器进程名小字
- **同上（检测卡片，续）**：`blacklist.detected.title` 检测到的应用 · `.at` 上次检测： · `.never` 尚未检测 · `.refresh` 刷新 · `.empty` 未检测到正在播放的应用。 · `.note` 空态说明 · `.add` 加入 · `.unparsed` 无法解析进程名 · `.unparsed.title` 手动输入提示 · `.unverified` 进程名未能验证（…） · `.unknown` 未知应用
- **`src/i18n.rs`（13 条）**：`menu.blacklist_current` 拉黑当前应用 · `menu.unblacklist_current` 恢复当前应用 · `menu.unknown_app` 未知应用 · `toast.blacklist.added` 已加入黑名单 · `.dup` 已在黑名单中 · `.empty` 请输入进程名 · `.invalid` 进程名不支持通配符或… · `.too_long` 进程名过长（最多 260 字符） · `.full` 黑名单已满（上限 200 条） · `.removed` 已从黑名单移除 · `.cleared` 黑名单已清空 · `.unverified` 已加入。这个进程名还没… · `.enum_failed` 未检测到正在播放的应用

**⚠ 唯一需拍板的文案不一致**：`blacklist.hint.match` 在 PRD P0-06（L279）定稿含末句「从「检测到的应用」里加入最稳。」，ARCH §8.1（L1397）**没有**这句。建议**采用 PRD 版**（这句正好给"手输不保证"指路）。另：`toast.blacklist.enum_failed` 与 `blacklist.detected.empty` 文本相同但一个走 toast 一个走卡片，**不必合并**（合并会把 Rust 词条与 HTML 字典耦合，违反 P0-06 硬要求 1）。

## 5. 验收标准（§8）需改写 / 加前置的条目

- **条 4（L432）**：① 基线 v1.7.1 → **v1.8.1**；② 补两条互不串台：切「歌词」分区只触发 `cacheStats`（`index.html:599`）、切「黑名单」分区只触发 `mediaApps`。
- **条 6（L434）**：版本号按 §2-1 改；「在通用里切换数据目录」仍可验——`storage.dir` 行仍在 `#group-general`（`index.html:383-390`），无需改。
- **条 7（L435）**：保留，但「placeholder 也是英文」在补 `[data-i18n-placeholder]` 循环前**必红**（页面现今零该属性，`render()` 只刷 `textContent`，`index.html:634-636`）→ 写成「先补循环、再验」，否则会被误判成文案缺失。
- **其余 7 条（1/2/3/5/8/9/10）无需改**：未引用 v1.8.1 删掉的 `Config.window` / `accent_*` / `LyricsConfig.mode`（`progress.md:206`）；引用的「本地歌词目录」卡仍在（`index.html:280-287`）、`media` 是本版新字段。三条 W1/W2/W3 与 v1.8.0 缓存卡改动无交集。

## 6. 纪律

本文件是 G7 唯一复核产出：不回写 `docs/PRD-G7-黑名单.md` 正文、不改源码、不动 `progress.md` 与 `deliverables/`。ARCH 侧的 `index.html:281-312 / 315 / 512-519 / 550-552 / 809-814` 等行号引用已随 v1.8.0 漂移（现为 `306-337 / 340-357 / 591-603 / 634-636 / 898-919`），建议架构师在 T03 前自行校正——不影响结论。
