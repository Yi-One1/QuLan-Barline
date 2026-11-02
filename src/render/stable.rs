//! 背景亮度的**三层稳定化**（v1.2.0 / G5 问题 3）。
//!
//! 背景只有一个标量（加权相对亮度）喂给「深色字还是浅色字」，而
//! `Theme` 的择优交叉点在 luma ≈ 0.179。动态壁纸 / 视频壁纸的亮度会在
//! 交叉点附近抖动，**每秒重采样 + 单点硬切**就会让文字色每秒翻一次
//! （用户看到的是「黑白闪烁」）。这里加三层防护：
//!
//! 1. **EMA 平滑**——单帧噪声被压掉（α = [`limits::LUMA_EMA_ALPHA`]）。
//! 2. **双阈值死区**——`ema >= LUMA_LIGHT_MIN` 才算浅色背景、
//!    `ema <= LUMA_DARK_MAX` 才算深色背景，中间（0.12..0.25）保持现状。
//! 3. **连续确认**——要连续 [`limits::LUMA_CONFIRM_SAMPLES`] 个样本都指向新值
//!    才真的翻转（1s 心跳下约 2 秒），避免「刚好抖过去一格」就翻。
//!
//! 本模块是**纯逻辑**，不碰 Win32、不碰线程，可以单独单测。
//! 状态由采样线程 [`crate::render::sampler::run`] 跨样本持有。
//!
//! ## 几何代次（epoch）
//!
//! 采样区域变了（窗口移动 / 尺寸变化 / DPI 变化），旧 EMA 说的其实是
//! **另一块屏幕区域**的亮度，继续拿它做平均是错的。所以 `epoch` 变化时
//! 整台状态归零、按「首帧规则」重新判定——宁可当帧就翻，也不要拖着旧区域
//! 的亮度污染新区域（`App::relayout` 之后紧接着就会重采样）。

use crate::config::defaults::limits;
use crate::render::theme::pick_on_light;

/// 亮度稳定器。跨样本持有状态，**不分配、不 panic**。
#[derive(Debug, Clone)]
pub struct LumaStabilizer {
    /// 指数滑动平均；`None` = 还没采过样
    ema: Option<f32>,
    /// 稳定后的结论；`None` = 还没采过样
    on_light: Option<bool>,
    /// 与当前结论冲突的**连续**样本数
    confirm: u32,
    /// 上次样本的几何代次（变化即重置）
    epoch: u64,
}

impl Default for LumaStabilizer {
    fn default() -> Self {
        Self::new()
    }
}

impl LumaStabilizer {
    /// 全空状态：第一次 [`push`](Self::push) 走「首帧规则」。
    pub const fn new() -> Self {
        Self {
            ema: None,
            on_light: None,
            confirm: 0,
            epoch: 0,
        }
    }

    /// 当前稳定结论（诊断 / 断言用）。`None` 表示还没采过样。
    pub fn on_light(&self) -> Option<bool> {
        self.on_light
    }

    /// 当前 EMA（诊断日志用）。`None` 表示还没采过样。
    pub fn ema(&self) -> Option<f32> {
        self.ema
    }

    /// 当前累积的冲突计数（诊断 / 断言用）。
    pub fn confirm(&self) -> u32 {
        self.confirm
    }

    /// 喂一个样本，返回**稳定后**的 `on_light`（`true` = 浅色背景 → 深色文字）。
    ///
    /// `epoch` 是几何代次；与上一次不同则先重置再按首帧规则判定。
    pub fn push(&mut self, luma: f32, epoch: u64) -> bool {
        // 防御：NaN / inf / 越界一律钳到合法区间，绝不让脏值进 EMA
        //（一次 NaN 会让 EMA 永久变成 NaN，文字色彻底卡死）。
        // 注意 `is_nan` 而不是 `is_finite`：±inf 是可排序的，直接 clamp 到 0/1
        // 更符合直觉；只有 NaN 没有大小关系，才需要单独兜底。
        let raw = if luma.is_nan() {
            0.0
        } else {
            luma.clamp(0.0, 1.0)
        };

        // ---- 首帧 / 几何代次变化：不做平滑，直接按对比度择优 ----
        if self.ema.is_none() || self.epoch != epoch {
            self.ema = Some(raw);
            self.on_light = Some(pick_on_light(raw));
            self.confirm = 0;
            self.epoch = epoch;
            return self.on_light.unwrap_or(false);
        }

        // ---- 后续样本：EMA ----
        let prev = self.ema.unwrap_or(raw);
        let alpha = limits::LUMA_EMA_ALPHA.clamp(0.0, 1.0);
        let ema = alpha * raw + (1.0 - alpha) * prev;
        self.ema = Some(ema);

        let current = self.on_light.unwrap_or_else(|| pick_on_light(ema));

        // ---- 双阈值 + 死区：死区里 want 就是 current（不产生冲突计数）----
        let want = if ema >= limits::LUMA_LIGHT_MIN {
            true
        } else if ema <= limits::LUMA_DARK_MAX {
            false
        } else {
            current
        };

        if want == current {
            self.confirm = 0;
            return current;
        }

        // ---- 连续确认：攒够才翻 ----
        self.confirm = self.confirm.saturating_add(1);
        if self.confirm >= limits::LUMA_CONFIRM_SAMPLES.max(1) {
            self.on_light = Some(want);
            self.confirm = 0;
            return want;
        }
        current
    }

