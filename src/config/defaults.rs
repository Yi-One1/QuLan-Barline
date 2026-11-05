//! 全部默认值 + 容量常量（`ARCHITECTURE.md` §7.6）。
//!
//! 规矩：**任何魔法数都不许散落在业务代码里**，一律进 `limits`。
//! 这样「封面缓存条数」这类参数在 M5 调优时只需改一处。

/// 容量常量（写死，禁止散落魔法数）。
pub mod limits {
    /// 封面解码目标尺寸（超过则缩放）。64×64 BGRA = 16 KB
    pub const COVER_DECODE_PX: u32 = 64;
    /// 封面位图缓存条数
    pub const COVER_CACHE_CAP: usize = 8;
    /// 歌词内存 LRU 条数
    pub const LYRIC_LRU_CAP: usize = 8;
    /// 磁盘歌词缓存保留天数
    pub const LYRIC_DISK_TTL_DAYS: i64 = 30;
    /// 负缓存保留小时数（PRD 6.2：同一曲目失败后 24h 内不再请求）
    pub const LYRIC_MISS_TTL_HOURS: i64 = 24;
    /// 在线歌词重试退避（毫秒）。**表长 = 重试次数上限**。
    ///
    /// v1.3.2（G1 提速）从 `[5s, 15s, 60s]` 收敛成只剩一项：原来的三连退避叠上
    /// 「单源网络失败立刻重试」后，一次查询最坏能烧到分钟级（日志实测 83.8s）。
    /// 现在整链有 8s 硬截止，再退回三连只是在已经确定慢的路上继续等。
    pub const LYRIC_RETRY_BACKOFF_MS: [u64; 1] = [3_000];
    /// 歌词落盘「本次运行内已处理」集合的容量上限（v1.7.0）。
    /// 满了直接 clear：真正的幂等由 `writeback` 的 `create_new` 保证，这个集合只省 syscall。
    pub const WRITE_BACK_DEDUP_CAP: usize = 64;
    /// 设置页「歌词磁盘缓存」占用统计的单次扫描条目上限（v1.8.0）。
    ///
    /// 只数**顶层文件**，到达上限立即早停并把 `truncated` 置真（页面显示 `5000+`）。
    /// 存在理由：用户可能把数据目录指到一个几万文件的目录，统计发生在 UI 线程上，
    /// 不封顶就会把设置页卡住（PRD v1.8.0 A5.6）。
    pub const CACHE_STATS_MAX_ENTRIES: usize = 5000;
    /// 小组件宽度范围（100% DPI 逻辑像素）
    pub const WIDGET_MIN_W: f32 = 200.0;
    pub const WIDGET_MAX_W: f32 = 340.0;
    /// 与托盘区左边缘的间距
    pub const TRAY_GAP: f32 = 8.0;
    /// 歌词区最小宽度（逻辑像素；v1.1.1：信息区宽 = max(歌名+歌手自然宽, 此值)，
    /// 歌词开关过渡动画把信息区最小宽在 0 ↔ 此值之间插值）
    pub const LYRIC_W: f32 = 160.0;
    /// 歌词区单行矩形高度（逻辑像素；48px 条带 = 2 + 22 + 22 + 2，上行歌名歌手、下行歌词）
    pub const LYRIC_LINE_H: f32 = 22.0;
    /// 上行内歌名与歌手名的横向间隔（逻辑像素）
    pub const TITLE_ARTIST_GAP: f32 = 6.0;
    /// 下行歌词相对行矩形中心的整体上移量（逻辑像素，v1.1.3：再上移 2px，累计 3.5px）
    pub const LYRIC_LIFT: f32 = 3.5;
    /// 长歌词滚动（跑马灯）：滚动窗口低于该值（秒）不滚，只显示行首（v1.1.4）。
    /// 窗口太短的行在窗口内匀速滚完全程会快到看不清，宁可只显示行首。
    pub const MARQUEE_MIN_WINDOW_SECS: f32 = 1.5;
    /// 长歌词滚动：**提前量**（v1.1.4）——比「最后一个字唱完」提前这么多毫秒
    /// 滚到尾，行尾 + 行间间歇留给用户读最后一个字；实际提前量同时钳到
    /// 演唱窗口的 1/4，避免短句提前太多导致滚动过快。
    pub const MARQUEE_LEAD_MS: i64 = 1200;
    /// 命中区最小边长
    pub const HIT_MIN: f32 = 28.0;
    /// 双击判定窗口
    pub const DOUBLE_CLICK_MS: u64 = 250;
    /// 拖动进入 Seeking 的阈值
    pub const SEEK_DRAG_PX: f32 = 3.0;
    /// 背景采样节流
    pub const SAMPLE_THROTTLE_MS: u64 = 300;
    /// **静止态心跳**背景采样的间隔（v1.4.4 / G2-B1：1s → 3s）。
    ///
    /// 这条心跳只负责「用户把窗口拖到小组件后面」这种没有任何通知的场景，
    /// 晚 2 秒更新字色用户看不出来；而抓屏（`BitBlt` 读已合成桌面）会跟
    /// DWM / GPU 抢锁，是残余卡顿的头号嫌疑，所以次数直接砍到 1/3。
    ///
    /// **不受影响**：悬停进入 / 壁纸主题变化 / 几何变化 / 退出截图态 /
    /// 切换播放器导致的几何重排 —— 它们都直接投递采样，不等心跳。
    pub const BACKDROP_IDLE_MS: u64 = 3_000;
    /// 媒体轮询周期 —— **快层**：播放状态（G2 修复版起它只是三层里的一层）
    pub const MEDIA_POLL_MS: u64 = 500;
    /// 媒体轮询 —— **中层**：时间线（时长 + 位置）。UI 侧靠 `position_at` 本地外推，
    /// 1s 校一次足够；位置用不上还要每 500ms 问，是 G2 定位到的「把网易云拖卡」主因之一。
    pub const MEDIA_POLL_MID_MS: u64 = 1_000;
    /// 媒体轮询 —— **慢层**：标题 / 歌手 / 专辑 / AUMID / 播控标志 + 封面。
    /// 最重的一组（`TryGetMediaPropertiesAsync` 要跨进程拉元数据），2s 一次。
    pub const MEDIA_POLL_SLOW_MS: u64 = 2_000;
    /// 时间线**退避**间隔：连续 [`MEDIA_POLL_ZERO_STREAK`] 次读到 (0,0)（网易云特征）
    /// 后，时间线轮询拉长到 5s，直到出现非零值或播放状态跃迁再恢复。
    pub const MEDIA_POLL_TIMELINE_BACKOFF_MS: u64 = 5_000;
    /// 触发时间线退避的连续 (0,0) 次数
    pub const MEDIA_POLL_ZERO_STREAK: u8 = 3;
    /// 「位置回退」判定阈值：比上次读到的位置倒退超过它 → 视为切歌 / 重播，
    /// 立即补读属性（标题 / 歌手）。取 1.5s 是因为正常播放只会前进，
    /// 而真实切歌的位置差通常是几十秒量级，1.5s 不会误伤。
    pub const MEDIA_TIMELINE_REGRESS_MS: i64 = 1_500;
    /// **歌词钟记忆的存活时长（v1.9.3 / G8-④）**。
    ///
    /// 恒报 `(0,0)` 的播放器（网易云）读不到真实位置，歌词钟完全靠锚点自维护；
    /// 「离开这首歌时它播到哪儿」只能由我们自己记住（见
    /// `smtc::LyricClockMemory`）。超过这个时长的记忆视为不可信（用户很可能
    /// 已经手动改进度 / 换了播放列表），切回该曲时退回「从头开始」。
    ///
    /// 30 分钟：覆盖「暂停 → 处理别的事 → 回来继续」的真实节奏，又短到不会在
    /// 「同一首歌重新从头播」时把陈旧位置硬塞回去。
    pub const LYRIC_CLOCK_MEMORY_TTL_MS: u64 = 30 * 60 * 1_000;
    /// **歌词钟记忆的容量上限（v1.9.3 / G8-④）**。
    ///
    /// 满了淘汰 `sampled_at` 最老的一条（确定性，无需额外 LRU 结构）。
    /// 16 条足够覆盖「几个播放器 × 最近几首」，且每条只有几十字节。
    pub const LYRIC_CLOCK_MEMORY_CAP: usize = 16;
    /// 日志单文件上限
    pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

