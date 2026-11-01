# 曲栏 Barline — 开发进度交接文档(progress.md)

> 用途:供「新开对话 / 接手者」快速恢复上下文。本文档与 `docs/` 下的 PRD / ARCHITECTURE / 各 QA 报告互补,本文件是**唯一需要通读就能上手**的索引。
> 最后更新:**2026-11-02 06:22,批次④(问题 8/11/12)v1.1.14 已验收;下一批:批次⑤(问题 6+9+7:会话音量+循环随机+封面激活,最后一批)。**

---

## 0. 项目一句话定位

Windows 任务栏「媒体小组件」:**Rust + Direct2D 逐像素透明分层窗口**,内嵌任务栏条带(贴托盘区左侧 8px),通过 **SMTC** 读取/控制媒体会话,支持**封面/播控三键/进度条/音量 HUD/逐字卡拉OK歌词/壁纸自适应对比度/托盘菜单/全局快捷键/WebView2 设置页**。

| 项 | 值 |
|---|---|
| 语言/栈 | Rust 1.99.1 (MSVC) + `windows` 0.62.2 + Direct2D/DWM/SMTC + WebView2 |
| 构建 | `powershell -ExecutionPolicy Bypass -File scripts/build.ps1 -Task check/clippy/test/build`(**禁止直接 `cargo build`**,见 §3 约束) |
| 仓库 | 本地 `E:\APP\WorkBuddy\工作空间\曲栏Barline`;远端 **https://github.com/Yi-One1/QuLan-Barline.git**(master,上游跟踪已建;push 若遇会话代理 502,用 `git -c http.proxy=http://127.0.0.1:7890 -c https.proxy=http://127.0.0.1:7890 push` 走本机 7890 代理) |
| 当前版本 | **v1.1.14**;交付包在 `deliverables/`,**只增不删** |
| 快捷启动 | 根目录 **`启动曲栏.bat`**(增量构建+运行未打包 exe,用户自检最新改动用) |
| 团队 | 交付总监(主理人)+ 寇豆码(software-engineer)+ 严过关(software-qa-engineer);团队 `software-barline-fix` 活跃,寇豆码上下文延续(可 SendMessage 继续) |
| 推进节奏(用户定) | **一次只做一项 → 带有效测试 → 提交 → 用户验收 → 下一项**;全部完成后跑全量测试 |
| 版本策略(用户定) | 每完成一项 bump 补丁号并出包 `deliverables/曲栏_Barline_vX.Y.Z.exe`(**历史包永久保留,绝不删除**) |

---

## 1. 提交序列(git log,oneline)

```
16187f5  fix: v1.1.14 — 右键显示设置接通+设置页取消永久置顶+菜单不再被遮(批次④ 问题8/11/12)
<docs>   docs: progress.md — 批次③(问题2+10)全闭环 v1.1.11→v1.1.13
06651be  fix: v1.1.13 — 常规半透明不生效+滑条不即时(批次③ v1.1.12 反馈)
60a456f  feat: v1.1.12 — 常规/悬停背景两态可选+透明度可调,「始终透明」改名「透明」(批次③ 反馈)
a7902b8  feat: v1.1.11 — 删除模糊背景只留半透明/透明,修歌名有空间被省略(批次③ 问题2+10)
<docs>   docs: progress.md — 批次②(问题5)闭环 v1.1.10;批次③成为当前
35332f8  fix: v1.1.10 — 设置页步进空白bug+滚轮10%被钳成5%(批次② 问题5)
<docs>   docs: progress.md — 批次①(问题3)全闭环;批次②缩减为问题5
eef66bf  feat: v1.1.9 — 目录路径可点击打开,按钮重排,删关于页重复项(批次① 界面反馈)
e9e10e6  fix: v1.1.8 — 歌词目录行内更改按钮对齐(批次① 问题3 界面反馈)
02f2ccf  feat: v1.1.7 — 目录更改统一弹窗自选迁移/删除,歌词目录可改位置(批次① 问题3 反馈)
aeef464  feat: v1.1.6 — 数据目录净搬+A→B切换支持(批次① 问题3 验收反馈)
bd11ba7  feat: v1.1.5 — 数据目录可自定义+启动自动迁移(批次① 问题3)
<docs>   docs: progress.md — v1.1.4 验收通过,问题1全闭环;登记问题10/11/12;批次计划定稿
8dbe1fb  feat: v1.1.4 — 跑马灯提前滚完留读尾时间,删除右缘渐隐带(问题1验收反馈)
6a9b5e8  feat: v1.1.3 — 跑马灯改行时间窗口驱动,歌词行再上移2px(问题1遗留a+b)
a6b4552  feat: v1.1.2 — 歌名歌手紧凑排列,歌词行上移,长歌词滚动(可开关)
ad2ef3d  feat: v1.1.1 — 歌词单行大字号,歌手名移至歌名右侧(布局修订)
d65cbbc  chore: 根目录添加开发快捷启动 bat
4145b89  feat: 歌词区移入任务栏条带(双行压缩 160px)   ← v1.1.0
6ba2b9f  chore: 版本号 1.1.0
836ced0  docs: QA M5 交付前终验报告(v1.0.0)
84fe061  docs: 添加 README 与 MIT LICENSE
5b425ac  feat: v1.0.0 — 版本资源 / 关于页版本号 / 打包准备
8af76f6 / bbdc327  v0.0.8 M3 遗留 P2 补全 + QA 报告
2ab866f / a667218  v0.0.7 M4 第二批 QA 缺陷修复 + QA 报告
ec803b0  feat: v0.0.6 — M4 第二批(WebView2 设置页)
be7f8d9 / b9d204b / 1f66500 / b29e619  v0.0.5~v0.0.2(M4 第一批/QA 清零/M3/M1+M2)
```

