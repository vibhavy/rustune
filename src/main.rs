mod api;
mod backend;
mod icons;
mod sdk;
mod ui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("rustune=info,librespot=warn"))
        .init();

    // Window / Dock icon, rendered from assets/icon.svg by assets/build_icons.sh.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-512.png"))
        .expect("embedded icon is a valid PNG");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("rustune")
            .with_icon(icon)
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "rustune",
        options,
        Box::new(|cc| {
            let (tx, rx) = std::sync::mpsc::channel();
            let cmd = backend::spawn(cc.egui_ctx.clone(), tx);
            Ok(Box::new(ui::App::new(cc, cmd, rx)))
        }),
    )
}
