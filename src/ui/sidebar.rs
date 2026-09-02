//! 左侧文件目录树（spec §7.7 侧边栏）。
//!
//! 把当前已加载的日志文件（[`crate::core::indexer::FileSet`]）按其磁盘路径构建成
//! 目录树，风格对齐 VSCode 资源管理器：
//! - 以所有文件父目录的**最长公共前缀**为根（工作区根），只显示相对结构，不显示 `/Users/…` 这种深而无用的绝对路径；
//! - 目录用 `CollapsingHeader` 折叠（📁 图标），文件用可点击行（📄 图标 + 文件名 + 行数）；
//! - 目录在前、文件按名排序在后（文件夹优先、组内字母序）；
//! - 点击文件即跳转到该文件在全局行号空间中的首行（设置 [`AppState::scroll_target`]），并持久高亮（[`AppState::sidebar_active_file`]）。

use std::path::{Component, Path, PathBuf};

use eframe::egui;

use crate::app::AppState;
use crate::ui::theme::{self, Palette};

/// 目录树节点：子目录 + 本目录下的文件（文件名, 文件索引）。
#[derive(Default)]
struct DirNode {
    dirs: std::collections::BTreeMap<String, DirNode>,
    files: Vec<(String, usize)>,
}

/// 计算所有已加载文件父目录的「最长公共前缀目录」，作为目录树的根（VSCode 以工作区根展示）。
fn common_ancestor(paths: &[(PathBuf, usize)]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut ancestor = first.0.parent()?.to_path_buf();
    for (p, _) in iter {
        let parent = p.parent()?;
        ancestor = longest_common_prefix(&ancestor, parent);
    }
    if ancestor.as_os_str().is_empty() {
        None
    } else {
        Some(ancestor)
    }
}

/// 两个路径的最长公共前缀（按组件比较）。
fn longest_common_prefix(a: &Path, b: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for (x, y) in a.components().zip(b.components()) {
        if x == y {
            result.push(x.as_os_str());
        } else {
            break;
        }
    }
    result
}

/// 根节点标签：取公共前缀目录的文件夹名；根目录（无文件名）显示 `/`。
fn root_label(ancestor: &Option<PathBuf>) -> String {
    match ancestor {
        Some(a) => a
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.display().to_string()),
        None => "文件".to_owned(),
    }
}

/// 由已加载文件路径构建目录树：先裁剪公共前缀，再按相对路径组件建树（跳过根/`/`组件）。
fn build_tree(paths: &[(PathBuf, usize)], ancestor: &Option<PathBuf>) -> DirNode {
    let mut root = DirNode::default();
    for (path, file_idx) in paths {
        let rel = match ancestor {
            Some(c) => path.strip_prefix(c).unwrap_or(path.as_path()),
            None => path.as_path(),
        };
        // 跳过 RootDir / Prefix，避免把 `/` 当成节点（VSCode 不显示挂载点/盘符这一层）。
        let comps: Vec<String> = rel
            .components()
            .filter(|c| !matches!(c, Component::RootDir | Component::Prefix(_)))
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
        node.files.push((filename[0].clone(), *file_idx));
    }
    root
}

/// 文件行相对目录行的左缩进，用于对齐到目录名（绕过折叠箭头宽度）。
const CHEVRON_PAD: f32 = 16.0;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let p = theme::palette(ui.ctx());
    egui::ScrollArea::both()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            if state.fileset.file_count() == 0 {
                empty_hint(ui, p);
                return;
            }
            let paths: Vec<(PathBuf, usize)> = state
                .fileset
                .files()
                .iter()
                .enumerate()
                .map(|(i, f)| (f.path.clone(), i))
                .collect();
            let ancestor = common_ancestor(&paths);
            let tree = build_tree(&paths, &ancestor);
            let full = ancestor
                .as_ref()
                .map(|a| a.display().to_string())
                .unwrap_or_default();
            ui.push_id("tree_root", |ui| {
                let resp = egui::collapsing_header::CollapsingHeader::new(
                    egui::RichText::new(format!("📂 {}", root_label(&ancestor))).strong(),
                )
                .default_open(true)
                .show(ui, |ui| {
                    render_node(ui, state, p, &tree, 0);
                });
                resp.header_response.on_hover_text(full);
            });
        });
}

/// 渲染一个目录节点（含其子目录与文件），`depth` 仅用于派生稳定 `Id`。
fn render_node(ui: &mut egui::Ui, state: &mut AppState, p: &Palette, node: &DirNode, depth: usize) {
    // 目录：BTreeMap 已按名排序；折叠箭头自带缩进，不再手动叠加 indent。
    for (name, child) in &node.dirs {
        ui.push_id(("dir", depth, name), |ui| {
            egui::collapsing_header::CollapsingHeader::new(format!("📁 {name}"))
                .default_open(true)
                .show(ui, |ui| {
                    render_node(ui, state, p, child, depth + 1);
                });
        });
    }

    // 文件：按文件名（不区分大小写）排序，目录之后展示，贴近 VSCode 资源管理器排序。
    let mut files = node.files.clone();
    files.sort_by_key(|a| a.0.to_lowercase());
    for (name, file_idx) in &files {
        let line_count = state
            .fileset
            .file(*file_idx)
            .map(|f| f.line_count())
            .unwrap_or(0);
        let selected = state.sidebar_active_file == Some(*file_idx);
        let full = state
            .fileset
            .file(*file_idx)
            .map(|f| f.path.display().to_string())
            .unwrap_or_default();

        let resp = ui.push_id(("file", *file_idx), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(CHEVRON_PAD);
                let resp = ui.selectable_label(selected, format!("📄 {name}"));
                let resp = resp.on_hover_text(full);
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("{} 行", crate::util::group_digits(line_count)))
                        .color(p.text_dim)
                        .small(),
                );
                resp
            })
        });
        if resp.inner.inner.clicked()
            && let Some(start) = state.fileset.file_global_start(*file_idx)
        {
            state.scroll_target = Some(start);
            state.selected_row = Some(start);
            state.sidebar_active_file = Some(*file_idx);
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