---

## 2. 已完成事项

### 第一轮(v1.0.0 之前,详见 docs/ 各报告)
- M1 骨架、M2 完整小组件、M3 歌词引擎(逐字卡拉OK)、M4 第一批(托盘/快捷键/lrclib/退出卡死)、M4 第二批(WebView2 设置页 + QA 三缺陷修复闭环)、M3 遗留 P2(长行渐隐/行切换上移/设置页即时回推)、M5 打包(v1.0.0,README/LICENSE/版本资源/GitHub 推送)。
- 四份 QA 独立验收报告全通过:`docs/QA-M2/QA-M4/QA-M3P2/QA-M5-REPORT.md`。

### 第二轮:问题清单修复(当前进行,批次制推进)
用户清单(一次一批,批内可合并同类):

1. **歌词移入小组件(✅ 已验收 2026-11-02,四版迭代)**
   - v1.1.0(4145b89):歌词区移到歌名歌手右侧 160px 双行压缩,高度恒定 48px 不出任务栏,PRD P0-05 修订(作废"向桌面扩展")。
   - v1.1.1(ad2ef3d):用户反馈字太小 → **单行歌词 11px,上行=歌名+歌手(右侧),下行=歌词**;设置页删「行数」项;右键菜单删单/双行项;WidgetForm 收敛为 {Compact, Lyric}。
   - v1.1.2(a6b4552):**紧凑排列**;歌词行上移 1.5px;**长歌词跑马灯**(固定 30px/s+停留循环;`painter::marquee_offset` 纯函数);设置页「歌词」分区新增「长歌词滚动」开关默认开(`lyrics.scroll`);239 测试。
   - v1.1.3(6a9b5e8):遗留 a+b——**跑马灯改行时间窗口驱动**(`LyricDraw` 携带 `line_start_ms/line_end_ms`;删行指纹归零机制 lyric_scroll_ms/lyric_scroll_key);**歌词行再上移 2px**(`LYRIC_LIFT` 1.5→3.5)。240 测试。用户验收反馈:最后一个字偶尔看不清/偶尔发淡。
   - v1.1.4(8dbe1fb):反馈闭环——**跑马灯提前滚完**(滚动窗口 = 演唱窗口 − min(1.2s, 窗口1/4);`LyricDraw.line_sing_end_ms`=最后一个字 end_ms,无字级时间戳退化为行尾);**删除右缘 24px 渐隐带**(LYRIC_FADE_W/fade_band/draw_lyric_line/Frame.fade/fade_brush 全删,普通路径实色硬裁,修掉"刚好放得下的行最后一个字被渐变吃淡";`MARQUEE_MIN_WINDOW_SECS` 2.0→1.5 滚动窗口口径);设置页中英文描述/README/PRD 同步。241 测试 / clippy 零。**用户验收通过**。
