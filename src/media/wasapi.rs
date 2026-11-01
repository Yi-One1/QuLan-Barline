//! WASAPI：系统音量读写（T16 的先行部分，T08 的滚轮 HUD 需要它）。
//!
//! ## 为什么现在就要
//!
//! T08 的验收项写着「滚轮：音量按步进变化，HUD 浮现」。如果只改内存里的数字，
//! 那是假实现。所以这里先把**系统主音量**的读写落地（约 90 行），
//! T16 再补「仅当前播放器」的 `ISimpleAudioVolume` 路径与静音图标。
//!
//! ## 线程
//!
//! `IAudioEndpointVolume` 的 `SetMasterVolumeLevelScalar` / `GetMasterVolumeLevelScalar`
//! 是同步 COM 调用，在 UI 线程（STA）直接调是安全的——不涉及 `IAsyncOperation`。
//! 单次耗时 < 1ms，不会卡消息泵。

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::core::Interface;

/// 解析 `"{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}"` 形态的会话 GUID。
///
/// windows-core 0.62 没实现 `GUID: FromStr`，按标准十六进制串转 u128 再
/// `from_u128`（与连字符串的位形一一对应）。
fn parse_guid(s: &str) -> Option<windows::core::GUID> {
    let hex: String = s
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let raw = u128::from_str_radix(&hex, 16).ok()?;
    Some(windows::core::GUID::from_u128(raw))
}

/// 取默认渲染设备的音量接口。失败返回 `None`（调用方退化为只更新 HUD）。
fn endpoint_volume() -> Option<IAudioEndpointVolume> {
    // SAFETY: 所有 COM 对象在函数作用域内释放；CLSID 是系统注册的进程内组件。
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        device.Activate(CLSCTX_ALL, None).ok()
    }
}

/// 读系统主音量 0.0..=1.0。失败返回 `None`。
pub fn system_volume() -> Option<f32> {
    let vol = endpoint_volume()?;
    // SAFETY: 纯读取标量。
    unsafe { vol.GetMasterVolumeLevelScalar().ok() }
}

/// 写系统主音量（0.0..=1.0，内部钳位）。返回是否成功。
pub fn set_system_volume(v: f32) -> bool {
    let v = v.clamp(0.0, 1.0);
    let Some(vol) = endpoint_volume() else {
        crate::log_debug!("拿不到 IAudioEndpointVolume，音量未改变");
        return false;
    };
    // SAFETY: 第二参数是「事件上下文」指针，传 null 表示不需要通知来源。
    match unsafe { vol.SetMasterVolumeLevelScalar(v, core::ptr::null()) } {
        Ok(()) => {
            crate::log_debug!("系统音量 → {:.0}%", v * 100.0);
            true
        }
        Err(e) => {
            crate::log_warn!("设置系统音量失败：{e}");
            false
        }
    }
}

/// 读系统静音状态。
pub fn system_muted() -> Option<bool> {
    let vol = endpoint_volume()?;
    // SAFETY: 纯读取布尔标量。
    unsafe { vol.GetMute().ok().map(|b| b.as_bool()) }
}

/// 设置系统静音。
pub fn set_system_mute(mute: bool) -> bool {
    let Some(vol) = endpoint_volume() else {
        return false;
    };
    // SAFETY: 同上。
    unsafe { vol.SetMute(mute, core::ptr::null()) }.is_ok()
}

/// 切换系统静音，返回切换后的状态（拿不到设备时返回 `false`）。
pub fn toggle_system_mute() -> bool {
    let cur = system_muted().unwrap_or(false);
    let next = !cur;
    if set_system_mute(next) {
        crate::log_info!("系统静音 → {}", if next { "开" } else { "关" });
        next
    } else {
        crate::log_warn!("切换静音失败（可能没有默认渲染设备）");
        cur
    }
}

// ------------------------------------------------------------------ 会话音量

/// 「没有匹配会话」的限流日志：最多每 5 秒一条（滚轮会连发，不能刷屏）。
fn log_no_match_throttled(want_process: &str, want_aumid: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_MS: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(LAST_MS.load(Ordering::Relaxed)) < 5_000 {
        return;
    }
    LAST_MS.store(now, Ordering::Relaxed);
    crate::log_info!(
        "会话音量：没有匹配的音频会话（process={want_process:?} aumid={want_aumid:?}）——播放器没在出声，或映射表未收录该播放器"
    );
}

