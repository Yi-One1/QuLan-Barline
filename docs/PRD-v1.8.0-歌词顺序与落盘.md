# 曲栏 Barline — v1.8.0 增量 PRD：歌词查询顺序可切换 · 落盘补齐 · 缓存可见

| 项目 | 内容 |
| --- | --- |
| 产品名称 | 曲栏 / Barline |
| 版本号 | `v1.8.0`（新增能力 + 新增配置项 → 升第二位） |
| 文档类型 | 增量 PRD（只覆盖本轮 4 条需求，其余行为沿用 `docs/PRD.md` 与 v1.7.0 口径） |
| 技术栈前提 | Rust + windows-rs；小组件 Direct2D 自绘；设置页 WebView2（`assets/ui/index.html`） |
| 语言 | 中文（设置页中英双语，默认中文） |
| 关联文档 | `docs/PRD.md`、`docs/ARCHITECTURE.md` §12.4–12.6、`progress.md` §3.2「G5 在线歌词落盘 → v1.7.0」 |

---

## 1. 背景与目标

**一句话：用户机器上「本地歌词目录\Barline\」至今是空的，而数据目录的 `lyrics-cache` 里已经躺了 93 个缓存文件——也就是说 v1.7.0 的「落盘」只在一条很少走到的分支上生效，用户真正想要的「查到的歌词就存一份到本地歌词目录，以后断网也能秒出歌词」没有实现。**

真机证据（用户确认）：

- 落盘目录 `E:\APP\曲栏Barline\本地歌词目录\Barline\` → **0 个文件**
- 缓存目录 `E:\APP\曲栏Barline\数据目录\lyrics-cache\` → **93 个文件**

代码侧根因（可复核）：

| 位置 | 事实 |
| --- | --- |
| `src/lyric/service.rs:294-382` `query_fast_path` | 顺序为 **内存(310-318) → 磁盘(320-328) → 本地(336-348) → 负缓存(350-358) → 在线(360-381)**，前两步命中即 `return`，**不经过落盘** |
| `src/lyric/service.rs:411-419` | `maybe_write_back` 只在 `on_online` 的「在线命中」分支被调用一次 |
| 结果 | 只要这首歌唱过一次（磁盘缓存 30 天 / 内存 LRU 8 条），以后再播就永远走缓存早退，**永远不会落盘** → 用户看到目录一直是空的 |

本轮四件事（用户已逐条拍板，口径不改）：

1. **R1** 让用户自己选「本地文件优先」还是「磁盘缓存优先」（默认保持现状）；
2. **R2** 把歌词磁盘缓存路径（与实际占用）摆到设置页，用户能看见、能自己判断要不要清；
3. **R3** 把落盘触发面补齐：**任何「拿到了歌词且来源不是本地文件」的路径都存一份**（本轮最重要）；
4. **R4** 落盘文件名改成用户认得的「歌名 - 歌手」（保留原始大小写）。

**产品目标（3 条，互相正交）**

- **不丢歌词**：只要这首歌在曲栏里成功显示过歌词，本地歌词目录里就有一份人类可读的 `.lrc`，断网 / 清缓存 / 换机后仍能直接命中。
- **顺序可解释**：查询顺序从「写死的实现细节」变成「用户可选的显式策略」，两种策略的代价用一句话讲清。
- **存储可见**：用户能一眼看到歌词缓存占了哪、占多少，不需要凭猜。

**非目标**：不做缓存清理按钮、不做批量导出、不改落盘目录位置、不动 LRC 渲染规则（仍是行级、无逐字标签、无 `[offset:]`）。

---

## 2. 需求描述（每条都含「用户看到什么」+「系统内部做什么」）

### R1 查询顺序可切换（`lyrics.local_first`）

**用户看到什么**

设置页「歌词」分区新增一个开关，默认**关**（＝保持 v1.7.0 现状）：

- **关（默认，磁盘缓存优先）**：最省事，重启也命中；但手工改过 `.lrc` 最长 30 天不生效（要等缓存过期或清缓存）。
- **开（本地歌词文件优先）**：每次切歌先扫本地歌词目录，手工改 `.lrc` 立刻生效；代价是每次切歌多一次目录遍历。

开关下方必须有一段说明把上面对比讲清（文案见 §4），不允许只写「本地优先」四个字。中英双语，改动即时生效（下一次切歌起按新顺序，不需要重启）。

**系统内部做什么**

- 新增配置 `lyrics.local_first: bool`，默认 `false`（`LyricsConfig` 见 `src/config/mod.rs:100-114`，`Default` 见 `src/config/defaults.rs:416-432`）。**旧 `config.json` 缺该字段必须落到 `false` 且不报错**（沿用容器 `#[serde(default)]` 的既有做法，与 `write_back` 同款）。
- `false`：`query_fast_path` 保持现有顺序**逐字节不变**（内存 → 磁盘 → 本地 → 负缓存 → 在线，`src/lyric/service.rs:310-381`）。
- `true`：**本地 → 内存 → 磁盘缓存 → 负缓存 → 在线**（用户裁决：本地排到内存**前**，理由是手改 `.lrc` 要立刻生效、**包括当前正在播放的那一首**——内存层若在本地之前，正在播放的曲子会一直吃旧的内存副本）。本地命中后仍走 `cache.put(&key, &l, "local")`（`service.rs:339`）把本地内容同时刷回内存与磁盘缓存——这是「手改立刻生效」的持久化保证，**不得省略**。
- 开关经 `LyricRequest` 传给歌词线程（现有同类字段：`local_dirs` / `online_enabled` / `write_back`，`src/event.rs:245-255`，构造点 `src/app.rs:1434-1442`）；设置页白名单加 `lyrics.local_first`（`src/settings/bridge.rs:113-143` 那一段，显式 match，非反射）。

