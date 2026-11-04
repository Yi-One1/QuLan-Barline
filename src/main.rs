//! 曲栏 Barline —— 入口。
//!
//! 启动顺序**不能变**（`ARCHITECTURE.md` T01 第 5 条）：
//! ```text
//! log::init()                                   // 最先，后续任何失败都能记日志
//! SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)
//! single_instance::acquire()                    // 已有实例 → 唤起并 ExitProcess(0)
//! ComApartment::sta()                           // RAII，Drop 时 CoUninitialize
//! App::bootstrap()                              // 建窗口 / 渲染器 / 媒体线程
//! message_loop()
//! ```

// 零 warning 是 M1 的硬门槛。任何新增告警都会让 `cargo build` 直接失败。
#![deny(warnings)]
// dead_code 整体放行。**这条 allow 早已过期，但不再用"里程碑还没到"当理由**——事实是：
// M1–M5 全部已发布（v1.8.0），本行却一直留到现在，于是**死码不会被编译器发现**。
// 当前被它兜住的都是「刻意留档」的东西，而不是"将来会用到"：
//   - `media::launch::tray_click_activate`（用户裁决搁置的托盘激活实测实现，progress.md 待办 D）；
//   - `app::start_msg_timer` / `kill_msg_timer`（成对预留的幂等入口，刻意不成单）；
//   - `Effect::RefreshTrayMenu`（「改语言后托盘不刷新」缺口的修复挂点）。
// 代价与纪律：**G6 这类死码清理只能靠人工 `rg` 逐符号盘点**（编译器帮不上忙），
// 每轮清理必须自己维护清单 + 用 `rg` 反向验证零引用，不能指望 `cargo build` 报错。
// 单独 lint 的优先级高于 lint 组，所以这一行能在 `deny(warnings)` 之下精确开口子，
// 其余告警（unused_imports / unreachable_code / unused_variables …）仍然是 error。
#![allow(dead_code)]
// GUI 程序：不弹控制台窗口。日志一律走文件。
#![windows_subsystem = "windows"]

mod app;
mod config;
mod event;
mod hotkey;
mod i18n;
mod log;
mod lyric;
mod media;
mod platform;
mod render;
mod settings;
mod storage;
mod taskbar;
mod ui;

use app::App;
use platform::com::ComApartment;

fn main() {
    // ---- 1. 日志（必须最先）----
    log::init();
    log_info!(
        "================ 曲栏 Barline v{} 启动 ================",
        env!("CARGO_PKG_VERSION")
    );
    log_info!(
        "构建：profile={} rustc={} 数据目录={}",
        if cfg!(debug_assertions) { "debug" } else { "release" },
        rustc_version_hint(),
        platform::barline_dir().display()
    );

    // ---- 2. DPI 感知（必须在创建任何窗口之前）----
    let dpi_ok = platform::dpi::set_process_dpi_aware();
    log_info!(
        "SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2) = {}",
        if dpi_ok { "成功" } else { "失败（可能已被清单/更早调用锁定）" }
    );

    // ---- 3. 单实例 ----
    if !platform::single_instance::acquire() {
        platform::single_instance::wake_existing();
        log_info!("第二个实例：已唤起主实例，本进程立即退出");
        log::flush();
        std::process::exit(0);
    }

    // ---- 4. COM STA（UI 线程）----
    let com = ComApartment::sta();
    match com.as_ref() {
        Some(c) if c.is_active() => log_info!("CoInitializeEx(STA) 成功"),
        _ => log_warn!("CoInitializeEx(STA) 未生效（继续运行，WebView2 可能受影响）"),
    }

    // ---- 5. 组装 ----
    let app_ptr = match App::bootstrap() {
        Ok(p) => p,
        Err(e) => {
            log_err!("启动失败：{e}");
            log::flush();
            std::process::exit(1);
        }
    };

    // ---- 6. 消息泵 ----
    let exit_code = ui::run_message_loop();

    // ---- 7. 收尾 ----
    // SAFETY: app_ptr 由 App::bootstrap 通过 Box::into_raw 产生，此处是唯一一次回收；
    // 消息泵已退出，不会再有任何窗口过程使用它。
    unsafe { App::shutdown(app_ptr) };
    drop(com);
    log_info!("进程退出，退出码 {exit_code}");
    log::flush();
    std::process::exit(exit_code);
}

/// 不引入额外依赖地拿到编译器版本：`rustc` 的版本在编译期不可直接读取，
/// 这里用 `RUSTC` 无关的常量替代，只记录标准库的 cfg 信息。
fn rustc_version_hint() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x86_64-pc-windows-msvc"
    } else {
        "non-64bit"
    }
}