/// 找「当前播放器」的会话音量接口（v1.1.16 重写）。
///
/// 匹配规则见 [`crate::media::session_matches`]（进程名直查 + player_map
/// 反查）。三条硬规则（v1.1.15 的三个教训）：
/// 1. **会话 GUID 必须取自 `GetSessionIdentifier` 尾段**——`GetSimpleAudioVolume`
///    只认它；`GetSessionInstanceIdentifier` 给的是**实例 GUID**，传进去会
///    静默创建一个「幽灵会话」（读到 1.0、写了个寂寞）。与 EarTrumpet 同款。
/// 2. **单个会话失败只跳过该会话**——系统会话不支持 identifier 之类的情况
///    不能让整个查找提前返回 `None`（v1.1.15 的 `?` 提前返回 bug）。
/// 3. 同名会话可能多个（浏览器每个标签一个）：**优先 Active 状态**的。
fn session_simple_volume(
    want_process: &str,
    want_aumid: &str,
) -> Option<(windows::Win32::Media::Audio::ISimpleAudioVolume, f32)> {
    use windows::Win32::Media::Audio::{IAudioSessionControl2, IAudioSessionManager2};
    use windows::Win32::System::Com::CoTaskMemFree;

    // SAFETY: 所有 COM 对象在函数作用域内释放；PWSTR 由 CoTaskMemFree 归还。
    let matched: Vec<_> = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        let mgr: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
        let sessions = mgr.GetSessionEnumerator().ok()?;
        let count = sessions.GetCount().unwrap_or(0);

        let mut matched = Vec::new();
        for i in 0..count {
            let Ok(ctrl) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(ctl2) = ctrl.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let Ok(pid) = ctl2.GetProcessId() else {
                continue;
            };
            if pid == 0 || pid == std::process::id() {
                continue;
            }
            let name = crate::media::launch::process_image_name(pid);
            if !crate::media::session_matches(&name, want_process, want_aumid) {
                continue;
            }
            let active = ctrl.GetState().map(|s| s.0 == 1).unwrap_or(false);
            // identifier 取「会话 GUID」（尾段）而不是实例 GUID（教训 1）
            let Ok(pw) = ctl2.GetSessionIdentifier() else {
                crate::log_debug!("会话 pid={pid} 没有 SessionIdentifier，跳过");
                continue;
            };
            let s = if pw.0.is_null() {
                String::new()
            } else {
                let len = (0..).take_while(|&i| *pw.0.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(pw.0, len))
            };
            if !pw.0.is_null() {
                CoTaskMemFree(Some(pw.0.cast()));
            }
            let guid_str = s.rsplit('|').next().unwrap_or("").trim();
            let Some(guid) = parse_guid(guid_str) else {
                crate::log_debug!("会话 pid={pid} GUID 解析失败：{guid_str}");
                continue;
            };
            matched.push((guid, active));
        }
        matched
    };

    if matched.is_empty() {
        log_no_match_throttled(want_process, want_aumid);
        return None;
    }
    // 活跃会话优先（教训 3）
    let guid = matched
        .iter()
        .find(|(_, active)| *active)
        .or(matched.first())
        .map(|(g, _)| *g)?;

    // SAFETY: COM 对象在作用域内；GUID 来自上面匹配到的会话。
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        let mgr: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
        let vol = mgr.GetSimpleAudioVolume(Some(&guid), 0).ok()?;
        let v = vol.GetMasterVolume().ok()?;
        Some((vol, v))
    }
}

/// 「当前播放器」会话音量步进（v1.1.16 问题 6）。
///
/// 一次定位会话、读当前值、加步进、写回，返回新值；找不到匹配会话返回
/// `None`——**绝不触碰系统主音量**，调用方据此不弹 HUD。
pub fn step_session_volume(want_process: &str, want_aumid: &str, delta: f32) -> Option<f32> {
    let (vol, old) = session_simple_volume(want_process, want_aumid)?;
    let next = (old + delta).clamp(0.0, 1.0);
    // SAFETY: 事件上下文传 null 表示不需要通知来源。
    match unsafe { vol.SetMasterVolume(next, core::ptr::null()) } {
        Ok(()) => {
            let tag = if want_process.is_empty() {
                want_aumid
            } else {
                want_process
            };
            crate::log_info!("会话音量（{tag}）{:.0}% → {:.0}%", old * 100.0, next * 100.0);
            Some(next)
        }
        Err(e) => {
            crate::log_warn!("设置会话音量失败：{e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    /// 音量钳位逻辑（不触碰真实设备，保证单测在无音频设备的环境也能跑）。
    fn clamp01(v: f32) -> f32 {
        v.clamp(0.0, 1.0)
    }

    #[test]
    fn volume_clamps_to_unit_range() {
        assert_eq!(clamp01(-0.5), 0.0);
        assert_eq!(clamp01(1.5), 1.0);
        assert_eq!(clamp01(0.42), 0.42);
    }
}
