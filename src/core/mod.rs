//! 与 GUI 完全解耦的核心逻辑：不得在此模块树内引用 `egui` / `eframe`。

pub mod dirscan;
pub mod export;
pub mod grepdir;
pub mod indexer;
pub mod recents;
pub mod search;

/// 跨模块共享的可中断令牌（检索与导出复用同一实现，见 `search::CancelToken`）。
pub use search::CancelToken;
