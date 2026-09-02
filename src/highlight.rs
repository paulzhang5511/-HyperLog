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

/// 时间戳匹配正则：覆盖三种常见日志时间形态，分支按"信息量从多到少"排列，
/// 使 `2026-09-01 10:00:01.123` 整体匹配而不是退化成 `10:00:01.123`。
///   - `[yyyy-MM-dd HH:mm:ss(.SSS)]` ISO 风格（Java / Log4j / Spring）
///   - `[MM-dd HH:mm:ss(.SSS)]`     logcat / syslog 风格（无年份）
///   - `[HH:mm:ss(.SSS)]`           纯时间（行首无日期）
const TIMESTAMP_PATTERN: &str = concat!(
    r"\d{4}[-/]\d{2}[-/]\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?",
    r"|\d{2}[-/]\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d{1,9})?",
    r"|\d{2}:\d{2}:\d{2}[.,]\d{1,9}",
);

/// 着色器：持有一组预编译正则，对每行输出分段列表。
#[derive(Clone)]
pub struct Highlighter {
    level_re: Regex,
    ts_re: Regex,
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
            ts_re: Regex::new(TIMESTAMP_PATTERN).expect("TIMESTAMP_PATTERN 必须是合法正则"),
            hit_re: None,
        }
    }

    /// 额外绑定一个检索命中正则（与 `core::search` 编译出的 `Regex` 复用），
    /// 用于检索结果命中高亮（spec §7.6，修复 G5）。
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

/// 一行中的一段：普通文本、时间戳、级别词、或检索命中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Plain(&'a str),
    /// 时间戳：日志的"结构"部分，着色时统一压暗，让级别与正文更突出。
    Timestamp(&'a str),
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
/// 扫描时取「起始位置最早」的匹配；多个规则同起点时按
/// `Level > Timestamp > Hit` 的优先级取其一（数组顺序即优先级）。
/// 命中正则缺省时忽略命中段。
pub fn segments<'a>(line: &'a str, h: &Highlighter) -> Vec<Segment<'a>> {
    let mut out = Vec::new();
    let mut cursor = 0usize; // 已消费到的字节位置
    let mut scan = 0usize; // 下一次搜索起点

    while scan < line.len() {
        let level = h.level_re.find_at(line, scan);
        let ts = h.ts_re.find_at(line, scan);
        let hit = h.hit_re.as_ref().and_then(|r| r.find_at(line, scan));

        let mut best: Option<(usize, usize, Kind)> = None;
        for (m, kind) in [
            (level, Kind::Level),
            (ts, Kind::Timestamp),
            (hit, Kind::Hit),
        ] {
            if let Some(m) = m
                && best.is_none_or(|(start, _, _)| m.start() < start)
            {
                best = Some((m.start(), m.end(), kind));
            }
        }
        let Some((start, end, kind)) = best else {
            break;
        };

        if start > cursor {
            out.push(Segment::Plain(&line[cursor..start]));
        }
        let text = &line[start..end];
        out.push(match kind {
            Kind::Level => Segment::Level(text, classify(text)),
            Kind::Timestamp => Segment::Timestamp(text),
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
    Timestamp,
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
            "09-01 10:00:01.123  1234  5678 E ActivityManager: 崩溃了",
        ];
        for c in cases {
            let h = Highlighter::new();
            let joined: String = segments(c, &h)
                .iter()
                .map(|s| match s {
                    Segment::Plain(t)
                    | Segment::Level(t, _)
                    | Segment::Hit(t)
                    | Segment::Timestamp(t) => *t,
                })
                .collect();
            assert_eq!(joined, c, "分段拼接应无损");
        }
    }

    fn timestamps_of(line: &str) -> Vec<String> {
        let h = Highlighter::new();
        segments(line, &h)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Timestamp(t) => Some(t.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn iso_timestamp_detected() {
        // 毫秒部分必须并入时间戳，不能被切成 Plain
        let v = timestamps_of("2026-09-01 10:00:00.004 ERROR database connection refused");
        assert_eq!(v, vec!["2026-09-01 10:00:00.004".to_string()]);
    }

    #[test]
    fn logcat_timestamp_detected() {
        // Android logcat：无年份的 `MM-dd HH:mm:ss.mmm`
        let v = timestamps_of("09-01 10:00:01.123  1234  5678 E ActivityManager: crash");
        assert_eq!(v, vec!["09-01 10:00:01.123".to_string()]);
    }

    #[test]
    fn bare_time_timestamp_detected() {
        let v = timestamps_of("10:00:01,123 INFO started");
        assert_eq!(v, vec!["10:00:01,123".to_string()]);
    }

    #[test]
    fn partial_time_is_not_timestamp() {
        // 只有时分、没有秒不算时间戳（避免把任意 `10:00` 染色）
        assert!(timestamps_of("at 10:00 the job started").is_empty());
        // 普通长度数字序列也不算
        assert!(timestamps_of("retry=1234567 max=3").is_empty());
    }

    #[test]
    fn timestamp_and_level_coexist() {
        // 时间戳与级别互不吞并：两段都要出现，且顺序正确
        let line = "2026-09-01 10:00:00 ERROR boom";
        let h = Highlighter::new();
        let v = segments(line, &h);
        assert!(matches!(v.first(), Some(Segment::Timestamp(_))));
        assert!(v.iter().any(|s| matches!(s, Segment::Level("ERROR", _))));
    }
}
