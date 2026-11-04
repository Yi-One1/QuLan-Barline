//! 文件日志：`%LOCALAPPDATA%\Barline\barline.log`
//!
//! 设计要点（ARCHITECTURE.md §14.7）：
//! - 全局 `OnceLock<Logger>`，内部 `Mutex<BufWriter<File>>`，任何线程可写；
//! - 单文件超 10 MB 轮转为 `barline.1.log`，最多保留 3 个历史文件；
//! - 行格式：`2025-11-01 19:45:12.345 [INFO ] [media] session changed: ...`
//!   第三个方括号是「模块标签」——取 `module_path!()` 的第二段，
//!   例如 `barline::media::smtc` → `media`，便于按模块 grep。
//! - `release` 默认 INFO，`debug` 默认 DEBUG，可用 `BARLINE_LOG=debug` 覆盖；
//! - `panic` hook 落盘后 flush，保证崩溃现场不丢。
//!
//! 这里不用 `log` / `tracing` crate（体积白名单，见 §4.1）。

use std::fs::{self, File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

/// 单文件上限（与 `config::defaults::limits::LOG_MAX_BYTES` 保持一致）。
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// 历史文件保留个数：barline.1.log .. barline.3.log
const ROTATE_KEEP: u32 = 3;

/// 日志级别。`PartialOrd` 用于阈值比较（Debug < Info < Warn < Error）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// 固定 5 字符宽，便于日志对齐。
    const fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

struct Logger {
    /// 写句柄。`None` 表示文件打开失败（此时只吞掉日志，不影响主流程）。
    file: Mutex<Option<BufWriter<File>>>,
    /// 当前文件已写字节数，用于轮转判断。
    written: AtomicU64,
    /// 当前日志路径。`Mutex`：支持启动后期数据目录迁移时换路径（v1.1.5）。
    path: Mutex<PathBuf>,
    min_level: Level,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// 日志初始化。**必须是 `main` 的第一条语句**，之后任何失败都能落盘。
pub fn init() {
    let dir = crate::platform::barline_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        // 连目录都建不出来（极罕见），退化到临时目录，保证还有日志可看。
        let fallback = std::env::temp_dir().join("Barline");
        let _ = fs::create_dir_all(&fallback);
        let _ = e;
        install(fallback.join("barline.log"));
        return;
    }
    install(dir.join("barline.log"));
}

fn install(path: PathBuf) {
    rotate_if_needed(&path);

    let opened = OpenOptions::new().create(true).append(true).open(&path);
    let (file, size) = match opened {
        Ok(f) => {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            (Some(BufWriter::with_capacity(16 * 1024, f)), len)
        }
        Err(_) => (None, 0),
    };

    let min_level = if cfg!(debug_assertions) || debug_requested() {
        Level::Debug
    } else {
        Level::Info
    };

    let _ = LOGGER.set(Logger {
        file: Mutex::new(file),
        written: AtomicU64::new(size),
        path: Mutex::new(path),
        min_level,
    });

    // panic hook：先落盘再交给默认 hook（release 下 panic=abort，hook 是最后机会）。
    std::panic::set_hook(Box::new(|info| {
        write(Level::Error, "panic", format_args!("{info}"));
        flush();
    }));
}

fn debug_requested() -> bool {
    matches!(std::env::var("BARLINE_LOG"), Ok(v) if v.eq_ignore_ascii_case("debug"))
}

fn rotate_if_needed(path: &PathBuf) {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < LOG_MAX_BYTES {
        return;
    }
    // 先删最老的，再逐个后移
    let _ = fs::remove_file(with_index(path, ROTATE_KEEP));
    for i in (1..ROTATE_KEEP).rev() {
        let _ = fs::rename(with_index(path, i), with_index(path, i + 1));
    }
    let _ = fs::rename(path, with_index(path, 1));
}

fn with_index(path: &Path, index: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| String::from("barline"));
    path.with_file_name(format!("{stem}.{index}.log"))
}

/// 把 `module_path!()` 压成模块标签：`barline::media::smtc` → `media`。
fn tag_of(module: &str) -> &str {
    let mut parts = module.split("::");
    let _crate = parts.next();
    match parts.next() {
        Some(second) => second,
        None => module,
    }
}

