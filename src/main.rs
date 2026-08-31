//! ddpm — DDC/CI monitor control (brightness, contrast, input source).
//!
//! All DDC/CI traffic happens on a dedicated worker thread that owns the
//! display handles; the UI thread only exchanges messages with it, so the
//! window never blocks on the (slow, sleep-heavy) i2c protocol.

use ddc_hi::{Ddc, Display, FeatureCode, Handle, VcpValue};
use eframe::egui;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// Wayland app id; must match the desktop file name (`ddpm.desktop`).
const APP_ID: &str = "ddpm";

const VCP_BRIGHTNESS: FeatureCode = 0x10; // MCCS: Luminance
const VCP_CONTRAST: FeatureCode = 0x12; // MCCS: Contrast
const VCP_INPUT_SOURCE: FeatureCode = 0x60; // MCCS: Input Select (non-continuous, value in SL)

/// Consecutive write failures after which a monitor's controls are disabled until "Retry".
const MAX_FAILURES: u8 = 3;
/// Minimum interval between writes while a slider is being dragged.
const DRAG_WRITE_INTERVAL: Duration = Duration::from_millis(150);
/// How long to wait after an input switch before reading the input back.
const INPUT_READBACK_DELAY: Duration = Duration::from_secs(1);
/// Do not re-read values on focus gain if they were read more recently than this.
const FOCUS_RELOAD_MIN_AGE: Duration = Duration::from_secs(2);
/// Errors kept per monitor.
const MAX_ERRORS: usize = 6;

/// MCCS 2.x input-source codes (VCP 0x60). Used as the fallback list when the
/// monitor's capabilities cannot be read, and for labelling.
const INPUT_SOURCES: &[(u8, &str)] = &[
    (0x01, "VGA-1"),
    (0x02, "VGA-2"),
    (0x03, "DVI-1"),
    (0x04, "DVI-2"),
    (0x05, "Composite-1"),
    (0x06, "Composite-2"),
    (0x07, "S-Video-1"),
    (0x08, "S-Video-2"),
    (0x09, "Tuner-1"),
    (0x0A, "Tuner-2"),
    (0x0B, "Tuner-3"),
    (0x0C, "Component-1"),
    (0x0D, "Component-2"),
    (0x0E, "Component-3"),
    (0x0F, "DisplayPort-1"),
    (0x10, "DisplayPort-2"),
    (0x11, "HDMI-1"),
    (0x12, "HDMI-2"),
    (0x19, "USB-C-1"),
    (0x1B, "USB-C-2"),
];

fn input_label(code: u8) -> String {
    INPUT_SOURCES
        .iter()
        .find(|(v, _)| *v == code)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| format!("Unknown (0x{code:02X})"))
}

fn feature_name(code: FeatureCode) -> &'static str {
    match code {
        VCP_BRIGHTNESS => "brightness",
        VCP_CONTRAST => "contrast",
        VCP_INPUT_SOURCE => "input",
        _ => "feature",
    }
}

/// ddc-hi reports a Linux display's id as the decimal `rdev` of its device node.
/// Turn that back into `/dev/i2c-N` (i2c-dev is char major 89).
fn i2c_device_path(rdev: &str) -> Option<String> {
    let dev: u64 = rdev.parse().ok()?;
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major == 89).then(|| format!("/dev/i2c-{minor}"))
}

fn format_value(v: f64, max: u16) -> String {
    if max == 100 {
        format!("{v:.0}%")
    } else {
        format!("{v:.0} / {max}")
    }
}

// ───────────────────────── Theme ─────────────────────────

/// Accent used for slider fills, selection and hover highlights (Breeze blue).
const ACCENT: egui::Color32 = egui::Color32::from_rgb(61, 174, 233);

fn tune_style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.slider_rail_height = 6.0;
    style.spacing.combo_width = 150.0;
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(17.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();
}

