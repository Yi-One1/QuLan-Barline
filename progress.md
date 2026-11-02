# 曲栏 Barline — 开发进度交接文档(progress.md)

> 用途:供「新开对话 / 接手者」快速恢复上下文。本文档与 `docs/` 下的 PRD / ARCHITECTURE / 各 QA 报告互补,本文件是**唯一需要通读就能上手**的索引。
> 最后更新:**2026-11-02 22:33,G3(删界面缩放)已验收 → v1.2.2;下一组 G4 快捷键体系。**

---

## 0. 项目一句话定位

Windows 任务栏「媒体小组件」:**Rust + Direct2D 逐像素透明分层窗口**,内嵌任务栏条带(贴托盘区左侧 8px),通过 **SMTC** 读取/控制媒体会话,支持**封面/播控三键/进度条/音量 HUD/逐字卡拉OK歌词/壁纸自适应对比度/托盘菜单/全局快捷键/WebView2 设置页**。

| 项 | 值 |
|---|---|
| 语言/栈 | Rust 1.99.1 (MSVC) + `windows` 0.62.2 + Direct2D/DWM/SMTC + WebView2 |
| 构建 | `powershell -ExecutionPolicy Bypass -File scripts/build.ps1 -Task check/clippy/test/build`(**禁止直接 `cargo build`**,见 §3 约束) |
| 仓库 | 本地 `E:\APP\WorkBuddy\工作空间\曲栏Barline`;远端 **https://github.com/Yi-One1/QuLan-Barline.git**(main,上游跟踪已建;push 若遇会话代理 502,用 `git -c http.proxy=http://127.0.0.1:7890 -c https.proxy=http://127.0.0.1:7890 push` 走本机 7890 代理) |
| 当前版本 | **v1.2.2**;交付包在 `deliverables/`,**只增不删** |
| 版本号策略(用户定 2026-11-02) | 改可见行为/新增能力 → 升第二位(1.2.0);纯修正/文案/内部重构 → 升第三位 |
| 快捷启动 | 根目录 **`启动曲栏.bat`**(增量构建+运行未打包 exe,用户自检最新改动用) |
| 团队 | 交付总监(主理人)+ 寇豆码(software-engineer);团队 `software-barline-12`(第三轮 12 项),后续每组按需 spawn |
| 推进节奏(用户定) | **一次只做一项 → 带有效测试 → 提交 → 用户验收 → 下一项**;全部完成后跑全量测试 |
| 版本策略(用户定) | 每完成一项 bump 补丁号并出包 `deliverables/曲栏_Barline_vX.Y.Z.exe`(**历史包永久保留,绝不删除**) |

---

## 1. 提交序列(git log,oneline)

