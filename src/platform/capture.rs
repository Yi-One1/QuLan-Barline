//! 截图工具探测（v1.2.0 / G5 问题 2）。
//!
//! ## 为什么需要它
//!
//! 截图工具（Snipping Tool / PixPin / Snipaste / ShareX …）启动后会创建一个
//! **全屏置顶遮罩**。我们的窗口也是 topmost，压不过遮罩也不被遮罩压住，所以
//! 遮罩期间小组件仍然可见、仍可点击——但**抓屏拿到的像素已经变了**
//! （整屏被压暗 / 加了十字线），背景亮度被污染 → 文字色可能翻一下；
//! 某些工具（PixPin）还会让任务栏几何短暂变化 → 小组件**左移**一下，
//! 1~2 秒后才弹回来。用户看到的就是「截图时小组件被影响」。
//!
//! 修法是**只冻结「跟屏幕变」的那部分**：截图态期间不抓屏、不重排几何；
//! 播放进度、歌词、封面、动画帧照常刷新。
//!
//! ## 分层
//!
//! 本模块**只做探测**，而且刻意分成两半：
//!
//! * [`foreground_info`]：薄壳，取前景窗口的 pid + 类名。
//! * [`is_capture_tool`]：**纯函数**，可单测、无 Win32 依赖。
//!
//! `pid → 进程名`的解析不在这里做——那属于业务层（`media::launch`），
//! 平台层不该反向依赖业务模块。调用方（`App`）自己拼：
//! `is_capture_tool(&process_image_name(pid), &class)`。
//!
//! ## 白名单要能长大
//!
//! 截图工具层出不穷，两个常量数组就是唯一的扩展点：
//! 新工具只要把进程名加进 [`CAPTURE_PROCESSES`]、把类名特征加进
//! [`CAPTURE_CLASS_HINTS`] 即可，不需要改任何逻辑。
//!
//! ## v1.4.4（G2-B2）：顺带判「前台是不是全屏应用」
//!
//! 全屏视频 / 游戏时读桌面既贵又没意义，心跳采样要能跳过。为了**不新增任何
//! 跨进程调用**，全屏判定复用了本来每秒就要做的那一次 [`foreground_info`]：
//! 同一个 `unsafe` 块里再取 `GetWindowRect`（win32k 本地查询，不是发给目标进程的
//! IPC）与 [`crate::platform::dpi::monitor_rect`]（纯本地），把结论塞进
//! [`ForegroundInfo::fullscreen`]。判定本身是纯函数 [`is_fullscreen_window`]，
//! 有单测。

use windows::Win32::Foundation::RECT;

/// 已知截图工具的**进程名**（小写，含 `.exe`）。
///
/// 覆盖：Windows 自带（Snipping Tool / Win10 截图工具 / Win+Shift+S 的 ScreenSketch）、
/// 国内常用（PixPin / Snipaste）、国外常用（ShareX / FastStone Capture）。
pub const CAPTURE_PROCESSES: &[&str] = &[
    // Windows 11 的「截图工具」与 Win+Shift+S 的宿主
    "snippingtool.exe",
    "screenclippinghost.exe",
    "screensketch.exe",
    // 第三方
    "pixpin.exe",
    "snipaste.exe",
    "sharex.exe",
    "fscapture.exe",
];

/// 已知截图工具的**窗口类名**特征（小写子串匹配）。
///
/// 有些工具的主窗口进程名会被宿主改掉（UWP / 单文件打包 / 升级后改名），
/// 类名反而更稳定；两边任一命中就算截图态。
pub const CAPTURE_CLASS_HINTS: &[&str] = &["snippingtool", "screenclipping", "pixpin", "snipaste"];

/// 全屏判定的容差（物理像素）。
///
/// 为什么要容差：部分播放器 / 游戏的全屏窗口会比显示器边缘多出 1~2px
/// （自绘边框、`WS_POPUP` 的 `AdjustWindowRect` 残留），逐像素相等会漏判；
/// 取 3px 又足够小——正常窗口（标题栏 + 任务栏）差着几十像素，不会误判。
pub const FULLSCREEN_TOLERANCE_PX: i32 = 3;

