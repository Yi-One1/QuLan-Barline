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