    // ------------------------------------------------------------------ M2 新增
    /// 悬停背景淡入时长（PRD P0-07：120ms）
    pub const HOVER_FADE_IN_MS: u32 = 120;
    /// 悬停背景淡出时长（PRD P0-07：200ms）
    pub const HOVER_FADE_OUT_MS: u32 = 200;
    /// 动画定时器周期（60fps 的整数近似）
    pub const ANIM_TIMER_MS: u32 = 16;
    /// 单击判定延迟（PRD §4.3：250ms 内第二下视为双击，不产生动作）
    pub const CLICK_DELAY_MS: u32 = 250;
    /// 封面视觉边长（逻辑像素）
    pub const COVER_SIDE: f32 = 32.0;
    /// 封面圆角（逻辑像素）
    pub const COVER_RADIUS: f32 = 4.0;
    /// 按钮图标边长
    pub const BTN_ICON: f32 = 16.0;
    /// 按钮命中区边长（PRD：≥ 28×28）
    pub const BTN_HIT: f32 = 28.0;
    /// 按钮间距
    pub const BTN_GAP: f32 = 6.0;
    /// 歌名字号（v1.1.1：两行结构内歌名最大、歌词略小、歌手最小偏灰）
    pub const TITLE_PX: f32 = 12.0;
    /// 歌手字号
    pub const ARTIST_PX: f32 = 10.0;
    /// 歌词字号（v1.1.1 单行大字：7 → 11px，用户核心诉求「歌词大一点」）
    pub const LYRIC_PX: f32 = 11.0;
    /// HUD / 气泡字号
    pub const HUD_PX: f32 = 12.0;
    /// 进度条静止高度
    pub const PROGRESS_H_IDLE: f32 = 2.0;
    /// 进度条悬停高度
    pub const PROGRESS_H_HOVER: f32 = 4.0;
    /// 封面与文本块的间距
    pub const COVER_TEXT_GAP: f32 = 10.0;
    /// 文本块与按钮组的间距
    pub const TEXT_BTN_GAP: f32 = 8.0;
    /// 内容区左右安全边距
    pub const CONTENT_PAD: f32 = 8.0;
    /// 背景采样的块尺寸（4×4 像素一块，对应 §11.2 的「4×4 块统计」）
    pub const SAMPLE_BLOCK: u32 = 4;
    /// 块内 alpha > 8 的像素占比超过它就算「我们自己画的」（§11.2 的 0.10）
    pub const SAMPLE_COVERED_RATIO: f32 = 0.10;
    /// 有效块占比低于它 → 退回全块平均（§11.2 第 4 步）
    pub const SAMPLE_MIN_VALID_RATIO: f32 = 0.30;
    /// 文本区域块的亮度权重倍数（§11.2 第 3 步）
    pub const SAMPLE_TEXT_WEIGHT: f32 = 3.0;
    // -------------------------------------------------- v1.5.0（G3）右上角 ×
    /// 关闭 × 的**字形**边长（逻辑像素）。
    ///
    /// v1.5.1（用户验收反馈「× 太大、太显眼」）：16 → 8。
    /// v1.5.4（用户验收反馈「大小改为 6px」）：8 → 6。
    pub const DISMISS_ICON: f32 = 6.0;
    /// 关闭 × 的**命中**方块边长（逻辑像素）。
    ///
    /// v1.5.1：命中区随字形一起缩小（用户原话「热区也随之缩小」）24 → 16，
    /// 字形 8 居中后四周各余 4px，手指/触控板仍不必精确命中笔画。
    /// v1.5.4：字形 8 → 6，命中区 16 → **14**（而不是 12）。
    ///
    /// 为什么是 14 而不是「6 + 2×4 = 14」之外的 12：14 − 6 = 8，两侧各余 **4**，
    /// 与 v1.5.3 的居中余量完全一致（手感不变）；同时
    /// 「「下一首」图标右缘 → × 字形左缘」= [`DISMISS_GAP`] 2 + 本余量 4 +
    /// 图标在 28 热区内的右余量 6 = **12px**，恰好是用户这一轮要求的
    /// 「图标与 × 的间距 12px 不变」。取 12 会让那段缝变成 10，破坏验收。
    /// **注意**：它刻意小于 [`HIT_MIN`]（28）——× 独占右上角、相邻只有「下一首」，
    /// 且与 next 命中区之间还留 [`DISMISS_GAP`] 的净距，误触由「不相邻」保证，
    /// 不靠放大热区（放大反而更容易压到 next）。
    pub const DISMISS_HIT: f32 = 14.0;
    /// 关闭 × 命中区与「下一首」命中区之间的**水平净距**（逻辑像素）。
    ///
    /// 两个热区**绝不重叠**是误触防线：点播放三键时手指偏一点点不会命中 ×、
    /// 把小组件收起来。
    ///
    /// ## 取值史
    ///
    /// v1.5.0 定为 8.0，理由是「点下一首手抖不收起界面」——当时 × 命中区还是 24、
    /// 右边距还是 `CONTENT_PAD = 8`，8px 是整条右边距预算里最大的一块。
    ///
    /// v1.5.1 缩字号（字形 16 → 8、命中 24 → 16）时**保持 8.0 不动**：那一轮
    /// 用户只要求「× 小一点」，误触防线不该跟着缩。
    ///
    /// v1.5.2 把右边距从 8 收到 4（用户反馈「右侧留空太多」）之后，问题变形了：
    /// 窗口右缘只剩 4 + 16 = 20px，而「下一首」**图标**到 × **字形**之间实测
    /// 18px（= 图标在 28 热区内的右侧余量 6 + 净距 8 + 字形在 16 热区内的
    /// 左侧余量 4）。视觉上 × 被挤在最右侧、三个按键左边却空出一块，用户
    /// 看到的是「× 左边没人用的空白」而不是「一条误触防线」。
    ///
    /// v1.5.3（用户验收反馈「× 左侧的按键和内容可以右移，充分利用空白空间，
    /// 只要按键之间不冲突即可」）：用户**显式授权**把这块空白吃掉 → 8.0 → 2.0。
    /// 净距仍然是**正数**（不是 0）：
    ///   - 两个命中区仍严格不重叠（`hit_next.right() < hit_dismiss.x`），
    ///     「点下一首不会收起小组件」这条语义一个字没松；
    ///   - 留 2px 给 `round()` / `trunc()` 的取整误差，任何 DPI 下都不会真的贴上
    ///     （实测 1.0 / 1.25 / 1.5 / 2.0 四档净距都恰为 `2*s`）。
    ///
    /// 宽度侧的影响见 [`limits::DISMISS_RIGHT_MARGIN`] 与 `layout::compute_full`
    /// 的 `dismiss_col`：这一列从 v1.5.2 的 28*s 收到 22*s，v1.5.4 再收到 **20*s**
    /// （命中区 16 → 14；本常量**一个字没动**）。
    pub const DISMISS_GAP: f32 = 2.0;
    /// 关闭 × 命中区右缘到客户区右缘的距离（逻辑像素）。
    /// 取 4（而不是 CONTENT_PAD 的 8）+ 字形在热区内的 4px 居中余量 = 字形右缘距边缘 8px，
    /// 与左侧 CONTENT_PAD 对称。v1.5.1 这里是 12px，用户反馈"右侧留空太多"。
    ///
    /// v1.5.4（字形 8 → 6、命中 16 → 14）**不动**：居中余量仍是
    /// `(14 − 6) / 2 = 4`，于是「4 + 4 = 8 = CONTENT_PAD」这条左右对称
    /// 一个字没变（由 `dismiss_glyph_right_edge_is_symmetric_with_left_pad` 锁住）。
    pub const DISMISS_RIGHT_MARGIN: f32 = 4.0;
    /// 关闭 × 的笔画宽度（逻辑像素，逻辑值；45° 斜线的**垂直**粗细）。
    ///
    /// v1.5.1：1.6 → 1.0。8px 字形配 1.6px 笔画，两条斜线在交叉处几乎糊成
    /// 一团实心块，「×」的形态会消失；1.0px 在 100% DPI 下是 2 像素宽的截面，
    /// 缩到 8px 后仍能看出是叉而不是方块。
    ///
    /// v1.5.4（用户反馈「× 的不透明度还是改为和按钮"下一首"一致」）：1.0 → **1.2**。
    /// 用户看到的「× 比按钮淡」**不是 alpha 的问题**——`draw_dismiss` 与
    /// `draw_buttons` 的 enabled 态本来就都是 `theme.text @ 0xFF`，一个 bit 都不差。
    /// 真正的原因是**笔画太细**：三键是 2.2px 宽的实心条，× 是 1.0px 斜线，
    /// 抗锯齿后每行的覆盖率只有一半左右 → 视觉上发灰。
    /// 加粗到 1.2 后每行水平段 = `1.2 × √2 ≈ 1.70px`，墨色浓度与三键可比，
    /// 同时（6 行像素）交叉处仍分得开，不会糊成一团（见 `painter::fill_cross`）。
    /// 上限由 `dismiss_ink_density_matches_buttons` 这条测试把守。
    pub const DISMISS_LINE: f32 = 1.2;
    /// 关闭 × 字形的**不透明度**（8 位 alpha）。
    ///
    /// v1.5.1：常态就是 `0xFF`（用户选 100% 不透明）。字形不再靠「变亮」表达
    /// 悬停 —— 已经满不透明就没有「更亮」的余地，悬停反馈改用极淡底色
    /// （[`DISMISS_BG_HOVER`]）。
    ///
    /// v1.5.4：用户再次要求「× 的不透明度与按钮"下一首"一致」。查证结果是
    /// **两者本来就同值**（`painter::draw_buttons` 的 enabled 态是
    /// `Rgba::from_rgb(theme.text, 0xFF)`），所以这里不需要改数值 —— 但为了
    /// 让「两个值可能漂移」在代码层面也不可能发生，`draw_dismiss` 已改成直接写
    /// 字面量 `0xFF`（与按钮同源的写法），本常量保留为**语义占位**并恒等于它。
    /// 真正让 × 看起来更淡的是笔画宽度，见 [`DISMISS_LINE`]。
    pub const DISMISS_ALPHA: u8 = 0xFF;
    // **历史契约常量（v1.5.1 × 淡底）**：下面三个 `DISMISS_BG_*` 仅被
    // `layout.rs` 的既有契约测试与 `WidgetLayout::dismiss_bg_rect()` 引用，
    // v1.6.0 / G4 起**不参与任何绘制路径**（`draw_dismiss` 已改为与三键共用
    // 双层光晕）。保留符号是「461 条既有测试零删改」的硬要求，
    // `#![deny(warnings)]` 下删了会直接编译失败。
    /// 关闭 × **悬停底色**的 alpha（`theme.text` @ 0x14 ≈ 8%）。
    ///
    /// 这是 Win11 任务栏图标悬停的常规做法：极淡圆角底，不描边、不光晕。
    /// 之所以必须换掉「变亮」：字形常态已 100% 不透明。
    pub const DISMISS_BG_HOVER: u8 = 0x14;
    /// 关闭 × **按下底色**的 alpha（0x22 ≈ 13%，比悬停深一档）。
    pub const DISMISS_BG_PRESSED: u8 = 0x22;
    /// 关闭 × 悬停/按下底色的圆角半径（逻辑像素，绘制时乘 scale）。
    ///
    /// 底色就是 `hit_dismiss` 那一格（v1.5.4 起 14×14 @100%），4*s 圆角 ≈ 2/7 边长，
    /// 与任务栏图标的悬停方块观感一致；再大就会把 14px 方块画成圆形
    /// （半径必须严格小于短边一半 7*s，由 `dismiss_bg_radius_is_safely_under_half_short_side` 把守）。
    pub const DISMISS_BG_RADIUS: f32 = 4.0;

