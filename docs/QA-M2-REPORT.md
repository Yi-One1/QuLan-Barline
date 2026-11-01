# 曲栏 Barline — M1+M2 独立 QA 验收报告

> 验收人：严过关（QA） · 验收对象：commit `b29e619`（独立工作树 `barline-qa-m2` 检出）
> 环境：Windows 11 2560×1440 @ 96dpi，任务栏底部 48px，Rust 1.99.1 MSVC，windows 0.62.2
> 方法：**全部自跑自证**，不采信工程师自述；工程师的 `scripts/m2-perf.py` 实际跑不起来
> （`import m2_accept.py` 但仓库里只有 `m2-accept.py`），故另写独立脚本 `qa/qa_m2.py`、
> `qa/qa_transparency.py`、`qa/qa_stability.py`。

---

## 0. 结论摘要

**有条件通过（Conditional Pass）。**

功能层面基本成立：从零 release 构建零 error、125 个单测全绿、小组件能真实识别系统 SMTC
会话并渲染、静止态背景视觉透明、悬停模糊真实生效、深色背景下文字对比度 6.90:1、
单实例 0.09s 退出、5 分钟常驻无泄漏、内存/CPU 均在预算内。

**但静态质量硬门槛未通过**：`cargo clippy -- -D warnings` 报 **11 个 error**（`#![deny(warnings)]`
下即编译失败），与工程师"M2 零 warning、M1 从未跑过 clippy"的自述共同构成一个**未闭合的
质量门**。此外有 1 处 PRD 自相矛盾（P0-05 vs 歌词形态高度）、内存已到预算 97%、以及若干
交互真实性项因环境限制**未取得证据**（见 §4）。上述未闭合项不影响 M2 演示，但在 M3 开工前
应清零 clippy、确认内存余量。

---

## 1. A —— 构建与静态质量（硬门槛）

| # | 项目 | 方法 | 证据 | 结论 |
|---|------|------|------|------|
| A1 | release 构建 | `scripts\build.ps1 -Task build`（独立工作树、从零） | `Finished release profile in 1m34s`，**0 error、0 rustc warning**；`barline.exe = 747,520 B (730 KB)` | ✅ 通过（≤8 MB） |
| A2 | 单元测试 | `scripts\build.ps1 -Task test` | `test result: ok. 125 passed; 0 failed; 0 ignored`；全仓 `#[ignore]` 计数 = **0** | ✅ 通过（无被跳过用例） |
| A3 | **clippy `-D warnings`** | `scripts\build.ps1 -Task clippy`（组件缺失，已 `rustup component add clippy`） | **11 个 error，编译失败**（明细见 §3 缺陷 P1-01） | ❌ **不通过** |
| A4 | 资源占用 | `qa/qa_m2.py` 65s 采样 + `qa/qa_stability.py` 300s 采样（`GetProcessMemoryInfo`/`GetProcessTimes` 差分） | 65s：CPU **0.385%**、WS 36.9→37.1 MB、Private 11.2→11.4 MB；300s：CPU **0.245%**、WS 38.45→38.74 MB、Private 11.66→11.45 MB | ✅ 通过（但见 P2-04：WS 已到 40MB 的 97%） |

> 关于 A1 的"零 warning"：rustc 层面确为零；但 `#![deny(warnings)]` 并不包含 clippy 的额外
> lint，因此**clippy 的 11 个 error 在 `cargo build` 中不显现**。工程师"M2 零 warning"的说法
> 在 rustc 语义下成立，在项目质量门（`deny(warnings)` 的本意 + M1 自述"没跑过 clippy"）下不成立。

**clippy 11 error 明细：**

