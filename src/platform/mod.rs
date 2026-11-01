//! 平台层：最底层的 Win32 封装，只被依赖、不依赖任何业务模块（ARCHITECTURE.md 附录 A）。

pub mod com;
pub mod dpi;
pub mod single_instance;
pub mod wake;
pub mod wide;

use std::path::PathBuf;

use windows::core::PWSTR;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG};

/// `%LOCALAPPDATA%`。走 `SHGetKnownFolderPath` 而不是读环境变量（§14.1：环境变量可能被篡改）。
///
/// 失败时依次退化：`%LOCALAPPDATA%` 环境变量 → 系统临时目录。永不返回空路径。
pub fn local_app_data_dir() -> PathBuf {
    // SAFETY: SHGetKnownFolderPath 返回由 CoTaskMemAlloc 分配的 NUL 结尾宽字符串，
    // 我们把它复制成 String 后立刻 CoTaskMemFree，所有权完整交还。
    let from_api = unsafe {
        match SHGetKnownFolderPath(&FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG(0), None) {
            Ok(pwstr) => {
                let s = pwstr_to_string(pwstr);
                CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
                Some(PathBuf::from(s))
            }
            Err(_) => None,
        }
    };
    if let Some(p) = from_api {
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    std::env::temp_dir()
}

/// `%LOCALAPPDATA%\Barline`。**默认根目录**：config.json 永远在这里（用户约定：
/// 配置文件不随数据目录迁移），迁移源也以它为准。
pub fn barline_dir() -> PathBuf {
    local_app_data_dir().join("Barline")
}

static DATA_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// 设置生效的数据目录（v1.1.5 问题 3）。**只在启动早期调用一次**
/// （`App::bootstrap` 里 `Config::load` 之后、任何缓存/歌词模块使用之前）。
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

/// 当前生效的数据目录：日志、歌词缓存、应用歌词目录、WebView2 数据的根。
/// 未初始化（测试 / 极早启动日志）时退化为 [`barline_dir`]。
pub fn data_dir() -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(barline_dir)
}

/// 目录选择对话框（v1.1.5 问题 3；IFileOpenDialog，FOS_PICKFOLDERS）。
///
/// 用户取消 / COM 失败一律 `None`（调用方静默忽略即可）。成功返回绝对路径。
pub fn pick_folder(parent: windows::Win32::Foundation::HWND, title: &str) -> Option<PathBuf> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        FileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, IFileOpenDialog, SIGDN_FILESYSPATH,
    };
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let _ = dialog.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
        if !title.is_empty() {
            let _ = dialog.SetTitle(&windows::core::HSTRING::from(title));
        }
        // Show 返回 Err = 用户取消（HRESULT_FROM_WIN32(ERROR_CANCELLED)）或失败
        if dialog.Show(Some(parent)).is_err() {
            return None;
        }
        let item = dialog.GetResult().ok()?;
        let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = pwstr_to_string(pwstr);
        CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
        let p = PathBuf::from(s);
        if p.as_os_str().is_empty() {
            None
        } else {
            Some(p)
        }
    }
}

/// 系统的「应用模式」明暗设置（HKCU\...\Themes\Personalize\AppsUseLightTheme）。
///
/// 为什么不沿用小组件的壁纸采样结果（QA-M4 缺陷 #3）：设置页内容跟随系统的
/// `prefers-color-scheme`（CSS 媒体查询），标题栏必须与它**同源**，否则浅色系统 +
/// 深壁纸时出现「深标题栏 + 浅内容」。用 `RegGetValueW` 直接读 API（不走 reg.exe
/// 命令行——本机安全钩子会拦截）。读不到（老系统 / 值缺失）按浅色处理，
/// 即 Windows 出厂默认 `AppsUseLightTheme = 1`。
pub fn system_app_theme_dark() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let subkey = crate::platform::wide::to_wide(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
    );
    let value = crate::platform::wide::to_wide("AppsUseLightTheme");
    let mut data: u32 = 1; // 缺省 = 浅色
    let mut size = core::mem::size_of::<u32>() as u32;
    // SAFETY: 两个宽字符串以 NUL 结尾且在本调用期间存活；出参都是本地变量。
    let hr = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(subkey.as_ptr()),
            windows::core::PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if hr != ERROR_SUCCESS {
        return false;
    }
    data == 0 // 0 = 深色应用模式
}

/// 把窗口强行提到前台。
///
/// `SetForegroundWindow` 在「用户正在操作别的程序」时会被系统拒绝（返回 false）。
/// 标准绕过办法是先把本线程 attach 到当前前台窗口的输入队列上，改完再 detach——
/// 这样前台权限被判给「同一个输入队列」，不会抢走用户的输入焦点语义。
///
/// 托盘菜单尤其需要它：`TrackPopupMenuEx` 的宿主不是前台窗口时，菜单会收不到
/// 键盘/鼠标、点外面也不消失，表现为「菜单卡住」。
pub fn force_foreground(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, SetForegroundWindow, SetWindowPos, HWND_TOPMOST,
        SWP_NOMOVE, SWP_NOACTIVATE, SWP_NOSIZE,
    };

    // SAFETY: 全是无副作用的窗口/线程查询与置顶调用；句柄都来自本进程或系统。
    unsafe {
        let fg = GetForegroundWindow();
        let tid_cur = GetCurrentThreadId();
        let tid_fg = if fg.is_invalid() {
            0
        } else {
            windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, None)
        };
        let attached = tid_fg != 0 && tid_fg != tid_cur && AttachThreadInput(tid_cur, tid_fg, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        if !SetForegroundWindow(hwnd).as_bool() {
            crate::log_debug!("SetForegroundWindow 仍被拒绝（前台窗口可能正被用户操作）");
        }
        if attached {
            let _ = AttachThreadInput(tid_cur, tid_fg, false);
        }
    }
}

/// 把 PWSTR 读成 String（读到 NUL 为止），不持有所有权。
fn pwstr_to_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: 调用方保证 p 指向一段以 NUL 结尾的合法 UTF-16 序列
    // （SHGetKnownFolderPath 的契约）。
    let mut len = 0usize;
    unsafe {
        while *p.0.add(len) != 0 {
            len += 1;
            // 防御性上限：路径不可能超过 32K 个 UTF-16 码元
            if len > 32768 {
                break;
            }
        }
        let slice = core::slice::from_raw_parts(p.0, len);
        String::from_utf16_lossy(slice)
    }
}
