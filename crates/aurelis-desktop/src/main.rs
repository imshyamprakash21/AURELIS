mod ui;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "AURELIS",
        options,
        Box::new(|creation_context| Ok(Box::new(ui::AurelisDesktop::new(creation_context)))),
    )
}
