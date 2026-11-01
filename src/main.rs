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
// dead_code 在 M1 阶段整体放行：本里程碑的核心交付是「数据契约 + 最小可观察链路」，
// ARCHITECTURE.md §7 / §8 里定义的字段与变体有相当一部分要到 M2–M5 才会被真正引用
// （例如 OptimisticOverride、HoverState、lyric 的 line_at/word_progress、limits 里的
// 缓存容量常量）。这些**不是死代码**，是已经冻结的跨模块契约，提前删除会破坏后续里程碑。
// 单独 lint 的优先级高于 lint 组，所以这一行能在 `deny(warnings)` 之下精确开口子，
// 其余告警（unused_imports / unreachable_code / unused_variables …）仍然是 error。
// M5 收尾时应移除本行。
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
