//! 目录内全文检索（"查找全部"）：对目录树下的每个日志文件建索引并检索，
//! 流式回传带文件路径的命中结果（spec §7.7「查找全部」）。
//!
//! 与 [`crate::core::search::run_search`] 的关系：
//! - 后者针对「已打开」的 [`FileSet`]（常驻 mmap 索引），做跨文件字节级并行检索；
//! - 本模块针对「目录」：遍历文件 → 逐个 [`LogFileIndex::open`] → 复用同样的
//!   `scan_chunk` 字节级匹配 → 命中附带文件路径。
//!
//! 结果按文件顺序流式回传，命中行以 [`GrepHit`]（含文件路径）表示，供结果页展示归属。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::core::dirscan::collect_log_files;
use crate::core::indexer::LogFileIndex;
use crate::core::search::{CancelToken, SearchOptions, build_regex_bytes};

/// 单条目录检索命中：文件路径 + 文件内行号 + 行文本（结果页直接展示，无需回查索引）。
///
/// 与 [`SearchHit`]（仅坐标）不同，这里**内联行文本**——目录检索的文件索引是临时的，
/// 结果页展示后即释放，不能依赖后续回查。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepHit {
    /// 相对根目录的展示路径（或文件名，见 [`GrepOptions::base`]）。
    pub display_path: String,
    /// 命中文件的绝对路径（点击结果跳转原文时据此打开并定位）。
    pub abs_path: PathBuf,
    /// 文件内行号（1-based，符合编辑器习惯）。
    pub line_number: usize,
    /// 该行文本（lossy）。
    pub line: String,
}

/// 目录检索配置。
#[derive(Clone, Debug)]
pub struct GrepOptions {
    /// 检索模式与大小写。
    pub search: SearchOptions,
    /// 命中上限（达上限后截断，复用 G6 语义）。
    pub max_hits: usize,
    /// 根目录（用于把绝对路径裁剪为相对展示路径；`None` 则显示文件名）。
    pub base: Option<PathBuf>,
}

/// 目录检索后台 → UI 的消息。
#[derive(Debug)]
pub enum GrepMessage {
    /// 增量命中（一个文件一批）。
    Partial { hits: Vec<GrepHit> },
    /// 进度：(已扫描文件数, 总文件数, 已扫描字节)。
    Progress {
        files_done: usize,
        files_total: usize,
        bytes_done: u64,
    },
    /// 命中达上限截断。
    Truncated { hits: usize },
    /// 完成。
    Completed {
        hits: usize,
        files: usize,
        elapsed: Duration,
    },
    /// 失败（无匹配文件 / 全部 IO 错误等）。
    Failed(String),
    /// 已取消。
    Cancelled,
}

/// 对 `root` 目录执行全文检索，结果经 `tx` 流式回传。
///
/// 调用方负责在后台线程中 `spawn`。单个文件检索复用 [`crate::core::search::scan_chunk`]
/// 的字节级匹配；文件之间串行（目录文件通常较多但单个不大，串行可简化取消与进度语义）。
pub fn run_grep(
    root: PathBuf,
    pattern: &str,
    options: &GrepOptions,
    cancel: &CancelToken,
    tx: &Sender<GrepMessage>,
) {
    let start = Instant::now();
    let re = match build_regex_bytes(&options.search, pattern) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(GrepMessage::Failed(e.to_string()));
            return;
        }
    };

    let files = collect_log_files(&root);
    let total_files = files.len();
    if total_files == 0 {
        let _ = tx.send(GrepMessage::Failed(format!(
            "目录 {} 下未找到日志文件（.log/.txt/.out）",
            root.display()
        )));
        return;
    }

    let mut total_hits = 0usize;
    let mut files_done = 0usize;
    let mut bytes_done = 0u64;

    for path in &files {
        if cancel.is_cancelled() {
            let _ = tx.send(GrepMessage::Cancelled);
            return;
        }

        // 单个文件：建索引 → 分块字节级匹配 → 命中映射回行号。
        let idx = match LogFileIndex::open(path) {
            Ok(i) => i,
            Err(_) => {
                // 空文件 / IO 错误：跳过该文件，不中断整体检索。
                files_done += 1;
                let _ = tx.send(GrepMessage::Progress {
                    files_done,
                    files_total: total_files,
                    bytes_done,
                });
                continue;
            }
        };

        let display = display_path(&idx.path, options.base.as_deref());
        let remaining = options.max_hits.saturating_sub(total_hits);
        let file_hits = search_one_file(&idx, &re, &display, remaining, &mut total_hits, cancel);

        bytes_done += idx.byte_len() as u64;
        files_done += 1;

        if !file_hits.is_empty() {
            let _ = tx.send(GrepMessage::Partial { hits: file_hits });
        }
        let _ = tx.send(GrepMessage::Progress {
            files_done,
            files_total: total_files,
            bytes_done,
        });

        if total_hits >= options.max_hits {
            let _ = tx.send(GrepMessage::Truncated {
                hits: options.max_hits,
            });
            return;
        }
    }

    let _ = tx.send(GrepMessage::Completed {
        hits: total_hits,
        files: total_files,
        elapsed: start.elapsed(),
    });
}