fn themed_visuals(theme: egui::Theme) -> egui::Visuals {
    use egui::{Color32, CornerRadius, Stroke, Theme};
    let mut v = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
    };
    if theme == Theme::Dark {
        v.panel_fill = Color32::from_rgb(0x16, 0x18, 0x1D); // page
        v.window_fill = Color32::from_rgb(0x1F, 0x23, 0x2B); // cards
        v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x30, 0x35, 0x40));
        v.extreme_bg_color = Color32::from_rgb(0x12, 0x14, 0x18); // slider rail / text edits
        v.faint_bg_color = Color32::from_rgb(0x26, 0x2B, 0x33);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x2A, 0x30, 0x3A); // buttons
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x34, 0x3B, 0x47);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0x3D, 0x45, 0x53);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x40, 0x4C));
    } else {
        v.panel_fill = Color32::from_rgb(0xEE, 0xF0, 0xF3);
        v.window_fill = Color32::WHITE;
        v.window_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0xD8, 0xDC, 0xE2));
        v.extreme_bg_color = Color32::from_rgb(0xE6, 0xE9, 0xEE);
        v.faint_bg_color = Color32::from_rgb(0xF3, 0xF5, 0xF8);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xF7, 0xF8, 0xFA);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xEC, 0xEF, 0xF3);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0xE2, 0xE6, 0xEC);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0xC9, 0xCE, 0xD6));
    }
    v.selection.bg_fill = ACCENT;
    v.hyperlink_color = ACCENT;
    v.slider_trailing_fill = true;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.9));
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    let r = CornerRadius::same(6);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v
}

fn apply_style(ctx: &egui::Context) {
    use egui::Theme;
    ctx.style_mut_of(Theme::Dark, tune_style);
    ctx.style_mut_of(Theme::Light, tune_style);
    ctx.set_visuals_of(Theme::Dark, themed_visuals(Theme::Dark));
    ctx.set_visuals_of(Theme::Light, themed_visuals(Theme::Light));
}

/// Rounded, tinted box for errors and warnings.
fn notice_frame(ui: &mut egui::Ui, color: egui::Color32, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.35)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

fn notice(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    notice_frame(ui, color, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(color, format!("⚠ {text}"));
        });
    });
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,ddpm=info"))
        .init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_inner_size([460.0, 460.0])
            .with_min_inner_size([380.0, 260.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DDPM - Monitor Control",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

// ───────────────────────── DDC helpers (worker thread only) ─────────────────────────

/// The ddc-i2c reply parser can index out of bounds on a malformed reply, so every
/// transaction is wrapped in `catch_unwind` and reported as an ordinary error.
fn read_vcp(handle: &mut Handle, code: FeatureCode) -> Result<VcpValue, String> {
    let mut last_err = String::new();
    // DDC reads fail transiently; try twice before giving up.
    for _ in 0..2 {
        match catch_unwind(AssertUnwindSafe(|| handle.get_vcp_feature(code))) {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) => last_err = format!("{e:#}"),
            Err(_) => last_err = "DDC layer panicked on a malformed reply".into(),
        }
    }
    Err(last_err)
}

fn write_vcp(handle: &mut Handle, code: FeatureCode, value: u16) -> Result<(), String> {
    match catch_unwind(AssertUnwindSafe(|| handle.set_vcp_feature(code, value))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err("DDC layer panicked".into()),
    }
}

/// Input sources advertised by the monitor's capabilities string, if readable.
fn supported_inputs(handle: &mut Handle) -> Option<Vec<InputOption>> {
    // The multi-fragment capabilities read is the flakiest DDC transaction; try twice.
    let mut caps = None;
    for attempt in 1..=2 {
        match catch_unwind(AssertUnwindSafe(|| handle.capabilities())) {
            Ok(Ok(c)) => {
                caps = Some(c);
                break;
            }
            Ok(Err(e)) => log::warn!("capabilities read attempt {attempt} failed: {e:#}"),
            Err(_) => log::warn!("capabilities read attempt {attempt} panicked"),
        }
    }
    let caps = caps?;
    let desc = caps.vcp_features.get(&VCP_INPUT_SOURCE)?;
    if desc.values.is_empty() {
        return None;
    }
    Some(
        desc.values
            .iter()
            .map(|(&code, name)| InputOption {
                code,
                label: name.clone().unwrap_or_else(|| input_label(code)),
            })
            .collect(),
    )
}

// ───────────────────────── State shared with the UI ─────────────────────────

#[derive(Clone, Debug)]
struct Control {
    /// Value shown by the slider (may be ahead of the monitor while a write is pending).
    value: u16,
    /// Last value the monitor confirmed.
    applied: u16,
    max: u16,
    last_sent_value: Option<u16>,
    last_sent_at: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq)]