| lint | 位置 | 说明 |
|------|------|------|
| `too_many_arguments` (8/7) | `render/painter.rs:177` `draw_cover` | |
| `too_many_arguments` (8/7) | `render/painter.rs:244` `draw_texts` | |
| `too_many_arguments` (8/7) | `render/painter.rs:295` `draw_line` | |
| `too_many_arguments` (9/7) | `render/painter.rs:538` `draw_pill` | |
| `too_many_arguments` (8/7) | `render/sampler.rs:327` `compute_theme` | |
| `manual_div_ceil` | `render/sampler.rs:61` | `(w+bs-1)/bs` → `w.div_ceil(bs)` |
| `manual_div_ceil` | `render/sampler.rs:62` | 同上 |
| `manual_div_ceil` | `render/sampler.rs:252` | 同上 |
| `manual_div_ceil` | `render/sampler.rs:253` | 同上 |
| `ptr_arg` | `log.rs:122` `with_index(&PathBuf)` | 应 `&Path` |
| `let_unit_value` | `render/d2d.rs:116` | `let _ = rt.SetTextAntialiasMode(..)` 多余绑定 |

分类：`too_many_arguments` ×5、`manual_div_ceil` ×4、`ptr_arg` ×1、`let_unit_value` ×1。均易修。

---

## 2. B —— 功能真实性

| # | 项目 | 方法 | 证据 | 结论 |
|---|------|------|------|------|
| B1 | 静止态背景透明 | `qa/qa_transparency.py`：有 widget / 无 widget 同区域像素比对，并测"无 app 两帧噪声地板"作为对照 | 空背景子区域：有/无 widget 平均差 **5.19/255**（最大 49）；同一区域**无 app** 时相邻 0.5s 噪声平均差 **7.45/255**（最大 46）→ **信号 ≤ 噪声**。视觉见 `qa/shots/t-idle1.bmp`（壁纸直接透出，无面板） | ✅ 通过（信号不高于背景自身噪声，证明该区域未绘制背景）。**注**：本机该处桌面是动态的，绝对 A/B 不可靠，故用信号/噪声对照法 |
| B2 | 亮度自适应（深色背景） | 单帧内测文字像素 vs 背景像素，WCAG 相对亮度公式 | 文字 RGB(250,250,250) vs 背景 RGB(74,93,56) → **6.90:1 ≥ 4.5** | ✅ 通过（深色场景实测） |
| B2' | 亮度自适应（浅色背景） | 本机壁纸被策略锁死、任务栏恒透壁纸，无法切纯白/纯黑背景 | 代码 + 单测：`from_backdrop` 按对比度择优、`light_backdrop_gets_dark_text`、`black_white_contrast_is_21`、`mid_gray_picks_the_readable_side` | ⚠️ 仅代码/单测级证据，**未取得真实浅色背景实测** |
| B3 | 悬停模糊 | `qa_transparency.py` + 分步移动复测：比较 idle/hover 同一空背景子区域的梯度（高频能量）与方差 | 梯度 idle **4.40** → hover **0.53（降 88%）**，方差仍维持 200+（不是纯色块）；视觉 `qa/shots/h2-hover.bmp` 明确呈**磨砂圆角玻璃面板**，右侧任务栏仍锐利 | ✅ 通过（真模糊，非叠色） |
| B4 | 播控真实性（暂停/上下首） | 需读 SMTC 播放状态 | **未取得独立证据**：SMTC 从 Python 侧查询需 WinRT 异步，本机 `Add-Type` 被禁；且会话属于用户真实播放器，未擅自操作 | ⚠️ 未验证（见 §4） |
| B5 | 进度条拖动到 50% | 需 playing 且 duration>0 的可控会话 | **未取得独立证据**（当前会话 Paused；不擅动用户播放器） | ⚠️ 未验证 |
| B6 | 点击封面激活播放器 | 需合成点击 + `GetForegroundWindow` 进程比对 | **未取得独立证据** | ⚠️ 未验证 |
| B7 | 滚轮音量 | 需读 `IAudioEndpointVolume` | **未取得独立证据**；代码路径已审（`interaction.on_wheel`→`app.on_wheel`→`wasapi::set_system_volume`，HUD 1.2s 计时在 `on_anim_tick`） | ⚠️ 未验证 |

> 关于 B4–B7：不是没查，而是**本机没有可控的"会播放且时长已知"的会话**，且从进程外独立读取
> SMTC/音量需 WinRT/COM 异步能力，被本机安全策略阻断。工程师的自述（日志）可作为线索，但按
> QA 原则不作为结论。**这些项的结论是"未取得证据"，不是"通过"。**

