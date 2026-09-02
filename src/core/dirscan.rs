//! 目录递归遍历：收集目录树下的日志文件（spec §7.7「打开目录」/ Q「查找全部」）。
//!
//! 设计要点：
//! - 只收集与日志相关的扩展名（`.log` / `.txt` / `.out`），跳过无关文件；
//! - 跳过隐藏目录/文件（`.` 或 `..` 开头）与符号链接（避免环与重复遍历）；
//! - 结果按路径排序，保证同一次遍历的确定性（便于复现与测试）；
//! - 遍历失败（权限不足等）静默跳过该条目，不中断整体收集。

use std::path::{Path, PathBuf};

/// 日志文件扩展名（小写比较）。
const LOG_EXTENSIONS: &[&str] = &["log", "txt", "out"];

/// 递归收集 `root` 目录树下的日志文件，按路径升序返回。
///
/// 若 `root` 本身是一个日志文件（而非目录），则直接返回该文件。
/// 目录不存在或不可读时返回空列表（由调用方提示）。
pub fn collect_log_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

/// 深度优先遍历。跳过隐藏条目与符号链接，避免符号链接环导致无限递归。
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    // 单文件：直接判定扩展名
    if dir.is_file() {
        if is_log_file(dir) {
            out.push(dir.to_path_buf());
        }
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // 权限不足 / 目录消失：静默跳过
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        // 跳过隐藏条目（.git、.DS_Store、.xxx 等）与符号链接
        if name.starts_with('.') || entry.file_type().is_ok_and(|t| t.is_symlink()) {
            continue;
        }

        if path.is_dir() {
            walk(&path, out);
        } else if is_log_file(&path) {
            out.push(path);
        }
    }
}

/// 扩展名是否为日志类型（大小写不敏感）。
fn is_log_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| LOG_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 生成唯一的临时目录名，避免并行测试间相互删除对方的数据。
    fn unique_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("hyperlog_dirscan_{tag}_{}_{n}", std::process::id()))
    }

    /// 在临时目录建一棵小文件树，返回 (root, 期望日志文件相对名列表)。
    fn build_tree() -> (PathBuf, Vec<&'static str>) {
        let root = unique_dir("tree");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".hidden_dir")).unwrap();

        // 日志文件（应被收集）
        std::fs::write(root.join("a.log"), b"a").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"b").unwrap();
        std::fs::write(root.join("sub").join("c.OUT"), b"c").unwrap(); // 大小写不敏感
        // 非日志 / 隐藏 / 目录（应被跳过）
        std::fs::write(root.join("readme.md"), b"x").unwrap();
        std::fs::write(root.join(".hidden"), b"x").unwrap();
        std::fs::write(root.join(".hidden_dir").join("d.log"), b"d").unwrap();
        std::fs::write(root.join("noext"), b"x").unwrap();

        (root, vec!["a.log", "sub/b.txt", "sub/c.OUT"])
    }

    #[test]
    fn collects_only_log_files_sorted() {
        let (root, expected) = build_tree();
        let files = collect_log_files(&root);
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, expected);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn single_log_file_returns_itself() {
        let (root, _) = build_tree();
        let f = root.join("a.log");
        assert_eq!(collect_log_files(&f), vec![f]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_dir_returns_empty() {
        let p = unique_dir("none");
        assert!(collect_log_files(&p).is_empty());
    }
}
