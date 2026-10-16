//! System tray / menu bar icon. One backend per platform, all behind the same
//! small [`Tray`] API; every interaction is forwarded to the UI thread as a
//! [`TrayEvent`].
//!
//! - Linux: StatusNotifierItem over D-Bus (ksni), like the network/volume applets.
//! - macOS: an NSStatusItem in the menu bar (tray-icon).
//! - elsewhere: no tray; [`Tray::spawn`] returns `None` and the app runs without one.

use crate::InputOption;
use eframe::egui;
use std::sync::mpsc::Sender;

#[cfg(target_os = "linux")]
mod sni;
#[cfg(target_os = "linux")]
pub use sni::Tray;

#[cfg(target_os = "macos")]
mod status_item;
#[cfg(target_os = "macos")]
pub use status_item::Tray;

/// Snapshot of one monitor shown in the tray menu.
#[derive(Clone, PartialEq)]
pub struct TrayMonitor {
    pub name: String,
    pub input: Option<u8>,
    pub inputs: Vec<InputOption>,
}

/// Actions triggered from the tray icon, handled on the UI thread.
pub enum TrayEvent {
    ToggleWindow,
    Rescan,
    SetInput {
        idx: usize,
        code: u8,
    },
    /// Adjust brightness on all monitors by this many VCP units (scroll wheel).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    BrightnessDelta(i32),
    Quit,
}

/// Forwards tray interactions to the UI thread and wakes its event loop.
#[derive(Clone)]
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
struct Emitter {
    events: Sender<TrayEvent>,
    ctx: egui::Context,
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
impl Emitter {
    fn emit(&self, event: TrayEvent) {
        let _ = self.events.send(event);
        // Wake the UI event loop so the event is handled promptly.
        self.ctx.request_repaint();
    }
}

/// Monitors whose input can be switched from the tray menu, with the index of
/// the currently selected option.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn switchable_inputs(
    monitors: &[TrayMonitor],
) -> impl Iterator<Item = (usize, &TrayMonitor, usize)> {
    monitors.iter().enumerate().filter_map(|(idx, m)| {
        let input = m.input?;
        let selected = m.inputs.iter().position(|o| o.code == input)?;
        Some((idx, m, selected))
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub struct Tray;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Tray {
    pub fn spawn(_events: Sender<TrayEvent>, _ctx: egui::Context) -> Option<Self> {
        None
    }

    pub fn update(&self, _monitors: &[TrayMonitor], _hidden: bool) -> bool {
        false
    }

    pub fn shutdown(self) {}
}
