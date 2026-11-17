//! macOS menu bar (status bar) item: left-click toggles the mini player, right-click
//! opens a menu with the current track and playback controls.

use anyhow::{Context, Result};
use eframe::egui;
use std::sync::mpsc::Sender;
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

#[derive(Debug)]
pub enum TrayMsg {
    /// Left click on the icon. `anchor` is the icon's bottom-centre in physical pixels.
    ToggleMini { anchor: Option<(f64, f64)> },
    PlayPause,
    Next,
    Prev,
    ShowMini,
    ShowApp,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    now: MenuItem,
    play: MenuItem,
    /// Last values pushed to the menu, to avoid redundant native updates.
    shown: (String, bool),
}

impl Tray {
    pub fn new(ctx: &egui::Context, tx: Sender<TrayMsg>) -> Result<Self> {
        let now = MenuItem::new("Nothing playing", false, None);
        let play = MenuItem::new("Play", true, None);
        let next = MenuItem::new("Next", true, None);
        let prev = MenuItem::new("Previous", true, None);
        let mini = MenuItem::new("Mini Player", true, None);
        let open = MenuItem::new("Open rustune", true, None);
        let quit = MenuItem::new("Quit rustune", true, None);

        let menu = Menu::new();
        for item in [
            &now as &dyn tray_icon::menu::IsMenuItem,
            &PredefinedMenuItem::separator(),
            &play,
            &next,
            &prev,
            &PredefinedMenuItem::separator(),
            &mini,
            &open,
            &PredefinedMenuItem::separator(),
            &quit,
        ] {
            menu.append(item)?;
        }

        let ids = [
            (play.id().clone(), 0u8),
            (next.id().clone(), 1),
            (prev.id().clone(), 2),
            (mini.id().clone(), 3),
            (open.id().clone(), 4),
            (quit.id().clone(), 5),
        ];
        let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let msg = match ids.iter().find(|(id, _)| *id == e.id).map(|(_, k)| *k) {
                Some(0) => TrayMsg::PlayPause,
                Some(1) => TrayMsg::Next,
                Some(2) => TrayMsg::Prev,
                Some(3) => TrayMsg::ShowMini,
                Some(4) => TrayMsg::ShowApp,
                Some(5) => TrayMsg::Quit,
                _ => return,
            };
            let _ = menu_tx.send(msg);
            menu_ctx.request_repaint();
        }));

        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = e
            {
                let anchor = (rect.size.width > 0).then(|| {
                    (
                        rect.position.x + rect.size.width as f64 / 2.0,
                        rect.position.y + rect.size.height as f64,
                    )
                });
                let _ = tx.send(TrayMsg::ToggleMini { anchor });
                click_ctx.request_repaint();
            }
        }));

        let png = image::load_from_memory(include_bytes!("../assets/tray.png"))
            .context("tray icon")?
            .into_rgba8();
        let (w, h) = png.dimensions();
        let icon = TrayIconBuilder::new()
            // Template icon: macOS tints it to match light or dark menu bars.
            .with_icon_templated(tray_icon::Icon::from_rgba(png.into_raw(), w, h)?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("rustune")
            .build()?;

        Ok(Self { icon, now, play, shown: (String::new(), false) })
    }

    /// Reflect the current track and play state in the menu and tooltip.
    pub fn update(&mut self, track: Option<(&str, &str)>, playing: bool) {
        let label = match track {
            Some((name, artists)) => format!("{name} — {artists}"),
            None => "Nothing playing".to_string(),
        };
        if self.shown.0 == label && self.shown.1 == playing {
            return;
        }
        // Keep the menu narrow.
        let short: String = if label.chars().count() > 48 {
            label.chars().take(47).chain(['…']).collect()
        } else {
            label.clone()
        };
        self.now.set_text(&short);
        self.play.set_text(if playing { "Pause" } else { "Play" });
        let tip = if track.is_some() { format!("rustune · {short}") } else { "rustune".into() };
        let _ = self.icon.set_tooltip(Some(tip));
        self.shown = (label, playing);
    }
}
