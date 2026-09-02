//! 左侧文件目录树（spec §7.7 侧边栏）。
//!
//! 把当前已加载的日志文件（[`crate::core::indexer::FileSet`]）按其磁盘路径构建成
//! 目录树：目录用 `CollapsingHeader` 折叠，文件用可点击行（显示文件名 + 行数）。
//! 点击文件即跳转到该文件在全局行号空间中的首行（设置 [`AppState::scroll_target`]）。

use std::collections::BTreeMap;

use eframe::egui;

use crate::app::AppState;
use crate::ui::theme::{self, Palette};

/// 目录树节点：子目录 + 本目录下的文件（文件名, 文件索引）。
#[derive(Default)]
struct DirNode {
    dirs: BTreeMap<String, DirNode>,
    files: Vec<(String, usize)>,
}

/// 由已加载文件路径构建目录树。
fn build_tree(state: &AppState) -> DirNode {
    let mut root = DirNode::default();
    for (file_idx, idx) in state.fileset.files().iter().enumerate() {
        let comps: Vec<String> = idx
            .path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if comps.is_empty() {
            continue;
        }
        let (dirs, filename) = comps.split_at(comps.len() - 1);
        let mut node = &mut root;
        for d in dirs {
            node = node.dirs.entry(d.clone()).or_default();
        }
        node.files.push((filename[0].clone(), file_idx));
    }
    root
}

/// 每层缩进像素。
const INDENT: f32 = 12.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let p = theme::palette(ui.ctx());
    egui::ScrollArea::both()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            if state.fileset.file_count() == 0 {
                empty_hint(ui, p);
                return;
            }
            let tree = build_tree(state);
            render_node(ui, state, p, &tree, 0);
        });
}

/// 渲染一个目录节点（含其子目录与文件），`depth` 控制缩进。
fn render_node(ui: &mut egui::Ui, state: &mut AppState, p: &Palette, node: &DirNode, depth: usize) {
    let indent = depth as f32 * INDENT;

    for (name, child) in &node.dirs {
        let id = egui::Id::new(("dir", depth, name));
        ui.push_id(id, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(indent);
                egui::collapsing_header::CollapsingHeader::new(name)
                    .default_open(true)
                    .show(ui, |ui| {
                        render_node(ui, state, p, child, depth + 1);
                    });
            });
        });
    }

    for (name, file_idx) in &node.files {
        let line_count = state
            .fileset
            .file(*file_idx)
            .map(|f| f.line_count())
            .unwrap_or(0);
        let selected = state
            .scroll_target
            .and_then(|r| state.fileset.file_global_start(*file_idx).map(|s| s == r))
            .unwrap_or(false);

        let resp = ui.horizontal(|ui| {
            ui.add_space(indent + INDENT);
            let resp = ui.selectable_label(selected, name);
            let full = state
                .fileset
                .file(*file_idx)
                .map(|f| f.path.display().to_string())
                .unwrap_or_default();
            let resp = resp.on_hover_text(full);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("{} 行", crate::util::group_digits(line_count)))
                    .color(p.text_dim)
                    .small(),
            );
            resp
        });
        if resp.inner.clicked()
            && let Some(start) = state.fileset.file_global_start(*file_idx)
        {
            state.scroll_target = Some(start);
            state.selected_row = Some(start);
        }
    }
}

/// 空态提示。
fn empty_hint(ui: &mut egui::Ui, p: &Palette) {
    ui.vertical_centered(|ui| {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("尚未打开文件")
                .color(p.text_dim)
                .small(),
        );
    });
}