---

### R2 设置页展示歌词磁盘缓存路径与占用

**用户看到什么**

设置页「歌词」分区新增一张只读卡片：

```
歌词磁盘缓存
缓存位置    E:\APP\曲栏Barline\数据目录\lyrics-cache      ← 可点击，点开资源管理器
已用空间    93 个文件 · 412 KB                            ← 进入分区时统计一次
说明        在线歌词按歌曲缓存在这里，命中就不再联网。单条 30 天过期、
            总量超过 5 MB 时自动删最旧的。这个文件夹可以安全删除——
            删掉后下次播放会重新查找并缓存；配置文件与已落盘的 .lrc 不受影响。
```

- 路径**只读展示**，来源必须是 `platform::data_dir().join("lyrics-cache")`（与歌词线程实际用的完全同源，见 `src/lyric/service.rs:203`）。用户改过数据目录就显示改后的实际路径，**禁止硬编码 `%LOCALAPPDATA%` 或任何拼接**。路径字符串由 Rust 侧组装并下发，JS **不拼接路径**（现有先例：`state_json` 的 `dataDir`，`src/settings/bridge.rs:62-64`）。
- 占用文本 = 顶层文件数 + 总字节（人类可读，如 `412 KB` / `1.2 MB`）。目录不存在时显示 `0 个文件 · 0 B`，不弹错、不崩。
- 路径可点击打开目录（复用 `openLogDir` 同款 `ShellExecuteW` 手法，`src/app.rs:1734-1749`）。
- **不做「一键清空缓存」按钮**（破坏性操作，不在本轮范围，见 §5）。

**统计读取时机（本轮裁决，实现必须照此）**

**进入歌词分区时读一次** —— 具体三个触发点，此外任何地方都不许读：

1. 用户点击左侧导航「歌词」（`assets/ui/index.html:519-526` 的 nav click 处加一次 `action`）；
2. 设置页打开时若初始就落在歌词分区（当前默认落在外观分区，因此正常路径只在条件 1 触发）；
3. 用户从别的分区切回歌词分区 → 再读一次（＝ 用户想刷新时的自然操作，不需要刷新按钮）。

**明令禁止**：`setInterval` / `setTimeout` 轮询、后台定时器、每次 `render()` 重读、每次 `state` 回推重读。同一分区内的开关改动触发 `state` 回推时，**不得**重新统计。

**系统内部做什么**

- 新增 JS→Rust 动作（如 `"lyricsCacheStats"`），Rust 侧 `run_settings_action`（`src/app.rs:1703-1747`）处理：`read_dir` 缓存目录**只读顶层**（缓存目录是平铺的 `<hex>.json` / `miss-<hex>.json`，不需要递归），累加 `metadata().len()`。
- 扫描上限 **5000 条**：达到上限立即停止并把文件数显示为 `5000+`（防用户数据目录被指向巨大目录时卡住 UI 线程）。
- 统计完成后向页面推一条独立消息（如 `{"type":"cacheStats","payload":{"dir":...,"files":N,"bytes":M,"truncated":bool}}`），JS 只负责把文本填进两张 `<span>`，重推时直接覆盖。
- 扫描期间占位文案为「统计中…」（正常情况下肉眼看不到）。