struct InputOption {
    code: u8,
    label: String,
}

#[derive(Clone, Debug, Default)]
struct MonitorState {
    name: String,
    ident: String,
    brightness: Option<Control>,
    contrast: Option<Control>,
    /// Input reported by the monitor (VCP 0x60 SL byte); `None` if unreadable.
    input: Option<u8>,
    /// Input selected in the dropdown but not yet sent.
    pending_input: Option<u8>,
    /// Inputs from the capabilities string; empty until read (falls back to `INPUT_SOURCES`).
    inputs: Vec<InputOption>,
    errors: Vec<String>,
    failures: u8,
}

impl MonitorState {
    fn probe(display: &mut Display) -> Self {
        let info = &display.info;
        let name = info
            .model_name
            .clone()
            .or_else(|| info.model_id.map(|m| format!("model {m:#06x}")))
            .unwrap_or_else(|| "Unknown display".into());
        let ident = [
            info.manufacturer_id.clone(),
            info.serial_number.clone().map(|s| format!("S/N {s}")),
            Some(i2c_device_path(&info.id).unwrap_or_else(|| format!("id {}", info.id))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

        let mut state = Self {
            name,
            ident,
            ..Default::default()
        };
        state.read_values(&mut display.handle);
        state
    }

    /// (Re)read brightness, contrast and input from the monitor.
    fn read_values(&mut self, handle: &mut Handle) {
        self.brightness = self.read_control(handle, VCP_BRIGHTNESS);
        self.contrast = self.read_control(handle, VCP_CONTRAST);
        self.input = match read_vcp(handle, VCP_INPUT_SOURCE) {
            // Non-continuous feature: the value is the SL byte; some monitors echo it into SH.
            Ok(v) => Some(v.sl),
            Err(e) => {
                self.push_error(format!("input: {e}"));
                None
            }
        };
    }

    fn read_control(&mut self, handle: &mut Handle, code: FeatureCode) -> Option<Control> {
        match read_vcp(handle, code) {
            Ok(v) if v.maximum() == 0 => {
                self.push_error(format!(
                    "{}: monitor reported a maximum of 0",
                    feature_name(code)
                ));
                None
            }
            Ok(v) => {
                let max = v.maximum();
                let value = v.value().min(max);
                Some(Control {
                    value,
                    applied: value,
                    max,
                    last_sent_value: None,
                    last_sent_at: None,
                })
            }
            Err(e) => {
                self.push_error(format!("{}: {e}", feature_name(code)));
                None
            }
        }
    }

    /// Take freshly read values, keeping UI-only state (errors, pending selection, inputs).
    fn apply_reload(&mut self, fresh: MonitorState) {
        self.brightness = fresh.brightness;
        self.contrast = fresh.contrast;
        self.input = fresh.input;
        if self.pending_input == self.input {
            self.pending_input = None;
        }
        for e in fresh.errors {
            self.push_error(e);
        }
    }

    fn control_mut(&mut self, code: FeatureCode) -> Option<&mut Control> {
        match code {
            VCP_BRIGHTNESS => self.brightness.as_mut(),
            VCP_CONTRAST => self.contrast.as_mut(),
            _ => None,
        }
    }

    fn push_error(&mut self, err: String) {
        if self.errors.last() == Some(&err) {
            return;
        }
        if self.errors.len() >= MAX_ERRORS {
            self.errors.remove(0);
        }
        self.errors.push(err);
    }

    /// Selectable inputs: the monitor's advertised list (or the generic one), always
    /// including whatever the monitor currently reports so it can be switched back to.
    fn input_options(&self) -> Vec<InputOption> {
        let mut options = if self.inputs.is_empty() {
            INPUT_SOURCES
                .iter()
                .map(|&(code, label)| InputOption {
                    code,
                    label: label.to_string(),
                })
                .collect()
        } else {
            self.inputs.clone()
        };
        if let Some(cur) = self.input
            && !options.iter().any(|o| o.code == cur)
        {
            options.push(InputOption {
                code: cur,
                label: input_label(cur),
            });
        }
        options
    }
}

// ───────────────────────── Worker thread ─────────────────────────

enum Cmd {
    Set {
        idx: usize,
        code: FeatureCode,
        value: u16,
    },
    /// Re-read values on the existing handles.
    Reload,
    /// Re-enumerate monitors.
    Rescan,
}

enum Msg {
    Scanned {
        monitors: Vec<MonitorState>,
        error: Option<String>,
    },
    Inputs {
        idx: usize,
        inputs: Vec<InputOption>,
    },
    Reloaded {
        idx: usize,
        state: MonitorState,
    },
    SetOk {
        idx: usize,
        code: FeatureCode,
        value: u16,
        /// For input switches: what the monitor reported afterwards.
        readback: Option<u8>,
    },
    SetErr {
        idx: usize,
        code: FeatureCode,
        value: u16,
        err: String,
    },
}

#[derive(Default, Debug, PartialEq)]
struct Batch {
    rescan: bool,
    reload: bool,
    /// Latest value per (monitor, feature), in first-seen order.
    sets: Vec<(usize, FeatureCode, u16)>,
}

/// Collapse a burst of queued commands: keep only the newest value per control.
fn coalesce(cmds: impl IntoIterator<Item = Cmd>) -> Batch {
    let mut batch = Batch::default();
    for cmd in cmds {
        match cmd {
            Cmd::Rescan => batch.rescan = true,
            Cmd::Reload => batch.reload = true,
            Cmd::Set { idx, code, value } => {
                match batch
                    .sets
                    .iter_mut()
                    .find(|(i, c, _)| *i == idx && *c == code)
                {
                    Some(slot) => slot.2 = value,
                    None => batch.sets.push((idx, code, value)),
                }
            }
        }
    }
    batch
}

fn worker(rx: Receiver<Cmd>, tx: Sender<Msg>, ctx: egui::Context) {
    let post = |msg: Msg| -> bool {
        let ok = tx.send(msg).is_ok();
        ctx.request_repaint();
        ok
    };

    let mut displays = scan(&post);
    while let Ok(first) = rx.recv() {
        let mut cmds = vec![first];
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        let batch = coalesce(cmds);

        if batch.rescan {
            // Queued sets refer to indices that may no longer be valid; drop them.
            displays = scan(&post);
            continue;
        }
        for (idx, code, value) in batch.sets {
            let Some(display) = displays.get_mut(idx) else {
                continue;
            };
            let msg = match write_vcp(&mut display.handle, code, value) {
                Ok(()) => {
                    let readback = (code == VCP_INPUT_SOURCE).then(|| {
                        thread::sleep(INPUT_READBACK_DELAY);
                        read_vcp(&mut display.handle, code).ok().map(|v| v.sl)
                    });
                    Msg::SetOk {
                        idx,
                        code,
                        value,
                        readback: readback.flatten(),
                    }
                }
                Err(err) => Msg::SetErr {
                    idx,
                    code,
                    value,
                    err,
                },
            };
            if !post(msg) {
                return;
            }
        }
        if batch.reload {
            for (idx, display) in displays.iter_mut().enumerate() {
                let mut state = MonitorState::default();
                state.read_values(&mut display.handle);
                if !post(Msg::Reloaded { idx, state }) {
                    return;
                }
            }
        }
    }
}

/// Enumerate monitors and read their values; then (slowly) read capabilities.
fn scan(post: &dyn Fn(Msg) -> bool) -> Vec<Display> {
    let mut displays = match catch_unwind(Display::enumerate) {
        Ok(d) => d,
        Err(_) => {
            log::error!("Display::enumerate panicked");
            Vec::new()
        }
    };
    let monitors: Vec<MonitorState> = displays.iter_mut().map(MonitorState::probe).collect();
    for m in &monitors {
        log::info!(
            "found {} ({}): brightness={:?} contrast={:?} input={:?}",
            m.name,
            m.ident,
            m.brightness.as_ref().map(|c| (c.value, c.max)),
            m.contrast.as_ref().map(|c| (c.value, c.max)),
            m.input.map(input_label)
        );
    }
    let error = displays.is_empty().then(|| {
        "No DDC/CI capable monitors found. i2c-dev must be loaded and your user needs \
         access to /dev/i2c-* (group `i2c`); `just doctor` checks both."
            .to_string()
    });
    if !post(Msg::Scanned { monitors, error }) {
        return displays;
    }
    // Capabilities are a slow multi-fragment read; deliver them after the first paint.
    for (idx, display) in displays.iter_mut().enumerate() {
        if let Some(inputs) = supported_inputs(&mut display.handle) {
            log::info!(
                "monitor {idx} advertises inputs: {}",
                inputs
                    .iter()
                    .map(|o| o.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if !post(Msg::Inputs { idx, inputs }) {
                break;
            }
        }
    }
    displays
}

// ───────────────────────── UI ─────────────────────────

struct App {
    cmd_tx: Sender<Cmd>,
    msg_rx: Receiver<Msg>,
    monitors: Vec<MonitorState>,
    scanning: bool,
    scan_error: Option<String>,
    worker_error: Option<String>,
    was_focused: bool,
    last_read: Instant,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        apply_style(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        thread::Builder::new()
            .name("ddc-worker".into())
            .spawn(move || worker(cmd_rx, msg_tx, ctx))
            .expect("failed to spawn DDC worker thread");
        Self {
            cmd_tx,
            msg_rx,
            monitors: Vec::new(),
            scanning: true,
            scan_error: None,
            worker_error: None,
            was_focused: true,
            last_read: Instant::now(),
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if matches!(cmd, Cmd::Rescan) {
            self.scanning = true;
        }
        if self.cmd_tx.send(cmd).is_err() {
            self.worker_error =
                Some("The DDC worker thread has stopped; restart the application.".into());
        }
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                Msg::Scanned { monitors, error } => {
                    self.monitors = monitors;
                    self.scan_error = error;
                    self.scanning = false;
                    self.last_read = Instant::now();
                }
                Msg::Inputs { idx, inputs } => {
                    if let Some(m) = self.monitors.get_mut(idx) {
                        m.inputs = inputs;
                    }
                }
                Msg::Reloaded { idx, state } => {
                    if let Some(m) = self.monitors.get_mut(idx) {
                        m.apply_reload(state);
                    }
                    self.last_read = Instant::now();
                }
                Msg::SetOk {
                    idx,
                    code,
                    value,
                    readback,
                } => {
                    let Some(m) = self.monitors.get_mut(idx) else {
                        continue;
                    };
                    m.failures = 0;
                    if code == VCP_INPUT_SOURCE {
                        let requested = value as u8;
                        m.input = Some(readback.unwrap_or(requested));
                        m.pending_input = None;
                        if let Some(actual) = readback
                            && actual != requested
                        {
                            m.push_error(format!(
                                "input: asked for {} but monitor reports {}",
                                input_label(requested),
                                input_label(actual)
                            ));
                        }
                    } else if let Some(c) = m.control_mut(code) {
                        c.applied = value;
                    }
                }
                Msg::SetErr {
                    idx,
                    code,
                    value,
                    err,
                } => {
                    let Some(m) = self.monitors.get_mut(idx) else {
                        continue;
                    };
                    m.failures = m.failures.saturating_add(1);
                    if let Some(c) = m.control_mut(code) {
                        c.value = c.applied;
                        c.last_sent_value = None;
                    }
                    m.push_error(format!(
                        "{}: could not set {value}: {err}",
                        feature_name(code)
                    ));
                }
            }
        }
    }
}

/// One slider row inside the per-monitor grid. Writes are throttled while dragging
/// and committed on release; keyboard/typed edits commit immediately.
fn control_row(
    ui: &mut egui::Ui,
    label: &str,
    idx: usize,
    code: FeatureCode,
    control: Option<&mut Control>,
    enabled: bool,
    cmds: &mut Vec<Cmd>,
) {
    ui.label(label);
    match control {
        None => {
            ui.weak("unavailable");
        }
        Some(c) => {
            ui.spacing_mut().slider_width = (ui.available_width() - 110.0).max(120.0);
            let max = c.max;
            let slider = egui::Slider::new(&mut c.value, 0..=max)
                .custom_formatter(move |v, _| format_value(v, max));
            let resp = ui.add_enabled(enabled, slider);
            let now = Instant::now();
            let throttled = resp.dragged()
                && resp.changed()
                && c.last_sent_at
                    .is_none_or(|t| now.duration_since(t) >= DRAG_WRITE_INTERVAL);
            let commit = resp.drag_stopped() || (resp.changed() && !resp.dragged()) || throttled;
            // The monitor is out of date if the value differs from what it confirmed, or from
            // what was last sent to it (a write may still be in flight).
            let stale = c.value != c.applied || c.last_sent_value.is_some_and(|v| v != c.value);
            if commit && stale && c.last_sent_value != Some(c.value) {
                c.last_sent_value = Some(c.value);
                c.last_sent_at = Some(now);
                cmds.push(Cmd::Set {
                    idx,
                    code,
                    value: c.value,
                });
            }
        }
    }
    ui.end_row();
}

fn monitor_ui(
    ui: &mut egui::Ui,
    idx: usize,
    m: &mut MonitorState,
    scanning: bool,
    cmds: &mut Vec<Cmd>,
) {
    let locked_out = m.failures >= MAX_FAILURES;
    let enabled = !scanning && !locked_out;
    let visuals = ui.visuals().clone();

    egui::Frame::new()
        .fill(visuals.window_fill)
        .stroke(visuals.window_stroke)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("🖥").size(22.0));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(egui::RichText::new(&m.name).strong().size(15.5));
                    ui.label(egui::RichText::new(&m.ident).weak().small());
                });
            });
            ui.add_space(10.0);

            egui::Grid::new(("controls", idx))
                .num_columns(2)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    control_row(
                        ui,
                        "☀ Brightness",
                        idx,
                        VCP_BRIGHTNESS,
                        m.brightness.as_mut(),
                        enabled,
                        cmds,
                    );
                    control_row(
                        ui,
                        "◑ Contrast",
                        idx,
                        VCP_CONTRAST,
                        m.contrast.as_mut(),
                        enabled,
                        cmds,
                    );

                    ui.label("🔌 Input");
                    ui.horizontal(|ui| match m.input {
                        None => {
                            ui.weak("unavailable");
                        }
                        Some(current) => {
                            let options = m.input_options();
                            let mut selected = m.pending_input.unwrap_or(current);
                            ui.add_enabled_ui(enabled, |ui| {
                                egui::ComboBox::from_id_salt(("input", idx))
                                    .selected_text(input_label(selected))
                                    .show_ui(ui, |ui| {
                                        for o in &options {
                                            ui.selectable_value(&mut selected, o.code, &o.label);
                                        }
                                    });
                            });
                            m.pending_input = (selected != current).then_some(selected);
                            let switch = ui
                                .add_enabled(
                                    enabled && m.pending_input.is_some(),
                                    egui::Button::new("Switch"),
                                )
                                .on_hover_text(
                                    "Sends the input change to the monitor. If this computer is \
                                     not connected to the new input, the monitor will stop \
                                     showing it.",
                                );
                            if switch.clicked() {
                                cmds.push(Cmd::Set {
                                    idx,
                                    code: VCP_INPUT_SOURCE,
                                    value: selected as u16,
                                });
                            }
                        }
                    });
                    ui.end_row();
                });

            if !m.errors.is_empty() {
                ui.add_space(8.0);
                let color = visuals.error_fg_color;
                notice_frame(ui, color, |ui| {
                    for e in &m.errors {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(color, format!("⚠ {e}"));
                        });
                    }
                    ui.horizontal(|ui| {
                        if locked_out {
                            ui.colored_label(
                                visuals.warn_fg_color,
                                "Controls disabled after repeated failures.",
                            );
                            if ui.button("Retry").clicked() {
                                m.failures = 0;
                                cmds.push(Cmd::Reload);
                            }
                        }
                        if ui.small_button("Clear").clicked() {
                            m.errors.clear();
                        }
                    });
                });
            }
        });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages();

        // Re-read values when the window regains focus (OSD / other tools may have changed them).
        let focused = ctx.input(|i| i.focused);
        if focused
            && !self.was_focused
            && !self.scanning
            && !self.monitors.is_empty()
            && self.last_read.elapsed() >= FOCUS_RELOAD_MIN_AGE
        {
            self.send(Cmd::Reload);
        }
        self.was_focused = focused;

        let quit = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Q);
        if ctx.input_mut(|i| i.consume_shortcut(&quit)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let mut cmds = Vec::new();
        let visuals = ctx.style().visuals.clone();

        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(visuals.window_fill)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🖥").size(19.0));
                    ui.label(egui::RichText::new("DDPM").strong().size(16.5));
                    ui.add_space(2.0);
                    if self.scanning {
                        ui.weak("Scanning…");
                    } else {
                        let n = self.monitors.len();
                        ui.weak(format!("{n} monitor{}", if n == 1 { "" } else { "s" }));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::global_theme_preference_switch(ui);
                        ui.add_space(2.0);
                        if ui
                            .add_enabled(!self.scanning, egui::Button::new("⟳ Rescan"))
                            .on_hover_text("Re-detect monitors (after plugging one in)")
                            .clicked()
                        {
                            cmds.push(Cmd::Rescan);
                        }
                        if ui
                            .add_enabled(
                                !self.scanning && !self.monitors.is_empty(),
                                egui::Button::new("🔄 Reload"),
                            )
                            .on_hover_text("Re-read the current values from the monitors")
                            .clicked()
                        {
                            cmds.push(Cmd::Reload);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(visuals.panel_fill)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| {
                if let Some(err) = &self.worker_error {
                    notice(ui, ui.visuals().error_fg_color, err);
                    ui.add_space(8.0);
                }
                if let Some(err) = &self.scan_error {
                    notice(ui, ui.visuals().warn_fg_color, err);
                    ui.add_space(8.0);
                }
                if self.monitors.is_empty() {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        if self.scanning {
                            ui.spinner();
                            ui.add_space(8.0);
                            ui.weak("Scanning for monitors…");
                        } else if self.scan_error.is_none() {
                            ui.label(egui::RichText::new("🖥").size(42.0).weak());
                            ui.weak("No monitors found");
                        }
                    });
                    return;
                }
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let scanning = self.scanning;
                        for (idx, m) in self.monitors.iter_mut().enumerate() {
                            monitor_ui(ui, idx, m, scanning, &mut cmds);
                            ui.add_space(10.0);
                        }
                    });
            });

        for cmd in cmds {
            self.send(cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_labels() {
        assert_eq!(input_label(0x11), "HDMI-1");
        assert_eq!(input_label(0x19), "USB-C-1");
        assert_eq!(input_label(0x1C), "Unknown (0x1C)");
    }

    #[test]
    fn i2c_path_from_rdev() {
        // major 89, minor 5
        assert_eq!(i2c_device_path("22789").as_deref(), Some("/dev/i2c-5"));
        // major 1 (mem) is not an i2c device
        assert_eq!(i2c_device_path("259"), None);
        assert_eq!(i2c_device_path("not a number"), None);
    }

    #[test]
    fn value_formatting() {
        assert_eq!(format_value(42.0, 100), "42%");
        assert_eq!(format_value(128.0, 255), "128 / 255");
    }

    #[test]
    fn coalesce_keeps_latest_per_control_in_order() {
        let batch = coalesce([
            Cmd::Set {
                idx: 0,
                code: VCP_BRIGHTNESS,
                value: 10,
            },
            Cmd::Set {
                idx: 1,
                code: VCP_CONTRAST,
                value: 50,
            },
            Cmd::Set {
                idx: 0,
                code: VCP_BRIGHTNESS,
                value: 30,
            },
            Cmd::Reload,
        ]);
        assert_eq!(
            batch,
            Batch {
                rescan: false,
                reload: true,
                sets: vec![(0, VCP_BRIGHTNESS, 30), (1, VCP_CONTRAST, 50)],
            }
        );
    }

    #[test]
    fn input_options_always_include_current() {
        let mut m = MonitorState {
            inputs: vec![InputOption {
                code: 0x0F,
                label: "DisplayPort-1".into(),
            }],
            input: Some(0x19),
            ..Default::default()
        };
        let opts = m.input_options();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[1].code, 0x19);
        m.inputs.clear();
        assert_eq!(m.input_options().len(), INPUT_SOURCES.len());
    }

    #[test]
    fn errors_are_deduplicated_and_capped() {
        let mut m = MonitorState::default();
        for _ in 0..3 {
            m.push_error("same".into());
        }
        assert_eq!(m.errors.len(), 1);
        for i in 0..10 {
            m.push_error(format!("e{i}"));
        }
        assert_eq!(m.errors.len(), MAX_ERRORS);
        assert_eq!(m.errors.last().map(String::as_str), Some("e9"));
    }
}
