//! 大文件性能验收（spec.md §11.2）。
//!
//! 这些用例需要先用 `scripts/gen_log.sh` 生成 GB 级样本，默认被 `#[ignore]` 跳过，
//! 因此 CI 不会跑。本地执行：
//!
//! ```bash
//! scripts/gen_log.sh /tmp/bench_1gb.log 10000000
//! cargo test --release --test perf_test -- --ignored --nocapture
//! ```
//!
//! **必须带 `--release`**：dev 构建下的正则性能慢 10~50 倍，数据无参考价值。

const BENCH_1GB: &str = "/tmp/bench_1gb.log";

fn require_sample(path: &str) -> std::path::PathBuf {
    let p = std::path::PathBuf::from(path);
    if !p.exists() {
        panic!("缺少性能测试样本 {path}。请先运行：scripts/gen_log.sh {path} 10000000");
    }
    p
}

/// P1：打开 1 GB（≈1000 万行）到可滚动 < 2.0 s
#[test]
#[ignore]
fn open_1gb_under_2s() {
    let path = require_sample(BENCH_1GB);
    let start = std::time::Instant::now();
    // TODO(T04): 替换为 LogFileIndex::open
    let bytes = std::fs::metadata(&path).unwrap().len();
    let elapsed = start.elapsed();

    println!("open 1GB: {elapsed:?} ({bytes} bytes)");
    assert!(elapsed.as_secs_f64() < 2.0, "P1 未达标: {elapsed:?}");
}

/// P3：索引常驻内存 ≤ 8 B/行
#[test]
#[ignore]
fn index_memory_within_budget() {
    let _path = require_sample(BENCH_1GB);
    // TODO(T04/T19): 打开后断言 index_memory_bytes() <= line_count * 8
}