---

## 3. C —— 边界与稳定性

| # | 项目 | 方法 | 证据 | 结论 |
|---|------|------|------|------|
| C1 | 无媒体会话时隐藏/崩溃 | 代码审查 + 尝试 | 代码：`compute_visible()` 在 `NoSession` 返回 false；`SessionCleared` 复位交互态。**未实测**：本机始终存在活动 SMTC 会话（用户播放器） | ⚠️ 仅代码级 |
| C2 | 播放器被强杀 3s 内收起 | 需 `Stop-Process` 用户播放器 | 未执行（会中断用户播放，风险高） | ⚠️ 未验证 |
| C3 | 配置损坏恢复 | 目录里已有 `config.bad.20261101-202618.json`（内容 `{ this is not json`），是工程师此前实测留下的备份 | 佐证恢复机制存在；**本次未重复写入**（避免污染用户配置） | ⚠️ 间接证据 |
| C4 | 单实例 | `qa/qa_m2.py` 启动第二实例并计时 | 第二实例**退出码 0、耗时 0.09s**；日志 `已有实例…已投递 WM_BARLINE_SHOW_SETTINGS`；互斥体名 `Local\Barline.SingleInstance.v1`（`Local\` 前缀→跨用户会话不互扰） | ✅ 通过（1s 内退出 + 跨会话隔离）。但"唤起设置窗口置前"在 M2 未实现（设置页是 M5），`on_second_instance` 仅重绘 |
| C5 | 5 分钟长稳 | `qa/qa_stability.py` 300s、每 15s 采样 | CPU **0.245%**；WS 38.45→38.74 MB（+0.29 MB 波动，非单调）；Private 11.66→11.45 MB | ✅ 通过（无泄漏、无抖动） |
| C6 | 自动隐藏 / 托盘图标增减 | 未主动改系统设置 | 间接：两次观测间托盘区宽度 366→398px（有图标增减），app 每次按当前 `tray_left` 贴合（右边缘 2154 = 2162−8）；**运行中动态增减未实测** | ⚠️ 部分（启动期贴合正确，动态未测） |

---

## 4. 未取得证据的验收项（明确列出）

以下项**未通过独立实测**，请勿视为通过：

1. **P0-01** 切歌 1s 内更新、关闭播放器 3s 内收起 —— 未操作真实播放器。
2. **P0-04** 播控三键 500ms 内真实改变播放状态 / 乐观更新回滚 —— 未验证。
3. **P0-07** 切换壁纸 300ms 内黑白切换、悬停 120ms 淡入/200ms 淡出 —— 未量化（本机壁纸锁定且动态）。
4. **P0-10** 拖动到 50% 跳转误差 ≤1s、拖动中屏蔽按钮 —— 未验证（无可控 playing 会话）。
5. **P0-11** 点击封面激活播放器到前台 —— 未验证。
6. **P1-01** 滚轮音量真实改变 + HUD 出现/消失 —— 未验证。
7. **P0-08/09/16** 歌词加载/逐字扫光/降级无抖动 —— 属 M3 范围，本快照只有 `♪ 暂无歌词` 占位，未验。
8. **P0-12** 设置页与托盘 —— M5 范围，本快照无设置页/托盘菜单。
9. **P0-15** 多显示器 / 125%·150%·200% / 四方位任务栏 —— 本机单屏 96dpi 底部任务栏，未验证其它档位。
10. **C1/C2/C3** 无会话、强杀播放器、配置损坏 —— 见 §3，仅代码/间接证据。
11. **C6** 运行中托盘图标动态增减、任务栏自动隐藏跟随 —— 未实测。

> 诚实声明：B1/B2/B3/C4/C5 是本报告**真正独立跑出来的证据**；其余项要么受环境阻断、要么超出
> M2 范围，已逐条标注"未取得证据"，不做含糊通过。

---

## 5. D —— 代码审查（只看不写）

| # | 审查点 | 发现 | 结论 |
|---|--------|------|------|
| D1 | `app.rs` 状态机（PRD §5 三台跨状态约束） | `reduce(NowPlaying)`：仅 `track_changed` 时 `interact.reset()` → 暂停（同曲）不打断 Seeking ✓；切歌退出 Seeking/VolumeHUD ✓；`sync_visibility()` 在 `!vis` 时 `interact.reset()` ✓。三条跨状态约束均落实 | ✅ 未见漏洞 |
| D2 | `sampler.rs` alpha 掩码是否防"文字自我强化" | `valid[i] = cov < 0.10`（`SAMPLE_COVERED_RATIO`），`cov` 取自上一帧 `last_coverage`（alpha>8 计覆盖）；模糊路径另做 `fill_holes` 填补自绘块。1px 字笔画在 4×4 块内覆盖 25%→被剔除，机制正确 | ✅ 机制成立。边界：4×4 块粒度；当 `valid_ratio<0.30`（密集布局）回退全块平均会含自身像素（本组件稀疏，风险低） |
| D3 | `unwrap()`/`expect()` 残留 | 全仓仅 `config/mod.rs` 4 处，且都在 `#[cfg(test)] mod tests`（244/245/265/275）。生产路径零残留 | ✅ 合规 |
| D4 | 渲染热路径 `format!` | `render/text.rs` 的 `format!` 仅在 `ellipsis()`（文本变化时）触发，标题/歌手结果缓存于 `layout.title_text/artist_text`；`format_hud_volume`/`format_time_pair` 仅在 HUD/Seeking 态。逐帧路径无 `format!` | ✅ 合规（§4.6 规则 2）。**但**架构 §11.1 要求的 `frame_hash` present 去重**未实现**（见 P2-02） |
| D5 | `unsafe` 是否都带 `// SAFETY:` | 3 处缺失：`app.rs:266`、`app.rs:271`、`ui/widget_wnd.rs:95`（其余 `unsafe` 块/`unsafe impl` 均有） | ❌ 违反 §4.6 规则 4（见 P2-01） |