---

### R3 落盘时机补齐（本轮最重要）

**用户看到什么**

- 任何一首歌只要在曲栏里**显示过歌词**，隔一会儿（毫秒级）它的 `.lrc` 就会出现在 `本地歌词目录第一个目录\Barline\` 下。
- 已存在的同名文件**永不覆盖**（用户手改过的 `.lrc` 绝对安全）。
- 关掉「歌词保存到本地」开关后，任何来源都不再产生文件。
- `Barline\` 下文件名是 `歌名 - 歌手.lrc`（R4），双击能在记事本里正常看到行级 LRC。

**系统内部做什么**

- **落盘判定条件从「来源 == 在线」放宽为「来源 != 本地文件」**，覆盖三条此前落空的路径（每条都在 `emit(LyricReady)` **之后**执行，先显示后落盘，UI 无感）：
  1. **内存命中**（`service.rs:310-318`）→ 需要落盘；
  2. **磁盘缓存命中**（`service.rs:320-328`）→ 需要落盘；
  3. **在线命中**（`service.rs:411-419`）→ 保持现状，需要落盘。
  本地文件命中（`service.rs:336-348`）**不落盘**（歌词本来就在磁盘上，防御性早退保留，见 `service.rs:480-482`）。
- **缓存命中也必须能区分来源**：若命中的缓存条目本身来自本地文件（`Cache::put(..., "local")` 写入的记录，`src/lyric/cache.rs:232-241`），则**不落盘**——否则等于把用户已有的 `.lrc` 复制一份到 `Barline/`。当前 `Cache::get_mem/get_disk` 只返回 `Option<Arc<Lyrics>>`（`cache.rs:210-230`），**不暴露 `source`**，这是本轮必须由架构师定方案的一处接口缺口（见 §6 风险 6 与回传给主理人的问题）。
- 幂等与去重口径不变：`Worker.wrote`（≤ `limits::WRITE_BACK_DEDUP_CAP`=64）做本次运行去重；跨进程靠 `create_new(true)` 原子「不存在才创建」（`src/lyric/writeback.rs:211-222`），`AlreadyExists → Skipped(Exists)` 静默。
- `lyrics.write_back == false` 时**任何路径都不落盘**（判定必须在落盘入口统一收口，不得分散在三个调用点各写一遍）。
- 落盘失败/跳过只记日志，**不改事件、不碰缓存、不影响歌词显示**（沿用 v1.7.0 口径）。
- 落盘位置不变：`local_dirs[0]` 非空 + 绝对路径 + `is_dir()` → `local_dirs[0]/Barline/`，惰性 `create_dir_all`（`writeback.rs:86-96`、`188-209`）。

---

### R4 落盘文件名改为「歌名 - 歌手」（带空格、保留原始大小写）

**用户看到什么**

- 网易云播《晴天》/周杰伦 → 落盘文件名是 `晴天 - 周杰伦.lrc`（连字符两侧各一个空格，**不是** `周杰伦 - 晴天.lrc`）。
- 英文/大小写按播放器上报原样保留：`Hello World` / `Adele` → `Hello World - Adele.lrc`，**不**变成 `hello world - adele.lrc`。
- 没有歌手信息时退化为 `歌名.lrc`。
- 旧文件 `周杰伦 - 晴天.lrc` 依然能被命中（查找侧候选不变）。

**系统内部做什么**

- 文件名主干从「`locate::candidates(track)[0]`（= `歌手 - 歌名`，已全小写）」改为 **`candidates` 的第 2 项口径 `{title} - {artist}`**（`src/lyric/locate.rs:78-92`，第 86 行是 `artist - title`、**第 87 行才是 `title - artist`**；0-based 索引 `[1]`）。`locate::candidates` 本体**不改**（查找侧顺序与向后兼容都靠它）。
- 「保留原始大小写」= 不用 `candidates` 的归一化结果做文件名，而是用**原始 `track.title` / `track.artist`** 拼 `{title} - {artist}`，只做两件必要的清理：
  1. Windows 非法字符 `\ / : * ? " < > |` 及 CR/LF → **空格**（沿用 `normalize_name` 的口径，见 `locate.rs:41-73`；**不许换成 `_`**，换了就永远命中不了，这是 v1.7.0 已踩过的坑）；
  2. 空白折叠（连续空白 → 单个空格）+ 首尾 trim。
  大小写、全角字符、中文标点**一律保留原样**。
