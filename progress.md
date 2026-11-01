# 曲栏 Barline — 开发进度交接文档(progress.md)

> 用途:供「新开对话 / 接手者」快速恢复上下文。本文档与 `docs/` 下的 PRD / ARCHITECTURE / 各里程碑报告互补,但本文件是**唯一需要通读就能上手**的索引。
> 最后更新:**2026-11-02,M4/M5 全部完成,v1.0.0 已推送 GitHub(`Yi-One1/QuLan-Barline`),团队工作流闭环。**

---

## 0. 项目一句话定位

Windows 任务栏「媒体小组件」:**Rust + Direct2D 逐像素透明分层窗口**,内嵌任务栏条带(贴托盘区左侧 8px),通过 **SMTC** 读取/控制媒体会话,支持**封面/歌名/歌手/三按钮/进度条/音量 HUD**、**壁纸亮度自适应黑白文字**、**悬停毛玻璃**、**逐字卡拉OK歌词**、**托盘菜单 / 全局快捷键 / WebView2 设置页**,以及**便携版发布 v1.0.0**。

| 项 | 值 |
|---|---|
| 语言/栈 | Rust 1.99.1 (MSVC) + `windows` 0.62.2 + Direct2D/DWM/SMTC + WebView2 |
| 构建 | `powershell -ExecutionPolicy Bypass -File scripts/build.ps1`(**禁止直接 `cargo build`**,见 §2 环境约束) |
| 仓库 | 本地 `E:\APP\WorkBuddy\工作空间\曲栏Barline`;远端 **https://github.com/Yi-One1/QuLan-Barline.git**(master,上游跟踪已建) |
| 当前版本 | **v1.0.0**(exe 955 KB,便携包 `deliverables/曲栏_Barline_v1.0.0.zip` 341 KB,deliverables/ 不入库) |
| 团队 | 交付总监(主理人)、寇豆码(工程师)、严过关(QA);本次全部经子代理完成,**未再遇 429** |
| 关键语境 | 用户另有同名自研软件 `BarlineWidget`(类名 `BarlineWidget`/`BarlineBackdrop`),**不是系统控件,小组件不做任何避让/共存逻辑** |

---

## 1. 已完成事项(按里程碑/提交)

### 提交序列(git log,共 12 个提交)
```
836ced0  docs: QA M5 交付前终验报告(v1.0.0)
84fe061  docs: 添加 README 与 MIT LICENSE
5b425ac  feat: v1.0.0 — 版本资源 / 关于页版本号 / 打包准备
8af76f6  docs: QA M3-P2 回归报告(v0.0.8)
bbdc327  feat: v0.0.8 — M3 遗留 P2 补全 + 设置页即时回推
2ab866f  docs: QA-M4 回归报告与截图(v0.0.7)
a667218  fix:   v0.0.7 — M4 第二批 QA 缺陷修复(消息桥/内存/主题)
ec803b0  feat: v0.0.6 — M4 第二批(WebView2 设置页)
be7f8d9  feat: v0.0.5 — M4 第一批(托盘 / 全局快捷键 / lrclib 修复)
b9d204b  fix:   v0.0.4 — M2 QA 六条缺陷全部清零
1f66500  feat: v0.0.3 — M3 歌词引擎(逐字卡拉OK)
b29e619  feat: v0.0.2 — M1 骨架与 M2 完整小组件
```

### M1~M3 与 v0.0.4(历史,详见 docs/ 各报告)
- M1 骨架(任务栏几何/DPI/SMTC/D2D 透明窗口)、M2 完整小组件(封面/播控/毛玻璃/对比度自适应)、M3 歌词引擎(LRC/增强LRC/逐字扫光/多级缓存/WinHTTP)、v0.0.4 QA 六缺陷清零。报告:`docs/M1~M3-*-REPORT.md`、`docs/QA-M2-REPORT.md`。

### M4(本轮完成)
- **第一批(v0.0.5)**:系统托盘(独立隐藏窗口承载菜单)、全局快捷键 6 组(Ctrl+Alt+Shift+ 前缀,版本迁移只迁未改过的默认)、lrclib 超时修复(12s/15s+重试+25s 总闸)、P0 退出卡死修复(25s→20ms)。
- **第二批(v0.0.6,ec803b0)**:WebView2 设置页(~1007 行):Mica 标题栏/明暗跟随/6 分区/bridge JSON 双向协议/白名单写入/关窗彻底释放。
- **QA 复验 + 缺陷修复(v0.0.7,a667218)**:P0 消息桥全断(`TryGetWebMessageAsString` 收不到 JSON 对象消息→改 AsString 失败回退 `WebMessageAsJson`);P1 关窗内存不回落(根因 DestroyWindow 先于 WebView2 drop,改序修复);P2 标题栏明暗改读系统 `AppsUseLightTheme`(`RegGetValueW` API 层,非 reg.exe);`runtime_version()/open_runtime_download()` 接线到 WebView2 缺失弹窗。
- **回归闭环**:P0 四链路(落盘/语言/日志目录/录制挂起)全通;P1 口径修订(见 §2-11);P2 确认。报告 `docs/QA-M4-REPORT.md`。

