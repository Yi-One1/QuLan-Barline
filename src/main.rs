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
// dead_code 仍需整体放行——**但这次有实数了**。
//
// v1.11.0：本轮实测「摘掉这一行」时编译器一次报出 **35 项**零引用符号
// （`cargo check` 输出，未计 `#[cfg(test)]` 内的使用），远超此前人工盘点的 6 项。
// 这说明靠 `rg` 逐符号找死码的方法确实不可靠；但一次清 35 项属于跨模块大改，
// 会把这轮「低风险收尾」变成高风险重构，所以本轮**只删已逐条复核的 6 项**
// （App::sink / App::msg_hwnd / ProviderChain::new / net::get / RectF::inset /
// provider 测试区 stub），其余 35 项连同清单列进 `deliverables/gstack/` 的分诊报告，
// 单独立项再清。
//
// **坑（本轮实测踩到）**：`cargo check` **不编译 `#[cfg(test)]`**，所以「只被测试用到」
// 的辅助函数在它的输出里也显示为「never used」。本轮按清单删掉 `net::get` 后
// `cargo check` 依然全绿，直到 `cargo test` 才报三条 `cannot find function get`。
// 判死码必须看**测试目标**（`-Task test` 的编译结果），否则会误删测试脚手架。
//
// **下一轮清理的死码清单获取方式（别再用人工 rg）**：临时注释掉本行 → `build.ps1 -Task check`
// → 编译器把「零引用」全列出来 → 逐条判「真死 / 测试在用 / RAII 占位（如
// `SessionSubscriptions` 的三个事件句柄必须留）」→ 清完再加回本行。
//
// 已知的刻意留档（带局部 `#[allow(dead_code)]` 或依赖本行）：
//   - `media::launch::tray_click_activate`（用户裁决搁置的托盘激活实测实现）；
//   - `app::start_msg_timer` / `kill_msg_timer`（成对预留的幂等入口，刻意不成单）。
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
