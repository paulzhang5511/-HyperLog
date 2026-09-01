//! 日志行着色：把一行文本切分为普通 / 级别 / 命中 三种分段。
//!
//! 纯函数（`segments`），不依赖 `egui`，可脱离 GUI 单测（spec §7.6，修复 D2）。

use regex::Regex;

/// 日志级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Fatal,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    Verbose,
}

/// 级别匹配正则：最长候选置前，避免 `WARN` 抢在 `WARNING` 之前截断匹配（spec §7.6）。
/// `(?-u)` 关闭 Unicode 词边界，使 `\b` 仅按 ASCII 单词边界判定，符合日志级别形态。
const LEVEL_PATTERN: &str = r"(?-u)\b(FATAL|ERROR|WARNING|WARN|VERBOSE|TRACE|DEBUG|INFO|E|W|I|D)\b";

/// 着色器：持有一组预编译正则，对每行输出分段列表。
pub struct Highlighter {
    level_re: Regex,
    /// 检索命中正则，仅在进行检索时为 `Some`（与检索复用同一 `Regex`，修复 G5）。
    hit_re: Option<Regex>,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    /// 仅启用级别高亮的着色器。
    pub fn new() -> Self {
        Self {
            level_re: Regex::new(LEVEL_PATTERN).expect("LEVEL_PATTERN 必须是合法正则"),
            hit_re: None,
        }
    }

    /// 额外绑定一个检索命中正则（与 `core::search` 编译出的 `Regex` 复用）。
    ///
    /// 供 M4 检索结果高亮使用（见 spec §7.6），M3 阶段尚未接入搜索流。
    #[allow(dead_code)]
    pub fn with_hit(mut self, hit_re: Regex) -> Self {
        self.hit_re = Some(hit_re);
        self
    }

    #[allow(dead_code)]
    pub fn level_re(&self) -> &Regex {
        &self.level_re
    }

    #[allow(dead_code)]
    pub fn hit_re(&self) -> Option<&Regex> {
        self.hit_re.as_ref()
    }
}

/// 一行中的一段：普通文本、级别词、或检索命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Plain(&'a str),
    Level(&'a str, Level),
    Hit(&'a str),
}

/// 把匹配到的级别词归类到 [`Level`]。
pub fn classify(token: &str) -> Level {
    match token.to_ascii_uppercase().as_str() {
        "FATAL" => Level::Fatal,
        "ERROR" | "E" => Level::Error,
        "WARN" | "WARNING" | "W" => Level::Warn,
        "INFO" | "I" => Level::Info,
        "DEBUG" | "D" => Level::Debug,
        "TRACE" => Level::Trace,
        "VERBOSE" => Level::Verbose,
        _ => Level::Info,
    }
}

/// 把一行切分为有序分段。**关键不变式**：所有分段的文本拼接后等于原行（无损，G5/D2）。
///
/// 扫描时取「起始位置最早」的匹配；级别与命中同起点时优先级别。命中正则缺省时忽略命中段。
pub fn segments<'a>(line: &'a str, h: &Highlighter) -> Vec<Segment<'a>> {
    let mut out = Vec::new();
    let mut cursor = 0usize; // 已消费到的字节位置
    let mut scan = 0usize; // 下一次搜索起点

    while scan < line.len() {
        let level = h.level_re.find_at(line, scan);
        let hit = h.hit_re.as_ref().and_then(|r| r.find_at(line, scan));

        let (start, end, kind) = match (level, hit) {
            (Some(l), Some(hm)) => {
                if l.start() <= hm.start() {
                    (l.start(), l.end(), Kind::Level)
                } else {
                    (hm.start(), hm.end(), Kind::Hit)
                }
            }
            (Some(l), None) => (l.start(), l.end(), Kind::Level),
            (None, Some(hm)) => (hm.start(), hm.end(), Kind::Hit),
            (None, None) => break,
        };

        if start > cursor {
            out.push(Segment::Plain(&line[cursor..start]));
        }
        let text = &line[start..end];
        out.push(match kind {
            Kind::Level => Segment::Level(text, classify(text)),
            Kind::Hit => Segment::Hit(text),
        });
        cursor = end;
        scan = end;
    }

    if cursor < line.len() {
        out.push(Segment::Plain(&line[cursor..]));
    }
    out
}

#[derive(Clone, Copy)]
enum Kind {
    Level,
    Hit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels_of(line: &str) -> Vec<(String, Level)> {
        let h = Highlighter::new();
        segments(line, &h)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Level(t, l) => Some((t.to_string(), l)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn error_level_detected() {
        let v = levels_of("2026-09-01 10:00 ERROR database connection refused");
        assert!(v.contains(&("ERROR".to_string(), Level::Error)));
    }

    #[test]
    fn warning_longest_match_wins() {
        // WARNING 必须整体匹配，不能截断成 WARN + "ING"
        let v = levels_of("WARN WARNING WARN");
        assert!(v.contains(&("WARNING".to_string(), Level::Warn)));
        assert!(v.contains(&("WARN".to_string(), Level::Warn)));
    }

    #[test]
    fn fatal_and_debug_detected() {
        let v = levels_of("FATAL out of memory; DEBUG entering handler");
        assert!(v.contains(&("FATAL".to_string(), Level::Fatal)));
        assert!(v.contains(&("DEBUG".to_string(), Level::Debug)));
    }

    #[test]
    fn errors_equals_three_not_highlighted() {
        // D2：errors=3 中的 "ERROR" 是大小写不一致 + 非词边界，不得被识别
        let v = levels_of("retry config: errors=3 max attempts");
        assert!(v.is_empty(), "errors=3 不应被识别为级别，实际: {:?}", v);
    }

    #[test]
    fn information_and_terror_not_levels() {
        // INFORMATION 含 INFO 但无词边界；TERROR 中 ERROR 前无词边界
        let v = levels_of("INFORMATION schema queried; TERROR is not a level");
        assert!(v.is_empty(), "INFORMATION/TERROR 不应被识别，实际: {:?}", v);
    }

    #[test]
    fn single_letter_level_works_in_isolation() {
        let v = levels_of("E: something failed (W)");
        assert!(v.contains(&("E".to_string(), Level::Error)));
        assert!(v.contains(&("W".to_string(), Level::Warn)));
    }

    #[test]
    fn segments_are_lossless() {
        // G5/D2 不变式：拼接所有分段必须等于原行
        let cases = [
            "2026-09-01 10:00:00.004 ERROR database connection refused",
            "plain line without any level",
            "WARN WARNING WARN INFO DEBUG TRACE",
            "retry config: errors=3 max attempts; INFORMATION; TERROR",
            "FATAL out of memory",
        ];
        for c in cases {
            let h = Highlighter::new();
            let joined: String = segments(c, &h)
                .iter()
                .map(|s| match s {
                    Segment::Plain(t) | Segment::Level(t, _) | Segment::Hit(t) => *t,
                })
                .collect();
            assert_eq!(joined, c, "分段拼接应无损");
        }
    }
}
