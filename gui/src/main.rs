//! earshot's native UI (egui): a DJ library explorer and performance decks. See
//! ../README.md.

// A release build on Windows is a windowed program: no console window beside it (its
// log is in the state folder). Debug builds keep the console, for what they print.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod core;
mod ui;

fn main() -> eframe::Result {
    // SAFETY: first thing, before any other thread starts.
    unsafe { earshot_engine::output::ask_pipewire_for_small_chunks() };
    // Where everything goes (core::folders), with what older versions kept elsewhere
    // moved in first.
    let folders = core::folders::Folders::real();
    let moved = core::folders::adopt_old(&folders);
    // A log per run in the state folder, for after a crash or freeze.
    if let Err(e) = core::log::start(&folders.state) {
        eprintln!("not logging to a file: {e}");
    }
    for line in moved {
        eprintln!("folders: {line}");
    }
    // Whether the system is holding the CPU back (a low-power profile), for the top bar.
    core::power::watch();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("earshot")
            // Matches earshot.desktop, which is how Wayland docks find the icon
            // (gui/install-desktop.sh).
            .with_app_id("earshot")
            .with_icon(icon())
            // No bigger than the screen, to start with or after (`ui::window`).
            .with_inner_size(ui::window::DEFAULT_SIZE)
            .with_min_inner_size(ui::window::MIN_SIZE)
            .with_clamp_size_to_monitor_size(true),
        vsync: true,
        persistence_path: Some(folders.window_state()),
        ..Default::default()
    };
    eframe::run_native(
        "earshot",
        options,
        Box::new(|cc| {
            // Not in the tests: their pictures use egui's own fonts only.
            ui::fonts::install_fallbacks(&cc.egui_ctx);
            match ui::app::EarshotApp::new(cc, core::services::Services::real(folders)) {
                Ok(app) => Ok(Box::new(app)),
                Err(e) => Err(e.into()),
            }
        }),
    )
}

/// The window icon (X11 and others; Wayland docks use the desktop file's).
fn icon() -> egui::IconData {
    let png = include_bytes!("../assets/icons/earshot-256.png");
    let img = image::load_from_memory(png).expect("icon").to_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}
