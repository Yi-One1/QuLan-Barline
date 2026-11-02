//! 汽水音乐「托盘恢复卡死」诊断工具（临时的，不进发布包）。
//!
//! 用法：
//!   cargo run --release --example diag_soda -- enum      只枚举 SodaMusic 全部顶层窗口
//!   cargo run --release --example diag_soda -- activate  复现 launch.rs 激活序列（逐步打印）
//!
//! 对照实验：用户点托盘图标恢复正常后，再跑一次 `enum`，对比两次窗口结构差异。

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, IsWindowEnabled, SendInput, SetActiveWindow, SetFocus, INPUT, INPUT_0,
    INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEINPUT, MOUSE_EVENT_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SendMessageTimeoutW, SetForegroundWindow, ShowWindow, SwitchToThisWindow, SMTO_ABORTIFHUNG,
    GWL_EXSTYLE, GWL_STYLE, GUITHREADINFO, SW_HIDE, SW_RESTORE, WM_NULL, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "enum".into());
    match mode.as_str() {
        "enum" => enum_all(),
        "activate" => activate(),
        "fg" => {
            let h = unsafe { GetForegroundWindow() };
            println!("前台窗口 = 0x{:X}", h.0 as usize);
            if let Some(w) = probe(h) {
                dump(&w);
            }
        }
        "click" => click_test(),
        "fix" => fix_test(),
        other => eprintln!("未知模式：{other}（enum / activate / fg）"),
    }
}

// ---------------------------------------------------------------- 进程

fn soda_pids() -> Vec<u32> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).expect("snapshot");
        let mut e = PROCESSENTRY32W {
            dwSize: core::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let exe = String::from_utf16_lossy(
                    &e.szExeFile[..e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len())],
                );
                if exe.eq_ignore_ascii_case("SodaMusic.exe") {
                    out.push(e.th32ProcessID);
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 窗口枚举

struct Info {
    hwnd: HWND,
    pid: u32,
    class: String,
    title: String,
    rect: (i32, i32, i32, i32),
    visible: bool,
    enabled: bool,
    iconic: bool,
    toolwin: bool,
    noactivate: bool,
    cloaked: u32,
    style: u32,
    exstyle: u32,
}

fn dump(w: &Info) {
    println!(
        "hwnd=0x{:X} pid={} class={:?} title={:?}",
        w.hwnd.0 as usize, w.pid, w.class, w.title
    );
    println!(
        "    rect=({},{},{},{}) size={}x{} visible={} enabled={} iconic={} toolwin={} noactivate={} cloaked={} style=0x{:X} ex=0x{:X}",
        w.rect.0, w.rect.1, w.rect.2, w.rect.3,
        w.rect.2 - w.rect.0, w.rect.3 - w.rect.1,
        w.visible as u8, w.enabled as u8, w.iconic as u8,
        w.toolwin as u8, w.noactivate as u8, w.cloaked, w.style, w.exstyle,
    );
}

fn probe(hwnd: HWND) -> Option<Info> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut class = [0u16; 256];
        let cn = GetClassNameW(hwnd, &mut class);
        let class = String::from_utf16_lossy(&class[..cn.max(0) as usize]);
        let mut title_buf = [0u16; 512];
        let tn = GetWindowTextW(hwnd, &mut title_buf);
        let title = String::from_utf16_lossy(&title_buf[..tn.max(0) as usize]);
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let st = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            4,
        );
        Some(Info {
            hwnd,
            pid,
            class,
            title,
            rect: (r.left, r.top, r.right, r.bottom),
            visible: IsWindowVisible(hwnd).as_bool(),
            enabled: IsWindowEnabled(hwnd).as_bool(),
            iconic: IsIconic(hwnd).as_bool(),
            toolwin: ex & WS_EX_TOOLWINDOW.0 != 0,
            noactivate: ex & WS_EX_NOACTIVATE.0 != 0,
            cloaked,
            style: st,
            exstyle: ex,
        })
    }
}

fn enum_windows_of(pids: &[u32]) -> Vec<Info> {
    struct Ctx<'a> {
        pids: &'a [u32],
        out: Vec<Info>,
    }
    let mut ctx = Ctx { pids, out: Vec::new() };
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(enum_proc),
            LPARAM(&mut ctx as *mut Ctx as isize),
        );
    }
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if ctx.pids.contains(&pid) {
            if let Some(info) = probe(hwnd) {
                ctx.out.push(info);
            }
        }
        BOOL(1)
    }
    ctx.out
}