---

## 6. 缺陷清单

### P1-01 · clippy 静态质量门未通过（11 error）
- **严重级**：P1（硬门槛不通过；不破坏运行时，但 `#![deny(warnings)]` + CI 语义下即失败）
- **复现**：`rustup component add clippy && powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task clippy`
- **期望**：0 error（M1 报告把"零 warning"列为硬门槛）
- **实际**：11 error，编译中止（清单见 §1）
- **涉及**：`render/painter.rs:177,244,295,538`、`render/sampler.rs:61,62,252,253,327`、`log.rs:122`、`render/d2d.rs:116`

### P2-01 · 3 处 `unsafe` 缺 `// SAFETY:` 注释
- **严重级**：P2（违反 `ARCHITECTURE.md` §4.6 规则 4）
- **位置**：`app.rs:266`（`(*ptr).widget.as_ref()`）、`app.rs:271`（启动媒体线程）、`ui/widget_wnd.rs:95`（`RegisterClassW`）
- **期望**：每个 `unsafe` 块上方有 `// SAFETY:` 说明不变式；**实际**：仅普通注释或无注释

### P2-02 · 架构 §11.1 的 `frame_hash` present 去重未实现
- **严重级**：P2（文档/实现漂移；当前 M2 无播放定时器故无影响，**M3 风险**）
- **证据**：`app.rs::redraw()` 无条件 `render()+present()`，全仓无 `frame_hash`/`last_hash`
- **影响**：M3 若按 §11.1 引入 16ms 歌词定时器，将每 16ms 全量 D2D 重绘 + WIC 回读 +
  `UpdateLayeredWindow`，缺少 present 去重会显著抬高播放态 CPU（当前空闲靠"无播放定时器 +
  `Theme::same_paint`"达成低 CPU，二者不是同一机制）