2~9. 见 §4 待办(批次②③④⑤)。
10. **新问题(2026-11-02 验收时发现,已登记)**:紧凑排列下歌名有空间却被省略——歌名"主动"+歌手"加木"明明放得下,歌名显示"主…"(布局截断判定 bug,layout.rs)。排入批次③。
11. **新问题(2026-11-02,已登记)**:设置页窗口始终置顶,不关闭时会遮住其他窗口(应改为普通窗口)。排入批次④。
12. **新问题(2026-11-02,已登记)**:右键小组件的菜单应始终在小组件上层,但有时小组件盖在菜单上层,菜单内容被遮(菜单窗口 z-order,menu.rs)。排入批次④。

### 批次①:问题 3 存储位置自定义(✅ 已验收 2026-11-02,六轮迭代 v1.1.5→v1.1.9)
- **v1.1.5(bd11ba7)**:新增 `config.storage.data_dir`(空=默认 LOCALAPPDATA 下 Barline);`platform::data_dir()` 全局解析器;`src/storage.rs`(解析/迁移/move_item);`log::relocate` 运行期换日志路径;设置页「通用」数据目录行(展示+更改 IFileDialog+恢复默认+重启生效 toast);关于页「日志目录」改「数据目录」。测试 247。
- **v1.1.6(aeef464)**:用户反馈①迁移后原目录数据/文件夹要删干净 → **净搬**(copy 回退后删源,不再留 .old,顺带清理 v1.1.5 遗留 .old;旧目录腾空后 remove_dir 整删;默认根因 config.json 必然留下);反馈②发现真缺陷:**A→B 切换不迁移** → 引入 `config.storage.active_data_dir`(上次实际生效目录,启动写回)作迁移源。测试 249。
- **v1.1.7(02f2ccf)**:用户新需求**所有目录更改统一弹窗自选**(是否迁移文件/是否删除原目录,MessageBoxW YESNO 两连问,选项记 `storage.pending_migrate/pending_delete_old` 重启执行);**歌词目录可改位置**(行内「更改」,迁移=move_lrc_files 递归净搬 .lrc 保持子目录结构,删除=整个原文件夹 remove_dir_all,立即执行);**「添加」改系统目录选择对话框**(原文本输入删除)= **批次②问题 4 就此吸收完成**;`platform::confirm_dialog` 新增;删净豁免 config.json/config.json.tmp。测试 251。
- **v1.1.8(e9e10e6)**:界面反馈——歌词目录行内「更改」按钮位置偏左,`.list .item` 路径文本 flex:1 吃满剩余宽度、按钮成组靠右。
- **v1.1.9(eef66bf)**:界面反馈四项——①「×」移到「更改」左侧;②两处「更改」去省略号(中英);③两处目录路径变蓝色链接点击资源管理器打开(`openLyricsDir:<idx>` / openLogDir 复用);④删除「关于-数据目录」重复项(state.logDir 字段/bridge 参数/i18n 全链清除)。
- 界面验收通过(2026-11-02),批次①闭环。

### 批次②:问题 5 设置页音量「调节步进」空白(✅ 已验收 2026-11-02,v1.1.10)
- **根因**:`volume.step` 是 f32,serde_json 序列化时拓宽 f64(0.05f32→0.05000000074505806),设置页 JS `String(value)` 与下拉选项 "0.05" 永不相等 → select 恒空白。
- **修复**:序列化钩子 `serialize_step_percent` 按 0.01 粒度取整(设置页与 config.json 都存干净值,内存 f32 不动);**连带修复** `on_wheel` 步进钳位 0.01..=0.05 笔误(设置页 10% 被静默钳成 5%),改为与桥接层同口径 clamp(0.01, 0.2)。252 测试。用户验收通过。