fn enum_all() {
    let pids = soda_pids();
    println!("SodaMusic pids = {pids:?}");
    if pids.is_empty() {
        println!("汽水音乐未运行。");
        return;
    }
    let wins = enum_windows_of(&pids);
    println!("顶层窗口共 {} 个：", wins.len());
    for w in &wins {
        dump(w);
    }
    // launch.rs 打分（排除 toolwin/cloaked/Barline）
    println!("\n---- launch.rs 打分结果 ----");
    let mut best: Option<(&Info, i32)> = None;
    for w in &wins {
        if w.toolwin || w.cloaked != 0 || w.class.starts_with("Barline") {
            continue;
        }
        let mut score = 0;
        if w.visible { score += 2; }
        if !w.title.is_empty() { score += 4; }
        let (x0, y0, x1, y1) = w.rect;
        if x1 - x0 >= 200 && y1 - y0 >= 120 { score += 2; }
        // GW_OWNER 省略（诊断用，影响很小）
        println!("hwnd=0x{:X} score={score} visible={} title={:?}", w.hwnd.0 as usize, w.visible as u8, w.title);
        if best.map(|(_, bs)| bs < score).unwrap_or(true) {
            best = Some((w, score));
        }
    }
    match best {
        Some((w, s)) => println!(">> launch.rs 会选：hwnd=0x{:X}（score={s}）", w.hwnd.0 as usize),
        None => println!(">> 无候选窗口"),
    }
}

// ---------------------------------------------------------------- 模拟用户点击

/// 模拟真实用户点击汽水窗口客户区（顶部中央，避开一切按钮），
/// 观察点击能否激活窗口——能激活 = 输入正常；不能 = 卡死实锤。
fn click_test() {
    let pids = soda_pids();
    let wins = enum_windows_of(&pids);
    let Some(w) = wins.iter().find(|w| w.class == "Chrome_WidgetWin_1" && !w.title.is_empty()) else {
        println!("找不到汽水主窗口");
        return;
    };
    let hwnd = w.hwnd;
    println!("目标：hwnd=0x{:X} visible={} fg={}", hwnd.0 as usize, w.visible as u8, unsafe { GetForegroundWindow() } == hwnd);

    // 客户区顶部中央（标题栏下方 60px）——搜索栏/导航空白区，无按钮
    let (x0, y0, x1, _) = w.rect;
    let px = (x0 + x1) / 2;
    let py = y0 + 60;
    println!("点击坐标 = ({px},{py})");

    // 主屏坐标 → 绝对坐标归一化（0..65535）
    let sx = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) };
    let sy = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) };
    let ax = (px * 65535) / sx.max(1);
    let ay = (py * 65535) / sy.max(1);

    let mi = |flags: MOUSE_EVENT_FLAGS| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: ax,
                dy: ay,
                mouseData: 0,
                dwFlags: flags | MOUSEEVENTF_ABSOLUTE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let _ = SendInput(&[mi(MOUSEEVENTF_MOVE)], core::mem::size_of::<INPUT>() as i32);
        std::thread::sleep(std::time::Duration::from_millis(120));
        let _ = SendInput(&[mi(MOUSEEVENTF_LEFTDOWN)], core::mem::size_of::<INPUT>() as i32);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = SendInput(&[mi(MOUSEEVENTF_LEFTUP)], core::mem::size_of::<INPUT>() as i32);
    }
    println!("已发送点击。");
    for ms in [300u64, 700, 1200] {
        std::thread::sleep(std::time::Duration::from_millis(if ms == 300 { 300 } else if ms == 700 { 400 } else { 500 }));
        let now_fg = unsafe { GetForegroundWindow() };
        let is_target = now_fg == hwnd;
        let gti_desc = {
            let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
            let mut gti = GUITHREADINFO {
                cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetGUIThreadInfo(tid, &mut gti) }.is_ok() {
                format!("active=0x{:X} focus=0x{:X}", gti.hwndActive.0 as usize, gti.hwndFocus.0 as usize)
            } else {
                "GUITHREADINFO 失败".into()
            }
        };
        println!("+{ms}ms 前台是汽水={is_target}（{}）{gti_desc}", fg_desc());
    }
}

// ---------------------------------------------------------------- 修复算法验证