### P2-03 · P0-05「完全位于任务栏条带内」与歌词形态高度冲突
- **严重级**：P2（需产品确认，非实现 bug）
- **实测**：歌词双行形态小组件 218×**94**，任务栏仅 **48**px，窗口矩形 y=1346..1440 而条带
  y=1392..1440 → **向上超出任务栏 46px 到桌面**（`qa/shots/t-idle1.bmp` 可见）
- **冲突**：P0-05 要求"不超出任务栏上下边界"，P0-03/§4.1 又要求歌词区在任务栏上方。二者互斥，
  实现选择了后者。建议 PRD 澄清：紧凑形态须在条带内、歌词形态允许上溢

### P2-04 · 工作集已达 40MB 预算的 97%
- **严重级**：P2（M3 风险）
- **实测**：300s 长稳 WS 38.45→38.74 MB（目标 ≤40 MB，余量仅 ~1.3 MB）；工程师自述 36.6 MB
  偏乐观。M3 加入歌词 LRU 缓存（预算 1 MB）+ 逐字字形缓存后存在**超预算风险**

### P2-05 · 次要文字（歌手）60% 不透明度，对比度低于 4.5:1
- **严重级**：P2（若 P0-07「文字…≥4.5:1」严格适用于所有文字则为缺口）
- **证据**：`theme.rs:145` `text_dim = mix_rgb(text, bg_gray, 0.4)`（等效 ~60% 不透明度），
  对比度必然低于主文字。主文字 6.90:1 达标，歌手名按 PRD §4.1「60% 不透明度」是有意设计

---

## 7. 对 M3（歌词引擎）的风险提示

1. **clippy 必须先清零**：M3 新增的 LRC 状态机/插值若沿用现有风格（多参数绘制函数），
   `too_many_arguments` 会继续堆积；建议 M3 开工前先把 11 个 error 修掉并把 clippy 纳入
   `build.ps1 -Task check` 常规门。
2. **`frame_hash` 去重缺失是最大性能隐患**：M3 的逐字扫光需要 16ms 定时器。若不在 `redraw()`
   前加 present 去重，播放态 CPU 会明显高于 §11.1 的设计预期（当前 0.245% 是在"无播放定时器"
   前提下取得的）。**建议 M3 第一件事就是补 `frame_hash`**。
3. **内存余量只剩 ~1.3 MB**：歌词内存 LRU（预算 ≤1 MB）+ 双行歌词布局对象会吃掉余量，
   很可能触及 40 MB 上限；建议 M3 期间复测 WS，并考虑调低 LRU 容量。
4. **歌词区高度与 P0-05 的冲突要在 M3 前定调**（P2-03），否则歌词一上来就会"合法地"超出条带，
   验收时无法判定。
5. **采样/模糊对"动态背景"的鲁棒性**：本机小组件所在桌面区域是**动态**的（无 app 时 0.5s
   平均差 7.45/255）。M3 若在歌词区做更频繁的重采样，需确认 `same_paint` 门控能挡住动态背景
   引起的伪重绘，避免 CPU 抖动。
6. **单实例的"唤起设置页"是空实现**：`on_second_instance()` 目前只重绘。M5 接入设置页时，
   需把 `WM_BARLINE_SHOW_SETTINGS` 真正接到 WebView2 窗口，否则 P0-13 第 2 条永远不达标。

---

## 8. 复现脚本与产物

| 脚本 | 用途 |
|------|------|
| `qa/qa_m2.py` | 透明性/对比度/65s 内存 CPU/单实例（一次跑完） |
| `qa/qa_transparency.py` | 透明性信号-噪声对照 + 模糊梯度 |
| `qa/qa_stability.py` | 300s 长稳采样 |
| `qa/shots/*.bmp` | 本次全部截屏证据 |

关键实测汇总：`exe 747,520 B` · `125 passed / 0 failed / 0 ignored` · `clippy 11 error` ·
`空闲 CPU 0.245%(300s) / 0.385%(65s)` · `WS 38.7MB / Private 11.5MB` ·
`右边缘贴合 2154 = tray_left 2162 − 8（误差 0px）` · `文字对比度 6.90:1` ·
`悬停模糊梯度 −88%` · `单实例退出 0.09s`。