/// 前景窗口的原始信息。
///
/// 只 derive `PartialEq`（不是 `Eq`）：`RECT` 只实现 `PartialEq`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForegroundInfo {
    /// 前景窗口所属进程；`0` 表示取不到（无前台窗口 / 调用失败）
    pub pid: u32,
    /// 前景窗口类名；取不到时为空串
    pub class: String,
    /// 前景窗口的窗口矩形（屏幕坐标，物理像素）；取不到时全 0
    pub rect: RECT,
    /// 该窗口所在显示器（最近的）**整块**矩形；取不到时全 0
    pub monitor: RECT,
    /// 前景窗口是否全屏（[`is_fullscreen_window`] 的结论，v1.4.4 / G2-B2）
    pub fullscreen: bool,
}

/// 取当前前台窗口的 pid、类名、窗口矩形与「是否全屏」。
///
/// 无前台窗口（锁屏 / 桌面切换瞬间）返回 `pid = 0`、矩形全 0、`fullscreen = false`，
/// 调用方按「不是截图工具、不是全屏」处理即可。
///
/// **调用次数约束（v1.4.4 / v1.5.6）**：1s 心跳里只调用本函数**一次**，
/// 截图态探测与全屏遮挡判定共用这一次的结果；appbar 事件到达时会**追加一次**
/// 纯本地查询（同样只调一次，不做 `detect_capture_with` / `OpenProcess`）。
/// 事件路径绝不触发 `detect_capture_with`。
pub fn foreground_info() -> ForegroundInfo {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    };

    // SAFETY: 四个调用都是只读的窗口查询；类名缓冲区是本地栈数组，
    // 长度按元素个数传入，GetClassNameW 只会写入不超过该长度的 UTF-16。
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return ForegroundInfo::default();
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(fg, Some(&mut pid));

        // 类名上限 256 个码元：Win32 类名规范上限是 256，足够覆盖全部已知工具
        let mut buf = [0u16; 256];
        let n = GetClassNameW(fg, &mut buf);
        let class = if n > 0 {
            let len = (n as usize).min(buf.len());
            String::from_utf16_lossy(&buf[..len])
        } else {
            String::new()
        };

        // v1.4.4（G2-B2）：全屏判定所需的两个矩形，就地取，不额外发一次查询。
        // `GetWindowRect` 是 win32k 的本地查询（不向目标进程发 IPC）；
        // `monitor_rect` 走 `MonitorFromWindow` + `GetMonitorInfoW`，纯本地。
        let mut rect = RECT::default();
        if GetWindowRect(fg, &mut rect).is_err() {
            rect = RECT::default();
        }
        let monitor = crate::platform::dpi::monitor_rect(fg)
            .map(|m| m.monitor)
            .unwrap_or_default();
        let fullscreen = is_fullscreen_window(rect, monitor);

        ForegroundInfo {
            pid,
            class,
            rect,
            monitor,
            fullscreen,
        }
    }
}

/// 判定「窗口是不是整块盖住了所在显示器」——**纯函数**，无 Win32 依赖，可单测。
///
/// 四条边都要落在 [`FULLSCREEN_TOLERANCE_PX`] 之内才算全屏：
///
/// * 最大化窗口只盖到**工作区**（任务栏那条露在外面）→ 判 `false`，这是对的：
///   最大化浏览器 / 资源管理器不会让读桌面变贵，也没有「看全屏视频」的语义；
/// * 显示器矩形取不到（全 0）→ 判 `false`（绝不能因为拿不到数据就永久跳过采样）；
/// * 窗口比显示器还大（自绘边框溢出）→ 边差仍在容差内就判 `true`。
pub fn is_fullscreen_window(win: RECT, monitor: RECT) -> bool {
    let mw = monitor.right - monitor.left;
    let mh = monitor.bottom - monitor.top;
    if mw <= 0 || mh <= 0 {
        return false;
    }
    let t = FULLSCREEN_TOLERANCE_PX;
    (win.left - monitor.left).abs() <= t
        && (win.top - monitor.top).abs() <= t
        && (win.right - monitor.right).abs() <= t
        && (win.bottom - monitor.bottom).abs() <= t
}