    // ------------------------------------------- v1.6.0 / G4 悬停反馈统一
    /// 按钮（含 ×）悬停光晕「内层底」的 alpha 上限。
    ///
    /// 实际绘制时**按 `theme.hover_bg.a` 缩放**：与全窗悬停背景同源。Q1 用户拍板
    /// 「保留极淡保底」——始终透明档（`hover_bg.a == 0`）下四 target 的光晕**不会
    /// 完全消失**，而是在 [`GLOW_FLOOR_ALPHA`] 处取整条强度曲线的非零下限截距；
    /// 强度仍随全窗档位缩放（不再是 v1.5.1 那层写死的 0x14），修掉 QA 遗留
    /// 「全局悬停消失但 × 仍亮」，同时保留可感知的悬停反馈。
    ///
    /// **v1.6.1（真人验收 Bug 2「悬停反馈不够明显」）**：用户已在「温和 / 中等 /
    /// 明显 / 很强」四档中拍板 **中等** —— 四个 `GLOW_*_{HOVER,PRESSED}` 标称值
    /// 整体上调（悬停 `0x20/0x0C` → `0x42/0x1C`，按下 `0x2E/0x14` → `0x60/0x2E`），
    /// 默认 70% 档（`hover_bg.a == 0xB3 == 179`，k ≈ 0.702）下内层约 **18%**、
    /// 外圈约 **8%**（此前只有 8.8% / 3.3%，肉眼几乎看不出）。
    /// 「按下比悬停更深」的比例关系保持不变（inner 96/66 ≈ 1.45、outer 46/28 ≈ 1.64，
    /// 与旧值的 1.44 / 1.67 一致）。半径 / 扩张 / 缩放 / 下沉一律未动，
    /// [`GLOW_FLOOR_ALPHA`] 也未动（受 Q1 的 `[0x01, 0x06]` 硬上界约束）。
    pub const GLOW_INNER_HOVER: u8 = 0x42;
    pub const GLOW_INNER_PRESSED: u8 = 0x60;
    /// 外层「影 / 光」的 alpha 上限，恒显著淡于内层 —— 才是柔和光晕而非描边。
    ///
    /// v1.6.1：随内层同比上调（0x0C/0x14 → 0x1C/0x2E），保持「外层恒淡于内层」。
    pub const GLOW_OUTER_HOVER: u8 = 0x1C;
    pub const GLOW_OUTER_PRESSED: u8 = 0x2E;
    /// Q1 拍板：始终透明档（`hover_bg.a == 0`）下四 target 保留的极淡保底 alpha。
    ///
    /// 它是全窗 hover 强度曲线 `painter::glow_params()` 在 `hover_bg.a → 0` 处的
    /// **非零下限截距**，四 target（含 ×）共享，**不是 × 专属底色**。真实取值由
    /// `scaled(nom) = round(nom * a / 255).max(本值).min(nom)` 逐分量产生：
    /// 保底档 `inner == outer == 0x06` 是预期（双层仍靠不同 RGB + 不同半径区分）。
    /// 真人验收可在 `[0x01, 0x06]` 内微调，`0x06` 是硬上界
    /// （由 `g4_constants_are_self_consistent` 把守）。
    pub const GLOW_FLOOR_ALPHA: u8 = 0x06;
    /// 按下时外圈扩张的比例（WIP 内联 0.5 → 具名化）。
    pub const GLOW_GROW_PRESSED_FACTOR: f32 = 0.5;
    /// 外圈相对内圈的扩张（逻辑像素，乘 scale）；按下时收紧到
    /// [`GLOW_GROW_PRESSED_FACTOR`] 倍。
    pub const GLOW_GROW_BUTTON: f32 = 3.0;
    pub const GLOW_GROW_DISMISS: f32 = 1.5;
    /// 光晕基准半径（逻辑像素，乘 scale）。三键命中区 28×28（半边 14），
    /// × 命中区 14×14（半边 7）：`半径 + 扩张` 必须 ≤ 各自半边，由单测把守。
    pub const GLOW_RADIUS_BUTTON: f32 = 11.0;
    pub const GLOW_RADIUS_DISMISS: f32 = 5.0;
    /// 按下时图标等比缩到 94%，并整体下沉 1 逻辑像素。
    pub const BUTTON_PRESSED_SCALE: f32 = 0.94;
    pub const BUTTON_PRESSED_SINK: f32 = 1.0;
    /// 封面悬停：恒暗蒙层 + 恒亮双箭头（边界说明见 `painter::draw_cover`）。
    /// Q5 蒙层暗度微调钩子：真人验收可在 `[0x40, 0x80]` 内调整，本轮保持 0x5A。
    pub const COVER_HOVER_DIM: u8 = 0x5A;
    /// 封面悬停蒙层的 RGB（恒黑）——与 [`COVER_HOVER_DIM`] 一起用，避免
    /// `painter.rs` 里出现内联 `Rgba::new(0, 0, 0, ...)` 魔法数。
    pub const COVER_DIM_RGB: (u8, u8, u8) = (0x00, 0x00, 0x00);
    /// 封面双箭头的 RGB（恒亮白）。**不用 `theme.text`**：浅壁纸的 `theme.text`
    /// 是深色，叠在黑色蒙层上会看不见；恒亮箭头在深浅两种壁纸下都成立。
    pub const COVER_ARROW_RGB: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);
    /// 封面双箭头的 alpha。
    pub const COVER_ARROW_ALPHA: u8 = 0xE6;
    /// Q4 拍板：封面双箭头朝向（`↖ + ↘` 对角外扩）。
    ///
    /// `0` = 左上 ↖ + 右下 ↘（当前）；`1` = 右上 ↗ + 左下 ↙（另一组对角）；
    /// 其它值**兜底为 0**。真人验收后只改这一处即可整体换向（两个 tip 与斜杆
    /// 方向都由它派生）。水平 ←+→ 需要另一套绘制分支，本轮不实现。
    pub const COVER_ARROW_ORIENTATION: u8 = 0;
    pub const COVER_ARROW_SIZE: f32 = 5.0;
    pub const COVER_ARROW_PAD: f32 = 3.0;
    pub const COVER_ARROW_LINE: f32 = 1.2;
    /// WCAG AA 最小对比度
    pub const MIN_CONTRAST: f32 = 4.5;

    // ------------------------------------------------- M5（v1.2.0）亮度稳定化
    /// 稳定化：原始亮度高于它才算「浅色背景」（要深色文字）。见 `render::stable`。
    pub const LUMA_LIGHT_MIN: f32 = 0.25;
    /// 稳定化：原始亮度低于它才算「深色背景」（要浅色文字）。
    /// 与 [`LUMA_LIGHT_MIN`] 之间是**死区**：亮度抖动落在死区里不翻转文字色。
    pub const LUMA_DARK_MAX: f32 = 0.12;
    /// 稳定化：连续多少个样本与当前值冲突才真的翻转（1s 心跳下 = 2 秒）
    pub const LUMA_CONFIRM_SAMPLES: u32 = 2;
    /// 稳定化「大幅跳变快速通道」的余量：原始亮度要**越过阈值这么多**才算
    /// "离阈值足够远、不可能是噪声"，此时立即翻转（1 帧）。
    ///
    /// 语义边界：当前浅色时 `raw <= LUMA_DARK_MAX - 本值`（≤ 0.07）→ 立即转深色；
    /// 当前深色时 `raw >= LUMA_LIGHT_MIN + 本值`（≥ 0.30）→ 立即转浅色。
    /// 中间那圈（0.07..0.30）走「死区 + 连续确认」，确认区固定 2 帧。
    ///
    /// 为什么要它：只靠死区+确认时，换壁纸要 2 帧才生效；而**确认区之外**的大步
    /// 跳变（换壁纸 / 切视频）是用户显式操作，必须当帧响应——用户实测过 7 秒延迟。
    pub const LUMA_FAST_MARGIN: f32 = 0.05;
    /// 快速通道的**反向冷却闸**：一次快速翻转之后，在接下来的这么多个样本内，
    /// 反向的快速翻转降级为慢速路径（死区 + 连续确认），避免动态壁纸把小组件
    /// 变成 1 Hz 明暗频闪。
    ///
    /// 语义：`seq - last_fast_seq >= 本值` 才允许下一次**快速**翻转。
    /// 慢速通道不受约束，所以真实的反向切换最坏 = 本值 + 1 个样本，不会被锁死。
    /// 取 2 → 极端场景（0.85 ↔ 0.04 逐秒横跳）从「每帧翻」降到「每 3 帧翻」。
    pub const LUMA_FAST_COOLDOWN_SAMPLES: u64 = 2;

    // ------------------------------------------- v1.9.0（G7）黑名单 / 媒体会话枚举
    /// 黑名单列表上限（防手改 json 写坏配置拖慢启动）。
    /// **数值唯一定义处**：`media/blacklist.rs` 用 `limits::` 引用，不许自带字面量。
    pub const BLACKLIST_MAX_ENTRIES: usize = 200;
    /// 黑名单单条上限（260 = Windows MAX_PATH 量级，足够任何进程名）。
    pub const BLACKLIST_MAX_LEN: usize = 260;
    /// 真实进程名缓存（`config.media.app_names`）上限。满时淘汰字典序最小键。
    /// 64 ≈ 日常机器出现过的播放器 AUMID 数的 10 倍余量；落盘体积 ≤ 4KB。
    pub const APP_NAME_CACHE_MAX: usize = 64;
    /// 黑名单命中日志的限流窗口（同一进程名一条 INFO，窗口内不重复）。
    pub const HIT_LOG_WINDOW_MS: u64 = 60_000;
    /// 反查阶段预算（毫秒）：`OpenProcess`+`QueryFullProcessImageNameW` 被安全软件
    /// 挂钩时"随机几~几十 ms"，120ms ≈ 忍 2~3 次被挂钩的调用。
    pub const ENUM_LOOKUP_BUDGET_MS: u64 = 120;
    /// 整个枚举（COM 激活 + 会话枚举 + 反查）的总预算。出口复核，超了照发结果 +
    /// `over_budget=true`，只记 WARN。
    pub const ENUM_TOTAL_BUDGET_MS: u64 = 200;
    /// 反查的**进程级**硬上限（真正贵的 `process_image_name` 次数，按 pid 去重后）。
    /// 去重后的进程数通常 ≤8 → 16 是 2× 保险丝。
    pub const ENUM_MAX_LOOKUPS: usize = 16;
    /// 反查的**会话级**硬上限（遍历音频会话个数）。超过即降级
    /// （`resolved=false`、AUMID 末段兜底），不追求解析成功。
    pub const ENUM_MAX_AUDIO_SESSIONS: usize = 64;
}

