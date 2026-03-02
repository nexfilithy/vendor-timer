// src/main.rs
use eframe::{App, Frame, NativeOptions, egui};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use time::{Duration, OffsetDateTime};

fn main() -> eframe::Result<()> {
    let mut native = NativeOptions::default();
    native.viewport = native.viewport.with_always_on_top();

    // ADD THIS (choose any stable dir):
    native.persistence_path = Some(persistence_path());

    eframe::run_native(
        "Vendor Timers",
        native,
        Box::new(|cc| {
            let mut app = VendorApp::new(cc);

            if let Some(storage) = cc.storage {
                if let Some(persisted) = eframe::get_value::<Persisted>(storage, eframe::APP_KEY) {
                    app.persisted = persisted;

                    // re-init UI-only fields that are #[serde(skip)]
                    for v in &mut app.persisted.vendors {
                        v.draft_buy = String::new();
                        v.draft_like = String::new();
                    }
                }
            }

            Ok(Box::new(app))
        }),
    )
}

fn persistence_path() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("vendor_timers").join("eframe_state.ron")
}

const RELATIONSHIP: [&str; 11] = [
    "Despised",
    "Hated",
    "Disliked",
    "Tolerated",
    "Neutral (starting)",
    "Comfortable",
    "Friends",
    "Close Friends",
    "Best Friends",
    "Like Family",
    "Soul Mates",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Vendor {
    name: String,
    money: i64,
    buys: Vec<String>,
    likes: Vec<String>,

    /// 1..=11 (maps to RELATIONSHIP)
    favor_level: u8,

    /// UTC timestamp when the reset *would* occur.
    /// Display clamps at 0 when passed, and stays there until user presses "Reset".
    reset_at: Option<OffsetDateTime>,
    reset_period_minutes: Option<i64>,

    // UI-only drafts (not persisted)
    #[serde(skip)]
    draft_buy: String,
    #[serde(skip)]
    draft_like: String,
}

impl Default for Vendor {
    fn default() -> Self {
        Self {
            name: "New vendor".to_string(),
            money: 0,
            buys: vec!["weapons".into(), "armor".into()],
            likes: vec![],
            favor_level: 5, // Neutral
            reset_at: None,
            reset_period_minutes: None,
            draft_buy: String::new(),
            draft_like: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Persisted {
    default_reset_minutes: i64,
    vendors: Vec<Vendor>,
}

struct VendorApp {
    persisted: Persisted,
    path: PathBuf,

    // Add-vendor drafts
    draft_name: String,
    draft_money: i64,

    // Top calculator (UI only)
    calc_days: i64,
    calc_hours: i64,
    calc_minutes: i64,

    dirty: bool,
}

impl VendorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let path = default_save_path();
        let mut persisted = load(&path).unwrap_or(Persisted {
            default_reset_minutes: 7 * 24 * 60,
            vendors: vec![],
        });
        if persisted.default_reset_minutes <= 0 {
            persisted.default_reset_minutes = 7 * 24 * 60;
        }

        // init UI-only fields for loaded vendors
        for v in &mut persisted.vendors {
            v.draft_buy = String::new();
            v.draft_like = String::new();
            v.favor_level = v.favor_level.clamp(1, 11);
        }

        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        Self {
            persisted,
            path,
            draft_name: String::new(),
            draft_money: 0,
            calc_days: 0,
            calc_hours: 0,
            calc_minutes: 0,
            dirty: false,
        }
    }

    fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.persisted) {
            let _ = fs::create_dir_all(self.path.parent().unwrap());
            let _ = fs::write(&self.path, json);
            self.dirty = false;
        }
    }

    fn normalize_tag(s: &str) -> String {
        s.trim().to_string()
    }

    fn add_unique(vec: &mut Vec<String>, item: String) -> bool {
        let item = item.trim();
        if item.is_empty() {
            return false;
        }
        if vec.iter().any(|x| x.eq_ignore_ascii_case(item)) {
            return false;
        }
        vec.push(item.to_string());
        vec.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        true
    }

    fn remaining_clamped(reset_at: Option<OffsetDateTime>) -> Option<Duration> {
        let reset_at = reset_at?;
        let now = OffsetDateTime::now_utc();
        let d = reset_at - now;
        Some(if d.is_negative() { Duration::ZERO } else { d })
    }

    fn fmt_d_h_m(d: Duration) -> String {
        let total_minutes = d.whole_minutes().max(0);
        let days = total_minutes / (60 * 24);
        let hours = (total_minutes % (60 * 24)) / 60;
        let mins = total_minutes % 60;
        format!("{days}d {hours:02}h {mins:02}m")
    }

    fn favor_label(level: u8) -> &'static str {
        let idx = (level.clamp(1, 11) - 1) as usize;
        RELATIONSHIP[idx]
    }

    fn chip_list(ui: &mut egui::Ui, items: &mut Vec<String>, dirty: &mut bool) {
        // Wrapped row of "chips" with remove buttons.
        let mut remove: Option<usize> = None;

        ui.horizontal_wrapped(|ui| {
            for (idx, item) in items.iter().enumerate() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(item);
                        if ui.small_button("x").clicked() {
                            remove = Some(idx);
                        }
                    });
                });
            }
        });

        if let Some(idx) = remove {
            items.remove(idx);
            *dirty = true;
        }
    }

    fn effective_reset_minutes(default_reset_minutes: i64, v: &Vendor) -> i64 {
        v.reset_period_minutes
            .unwrap_or(default_reset_minutes)
            .max(1)
    }

    fn remaining_minutes_clamped(reset_at: Option<OffsetDateTime>) -> Option<i64> {
        let reset_at = reset_at?;
        let now = OffsetDateTime::now_utc();
        let d = reset_at - now;
        let mins = d.whole_minutes();
        Some(mins.max(0))
    }

    fn fmt_d_h_m_from_minutes(total_minutes: i64) -> String {
        let total_minutes = total_minutes.max(0);
        let days = total_minutes / (60 * 24);
        let hours = (total_minutes % (60 * 24)) / 60;
        let mins = total_minutes % 60;
        format!("{days}d {hours:02}h {mins:02}m")
    }
}