/// 候选激活策略（按序轮换）：
/// 0. 裸 SetForegroundWindow
/// 1-2. ALT 键解锁前台锁 + SetForegroundWindow
/// 3-4. AttachThreadInput + SetFocus/SetActiveWindow + SetForegroundWindow
/// 5. SwitchToThisWindow
/// 每次都 sleep 后验证前台真实转移，成功立即返回。
fn try_activate(hwnd: HWND) -> bool {
    let target_tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    let self_tid = unsafe { GetCurrentThreadId() };
    let vk_menu = 0x12u8; // VK_MENU (ALT)
    unsafe {
        for step in 0..6u32 {
            match step {
                1 | 2 => {
                    // ALT down/up：让系统认为刚有用户输入，绕过前台锁
                    windows::Win32::UI::Input::KeyboardAndMouse::keybd_event(vk_menu, 0, windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_EXTENDEDKEY, 0);
                    windows::Win32::UI::Input::KeyboardAndMouse::keybd_event(vk_menu, 0, windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP, 0);
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
                3 | 4 => {
                    if target_tid != 0 && target_tid != self_tid {
                        let _ = AttachThreadInput(self_tid, target_tid, true);
                    }
                    let _ = SetFocus(Some(hwnd));
                    let _ = SetActiveWindow(hwnd);
                }
                5 => {
                    SwitchToThisWindow(hwnd, false);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    if GetForegroundWindow() == hwnd { return true; }
                    continue;
                }
                _ => {}
            }
            let _ = SetForegroundWindow(hwnd);
            if target_tid != 0 && target_tid != self_tid && (step == 3 || step == 4) {
                let _ = AttachThreadInput(self_tid, target_tid, false);
            }
            std::thread::sleep(std::time::Duration::from_millis(180));
            if GetForegroundWindow() == hwnd {
                println!("  >> 第 {step} 号策略成功");
                return true;
            }
        }
    }
    false
}

fn fix_test() {
    let pids = soda_pids();
    let wins = enum_windows_of(&pids);
    let Some(w) = wins.iter().find(|w| w.class == "Chrome_WidgetWin_1" && !w.title.is_empty()) else {
        println!("找不到汽水主窗口");
        return;
    };
    let hwnd = w.hwnd;
    println!("目标：hwnd=0x{:X}", hwnd.0 as usize);

    // 1. 复原托盘态
    if w.visible {
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("[1] 已 SW_HIDE 复原托盘态");
    }
    // 2. 前台交给 Barline（最严苛：diag 无前台权限）
    unsafe {
        let mut barline: Option<HWND> = None;
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(find_barline),
            LPARAM(&mut barline as *mut Option<HWND> as isize),
        );
        unsafe extern "system" fn find_barline(h: HWND, lp: LPARAM) -> BOOL {
            let slot = &mut *(lp.0 as *mut Option<HWND>);
            let mut cls = [0u16; 64];
            let n = unsafe { GetClassNameW(h, &mut cls) };
            if n > 0 {
                let s = String::from_utf16_lossy(&cls[..n as usize]);
                if s.starts_with("Barline") && unsafe { IsWindowVisible(h).as_bool() } {
                    *slot = Some(h);
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
        if let Some(b) = barline {
            SwitchToThisWindow(b, false);
            std::thread::sleep(std::time::Duration::from_millis(600));
            println!("[2] 前台交给 Barline：{}", fg_desc());
        }
    }
    // 3. SW_RESTORE 显示
    let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
    std::thread::sleep(std::time::Duration::from_millis(100));
    println!("[3] 已 SW_RESTORE 显示，前台 = {}", fg_desc());

    // 4. 轮换策略激活
    println!("[4] 轮换策略激活……");
    let ok = try_activate(hwnd);
    println!("[4] 结果 = {ok}，前台 = {}", fg_desc());

    // 5. 点击验证可交互
    if ok {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let (x0, y0, x1, _) = probe(hwnd).map(|p| p.rect).unwrap_or((0, 0, 0, 0));
        let px = (x0 + x1) / 2;
        let py = y0 + 60;
        let sx = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) };
        let sy = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) };
        let ax = (px * 65535) / sx.max(1);
        let ay = (py * 65535) / sy.max(1);
        let mi = |flags: MOUSE_EVENT_FLAGS| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: ax, dy: ay, mouseData: 0,
                    dwFlags: flags | MOUSEEVENTF_ABSOLUTE,
                    time: 0, dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            let _ = SendInput(&[mi(MOUSEEVENTF_MOVE)], core::mem::size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(120));
            let _ = SendInput(&[mi(MOUSEEVENTF_LEFTDOWN)], core::mem::size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(60));
            let _ = SendInput(&[mi(MOUSEEVENTF_LEFTUP)], core::mem::size_of::<INPUT>() as i32);
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
        let still = unsafe { GetForegroundWindow() } == hwnd;
        println!("[5] 点击后前台仍是汽水 = {still}（点击未把前台抢走 = 激活真实生效）");
    }
}

