//! Woodshed: a music player for transcription and rehearsal.

mod analysis;
mod app;
mod display;
mod engine;
mod metadata;
mod midi;
mod pitch;
mod playlist_io;
mod session;
mod track;

use eframe::egui;

/// Whether this is a Wayland desktop that can also run X11 programs.
pub fn wayland_session_with_x11() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    cfg!(target_os = "linux") && (set("WAYLAND_DISPLAY") || set("WAYLAND_SOCKET")) && set("DISPLAY")
}

fn main() -> eframe::Result {
    let files: Vec<_> = std::env::args_os().skip(1).map(std::path::PathBuf::from).collect();
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Woodshed")
            .with_app_id("woodshed")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([640.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    // The windowing library does not deliver dropped files on Wayland, so on
    // a Wayland desktop the window is opened through X11 (XWayland) unless
    // the preference says otherwise.
    #[cfg(target_os = "linux")]
    if wayland_session_with_x11() && session::Settings::load().x11_on_wayland {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11 as _;
            builder.with_x11();
        }));
    }
    eframe::run_native("Woodshed", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, files)))))
}
