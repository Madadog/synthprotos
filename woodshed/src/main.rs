//! Woodshed: a music player for transcription and rehearsal.

mod analysis;
mod app;
mod display;
mod engine;
mod pitch;
mod session;
mod track;

use eframe::egui;

fn main() -> eframe::Result {
    let files: Vec<_> = std::env::args_os().skip(1).map(std::path::PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Woodshed")
            .with_app_id("woodshed")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([640.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Woodshed", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, files)))))
}
