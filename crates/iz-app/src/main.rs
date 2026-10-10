//! The Looking Glass desktop app: the egui/eframe UI and the binary.
//!
//! The UI talks to the world and the services only through
//! [`iz_types::Engine`]; [`make_engine`] is the single place it is built.

mod app;
#[cfg(test)]
mod dev_engine;
mod help_screen;
mod main_screen;
mod open_screen;
mod status_bar;
mod textures;

use iz_types::settings::{paths, Settings};
use std::sync::Arc;

/// Build the engine the UI drives: the mock services under `IZ_MOCK=1`
/// (no network, no cost), otherwise Claude through the headless CLI and
/// OpenAI's images API. Prompts and replies go to stdout either way.
fn make_engine() -> Box<dyn iz_types::Engine> {
    let log: Arc<dyn iz_types::CallLog> = Arc::new(iz_types::StdoutLog);
    if iz_types::settings::mock_mode() {
        println!("mock mode: no network calls will be made");
        return Box::new(iz_world::mock_engine(log));
    }
    let describer: Arc<dyn iz_types::Describer> = Arc::new(iz_claude::ClaudeCli::new());
    let painter: Arc<dyn iz_types::Painter> = Arc::new(iz_openai::OpenAiPainter::new());
    Box::new(iz_world::RealEngine::new(describer, painter, log))
}

fn main() -> eframe::Result {
    iz_types::settings::load_dotenv();
    let settings = Settings::load();
    println!("{} · data dir {}", iz_types::APP_NAME, paths::app_dir().display());
    let engine = make_engine();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(iz_types::APP_NAME)
            .with_inner_size(app::PREFERRED_WINDOW)
            .with_min_inner_size([800.0, 600.0])
            .with_clamp_size_to_monitor_size(true)
            .with_drag_and_drop(true),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        iz_types::APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, engine, settings)))),
    )
}
