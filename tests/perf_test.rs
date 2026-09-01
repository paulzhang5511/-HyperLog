//! 大文件性能验收入口（spec.md §11.2 / T19）。
//!
//! 本项目是**二进制 crate**（无 lib target），集成测试无法访问 `core` 模块。
//! 因此真实的性能验收用例实现为 `src/core/*` 内的 `#[ignore]` 单元测试：
//!
//! - `src/core/indexer.rs` → `open_1gb_under_2s`（P1）、`index_memory_within_budget`（P3）
//! - `src/core/search.rs`   → `search_throughput_gb`（P7 / P8）
//!
//! 运行步骤（**必须带 `--release`**：dev 构建下正则慢 10~50 倍，数据无参考价值）：
//!
//! ```bash
//! scripts/gen_log.sh /tmp/bench_1gb.log 10000000
//! cargo test --release -- --ignored open_1gb_under_2s index_memory_within_budget search_throughput_gb
//! ```