### 批次③:问题 2 悬停去模糊 + 问题 10 歌名省略(✅ 已验收 2026-11-02,三版迭代 v1.1.11→v1.1.13)
- **v1.1.11(a7902b8)**:
  - **问题 2**:模糊整链删除(净删 635 行)——`render/blur.rs`、sampler `build_blur`/`need_blur`/`corner_radius`、`BlurBuffer`、`state.blur`、`TIMER_BLUR` 定时器、`Frame.blur` 位图缓存、`bitmap_from_bgra`、`BLUR_*` 常量、`Effect::DropBackdrop` 全删;`BackgroundMode` 收敛为 {Translucent(默认), Transparent},旧配置 `"blur"` 经 serde alias 迁移为半透明;壁纸亮度采样**保留**(文字对比度仍靠它)。
  - **问题 10**:歌名"主动"+歌手"加木"显示"主…"——双根因:①layout 紧凑排列自然宽 **round→ceil**(矩形宽 < 实测宽让省略判定误判放不下);②`text::measure` 增加 **lineCount>1=放不下**(被换行拆短的多行布局此前会误报)。248 测试。
- **v1.1.12(60a456f)**:用户新需求——①常规(不悬停)背景也可选透明/半透明(`appearance.idle_mode`,默认透明=旧行为);②两态不透明度滑条可调(`hover_alpha`/`idle_alpha` 百分比 1..=100,默认 70/50;70%→0xB3 与旧固定值一致);③「始终透明」改名「透明」(设置页+右键菜单中英);`Theme.idle_bg` 新字段 + `mix_rgba` 悬停淡入淡出改为 idle→hover 两态插值;设置页 appearance.* 改动立即重采样。249 测试。
- **v1.1.13(06651be)**:用户反馈两问题**同源**——painter 背景守卫 `hover_a > 0.004` 是旧语义遗留,常规态(hover_alpha=0)整帧跳过背景 → idle_bg 永远不上屏(离开后"失效"+滑条看不到变化都是它)。守卫改 `hover_a > 0.004 || idle_bg.a > 0`;新增 `Theme::apply_bg_config` 就地同步主题,设置页/右键菜单/模式切换三触发点接入,滑条拖动即时重绘不再等异步采样。
- 用户验收通过(2026-11-02),批次③闭环。

### 批次④:问题 8/11/12 菜单与窗口层级(✅ 已验收 2026-11-02,v1.1.14)
- **问题 8**:右键菜单「显示设置」是 M3 占位(只记日志)→ 接通与托盘同链路 `on_second_instance()`(已开则前置,未开则创建)。
- **问题 11**:`platform::force_foreground` 抢前台后 `SetWindowPos(HWND_TOPMOST)` **没有撤销** → 设置页被永久钉在 topmost 带。补经典两连:TOPMOST 抬到最前立刻 NOTOPMOST 降回;AttachThreadInput/SetForegroundWindow 逻辑不变。
- **问题 12**:1s 心跳 `ensure_on_top()` 在 TrackPopupMenuEx 模态循环期间照常派发(WM_TIMER 会被菜单循环处理)→ 每秒把 topmost 小组件抬到非 topmost 菜单之上。新增 `App.menu_open` 标志,`show_menu` 期间暂停 ensure_on_top。
- 用户验收通过,批次④闭环。

### 交付包现状(deliverables/,只增不删)
`曲栏_Barline_v1.0.0.exe`、`.zip`、`v1.1.0.exe` ~ `v1.1.14.exe` 全序列。
> 注:worktree 检出 `.ps1` 会触发 git working-tree-encoding 转码失败;**重建历史版本包**用「主仓库 detached checkout <hash> → build.ps1 build → 拷 exe → checkout master」方式(已验证可行)。

---

## 3. 关键决策(拍板记录,避免推翻重来)

### 本轮新增
1. **布局(v1.1.1/v1.1.2 终态)**:高度**恒定 48px**(=2+22+22+2,随条带厚度自适应),任何形态不出任务栏;上行=歌名+歌手紧凑同排(歌手左缘=歌名右缘+6px),下行=单行歌词(11px SemiBold,歌名 12px/歌手 10px);信息区宽=max(紧凑总宽,160px),上限 340px;歌词开关=200ms 宽度+alpha 动画;**扫光码元映射不可错位是底线**。
2. **设置开关分区**:「长歌词滚动」放**歌词分区**而非外观——config 路径 `lyrics.*` 与分区一一对应。
3. **旧配置兼容**:删除的配置字段(two_line 等)**保留反序列化、布局层忽略**,不迁移不删除。
4. **右键菜单联动**:设置页删「行数」后,右键菜单「单行/双行歌词」死项一并删除,菜单 ID 重排。
5. **QA 无独立复验轮**:第二轮按用户节奏,工程师自测+主理人出包+**用户真人验收**即闭环;全量 QA 回归放到 9 项全部完成后。