/// 配置结构版本。`Config.version < CURRENT_VERSION` 时走迁移链。
///
/// v1.9.0（G7）：1 → 2 —— `media` 分组一次引入 `blacklist` + `app_names` 两个字段，
/// 提一次版本足够（版本号表达 schema 代数，不是字段计数）。纯标记，零迁移分支，
/// 为未来迁移留闸门。回滚注意：老版本读 version=2 的配置会把 `media` 整组丢掉。
pub const CURRENT_VERSION: u32 = 2;

/// 热键修饰位（与 `MOD_*` 一致）。
pub mod mods {
    pub const ALT: u32 = 0x0001;
    pub const CONTROL: u32 = 0x0002;
    pub const SHIFT: u32 = 0x0004;
    pub const WIN: u32 = 0x0008;
}

/// 快捷键默认值的版本号。
///
/// 0 = M2/M3 时代的 `Ctrl+Alt+*`；
/// 1 = v1.1.x 线上默认 `Ctrl+Alt+Shift+*`（当年因 `Ctrl+Alt+*` 被其它软件占用
///     而临时加 Shift）；
/// 2 = **v1.2.3 起**回到用户要的 `Ctrl+Alt+*`。与 0 内容相同但语义不同
///     （那是"历史上的旧默认"，这是"现在的默认"），所以迁移判定用两个独立函数
///     [`HotkeyConfig::v0_defaults`] / [`HotkeyConfig::v1_defaults`]。
///
/// 为什么要版本号：旧配置里存的是**旧默认值**，但那也可能是用户**手动选过的**
/// 同款组合。只有"6 组全都等于某一代旧默认值"才能判定为"用户没改过"→ 可以迁移；
/// 改过任何一组就原样保留。
pub const HOTKEYS_VERSION: u32 = 2;

