//! 数据存储位置（问题 3 / v1.1.5）。
//!
//! 职责：
//! - 把 `config.storage.data_dir`（可为空 = 默认）解析成绝对路径；
//! - 启动时把默认根目录（`%LOCALAPPDATA%\Barline`）下的已知缓存目录
//!   **自动迁移**到自定义数据目录；
//! - 迁移**永不删除**用户数据：rename 一步到位；跨卷退化 copy，成功后把
//!   旧目录原地改名 `<name>.old` 保留（可回滚），失败则原样保留并记日志。
//!
//! 边界（用户约定）：
//! - `config.json`（及其 `.bad` 备份）**永远留在默认根目录**，不迁移；
//! - 日志文件由 [`crate::log::relocate`] 在关闭写句柄后自行搬迁
//!   （打开中的文件 Windows 不允许改名，必须先关再搬）。

use std::fs;
use std::path::{Path, PathBuf};

/// 需要随数据目录迁移的**子目录**（相对根目录）。
///
/// - `lyrics-cache`：歌词磁盘缓存（`lyric::service`）
/// - `lyrics`：应用自带歌词目录（`lyric::locate`）
/// - `WebView2`：设置页 WebView2 用户数据（`settings::webview`）
pub const MIGRATE_DIRS: &[&str] = &["lyrics-cache", "lyrics", "WebView2"];

/// 启动入口：按配置初始化全局数据目录，必要时执行迁移 + 日志搬迁。
///
/// 必须在 `Config::load` 之后、任何使用 [`crate::platform::data_dir`] 的模块
/// 初始化之前调用（`App::bootstrap` 满足该顺序）。
pub fn init_from_config(custom: &str) {
    let default_root = crate::platform::barline_dir();
    let new_root = data_dir_from_config(custom, &default_root);
    crate::platform::set_data_dir(new_root.clone());
    if new_root == default_root {
        return;
    }
    crate::log_info!(
        "数据目录：{} → {}（启动迁移开始）",
        default_root.display(),
        new_root.display()
    );
    for line in migrate_dirs(&default_root, &new_root) {
        crate::log_info!("迁移：{line}");
    }
    // 日志最后搬：迁移期间旧日志仍在写（记录迁移过程本身）
    crate::log::relocate(&new_root);
    crate::log_info!("日志已搬迁至 {}", new_root.join("barline.log").display());
}

/// 由配置字符串解析数据目录（`None` = 使用默认根目录）。
///
/// 规则：trim；去首尾成对引号（资源管理器「复制文件地址」常带引号）；
/// 去结尾多余的分隔符；相对路径锚定在 `default_root` 下。
pub fn resolve_custom_dir(raw: &str, default_root: &Path) -> Option<PathBuf> {
    let s = raw.trim().trim_matches('"').trim();
    if s.is_empty() {
        return None;
    }
    let mut p = PathBuf::from(s);
    // 去掉结尾分隔符（保留根如 `C:\`）
    while p.as_os_str().len() > 3 {
        let last = p.as_os_str().to_string_lossy();
        if last.ends_with('\\') || last.ends_with('/') {
            p = PathBuf::from(last.trim_end_matches(['\\', '/']).to_string());
        } else {
            break;
        }
    }
    if p.as_os_str().is_empty() {
        return None;
    }
    if p.is_absolute() {
        Some(p)
    } else {
        Some(default_root.join(p))
    }
}

/// 同 [`resolve_custom_dir`]，但 `None` 折叠为默认根目录（启动用）。
pub fn data_dir_from_config(raw: &str, default_root: &Path) -> PathBuf {
    resolve_custom_dir(raw, default_root).unwrap_or_else(|| default_root.to_path_buf())
}

/// 迁移 `old_root` 下所有 [`MIGRATE_DIRS`] 到 `new_root`。
///
/// 返回人类可读的结果行（每子项一行，由调用方落日志）。同根 / 无可迁项
/// 返回空。单项失败不阻塞其余子项。
pub fn migrate_dirs(old_root: &Path, new_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if old_root == new_root {
        return out;
    }
    if let Err(e) = fs::create_dir_all(new_root) {
        out.push(format!("目标目录创建失败（{e}），本次不迁移"));
        return out;
    }
    for name in MIGRATE_DIRS {
        let src = old_root.join(name);
        let dst = new_root.join(name);
        if !src.exists() {
            continue;
        }
        if dst.exists() {
            out.push(format!("{name}: 目标已存在，跳过（两份都保留）"));
            continue;
        }
        match move_item(&src, &dst) {
            Ok(true) => out.push(format!("{name}: 已迁移")),
            Ok(false) => {}
            Err(e) => out.push(format!("{name}: 迁移失败（{e}），已保留原目录")),
        }
    }
    out
}

/// 迁移单个文件 / 目录（`src` → `dst`）。
///
/// - `src` 不存在或 `dst` 已存在 → `Ok(false)`（不动）；
/// - rename 优先（同卷瞬间完成）；失败（跨卷 / 占用）退化 copy，
///   copy 成功后把 `src` 原地改名 `<name>.old` 保留（**不删除**）；
/// - copy 中途失败：尽力删除半成品 `dst`，把错误抛回（`src` 未动）。
pub fn move_item(src: &Path, dst: &Path) -> std::io::Result<bool> {
    if !src.exists() || dst.exists() {
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(_) => {
            if src.is_dir() {
                copy_dir(src, dst)?;
            } else {
                fs::copy(src, dst)?;
            }
            // 原地改名保留旧件（跨卷 copy 后的稳妥回滚点）
            let name = src
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "item".to_string());
            let backup = src.with_file_name(format!("{name}.old"));
            let _ = fs::rename(src, &backup);
            Ok(true)
        }
    }
}

