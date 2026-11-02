//! 背景亮度的**三层稳定化**（v1.2.0 / G5 问题 3）。
//!
//! 背景只有一个标量（加权相对亮度）喂给「深色字还是浅色字」，而
//! `Theme` 的择优交叉点在 luma ≈ 0.179。动态壁纸 / 视频壁纸的亮度会在
//! 交叉点附近抖动，**每秒重采样 + 单点硬切**就会让文字色每秒翻一次
//! （用户看到的是「黑白闪烁」）。三层防护，全部作用在**原始亮度 `raw`** 上：
//!
//! 1. **双阈值死区**——`raw >= LUMA_LIGHT_MIN`（0.25）才算浅色背景、
//!    `raw <= LUMA_DARK_MAX`（0.12）才算深色背景；中间（0.12..0.25）保持现状，
//!    并且**不累计确认计数**（否则一离开死区就会瞬间翻）。
//! 2. **连续确认**——要连续 [`limits::LUMA_CONFIRM_SAMPLES`] 个样本都指向新值
//!    才真的翻转（1s 心跳下 2 秒），避免「刚好抖过去一格」就翻。
//! 3. **大幅跳变快速通道**——原始亮度离阈值足够远（越过
//!    [`limits::LUMA_FAST_MARGIN`]）时**当帧就翻**；但带一道**反向冷却闸**
//!    （[`limits::LUMA_FAST_COOLDOWN_SAMPLES`]），见下文"动态壁纸"一节。
//!
//! 本模块是**纯逻辑状态机**：不碰 Win32、不碰线程、**不含任何浮点收敛过程**。
//! 同一输入序列必然得到同一输出（幂等 / 可重放），因此可以单独单测。
//! 状态由采样线程 [`crate::render::sampler::run`] 跨样本持有。
//!
//! ## 为什么**不要** EMA
//!
//! v1.2.0 最初版本是「EMA(α=0.25) + 死区 + 连续确认 + 快速通道」。实测
//! 用户切壁纸白(1.0)→黑(0.02) 要 **7 秒**，原因就是 EMA 是**渐近逼近**：
//! `0.755 → 0.571 → 0.433 → 0.330 → 0.252 → 0.194 → …`，要 11 个样本才跌出
//! 死区 0.12；反向的中间档（0.06 → 0.29）同样要 8 个样本。把 EMA 换成一个
//! 0.29 的样本，它永远差阈值一点点，**你不可能靠调确认次数救回来**。
//!
//! 更根本的问题是：**EMA 和「死区 + 连续确认」在做同一件事（抗抖）**，
//! 三者叠加等于把延迟相乘。抗抖只需要后者：真实抖动都发生在阈值附近，
//! 那里 `want == current`（死区）或方向反复（confirm 被清零），本来就翻不了；
//! 而真正的背景变化要么一步跨过阈值很远（快速通道，1 帧），要么落在确认区
//! 连续两帧同向（2 帧）。删掉 EMA 之后所有时延都是**离散、可预期**的：
//!
//! | 场景 | 采样数 |
//! |---|---|
//! | 换壁纸（越过对侧阈值 ≥ margin） | 1 |
//! | 确认区（0.07..0.12 或 0.25..0.30） | 2 |
//! | 死区（0.12..0.25） | 永不翻 |
//! | 确认区 / 死区来回抖动 | 永不翻（confirm 反复清零） |
//! | 动态壁纸逐秒横跳（0.85 ↔ 0.04） | 每 3 个样本翻一次（≈0.33 Hz） |
//!
//! ## 动态壁纸：为什么快速通道还需要反向冷却闸
//!
//! 用户明确说过"我有一些动态壁纸"。如果某个视频镜头在小组件那一小块区域里
//! 逐秒黑白闪切（`raw` 在 0.85 与 0.04 之间横跳），**两条快速通道会轮流命中**，
//! 于是每帧都翻 → 1 Hz 明暗频闪，正好把最初要修的 bug 请回来。
//!
//! 冷却闸的规则：一次快速翻转之后，`seq - last_fast_seq < LUMA_FAST_COOLDOWN_SAMPLES`
//! 期间**反向的快速翻转降级为慢速路径**（不是禁止翻转！）。慢速路径不受冷却约束，
//! 所以真实的反向切换最坏 = 冷却 + 1 个样本，绝不会被锁死。
//!
//! 效果（`COOLDOWN = 2`，1s 采样）：频闪周期从**每 1 秒**降到**每 3 秒**——
//! 见 `alternating_video_wallpaper_flips_every_three_samples`，翻转发生在
//! 第 1/4/7/10 个样本。要彻底不闪只能靠"同向连续 2 次才算真事件"，
//! 那会把换壁纸也拖成 2 帧，所以这里选择"压慢 + 保留 1 帧响应"。
//!
//! ## 第 3 层的威胁模型（为什么快速通道不会把抗抖能力还回去）
//!
//! 快速通道只在**离阈值足够远**时触发：要么 `raw <= 0.07`（深色死区下沿
//! 0.12 再退 0.05），要么 `raw >= 0.30`（浅色死区上沿 0.25 再进 0.05）。
//! 真实噪声都发生在阈值附近（0.12~0.25 那一圈），根本摸不到 0.07 以下或
//! 0.30 以上。另外两个细节：
//!
//! * 快速通道**只对有限输入生效**——`NaN` / `±inf` 是脏数据（不是大幅跳变），
//!   一律走慢速路径，避免一脸坏数据把文字色甩翻。
//! * 快速通道翻转后 `confirm` 必须清零，且 `on_light` 立刻生效——下一帧同值
//!   样本必须被死区「粘住」（`fast_flip_is_sticky` 钉这个）。
//!
//! ## 几何代次（epoch）
//!
//! 采样区域变了（窗口移动 / 尺寸变化 / DPI 变化），旧状态说的是
//! **另一块屏幕区域**的亮度，直接沿用会拖慢新区域的判定。所以 `epoch` 变化时
//! 整台状态归零、按「首帧规则」重新判定——宁可当帧就翻，也不要拖着旧区域
//! 的结论（`App::relayout` 之后紧接着就会重采样）。

