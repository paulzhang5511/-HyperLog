mod app;
mod core;
mod highlight;
mod ui;
mod util;

use std::path::PathBuf;

use app::LogViewerApp;

/// 渲染后端开关。wgpu 在部分 macOS 机型上会因 Metal 着色器编译失败而直接退出，
/// 因此默认使用兼容性更好的 glow（OpenGL）。
const RENDERER_ENV: &str = "HYPER_LOG_RENDERER";

/// 收集启动即载入的日志路径：支持位置参数与 `-o/--open <path>`（以及 `--open=<path>`）。
/// 未知 `-x` 选项会被忽略并提示，便于将来扩展而不会影响「直接拖文件/路径打开」。
fn collect_initial_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" | "--open" => {
                if let Some(p) = args.next() {
                    paths.push(PathBuf::from(p));
                }
            }
            s if s.starts_with("--open=") => {
                paths.push(PathBuf::from(&s["--open=".len()..]));
            }
            s if s.starts_with('-') && s != "-" => {
                eprintln!("hyper-log: 忽略未知参数 {s}");
            }
            s => paths.push(PathBuf::from(s)),
        }
    }
    paths
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let native_options = eframe::NativeOptions {
        renderer: select_renderer(),
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    log::info!("使用渲染后端: {:?}", native_options.renderer);

    let initial_paths = collect_initial_paths();
    if !initial_paths.is_empty() {
        log::info!("启动即载入 {} 个路径", initial_paths.len());
    }

    eframe::run_native(
        "Hyper Log",
        native_options,
        Box::new(move |cc| Ok(Box::new(LogViewerApp::new(cc, initial_paths)))),
    )
}

fn select_renderer() -> eframe::Renderer {
    match std::env::var(RENDERER_ENV).as_deref() {
        Ok("wgpu") => eframe::Renderer::Wgpu,
        Ok("glow") => eframe::Renderer::Glow,
        Ok(other) => {
            log::warn!("未知的 {RENDERER_ENV}={other}，回退到 glow");
            eframe::Renderer::Glow
        }
        Err(_) => eframe::Renderer::Glow,
    }
}