/// 常用虚拟键码（`RegisterHotKey` 用）。
pub mod vk {
    pub const P: u32 = 0x50;
    pub const L: u32 = 0x4C;
    pub const M: u32 = 0x4D;
    pub const H: u32 = 0x48;
    pub const LEFT: u32 = 0x25;
    pub const RIGHT: u32 = 0x27;
}

use super::{
    AppearanceConfig, Config, GeneralConfig, HotkeyBinding, HotkeyConfig, LangSerde, LyricsConfig,
    MediaConfig, VolumeConfig, VolumeScope,
};
use crate::render::theme::BackgroundMode;

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            appearance: AppearanceConfig::default(),
            lyrics: LyricsConfig::default(),
            volume: VolumeConfig::default(),
            hotkeys: HotkeyConfig::default(),
            general: GeneralConfig::default(),
            storage: crate::config::StorageConfig::default(),
            // v1.9.0（G7）：媒体源过滤（黑名单 + 真实进程名缓存）
            media: MediaConfig::default(),
        }
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            background_mode: BackgroundMode::Translucent,
            idle_mode: BackgroundMode::Transparent,
            hover_alpha: 70,
            idle_alpha: 50,
            monitor_index: 0,
        }
    }
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            karaoke_sweep: true,
            // v1.1.2：长歌词滚动（跑马灯），默认开
            scroll: true,
            online_enabled: true,
            local_dirs: Vec::new(),
            // v1.7.0：在线歌词落盘（只新建不覆盖），默认开
            write_back: true,
            // v1.8.0：查询顺序默认「磁盘缓存优先」= 与 v1.7.0 行为逐字节一致
            local_first: false,
        }
    }
}

