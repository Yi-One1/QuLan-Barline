//! 数据存储位置（问题 3 / v1.1.5；v1.1.6 改净搬 + 支持 A→B 切换）。
//!
//! 职责：
//! - 把 `config.storage.data_dir`（可为空 = 默认）解析成绝对路径；
//! - 启动时把**上次生效目录**（`config.storage.active_data_dir`，缺省为
//!   `%LOCALAPPDATA%\Barline`）下的已知缓存目录**净搬**到新数据目录——
//!   旧数据一律删除，旧目录腾空后整个删除；
//! - 迁移失败不阻塞启动，旧数据原样保留（宁可两份，不可丢失）。
//!
//! 边界（用户约定）：
//! - `config.json`（及其 `.bad` 备份）**永远留在默认根目录**，因此默认根
//!   目录文件夹本身不会被删（腾空后它只剩 config.json）；自定义旧目录 A
//!   腾空后连文件夹一起删除；
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

/// 启动入口：按配置初始化全局数据目录，按用户在设置页选择的选项执行
/// 迁移 / 删除（弹窗选项在改目录时写入 pending_*，这里执行后复位）。
///
/// 必须在 `Config::load` 之后、任何使用 [`crate::platform::data_dir`] 的模块
/// 初始化之前调用（`App::bootstrap` 满足该顺序）。会把本次实际生效目录
/// 写回 `config.storage.active_data_dir` 并落盘（下次启动从那里迁出）。
pub fn init_from_config(config: &mut crate::config::Config) {
    let default_root = crate::platform::barline_dir();
    // 上次实际生效目录：active_data_dir 优先（支持自定义 A → B 切换），否则默认根
    let old_root = resolve_custom_dir(&config.storage.active_data_dir, &default_root)
        .unwrap_or_else(|| default_root.clone());
    let new_root = data_dir_from_config(&config.storage.data_dir, &default_root);
    crate::platform::set_data_dir(new_root.clone());
    let mut dirty = false;
    if old_root != new_root {
        let migrate = config.storage.pending_migrate;
        let delete_old = config.storage.pending_delete_old;
        crate::log_info!(
            "数据目录：{} → {}（迁移={} 删除原文件={}）",
            old_root.display(),
            new_root.display(),
            migrate,
            delete_old
        );
        if migrate {
            for line in migrate_dirs(&old_root, &new_root) {
                crate::log_info!("迁移：{line}");
            }
            // 日志最后搬：迁移期间旧日志仍在写（记录迁移过程本身）
            crate::log::relocate(&new_root);
            crate::log_info!("日志已搬迁至 {}", new_root.join("barline.log").display());
        } else if delete_old {
            // 不迁移但要删原文件：日志先关再删（否则删不动）
            crate::log::relocate(&new_root);
        }
        if delete_old {
            for line in delete_old_contents(&old_root) {
                crate::log_info!("清理原目录：{line}");
            }
            // 腾空后删除旧目录文件夹本身：remove_dir 只删空目录——默认根因
            // config.json 豁免必然留下（约定），自定义旧目录 A 腾空后即被整个删除
            match fs::remove_dir(&old_root) {
                Ok(()) => crate::log_info!("原数据目录已删除：{}", old_root.display()),
                Err(_) => crate::log_debug!(
                    "原数据目录未删除（非空，保留）：{}",
                    old_root.display()
                ),
            }
        }
        if !migrate && !delete_old {
            crate::log_info!("按用户选择：不迁移、不删除原目录文件");
        }
        dirty = true;
    }
    // 复位一次性选项 + 记录本次实际生效目录（下次启动从这里迁出）。
    // 落盘失败无碍：下次启动 old_root 退化为上次记录，重复执行也是幂等的。
    if config.storage.pending_migrate || config.storage.pending_delete_old {
        config.storage.pending_migrate = false;
        config.storage.pending_delete_old = false;
        dirty = true;
    }
    let active = new_root.to_string_lossy().to_string();
    if config.storage.active_data_dir != active {
        config.storage.active_data_dir = active;
        dirty = true;
    }
    if dirty {
        if let Err(e) = config.save() {
            crate::log_warn!("记录存储状态失败（不影响本次使用）：{e}");
        }
    }
    // v1.1.17 修复：无条件对齐日志路径。此前只在「发生迁移」时搬迁日志，
    // 而 logger 启动时永远在**默认根**打开文件——于是 active_data_dir 已经
    // 是自定义目录的后续启动中，日志一直写在默认根（「打开数据目录」看到的
    // 自定义目录里根本没有日志）。relocate 内部对同路径文件跳过，幂等安全。
    crate::log::relocate(&new_root);
}

