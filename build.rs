//! 构建脚本：
//! 1. 用 [`icon_geom`] 的**同一份几何**生成 `assets/icon.ico`（深色字形）与
//!    `assets/icon_light.ico`（浅色字形）。这样 exe 资源、托盘、设置窗口标题栏
//!    与小组件里的双音符不可能画成两个样——用户对图标的核心要求就是「到处一致」。
//! 2. 把 `assets/barline.rc`（图标 + manifest）编译成可链接的 `.lib`。
//!
//! `embed-resource` 在 `*-pc-windows-msvc` 下会自己去找 Windows SDK 里的 `rc.exe`
//! （见 `embed_resource::windows_msvc::find_windows_sdk_tool_impl`），找不到时才退回
//! `PATH` 里的 `rc.exe`。若两者都不可用，可设置环境变量 `RC` 显式指定路径。

// 用 `#[path]` 把同一份几何当模块编译：`icon_geom.rs` 开头是 `//!` 模块文档，
// 而 `include!` 展开出来的内部文档注释不被接受（只能在模块体最前面）。
// `#[path]` 走的是正常模块解析，`//!` 合法，且不需要复制任何几何。
#[path = "src/render/icon_geom.rs"]
#[allow(dead_code)] // 该模块有一部分 API 只给运行时（src/render/icon.rs）用
mod icon_geom;

/// 深色字形（给浅色背景用）：取 `Theme::DARK_TEXT`（#0A0A0A），
/// 与小组件里的墨色同值，托盘/标题栏上的图标因此和小组件里画的一模一样。
const INK_DARK: (u8, u8, u8) = (0x0A, 0x0A, 0x0A);
/// 浅色字形（给深色任务栏 / 深色标题栏用）：取 `Theme::LIGHT_TEXT`（#FAFAFA）。
const INK_LIGHT: (u8, u8, u8) = (0xFA, 0xFA, 0xFA);
/// 每像素每轴超采样数。6×6 = 36 个样本，覆盖 37 级灰度，肉眼无锯齿。
const SUPERSAMPLE: u32 = 6;

fn main() {
    println!("cargo:rerun-if-changed=assets/barline.rc");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    // 几何改动必须重新生成 ICO；生成的 ico 本身**不**作为 rerun 输入
    // （否则 build.rs 写文件 → mtime 变化 → 无限重建）。
    println!("cargo:rerun-if-changed=src/render/icon_geom.rs");

    write_if_changed(
        "assets/icon.ico",
        &icon_geom::ico_bytes(&icon_geom::ICO_SIZES, INK_DARK, SUPERSAMPLE),
    );
    write_if_changed(
        "assets/icon_light.ico",
        &icon_geom::ico_bytes(&icon_geom::ICO_SIZES, INK_LIGHT, SUPERSAMPLE),
    );

    // 返回值是 CompilationResult，失败会在内部 panic，这里显式忽略避免 unused_must_use。
    let _ = embed_resource::compile("assets/barline.rc", embed_resource::NONE);
}

/// 内容有变化才写盘：避免每次构建都刷新 mtime（会让 git status 永远脏）。
fn write_if_changed(path: &str, bytes: &[u8]) {
    if let Ok(old) = std::fs::read(path) {
        if old == bytes {
            return;
        }
    }
    if let Err(e) = std::fs::write(path, bytes) {
        panic!("写入 {path} 失败：{e}");
    }
    println!("cargo:warning=已重新生成 {path}（{} 字节）", bytes.len());
}
