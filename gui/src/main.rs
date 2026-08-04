// GUI-subsystem release builds get no console window on Windows; debug
// builds keep the console so `cargo run` still shows println/log output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod download_queue;
mod encode_config;
mod format;
mod metadata_worker;
mod output;
mod probe_worker;
mod queue;
mod settings;
mod sys;
mod update_worker;

use app::MediaKitApp;
use clap::Parser;

fn main() -> eframe::Result<()> {
    // Any args at all means headless CLI mode; no args launches the GUI.
    // (clap's generated `--help`/`--version` also flow through here, which
    // is exactly what a CLI user expects from a bare `mediakit --help`.)
    if std::env::args_os().len() > 1 {
        sys::ensure_console_attached();
        let args = cli::CliArgs::parse();
        std::process::exit(cli::run(args));
    }

    tracing_subscriber::fmt::init();

    let persisted = settings::load();

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([persisted.window.width, persisted.window.height])
            .with_min_inner_size([600.0, 400.0])
            .with_drag_and_drop(true)
            .with_icon(load_app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "MediaKit",
        native_options,
        Box::new(|cc| Ok(Box::new(MediaKitApp::new(cc, persisted)))),
    )
}

/// Window/taskbar icon shown by the windowing system at runtime (all
/// platforms). On Windows the .exe's own icon - shown in Explorer before the
/// window even opens - comes separately from the embedded PE resource built
/// by `build.rs`.
fn load_app_icon() -> std::sync::Arc<eframe::egui::IconData> {
    let bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(bytes)
        .expect("bundled assets/icon.png must be a valid image")
        .into_rgba8();
    let (width, height) = image.dimensions();
    std::sync::Arc::new(eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}