/// 判定「这是不是一个截图工具」——**纯函数**，大小写不敏感。
///
/// * 进程名精确匹配 [`CAPTURE_PROCESSES`]（避免 `mysnipastehelper.exe` 之类误命中）；
/// * 类名按子串匹配 [`CAPTURE_CLASS_HINTS`]（类名常带随机后缀，如
///   `PixPin_Widget_1234`）。
/// * 两者都为空 → `false`（拿不到前端窗口时绝不能误判成截图态，
///   否则小组件会被永久冻结）。
pub fn is_capture_tool(process: &str, class: &str) -> bool {
    let p = process.trim().to_ascii_lowercase();
    let c = class.trim().to_ascii_lowercase();
    if p.is_empty() && c.is_empty() {
        return false;
    }
    if !p.is_empty() && CAPTURE_PROCESSES.iter().any(|want| *want == p) {
        return true;
    }
    !c.is_empty() && CAPTURE_CLASS_HINTS.iter().any(|hint| c.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_capture_tools_are_detected() {
        assert!(is_capture_tool("pixpin.exe", ""), "PixPin 必须命中");
        assert!(
            is_capture_tool("SnippingTool.exe", ""),
            "SnippingTool 大小写不敏感"
        );
        assert!(is_capture_tool("screensketch.exe", ""), "Win+Shift+S 宿主必须命中");
        assert!(is_capture_tool("snipaste.exe", ""), "Snipaste 必须命中");
    }

    #[test]
    fn class_name_hint_also_detects() {
        // 进程名被宿主改掉的场景：只靠类名
        assert!(is_capture_tool("unknown.exe", "PixPin_Widget_1234"));
        assert!(is_capture_tool("", "ScreenClippingHost"));
        assert!(!is_capture_tool("unknown.exe", "Chrome_WidgetWin_1"));
    }

    #[test]
    fn ordinary_windows_are_not_capture_tools() {
        assert!(!is_capture_tool("chrome.exe", ""), "浏览器不是截图工具");
        assert!(!is_capture_tool("explorer.exe", "Shell_TrayWnd"));
        assert!(!is_capture_tool("", ""), "空串绝不能命中（会永久冻结小组件）");
        assert!(!is_capture_tool("   ", "   "), "纯空白等于空串");
    }

    #[test]
    fn process_match_is_exact_not_substring() {
        // 子串匹配会让「名字里带 snipaste 的无关进程」误判
        assert!(
            !is_capture_tool("snipastehelper_old.exe", ""),
            "进程名必须精确匹配"
        );
        assert!(!is_capture_tool("mypixpinnotes.exe", ""));
    }

    #[test]
    fn whitelist_entries_are_lowercase_and_non_empty() {
        for p in CAPTURE_PROCESSES {
            assert_eq!(*p, p.to_ascii_lowercase(), "白名单必须小写：{p}");
            assert!(p.ends_with(".exe"), "进程名必须带扩展名：{p}");
        }
        for c in CAPTURE_CLASS_HINTS {
            assert_eq!(*c, c.to_ascii_lowercase(), "类名特征必须小写：{c}");
            assert!(!c.is_empty());
        }
    }

    // ------------------------------------------------ v1.4.4：全屏判定（B2）

    /// 一台 1920×1080 主显示器。
    fn monitor_1080p() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    /// 边缘逐像素贴合 = 全屏。
    #[test]
    fn exact_monitor_rect_is_fullscreen() {
        assert!(is_fullscreen_window(monitor_1080p(), monitor_1080p()));
    }

    /// 容差内的偏移仍算全屏（自绘边框 / `WS_POPUP` 残留），容差外不算。
    #[test]
    fn tolerance_boundary_is_inclusive() {
        let t = FULLSCREEN_TOLERANCE_PX;
        assert!((2..=4).contains(&t), "容差取 2~4px（任务书口径），实际 {t}");
        let m = monitor_1080p();

        for d in [-t, t] {
            let win = RECT {
                left: m.left + d,
                top: m.top,
                right: m.right,
                bottom: m.bottom,
            };
            assert!(is_fullscreen_window(win, m), "左边缘偏 {d}px 仍在容差内");
            let win = RECT {
                left: m.left,
                top: m.top,
                right: m.right + d,
                bottom: m.bottom,
            };
            assert!(is_fullscreen_window(win, m), "右边缘偏 {d}px 仍在容差内");
        }

        // 容差 + 1px 就必须判否
        let over = t + 1;
        for win in [
            RECT {
                left: m.left + over,
                ..m
            },
            RECT {
                top: m.top + over,
                ..m
            },
            RECT {
                right: m.right - over,
                ..m
            },
            RECT {
                bottom: m.bottom - over,
                ..m
            },
        ] {
            assert!(!is_fullscreen_window(win, m), "超容差必须判否：{win:?}");
        }
    }

    /// 最大化窗口只盖到工作区（任务栏露在外面）→ **不是**全屏。
    #[test]
    fn maximized_window_is_not_fullscreen() {
        let m = monitor_1080p();
        let work = RECT {
            bottom: 1080 - 48, // 48px 任务栏
            ..m
        };
        assert!(!is_fullscreen_window(work, m), "最大化 ≠ 全屏（不该跳过采样）");
        // 常见的带标题栏普通窗口更不可能命中
        let normal = RECT {
            left: 100,
            top: 80,
            right: 1100,
            bottom: 700,
        };
        assert!(!is_fullscreen_window(normal, m));
    }

    /// 窗口比显示器还大（自绘边框溢出）→ 边差在容差内仍算全屏。
    #[test]
    fn oversized_border_still_counts_as_fullscreen() {
        let m = monitor_1080p();
        let win = RECT {
            left: m.left - 1,
            top: m.top - 1,
            right: m.right + 1,
            bottom: m.bottom + 1,
        };
        assert!(is_fullscreen_window(win, m));
    }

    /// 显示器矩形拿不到（全 0 / 反向）→ **绝不**判全屏，否则心跳采样会被永久跳过。
    #[test]
    fn unknown_monitor_never_reports_fullscreen() {
        let zero = RECT::default();
        assert!(!is_fullscreen_window(zero, zero));
        assert!(!is_fullscreen_window(monitor_1080p(), zero));
        let inverted = RECT {
            left: 1920,
            top: 1080,
            right: 0,
            bottom: 0,
        };
        assert!(!is_fullscreen_window(monitor_1080p(), inverted));
    }

    /// 副屏同样成立（矩形不要求从 (0,0) 开始）。
    #[test]
    fn secondary_monitor_offset_is_fullscreen() {
        let m = RECT {
            left: 1920,
            top: -200,
            right: 3840,
            bottom: 880,
        };
        assert!(is_fullscreen_window(m, m));
        let win = RECT {
            left: 1921,
            top: -200,
            right: 3840,
            bottom: 879,
        };
        assert!(is_fullscreen_window(win, m));
    }

    /// `ForegroundInfo` 的默认值必须是「安全态」：不是截图工具、不是全屏。
    #[test]
    fn default_foreground_info_is_safe() {
        let info = ForegroundInfo::default();
        assert_eq!(info.pid, 0);
        assert!(!info.fullscreen, "默认不许是全屏（否则心跳采样永久停摆）");
        assert_eq!(info.rect, RECT::default());
        assert_eq!(info.monitor, RECT::default());
        assert!(!is_capture_tool(&info.class, &info.class));
    }
}