### 历史铁律(必须继续遵守)
6. **构建纪律**:一律 `scripts/build.ps1`(-Task check/clippy/test/build),**禁止裸 cargo**(Git Bash link.exe 抢占 MSVC);测试汇总需直跑 target/debug(deps) 下测试二进制(windows_subsystem 吞 stdout)。
7. **本机安全约束**:禁 `reg.exe`、`Add-Type`、`cmd.exe`(WorkBuddy 工具层拦截)、`rm` 清 %LOCALAPPDATA%;注册表读取用 Rust API(RegGetValueW);截屏统一 `scripts/grab-screen.py`;启动 exe 后台起+父进程保活(或让用户用 启动曲栏.bat 自己跑),结束 `Stop-Process -Name barline -Force`;WorkBuddy PowerShell 工具里**禁用 WScript.Shell COM**(快捷方式不能代建);Bash 里调 powershell 亦被拦,构建用 PowerShell 工具跑。
8. **QA 纪律**:独立 git worktree(`E:\APP\WorkBuddy\工作空间\barline-qa-m4`)验收,不信任工程师自述。
9. **系统控件避让**:用户自有 BarlineWidget 不避让,逻辑已删。
10. **CPU 口径**:扫光开 ≤2%、扫光关 ≤ 扫光开(同环境相对判定,壁纸轮播噪声使绝对值不可测)。
11. **内存口径**:关窗后私有内存有界且不随开关增长(3 轮增量<1MB 且峰值<60MB)= PASS;NT 堆/LFH 页保留属正常。
12. **保持 25fps** 扫光(降 fps 有可见台阶,不划算)。

### v1.1.3/v1.1.4 补充(已验收,继续遵守)
13. **跑马灯 = 行时间窗口驱动 + 提前滚完**:滚动量按当前行 `[start_ms, sing_end_ms)` 线性分配(与扫光同一时间基准 = 播放位置外推,暂停冻结、拖动跟随,无行指纹机制);滚动窗口 = 演唱窗口 − min(1.2s, 窗口1/4),行尾+行间间歇留给读尾;滚动窗口 <1.5s(`MARQUEE_MIN_WINDOW_SECS`)不滚只显示行首。
14. **右缘渐隐带已删除(v1.1.4)**:普通路径实色硬裁,滚动关闭 = 行首截断;不许再引入任何"把最后一个字变淡"的绘制效果。
15. **文档纪律(用户定)**:每次验收通过后,同步更新 progress.md 及受影响的 PRD/README 等文档。

### 批次①补充(已验收,继续遵守)
16. **存储架构(v1.1.5~v1.1.9 终态)**:`config.storage.data_dir`(空=默认)+ `active_data_dir`(上次生效,迁移源)+ `pending_migrate/pending_delete_old`(弹窗选项,重启执行后复位);`platform::data_dir()` 是**唯一**数据路径入口,新增磁盘读写必须走它;`config.json`/`.tmp` **永远留在默认根**且删净时豁免。
17. **目录更改交互(用户定,通用模式)**:所有目录改位置 = 选目录 → 双弹窗(是否迁移/是否删除原目录)由用户决定;数据目录重启生效(WebView2/日志句柄占用,运行中搬会损坏),歌词目录立即执行(只碰 .lrc);「删除原目录」= 整个文件夹删掉。
18. **设置页交互约定**:目录路径显示为蓝色链接(点击资源管理器打开);「更改」按钮无省略号;列表行内按钮 × 在左、更改在右;「关于」页不再展示数据目录(与存储分区重复)。