impl App for VendorApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Vendors");
                ui.separator();
                ui.label("Default reset (min):");
                if ui
                    .add(
                        egui::DragValue::new(&mut self.persisted.default_reset_minutes)
                            .clamp_range(1..=30 * 24 * 60)
                            .speed(10),
                    )
                    .changed()
                {
                    self.dirty = true;
                }
                ui.separator();

                ui.horizontal_wrapped(|ui| {
                    ui.label("Minutes calc:");

                    ui.label("d");
                    ui.add(
                        egui::DragValue::new(&mut self.calc_days)
                            .clamp_range(0..=365)
                            .speed(1),
                    );

                    ui.label("h");
                    ui.add(
                        egui::DragValue::new(&mut self.calc_hours)
                            .clamp_range(0..=23)
                            .speed(1),
                    );

                    ui.label("m");
                    ui.add(
                        egui::DragValue::new(&mut self.calc_minutes)
                            .clamp_range(0..=59)
                            .speed(1),
                    );

                    let total = self.calc_days * 24 * 60 + self.calc_hours * 60 + self.calc_minutes;

                    ui.separator();
                    ui.monospace(format!("= {total} minutes"));

                    if ui.button("Set default").clicked() {
                        self.persisted.default_reset_minutes = total.max(1);
                        self.dirty = true;
                    }
                });
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.dirty = true;
                    self.save_if_dirty();
                }
                if ui.button("Quit").clicked() {
                    self.dirty = true;
                    self.save_if_dirty();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(self.path.display().to_string());
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                egui::CollapsingHeader::new("Compact overview")
                    .default_open(true)
                    .show(ui, |ui| {
                        // Build a temporary list for display + sorting without borrowing vendors mutably.
                        let mut rows: Vec<(String, i64, Option<i64>)> = self
                            .persisted
                            .vendors
                            .iter()
                            .map(|v| {
                                (
                                    v.name.clone(),
                                    v.money,
                                    VendorApp::remaining_minutes_clamped(v.reset_at),
                                )
                            })
                            .collect();

                        // Sort: ready (0) first, then soonest, then "—" (None) last, then by name.
                        rows.sort_by(|a, b| match (a.2, b.2) {
                            (Some(ma), Some(mb)) => ma.cmp(&mb).then_with(|| a.0.cmp(&b.0)),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => a.0.cmp(&b.0),
                        });

                        // A very compact table-like list
                        egui::Grid::new("compact_overview_grid")
                            .num_columns(3)
                            .spacing([12.0, 2.0])
                            .striped(true)
                            .show(ui, |ui| {
                                ui.strong("Vendor");
                                ui.strong("Money");
                                ui.strong("Remaining");
                                ui.end_row();

                                for (name, money, mins_opt) in rows {
                                    ui.label(name);
                                    ui.label(money.to_string());
                                    match mins_opt {
                                        None => {
                                            ui.label("—");
                                        }
                                        Some(0) => {
                                            ui.colored_label(
                                                egui::Color32::LIGHT_RED,
                                                "0d 00h 00m",
                                            );
                                        }
                                        Some(m) => {
                                            ui.monospace(VendorApp::fmt_d_h_m_from_minutes(m));
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });

                ui.separator();
                // Legend (top). You can move to bottom by swapping panels.
                egui::CollapsingHeader::new("Relationship legend")
                    .default_open(false)
                    .show(ui, |ui| {
                        for (i, name) in RELATIONSHIP.iter().enumerate() {
                            ui.label(format!("{} = {}", i + 1, name));
                        }
                    });

                //ui.add_space(4.0);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::CollapsingHeader::new("Add vendor")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut self.draft_name);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Money");
                        ui.add(egui::DragValue::new(&mut self.draft_money).speed(10));
                    });

                    ui.horizontal(|ui| {
                        let add_enabled = !self.draft_name.trim().is_empty();
                        if ui
                            .add_enabled(add_enabled, egui::Button::new("Add"))
                            .clicked()
                        {
                            let v = Vendor {
                                name: self.draft_name.trim().to_string(),
                                money: self.draft_money,
                                ..Vendor::default()
                            };
                            self.persisted.vendors.push(v);
                            self.draft_name.clear();
                            self.draft_money = 0;
                            self.dirty = true;
                            self.save_if_dirty();
                        }

                        if ui.button("Clear all").clicked() {
                            self.persisted.vendors.clear();
                            self.dirty = true;
                            self.save_if_dirty();
                        }
                    });
                });

            ui.separator();

            let mut remove_idx: Option<usize> = None;

            egui::ScrollArea::vertical().show(ui, |ui| {
                let default_reset_minutes = self.persisted.default_reset_minutes;
                for (i, v) in self.persisted.vendors.iter_mut().enumerate() {
                    egui::Frame::group(ui.style())
                        .fill(ui.visuals().extreme_bg_color)
                        .show(ui, |ui| {
                            // Header
                            ui.horizontal(|ui| {
                                ui.label(format!("#{i}"));

                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut v.name)
                                            .desired_width(160.0),
                                    )
                                    .changed()
                                {
                                    self.dirty = true;
                                }

                                ui.separator();

                                ui.label("Money");
                                if ui
                                    .add(egui::DragValue::new(&mut v.money).speed(10))
                                    .changed()
                                {
                                    self.dirty = true;
                                }

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Remove").clicked() {
                                            remove_idx = Some(i);
                                        }
                                    },
                                );
                            });

                            // Timer line
                            ui.horizontal(|ui| {
                                ui.label("Reset in:");

                                if let Some(rem) = VendorApp::remaining_clamped(v.reset_at) {
                                    let text = VendorApp::fmt_d_h_m(rem);
                                    if rem == Duration::ZERO && v.reset_at.is_some() {
                                        ui.colored_label(
                                            egui::Color32::LIGHT_RED,
                                            format!("{text} (ready)"),
                                        );
                                    } else {
                                        ui.label(text);
                                    }
                                } else {
                                    ui.label("—");
                                }
                                ui.separator();
                                ui.label("Reset override (min):");

                                let mut override_enabled = v.reset_period_minutes.is_some();
                                if ui.checkbox(&mut override_enabled, "Use override").changed() {
                                    if override_enabled {
                                        v.reset_period_minutes =
                                            Some(self.persisted.default_reset_minutes.max(1));
                                    } else {
                                        v.reset_period_minutes = None;
                                    }
                                    self.dirty = true;
                                }

                                if let Some(m) = v.reset_period_minutes.as_mut() {
                                    if ui
                                        .add(
                                            egui::DragValue::new(m)
                                                .clamp_range(1..=30 * 24 * 60)
                                                .speed(10),
                                        )
                                        .changed()
                                    {
                                        self.dirty = true;
                                    }
                                }

                                ui.separator();

                                // Start sets the reset timestamp to now+7d (fixed)
                                if ui.button("Start").clicked() {
                                    let mins = VendorApp::effective_reset_minutes(
                                        default_reset_minutes,
                                        v,
                                    );
                                    v.reset_at =
                                        Some(OffsetDateTime::now_utc() + Duration::minutes(mins));
                                    self.dirty = true;
                                }

                                // Reset: sets a new 7-day window starting now (and you can press it
                                // after it hits 0 to “consume” the reset).
                                if ui.button("Reset").clicked() {
                                    let mins = VendorApp::effective_reset_minutes(
                                        default_reset_minutes,
                                        v,
                                    );
                                    v.reset_at =
                                        Some(OffsetDateTime::now_utc() + Duration::minutes(mins));
                                    self.dirty = true;
                                }

                                if ui.button("Clear").clicked() {
                                    v.reset_at = None;
                                    self.dirty = true;
                                }
                            });

                            ui.separator();

                            // Favor (1..11)
                            ui.horizontal(|ui| {
                                ui.label("Favor");
                                let label = VendorApp::favor_label(v.favor_level);
                                ui.label(format!("{} ({})", v.favor_level, label));

                                let old = v.favor_level;
                                ui.add(
                                    egui::Slider::new(&mut v.favor_level, 1..=11).show_value(false),
                                );
                                if v.favor_level != old {
                                    self.dirty = true;
                                }
                            });
                            ui.separator();

                            // Buys (dynamic)
                            ui.label("Buys:");
                            ui.horizontal(|ui| {
                                let enter = ui
                                    .add(
                                        egui::TextEdit::singleline(&mut v.draft_buy)
                                            .hint_text("add category (e.g. rings)"),
                                    )
                                    .lost_focus()
                                    && ui.input(|inp| inp.key_pressed(egui::Key::Enter));

                                if ui.button("Add").clicked() || enter {
                                    let item = VendorApp::normalize_tag(&v.draft_buy);
                                    if VendorApp::add_unique(&mut v.buys, item) {
                                        self.dirty = true;
                                    }
                                    v.draft_buy.clear();
                                }

                                if ui.button("Clear list").clicked() {
                                    if !v.buys.is_empty() {
                                        v.buys.clear();
                                        self.dirty = true;
                                    }
                                }
                            });

                            VendorApp::chip_list(ui, &mut v.buys, &mut self.dirty);
                            ui.separator();

                            // Likes (dynamic)
                            ui.label("Likes:");
                            ui.horizontal(|ui| {
                                let enter = ui
                                    .add(
                                        egui::TextEdit::singleline(&mut v.draft_like)
                                            .hint_text("add like (e.g. gemstones)"),
                                    )
                                    .lost_focus()
                                    && ui.input(|inp| inp.key_pressed(egui::Key::Enter));

                                if ui.button("Add").clicked() || enter {
                                    let item = VendorApp::normalize_tag(&v.draft_like);
                                    if VendorApp::add_unique(&mut v.likes, item) {
                                        self.dirty = true;
                                    }
                                    v.draft_like.clear();
                                }

                                if ui.button("Clear list").clicked() {
                                    if !v.likes.is_empty() {
                                        v.likes.clear();
                                        self.dirty = true;
                                    }
                                }
                            });

                            VendorApp::chip_list(ui, &mut v.likes, &mut self.dirty);
                        });

                    ui.add_space(8.0);
                }
            });

            if let Some(i) = remove_idx {
                self.persisted.vendors.remove(i);
                self.dirty = true;
            }

            self.save_if_dirty();
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.dirty = true;
        self.save_if_dirty();
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Save your own state too (optional); but for window size, eframe handles viewport.
        eframe::set_value(storage, eframe::APP_KEY, &self.persisted);
    }
}

fn default_save_path() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("vendor_timers").join("vendors.json")
}

fn load(path: &PathBuf) -> Option<Persisted> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}
