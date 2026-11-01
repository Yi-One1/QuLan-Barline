# 曲栏 Barline — M4 实现报告（系统集成 · 第一批）

> 里程碑：**M4 系统集成**（T14 托盘 / T15 快捷键 / T16 音量与自启 / T13 设置页）
> 版本：v0.0.5 · 构建：`release` / `x86_64-pc-windows-msvc`
> 本轮范围：**P1-1 lrclib 超时修复 + P1-2 托盘 + P1-3 全局快捷键 + 退出卡死修复**
> 未完成：**P1-4 设置页（WebView2）**、P2 观感项 —— 见 §7

---

## 0. TL;DR

| 项目 | 结果 |
| --- | --- |
| 测试 | **203 passed / 0 failed / 1 ignored**（`ignored` 是联网的 lrclib 实测） |
| Clippy | `-D warnings` 零告警；`src/` 里零 `allow` |
| 体积 | `barline.exe` = **853 KB** |
| 在线歌词 | **修复**：`/api/get` 实测 **HTTP 200 / 7336 字节 / 6.0s**（原来 5s 超时自己掐断） |
| 托盘 | 图标常驻 ✓ / 三项菜单 ✓ / 勾选态 ✓ / 双击开设置 ✓ / Explorer 重启重加 ✓ |
| 快捷键 | 注册 / 冲突不崩 / 触发 ✓；**本机 5 组默认键被其它软件占用**（见 §3.3） |
| 退出 | 修掉「点退出后进程卡 25 秒」——现在 20ms 内干净退出（退出码 0） |

---

## 1. 文件清单

### 1.1 新建

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/ui/tray.rs` | 285 | 托盘图标 + 右键菜单 + 隐藏宿主窗口 |
| `src/hotkey.rs` | 245 | 6 组全局快捷键注册 / 冲突 / 挂起恢复 |
| `scripts/m4-tray-check.py` | 205 | 托盘 + 快捷键真机验收 |
| `scripts/m4-lrclib-check.py` | 110 | lrclib 端到端耗时验收 |

### 1.2 修改

| 文件 | 改动 |
| --- | --- |
| `src/lyric/net.rs` | 超时改成 `Timeouts` 四元组（resolve/connect/send/receive 分开）；`/api/get` 12s、`/api/search` 15s；新增 **25s 总预算**；`Response` 增加 `bytes`；新增 `#[ignore]` 的联网实测 |
| `src/lyric/lrclib.rs` | `fetch_once()` 计时日志（状态码 / 字节数 / 耗时）；超时或 5xx **立刻重试一次**（总预算 24s） |
| `src/lyric/service.rs` | **退出不再 join 歌词线程**（原来会卡到 25s 网络预算用尽） |
| `src/app.rs` | 托盘 / 快捷键接线；`on_tray_message` / `on_hotkey` / `reduce_hotkey`；`Effect::RefreshTrayMenu` / `RebindHotkeys` 落地 |
| `src/ui/mod.rs` / `src/main.rs` | 注册 `tray` / `hotkey` 模块 |
| `src/ui/widget_wnd.rs` | `WM_HOTKEY` 分发 |
| `src/media/wasapi.rs` | 新增 `toggle_system_mute()` |
| `src/config/mod.rs` | `HotkeyConfig` 派生 `Copy`（注册时要同时借用 config 与 `&mut hotkeys`） |

---

## 2. P1-1：在线歌词超时修复（根因是超时值，不是墙）

主理人用 curl 实测 lrclib **能通但慢**（`/api/get` 6.64s、`/api/search` 12.0s），
而我原来 `receive` 只给 5s——每次都被自己掐断，表现成"网络失败"。

**修法**：

| 项 | 原来 | 现在 |
| --- | --- | --- |
| resolve / connect / send | 3s / 3s / 3s | 5s / 5s / 5s |
| `/api/get` receive | 5s | **12s** |
| `/api/search` receive | 5s | **15s** |
| 失败重试 | 交给 service 的 5s/15s/60s 退避 | **立刻重试 1 次**（总预算 ≤ 25s），仍失败才进负缓存 |
| 总闸 | 无 | `TOTAL_BUDGET_MS = 25_000`（分片接收累计超时也判失败） |

> 为什么还要加"总闸"：`WinHttpSetTimeouts` 的 receive 是**单次接收操作**的超时，不是总时长。
> 实测本机一个 404 也能拖 **22.4s**（数据一点点滴出来，每次都 < 12s）。没有总闸时
> 歌词线程会被单个请求占住半分钟。

**验收证据**（`cargo test lrclib_live -- --ignored --nocapture`）：

```text
lrclib /api/get → HTTP 200 / 7336 字节 / 6.0s
test lyric::net::tests::lrclib_live_get_returns_synced_lyrics ... ok
```

