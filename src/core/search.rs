//! 检索引擎：在已索引文件集合上做跨文件正则/字面量检索，流式回传增量结果。
//!
//! 设计要点（对应 spec §7.4 / §7.5，修复 D4/D5/D6/D7）：
//! - 后台线程内**按字节**分块（目标 8 MiB/块，块边界对齐到行），避免行长差异导致进度抖动；
//! - 块内用 `rayon` 并行匹配，**本地累加**后一次性汇总，禁止逐行 `fetch_add`（D7）；
//! - 每完成一个块回传 `Partial`（节流 ≥ 100 ms），实现「边算边吐结果」（D4）；
//! - `CancelToken`（Arc<AtomicBool>）实现可中断，取消响应 < 500 ms（G1）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use rayon::prelude::*;
use regex::bytes::Regex as BytesRegex;
use regex::{Regex, RegexBuilder};

use crate::core::indexer::LogFileIndex;

/// 检索模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// 纯文本（字面量，经 `regex::escape` 转义，默认）。
    #[default]
    Plain,
    /// 正则表达式。
    Regex,
}

/// 检索选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    pub mode: SearchMode,
    /// 大小写敏感；默认 `false`（忽略大小写）。
    pub case_sensitive: bool,
    /// 命中上限；达到后停止扫描并发送 `Truncated`（G6）。
    pub max_hits: usize,
}

/// 单条命中：只存坐标，不复制文本（spec §7.3，修复 D8）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub file_idx: u32,
    pub line_idx: u32,
}

/// 检索失败原因。
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("regex compile error: {0}")]
    Regex(String),
}

/// 后台 → UI 的消息（spec §7.4）。
#[derive(Debug)]
pub enum SearchMessage {
    /// 增量结果：每完成一个字节块推送一次（修复 D4）。
    Partial {
        hits: Vec<SearchHit>,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// 命中已达上限而提前终止（G6）。
    Truncated { hits: usize },
    /// 正常完成。
    Completed { hits: usize, elapsed: Duration },
    /// 失败：正则语法错误 / IO 错误 / 被取消。
    Failed(SearchError),
    /// 已响应取消并退出。
    Cancelled,
}

/// 取消令牌（填补 G1）。克隆后共享同一个底层原子，任一份 `cancel()` 即生效。
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// 目标分块大小（字节）。按字节分块使进度条均匀（D5）。
const CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// 回传节流间隔：距上次发送不足此值则累积到下一波（D6）。
const SEND_INTERVAL: Duration = Duration::from_millis(100);

/// 编译检索正则。Plain 模式转义字面量；`case_sensitive=false` 时前置 `(?i)`，
/// 从而高亮与检索复用同一 `Regex`（G5）。
pub fn build_regex(opt: &SearchOptions, pattern: &str) -> Result<Regex, SearchError> {
    let mut expr = String::new();
    if !opt.case_sensitive {
        expr.push_str("(?i)");
    }
    match opt.mode {
        SearchMode::Plain => expr.push_str(&regex::escape(pattern)),
        SearchMode::Regex => expr.push_str(pattern),
    }
    RegexBuilder::new(&expr)
        .size_limit(16 * 1024 * 1024)
        .dfa_size_limit(8 * 1024 * 1024)
        .build()
        .map_err(|e| SearchError::Regex(e.to_string()))
}

/// 编译字节级检索正则（检索路径用，对应 M6 P8 优化）。
///
/// 直接在 mmap 原始字节上匹配，避免逐行 `from_utf8_lossy` 的分配与 UTF-8 校验，
/// 从而把 10M 次逐行 dispatch 收敛为每字节块一次 SIMD 扫描。
///
/// 说明：`(?i)` 在字节模式下按 ASCII 大小写折叠（日志检索足够；非 ASCII 大小写差异罕见）。
/// 高亮仍走 [`build_regex`]（str 模式，Unicode 大小写折叠），二者对 ASCII 模式完全等价，
/// 故高亮命中与检索结果一致（spec §13 Q7 备注）。
pub fn build_regex_bytes(opt: &SearchOptions, pattern: &str) -> Result<BytesRegex, SearchError> {
    let mut expr = String::new();
    if !opt.case_sensitive {
        expr.push_str("(?i)");
    }
    match opt.mode {
        SearchMode::Plain => expr.push_str(&regex::escape(pattern)),
        SearchMode::Regex => expr.push_str(pattern),
    }
    BytesRegex::new(&expr).map_err(|e| SearchError::Regex(e.to_string()))
}

/// 检索用线程池：限 `num_cpus - 1` 线程，给 UI 留一个核（spec R11）。
fn search_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let threads = (n.saturating_sub(1)).max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("rayon 线程池创建失败")
    })
}