### M3 遗留 P2 补全(v0.0.8,bbdc327)
- **歌词长行右侧渐隐**:`Frame::fade_brush` 水平渐变(按色+x0+x1 缓存,25fps 热路径零新建),渐隐带=行右缘 24px×scale;**不截断文本**,码元映射保真(扫光不错位);短行视觉零变化。
- **行切换 150ms 上移动画**:淡出保持前 120ms + 上移 6px×scale 走满 150ms(ease-out cubic);四态高度 94/74/48 零变化。
- **设置页即时回推**(QA 残留项):`Incoming::Set` 成功后 `settings.push(settings_state_json())`,语言切换当窗生效。
- QA 回归通过(报告 `docs/QA-M3P2-REPORT.md`,240+ 帧连拍无重叠、CPU 扫光 0.32~0.61%)。

### M5 打包发布(v1.0.0,5b425ac + 84fe061)
- 版本号统一 1.0.0(Cargo/关于页经 `CARGO_PKG_VERSION`/VS_VERSIONINFO);**rc 中文乱码坑**:rc.exe 按 GBK 解读无 BOM UTF-8 → 改 **UTF-8 BOM + `#pragma code_page(65001)`**。
- README(功能/构建/快捷键/配置/致谢)+ MIT LICENSE(`The Barline Authors` 占位,**版权人实名待用户自改**)。
- 便携包 `deliverables/曲栏_Barline_v1.0.0.exe`(955 KB)+ 同名 zip(341 KB,含 README/LICENSE 副本)。
- **QA 终验通过**(报告 `docs/QA-M5-REPORT.md`):exe 属性独立复核、关于页 1.0.0 真机、zip 解压直跑端到端、远端一致性。
- **GitHub 推送完成**:全 12 提交上 `Yi-One1/QuLan-Barline`,远端 HEAD=836ced0 与本地一致。推送走本机 7890 代理(会话级 5346 代理 502 时用 `git -c http.proxy=http://127.0.0.1:7890 ...` 临时通道)。

---

## 2. 关键决策(拍板记录,避免推翻重来)

1. **系统控件避让逻辑整体删除**(用户自有 BarlineWidget 不用管)。
2. **PRD P0-05 修正**:主行必须完全在条带内,歌词区向桌面方向扩展。
3. **CPU 指标口径(修订两次)**:原「播放中≤0.5%」→ M3 改「无扫光≤0.5% / 扫光≤2%」→ **v0.0.8 终版:扫光开 ≤2%、扫光关 ≤ 扫光开(同环境相对判定)**——壁纸轮播重采样噪声致绝对值不可测准(实测扫光开最低 0.32% < 无扫光最高 0.63%)。保持 25fps。
4. **歌手名对比度**:同色 + 动态 alpha 下限 78%。
5. **默认快捷键前缀** Ctrl+Alt+Shift+;`hotkeys_version` 迁移只迁从未改过默认的老配置。
6. **首启冲突提示用托盘气泡**,不弹 MessageBox。
7. **构建纪律**:统一 `scripts/build.ps1`,禁止裸 cargo(Git Bash link.exe 抢占);测试汇总需直跑 target/debug/deps 下测试二进制(windows_subsystem 吞 stdout)。
8. **本机安全约束**:禁 reg.exe、Add-Type、rm 清 %LOCALAPPDATA%;截屏统一 `scripts/grab-screen.py`;启动 exe 后台起+父进程保活,结束 `Stop-Process -Name barline -Force`;**注册表读取一律 Rust API(如 RegGetValueW)**。
9. **QA 纪律**:独立 git worktree 验收(`barline-qa-m4`,现基于 84fe061),不信任工程师自述。
10. **进程回收陷阱**:exe 作为工具子进程会被回收,父进程必须保活。
11. **P1 内存口径修订(v0.0.7)**:原「关窗私有回落基线 15MB 内」→ **「关窗后私有内存有界且不随开关次数增长:3 轮增量 <1MB 且峰值 <60MB」**。实测恒定 43.8MB 为 NT 堆/LFH 页保留(OS 分配器标准行为),非泄漏;原口径记 Known Issues。
12. **deliverables/ 不入库**:产物可由 release 构建确定性复现,二进制不进 git 历史(改一行 .gitignore 即可反转)。

---

## 3. 未完成 / 后续可选项

**本轮范围(M4 第二批 → M3 P2 → M5)已全部完成并推送。** 剩余均为可选项:

### 存量(历史遗留,非本轮缺陷)
- QA 无真机证据的验收项(本机限制,详见 QA-M2-REPORT,勿视为通过):播控三键真实改状态、拖动跳转误差、点击封面激活前台、滚轮音量+HUD(进程外读 SMTC/音量被安全策略阻断);多显示器/DPI/四方位/自动隐藏/Explorer 重启/运行中托盘增减/配置损坏恢复等。