应用层日志格式（每次请求都会打）：

```text
[lyric] 请求 lrclib：https://lrclib.net/api/get?track_name=..&artist_name=..&duration=140
[lyric] lrclib 响应：HTTP 404 / 84 字节 / 22.4s        ← 修复前的慢请求实例
```

---

## 3. P1-2：托盘图标（T14）

### 3.1 结构

托盘有自己的**隐藏顶层窗口**（`BarlineTrayWnd`），不是复用小组件窗口：

- 小组件是 `WS_EX_NOACTIVATE`（按设计不抢焦点）→ 拿它当菜单宿主，菜单会飘；
- 消息窗是 `HWND_MESSAGE`（没有 Z 序）→ 无法 `SetForegroundWindow`。

`Shell_NotifyIconW(NIM_ADD/NIM_MODIFY/NIM_DELETE)` + `LoadImageW` 取 `assets/barline.rc`
里 id=1 的图标（16×16）。

### 3.2 菜单

三项 + 勾选态 + 分隔线，`SetForegroundWindow` → `TrackPopupMenuEx(TPM_RETURNCMD)` → 补 `WM_NULL`。

真机日志（`scripts/m4-tray-check.py`）：

```text
[ui] 托盘图标已添加（tip=曲栏 Barline）           图标矩形 = (2162,1392)-(2194,1440)
[app] 托盘回调：右键抬起，弹菜单
[ui] 托盘菜单：SetForegroundWindow=true 光标=(2000,1298)
[ui] 托盘菜单返回 cmd=0                            ← Esc 取消
[ui] 托盘菜单返回 cmd=102  → [app] 托盘：小组件隐藏   ← 可见性 True → False
[ui] 托盘菜单返回 cmd=102  → 可见性 False → True     ← 再点一次恢复
[ui] 托盘菜单返回 cmd=103  → [app] 托盘：退出 → 进程退出，退出码 0
[ui] 托盘菜单返回 cmd=101  → [app] 第二个实例请求唤起 ← 显示设置（等 T13 接上窗口）
```

- `TaskbarCreated` 广播 → `App::on_taskbar_created()` 里重新 `NIM_ADD`（T14 第 4 条）。
- 语言切换（`ConfigChanged`）→ `Effect::RefreshTrayMenu` → `NIM_MODIFY` 刷新 tooltip；
  菜单文案每次弹出时现读 `i18n::t(lang, ..)`，所以永远是最新语言。
- 退出时 `Tray::drop` → `NIM_DELETE`，通知区不留死图标。

### 3.3 一个真机发现：图标被折叠进「隐藏图标」浮出层

Win11 默认把**新注册**的托盘图标放进 `^` 浮出层。`Shell_NotifyIconGetRect` 返回的是
那个 `^` 按钮的矩形（`(2162,1392)-(2194,1440)`），直接点托盘区点不到我们的图标。

验收脚本因此改为**直接投递 shell 的回调消息**（`PostMessage(WM_BARLINE_TRAY, WM_RBUTTONUP)`）
——等价于"用户右键了图标"，菜单本身的代码路径（建菜单 / 勾选态 / 键盘导航 / 命令分发）
完全是正式代码。截图 `m4-01-tray-menu.bmp` 就是这样拿到的。

> 用户把图标拖到常显区后，鼠标右键的链路与这条完全一致。

---

## 4. P1-3：全局快捷键（T15）

- 6 组固定 id 1..=6 → `RegisterHotKey(msg_hwnd, id, mods|MOD_NOREPEAT, vk)`；
  `WM_HOTKEY` 在 `msg_proc` 里分发到 `App::on_hotkey`。
- 语义复用小组件按钮：`PlayPause/Prev/Next/ToggleLyrics` 直接走 `reduce_input`，
  `ToggleWidget` 走托盘同一条逻辑，`Mute` 走 WASAPI（`toggle_system_mute`）。
- 冲突：`RegisterHotKey` 失败时记 `Conflict { action, binding, code }`，
  **只记日志、不阻止启动**（R4 拍板），日志里写明错误码与"已被其它程序占用"。
- `Effect::RebindHotkeys` → 先全部 `UnregisterHotKey` 再重注册（配置变更即生效）。
- `Hotkeys::suspend/resume`：给设置页录制态用（录制期间全部注销，避免误触发）。

### 4.1 真机发现：5 组默认键在本机已被占用

```text
[hotkey] 热键注册失败：PlayPause     = mods=0x3 vk=0x50，错误码 1409（已被其它程序占用）
[hotkey] 热键注册失败：Prev          = mods=0x3 vk=0x25，错误码 1409
[hotkey] 热键注册失败：Next          = mods=0x3 vk=0x27，错误码 1409
[hotkey] 热键注册失败：ToggleLyrics  = mods=0x3 vk=0x4C，错误码 1409
[hotkey] 热键注册失败：Mute          = mods=0x3 vk=0x4D，错误码 1409
[hotkey] 热键已注册：ToggleWidget    = mods=0x3 vk=0x48
```