```
5eb82a5 refactor: v1.2.2 — 删除死配置「界面缩放」(批次G3 问题8),旧配置兼容有测试兜底
b8e6cbc feat: v1.2.1 — 设置窗口最小化还原(SW_RESTORE)+右键菜单背景项扩为4项(批次G2 问题4/6)
ca6e151 fix: v1.2.0 — 修自适应过慢(删 EMA 改确定性三层+快速通道+反向冷却闸),换壁纸 1s 翻
4f677f8 feat: v1.2.0 — 渲染稳定性三项(批次G5 问题1/3/2):过渡色偏/黑白闪烁+描边删除/截图冻结
eade623 feat: v1.1.22 — 设置项文案与次序修正+歌词关闭整组置灰(批次G1 问题5/9/11/12);顺修关于对话框版本号写死 v0.0.1
21b1e42  chore: v1.1.21 — 用户裁决搁置汽水激活(托盘点击抢鼠标,代码留档待办D);托盘态不激活
bddc6b6  fix: v1.1.20 — 汽水改托盘图标点击通道(Shell_NotifyIconGetRect+SendInput,后经裁决停用)
ed8d2b7  fix: v1.1.19 — 汽水激活轮换策略+每步验证前台真转移(ALT键解锁前台锁)
70c78b6  fix: v1.1.18 — 汽水托盘恢复改SW_RESTORE+EnableWindow检查+SwitchToThisWindow兜底
bf73e26  fix: v1.1.17 — 汽水AUMID中文章节+会话音量暂禁用+日志路径bug
ce6906b  fix: v1.1.16 — 会话音量GUID/匹配修复+封面激活收编隐藏窗口删网页回退+删循环随机
cfef1b1  feat: v1.1.15 — 会话音量/封面激活多进程/循环随机结论(批次⑤ 问题6/7/9)
a597cb0  docs: progress.md — 批次④(问题8/11/12)闭环 v1.1.14;批次⑤(最后一批)成为当前
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

### 第三轮:12 项问题(2026-11-02 21:08 用户提出,分 6 组推进)

分组与顺序(用户 21:14 拍板):`G1 文案次序 → G5 渲染稳定性 → G2 窗口菜单 → G3 界面缩放 → G4 快捷键 → G6 歌词源`。每组=全绿测试+单 commit+单补丁包+用户真人验收,验收通过才更新文档(本段即 G1 验收产物)。

**G1:问题 5/9/11/12(✅ 已验收 2026-11-02,v1.1.22 eade623)**
- 问题5:右键菜单「显示设置」→「设置」(小组件菜单 `menu.rs` 与托盘菜单 `tray.rs` 共用 `i18n.rs` 的 `menu.settings`,改一处两处生效,已 grep 确认无第二处字面量)。
- 问题9:「显示歌词」描述原写「关闭后小组件收合到任务栏高度」**与实际不符**(高度恒为条带厚度,关闭只是不画歌词行)→ 改为「不再显示歌词行,只保留歌名、歌手与播放控制」;关闭时**逐字扫光/长歌词滚动/在线歌词 + 本地歌词目录整块**置灰(`.dimmed` opacity .45 + `disabled`),主开关 `#lyrics-enabled` 保持可点。实现:`syncLyricsDeps()` 幂等函数,`render()` 与主开关 `change` 双触发。
- 问题11:「开机自启」→「开机自启动」,含**用户未提及的**通用页副标题「开机自启、语言与显示开关」(由残留检查测试抓出)。
- 问题12:悬停背景下拉改「透明」在上(与常规背景一致;value 读写不依赖索引)。
- **顺手修既有缺陷**:`i18n.rs` 的 `about.text` 版本号写死 `v0.0.1`(右键「关于」MessageBoxW 直接弹它,而设置页用 `env!("CARGO_PKG_VERSION")`,两处打架)→ 改 `concat!` + `env!`;歌词来源文案同步为「lrclib.net、网易云音乐」(为 G6 铺路)。
- 新增 6 个契约回归测试(菜单文案/下拉次序/开机自启无残留/歌词描述/关于版本号/双来源),合计 256 项通过、clippy 0 警告、release 包 1001 KB。

**G5:问题 1/3/2 渲染稳定性(✅ 已验收 2026-11-02,v1.2.0 两提交 4f677f8 + ca6e151)**
- 问题1(过渡先灰后白):`apply_bg_config` 透明档原写 `Rgba::new(0,0,0,0)` 把 RGB 谎报成黑色,`mix_rgba` 四通道插值 → 浅色壁纸下淡入中点是**半透明中灰**。改为透明档保留 on_light 基色、只把 alpha 置 0;`mix_rgba` 未动,淡入 120ms/淡出 200ms 保留。
- 问题3(黑白闪烁+异常描边):新增 `src/render/stable.rs` **确定性三层状态机**——双阈值死区(0.12/0.25,中间保持现状)+ 连续 2 次确认 + **大幅跳变快速通道**(越过对侧阈值 ≥0.05 即 `LUMA_FAST_MARGIN`,当帧翻)+ **反向冷却闸**(`LUMA_FAST_COOLDOWN_SAMPLES=2`,快速翻转后 2 帧内反向降级为慢路径,防视频壁纸镜头闪切 1Hz 频闪)。**描边整链删除**(`needs_outline`/`Frame::outline`/`TextDraw::outline`/四次偏移重画,grep 归零;中灰带 4.36:1 可接受)。
- 问题2(截图干扰):新增 `src/platform/capture.rs` 截图工具探测(进程白名单 snippingtool/screenclippinghost/screensketch/pixpin/snipaste/sharex/fscapture + 类名子串,纯函数可单测);遮罩期间**只冻结亮度采样与几何重算**,播放内容/歌词/封面照常,退出立即补采一次。用户实测:字色不再变白、位置不再左移(经复测确认)。
- 诊断设施(长期保留):`widget rect 变化` info 日志带 `reason` 枚举(TaskbarGeometry/EnsureOnTop/Resize/DpiChange/Unknown)、`ensure_on_top` 对实际位置与 target 对账、几何 `diff()` 输出变化字段名。
- **关键教训**:`AppState::geometry_epoch` 每次 `geometry::probe()` 都 +1(1s 心跳每秒 probe)→ 它实际是"探测序号"不是"几何代次",任何拿它当稳定标识的逻辑都会被每秒重置。稳定器改用采样矩形指纹 `area_epoch`。
- 测试 282 项(274→282),clippy 0 警告,release 1006 KB。