- **必须满足的可验证不变式**：写出的文件名主干 `stem` 满足
  `locate::normalize_name(stem) == candidates(track)[1]`（无歌手时退化为 `== candidates(track)[0]`）。
  这条不变式是「写了能被搜到」的机械保证：查找侧 `index_dir` 用 `normalize_name(file_stem)` 建索引（`locate.rs:179`），大小写与全角在归一化时被折平，所以保留大小写不会破坏命中。
- 无歌手 / `candidates` 只有 1 项时，回退用 `candidates[0]`（即 `歌名`），不得产出空名或 ` - 歌名.lrc`。
- 其余落盘规则**完全不变**：只写行级 LRC（`[ti:]/[ar:]/[al:]` + `[mm:ss.cc]`，不写逐字标签、不写 `[offset:]`，`writeback.rs:144-176`）、UTF-8 无 BOM + CRLF、保留名拦截（CON/PRN/AUX/NUL/COM1-9/LPT1-9）、主干 ≤120 码元 / 全路径 ≤259 码元就放弃不截断（`writeback.rs:107-142`）。

---

## 3. 验收标准（A1–A8，全部可由 QA 用测试或读代码判定）

### A1 · 默认顺序不变 + 旧配置向后兼容（R1）

1. 手工把 `config.json` 里的 `"local_first"` 整行删掉后启动：程序正常启动、日志无 serde 反序列化错误、设置页该开关显示为**关**。
2. `local_first=false` 时播放一首**已缓存**的曲子：日志出现 `歌词内存命中` 或 `歌词磁盘命中`，且**不出现** `歌词本地命中`（因为缓存命中即 `return`，`service.rs:311-328`）。
3. `local_first=false` 时**逐分支行为等价**：事件序列（E1）、三条命中日志文本逐字符不变（E2）、同一计时起点（E3）、`LyricLoading` 位置（E4）、最多一次 `LyricReady`（E5）、`false` 分支层序数组字面量为 `[Mem, Disk, Local]`（E6）；等价判据见 `docs/ARCH-v1.8.0-歌词顺序与落盘.md` §2.4。（原表述「逐语句等价、不允许改写既有行先后」作废——实现改为 `Step` 层序数组 + 单循环，该替代方案经架构 §2.4 授权，等价性由 E1–E6 + 结构红线 S1–S3 保证。）

### A2 · `local_first=true` 时本地文件优先（R1）

1. 造两条内容不同的歌词：本地文件 `local_dirs[0]/zz测试曲.lrc` 内容含标记 `AAA`，磁盘缓存 JSON 对应内容含标记 `BBB`；`local_first=true` 时**直接播放该曲，无需重启**（本地层先于内存层——即使该曲已在内存 LRU 里，也必须显示本地文件内容；这条同时是新顺序的实证）→ 小组件显示 `AAA`，日志为 `歌词本地命中`。
2. 同条件 `local_first=false` → 显示 `BBB`，日志为 `歌词磁盘命中`。
3. `local_first=true` 且本地命中后，磁盘缓存 JSON 的 `source` 被改写为 `"local"`（`service.rs:339` 的 `cache.put` 未被绕过）；**同时内存条目也被刷新**——紧接着播放另一首再切回本曲，仍显示 `AAA`（若只写磁盘不刷内存，切回时会拿到内存里残留的 `BBB`；`Cache::put` 内部是 `insert_mem` + 写磁盘，`cache.rs:232-241`）。
4. 切歌即时生效：运行中把开关从关切到开，**下一首**即按新顺序（`LyricRequest` 每次重建，`app.rs:1434-1442`），无需重启。

### A3 · 开关可持久化 + 双语不露键（R1）

1. 勾选后 `config.json` 出现 `"local_first": true`；取消后为 `false`；重启程序状态保持。
2. 中文 / English 两套文案均存在：页面上**不出现** `lyrics.local_first` / `lyrics.local_first.desc` 等裸键名。
3. 设置窗口 1280×800 下说明文案不被截断（换行显示完整），英文文案不溢出卡片。
4. `src/settings/bridge.rs` 白名单含 `lyrics.local_first` 且**只接受 `as_bool()`**：`{"path":"lyrics.local_first","value":"yes"}` 返回 `false`（拒绝），配置不变（与 `write_back` 同款用例，`bridge.rs:391-402`）。

