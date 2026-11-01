//! 构建脚本：把 `assets/barline.rc`（图标 + manifest）编译成可链接的 `.lib`。
//!
//! `embed-resource` 在 `*-pc-windows-msvc` 下会自己去找 Windows SDK 里的 `rc.exe`
//! （见 `embed_resource::windows_msvc::find_windows_sdk_tool_impl`），找不到时才退回
//! `PATH` 里的 `rc.exe`。若两者都不可用，可设置环境变量 `RC` 显式指定路径。
fn main() {
    println!("cargo:rerun-if-changed=assets/barline.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");

    // 返回值是 CompilationResult，失败会在内部 panic，这里显式忽略避免 unused_must_use。
    let _ = embed_resource::compile("assets/barline.rc", embed_resource::NONE);
}