**G2:问题 4/6 窗口与菜单(✅ 已验收 2026-11-02,v1.2.1 b8e6cbc)**
- 问题4(最小化后「设置」唤不起):`SettingsService::open` 的「已存在→只前置」分支原本只调 `SW_SHOW`,而 **`SW_SHOW` 对最小化窗口是 no-op** → 窗口躺在任务栏里纹丝不动。改为 `IsIconic` 判定后走 `SW_RESTORE`;判定抽成纯函数 `reveal_action(iconic)`(唯一判定点,2 条单测),小组件菜单与托盘菜单两个入口同时修好。
  连带修复:最小化时 `WM_SIZE` 派发 0×0 → `resize()` 原样把 `set_bounds(0,0,0,0)` 灌给 WebView2;加 `w<=0||h<=0` 守卫。
- 问题6(右键菜单背景项):从 2 项扩为 **4 项**——`常规背景：透明/半透明`、`悬停背景：透明/半透明`,两组各自独立勾选(常规看 `idle_mode`、悬停看 `background_mode`),组内顺序统一「透明→半透明」。`MenuCommand` 重构为 4 个背景变体 + `bg_target()` 纯函数映射(有单测);`app.rs` 两段重复代码收敛为 `set_background(slot, mode)`(内部 `apply_bg_config` 就地同步 → 当帧重绘,遵守决策 21)。i18n 新增 4 键、删旧 2 键(已 grep 确认无其它引用)。托盘菜单故意不跟着扩。
- 测试 286 项,clippy 0 警告,release 1006 KB。

**G3:问题 8 删除「界面缩放」(✅ 已验收 2026-11-02,v1.2.2 5eb82a5)**
- `ui_scale` 是**死配置**(只在 config/bridge/index.html 出现,渲染与布局层从未读取);且高度锁死 48px,放大必溢出、缩小只让字更小,故**整项删除**,不改成字号档位。
- 删除点:config 字段与默认值/启动日志占位符、bridge 白名单分支、index.html 整行(含中英词条 + render + bind,共 -12 行)。
- **三条回归保障**:①旧配置兼容测试(喂含 `"ui_scale":1.25` 的旧 JSON,断言不报错且同级字段照常解析——用户现存 config.json 就有这个键);②白名单拒绝测试(旧页面再发也写不进);③HTML 契约测试(5 个 needle 不存在 + 反向断言外观块其它控件未删坏)。
- 文档:PRD 设置项表格删该行;ARCHITECTURE 字段定义与桥接示例改用 `hover_alpha`。`docs/TASKS.md:862` 的 m4 验收项属历史记录,已作废但不回改。
- 测试 288 项,clippy 0 警告,release 1003 KB。

### 交付包现状(deliverables/,只增不删)
`曲栏_Barline_v1.0.0.exe`、`.zip`、`v1.1.0.exe` ~ `v1.1.14.exe` 全序列。
> 注:worktree 检出 `.ps1` 会触发 git working-tree-encoding 转码失败;**重建历史版本包**用「主仓库 detached checkout <hash> → build.ps1 build → 拷 exe → checkout main」方式(已验证可行)。

---

### 本轮收尾整理(2026-11-02 09:14,本对话)
- **GitHub 仓库默认分支 master→main 迁移(完成)**:本地 `git branch -m master main` → 推 origin main → GitHub 网页 rename master→main(自动设为默认) → `git fetch -p` 清理 origin/master + `git remote set-head origin -a` 对齐;远端现仅 `main` 且为默认,本地 `main` 跟踪 `origin/main`,工作区干净。迁移零数据损失(全部 27 提交保留在 main)。
- **文档分支名同步(完成)**:`progress.md` / `docs/QA-M5-REPORT.md` 中硬编码 `master` 改为 `main`(歌名 `Remastered` 不受影响);`.github/` 目录不存在,无 CI 工作流需改。
- **项目文件整理(完成)**:删除 `ui/` 空目录(无任何文件)、`examples/diag_soda.rs`(v1.1.19/1.1.20 汽水激活排查临时诊断工具,待办 D 已注明可删;汽水激活搁置后无复用价值)。

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