### A4 · 缓存路径来源正确、无硬编码（R2）

1. 数据目录为默认值时，设置页显示的路径 == `platform::data_dir().join("lyrics-cache")`。
2. 把数据目录改到 `E:\APP\曲栏Barline\数据目录` 并重启 → 页面显示 `E:\APP\曲栏Barline\数据目录\lyrics-cache`（跟随用户配置变化）。
3. 全仓 grep：设置页路径**不得**由 JS 拼字符串（`index.html` 内不出现 `"lyrics-cache"` 字面量）、Rust 侧不得出现 `LOCALAPPDATA` 硬编码拼歌词缓存路径（唯一出处仍为 `service.rs:203` 的 `data_dir().join("lyrics-cache")`）。
4. 缓存目录不存在时：显示路径 + `0 个文件 · 0 B`，不弹错、不崩溃、不创建目录。

### A5 · 统计时机严格「进入分区读一次」，无定时器（R2）

1. 打开设置页（落在「外观」分区）→ **零次**扫描（无扫描日志）。
2. 点击左侧「歌词」→ **恰好一次**扫描日志；文件数/字节数与 `dir` 实际内容一致（QA 用 PowerShell 数一遍对照）。
3. 在歌词分区内改动任一开关（触发 `state` 回推 + `render()`）→ **不新增**扫描。
4. 切到「音量」再切回「歌词」→ **再扫一次**（共两次）。
5. 全仓 grep 不得新增用于该统计的 `setInterval` / `setTimeout` 循环 / `WM_TIMER` / 后台线程；扫描只在 `run_settings_action` 的动作分支里发生。
6. 上限行为：目录内 >5000 个文件时停止扫描并显示 `5000+`，设置页仍可正常操作（读代码确认 `MAX 5000` 常量与提前 `return`）。

### A6 · 三条路径都落盘（R3，核心）

前置：每项测试前把 `local_dirs[0]/Barline/` 清空。

1. **在线命中**：播放一首从未查过的曲子 → 在线命中后出现 1 个 `.lrc`，内容为行级 LRC（首行 `[ti:...]`），无 `<`、无 `[offset:`。
2. **内存命中**：同一首曲子重播，且 `Barline/` 已被清空、磁盘缓存保留 → 重新出现 1 个 `.lrc`（**这条是 v1.7.0 漏掉的缺口**）。
3. **磁盘缓存命中**：重启程序（内存 LRU 清空）后播放同一首曲子，`Barline/` 已清空 → 出现 1 个 `.lrc`。
4. **绝不覆盖**：预先在 `Barline/` 放同名文件、内容写 `USER` → 上述任一场景后该文件内容仍为 `USER`，`mtime` 不变，日志记 `歌词不落盘：Exists`。
5. **本地命中不落盘**：只有本地 `.lrc`（缓存已清）→ `Barline/` 不新增文件（本地命中分支不调用落盘）。
6. **来源为 local 的缓存命中不落盘**：让一首歌先走本地命中（写入 `source="local"` 缓存），清空 `Barline/` 后重播 → `Barline/` 仍为空（不得把用户自己的 `.lrc` 复制一份）。
7. **事件顺序不变**：日志里 `LyricReady` 相关记录先于 `歌词已落盘`；落盘失败（如把 `Barline` 建成同名**文件**占位）时不产生任何 UI 错误、不影响歌词显示。

### A7 · `write_back=false` 全链不落盘（R3）

1. 关闭开关、清空 `Barline/` 后，分别制造在线命中 / 内存命中 / 磁盘命中三种场景 → `Barline/` 目录**始终不存在或为空**（0 文件）。
2. 关闭状态下日志**不出现** `歌词已落盘`。
3. 判定收口在单一入口（读代码：三个调用点不得各自复制 `if !write_back { return; }`）。

### A8 · 文件名 = 「歌名 - 歌手」（保留大小写）且写后可回查（R4）

1. 命名断言（表驱动，纯函数级即可）：
   - `title="晴天", artist="周杰伦"` → `晴天 - 周杰伦.lrc`
   - `title="Hello World", artist="Adele"` → `Hello World - Adele.lrc`（大小写保留、连字符两侧各一空格）
   - `title="A/B:C*D", artist="X"` → `A B C D - X.lrc`（非法字符 → 空格，**不得**出现 `_`、不得出现 `/` `:` `*`）
   - `title="晴天", artist=""` → `晴天.lrc`（无歌手回退）
