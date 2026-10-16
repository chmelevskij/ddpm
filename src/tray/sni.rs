//! Linux: StatusNotifierItem — the KDE/freedesktop tray protocol, the same one
//! the network/volume icons use. ksni runs it on its own service thread.

use super::{Emitter, TrayEvent, TrayMonitor, switchable_inputs};
use crate::APP_ID;
use eframe::egui;
use ksni::blocking::TrayMethods;
use std::sync::mpsc::Sender;

/// Brightness change per scroll-wheel notch on the tray icon, in VCP units.
const SCROLL_BRIGHTNESS_STEP: i32 = 5;

/// Normalize an SNI scroll delta to wheel notches (Plasma sends ±120 per notch,
/// other environments may send single units).
fn scroll_notches(delta: i32) -> i32 {
    if delta.abs() >= 120 {
        delta / 120
    } else {
        delta.signum()
    }
}

pub struct Tray(ksni::blocking::Handle<DdpmTray>);

impl Tray {
    pub fn spawn(events: Sender<TrayEvent>, ctx: egui::Context) -> Option<Self> {
        let tray = DdpmTray {
            emitter: Emitter { events, ctx },
            monitors: Vec::new(),
            hidden: false,
        };
        match tray.spawn() {
            Ok(handle) => Some(Self(handle)),
            Err(err) => {
                // No StatusNotifier host (or no D-Bus): run without a tray.
                log::warn!("system tray unavailable: {err}");
                None
            }
        }
    }

    /// Replace the menu state; returns false if the tray service is gone.
    pub fn update(&self, monitors: &[TrayMonitor], hidden: bool) -> bool {
        let monitors = monitors.to_vec();
        self.0
            .update(move |t| {
                t.monitors = monitors;
                t.hidden = hidden;
            })
            .is_some()
    }

    pub fn shutdown(self) {
        self.0.shutdown().wait();
    }
}

struct DdpmTray {
    emitter: Emitter,
    monitors: Vec<TrayMonitor>,
    hidden: bool,
}

impl DdpmTray {
    fn emit(&self, event: TrayEvent) {
        self.emitter.emit(event);
    }
}

impl ksni::Tray for DdpmTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "DDPM".into()
    }

    fn icon_name(&self) -> String {
        "video-display".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            title: "DDPM — Monitor Control".into(),
            description: "Scroll to adjust brightness".into(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.emit(TrayEvent::ToggleWindow);
    }

    fn scroll(&mut self, delta: i32, orientation: ksni::Orientation) {
        if orientation == ksni::Orientation::Vertical && delta != 0 {
            self.emit(TrayEvent::BrightnessDelta(
                scroll_notches(delta) * SCROLL_BRIGHTNESS_STEP,
            ));
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};

        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: if self.hidden {
                    "Show Window"
                } else {
                    "Hide to Tray"
                }
                .into(),
                icon_name: "video-display".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(TrayEvent::ToggleWindow)),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
        ];
        let mut any_inputs = false;
        // Input switching straight from the tray, one submenu per monitor.
        for (idx, m, selected) in switchable_inputs(&self.monitors) {
            any_inputs = true;
            let codes: Vec<u8> = m.inputs.iter().map(|o| o.code).collect();
            items.push(
                SubMenu {
                    label: m.name.clone(),
                    submenu: vec![
                        RadioGroup {
                            selected,
                            select: Box::new(move |tray: &mut Self, i| {
                                if let Some(&code) = codes.get(i) {
                                    tray.emit(TrayEvent::SetInput { idx, code });
                                }
                            }),
                            options: m
                                .inputs
                                .iter()
                                .map(|o| RadioItem {
                                    label: o.label.clone(),
                                    ..Default::default()
                                })
                                .collect(),
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }
        if any_inputs {
            items.push(ksni::MenuItem::Separator);
        }
        items.push(
            StandardItem {
                label: "Rescan Monitors".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(TrayEvent::Rescan)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(TrayEvent::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_deltas_normalize_to_notches() {
        for (delta, notches) in [
            (120, 1),
            (-120, -1),
            (240, 2),
            (-360, -3),
            (1, 1),
            (-3, -1),
            (0, 0),
        ] {
            assert_eq!(scroll_notches(delta), notches, "delta {delta}");
        }
    }
}