### 第三轮决策(2026-11-02 逐条确认,后续各组必须遵守)

22. **版本号策略**:改可见行为/新增能力 → 升第二位;纯修正/文案/内部重构 → 升第三位。预期:G5=1.2.0、G2=1.2.1、G3=1.2.2、G4=1.2.3、G6=1.3.0。
23. **过渡色偏(#1 根因已定)**:`mix_rgba` 四通道一起插值,而「透明」态背景谎报 `(0,0,0,0)` → 浅色壁纸下过渡中点得到半透明**中灰**。修法:`idle_bg`/`hover_bg` 在透明态**必须携带当前 on_light 基色**(仅 alpha=0),`mix_rgba` 不动;**淡入 120ms/淡出 200ms 保留**(用户确认)。
24. **黑白判定稳定化(#3)**:现状是 1s 重采样 + 交点 0.179 单点硬切、无迟滞。**终态(已实现)**:① 双阈值迟滞:luma ≥ **0.25** 才判浅色背景(黑字)、≤ **0.12** 才判深色背景(白字),0.12~0.25 死区保持现状;② 同向判定**连续 2 次**才真翻转,反向清零;③ **大幅跳变快速通道**:越过对侧阈值 ≥0.05 当帧翻;④ **反向冷却闸**:快速翻转后 2 帧内反向降级慢路径。
    ⚠️ **中途走过的弯路(不得重犯)**:最初设计里还有一层「EMA 平滑 α=0.25」,用户实测白→黑壁纸要 **7 秒**、黑→中灰要 8 秒——因为 EMA 与死区+二次确认**在做同一件事(抗抖)**,三者叠加是延迟相乘,且 EMA 渐近逼近永远差一点到阈值。**EMA 已整链删除**,时延变成可数:换壁纸 1 帧 / 阈值附近 2 帧 / 死区永不翻 / 镜头逐秒横跳 3 帧一次。教训:**抗抖只需要「死区 + 连续确认」两件,不要叠加第三个平滑器**。
25. **描边整链删除(#3 另一半)**:原 `needs_outline` 在 luma 0.174~0.189 触发(该带最高对比度仅 **4.36:1**,因文字色是 `#0A0A0A`/`#FAFAFA` 而非纯黑白),画的是**反色**描边(白字黑边/黑字白边)——用户判定为视觉噪音。**整链删除**(`needs_outline` 字段、`Frame::outline`、`TextDraw::outline`、`draw_line` 四次偏移重画、sampler 日志、`state` 里 1911 行的引用),中灰带 4.36:1 可接受。**不许**改用阴影或底色替代。
26. **截图遮罩行为(#2,方案 B)**:遮罩期间**冻结亮度采样与几何重算**(防字变白、防位移),**播放内容(封面/进度/歌词)照常更新**。位移根因另查后单独修,不许用冻结掩盖。截图工具为 PixPin / `Win+Shift+S` 都能复现;实测证据:遮罩期间小组件**仍可点击**、与托盘间距变大(左移)、1–2s 后才恢复 → 与 300ms 采样节流 + 1s 重采样节奏吻合。
27. **歌词源(G6,方案 B)**:lrclib **精确接口未中 → 走 `/api/search` 模糊择优**(标题/歌手相似度+时长差)、标题清洗(去 `(Live)/(Remastered)/feat.` 、全角归一)、多歌手拆分;全链失败后以**网易云**兜底(公开接口,已实测可用并返回带时间轴 LRC)。单开关统管双源、失败静默、**旧负缓存全部作废**(否则 24h 内继续显示暂无歌词)。不加 QQ 音乐(边际收益低、维护面翻倍)。
28. **右键菜单背景项(#6)**:平铺 4 项(**不用子菜单**),两组内部顺序统一为**透明 → 半透明**:`常规背景：透明/半透明`、`悬停背景：透明/半透明`。
29. **界面缩放(#8)**:`ui_scale` 是死配置(只在 config/bridge/HTML 出现,渲染层从未读)→ **整项删除**(字段保留反序列化兼容),不做"往小缩"也不改成字号档位。
30. **快捷键(G4)**:默认改回 `Ctrl+Alt+P/L/M/H` + `Ctrl+Alt+←/→`(**历史反证**:M2 时代实测本机这 5 组曾被占用 1409,故当年才改 `Ctrl+Alt+Shift+*`;用户知情后仍选择改回);冲突行标红 + 一键换组;迁移条件=当前 6 组**全等于**上一版默认值才自动迁移;录制按钮改名**「更改」**;每行右侧加 **`↺` 图标按钮**(tooltip「恢复默认」,用户否掉了文字按钮方案);**不许**自动降级偷偷加 Shift。
31. **歌词置灰范围(#9)**:方案 B——三个子开关 + 本地歌词目录区一起灰,主开关保留可点。

---

## 4. 未完成的待办

### 第三轮 12 项(6 组,2026-11-02 起,当前工作)

| 组 | 问题编号 | 内容 | 状态 |
|---|---|---|---|
| G1 | 5 / 9 / 11 / 12 | 文案与控件次序 | ✅ 已验收 v1.1.22 |
| G5 | 1 / 3 / 2 | 渲染稳定性(过渡色偏、黑白闪烁+描边、截图干扰) | ✅ 已验收 v1.2.0 |
| G2 | 4 / 6 | 设置窗口最小化唤不起 + 右键菜单背景四项 | ✅ 已验收 v1.2.1 |
| G3 | 8 | 删除「界面缩放」 | ✅ 已验收 v1.2.2 |
| G4 | 10 | 快捷键体系(默认值/更改/逐行恢复) | ⬜ 进行中(下一组) |
| G6 | 7 | 歌词源扩充(模糊检索+网易云) | ⬜ |

> 全部完成后:严过关独立 worktree 全量回归 + 用户终验。

### 第二轮批次计划(已全部闭环,留档)
- **批次①**:[x] **3. 所有文件/缓存可自定义存储位置 + 自动迁移**——✅ 已验收(v1.1.5→v1.1.9,含弹窗自选迁移/删除、歌词目录改位置);**[x] 4. 「本地歌词目录」添加改浏览对话框**——✅ 已并入 v1.1.7 完成
- **批次②**:[x] **5. 设置页音量「调节步进」始终显示空白**——✅ 已验收(v1.1.10,含滚轮 10% 被钳连带修复)
- **批次③**:[x] **2. 悬停背景去模糊**——✅ 已验收(v1.1.11,模糊整链删除,后续 v1.1.12/v1.1.13 扩展两态+可调透明度)+ [x] **10. 紧凑排列下歌名有空间却被省略**——✅ 已验收(v1.1.11,ceil+换行检测)
- **批次④**:[x] **8. 右键菜单「显示设置」无反应**(接通 on_second_instance)+ [x] **11. 设置页永久置顶**(force_foreground 补 NOTOPMOST 撤销)+ [x] **12. 右键菜单被小组件遮住**(menu_open 期间暂停 ensure_on_top)——✅ 已验收(v1.1.14)
- **批次⑤(✅ 收尾闭环 2026-11-02)**:[x] **6. 会话音量**——v1.1.15/v1.1.16 两轮修复后用户裁决**暂时禁用**(留档待办 A);[x] **9. 循环/随机播放**——用户裁决**整体删除**(菜单+SMTC 写入链+字段,已验收);[x] **7. 封面激活**——网易云/Edge 已修好(v1.1.15~v1.1.16);汽水音乐经 v1.1.17~v1.1.20 五轮攻坚(实测根因:CEF/Electron 托盘恢复型应用任何外部 ShowWindow 都致输入挂起,托盘点击通道有效但抢鼠标),用户裁决**搁置**(留档待办 D,托盘态不激活不制造卡死窗口)
- 全部完成后:严过关全量回归(独立 worktree)+ 用户终验——**2026-11-02 收尾回归绿**:clippy 0 警告、249 测试通过(1 ignored 为联网用例)、release 构建出包 v1.1.21。
- 问题 1(歌词移入小组件)**✅ 已验收**(v1.1.0→v1.1.4,遗留 a/b 与验收反馈全部闭环)。

### 待办登记(2026-11-02 批次⑤验收反馈,用户裁决)

**A. 会话音量(问题 6)——用户裁决:暂时禁用,以后有需要再改。登记如下供将来续修:**
- 现象:v1.1.15/v1.1.16 两版在网易云/汽水/Edge(B站)上滚轮均无效。v1.1.16 运行日志(注意:日志实际写在默认根 `%LOCALAPPDATA%\Barline`,见 C 项 bug)显示**连网易云(process="cloudmusic.exe", 正在播放)也报「没有匹配的音频会话」**——失败发生在**音频会话枚举/匹配环节**,比 GUID 解析更靠前。
- 已排除:会话 GUID 来源(已改 GetSessionIdentifier 尾段)、循环内 `?` 提前返回(已改 continue)、汽水映射(SodaMusic.exe 已入表)。
- 未排查嫌疑:① `IMMDevice::Activate` 拿到的 `IAudioSessionManager2` 会话枚举是否为空(GetCount/GetSession 全失败被静默吞掉,枚举环节**无日志**);② `process_image_name`(QueryFullProcessImageNameW)对会话 pid 是否失败返回空串;③ 设备归属(网易云/Edge 是否走非默认 endpoint);④ CLSCTX/COM apartment 影响。
- 续修入口:src/media/wasapi.rs `session_simple_volume`(枚举处补逐会话 log_info 诊断);src/app.rs on_wheel 的 CurrentPlayer 分支(实现完整保留);禁用点 = Config::load 归一化 + bridge "volume.scope" 强制 System + 设置页 select disabled。
- 禁用落地(v1.1.17):设置页「调节范围」置灰固定系统主音量;旧配置 current_player 加载时归一化。

**B. 汽水激活(问题 7)——终态:网易云/Edge 已修好,汽水部分用户裁决搁置(见 D)**:汽水 SMTC AUMID 实测就是中文名「汽水音乐」(日志 aumid=汽水音乐、process 为空),player_map 补中文键 → process_name=sodamusic.exe。

**C. 日志路径 bug——v1.1.17 已修**:logger 启动时永远在默认根打开日志,而 relocate 只在「发生迁移」时执行 → active_data_dir 已是自定义目录后,日志**永远留在默认根** `%LOCALAPPDATA%\Barline\barline.log`(数据目录里看不到),排查极坑。修复:init_from_config 末尾无条件 relocate(内部同路径幂等跳过)。

**D. 汽水封面激活卡死(问题 7 余项)——用户裁决 2026-11-02:搁置,以后再解决。登记如下供将来续修:**
- 实测根因(诊断工具 `examples/diag_soda.rs`,五模式 enum/activate/click/fix/tray/fg):汽水(Electron/CEF)Hide 到托盘后,**任何外部 `ShowWindow`(SW_SHOW/SW_RESTORE)无论配什么前台技巧(SFW/ALT 解锁/AttachThreadInput/SwitchToThisWindow)都会让 CEF 输入管线挂起**——窗口显示、甚至前台与 GUITHREADINFO 激活态全部正常,但内容完全不可点击(窗口冻结帧,截图证据:× 按钮冻结在按下态)。诊断关键:**验证「卡死」必须用行为验证(点 × 能否隐藏窗口),前台/激活状态查询全是假阳性**。
- 有效但被否的通道:v1.1.20 托盘图标点击(`Shell_NotifyIconGetRect` 按 hWnd+uID 精确定位该进程注册的图标 + `SendInput` 真实左键,恢复+行为验证全通)——**SendInput 会抢鼠标**(光标拉到托盘再恢复),用户裁决不启用。
- 已排除:AUMID `ActivateApplication` 全候选 E_INVALIDARG;`ShellExecuteW`(开始菜单 lnk)触发汽水**重启接管**(旧实例退出中断播放)。
- 现行行为(v1.1.21):汽水窗口**可见**时正常前台激活;**托盘隐藏态不做任何激活**(日志「激活暂禁用(缺陷搁置,待办 D)」),不制造卡死窗口、不抢鼠标。
- 续修入口:src/media/launch.rs `tray_click_activate`(`#[allow(dead_code)]` 留档,注释含续修方向)、`TRAY_RECOVERY_APPS`/`is_tray_recovery_app`、activate_player 开头 tray_recovery 分支。(注:`examples/diag_soda.rs` 诊断工具已于 2026-11-02 整理时删除,续修如需复现需在 examples/ 下重建等效诊断程序。)
- 续修方向:①逆向 Electron NotifyIconHost 托盘回调消息号直接 `PostMessage`(零鼠标移动);②探测汽水 deep-link/second-instance 无重启唤起;③仅用户光标静止时模拟点击+光标恢复动画。

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
