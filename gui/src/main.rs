//! earshot's native UI (egui): a DJ library explorer and performance decks. See
//! ../README.md.

mod core;
mod ui;

fn main() -> eframe::Result {
    // A log per run in the data folder, for after a crash or freeze.
    if let Err(e) = core::log::start(&core::services::data_dir()) {
        eprintln!("not logging to a file: {e}");
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("earshot")
            // Matches earshot.desktop, which is how Wayland docks find the icon
            // (gui/install-desktop.sh). Also names eframe's storage folder.
            .with_app_id("earshot")
            .with_icon(icon())
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1000.0, 640.0]),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "earshot",
        options,
        Box::new(|cc| {
            // Not in the tests: their pictures use egui's own fonts only.
            ui::fonts::install_fallbacks(&cc.egui_ctx);
            match ui::app::EarshotApp::new(cc, core::services::Services::real()) {
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