impl Default for VolumeConfig {
    fn default() -> Self {
        Self {
            wheel_enabled: true,
            scope: VolumeScope::System,
            step: 0.02,
            hud_ms: 1200,
        }
    }
}

impl HotkeyConfig {
    /// 给定修饰位，产出一整套绑定表。
    ///
    /// **vk 表（P / ← / → / L / M / H）跨代不变**——v0 / v1 / v2 只差修饰位。
    /// 把它集中在这一处，是为了让 [`HotkeyConfig::default`]、
    /// [`HotkeyConfig::v0_defaults`]、[`HotkeyConfig::v1_defaults`]、
    /// [`HotkeyConfig::default_binding`] 全部**同源**：vk 字面量只在
    /// 这里出现一次，不可能出现"改了默认值忘了改逐行恢复"的漂移。
    /// （`hotkeys_version` 由调用方决定，这里给 0 表示"未标注代次"。）
    fn bindings_with(modifiers: u32) -> Self {
        Self {
            enabled: true,
            hotkeys_version: 0,
            conflict_notified: false,
            play_pause: HotkeyBinding {
                modifiers,
                vk: vk::P,
            },
            prev: HotkeyBinding {
                modifiers,
                vk: vk::LEFT,
            },
            next: HotkeyBinding {
                modifiers,
                vk: vk::RIGHT,
            },
            toggle_lyrics: HotkeyBinding {
                modifiers,
                vk: vk::L,
            },
            mute: HotkeyBinding {
                modifiers,
                vk: vk::M,
            },
            toggle_widget: HotkeyBinding {
                modifiers,
                vk: vk::H,
            },
        }
    }