### 批次③补充(已验收,继续遵守)
19. **背景体系(v1.1.11~v1.1.13 终态)**:模糊背景已**永久删除**,不许再引入;两态各自独立——常规 `idle_mode`/悬停 `background_mode`(Translucent/Transparent)+ `idle_alpha`/`hover_alpha`(百分比 1..=100,70%→0xB3 与历史值一致);悬停淡入淡出 = `mix_rgba(idle_bg→hover_bg)` 插值;`Theme::apply_bg_config` 是两态背景色的唯一写入口(采样 from_backdrop 与设置页就地同步共用)。
20. **绘制守卫教训**:painter 里任何「状态不满足就整帧跳过」的守卫,扩展新状态时必须重新审视——v1.1.12 的 idle_bg 就是被旧守卫 `hover_a > 0.004` 静默吞掉一整版。新增可见元素时先查 draw_frame 的分段守卫。
21. **改外观必须即时生效(用户预期)**:设置页 appearance.* / 右键菜单改背景 → `apply_bg_config` 就地同步 + Redraw,**不许**只依赖异步采样回填。

---

## 4. 未完成的待办

### 批次计划(2026-11-02 用户定:顺序由主理人决定,同类可合并,越拖越麻烦的优先;批次①闭环后缩减)
- **批次①**:[x] **3. 所有文件/缓存可自定义存储位置 + 自动迁移**——✅ 已验收(v1.1.5→v1.1.9,含弹窗自选迁移/删除、歌词目录改位置);**[x] 4. 「本地歌词目录」添加改浏览对话框**——✅ 已并入 v1.1.7 完成
- **批次②**:[x] **5. 设置页音量「调节步进」始终显示空白**——✅ 已验收(v1.1.10,含滚轮 10% 被钳连带修复)
- **批次③**:[x] **2. 悬停背景去模糊**——✅ 已验收(v1.1.11,模糊整链删除,后续 v1.1.12/v1.1.13 扩展两态+可调透明度)+ [x] **10. 紧凑排列下歌名有空间却被省略**——✅ 已验收(v1.1.11,ceil+换行检测)
- **批次④**:[x] **8. 右键菜单「显示设置」无反应**(接通 on_second_instance)+ [x] **11. 设置页永久置顶**(force_foreground 补 NOTOPMOST 撤销)+ [x] **12. 右键菜单被小组件遮住**(menu_open 期间暂停 ensure_on_top)——✅ 已验收(v1.1.14)
- **批次⑤(当前)**:[ ] **6. 音量范围「当前播放器」时滚轮仍改系统音量**(应改会话音量 ISimpleAudioVolume)+ [ ] **9. 右键菜单循环/随机播放不可用**(SMTC SetAutoRepeatMode/SetShuffle 写入链路;若播放器不支持需如实说明)+ [ ] **7. 单击封面激活播放器:汽水音乐/Edge(B站)失败,网易云成功**(launch.rs 激活链路)——媒体链路合并一批,用户分三场景一次验收
- 全部完成后:严过关全量回归(独立 worktree)+ 用户终验。
- 问题 1(歌词移入小组件)**✅ 已验收**(v1.1.0→v1.1.4,遗留 a/b 与验收反馈全部闭环)。

### 存量与可选(不阻塞)
- QA 无真机证据项(M2 时代,勿视为通过):播控真实改状态/拖动跳转/封面激活前台/滚轮音量 HUD(进程外读 SMTC 被安全策略阻)、多显示器/DPI/四方位/自动隐藏等。
- GitHub Release 发布(挂 deliverables zip)、LICENSE 版权人实名(现为 The Barline Authors 占位)、设置页标题栏明暗动态跟随、热键日志冗余清理。

---

## 5. 接手须知(新对话从这里开始)

1. **先 `git status` / `git log --oneline -5`** 确认干净与进度(本文档头部"最后更新"为准)。
2. 重建团队 `software-barline-fix`(TeamCreate)→ 按用户节奏派发:SendMessage 继续寇豆码(若同会话)或新 spawn `software-engineer`(prompt 必须自包含:工作目录、任务、约束 §3-6/7、版本 bump 四处、build.ps1 全链、单提交、汇报格式)。参考本文件 §4 清单与 docs/PRD.md。
3. 每项完成:主理人抽验 → 出包 `deliverables/曲栏_Barline_vX.Y.Z.exe`(**不删旧包**)→ 播报请用户验收 → **停下等回复**。
4. 工程师不启动 exe、不动 deliverables(主理人负责);用户自测用根目录 `启动曲栏.bat`。
5. 9 项全完成后:严过关独立 worktree 全量回归 + 终验报告。
6. 用户在用网易云/汽水音乐/Edge B站视频测试,验收反馈常附截图,现象描述以截图为准。