/// 检索单个文件，返回该文件的命中（已内联行文本与展示路径）。
///
/// 复用 `search::scan_chunk` 的字节级匹配逻辑，但把坐标 `SearchHit` 展开为 `GrepHit`
/// （内联行文本），因为目录检索的文件索引是临时的。
///
/// `limit` 是剩余命中额度：返回的命中数不超过 `limit`，到达后提前停止扫描。
fn search_one_file(
    idx: &LogFileIndex,
    re: &regex::bytes::Regex,
    display: &str,
    limit: usize,
    total_hits: &mut usize,
    cancel: &CancelToken,
) -> Vec<GrepHit> {
    let mut out = Vec::new();
    let n = idx.line_count();
    if n == 0 || limit == 0 {
        return out;
    }

    let mut last_li: Option<usize> = None;
    'outer: for (lo, hi) in idx.chunk_bounds(8 * 1024 * 1024) {
        // 块间检查取消令牌：大文件整扫时也能在数毫秒内响应「停止查找全部」（G1）。
        if cancel.is_cancelled() {
            break 'outer;
        }
        let (bl, bh) = idx.byte_range(lo, hi);
        let slice = &idx.raw_bytes()[bl..bh];
        for m in re.find_iter(slice) {
            let (s, e) = (m.start(), m.end());
            if slice[s..e].contains(&b'\n') {
                continue;
            }
            let li = idx.line_index_of_byte(bl + s);
            if last_li != Some(li) {
                last_li = Some(li);
                if let Some(line) = idx.line(li) {
                    out.push(GrepHit {
                        display_path: display.to_owned(),
                        abs_path: idx.path.clone(),
                        line_number: li + 1,
                        line: line.into_owned(),
                    });
                    *total_hits += 1;
                    if out.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
    }
    out
}

/// 计算展示路径：相对 `base` 的路径；无 `base` 时退化为文件名。
fn display_path(path: &std::path::Path, base: Option<&std::path::Path>) -> String {
    if let Some(b) = base
        && let Ok(rel) = path.strip_prefix(b)
    {
        return rel.to_string_lossy().into_owned();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::dirscan::collect_log_files;
    use crate::core::search::{CancelToken, SearchMode, SearchOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// 生成唯一的临时目录名，避免并行测试间相互删除对方的数据。
    fn tmp_dir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("hyperlog_grep_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        d
    }

    fn opts(case_sensitive: bool, regex: bool) -> GrepOptions {
        GrepOptions {
            search: SearchOptions {
                mode: if regex {
                    SearchMode::Regex
                } else {
                    SearchMode::Plain
                },
                case_sensitive,
                max_hits: 2_000_000,
            },
            max_hits: 2_000_000,
            base: None,
        }
    }

    fn drain(rx: crossbeam_channel::Receiver<GrepMessage>) -> (usize, bool, Vec<GrepHit>) {
        let mut hits = Vec::new();
        let mut total = 0;
        let mut completed = false;
        while let Ok(m) = rx.recv() {
            match m {
                GrepMessage::Partial { hits: h } => hits.extend(h),
                GrepMessage::Completed { hits, .. } => {
                    total = hits;
                    completed = true;
                }
                GrepMessage::Truncated { .. } => break,
                GrepMessage::Failed(_) | GrepMessage::Cancelled => break,
                GrepMessage::Progress { .. } => {}
            }
        }
        (total, completed, hits)
    }

    #[test]
    fn finds_across_nested_files_with_paths() {
        let d = tmp_dir();
        std::fs::write(d.join("a.log"), b"alpha ERROR one\nbeta\n").unwrap();
        std::fs::write(d.join("sub").join("b.log"), b"x\nERROR again\n").unwrap();
        std::fs::write(d.join("readme.md"), b"ERROR not a log\n").unwrap(); // 非日志，应跳过

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut opt = opts(false, false);
        opt.base = Some(d.clone()); // 显示相对根目录的路径
        run_grep(d.clone(), "ERROR", &opt, &CancelToken::new(), &tx);
        drop(tx);

        let (total, completed, hits) = drain(rx);
        assert!(completed);
        assert_eq!(total, 2, "两个日志文件各一条 ERROR");
        // 命中带行文本与文件路径（相对根目录）
        assert_eq!(hits.len(), 2);
        let paths: Vec<&str> = hits.iter().map(|h| h.display_path.as_str()).collect();
        assert!(paths.contains(&"a.log"));
        // `display_path` 用平台原生分隔符渲染（Windows 为 `\`），故期望值也按 `MAIN_SEPARATOR` 构建，
        // 避免 macOS 通过、Windows 失败的跨平台测试漂移。
        let nested = format!("sub{}b.log", std::path::MAIN_SEPARATOR);
        assert!(paths.contains(&nested.as_str()));
        // abs_path 必须指向真实磁盘文件（点击结果跳转原文依赖它）。
        for h in &hits {
            assert!(
                h.abs_path.is_absolute(),
                "abs_path 应为绝对路径：{:?}",
                h.abs_path
            );
            assert!(
                h.abs_path.exists(),
                "abs_path 应指向存在的文件：{:?}",
                h.abs_path
            );
        }

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_dir_sends_failed() {
        let d = tmp_dir();
        let (tx, rx) = crossbeam_channel::unbounded();
        let opt = opts(false, false);
        run_grep(d.clone(), "x", &opt, &CancelToken::new(), &tx);
        drop(tx);
        assert!(matches!(rx.recv(), Ok(GrepMessage::Failed(_))));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn regex_mode_and_case_sensitivity() {
        let d = tmp_dir();
        std::fs::write(d.join("a.log"), b"ERR_001\nok\nERR_002\n").unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let opt = opts(true, true); // 大小写敏感 + 正则
        run_grep(d.clone(), r"ERR_\d+", &opt, &CancelToken::new(), &tx);
        drop(tx);
        let (total, completed, _) = drain(rx);
        assert!(completed);
        assert_eq!(total, 2);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn max_hits_truncates() {
        let d = tmp_dir();
        let mut content = String::new();
        for i in 0..100 {
            content.push_str(&format!("line {i} needle\n"));
        }
        std::fs::write(d.join("a.log"), content).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut opt = opts(false, false);
        opt.max_hits = 5;
        opt.search.max_hits = 5;
        run_grep(d.clone(), "needle", &opt, &CancelToken::new(), &tx);
        drop(tx);
        let mut saw_truncated = false;
        let mut total = 0;
        while let Ok(m) = rx.recv() {
            match m {
                GrepMessage::Partial { hits } => total += hits.len(),
                GrepMessage::Truncated { .. } => saw_truncated = true,
                _ => {}
            }
        }
        assert!(saw_truncated);
        assert!(total <= 5);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// 目录检索样本目录（需自备，内含一个较大的 .log 才能体现块间取消的意义）。
    fn require_grep_sample() -> PathBuf {
        let p = PathBuf::from("/tmp/bench_grep_dir");
        assert!(
            p.is_dir() && !collect_log_files(&p).is_empty(),
            "缺少目录检索样本 /tmp/bench_grep_dir；请先：mkdir -p /tmp/bench_grep_dir && scripts/gen_log.sh /tmp/bench_grep_dir/big.log 20000000"
        );
        p
    }

    /// P10（目录检索版）：「停止查找全部」取消响应延迟 < 500 ms（spec §11.2 P10 / G1）。
    ///
    /// 锁定 M14 修复：原 `search_one_file` 仅在文件间检查取消令牌，单大文件整扫时「停止」无效；
    /// 修复后块间每 8 MiB 检查一次，取消应在数毫秒内回落（受单块扫描耗时约束，远低于 500ms）。
    #[test]
    #[ignore]
    fn grep_cancel_response_under_500ms() {
        let root = require_grep_sample();
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let opt = GrepOptions {
            search: SearchOptions {
                mode: SearchMode::Plain,
                case_sensitive: false,
                max_hits: usize::MAX,
            },
            max_hits: usize::MAX,
            base: Some(root.clone()),
        };
        let handle = std::thread::spawn(move || {
            run_grep(root, "ERROR", &opt, &c2, &tx);
        });
        std::thread::sleep(Duration::from_millis(50)); // 让检索先跑起来
        let t_cancel = Instant::now();
        cancel.cancel();

        let mut latency = None;
        let deadline = t_cancel + Duration::from_secs(2);
        while latency.is_none() {
            match rx.try_recv() {
                Ok(GrepMessage::Cancelled) | Ok(GrepMessage::Completed { .. }) => {
                    latency = Some(t_cancel.elapsed());
                }
                _ => {
                    if Instant::now() > deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        handle.join().unwrap();
        let latency = latency.expect("应收到终止消息（Cancelled 或 Completed）");
        println!("目录检索取消响应延迟: {latency:?}");
        assert!(latency.as_secs_f64() < 0.5, "P10 未达标：{latency:?}");
    }
}
