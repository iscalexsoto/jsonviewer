#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod common;
mod diff;
mod grid;
mod highlight;
mod json;
mod settings;
mod table;
mod text;
mod tree;

fn main() -> eframe::Result {
    let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Visor JSON")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Visor JSON", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, files)))))
}
