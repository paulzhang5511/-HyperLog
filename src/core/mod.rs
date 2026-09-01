//! 与 GUI 完全解耦的核心逻辑：不得在此模块树内引用 `egui` / `eframe`。

pub mod export;
pub mod indexer;
pub mod search;
