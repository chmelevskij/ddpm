//! macOS: an NSStatusItem in the menu bar. AppKit requires it to be created on
//! the main thread, which is where eframe builds the app; clicking the icon
//! opens the menu (the macOS convention), so there is no scroll-to-adjust.

use super::{Emitter, TrayEvent, TrayMonitor, switchable_inputs};
use eframe::egui;
use std::sync::mpsc::Sender;
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ID_TOGGLE: &str = "toggle";
const ID_RESCAN: &str = "rescan";
const ID_QUIT: &str = "quit";
const ID_INPUT_PREFIX: &str = "input:";

pub struct Tray(TrayIcon);

impl Tray {
    pub fn spawn(events: Sender<TrayEvent>, ctx: egui::Context) -> Option<Self> {
        let emitter = Emitter { events, ctx };
        // Menu clicks arrive on the main thread from AppKit; translate the item id.
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(event) = parse_menu_id(&event.id.0) {
                emitter.emit(event);
            }
        }));
        let icon = match Icon::from_rgba(monitor_glyph(), GLYPH_SIZE, GLYPH_SIZE) {
            Ok(icon) => icon,
            Err(err) => {
                log::warn!("menu bar icon unavailable: {err}");
                return None;
            }
        };
        match TrayIconBuilder::new()
            .with_icon_templated(icon)
            .with_tooltip("DDPM — Monitor Control")
            .with_menu(Box::new(build_menu(&[], false)))
            .build()
        {
            Ok(tray) => Some(Self(tray)),
            Err(err) => {
                log::warn!("menu bar icon unavailable: {err}");
                None
            }
        }
    }

    /// Rebuild the menu from the current state.
    pub fn update(&self, monitors: &[TrayMonitor], hidden: bool) -> bool {
        self.0
            .set_menu(Some(Box::new(build_menu(monitors, hidden))));
        true
    }

    pub fn shutdown(self) {
        let _ = self.0.set_visible(false);
    }
}

fn input_id(idx: usize, code: u8) -> String {
    format!("{ID_INPUT_PREFIX}{idx}:{code}")
}

fn parse_menu_id(id: &str) -> Option<TrayEvent> {
    match id {
        ID_TOGGLE => Some(TrayEvent::ToggleWindow),
        ID_RESCAN => Some(TrayEvent::Rescan),
        ID_QUIT => Some(TrayEvent::Quit),
        _ => {
            let (idx, code) = id.strip_prefix(ID_INPUT_PREFIX)?.split_once(':')?;
            Some(TrayEvent::SetInput {
                idx: idx.parse().ok()?,
                code: code.parse().ok()?,
            })
        }
    }
}

fn build_menu(monitors: &[TrayMonitor], hidden: bool) -> Menu {
    let toggle = MenuItem::with_id(
        ID_TOGGLE,
        if hidden { "Show Window" } else { "Hide Window" },
        true,
        None,
    );
    // Input switching straight from the menu bar, one submenu per monitor.
    let submenus: Vec<Submenu> = switchable_inputs(monitors)
        .map(|(idx, m, selected)| {
            let options: Vec<CheckMenuItem> = m
                .inputs
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    CheckMenuItem::with_id(
                        input_id(idx, o.code),
                        &o.label,
                        true,
                        i == selected,
                        None,
                    )
                })
                .collect();
            let refs: Vec<&dyn IsMenuItem> = options.iter().map(|o| o as &dyn IsMenuItem).collect();
            Submenu::with_items(&m.name, true, &refs).expect("failed to build input submenu")
        })
        .collect();
    let rescan = MenuItem::with_id(ID_RESCAN, "Rescan Monitors", true, None);
    let quit = MenuItem::with_id(ID_QUIT, "Quit DDPM", true, None);

    let menu = Menu::new();
    let separator = PredefinedMenuItem::separator();
    let mut items: Vec<&dyn IsMenuItem> = vec![&toggle, &separator];
    let separator2 = PredefinedMenuItem::separator();
    if !submenus.is_empty() {
        items.extend(submenus.iter().map(|s| s as &dyn IsMenuItem));
        items.push(&separator2);
    }
    items.extend([&rescan as &dyn IsMenuItem, &quit]);
    menu.append_items(&items)
        .expect("failed to build tray menu");
    menu
}

/// Side length of the menu bar glyph in pixels (18pt at 2x).
const GLYPH_SIZE: u32 = 36;

/// A monitor-on-a-stand glyph as RGBA. It is drawn as a template image, so only
/// the alpha channel matters and macOS tints it for light/dark menu bars.
fn monitor_glyph() -> Vec<u8> {
    let n = GLYPH_SIZE as i32;
    let inside = |x: i32, y: i32, (x0, y0, x1, y1): (i32, i32, i32, i32)| {
        (x0..x1).contains(&x) && (y0..y1).contains(&y)
    };
    let screen = (2, 5, 34, 26);
    let panel = (5, 8, 31, 23);
    let neck = (16, 26, 20, 30);
    let base = (10, 30, 26, 33);
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let on = (inside(x, y, screen) && !inside(x, y, panel))
                || inside(x, y, neck)
                || inside(x, y, base);
            rgba.extend_from_slice(&[0, 0, 0, if on { 255 } else { 0 }]);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_round_trip() {
        assert!(matches!(
            parse_menu_id(&input_id(2, 0x19)),
            Some(TrayEvent::SetInput { idx: 2, code: 0x19 })
        ));
        assert!(matches!(parse_menu_id(ID_QUIT), Some(TrayEvent::Quit)));
        assert!(parse_menu_id("input:x:1").is_none());
        assert!(parse_menu_id("bogus").is_none());
    }
}