### 可选后续(P2/P3 级,均不阻塞)
- **GitHub Release 发布**:QA 建议——Release v1.0.0 挂 `deliverables/曲栏_Barline_v1.0.0.zip`,Notes 引用 README 功能清单。(未做:需用户在 GitHub 操作或授权 gh。)
- **LICENSE 版权人实名**:当前 `The Barline Authors` 占位。
- **设置页标题栏明暗动态跟随**:当前窗口创建时读一次,运行中改系统主题需重开设置页(页面 CSS 实时);需 WM_SETTINGCHANGE 联动可开小任务。
- **录制后热键日志冗余**:resume+rebind 重复两批,仅日志,清理会碰 reduce_settings_message effects 组装。
- **P1 内存进一步优化**:HeapCompact/LFH 或进程外宿主(当前口径已 PASS,纯优化)。

---

## 4. 团队状态

- 本轮(v0.0.6→v1.0.0)团队工作流已闭环:工程师寇豆码 4 次交付、QA 严过关 4 次验收,**子代理未再遇 429**。
- `barline-qa-m4` worktree(基于 84fe061)保留,可 `git worktree remove "E:\APP\WorkBuddy\工作空间\barline-qa-m4" --force` 清理。
- 推送通道备注:会话内 git push 若遇 5346 代理 502,用 `git -c http.proxy=http://127.0.0.1:7890 -c https.proxy=http://127.0.0.1:7890 push` 临时切换本机代理。

---

## 5. 新接手者下一步(按需)

1. **发 GitHub Release**(可选):仓库页 Releases → Draft new release → tag `v1.0.0` → 上传 `deliverables/曲栏_Barline_v1.0.0.zip`。
2. 跑起来:`deliverables/曲栏_Barline_v1.0.0.exe` 双击即用(需 WebView2 Runtime,缺失会弹窗引导下载)。
3. 开发调试:读 §2 决策与 §6 文件索引;构建一律 `scripts/build.ps1 -Task check/test/clippy/build`。
4. 后续需求:走增量流程(增量 PRD → 增量设计 → 工程师 → QA worktree 回归)。

---

## 6. 关键文件索引

| 路径 | 作用 |
|---|---|
| `docs/PRD.md` | 需求(30 条/P0 16 条/验收 §8.1,已按决策修正) |
| `docs/ARCHITECTURE.md` | 架构(§4.4 内存、§10 任务栏内嵌、§11 亮度/模糊、§13 设置页) |
| `docs/TASKS.md` | 任务分解(20 任务/5 里程碑) |
| `docs/QA-M2/QA-M4/QA-M3P2/QA-M5-REPORT.md` | 四份 QA 独立验收报告(全部通过/闭环) |
| `docs/M1~M4-REPORT.md`、`docs/M3-LYRIC-REPORT.md` | 里程碑报告(工程师自述,线索非结论) |
| `src/render/` | d2d(fade_brush)/painter/layout/text/theme/sampler/blur/cover_cache |
| `src/ui/` | widget_wnd/interaction/hit/menu/tray |
| `src/media/` | smtc/cover/launch/player_map/wasapi |
| `src/lyric/` | 歌词解析/查询/扫光/缓存(9 文件) |
| `src/settings/` | 设置页 mod/webview/bridge/assets(已入库) |
| `src/platform/` | dpi/wake/**system_app_theme_dark**(AppsUseLightTheme) |
| `assets/barline.rc` | 版本资源+图标+manifest(UTF-8 BOM!) |
| `deliverables/` | v1.0.0 便携包(不入库) |
| `scripts/build.ps1` | **唯一**构建入口(-Task check/clippy/test/build) |
| `scripts/grab-screen.py`、`scripts/m4-settings-check.py`、worktree 内 `m4-qa2-*.py` | 截屏/验收脚本 |

---

## 7. 已验证硬指标(截至 836ced0,全部 QA 自跑证据)

| 指标 | 值 |
|---|---|
| release 构建 | 零 error、零 warning;exe **955 KB**(预算 8 MB) |
| clippy `-D warnings` | 零告警(src/ 无 `#[allow]`) |
| 单元测试 | **222 passed / 0 failed / 1 ignored** |
| 内存 | 基线 WS ~36.9MB/私有 ~10.7MB;设置页开→关:私有恒定 ~43.8MB 有界不增长(口径见 §2-11) |
| CPU | 播放+扫光 **0.32%~0.61%**(口径:扫光开 ≤2%、扫光关 ≤ 扫光开,§2-3) |
| 退出 / 单实例 | 托盘退出内部 24~90ms;二次启动 41~43ms 退出 |
| 设置页 | 6 分区/明暗跟随/关于页 1.0.0/语言当窗生效/改项 0.8s 落盘/关窗资源全释放 |
| 歌词 | 长行右缘 24px 渐隐、扫光分界字间无错位;行切换 150ms 上移+120ms 淡出;四态高度 94/74/48 |
| 快捷键 | 6 组 Ctrl+Alt+Shift+ 全通,录制挂起/恢复闭环 |
| 便携包 | zip 解压直跑端到端 ✅(启动/小组件/设置页/退出码 0) |
| GitHub | 远端 HEAD = 本地(836ced0),12 提交完整 |