2. 不变式断言（每条都要过）：`normalize_name(stem) == candidates(track)[1]`，无歌手时为 `[0]`。
3. 闭环断言：落盘成功后**立刻** `locate::find(track, dirs)` 必须命中**同一路径**（沿用 v1.7.0 的 W5/W15 闭环写法，`writeback.rs:337-366`、`573-590`）。
4. 向后兼容：目录里只有旧式 `歌手 - 歌名.lrc`（如 `周杰伦 - 晴天.lrc`）时，`locate::find` 仍命中（`candidates[0]` 口径未变）。
5. 内容不变：文件名改了但内容规则不变——`[ti:]/[ar:]/[al:]` + 行级时间戳、无 `<`、无 `[offset:`、UTF-8 无 BOM、CRLF（`render_lrc` 零改动）。
6. 既有测试处置有据：仅允许改写「断言文件名 == `candidates()[0]`」的用例（`writeback.rs` 的 `w1_file_name_equals_first_candidate`、`w2`、`w5` 等），改写必须注明「v1.8.0 R4 改口径」，**其余 494 条断言一条不得放宽**。

---

## 4. 中英双语词条清单（键名 + 中文 + 英文）

命名风格沿用 `assets/ui/index.html` 现有 `lyrics.*`（zh 见 `:406-414`，en 见 `:451-459`）。

| 键名 | 中文 | English |
| --- | --- | --- |
| `lyrics.local_first` | 本地歌词文件优先 | Prefer local lyric files |
| `lyrics.local_first.desc` | 「关」（默认）：优先用磁盘缓存，重启也命中、最省事，但手工改的 .lrc 最长 30 天不生效，要等缓存过期或清理缓存。「开」：每次切歌先扫描本地歌词目录，再读内存缓存——手工改的 .lrc 立刻生效，正在播放的这一首也生效；代价是每次切歌多一次目录遍历（目录越大越慢）。 | Off (default): disk cache first — fastest and survives restarts, but manual .lrc edits only take effect after the 30-day cache expires or is cleared. On: scan local lyric folders before the memory cache on every track change, so manual .lrc edits apply immediately — including the track currently playing — at the cost of one extra folder scan per track. |
| `lyrics.cache` | 歌词磁盘缓存 | Lyric disk cache |
| `lyrics.cache.path` | 缓存位置 | Cache folder |
| `lyrics.cache.open.hint` | 点击打开文件夹 | Click to open the folder |
| `lyrics.cache.usage` | {files} 个文件 · {size} | {files} files · {size} |
| `lyrics.cache.usage.more` | {files}+ 个文件 · {size} | {files}+ files · {size} |
| `lyrics.cache.scanning` | 统计中… | Counting… |
| `lyrics.cache.desc` | 命中的歌词按歌曲缓存在这里，命中就不再联网。单条 30 天过期，总量超过 5 MB 时自动删最旧的。这个文件夹可以安全删除——删掉后下次播放会重新查找并缓存；配置文件与已经保存到本地歌词目录的 .lrc 都不受影响。 | Matched lyrics are cached here per track, so a hit needs no network. Entries expire after 30 days, and the oldest are pruned beyond 5 MB. This folder is safe to delete — the next play simply looks up and caches again; your config and the .lrc files saved into your local lyric folder are unaffected. |
| `lyrics.writeback`（**改文案**） | 歌词保存到本地 | Save lyrics locally |
| `lyrics.writeback.desc`（**改文案**） | 只要拿到歌词时它不在本地歌词目录里（在线查到、内存或磁盘缓存命中），就会保存一份到「本地歌词目录」第一个目录下的 Barline 子文件夹，文件名是「歌名 - 歌手.lrc」。只新建，绝不覆盖已有文件（手工改过的 .lrc 安全）。查询顺序为「磁盘缓存优先」时，手工改 .lrc 需等缓存过期或清理缓存才生效；切到「本地歌词文件优先」可立即生效。 | Whenever lyrics are obtained and are not already in your local lyric folders (fetched online, or from the memory/disk cache), a copy is saved into the Barline subfolder of the first local lyric folder, named "Title - Artist.lrc". New files only — existing files are never overwritten (your manual edits are safe). With disk-cache-first order, manual .lrc edits apply only after the cache expires or is cleared; switch to local-first to apply them immediately. |
| `lyrics.sub`（**建议改文案**） | 优先使用已缓存的歌词，其次本地 .lrc 与在线歌词；顺序可在下方切换。 | Cached lyrics first, then local .lrc files and online sources; the order is adjustable below. |