    /// **v0 默认值**（M2/M3 时代）：`Ctrl+Alt+*`。**只用于迁移判定**，不要拿来注册。
    ///
    /// 与 v2 新默认内容相同是**有意为之**（v1.2.3 回到用户要的组合），
    /// 但两个函数语义不同（一个是"历史上的旧默认"，一个是"现在的默认"），
    /// 迁移判定必须分别比对，故不合并。
    pub fn v0_defaults() -> Self {
        Self::bindings_with(mods::CONTROL | mods::ALT)
    }

    /// **v1 默认值**（v1.1.x 线上默认）：`Ctrl+Alt+Shift+*`。**只用于迁移判定**。
    ///
    /// 当年临时加 Shift 的原因：实测那台机器上 `Ctrl+Alt+P / ← / → / L / M`
    /// 全被其它软件占用（错误码 1409）。v1.2.3 起按用户要求回到 `Ctrl+Alt+*`。
    pub fn v1_defaults() -> Self {
        Self::bindings_with(mods::CONTROL | mods::ALT | mods::SHIFT)
    }

    /// 某个动作在**当前默认值**下的绑定（v1.2.3：设置页逐行「恢复默认」）。
    ///
    /// 实现上直接取 [`HotkeyConfig::default`]，**绝不另抄一份 vk 字面量**——
    /// 否则默认值与逐行恢复两处必然漂移。
    pub fn default_binding(action: crate::event::HotkeyAction) -> HotkeyBinding {
        use crate::event::HotkeyAction as A;
        let d = Self::default();
        match action {
            A::PlayPause => d.play_pause,
            A::Prev => d.prev,
            A::Next => d.next,
            A::ToggleLyrics => d.toggle_lyrics,
            A::Mute => d.mute,
            A::ToggleWidget => d.toggle_widget,
        }
    }

    /// 6 组绑定是否完全一致（不含 `enabled` 与元数据字段）。
    pub fn same_bindings(&self, other: &Self) -> bool {
        self.play_pause == other.play_pause
            && self.prev == other.prev
            && self.next == other.next
            && self.toggle_lyrics == other.toggle_lyrics
            && self.mute == other.mute
            && self.toggle_widget == other.toggle_widget
    }

    /// 旧配置迁移（v1.2.3：目标版本 2，矩阵见下）。
    ///
    /// | 当前 6 组绑定 | 处理 |
    /// |---|---|
    /// | 全等于 [`v1_defaults`](Self::v1_defaults)（`Ctrl+Alt+Shift+*`） | 迁到新默认 `Ctrl+Alt+*` |
    /// | 全等于 [`v0_defaults`](Self::v0_defaults)（`Ctrl+Alt+*`） | 内容不变，只提版本号 |
    /// | 其它（用户手动改过任意一组） | **原样保留，绝不覆盖** |
    /// | `hotkeys_version >= 2` | 直接返回 `false`（不写盘） |
    ///
    /// 返回值语义不变：`true` ＝ 配置被改动（或版本号被提升）、调用方应落盘。
    pub fn migrate(&mut self) -> bool {
        if self.hotkeys_version >= HOTKEYS_VERSION {
            return false;
        }
        let enabled = self.enabled;
        let notified = self.conflict_notified;

        if self.same_bindings(&Self::v1_defaults()) {
            // 线上默认 → 新默认。保留用户自己的开关与"已提示过冲突"标记
            *self = Self::default();
            self.enabled = enabled;
            self.conflict_notified = notified;
            crate::log_info!(
                "快捷键默认值已从 Ctrl+Alt+Shift+* 迁移到 Ctrl+Alt+*（版本 1 → {}）",
                HOTKEYS_VERSION
            );
        } else if self.same_bindings(&Self::v0_defaults()) {
            // 已经是新默认的内容，只把版本号提上去（绑定一个字不动）
            crate::log_info!(
                "快捷键已是 Ctrl+Alt+* 默认值，仅把版本号提到 {}（绑定不变）",
                HOTKEYS_VERSION
            );
        } else {
            crate::log_info!("快捷键保持用户自定义值（不迁移），仅把版本号提到 {HOTKEYS_VERSION}");
        }
        self.hotkeys_version = HOTKEYS_VERSION;
        true
    }
}

