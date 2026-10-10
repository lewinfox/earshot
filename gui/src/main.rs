//! earshot's native UI (egui): a DJ library explorer and performance decks. See
//! ../README.md.

mod core;
mod ui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("earshot")
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1000.0, 640.0]),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "earshot",
        options,
        Box::new(|cc| match ui::app::EarshotApp::new(cc) {
            Ok(app) => Ok(Box::new(app)),
            Err(e) => Err(e.into()),
        }),
    )
}