> 说明：`lyrics.writeback` 的标签与说明**必须一起改**——R3 之后落盘不再只发生在「在线命中」，旧文案「在线歌词保存到本地」会与实际行为不符。

---

## 5. 边界与明确不做的事

1. **不改默认查询顺序**：`local_first` 默认 `false`，老用户升级后行为与 v1.7.0 完全一致。
2. **不做批量导出**：不提供「把 `lyrics-cache` 里那 93 个缓存转换成 `.lrc`」的功能（缓存是带 `words` 的 JSON，导出等于新造一条落盘链路）。用户要的效果由 R3 在下次播放时自然达成。
3. **不做「清空缓存」按钮**：破坏性操作，本轮用户未拍板（v1.7.0 留档曾建议，但本轮范围只有「展示路径」）。文案只告知「可以安全删除」，动作交给用户自己。
4. **不做动画 / 进度条 / 自动刷新**：统计文本直接覆盖，不加过渡、不加骨架屏、不加定时器。
5. **不动落盘目录**：仍是 `local_dirs[0]/Barline/`，不新增第二个落盘位置，不提供自定义落盘目录。
6. **不动 LRC 渲染规则**：行级、无逐字标签、无 `[offset:]`、UTF-8 无 BOM + CRLF 全部沿用。
7. **不动其它模块**：`locate` / `provider` / `cache` 的**对外语义**不因本轮改变（`candidates` 顺序、`index_dir` 归一化、TTL 与 5 MB 上限都不动）；`render` / `platform` / `media` / `taskbar` / `ui` 零改动。
8. **不动交付物与文档纪律**：`deliverables/` 一个字节不动；`docs/PRD.md` 与 `progress.md` 由主理人在验收通过后统一更新，本轮只产出本文件。
9. **不做 G7 黑名单等其它队列**：本轮不碰媒体会话过滤链路。

---

## 6. 风险与已知取舍

1. **本地优先在超大歌词目录下的遍历成本**（R1）。
   `locate::index_dir` 每次要重建整棵目录索引，硬上限「深度 ≤4、总条目 ≤5000」（`src/lyric/locate.rs:23-25`、`147-188`）；用户把整个 D 盘加进歌词目录时，单次扫描可能几十到上百 ms，且发生在**歌词线程**——不会卡 UI，但会让这首歌的歌词晚出来、并延后该线程处理下一条请求。默认（磁盘优先）只在缓存未命中时付这笔成本；开了本地优先**每次切歌都要付**。文案已提示「目录越大越慢」，本机目标是把本地目录控制在几千个 `.lrc` 以内。
2. **保留原始大小写后文件名可能含全角字符**（R4）。
   写入侧保留全角（如 `，`、全角空格、`ＣＤ`），查找侧 `normalize_name` 会折成半角再比对（`locate.rs:41-73`）——**命中不受影响**，但用户在资源管理器里看到的文件名与其在别的播放器里看到的可能不一致；对全角/半角混用的同一首歌，`Barline/` 里可能同时存在两个名字（视为两个不同文件，`create_new` 各自成功）。接受。
3. **缓存里的标题/歌手与播放器上报不一致时落盘名可能命中不了**（R3+R4 组合风险，最需要留档）。
   缓存键走 `query::cache_key`（会摘 `_哔哩哔哩_bilibili` 平台后缀与首尾 `【】「」` 装饰），而落盘文件名走 `candidates(track)`（用**原始 track**）。因此 B 站等场景写出的文件名会**带着平台后缀**。同一播放器下次上报同样标题时仍能命中；但若上报风格变化（带/不带后缀）、或换播放器播同一首歌，落盘名与候选名就不等 → 命中不了，而落盘规则是「只新建不覆盖」→ 该无效文件**永久留存且不会自愈**。本轮不修（修它要破坏「落盘名与查找侧机械等价」这条更重要的性质），先靠 A8 的不变式断言把「与当前 track 一致」锁死。
4. **R3 扩大了落盘触发面 → `Barline/` 增长更快、且只增不减**。
   引擎没有配额、没有清理 UI（v1.7.0 已知取舍）。听 1000 首约几十 MB；用户若要回收只能自己删目录（文案已说明可安全删除）。