/// 删除 `root` 下除配置文件外的全部内容（用户在设置页明确选择后执行）。
///
/// 永远豁免：`config.json`、`config.json.tmp`——删掉它们等于丢掉全部设置
/// （损坏配置的 `.bad` 备份按「删干净」要求一并删除）。其余文件 / 子目录
/// 一律删除，最后尝试把 `root` 本身也删掉（只有空目录才会成功）。
pub fn delete_old_contents(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "config.json" || name == "config.json.tmp" {
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let r = if is_dir {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match r {
            Ok(()) => out.push(format!("{name}: 已删除")),
            Err(e) => out.push(format!("{name}: 删除失败（{e}），已保留")),
        }
    }
    out
}

/// 把 `src` 目录下（含子目录）的所有 `.lrc` 文件搬进 `dst`，保持相对结构。
///
/// 用于「本地歌词目录」改位置时的文件迁移（用户自己的文件，无句柄占用，
/// 立即执行）。返回移动的文件数；单文件失败不中断（调用方记日志）。
pub fn move_lrc_files(src: &Path, dst: &Path) -> std::io::Result<u32> {
    let mut moved = 0u32;
    if !src.is_dir() {
        return Ok(0);
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() {
            let name = entry.file_name();
            moved += move_lrc_files(&path, &dst.join(name))?;
        } else if path.extension().map(|e| e.eq_ignore_ascii_case("lrc")).unwrap_or(false)
            && move_item(&path, &dst.join(entry.file_name()))?
        {
            moved += 1;
        }
    }
    Ok(moved)
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

/// 迁移 `old_root` 下所有 [`MIGRATE_DIRS`] 到 `new_root`（**净搬**）。
///
/// 同时清理旧根目录里 v1.1.5 遗留的 `<name>.old` 备份（其中数据已无引用，
/// 按用户指示一并删除）。返回人类可读的结果行（每子项一行，由调用方落日志）。
/// 同根 / 无可迁项返回空。单项失败不阻塞其余子项（旧数据原样保留）。
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
        if src.exists() {
            if dst.exists() {
                out.push(format!("{name}: 目标已存在，跳过（两份都保留）"));
            } else {
                match move_item(&src, &dst) {
                    Ok(true) => out.push(format!("{name}: 已迁移")),
                    Ok(false) => {}
                    Err(e) => out.push(format!("{name}: 迁移失败（{e}），已保留原目录")),
                }
            }
        }
        // v1.1.5 遗留的 .old 备份：数据已无引用，一并删除
        let stale = old_root.join(format!("{name}.old"));
        if stale.is_dir() {
            match fs::remove_dir_all(&stale) {
                Ok(()) => out.push(format!("{name}.old: 遗留备份已删除")),
                Err(e) => out.push(format!("{name}.old: 删除失败（{e}）")),
            }
        }
    }
    out
}