impl Default for HotkeyConfig {
    /// 新默认（v1.2.3 / 版本 2）：`Ctrl+Alt+*` —— 播放/暂停 P、上一首 ←、
    /// 下一首 →、歌词开关 L、静音 M、显隐小组件 H。
    fn default() -> Self {
        let mut cfg = Self::bindings_with(mods::CONTROL | mods::ALT);
        cfg.hotkeys_version = HOTKEYS_VERSION;
        cfg
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            autostart: false,
            language: LangSerde::System,
            widget_hidden: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkeys_are_bound_and_unique() {
        let hk = HotkeyConfig::default();
        let all = [
            hk.play_pause,
            hk.prev,
            hk.next,
            hk.toggle_lyrics,
            hk.mute,
            hk.toggle_widget,
        ];
        for b in all {
            assert!(b.is_bound(), "默认热键必须已绑定");
        }
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(
                    (all[i].modifiers, all[i].vk),
                    (all[j].modifiers, all[j].vk),
                    "默认热键不能重复"
                );
            }
        }
    }

    #[test]
    fn limits_are_sane() {
        assert!(limits::WIDGET_MIN_W < limits::WIDGET_MAX_W);
        assert!(limits::TRAY_GAP > 0.0);
        assert_eq!(limits::LOG_MAX_BYTES, crate::log::LOG_MAX_BYTES);
    }

    // ------------------------------------------------ v1.2.3（G4）快捷键体系

    /// 用户明确要求：所有默认按键回到 `Ctrl+Alt+对应按键`。
    /// 逐项对键位（不是只数数量），并用 `hotkey::bindings` 走真实遍历路径。
    #[test]
    fn default_hotkeys_are_ctrl_alt() {
        use crate::event::HotkeyAction as A;
        let cfg = HotkeyConfig::default();
        let want = mods::CONTROL | mods::ALT;
        let expected = [
            (A::PlayPause, vk::P),
            (A::Prev, vk::LEFT),
            (A::Next, vk::RIGHT),
            (A::ToggleLyrics, vk::L),
            (A::Mute, vk::M),
            (A::ToggleWidget, vk::H),
        ];
        let got = crate::hotkey::bindings(&cfg);
        assert_eq!(got.len(), expected.len(), "必须有 6 组");
        for ((action, binding), (want_action, want_vk)) in got.iter().zip(expected.iter()) {
            assert_eq!(*action, *want_action, "动作顺序/映射错位");
            assert_eq!(
                binding.modifiers, want,
                "{:?} 的默认修饰键必须是 Ctrl+Alt",
                action
            );
            assert_eq!(binding.vk, *want_vk, "{:?} 的默认键位错", action);
        }
        assert_eq!(cfg.hotkeys_version, HOTKEYS_VERSION);
    }

    /// 迁移矩阵①：全等于 v1 默认（`Ctrl+Alt+Shift+*`）→ 迁到新默认。
    /// 同时验证 `enabled` / `conflict_notified` 被保留。
    #[test]
    fn migrate_from_v1_defaults_to_new() {
        let mut cfg = HotkeyConfig::v1_defaults();
        cfg.hotkeys_version = 1;
        cfg.enabled = false;
        cfg.conflict_notified = true;

        assert!(cfg.migrate(), "版本落后且有可迁移内容时必须返回 true（调用方要落盘）");
        assert_eq!(cfg.hotkeys_version, HOTKEYS_VERSION);
        assert!(cfg.same_bindings(&HotkeyConfig::default()), "必须迁到新默认 Ctrl+Alt+*");
        assert_eq!(cfg.play_pause.modifiers, mods::CONTROL | mods::ALT);
        assert!(!cfg.enabled, "enabled 必须保留用户设置");
        assert!(cfg.conflict_notified, "conflict_notified 必须保留");
    }

    /// 迁移矩阵②：全等于 v0 默认（`Ctrl+Alt+*`）→ 绑定不变，只提版本号。
    #[test]
    fn migrate_from_v0_defaults_bumps_version_only() {
        let mut cfg = HotkeyConfig::v0_defaults();
        cfg.hotkeys_version = 0;
        let before = cfg.clone();

        assert!(cfg.migrate(), "版本号被提升也算改动");
        assert_eq!(cfg.hotkeys_version, HOTKEYS_VERSION);
        assert!(
            cfg.same_bindings(&before),
            "已是新默认内容时，绑定一个字都不许动"
        );
    }

    /// 迁移矩阵③：用户手动改过任意一组 → **原样保留**，绝不覆盖。
    #[test]
    fn migrate_keeps_user_custom() {
        let mut cfg = HotkeyConfig::v1_defaults();
        cfg.hotkeys_version = 1;
        // 用户把播放/暂停改成 Ctrl+Alt+Q，其余仍是 v1 默认
        cfg.play_pause = HotkeyBinding {
            modifiers: mods::CONTROL | mods::ALT,
            vk: 0x51,
        };
        let before = cfg.clone();

        assert!(cfg.migrate());
        assert_eq!(cfg.hotkeys_version, HOTKEYS_VERSION);
        assert_eq!(cfg.play_pause, before.play_pause, "自定义项不许被覆盖");
        assert_eq!(cfg.prev, before.prev, "其余组也不许被覆盖");
        assert_eq!(cfg.toggle_widget, before.toggle_widget);
    }

    /// 迁移矩阵④：已经是当前版本 → 什么都不做、不写盘。
    #[test]
    fn migrate_is_noop_at_current_version() {
        let mut cfg = HotkeyConfig::default();
        assert_eq!(cfg.hotkeys_version, HOTKEYS_VERSION);
        assert!(!cfg.migrate(), "当前版本必须返回 false（不落盘）");

        // 用户改过 + 已是当前版本：同样不许动
        cfg.mute = HotkeyBinding {
            modifiers: mods::CONTROL | mods::ALT | mods::SHIFT,
            vk: 0x51,
        };
        let before = cfg.clone();
        assert!(!cfg.migrate());
        assert!(cfg.same_bindings(&before), "当前版本下不许改任何绑定");
        assert_eq!(cfg.hotkeys_version, before.hotkeys_version);
    }

    /// `default_binding`（逐行恢复默认用）必须与 `HotkeyConfig::default()` 逐项一致。
    #[test]
    fn default_binding_matches_default_config() {
        let d = HotkeyConfig::default();
        for (action, binding) in crate::hotkey::bindings(&d) {
            assert_eq!(
                HotkeyConfig::default_binding(action),
                *binding,
                "{:?} 的 default_binding 与默认配置不一致（两处 vk 字面量漂移了）",
                action
            );
        }
    }

    /// v0 / v1 / v2 三代默认值只差修饰位，vk 表必须一致——
    /// 这是"vk 字面量只写一处"的护栏。
    #[test]
    fn hotkey_generations_differ_only_in_modifiers() {
        let v0 = HotkeyConfig::v0_defaults();
        let v1 = HotkeyConfig::v1_defaults();
        let cur = HotkeyConfig::default();
        assert!(v0.same_bindings(&cur), "v2 内容应与 v0 相同（都是 Ctrl+Alt+*）");
        assert!(!v0.same_bindings(&v1), "v1 必须是 Ctrl+Alt+Shift+*，与 v0 不同");
        let pairs = [
            (v1.play_pause, v0.play_pause),
            (v1.prev, v0.prev),
            (v1.next, v0.next),
            (v1.toggle_lyrics, v0.toggle_lyrics),
            (v1.mute, v0.mute),
            (v1.toggle_widget, v0.toggle_widget),
        ];
        for (a, b) in pairs {
            assert_eq!(a.vk, b.vk, "两代默认值的 vk 必须相同");
            assert_eq!(a.modifiers, b.modifiers | mods::SHIFT, "v1 只多一个 Shift");
        }
    }
}
