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

/// 找「当前播放器」的会话音量接口并读当前值（v1.1.15 问题 6）。
///
/// 匹配规则见 [`crate::media::session_matches`]（进程名直查 + player_map
/// 反查）。`ISimpleAudioVolume` 必须按**会话 GUID** 从
/// `IAudioSessionManager2::GetSimpleAudioVolume` 取，GUID 是
/// `IAudioSessionControl2::GetSessionInstanceIdentifier` 返回串的最后一段
/// （`…|{GUID}`，与 EarTrumpet 同款做法）。
fn session_simple_volume(
    want_process: &str,
    want_aumid: &str,
) -> Option<(windows::Win32::Media::Audio::ISimpleAudioVolume, f32)> {
    use windows::Win32::Media::Audio::{IAudioSessionControl2, IAudioSessionManager2};
    use windows::Win32::System::Com::CoTaskMemFree;

    // SAFETY: 所有 COM 对象在函数作用域内释放；PWSTR 由 CoTaskMemFree 归还。
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        let mgr: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
        let sessions = mgr.GetSessionEnumerator().ok()?;
        let count = sessions.GetCount().unwrap_or(0);

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
            let pw = ctl2.GetSessionInstanceIdentifier().ok()?;
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
                crate::log_debug!("会话 GUID 解析失败：{guid_str}");
                continue;
            };
            let vol = mgr.GetSimpleAudioVolume(Some(&guid), 0).ok()?;
            let v = vol.GetMasterVolume().ok()?;
            return Some((vol, v));
        }
    }
    None
}

/// 读「当前播放器」的会话音量（0.0..=1.0）。找不到匹配会话返回 `None`。
pub fn session_volume(want_process: &str, want_aumid: &str) -> Option<f32> {
    session_simple_volume(want_process, want_aumid).map(|(_, v)| v)
}

/// 写「当前播放器」的会话音量。找不到匹配会话返回 `false`（**不改系统音量**）。
pub fn set_session_volume(want_process: &str, want_aumid: &str, v: f32) -> bool {
    let v = v.clamp(0.0, 1.0);
    match session_simple_volume(want_process, want_aumid) {
        Some((vol, old)) => {
            // SAFETY: 事件上下文传 null 表示不需要通知来源。
            match unsafe { vol.SetMasterVolume(v, core::ptr::null()) } {
                Ok(()) => {
                    crate::log_info!("会话音量（{want_process}）{:.0}% → {:.0}%", old * 100.0, v * 100.0);
                    true
                }
                Err(e) => {
                    crate::log_warn!("设置会话音量失败：{e}");
                    false
                }
            }
        }
        None => {
            crate::log_debug!("没有匹配当前播放器的音频会话，会话音量未改变");
            false
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
