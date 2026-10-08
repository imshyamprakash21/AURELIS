mod components;
mod theme;
mod ui;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size(theme::window::INITIAL_SIZE)
            .with_min_inner_size(theme::window::MINIMUM_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        "AURELIS",
        options,
        Box::new(|creation_context| Ok(Box::new(ui::AurelisDesktop::new(creation_context)))),
    )
}
