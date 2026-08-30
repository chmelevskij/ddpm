use ddc_hi::{Ddc, Display};
use eframe::egui;
use std::sync::{Arc, Mutex};

const INPUT_SOURCES: &[(u16, &str)] = &[
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
];

fn input_source_label(value: u16) -> &'static str {
    INPUT_SOURCES
        .iter()
        .find(|(v, _)| *v == value)
        .map(|(_, label)| *label)
        .unwrap_or("Unknown")
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DDPM - Monitor Control",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

struct MonitorState {
    name: String,
    brightness: u16,
    contrast: u16,
    max_brightness: u16,
    max_contrast: u16,
    input_source: u16,
}

struct App {
    displays: Arc<Mutex<Vec<Display>>>,
    monitors: Vec<MonitorState>,
    error: Option<String>,
}

impl App {
    fn new() -> Self {
        let mut displays_vec = Display::enumerate();
        let mut monitors = Vec::new();
        let mut error = None;

        for display in &mut displays_vec {
            let name = display
                .info
                .model_name
                .clone()
                .unwrap_or_else(|| "Unknown".into());

            let brightness = match display.handle.get_vcp_feature(0x10) {
                Ok(v) => (v.value(), v.maximum()),
                Err(e) => {
                    error = Some(format!("Failed to read brightness for {name}: {e}"));
                    (50, 100)
                }
            };

            let contrast = match display.handle.get_vcp_feature(0x12) {
                Ok(v) => (v.value(), v.maximum()),
                Err(e) => {
                    error = Some(format!("Failed to read contrast for {name}: {e}"));
                    (50, 100)
                }
            };

            // VCP 0x60 = Input Source Select
            let input_source = match display.handle.get_vcp_feature(0x60) {
                Ok(v) => v.value(),
                Err(_) => 0,
            };

            monitors.push(MonitorState {
                name,
                brightness: brightness.0,
                contrast: contrast.0,
                max_brightness: brightness.1,
                max_contrast: contrast.1,
                input_source,
            });
        }

        if displays_vec.is_empty() {
            error = Some(
                "No DDC/CI capable monitors found. Check i2c-dev module and permissions.".into(),
            );
        }

        Self {
            displays: Arc::new(Mutex::new(displays_vec)),
            monitors,
            error,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("DDPM - Monitor Control");
            ui.separator();

            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::from_rgb(255, 100, 100), err);
                ui.separator();
            }

            let mut displays = self.displays.lock().unwrap();

            for (i, monitor) in self.monitors.iter_mut().enumerate() {
                ui.group(|ui| {
                    ui.label(egui::RichText::new(&monitor.name).strong().size(16.0));
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Brightness:");
                        let slider = egui::Slider::new(
                            &mut monitor.brightness,
                            0..=monitor.max_brightness,
                        )
                        .suffix("%");
                        if ui.add(slider).changed() {
                            if let Some(display) = displays.get_mut(i) {
                                let _ =
                                    display.handle.set_vcp_feature(0x10, monitor.brightness);
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Contrast:   ");
                        let slider = egui::Slider::new(
                            &mut monitor.contrast,
                            0..=monitor.max_contrast,
                        )
                        .suffix("%");
                        if ui.add(slider).changed() {
                            if let Some(display) = displays.get_mut(i) {
                                let _ =
                                    display.handle.set_vcp_feature(0x12, monitor.contrast);
                            }
                        }
                    });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Input:       ");
                        let current_label = input_source_label(monitor.input_source);
                        egui::ComboBox::from_id_salt(format!("input_{i}"))
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                for &(value, label) in INPUT_SOURCES {
                                    if ui
                                        .selectable_value(
                                            &mut monitor.input_source,
                                            value,
                                            label,
                                        )
                                        .changed()
                                    {
                                        if let Some(display) = displays.get_mut(i) {
                                            let _ = display
                                                .handle
                                                .set_vcp_feature(0x60, monitor.input_source);
                                        }
                                    }
                                }
                            });
                    });
                });

                ui.add_space(8.0);
            }
        });
    }
}
