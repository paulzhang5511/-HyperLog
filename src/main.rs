mod app;
mod core;
mod highlight;
mod ui;
mod util;

use app::LogViewerApp;

/// 渲染后端开关。wgpu 在部分 macOS 机型上会因 Metal 着色器编译失败而直接退出，
/// 因此默认使用兼容性更好的 glow（OpenGL）。
const RENDERER_ENV: &str = "HYPER_LOG_RENDERER";

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

    eframe::run_native(
        "Hyper Log",
        native_options,
        Box::new(|cc| Ok(Box::new(LogViewerApp::new(cc)))),
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