用 `RegisterHotKey` 单独探过：`Ctrl+Alt+P / ← / M / L` 全部返回 1409，
`Ctrl+Alt+H`、`Ctrl+Shift+Alt+P`、`Ctrl+Shift+F9` 可用。**程序没崩、没卡、其余功能正常**，
符合 R4 的约定；`Ctrl+Alt+H` 真机触发已验证：

```text
[app] 热键触发：ToggleWidget  →  小组件可见性 True → False → True
```

> 建议：设置页（T13）做好后，把默认值换成冲突率更低的组合（如 `Ctrl+Shift+Alt+*`），
> 或在首次启动时检测冲突并提示用户改绑。这条留给你拍板。

---

## 5. 顺带修掉的 P0：点「退出」后进程卡住

点托盘「退出」→ `WM_QUIT` → `App::shutdown()` → `LyricService::shutdown()` **`join` 歌词线程**。
而歌词线程可能正卡在一次最长 25 秒的网络请求里 → 进程"退而不出"。

修法：`shutdown()` 只断开命令通道 + **detach**（不等 join）。理由写在代码注释里：
进程即将结束，`EventSink::send` 在接收端销毁后只是静默失败，不会 panic。

修复后：

```text
[app] 收到 Quit，准备退出
[ui]  收到 WM_QUIT，消息泵退出
[media] 媒体线程退出
[lyric] 歌词线程退出
[render] 采样线程退出（共采样 12 次）
[barline] 进程退出，退出码 0
```

从点「退出」到进程消失 **20ms**（原来 ≥ 5s，最坏 25s）。

---

## 6. 验收截图

| 文件 | 说明 |
| --- | --- |
| `m4-01-tray-menu.bmp` | 托盘右键菜单：`显示设置` / `显示/隐藏小组件` / `退出`（560×300，含任务栏上下文） |

> 设置页六个分组、明暗主题的截图**未产出**——设置页本轮未实现（见 §7）。

---

## 7. 本轮未完成项（明确交付边界）

| 项 | 状态 | 说明 |
| --- | --- | --- |
| **P1-4 设置页（WebView2）** | **未开始** | 见下方说明 |
| P2-1 长歌词行右侧 24px 渐隐 | 未做 | 方案已定：给歌词那两行用 `ID2D1LinearGradientBrush` 当文字笔刷（末 24px alpha 255→0），零额外绘制遍数 |
| P2-2 行切换 150ms 上移动画 | 未做 | 需要"上一行 y 偏移"状态，约 40 行 |

### 为什么设置页没做完

`webview2-com 0.39.1` 已在本机 registry 缓存里（可离线构建），但**设置页不是"加个窗口"**：

1. `CreateCoreWebView2EnvironmentWithOptions` 是**异步**的，按架构 §6.3 必须走完成回调、
   不能 `join`/`wait_with_pump`（那会把 UI 线程卡住，小组件同时失去响应）；
2. 6 组控件的 Fluent HTML 要 1500 行左右（含明暗主题、i18n 字典、双向 JSON 协议）；
3. 关窗要 `controller.Close()` + 释放全部 COM 引用 + `DestroyWindow`（否则常驻 80MB+，
   直接违反 40MB 预算）——这条最容易被"先隐藏窗口"糊过去，但会炸内存。

我评估**本轮剩余预算不足以把它做到可交付的质量**（半成品比没有更糟：WebView2 一旦
初始化不完整，关窗时容易漏引用）。所以本轮先把托盘 / 快捷键 / 在线歌词 / 退出这四件
**能验收的**做完并交给你，设置页单独一轮做。

如果时间紧，我的建议是：**下一轮只做设置页**（不做别的），按
`window.rs`（Mica 窗口）→ `webview.rs`（环境 + controller + 完成回调）→
`bridge.rs`（JSON 协议）→ `assets/ui/index.html`（六组控件）的顺序，做完再一起验收。

---

## 8. 复现步骤

```text
build.ps1                     → 零 warning
build.ps1 -Task test          → 203 passed / 1 ignored
build.ps1 -Task clippy        → 0 warning 0 error

python scripts\m4-tray-check.py     → 托盘图标 / 三项菜单 / 勾选态 / 快捷键触发 / 干净退出
python scripts\m4-lrclib-check.py   → 真实 lrclib 请求的耗时与状态码

# 联网实测（默认 #[ignore]，不拖慢常规测试）
target\release\deps\barline-*.exe lrclib_live --ignored --nocapture
```