// ---------------------------------------------------------------- 激活序列

fn fg() -> usize {
    unsafe { GetForegroundWindow().0 as usize }
}

fn fg_desc() -> String {
    let h = unsafe { GetForegroundWindow() };
    if h.0 as usize == 0 {
        return "0x0（无）".into();
    }
    match probe(h) {
        Some(w) => format!(
            "0x{:X} pid={} {:?} {:?}",
            w.hwnd.0 as usize, w.pid, w.class, w.title
        ),
        None => format!("0x{:X}（查询失败）", h.0 as usize),
    }
}

fn hwnd_info(tag: &str, hwnd: HWND) {
    if let Some(w) = probe(hwnd) {
        println!(
            "[{tag}] visible={} enabled={} iconic={} fg={} rect=({},{},{},{})",
            w.visible as u8, w.enabled as u8, w.iconic as u8,
            fg() == hwnd.0 as usize, w.rect.0, w.rect.1, w.rect.2, w.rect.3,
        );
    } else {
        println!("[{tag}] 窗口已失效");
    }
}

fn activate() {
    let pids = soda_pids();
    println!("SodaMusic pids = {pids:?}");
    if pids.is_empty() {
        println!("汽水音乐未运行。");
        return;
    }
    let wins = enum_windows_of(&pids);
    // 照 launch.rs 打分选主窗口
    let mut best: Option<(&Info, i32)> = None;
    for w in &wins {
        if w.toolwin || w.cloaked != 0 || w.class.starts_with("Barline") {
            continue;
        }
        let mut score = 0;
        if w.visible { score += 2; }
        if !w.title.is_empty() { score += 4; }
        let (x0, y0, x1, y1) = w.rect;
        if x1 - x0 >= 200 && y1 - y0 >= 120 { score += 2; }
        if best.map(|(_, bs)| bs < score).unwrap_or(true) {
            best = Some((w, score));
        }
    }
    let Some((target, _)) = best else {
        println!("无候选主窗口，退出");
        return;
    };
    let hwnd = target.hwnd;
    println!("目标：hwnd=0x{:X} class={:?} title={:?}", hwnd.0 as usize, target.class, target.title);

    // 复原托盘态（模拟「最小化到托盘」）：可见就先藏起来
    if target.visible {
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("\n[预备] 窗口原本可见 → 已 SW_HIDE 复原托盘态，500ms 后重查");
        let now = probe(hwnd).map(|w| w.visible).unwrap_or(false);
        println!("[预备] 复原后 visible={}", now as u8);
    }

    // 贴近真实场景：若前台恰好是汽水（隐藏窗口当前台的异常态），把前台交给 Barline；
    // 否则保持当前前台不动（WorkBuddy/终端等，= 第一轮实验同款场景）
    unsafe {
        if GetForegroundWindow() == hwnd {
            let mut barline: Option<HWND> = None;
            let _ = windows::Win32::UI::WindowsAndMessaging::EnumWindows(
                Some(find_barline),
                LPARAM(&mut barline as *mut Option<HWND> as isize),
            );
            unsafe extern "system" fn find_barline(h: HWND, lp: LPARAM) -> BOOL {
                let slot = &mut *(lp.0 as *mut Option<HWND>);
                let mut cls = [0u16; 64];
                let n = unsafe { GetClassNameW(h, &mut cls) };
                if n > 0 {
                    let s = String::from_utf16_lossy(&cls[..n as usize]);
                    if s.starts_with("Barline") && unsafe { IsWindowVisible(h).as_bool() } {
                        *slot = Some(h);
                        return BOOL(0);
                    }
                }
                BOOL(1)
            }
            match barline {
                Some(b) => {
                    SwitchToThisWindow(b, false);
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    println!("[预备] 前台已交给 Barline 小组件 0x{:X}（{}）", b.0 as usize, fg_desc());
                }
                None => println!("[预备] 找不到 Barline 窗口，前台保持 {}", fg_desc()),
            }
        } else {
            println!("[预备] 前台保持 {}", fg_desc());
        }
    }

    println!("\n== 步骤 0：激活前状态 ==");
    hwnd_info("前", hwnd);
    println!("当前前台 = 0x{:X}", fg());

    println!("\n== 步骤 1：IsIconic / 不可见 → SW_RESTORE ==");
    unsafe {
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            let r = ShowWindow(hwnd, SW_RESTORE);
            println!("ShowWindow(SW_RESTORE) 已调用（旧状态 visible={}）", r.as_bool() as u8);
        } else {
            println!("窗口已可见且未最小化，跳过 ShowWindow");
        }
    }
    hwnd_info("SW_RESTORE 后", hwnd);

    println!("\n== 步骤 2：IsWindowEnabled 检查 ==");
    unsafe {
        if !IsWindowEnabled(hwnd).as_bool() {
            let _ = EnableWindow(hwnd, true);
            println!("窗口原本被禁用 → 已 EnableWindow(true)");
        } else {
            println!("窗口未禁用，无需处理");
        }
    }

    println!("\n== 步骤 3：SetForegroundWindow（返回 true 也要验证前台真变了）==");
    let target_tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    let self_tid = unsafe { GetCurrentThreadId() };
    println!("target_tid={target_tid} self_tid={self_tid}");

    // v1.1.18 缺陷验证：SetForegroundWindow 返回 true ≠ 前台真的切换
    let ok1 = unsafe { SetForegroundWindow(hwnd) }.as_bool();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let real1 = unsafe { GetForegroundWindow() } == hwnd;
    println!("第 1 次 SetForegroundWindow = {ok1}，300ms 后前台 = {}", fg_desc());

    let mut ok = real1;
    if !ok {
        for attempt in 1..=3u32 {
            unsafe {
                if target_tid != 0 && target_tid != self_tid {
                    let _ = AttachThreadInput(self_tid, target_tid, true);
                }
                let _ = SetFocus(Some(hwnd));
                let _ = SetActiveWindow(hwnd);
                ok = SetForegroundWindow(hwnd).as_bool();
                if target_tid != 0 && target_tid != self_tid {
                    let _ = AttachThreadInput(self_tid, target_tid, false);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            let real = unsafe { GetForegroundWindow() } == hwnd;
            println!("attach 第 {attempt} 次 SetForegroundWindow = {ok}，300ms 后前台生效={real}（{}）", fg_desc());
            ok = real;
            if ok { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    if !ok {
        println!("\n== 步骤 3b：SwitchToThisWindow 兜底 ==");
        unsafe { SwitchToThisWindow(hwnd, false) };
        std::thread::sleep(std::time::Duration::from_millis(600));
        let real = unsafe { GetForegroundWindow() } == hwnd;
        println!("SwitchToThisWindow 后前台生效={real}（{}）", fg_desc());
    }

    println!("\n== 步骤 4：前台流动观察（300/800/1500/2500ms）==");
    for ms in [300u64, 500, 700, 1000] {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        let now = unsafe { GetForegroundWindow() } == hwnd;
        println!("+{ms}ms 前台是目标={now}（{}）", fg_desc());
    }
    hwnd_info("终态", hwnd);

    // 焦点 / 消息泵活性
    unsafe {
        let mut gti = GUITHREADINFO {
            cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(target_tid, &mut gti).is_ok() {
            println!(
                "GetGUIThreadInfo: hwndFocus=0x{:X} hwndCapture=0x{:X} hwndActive=0x{:X}",
                gti.hwndFocus.0 as usize, gti.hwndCapture.0 as usize, gti.hwndActive.0 as usize
            );
        } else {
            println!("GetGUIThreadInfo 失败");
        }
        let mut reply: usize = 0;
        let r = SendMessageTimeoutW(
            hwnd, WM_NULL, WPARAM(0), LPARAM(0),
            SMTO_ABORTIFHUNG, 800, Some(&mut reply),
        );
        println!("SendMessageTimeout(WM_NULL, 800ms) = {}（0=超时，消息队列挂起）", (r.0 != 0) as u8);
    }

    println!("\n诊断序列执行完毕。请现在目视确认汽水窗口：能拖动吗？内容能点吗？");
}