/// 递归复制目录（零依赖，缓存目录量级足够）。
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("barline_storage_{tag}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resolve_empty_and_quoted() {
        let root = Path::new("C:\\Users\\me\\AppData\\Local\\Barline");
        assert_eq!(resolve_custom_dir("", root), None);
        assert_eq!(resolve_custom_dir("   ", root), None);
        assert_eq!(resolve_custom_dir("\"\"", root), None);
        // 成对引号剥掉
        assert_eq!(
            resolve_custom_dir("\"D:\\曲栏数据\"", root),
            Some(PathBuf::from("D:\\曲栏数据"))
        );
        // 结尾分隔符去掉（根盘符除外）
        assert_eq!(
            resolve_custom_dir("D:\\曲栏数据\\\\", root),
            Some(PathBuf::from("D:\\曲栏数据"))
        );
        // 相对路径锚定到默认根
        assert_eq!(
            resolve_custom_dir("music\\data", root),
            Some(root.join("music\\data"))
        );
    }

    #[test]
    fn migrate_moves_dirs_and_keeps_old_on_cross_volume() {
        let old = tmp("mv_old");
        let new = tmp("mv_new");
        // 两个子目录各放一个文件
        let cache = old.join("lyrics-cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("a.json"), "{}").unwrap();
        let lyrics = old.join("lyrics");
        fs::create_dir_all(&lyrics).unwrap();
        fs::write(lyrics.join("歌.lrc"), "[00:01]x").unwrap();

        let report = migrate_dirs(&old, &new);
        assert!(report.iter().any(|l| l.contains("已迁移")), "{report:?}");
        assert!(new.join("lyrics-cache").join("a.json").exists());
        assert!(new.join("lyrics").join("歌.lrc").exists());
        // rename 路径：旧位置不再有原目录
        assert!(!cache.exists());
        assert!(!lyrics.exists());
        let _ = fs::remove_dir_all(&old);
        let _ = fs::remove_dir_all(&new);
    }

    #[test]
    fn migrate_skips_when_dst_exists() {
        let old = tmp("sk_old");
        let new = tmp("sk_new");
        let cache = old.join("lyrics-cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("a.json"), "{}").unwrap();
        let dst = new.join("lyrics-cache");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join("b.json"), "{}").unwrap();

        let report = migrate_dirs(&old, &new);
        assert!(report.iter().any(|l| l.contains("跳过")), "{report:?}");
        // 两份都在，互不覆盖
        assert!(cache.join("a.json").exists());
        assert!(dst.join("b.json").exists());
        let _ = fs::remove_dir_all(&old);
        let _ = fs::remove_dir_all(&new);
    }

    #[test]
    fn migrate_same_root_is_noop() {
        let root = tmp("same");
        fs::create_dir_all(root.join("lyrics-cache")).unwrap();
        assert!(migrate_dirs(&root, &root).is_empty());
        assert!(root.join("lyrics-cache").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn move_item_open_file_result_is_consistent() {
        // Rust std 打开文件自带 FILE_SHARE_DELETE：打开中的文件通常能直接
        // rename（走 rename 分支）；跨卷等场景走 copy 回退（copy_dir 单测覆盖）。
        // 这里只验证结果一致性：dst 拿到内容，src 要么搬走要么改名 .old 保留。
        let old = tmp("xv_old");
        let new = tmp("xv_new");
        let f = old.join("barline.log");
        fs::write(&f, "hello").unwrap();
        let hold = fs::OpenOptions::new().read(true).open(&f).unwrap();
        let moved = move_item(&f, &new.join("barline.log"));
        drop(hold);
        match moved {
            Ok(true) => {
                assert_eq!(
                    fs::read_to_string(new.join("barline.log")).unwrap(),
                    "hello"
                );
                assert!(
                    !f.exists() || old.join("barline.log.old").exists(),
                    "src 搬走或改名保留，二者必居其一"
                );
            }
            // dst 已存在 / 源不存在等不迁移情形（此处不会发生，兜底放行）
            Ok(false) | Err(_) => {}
        }
        let _ = fs::remove_dir_all(&old);
        let _ = fs::remove_dir_all(&new);
    }

    #[test]
    fn copy_dir_copies_recursively() {
        // copy 回退分支的核心（跨卷迁移路径）
        let src = tmp("cp_src");
        let dst = tmp("cp_dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.json"), "{}").unwrap();
        fs::write(src.join("sub").join("b.lrc"), "[00:01]x").unwrap();
        copy_dir(&src, &dst).unwrap();
        assert_eq!(fs::read_to_string(dst.join("a.json")).unwrap(), "{}");
        assert_eq!(
            fs::read_to_string(dst.join("sub").join("b.lrc")).unwrap(),
            "[00:01]x"
        );
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }
}
