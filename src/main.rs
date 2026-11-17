mod api;
mod backend;
mod covers;
mod icons;
mod lyrics;
mod reopen;
mod sdk;
mod tray;
mod ui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("rustune=info,librespot=warn"))
        .init();

    // Window / Dock icon, rendered from assets/icon.svg by assets/build_icons.sh.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-512.png"))
        .expect("embedded icon is a valid PNG");
    let mut options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("rustune")
            .with_icon(icon)
            // Content runs under a transparent title bar so the window is themed edge
            // to edge; the UI keeps the top strip clear and draggable.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            // All windows share one renderer, which only enables transparency if the
            // main window asks for it; the rounded mini player needs it. The main
            // window paints every pixel opaque, so it looks the same.
            .with_transparent(true)
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    // A music player doesn't need the discrete GPU on Macs that have two.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.power_preference = eframe::egui_wgpu::wgpu::PowerPreference::LowPower;
    }

    eframe::run_native(
        "rustune",
        options,
        Box::new(|cc| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (cmd, rt) = backend::spawn(cc.egui_ctx.clone(), tx);
            covers::install(&cc.egui_ctx, rt);
            Ok(Box::new(ui::App::new(cc, cmd, rx)))
        }),
    )
}