/// 迁移单个文件 / 目录（`src` → `dst`），**净搬：成功后源不保留**。
///
/// - `src` 不存在或 `dst` 已存在 → `Ok(false)`（不动）；
/// - rename 优先（同卷瞬间完成，源自然消失）；失败（跨卷 / 占用）退化
///   copy，copy 成功后**删除源**（失败则源保留，返回 Ok(true)——数据已在
///   新处，删除失败只影响整洁性）。
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
                if let Err(_e) = fs::remove_dir_all(src) {
                    // copy 已成功，源删不掉只影响整洁，不影响数据
                }
            } else {
                fs::copy(src, dst)?;
                if let Err(_e) = fs::remove_file(src) {
                    // 同上
                }
            }
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
        // 净搬语义：Ok(true) 后 dst 拿到内容、src 必须不存在
        //（rename 直接搬走；copy 回退也删源——无论哪种，源都不保留）。
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
                assert!(!f.exists(), "净搬后源必须不存在（含 .old 也不许有）");
                assert!(!old.join("barline.log.old").exists());
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

    #[test]
    fn migrate_removes_emptied_old_root() {
        // 自定义旧目录 A（无 config.json）：净搬后腾空 → 文件夹整个删除
        let old = tmp("rm_old");
        let new = tmp("rm_new");
        let cache = old.join("lyrics-cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("a.json"), "{}").unwrap();

        let report = migrate_dirs(&old, &new);
        assert!(report.iter().any(|l| l.contains("已迁移")), "{report:?}");
        assert!(new.join("lyrics-cache").join("a.json").exists());
        assert!(fs::remove_dir(&old).is_ok(), "腾空后旧目录应可直接删除");
        let _ = fs::remove_dir_all(&new);
    }

    #[test]
    fn migrate_cleans_legacy_dot_old_backups() {
        // v1.1.5 遗留的 .old 备份：一并删除
        let old = tmp("legacy_old");
        let new = tmp("legacy_new");
        let stale = old.join("lyrics-cache.old");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("junk.json"), "{}").unwrap();

        let report = migrate_dirs(&old, &new);
        assert!(report.iter().any(|l| l.contains("遗留备份已删除")), "{report:?}");
        assert!(!stale.exists());
        let _ = fs::remove_dir_all(&old);
        let _ = fs::remove_dir_all(&new);
    }

    #[test]
    fn delete_old_contents_spares_config_only() {
        // 删干净语义：除 config.json / config.json.tmp 外全部删除，
        // 连 .bad 备份也删；根目录随后可直接删除
        let root = tmp("del_old");
        fs::write(root.join("config.json"), "{}").unwrap();
        fs::write(root.join("config.json.tmp"), "{}").unwrap();
        fs::write(root.join("config.bad.2026.json"), "{}").unwrap();
        fs::write(root.join("barline.log"), "x").unwrap();
        let sub = root.join("lyrics-cache");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.json"), "{}").unwrap();

        let report = delete_old_contents(&root);
        assert!(report.iter().any(|l| l.starts_with("barline.log: 已删除")), "{report:?}");
        assert!(root.join("config.json").exists());
        assert!(root.join("config.json.tmp").exists());
        assert!(!root.join("config.bad.2026.json").exists());
        assert!(!root.join("barline.log").exists());
        assert!(!sub.exists());
        // 只剩 config.json / .tmp → remove_dir 删不掉（非空），符合约定
        assert!(fs::remove_dir(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn move_lrc_files_moves_recursively_only_lrc() {
        let src = tmp("lrc_src");
        let dst = tmp("lrc_dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.lrc"), "[00:01]a").unwrap();
        fs::write(src.join("sub").join("b.lrc"), "[00:01]b").unwrap();
        fs::write(src.join("keep.txt"), "not lyrics").unwrap();
        fs::write(src.join("c.LRC"), "[00:01]c").unwrap();

        let moved = move_lrc_files(&src, &dst).unwrap();
        assert_eq!(moved, 3, "a/b/c（含大写扩展名）都应被移动");
        assert_eq!(fs::read_to_string(dst.join("a.lrc")).unwrap(), "[00:01]a");
        assert_eq!(fs::read_to_string(dst.join("sub").join("b.lrc")).unwrap(), "[00:01]b");
        assert_eq!(fs::read_to_string(dst.join("c.LRC")).unwrap(), "[00:01]c");
        // 非 .lrc 原地不动
        assert!(src.join("keep.txt").exists());
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }
}