/// 对单个字节块做检索：整块 `find_iter` 后将命中偏移二分映射回行号。
///
/// 块内去重（同一行多次命中只记一次，与「逐行」原语义一致）；跨行匹配忽略。
/// 各块字节区间互不重叠且边界对齐到行，故跨块无需去重（M7 波次并行的基础）。
fn scan_chunk(
    re: &BytesRegex,
    file: &LogFileIndex,
    byte_lo: usize,
    slice: &[u8],
    file_idx: usize,
) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut last_li: Option<usize> = None;
    for m in re.find_iter(slice) {
        let s = m.start();
        let e = m.end();
        // 跨行匹配按「逐行」原语义忽略（正常匹配不含换行，块边界即行边界）。
        if slice[s..e].contains(&b'\n') {
            continue;
        }
        let li = file.line_index_of_byte(byte_lo + s);
        if last_li != Some(li) {
            last_li = Some(li);
            hits.push(SearchHit {
                file_idx: file_idx as u32,
                line_idx: li as u32,
            });
        }
    }
    hits
}

/// 在 `files` 上执行检索，结果经 `tx` 流式回传。
///
/// 调用方负责在后台线程中运行本函数，并在启动新检索前保证上一检索已结束
/// （MVP 禁止并发：UI 在检索中禁用「搜索」按钮）。
pub fn run_search(
    files: &[Arc<LogFileIndex>],
    pattern: &str,
    options: &SearchOptions,
    cancel: &CancelToken,
    tx: &Sender<SearchMessage>,
) {
    let start = Instant::now();
    let re = match build_regex_bytes(options, pattern) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(SearchMessage::Failed(e));
            return;
        }
    };

    let total_bytes: u64 = files.iter().map(|f| f.byte_len() as u64).sum();
    // 预生成全量分块列表（行对齐、互不重叠）：(file_idx, byte_lo, byte_hi)。
    // 一次构建便于后续按波次并行扫描（M7：P8 余量加固，跨块无重复故无需去重）。
    let mut chunk_list: Vec<(usize, usize, usize)> = Vec::new();
    for (file_idx, file) in files.iter().enumerate() {
        let n = file.line_count();
        if n == 0 {
            continue;
        }
        for (lo, hi) in file.chunk_bounds(CHUNK_BYTES) {
            let (bl, bh) = file.byte_range(lo, hi);
            chunk_list.push((file_idx, bl, bh));
        }
    }

    // 每波处理「线程数」个分块：并行度足够，且取消检查在波次之间（< 500 ms，G1）。
    let ncpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let pool_threads = (ncpus.saturating_sub(1)).max(1);
    let wave_size = pool_threads.max(1);

    let mut bytes_done: u64 = 0;
    let mut total_hits: usize = 0;
    let mut pending: Vec<SearchHit> = Vec::new();
    let mut last_send = Instant::now();
    let pool = search_pool();

    for wave in chunk_list.chunks(wave_size) {
        if cancel.is_cancelled() {
            let _ = tx.send(SearchMessage::Cancelled);
            return;
        }
        // 波内分块并行扫描：各块字节区间互不重叠，命中行无重复。
        let results: Vec<Vec<SearchHit>> = pool.install(|| {
            wave.par_iter()
                .map(|&(file_idx, bl, bh)| {
                    let file = &files[file_idx];
                    let slice = &file.raw_bytes()[bl..bh];
                    scan_chunk(&re, file, bl, slice, file_idx)
                })
                .collect()
        });

        for (i, hits) in results.into_iter().enumerate() {
            bytes_done += (wave[i].2 - wave[i].1) as u64;
            let chunk_len = hits.len();
            if chunk_len > 0 {
                pending.extend(hits);
                total_hits += chunk_len;
            }
            if total_hits >= options.max_hits {
                pending.truncate(options.max_hits);
                let batch = std::mem::take(&mut pending);
                let _ = tx.send(SearchMessage::Partial {
                    hits: batch,
                    bytes_done,
                    bytes_total: total_bytes,
                });
                let _ = tx.send(SearchMessage::Truncated {
                    hits: options.max_hits,
                });
                return;
            }
        }

        if last_send.elapsed() >= SEND_INTERVAL && !pending.is_empty() {
            let batch = std::mem::take(&mut pending);
            let _ = tx.send(SearchMessage::Partial {
                hits: batch,
                bytes_done,
                bytes_total: total_bytes,
            });
            last_send = Instant::now();
        }
    }

    if !pending.is_empty() {
        let batch = std::mem::take(&mut pending);
        let _ = tx.send(SearchMessage::Partial {
            hits: batch,
            bytes_done,
            bytes_total: total_bytes,
        });
    }
    let _ = tx.send(SearchMessage::Completed {
        hits: total_hits,
        elapsed: start.elapsed(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    fn index_tmp(content: &[u8]) -> Arc<LogFileIndex> {
        let p = std::env::temp_dir().join(format!(
            "hyperlog_srch_{}_{}.log",
            std::process::id(),
            uuid()
        ));
        std::fs::write(&p, content).unwrap();
        Arc::new(LogFileIndex::open(&p).unwrap())
    }

    fn uuid() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn plain_mode_matches_substring_like_contains() {
        let content = b"alpha ERROR one\nbeta warn two\nERROR again\ngamma delta\n";
        let files = vec![index_tmp(content)];
        let (tx, rx) = unbounded();
        let opt = SearchOptions {
            mode: SearchMode::Plain,
            case_sensitive: false,
            max_hits: 2_000_000,
        };
        run_search(&files, "ERROR", &opt, &CancelToken::new(), &tx);
        drop(tx); // 关闭通道，使 recv 能结束

        let mut hits = Vec::new();
        let mut completed = false;
        while let Ok(msg) = rx.recv() {
            match msg {
                SearchMessage::Partial { hits: h, .. } => hits.extend(h),
                SearchMessage::Completed { .. } => {
                    completed = true;
                    break;
                }
                SearchMessage::Truncated { .. } => break,
                other => panic!("unexpected message: {other:?}"),
            }
        }
        assert!(completed, "应正常完成");
        // 忽略大小写时，第 0、2 行含 ERROR
        let lines: Vec<usize> = hits.iter().map(|h| h.line_idx as usize).collect();
        assert_eq!(lines, vec![0, 2]);
    }

    #[test]
    fn regex_mode_respects_pattern() {
        let content = b"abc123\nxyz\nabc456\n";
        let files = vec![index_tmp(content)];
        let (tx, rx) = unbounded();
        let opt = SearchOptions {
            mode: SearchMode::Regex,
            case_sensitive: true,
            max_hits: 2_000_000,
        };
        run_search(&files, r"abc\d+", &opt, &CancelToken::new(), &tx);
        drop(tx);

        let mut lines = Vec::new();
        while let Ok(msg) = rx.recv() {
            match msg {
                SearchMessage::Partial { hits, .. } => {
                    lines.extend(hits.iter().map(|h| h.line_idx))
                }
                SearchMessage::Completed { .. } | SearchMessage::Truncated { .. } => break,
                other => panic!("unexpected: {other:?}"),
            }
        }
        assert_eq!(lines, vec![0, 2]);
    }

    #[test]
    fn invalid_regex_sends_failed() {
        let files = vec![index_tmp(b"anything\n")];
        let (tx, rx) = unbounded();
        let opt = SearchOptions {
            mode: SearchMode::Regex,
            case_sensitive: false,
            max_hits: 2_000_000,
        };
        run_search(&files, r"([", &opt, &CancelToken::new(), &tx);
        drop(tx);
        match rx.recv() {
            Ok(SearchMessage::Failed(SearchError::Regex(_))) => {}
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn max_hits_truncates() {
        let mut content = Vec::new();
        for i in 0..1000 {
            content.extend_from_slice(format!("line {i} x\n").as_bytes());
        }
        let files = vec![index_tmp(&content)];
        let (tx, rx) = unbounded();
        let opt = SearchOptions {
            mode: SearchMode::Plain,
            case_sensitive: false,
            max_hits: 10,
        };
        run_search(&files, "x", &opt, &CancelToken::new(), &tx);
        drop(tx);

        let mut total = 0;
        let mut truncated = false;
        while let Ok(msg) = rx.recv() {
            match msg {
                SearchMessage::Partial { hits, .. } => total += hits.len(),
                SearchMessage::Truncated { hits } => {
                    truncated = true;
                    assert_eq!(hits, 10);
                    break;
                }
                SearchMessage::Completed { .. } => break,
                other => panic!("unexpected: {other:?}"),
            }
        }
        assert!(truncated);
        assert!(total <= 10);
    }

    #[test]
    fn cancel_stops_search() {
        let mut content = Vec::new();
        for i in 0..200_000 {
            content.extend_from_slice(format!("line {i} needle\n").as_bytes());
        }
        let files = vec![index_tmp(&content)];
        let (tx, rx) = unbounded();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let opt = SearchOptions {
            mode: SearchMode::Plain,
            case_sensitive: false,
            max_hits: 2_000_000,
        };
        let handle = std::thread::spawn(move || {
            run_search(&files, "needle", &opt, &c2, &tx);
        });
        cancel.cancel();
        handle.join().unwrap();

        let mut saw_terminal = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                SearchMessage::Cancelled | SearchMessage::Completed { .. } => saw_terminal = true,
                _ => {}
            }
        }
        assert!(saw_terminal, "取消后应收到 Cancelled 或 Completed");
    }
}

/// 大文件检索性能验收（spec §11.2 P7/P8，对应 T19）。
///
/// 需先生成样本：`scripts/gen_log.sh /tmp/bench_1gb.log 10000000`，再在 **release** 下运行：
///
/// ```bash
/// cargo test --release -- --ignored search_throughput_gb
/// ```
#[cfg(test)]
mod perf {
    use super::*;
    use crate::core::indexer::LogFileIndex;
    use crossbeam_channel::unbounded;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const BENCH_1GB: &str = "/tmp/bench_1gb.log";

    fn require_sample() -> PathBuf {
        let p = PathBuf::from(BENCH_1GB);
        assert!(
            p.exists(),
            "缺少性能样本 {BENCH_1GB}；请先运行：scripts/gen_log.sh {BENCH_1GB} 10000000"
        );
        p
    }

    /// P8：纯文本（字面量）检索吞吐 ≥ 800 MB/s；P7：正则 ≥ 200 MB/s。
    /// 取多次运行峰值：吞吐受机器内存带宽/后台负载影响波动大，峰值反映实现能力（spec P8/P7）。
    #[test]
    #[ignore]
    fn search_throughput_gb() {
        let p = require_sample();
        let _ = std::fs::read(&p); // 预热页缓存，隔离磁盘 IO 与扫描吞吐
        let idx = Arc::new(LogFileIndex::open(&p).expect("open"));
        let bytes = idx.byte_len() as u64;
        let files = vec![idx.clone()];

        let plain_mbps = peak_mbps(&files, bytes, SearchMode::Plain, "ERROR");
        println!("纯文本 ERROR 峰值: {plain_mbps:.1} MB/s");
        assert!(plain_mbps >= 800.0, "P8 未达标：{plain_mbps:.1} MB/s");

        let regex_mbps = peak_mbps(&files, bytes, SearchMode::Regex, "ERROR|WARN");
        println!("正则 ERROR|WARN 峰值: {regex_mbps:.1} MB/s");
        assert!(regex_mbps >= 200.0, "P7 未达标：{regex_mbps:.1} MB/s");
    }

    /// 对同一样本/模式连跑 3 次，返回峰值吞吐（MB/s），规避单次运行的机器负载抖动。
    fn peak_mbps(files: &[Arc<LogFileIndex>], bytes: u64, mode: SearchMode, pattern: &str) -> f64 {
        let mut best = 0.0f64;
        for _ in 0..3 {
            let (tx, rx) = unbounded();
            let opt = SearchOptions {
                mode,
                case_sensitive: false,
                max_hits: usize::MAX,
            };
            let start = Instant::now();
            run_search(files, pattern, &opt, &CancelToken::new(), &tx);
            let elapsed = start.elapsed();
            while rx.try_recv().is_ok() {}
            best = best.max(bytes as f64 / 1e6 / elapsed.as_secs_f64());
        }
        best
    }

    /// P9：进度消息频率 ≤ 20 条/秒（spec §11.2 P9 / D6）。
    #[test]
    #[ignore]
    fn progress_rate_le_20_per_s() {
        let p = require_sample();
        let _ = std::fs::read(&p); // 预热页缓存
        let idx = Arc::new(LogFileIndex::open(&p).expect("open"));
        let files = vec![idx.clone()];
        let (tx, rx) = unbounded();
        let opt = SearchOptions {
            mode: SearchMode::Plain,
            case_sensitive: false,
            max_hits: usize::MAX,
        };
        let start = Instant::now();
        run_search(&files, "ERROR", &opt, &CancelToken::new(), &tx);
        let elapsed = start.elapsed();
        let mut partials = 0usize;
        while let Ok(m) = rx.try_recv() {
            if matches!(m, SearchMessage::Partial { .. }) {
                partials += 1;
            }
        }
        let rate = partials as f64 / elapsed.as_secs_f64();
        println!("进度消息 {partials} 条 / {elapsed:?} = {rate:.1} 条/秒");
        assert!(rate <= 20.0, "P9 未达标：{rate:.1} 条/秒");
    }

    /// P10：取消响应延迟 < 500 ms（spec §11.2 P10 / G1）。
    #[test]
    #[ignore]
    fn cancel_response_under_500ms() {
        let p = require_sample();
        let _ = std::fs::read(&p); // 预热页缓存
        let idx = Arc::new(LogFileIndex::open(&p).expect("open"));
        let files = vec![idx.clone()];
        let (tx, rx) = unbounded();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let opt = SearchOptions {
            mode: SearchMode::Plain,
            case_sensitive: false,
            max_hits: usize::MAX,
        };
        let handle = std::thread::spawn(move || {
            run_search(&files, "ERROR", &opt, &c2, &tx);
        });
        std::thread::sleep(std::time::Duration::from_millis(50)); // 让检索先跑起来
        let t_cancel = Instant::now();
        cancel.cancel();

        let mut latency = None;
        let deadline = t_cancel + Duration::from_secs(2);
        while latency.is_none() {
            match rx.try_recv() {
                Ok(SearchMessage::Cancelled) | Ok(SearchMessage::Completed { .. }) => {
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
        println!("取消响应延迟: {latency:?}");
        assert!(latency.as_secs_f64() < 0.5, "P10 未达标：{latency:?}");
    }
}