    /// 显式重置（等价于「下一个样本走首帧规则」，但代次沿用现值）。
    pub fn reset(&mut self) {
        self.ema = None;
        self.on_light = None;
        self.confirm = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 深色壁纸亮度（≈ 纯黑）
    const DARK: f32 = 0.02;
    /// 浅色壁纸亮度（≈ 纯白）
    const LIGHT: f32 = 0.90;
    /// 死区中段的抖动样本
    const JITTER_A: f32 = 0.17;
    const JITTER_B: f32 = 0.19;

    /// 1. 抖动不翻转：在死区里来回抖 20 次，一次都不许变。
    #[test]
    fn jitter_inside_dead_zone_never_flips() {
        let mut s = LumaStabilizer::new();
        let first = s.push(JITTER_A, 1);
        // 首帧按对比度择优（0.17 这一档浅色字更可读），之后必须锁死
        for i in 0..20 {
            let luma = if i % 2 == 0 { JITTER_B } else { JITTER_A };
            let got = s.push(luma, 1);
            assert_eq!(got, first, "死区内第 {i} 次抖动把文字色翻了");
        }
        assert_eq!(s.on_light(), Some(first));
    }

    /// 2. 真实切换要**连续确认**：首帧深色，然后连续给亮背景——
    /// 第 1 次绝不翻（单样本翻转就是闪烁的根因），第 2 次才翻（≈2 秒）。
    #[test]
    fn real_switch_needs_two_confirmations() {
        let mut s = LumaStabilizer::new();
        // 从死区下沿起步（典型的「深色壁纸」亮度）
        let first = s.push(limits::LUMA_DARK_MAX, 1);
        assert!(!first, "0.12 应判为深色背景（浅色文字）");

        let a = s.push(LIGHT, 1);
        assert!(!a, "第 1 个亮样本不许翻转（单样本翻转 = 每秒闪烁）");
        assert!(s.confirm() >= 1, "第 1 个冲突样本应计入 confirm");

        let b = s.push(LIGHT, 1);
        assert!(b, "第 2 个亮样本应完成翻转（≈2 秒）");
        assert_eq!(s.confirm(), 0, "翻转后 confirm 必须清零");
    }

    /// 2b. 极端起步（接近纯黑）时 EMA 收敛更慢，需要第 3 个样本——
    /// 这里把真实行为钉住，防止有人以为「两个样本必然翻」而误改参数。
    #[test]
    fn deep_dark_start_needs_three_samples() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 1), "0.02 应判为深色背景");
        assert!(!s.push(LIGHT, 1), "第 1 个亮样本不许翻");
        // ema ≈ 0.24 —— 还在死区里，这一帧连冲突都不计
        assert!(!s.push(LIGHT, 1), "EMA 尚未爬出死区时不许翻");
        assert!(s.push(LIGHT, 1), "EMA 出死区 + 攒够确认后应翻转");
    }

    /// 3. 死区保持：当前深色，给死区内的值任意多次都不翻。
    #[test]
    fn dead_zone_holds_current_choice() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 1));
        for _ in 0..10 {
            assert!(!s.push(0.18, 1), "死区内的样本把深色翻成了浅色");
        }
        // 连「确认计数」都不该攒，否则一离开死区就会瞬间翻
        assert_eq!(s.confirm(), 0, "死区样本不该计入 confirm");
    }

    /// 4. 首帧立即判定：第一个样本就要给出结论，不能返回默认值。
    #[test]
    fn first_sample_decides_immediately() {
        let mut s = LumaStabilizer::new();
        assert!(s.push(LIGHT, 1), "首帧亮背景必须立刻判为浅色");
        assert_eq!(s.on_light(), Some(true));

        let mut d = LumaStabilizer::new();
        assert!(!d.push(DARK, 1), "首帧暗背景必须立刻判为深色");
        assert_eq!(d.on_light(), Some(false));
    }

    /// 5. 几何代次变化 → 清空 EMA 与确认计数，按首帧规则重判。
    #[test]
    fn epoch_change_resets_state() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 1));
        assert!(!s.push(LIGHT, 1), "同一代次内第 1 个亮样本不许翻");
        assert!(s.ema().is_some());

        // 换了采样区域：新区域的第一个样本立刻生效
        assert!(s.push(LIGHT, 2), "epoch 变化后应立刻按首帧规则判定");
        assert_eq!(s.epoch, 2);
        assert_eq!(s.confirm(), 0);
    }

    /// 防御：NaN / inf 不许污染 EMA（否则 EMA 永久 NaN，文字色卡死）。
    #[test]
    fn non_finite_luma_is_sanitised() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(f32::NAN, 1), "NaN 应被当成 0（深色）");
        assert_eq!(s.ema(), Some(0.0), "NaN 不该写进 EMA");

        // inf 钳到 1.0 → EMA 应等于 α*1.0（有限值），不能是 inf / NaN
        let got = s.push(f32::INFINITY, 1);
        let ema = s.ema().unwrap_or(-1.0);
        assert!(ema.is_finite(), "inf 污染了 EMA：{ema}");
        assert!(
            (ema - limits::LUMA_EMA_ALPHA).abs() < 1e-6,
            "inf 应被钳成 1.0：ema = {ema}"
        );
        // EMA 一次就冲到 0.25（浅色阈值），但单样本不许翻转
        assert!(!got, "单样本不许翻转文字色");
    }

    /// `reset()` 之后下一个样本走首帧规则。
    #[test]
    fn reset_restores_first_sample_rule() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 7));
        s.reset();
        assert!(s.push(LIGHT, 7), "reset 后首个样本应立刻判定");
    }
}