5. **R3 增加了歌词线程的同步 IO 次数**。
   原先每首歌最多一次落盘尝试，现在「缓存命中」也会各尝试一次（同会话内被 `wrote` 去重，最多一次）。`create_new` 是微秒级，但落盘文件不存在时会真的写一次（几 KB）。杀软实时扫描环境下可能偶发几 ms 抖动，全部发生在歌词线程，不影响 UI 帧。
6. **缓存层不暴露 `source`，与「来源为 local 的缓存命中不落盘」存在张力**（A6.6）。
   现在 `Cache::get_mem/get_disk` 只返回 `Option<Arc<Lyrics>>`（`cache.rs:210-230`），`source` 只躺在磁盘 `DiskLyrics` 里（`cache.rs:84-88`）。若不加接口，缓存命中一律落盘 → 会把用户在 `local_dirs[1]` 里已有的 `.lrc` 复制一份进 `local_dirs[0]/Barline/`。两种处置（架构师定）：
   - **(a)** 扩缓存接口返回 `(Arc<Lyrics>, source)`（或 `Option<String>`），沿用 `source == "local"` 早退 —— 语义最准，改动面在 `cache.rs` + `service.rs`；
   - **(b)** 缓存命中一律落盘，接受「可能多一份副本」—— 零接口改动，几 KB 冗余，但用户可能会看到重复文件。
   **PM 倾向 (a)**：它让「来源 != 本地才落盘」这条规则在三条路径上口径统一，也给 R2 的「占用」文案兜住底。
7. **「手改 .lrc 立刻生效」存在口径缺口（内存层）—— 已消解（R1 顺序裁决）。**
   原始风险：若内存层排在本地之前，则同一会话内、该曲仍在内存 LRU（8 条、无 TTL，`cache.rs:186-216`）时，手改 `.lrc` 不会生效（只能等重启或被挤出 8 条之外）。
   **消解结论（用户裁决「本地排到内存前」）**：`local_first=true` 时顺序为「本地 → 内存 → 磁盘缓存 → 负缓存 → 在线」，本地命中先于内存命中，因此**包括当前正在播放的那一首**在内，手改 `.lrc` 在下次切歌 / 重播时立即生效；**不存在「重启后生效」的限制**。设置页文案按此表述（见 §4）。
8. **统计占用的成本与精度**（R2）。
   只数顶层文件、上限 5000 条：与实际占用在正常情况（平铺 JSON）完全一致；极端情况（用户把数据目录指到一个几万文件的目录）会显示 `5000+` 并低估字节。接受。
9. **`local_first=true` 的目录遍历是固有代价 —— 已裁决不优化**（R1，与 6.1 同源，此处只记裁决，不重复描述成本）。
   开本地优先后，**每次切歌**都要付一次 `index_dir` 目录遍历（深度 ≤4、每目录 ≤5000 条目，`locate.rs:23-25`）——这是「手改立刻生效」换来「当前正在播放的那首也生效」的必然代价，本机在超大歌词目录下歌词显示会相应推迟。**本轮不另做优化**（不做索引缓存 / 不做增量扫描 / 不做 watch 通知），代价已写进开关说明（`lyrics.local_first.desc`「目录越大越慢」，§4）与 6.1；后续若要优化，属新需求，不在 v1.8.0 范围。

---

## 7. 待架构师先回答的问题（1 项待答 + 1 项已裁决）

1. **缓存命中的来源可见性**：R3 要求「来源为本地文件的缓存命中不落盘」，但 `Cache::get_mem/get_disk` 不返回 `source`。是扩接口（方案 a），还是接受复制（方案 b）？这直接决定 A6.6 能否成立、以及 `cache.rs` 的改动面。
2. **内存 LRU 与「手改立刻生效」的冲突 —— 已裁决（用户裁决「本地排到内存前」）。**
   原问题：`local_first=true` 时是否允许本地命中绕过/覆盖内存缓存？不允许 → 文案必须如实写「重启后生效」；允许 → 等于调整用户拍板的「内存 → 本地」顺序，需用户确认。
   **裁决**：`local_first=true` 时顺序为「本地 → 内存 → 磁盘」，本地命中后 `cache.put` 同时刷回内存与磁盘缓存（见 §2 R1），因此不存在「重启后生效」的限制。此项已关闭，§7 只剩上面第 1 问（`source` 可见性）。

（次要待定，不阻塞设计：R2 的 `cacheStats` 消息是复用 `state` 增字段还是新增消息类型；R4 的原始大小写拼名是否要单独暴露一个 `locate::display_candidates` 纯函数供 `writeback` 复用，以保住 A8.2 的不变式断言。）
