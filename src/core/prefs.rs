//! 用户偏好持久化（M17，对应 spec §1.3 待定 P2「序列化（配置）」）。
//!
//! 与 `recents.rs` 同一思路：不引入任何新依赖，仅用 `std::fs` 把偏好写入平台配置目录下的
//! 纯文本文件（零配置文件框架依赖，不违背 Q3 当初「需引入配置文件依赖」的推迟理由）。
//!
//! 格式为简单的 `key=value` 行；解析时忽略未知键、损坏文件整体回退到默认值，绝不 panic。
//! 主题用自有枚举 [`ThemePref`] 表达，避免在核心层引用 `egui`（核心层禁止依赖 GUI）。

use std::path::{Path, PathBuf};

/// 主题偏好。与 `egui::Theme` 解耦，在 GUI 层映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePref {
    #[default]
    Dark,
    Light,
}

impl ThemePref {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemePref::Dark => "dark",
            ThemePref::Light => "light",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ThemePref::Dark),
            "light" => Some(ThemePref::Light),
            _ => None,
        }
    }
}

/// 持久化偏好（窗口/主题/折行/侧栏/最近检索词）。
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub theme: ThemePref,
    pub wrap: bool,
    pub sidebar_visible: bool,
    pub window_w: f32,
    pub window_h: f32,
    pub recent_searches: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemePref::Dark,
            wrap: false,
            sidebar_visible: true,
            window_w: 1280.0,
            window_h: 860.0,
            recent_searches: Vec::new(),
        }
    }
}

const MAX_RECENT_SEARCHES: usize = 10;

impl Prefs {
    /// 从平台默认位置加载；位置不可用或读取失败时返回默认值。
    pub fn load() -> Self {
        match default_path() {
            Some(p) => Self::load_from(&p),
            None => Self::default(),
        }
    }

    /// 从指定文件加载。文件不存在或为空时返回默认值（首次启动的正常情况）。
    /// 解析容错：未知键忽略、非法值回退默认、损坏行跳过，绝不 panic。
    pub fn load_from(path: &Path) -> Self {
        let mut prefs = Prefs::default();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some((k, v)) = line.split_once('=') else {
                    continue;
                };
                match k {
                    "theme" => {
                        if let Some(t) = ThemePref::from_str(v) {
                            prefs.theme = t;
                        }
                    }
                    "wrap" => prefs.wrap = parse_bool(v),
                    "sidebar" => prefs.sidebar_visible = parse_bool(v),
                    "window" => {
                        if let Some((w, h)) = v.split_once(',')
                            && let (Ok(w), Ok(h)) =
                                (w.trim().parse::<f32>(), h.trim().parse::<f32>())
                            && w > 0.0
                            && h > 0.0
                        {
                            prefs.window_w = w;
                            prefs.window_h = h;
                        }
                    }
                    "recent" => {
                        let s = v.trim().to_string();
                        if !s.is_empty() && !prefs.recent_searches.contains(&s) {
                            prefs.recent_searches.push(s);
                        }
                    }
                    _ => {} // 未知键忽略
                }
            }
        }
        prefs.recent_searches.truncate(MAX_RECENT_SEARCHES);
        prefs
    }

    /// 记录一次检索词（去重、最近优先、上限 [`MAX_RECENT_SEARCHES`]）。
    pub fn push_recent_search(&mut self, term: &str) {
        let term = term.trim().to_string();
        if term.is_empty() {
            return;
        }
        self.recent_searches.retain(|e| e != &term);
        self.recent_searches.insert(0, term);
        self.recent_searches.truncate(MAX_RECENT_SEARCHES);
    }

    /// 写回平台默认位置；失败只记日志，不打断用户操作。
    pub fn save(&self) {
        if let Some(path) = default_path()
            && let Err(e) = self.save_to(&path)
        {
            log::warn!("保存偏好失败：{e}");
        }
    }

    /// 写入指定文件：先写临时文件再 `rename`，避免写入中断留下半截文件。
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        text.push_str(&format!("theme={}\n", self.theme.as_str()));
        text.push_str(&format!("wrap={}\n", i32::from(self.wrap)));
        text.push_str(&format!("sidebar={}\n", i32::from(self.sidebar_visible)));
        text.push_str(&format!(
            "window={:.0},{:.0}\n",
            self.window_w, self.window_h
        ));
        for r in &self.recent_searches {
            // 含换行符的词无法在按行格式中往返，跳过而非写坏文件。
            if r.contains('\n') {
                continue;
            }
            text.push_str("recent=");
            text.push_str(r);
            text.push('\n');
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(v.trim(), "1" | "true" | "yes" | "on")
}

/// 平台配置目录下的存储位置（与 `recents` 同目录，文件名不同）。
///
/// - macOS：`~/Library/Application Support/hyper-log/prefs.txt`
/// - Windows：`%APPDATA%\hyper-log\prefs.txt`
/// - 其它：`~/.config/hyper-log/prefs.txt`
fn default_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        return Some(PathBuf::from(home).join("Library/Application Support/hyper-log/prefs.txt"));
    }
    if cfg!(target_os = "windows") {
        let appdata = std::env::var_os("APPDATA")?;
        return Some(PathBuf::from(appdata).join("hyper-log").join("prefs.txt"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/hyper-log/prefs.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hyper-log-prefs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_when_missing() {
        let dir = scratch("missing");
        assert_eq!(Prefs::load_from(&dir.join("nope.txt")), Prefs::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip() {
        let dir = scratch("rt");
        let p = dir.join("prefs.txt");
        let mut prefs = Prefs {
            theme: ThemePref::Light,
            wrap: true,
            sidebar_visible: false,
            window_w: 1024.0,
            window_h: 768.0,
            ..Default::default()
        };
        prefs.push_recent_search("ERROR");
        prefs.push_recent_search("WARN");
        prefs.push_recent_search("ERROR"); // 重复 -> 移到最前，数量保持 2
        prefs.save_to(&p).unwrap();

        let loaded = Prefs::load_from(&p);
        assert_eq!(loaded.theme, ThemePref::Light);
        assert!(loaded.wrap);
        assert!(!loaded.sidebar_visible);
        assert_eq!(loaded.window_w, 1024.0);
        assert_eq!(loaded.window_h, 768.0);
        assert_eq!(
            loaded.recent_searches,
            vec!["ERROR".to_string(), "WARN".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_garbage_and_bad_window() {
        let dir = scratch("garbage");
        let p = dir.join("prefs.txt");
        // 未知主题/非法 bool/非法 window/空 recent/未知键 都应被安全忽略或回退默认
        std::fs::write(
            &p,
            "theme=neon\nwrap=maybe\nsidebar\nwindow=0,0\nrecent=\nfoo=bar\nrecent=ERROR\n",
        )
        .unwrap();
        let loaded = Prefs::load_from(&p);
        assert_eq!(loaded.theme, ThemePref::Dark);
        assert!(!loaded.wrap);
        assert_eq!(loaded.window_w, 1280.0);
        assert_eq!(loaded.recent_searches, vec!["ERROR".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_search_dedupes_and_caps() {
        let mut prefs = Prefs::default();
        for i in 0..15 {
            prefs.push_recent_search(&format!("q{i}"));
        }
        assert_eq!(prefs.recent_searches.len(), MAX_RECENT_SEARCHES);
        assert_eq!(prefs.recent_searches[0], "q14");
        assert!(!prefs.recent_searches.contains(&"q0".to_string()));
        let _ = std::fs::remove_dir_all(scratch("cap"));
    }
}
