//! 左侧文件目录树（spec §7.7 侧边栏）。
//!
//! 把当前已加载的日志文件（[`crate::core::indexer::FileSet`]）按其磁盘路径构建成
//! 目录树，风格对齐 VSCode 资源管理器：
//! - 以所有文件父目录的**最长公共前缀**为根（工作区根），只显示相对结构，不显示 `/Users/…` 这种深而无用的绝对路径；
//! - 目录用 `CollapsingHeader` 折叠（📁 图标），文件用可点击行（📄 图标 + 文件名 + 行数）；
//! - 目录在前、文件按名排序在后（文件夹优先、组内字母序）；
//! - 点击文件即跳转到该文件在全局行号空间中的首行（设置 [`AppState::scroll_target`]），并持久高亮（[`AppState::sidebar_active_file`]）；
//! - **反查高亮**：用户在日志区选中某行（`selected_row`）时，自动高亮其所属文件（VSCode 高亮光标所在文件）；
//! - **右键菜单**：文件行与根节点支持「复制路径 / 在文件管理器中显示（macOS `open -R`、Windows `explorer /select`、Linux `xdg-open`）/ 跳转」。

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
///
/// 目录只作为文件的「中间节点」被创建，因此不会凭空出现空目录（空目录折叠天然满足）。
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
            // 反查高亮：用户在日志区选中某行时，高亮其所属文件（VSCode 高亮光标所在文件）。
            if let Some(row) = state.selected_row
                && let Some(f) = state.fileset.file_for_global_row(row)
            {
                state.sidebar_active_file = Some(f);
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
                    // 同 toolbar：强调色显式取 Palette，不用 `.strong()`（亮色主题下会是白字）。
                    egui::RichText::new(format!("📂 {}", root_label(&ancestor)))
                        .color(p.text_strong),
                )
                .default_open(true)
                .show(ui, |ui| {
                    render_node(ui, state, p, &tree, 0);
                });
                let hdr = resp.header_response.on_hover_text(full.clone());
                hdr.context_menu(|ui| {
                    root_context_menu(ui, &ancestor, &full);
                });
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

        // 整行可点击跳转；右键菜单挂在选中行上（`context_menu` 是 `Response` 方法）。
        // 在 horizontal 闭包内直接返回 `sel.clicked()`，避免 `InnerResponse` 嵌套导致类型错乱。
        let clicked = ui
            .push_id(("file", *file_idx), |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(CHEVRON_PAD);
                    let sel = ui.selectable_label(selected, format!("📄 {name}"));
                    let clicked = sel.clicked();
                    let sel = sel.on_hover_text(full.clone());
                    sel.context_menu(|ui| {
                        file_context_menu(ui, state, *file_idx, &full);
                    });
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} 行",
                            crate::util::group_digits(line_count)
                        ))
                        .color(p.text_dim)
                        .small(),
                    );
                    clicked
                })
                .inner
            })
            .inner;
        if clicked && let Some(start) = state.fileset.file_global_start(*file_idx) {
            state.scroll_target = Some(start);
            state.selected_row = Some(start);
            state.sidebar_active_file = Some(*file_idx);
        }
    }
}

/// 文件行右键菜单：复制路径 / 在文件管理器中显示 / 跳转。
fn file_context_menu(ui: &mut egui::Ui, state: &mut AppState, file_idx: usize, full: &str) {
    if ui.button("复制路径").clicked() {
        ui.ctx().copy_text(full.to_owned());
        ui.close();
    }
    if ui.button("在文件管理器中显示").clicked() {
        if let Some(f) = state.fileset.file(file_idx) {
            reveal_in_file_manager(&f.path);
        }
        ui.close();
    }
    if ui.button("跳到该文件").clicked() {
        if let Some(start) = state.fileset.file_global_start(file_idx) {
            state.scroll_target = Some(start);
            state.selected_row = Some(start);
            state.sidebar_active_file = Some(file_idx);
        }
        ui.close();
    }
}

/// 根节点右键菜单：复制根目录路径 / 在文件管理器中显示根目录。
fn root_context_menu(ui: &mut egui::Ui, ancestor: &Option<PathBuf>, full: &str) {
    if ui.button("复制根目录路径").clicked() {
        ui.ctx().copy_text(full.to_owned());
        ui.close();
    }
    if ui.button("在文件管理器中显示").clicked() {
        if let Some(a) = ancestor {
            reveal_in_file_manager(a);
        }
        ui.close();
    }
}

/// 在系统文件管理器中定位并选中文件（VSCode 右键「Reveal in File Explorer」等价）。
#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
}

/// Windows：用资源管理器选中文件。
#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

/// 其他平台：打开父目录（无「选中」语义）。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_in_file_manager(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
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