use crate::config::defaults::limits;
use crate::render::theme::pick_on_light;

/// 亮度稳定器。跨样本持有状态，**不分配、不 panic、无浮点收敛过程**。
#[derive(Debug, Clone)]
pub struct LumaStabilizer {
    /// 稳定后的结论；`None` = 还没采过样
    on_light: Option<bool>,
    /// 与当前结论冲突的**连续**样本数
    confirm: u32,
    /// 上次样本的几何代次（变化即重置）
    epoch: u64,
    /// 样本序号（每次 `push` +1；冷却闸的时间基准）
    seq: u64,
    /// 上一次**快速**翻转发生在第几个样本（`None` = 还没有过，或刚被 epoch 清空）
    last_fast_seq: Option<u64>,
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
            on_light: None,
            confirm: 0,
            epoch: 0,
            seq: 0,
            last_fast_seq: None,
        }
    }

    /// 当前稳定结论（诊断 / 断言用）。`None` 表示还没采过样。
    pub fn on_light(&self) -> Option<bool> {
        self.on_light
    }

    /// 当前累积的冲突计数（诊断 / 断言用）。
    pub fn confirm(&self) -> u32 {
        self.confirm
    }

    /// 喂一个样本，返回**稳定后**的 `on_light`（`true` = 浅色背景 → 深色文字）。
    ///
    /// `epoch` 是几何代次；与上一次不同则先重置再按首帧规则判定。
    ///
    /// 判定顺序：首帧 / epoch → 快速通道（受反向冷却闸约束）→ 死区 → 连续确认。
    /// **没有 EMA**，所以同一输入序列必然得到同一输出（纯状态机）。
    pub fn push(&mut self, luma: f32, epoch: u64) -> bool {
        // 防御：NaN / inf / 越界一律钳到合法区间
        //（NaN 没有大小关系，单独兜底成 0 = 深色；±inf 可排序，直接 clamp）。
        let finite = luma.is_finite();
        let raw = if luma.is_nan() {
            0.0
        } else {
            luma.clamp(0.0, 1.0)
        };
        // 样本序号：冷却闸靠它计时间（首帧也要 +1，保证序号单调）
        self.seq = self.seq.wrapping_add(1);

        // ---- 首帧 / 几何代次变化：不做任何平滑，直接按对比度择优 ----
        if self.on_light.is_none() || self.epoch != epoch {
            self.on_light = Some(pick_on_light(raw));
            self.confirm = 0;
            self.epoch = epoch;
            // 换了采样区域，快速翻转的历史无意义（否则冷却会把新区域拖慢）
            self.last_fast_seq = None;
            return self.on_light.unwrap_or(false);
        }

        let current = self.on_light.unwrap_or(false);

        // ---- 第 3 层：大幅跳变快速通道（只对有限输入生效 + 反向冷却闸）----
        if finite {
            if let Some(flipped) = self.try_fast_flip(raw, current) {
                return flipped;
            }
        }

        // ---- 第 1 层：双阈值 + 死区（死区里 want == current，不计冲突）----
        let want = if raw >= limits::LUMA_LIGHT_MIN {
            true
        } else if raw <= limits::LUMA_DARK_MAX {
            false
        } else {
            current
        };

        if want == current {
            self.confirm = 0;
            return current;
        }

        // ---- 第 2 层：连续确认，攒够才翻 ----
        self.confirm = self.confirm.saturating_add(1);
        if self.confirm >= limits::LUMA_CONFIRM_SAMPLES.max(1) {
            self.on_light = Some(want);
            self.confirm = 0;
            return want;
        }
        current
    }

    /// 第 3 层：原始亮度**离阈值足够远**时立即翻转（真事件快速通道）。
    ///
    /// * 当前浅色背景（深色字）+ `raw <= LUMA_DARK_MAX - LUMA_FAST_MARGIN` → 转深色背景
    /// * 当前深色背景（浅色字）+ `raw >= LUMA_LIGHT_MIN + LUMA_FAST_MARGIN` → 转浅色背景
    ///
    /// **反向冷却闸**：刚快速翻过的话（`seq - last_fast_seq < LUMA_FAST_COOLDOWN_SAMPLES`）
    /// 直接返回 `None`，让调用方掉回慢速路径——注意是"降级"而不是"丢弃"，
    /// 否则下一次真实换壁纸会被卡住不翻。慢速通道本身不受冷却约束。
    ///
    /// 命中时 `confirm` 清零并记下 `last_fast_seq`（没有 EMA 需要对齐，
    /// `on_light` 一旦落定，后续同值样本会被死区粘住）。
    fn try_fast_flip(&mut self, raw: f32, current: bool) -> Option<bool> {
        let deep_dark = raw <= limits::LUMA_DARK_MAX - limits::LUMA_FAST_MARGIN;
        let clearly_light = raw >= limits::LUMA_LIGHT_MIN + limits::LUMA_FAST_MARGIN;

        let new = if current && deep_dark {
            false
        } else if !current && clearly_light {
            true
        } else {
            return None;
        };

        // 冷却闸：只在"刚快速翻过"时生效，且只降级为慢速路径（不是不翻）
        if let Some(last) = self.last_fast_seq {
            if self.seq.saturating_sub(last) < limits::LUMA_FAST_COOLDOWN_SAMPLES {
                return None;
            }
        }

        self.on_light = Some(new);
        self.confirm = 0;
        self.last_fast_seq = Some(self.seq);
        crate::log_info!(
            "文字色快速翻转（大幅跳变，fast=true）：on_light={} (was {}) raw={:.3} seq={}",
            new,
            current,
            raw,
            self.seq
        );
        Some(new)
    }

    /// 显式重置（等价于「下一个样本走首帧规则」，但代次沿用现值）。
    pub fn reset(&mut self) {
        self.on_light = None;
        self.confirm = 0;
        self.last_fast_seq = None;
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

    /// 2. 确认区**精确 2 帧**翻转（第 1 次记确认、第 2 次翻）。
    ///
    /// 0.29 在确认区（≥0.25 且 <0.30）：不会走快速通道，但也不需要 EMA 爬坡——
    /// 直接就是「第 1 帧 confirm=1、第 2 帧翻」。
    #[test]
    fn confirm_zone_needs_two_confirmation_samples() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(0.06, 1), "0.06 首帧应判为深色背景");

        assert!(!s.push(0.29, 1), "第 1 个确认区样本不许翻");
        assert_eq!(s.confirm(), 1, "第 1 个样本应记 1 次确认");

        assert!(s.push(0.29, 1), "第 2 个确认区样本必须翻（精确 2 帧）");
        assert_eq!(s.confirm(), 0, "翻转后 confirm 必须清零");
        assert_eq!(s.on_light(), Some(true));
    }

    /// 2b. **大幅跳变必须当帧翻**（第 3 层快速通道）。
    #[test]
    fn big_jump_flips_in_one_sample() {
        // 浅 → 深（白壁纸切黑壁纸：用户实测从 7 秒压到 1 秒）
        let mut a = LumaStabilizer::new();
        assert!(a.push(1.0, 1), "1.0 首帧应判为浅色背景");
        assert!(
            !a.push(DARK, 1),
            "白→黑必须 1 个样本就翻（0.02 ≤ 0.07，走快速通道）"
        );
        assert_eq!(a.confirm(), 0, "快速通道不经过确认计数");
        assert_eq!(a.on_light(), Some(false));

        // 深 → 浅
        let mut b = LumaStabilizer::new();
        assert!(!b.push(DARK, 1), "0.02 首帧应判为深色背景");
        assert!(
            b.push(LIGHT, 1),
            "黑→白必须 1 个样本就翻（0.90 ≥ 0.30，走快速通道）"
        );
        assert_eq!(b.on_light(), Some(true));
    }

    /// 2c. **不够远就不许走快速通道**：0.08 已经进深色区（≤0.12），
    /// 但离阈值不够远（>0.07），必须走确认区、**精确 2 帧**。
    #[test]
    fn fast_path_does_not_fire_near_threshold() {
        let mut s = LumaStabilizer::new();
        assert!(s.push(1.0, 1), "起手浅色背景");

        assert!(s.push(0.08, 1), "不够远就不许当帧翻");
        assert_eq!(s.confirm(), 1, "应记为第 1 次确认");

        assert!(!s.push(0.08, 1), "第 2 个样本必须翻（精确 2 帧）");
        assert_eq!(s.confirm(), 0);
        assert_eq!(s.on_light(), Some(false));
    }

    /// 2d. **快速翻转是粘的**：翻完再连喂 3 个同值样本，结论不许回弹。
    #[test]
    fn fast_flip_is_sticky() {
        let mut s = LumaStabilizer::new();
        assert!(s.push(1.0, 1), "起手浅色背景");
        assert!(!s.push(DARK, 1), "快速通道翻成深色背景");

        for i in 0..3 {
            assert!(
                !s.push(DARK, 1),
                "第 {i} 个同值样本把结论弹回去了"
            );
        }
        assert_eq!(s.on_light(), Some(false));
    }

    /// 2e. **冷却闸挡住"紧接着反向"**：防止动态壁纸把小组件变成 1 Hz 频闪。
    ///
    /// 关键点：被挡住的那一帧必须**掉进慢速路径**（记 1 次确认），而不是被丢弃——
    /// 否则真实的反向换壁纸会卡住不翻。
    #[test]
    fn fast_flip_cooldown_blocks_immediate_reverse() {
        let mut s = LumaStabilizer::new();
        assert!(s.push(1.0, 1), "起手浅色背景");
        assert!(!s.push(0.02, 1), "第 1 次快速翻转必须当帧生效");

        assert!(
            !s.push(0.90, 1),
            "冷却期内反向大跳变不许走快速通道"
        );
        assert_eq!(
            s.confirm(),
            1,
            "必须降级到慢速路径并记 1 次确认（不是被丢弃）"
        );

        assert!(s.push(0.90, 1), "冷却过期后应完成翻转（最坏 2 帧）");
        assert_eq!(s.on_light(), Some(true));
    }

    /// 2f. 冷却**不会被永久废掉**：中间隔够同向样本，反向大跳变仍能当帧快速翻。
    #[test]
    fn fast_flip_cooldown_resets_after_idle_samples() {
        let mut s = LumaStabilizer::new();
        assert!(s.push(1.0, 1), "起手浅色背景");
        assert!(!s.push(0.02, 1), "快速翻成深色背景");

        // 喂足够多同向样本让冷却时间过去
        for i in 0..limits::LUMA_FAST_COOLDOWN_SAMPLES {
            assert!(!s.push(0.02, 1), "同向样本 #{i} 不该翻转");
        }
        // 反向大跳变：必须**当帧**翻（若冷却逻辑把快速通道废掉，这里会返回 false）
        assert!(s.push(0.90, 1), "冷却结束后反向大跳变必须当帧快速翻转");
        assert_eq!(s.on_light(), Some(true));
    }

    /// 2g. **动态壁纸极端场景的实测周期**（这条是给用户看的数字）。
    ///
    /// `raw` 在 0.85 与 0.04 之间逐秒横跳（视频镜头黑白闪切）：
    /// 没有冷却闸时每帧都翻（1 Hz）；有冷却闸（=2）后**每 3 个样本翻一次**，
    /// 翻转落在第 1、4、7、10 个样本 → 1s 采样下 ≈ **每 3 秒一次**（0.33 Hz）。
    #[test]
    fn alternating_video_wallpaper_flips_every_three_samples() {
        let mut s = LumaStabilizer::new();
        let mut flips: Vec<usize> = Vec::new();
        let mut prev: Option<bool> = None;
        for i in 0..12usize {
            let raw = if i % 2 == 0 { 0.85 } else { 0.04 };
            let got = s.push(raw, 1);
            if let Some(p) = prev {
                if p != got {
                    flips.push(i);
                }
            }
            prev = Some(got);
        }
        assert_eq!(
            flips,
            vec![1, 4, 7, 10],
            "动态壁纸横跳的翻转间隔必须是 3 个样本（≈3 秒），实际 {flips:?}"
        );
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

    /// 3b. **确认区 / 死区来回抖动永不翻**——这是删掉 EMA 之后抗抖能力的核心证据。
    ///
    /// 0.29 在确认区（want=浅色），0.20 在死区（want=当前值）：
    /// 方向相反 → 每次 `want == current` 都把 `confirm` 清零，永远攒不满 2 次。
    ///
    /// 注意**不能**用 0.30 做这个测试：`0.25 + margin(0.05) = 0.30` 正好是快速通道
    /// 的边界，那会被当成「大幅跳变」当帧翻——这是定论里明确要的行为。
    #[test]
    fn threshold_jitter_never_flips() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 1), "起手深色背景");

        let mut peak = 0u32;
        for i in 0..10 {
            assert!(!s.push(0.20, 1), "第 {i} 轮：死区样本把深色翻成了浅色");
            peak = peak.max(s.confirm());
            assert!(!s.push(0.29, 1), "第 {i} 轮：确认区样本攒够确认把深色翻成了浅色");
            peak = peak.max(s.confirm());
        }
        assert_eq!(s.on_light(), Some(false), "全程只能保持深色背景");
        assert!(
            peak < limits::LUMA_CONFIRM_SAMPLES,
            "confirm 峰值 {peak} 达到了翻转门槛 {} —— 死区样本没把计数清零",
            limits::LUMA_CONFIRM_SAMPLES
        );
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

    /// 5. 几何代次变化 → 清空确认计数，按首帧规则重判。
    ///
    /// 第二步用确认区的 0.26（< 0.30）而不是 0.9：0.9 会命中快速通道当帧就翻，
    /// 就测不到「同代次内不许翻」这件事了。
    #[test]
    fn epoch_change_resets_state() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 1));
        assert!(!s.push(0.26, 1), "同一代次内第 1 个确认区样本不许翻");

        // 换了采样区域：新区域的第一个样本立刻生效（不等确认）
        assert!(s.push(LIGHT, 2), "epoch 变化后应立刻按首帧规则判定");
        assert_eq!(s.epoch, 2);
        assert_eq!(s.confirm(), 0);
    }

    /// 防御：NaN / inf 不许污染状态，也不许被当成「大幅跳变」。
    #[test]
    fn non_finite_luma_is_sanitised() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(f32::NAN, 1), "NaN 应被当成 0（深色）");
        assert_eq!(s.on_light(), Some(false));

        // inf 钳成 1.0，但**不走快速通道**（脏数据不是大幅跳变）→ 只记 1 次确认
        assert!(
            !s.push(f32::INFINITY, 1),
            "单样本不许翻转文字色"
        );
        assert_eq!(s.confirm(), 1, "inf 应被当作普通样本走确认流程");
        assert_eq!(s.on_light(), Some(false));

        // -inf 钳成 0.0 → 与当前结论一致，确认计数清零
        assert!(!s.push(f32::NEG_INFINITY, 1));
        assert_eq!(s.confirm(), 0);
        assert_eq!(s.on_light(), Some(false));
    }

    /// `reset()` 之后下一个样本走首帧规则。
    #[test]
    fn reset_restores_first_sample_rule() {
        let mut s = LumaStabilizer::new();
        assert!(!s.push(DARK, 7));
        s.reset();
        assert!(s.push(LIGHT, 7), "reset 后首个样本应立刻判定");
    }

    /// 纯状态机：同一输入序列跑两遍，输出必须逐帧一致（无浮点收敛过程）。
    #[test]
    fn push_is_deterministic() {
        let seq = [0.02f32, 0.29, 0.29, 0.20, 0.08, 0.08, 0.19, 0.90, 0.30, 0.05];
        let run = || {
            let mut s = LumaStabilizer::new();
            seq.iter().map(|l| s.push(*l, 1)).collect::<Vec<bool>>()
        };
        assert_eq!(run(), run(), "同一输入序列必须得到同一输出");
    }
}