/// 真正的写入口。宏都指向它。
pub fn write(level: Level, module: &str, args: std::fmt::Arguments<'_>) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    if level < logger.min_level {
        return;
    }

    let stamp = timestamp();
    let tag = tag_of(module);
    let line = format!("{stamp} [{}] [{}] {}\n", level.tag(), tag, args);

    if let Ok(mut guard) = logger.file.lock() {
        if let Some(writer) = guard.as_mut() {
            use std::io::Write as _;
            let _ = writer.write_all(line.as_bytes());
            // 每条都 flush：M1 阶段日志量 ~1 行/秒，flush 成本可忽略，
            // 换来「taskkill 之后日志一定完整」的调试确定性。
            let _ = writer.flush();
        }
    }

    let total = logger.written.fetch_add(line.len() as u64, Ordering::Relaxed) + line.len() as u64;
    if total >= LOG_MAX_BYTES {
        rotate_live(logger);
    }
}

/// 运行期轮转：关旧文件、滚动、重开。
fn rotate_live(logger: &Logger) {
    if let Ok(mut guard) = logger.file.lock() {
        // 先把 writer 换出来并落盘，再关闭
        if let Some(mut writer) = guard.take() {
            use std::io::Write as _;
            let _ = writer.flush();
        }
    }
    let path = logger.path.lock().map(|p| p.clone()).unwrap_or_default();
    rotate_if_needed(&path);
    if let Ok(f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        if let Ok(mut guard) = logger.file.lock() {
            *guard = Some(BufWriter::with_capacity(16 * 1024, f));
        }
        logger.written.store(0, Ordering::Relaxed);
    }
}

/// 数据目录迁移：把日志（含轮转历史）搬到新目录并从那里继续写（v1.1.5）。
///
/// **必须先关写句柄再搬**——Windows 不允许改名打开中的文件。搬迁用
/// [`crate::storage::move_item`]：rename 优先，跨卷退化 copy，净搬（源不保留）。
/// 任何失败都不阻塞启动（日志留在原地继续写旧路径）。
pub fn relocate(new_dir: &Path) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    // 1) 关写句柄
    if let Ok(mut guard) = logger.file.lock() {
        if let Some(mut writer) = guard.take() {
            use std::io::Write as _;
            let _ = writer.flush();
        }
    }
    // 2) 搬当前日志 + 轮转历史
    let old_dir = logger
        .path
        .lock()
        .map(|p| p.parent().map(Path::to_path_buf).unwrap_or_default())
        .unwrap_or_default();
    let _ = fs::create_dir_all(new_dir);
    for name in ["barline.log", "barline.1.log", "barline.2.log", "barline.3.log"] {
        let src = old_dir.join(name);
        if src == new_dir.join(name) {
            continue;
        }
        match crate::storage::move_item(&src, &new_dir.join(name)) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("日志搬迁失败 {name}：{e}");
            }
        }
    }
    // 3) 换路径并重开
    let new_path = new_dir.join("barline.log");
    if let Ok(mut p) = logger.path.lock() {
        *p = new_path.clone();
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&new_path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if let Ok(mut guard) = logger.file.lock() {
            *guard = Some(BufWriter::with_capacity(16 * 1024, f));
        }
        logger.written.store(len, Ordering::Relaxed);
    }
}

/// 显式 flush（退出前调用）。
pub fn flush() {
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut guard) = logger.file.lock() {
            if let Some(writer) = guard.as_mut() {
                use std::io::Write as _;
                let _ = writer.flush();
            }
        }
    }
}

/// `2025-11-01 19:45:12.345`
fn timestamp() -> String {
    // SAFETY: GetLocalTime 只写一个由我们提供的栈上 SYSTEMTIME，无任何前置条件。
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    )
}

/// 文件名安全的紧凑时间戳：`20251101-194512`。用于 `config.bad.<ts>.json` 之类。
pub fn timestamp_compact() -> String {
    // SAFETY: 同 timestamp()
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Debug, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Info, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Warn, module_path!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_err {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Error, module_path!(), format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::tag_of;

    #[test]
    fn tag_is_second_segment() {
        assert_eq!(tag_of("barline::media::smtc"), "media");
        assert_eq!(tag_of("barline::main"), "main");
        assert_eq!(tag_of("barline"), "barline");
    }
}
