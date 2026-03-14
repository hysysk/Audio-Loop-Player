mod app;
mod audio;
mod theme;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 650.0])
            .with_min_inner_size([700.0, 450.0])
            .with_title("Audio Player — Language Learning"),
        ..Default::default()
    };

    eframe::run_native(
        "audioplayer",
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
