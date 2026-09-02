//! 「最近打开的文件」列表（M11，对应 spec §13 Q3）。
//!
//! 设计取舍：spec 原先以「需引入配置文件依赖」为由把 Q3 推迟到 MVP 之后。
//! 本实现**不引入任何新依赖**——仅用 `std::fs` 把路径按行写入平台配置目录下的
//! 纯文本文件，因此既满足了持久化需求，又不违背当初的推迟理由。
//!
//! 文件格式：每行一个绝对路径，**最近打开的在最前面**；最多 [`MAX_RECENTS`] 条。
//! 读取时会自动丢弃空行、重复项以及磁盘上已不存在的文件（避免失效条目堆积）。
//! 路径本身若含换行符则无法在该格式中往返，写入时跳过。

use std::path::{Path, PathBuf};

/// 最近文件列表最多保留的条数。
pub const MAX_RECENTS: usize = 10;

/// 最近打开的文件列表（按最近使用排序，索引 0 为最近一次）。
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Recents {
    entries: Vec<PathBuf>,
}

impl Recents {
    /// 从平台默认位置加载；位置不可用或读取失败时返回空列表。
    pub fn load() -> Self {
        match default_path() {
            Some(p) => Self::load_from(&p),
            None => Self::default(),
        }
    }

    /// 从指定文件加载。文件不存在时返回空列表（首次启动的正常情况）。
    pub fn load_from(path: &Path) -> Self {
        let mut entries: Vec<PathBuf> = Vec::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let p = PathBuf::from(line);
                // 只保留仍存在的文件，并顺带去重（保持原有先后顺序）。
                if p.is_file() && !entries.contains(&p) {
                    entries.push(p);
                }
            }
        }
        entries.truncate(MAX_RECENTS);
        Self { entries }
    }

    /// 记录一次成功打开：已存在则移到最前，超出上限则丢弃最旧的。
    pub fn push(&mut self, path: PathBuf) {
        self.entries.retain(|e| e != &path);
        self.entries.insert(0, path);
        self.entries.truncate(MAX_RECENTS);
    }

    /// 当前条目（最近优先）。
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    /// 清空列表。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 写回平台默认位置；失败只记日志，不打断用户操作。
    pub fn save(&self) {
        let Some(path) = default_path() else {
            return;
        };
        if let Err(e) = self.save_to(&path) {
            log::warn!("保存最近文件列表失败：{e}");
        }
    }

    /// 写入指定文件：先写临时文件再 `rename`，避免写入中断留下半截文件。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        for e in &self.entries {
            let s = e.to_string_lossy();
            // 含换行符的路径无法在按行格式中往返，直接跳过而非写坏文件。
            if s.contains('\n') {
                continue;
            }
            text.push_str(&s);
            text.push('\n');
        }
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }
}

/// 平台配置目录下的存储位置。
///
/// - macOS：`~/Library/Application Support/hyper-log/recent_files.txt`
/// - Windows：`%APPDATA%\hyper-log\recent_files.txt`
/// - 其它：`~/.config/hyper-log/recent_files.txt`
///
/// 取不到对应的环境变量时返回 `None`，此时最近文件功能退化为「仅本次会话内有效」。
fn default_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        return Some(
            PathBuf::from(home).join("Library/Application Support/hyper-log/recent_files.txt"),
        );
    }
    if cfg!(target_os = "windows") {
        let appdata = std::env::var_os("APPDATA")?;
        return Some(
            PathBuf::from(appdata)
                .join("hyper-log")
                .join("recent_files.txt"),
        );
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/hyper-log/recent_files.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hyper-log-recents-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn push_dedupes_orders_and_caps() {
        let mut r = Recents::default();
        for i in 0..(MAX_RECENTS + 5) {
            r.push(PathBuf::from(format!("/tmp/f{i}.log")));
        }
        // 上限生效
        assert_eq!(r.entries().len(), MAX_RECENTS);
        // 最近打开的排在最前
        assert_eq!(r.entries()[0], PathBuf::from("/tmp/f14.log"));

        // 重复 push 只是移到最前，不会新增条目
        r.push(PathBuf::from("/tmp/f10.log"));
        assert_eq!(r.entries()[0], PathBuf::from("/tmp/f10.log"));
        assert_eq!(r.entries().len(), MAX_RECENTS);
        assert!(!r.entries()[1..].contains(&PathBuf::from("/tmp/f10.log")));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = scratch_dir("roundtrip");
        let a = dir.join("a.log");
        let b = dir.join("b.log");
        std::fs::write(&a, b"x\n").unwrap();
        std::fs::write(&b, b"y\n").unwrap();

        let store = dir.join("recent_files.txt");
        let mut r = Recents::default();
        r.push(a.clone());
        r.push(b.clone());
        r.save_to(&store).unwrap();

        let loaded = Recents::load_from(&store);
        // 最近优先：后 push 的 b 在前
        assert_eq!(loaded.entries(), &[b, a][..]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = scratch_dir("missing");
        let store = dir.join("recent_files.txt");
        assert!(Recents::load_from(&store).entries().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_drops_missing_and_duplicate_entries() {
        let dir = scratch_dir("stale");
        let keep = dir.join("keep.log");
        std::fs::write(&keep, b"z\n").unwrap();
        let gone = dir.join("gone.log"); // 故意不创建
        let store = dir.join("recent_files.txt");
        std::fs::write(
            &store,
            format!(
                "{}\n{}\n{}\n\n",
                keep.display(),
                gone.display(),
                keep.display()
            ),
        )
        .unwrap();

        let loaded = Recents::load_from(&store);
        assert_eq!(loaded.entries(), &[keep][..]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_path_is_under_config_dir() {
        // 本机（macOS）应落在 Application Support 下；其它平台同理，仅校验非空与文件名。
        let p = default_path().expect("应能取到配置目录");
        assert_eq!(p.file_name().unwrap(), "recent_files.txt");
    }
}
