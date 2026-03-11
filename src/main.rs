// src/main.rs
use eframe::{App, Frame, NativeOptions, egui};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, mpsc},
};
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
                        v.favor_level = v.favor_level.clamp(1, 11);
                        v.max_money_buf = v.max_money.map(|x| x.to_string()).unwrap_or_default();
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
fn default_true() -> bool {
    true
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
    #[serde(default)]
    npc_key: Option<String>,
    money: i64,
    #[serde(default)]
    max_money: Option<i64>,
    buys: Vec<String>,
    likes: Vec<String>,

    /// 1..=11 (maps to RELATIONSHIP)
    favor_level: u8,

    /// UTC timestamp when the reset *would* occur.
    /// Display clamps at 0 when passed, and stays there until user presses "Reset".
    reset_at: Option<OffsetDateTime>,
    reset_period_minutes: Option<i64>,
    #[serde(default)]
    ready_refilled: bool,

    // UI-only drafts (not persisted)
    #[serde(skip)]
    draft_buy: String,
    #[serde(skip)]
    draft_like: String,
    #[serde(skip)]
    max_money_buf: String,
    #[serde(default = "default_true")]
    show_in_compact: bool,
}

impl Default for Vendor {
    fn default() -> Self {
        Self {
            name: "New vendor".to_string(),
            npc_key: None,
            money: 0,
            max_money: None,
            buys: vec!["weapons".into(), "armor".into()],
            likes: vec![],
            favor_level: 5, // Neutral
            reset_at: None,
            reset_period_minutes: None,
            ready_refilled: false,
            draft_buy: String::new(),
            draft_like: String::new(),
            max_money_buf: String::new(),
            show_in_compact: true,
        }
    }
}

#[derive(Debug, Clone)]
enum LogEvent {
    StartInteraction {
        npc_id: i64,
        npc_key: Option<String>,
    },
    VendorScreen {
        npc_id: i64,
        favor: Option<String>,
    },
    VendorGoldUpdate {
        remaining: i64,
        reset_at_ms: i64,
        cap: i64,
    },
}

struct WatcherHandle {
    stop: Arc<AtomicBool>,
    _join: std::thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Persisted {
    default_reset_minutes: i64,
    #[serde(default)]
    player_log_path: String,
    #[serde(default)]
    watch_player_log: bool,
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

    vendor_card_y: HashMap<usize, f32>,
    scroll_to_vendor: Option<usize>,

    dirty: bool,
    log_rx: mpsc::Receiver<LogEvent>,
    log_tx: mpsc::Sender<LogEvent>,
    watcher: Option<WatcherHandle>,

    // Used when updates don’t include npc_id:
    npc_id_to_key: HashMap<i64, String>,
    last_vendor_npc_id: Option<i64>,
}

impl VendorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let path = default_save_path();
        let mut persisted = load(&path).unwrap_or(Persisted {
            default_reset_minutes: 7 * 24 * 60,
            player_log_path: String::new(),
            watch_player_log: false,
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
            v.max_money_buf = v.max_money.map(|x| x.to_string()).unwrap_or_default();
        }
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (log_tx, log_rx) = mpsc::channel();

        Self {
            persisted,
            path,
            draft_name: String::new(),
            draft_money: 0,
            calc_days: 0,
            calc_hours: 0,
            calc_minutes: 0,
            vendor_card_y: HashMap::new(),
            scroll_to_vendor: None,
            dirty: false,
            log_rx,
            log_tx,
            watcher: None,
            npc_id_to_key: HashMap::new(),
            last_vendor_npc_id: None,
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
    fn stop_watcher(&mut self) {
        if let Some(w) = self.watcher.take() {
            w.stop.store(true, Ordering::Relaxed);
        }
    }

    fn ensure_watcher(&mut self) {
        if !self.persisted.watch_player_log {
            self.stop_watcher();
            return;
        }
        if self.watcher.is_some() {
            return;
        }

        let p = self.persisted.player_log_path.trim();
        let path = expand_user_path(p);
        if p.is_empty() || !path.exists() {
            return;
        }

        let path = PathBuf::from(p);
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let tx = self.log_tx.clone();

        let join = std::thread::spawn(move || {
            tail_player_log(path, stop2, tx);
        });

        self.watcher = Some(WatcherHandle { stop, _join: join });
    }

    fn strip_npc_prefix(key: &str) -> String {
        key.strip_prefix("NPC_").unwrap_or(key).to_string()
    }

    fn favor_str_to_level(s: &str) -> Option<u8> {
        Some(match s {
            "Despised" => 1,
            "Hated" => 2,
            "Disliked" => 3,
            "Tolerated" => 4,
            "Neutral" => 5,
            "Comfortable" => 6,
            "Friends" => 7,
            "CloseFriends" => 8,
            "BestFriends" => 9,
            "LikeFamily" => 10,
            "SoulMates" => 11,
            _ => return None,
        })
    }

    fn apply_log_event(&mut self, ev: LogEvent) {
        match ev {
            LogEvent::StartInteraction { npc_id, npc_key } => {
                if let Some(k) = npc_key {
                    self.npc_id_to_key.insert(npc_id, k);
                }
            }
            LogEvent::VendorScreen { npc_id, favor } => {
                self.last_vendor_npc_id = Some(npc_id);

                // Apply favor if we can resolve npc_key
                if let Some(npc_key) = self.npc_id_to_key.get(&npc_id).cloned() {
                    if let Some(favor) = favor {
                        if let Some(level) = Self::favor_str_to_level(&favor) {
                            self.upsert_vendor_by_key(&npc_key, |v| {
                                v.favor_level = level;
                            });
                        }
                    }
                }
            }
            LogEvent::VendorGoldUpdate {
                remaining,
                reset_at_ms,
                cap,
            } => {
                let Some(npc_id) = self.last_vendor_npc_id else {
                    return;
                };
                let Some(npc_key) = self.npc_id_to_key.get(&npc_id).cloned() else {
                    return;
                };

                let reset_at = if reset_at_ms > 0 {
                    OffsetDateTime::from_unix_timestamp(reset_at_ms / 1000).ok()
                } else {
                    None
                };

                self.upsert_vendor_by_key(&npc_key, |v| {
                    v.money = remaining;
                    v.max_money = Some(cap);
                    v.max_money_buf = cap.to_string();
                    if let Some(t) = reset_at {
                        v.reset_at = Some(t);
                        v.ready_refilled = false;
                    }
                });
            }
        }
    }

    fn upsert_vendor_by_key(&mut self, npc_key: &str, f: impl FnOnce(&mut Vendor)) {
        let found = self
            .persisted
            .vendors
            .iter_mut()
            .find(|v| v.npc_key.as_deref() == Some(npc_key));

        if let Some(v) = found {
            f(v);
            self.dirty = true;
            return;
        }

        // Not found => create new vendor (backward compat: old manual ones can be deleted)
        let mut v = Vendor::default();
        v.npc_key = Some(npc_key.to_string());
        v.name = Self::strip_npc_prefix(npc_key);
        f(&mut v);
        self.persisted.vendors.push(v);
        self.dirty = true;
    }
}

impl App for VendorApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        self.ensure_watcher();

        // Apply queued log events
        while let Ok(ev) = self.log_rx.try_recv() {
            self.apply_log_event(ev);
        }

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
            ui.horizontal_wrapped(|ui| {
                ui.label("Player.log:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.persisted.player_log_path)
                        .desired_width(360.0)
                        .hint_text(default_player_log_hint()),
                );

                if ui.button("Browse…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Player log", &["log", "txt"])
                        .pick_file()
                    {
                        self.persisted.player_log_path = path.display().to_string();
                        self.dirty = true;
                        self.stop_watcher();
                    }
                }
                if ui.button("Use default").clicked() {
                    if let Some(p) = default_player_log_path() {
                        self.persisted.player_log_path = p.display().to_string();
                        self.dirty = true;
                        self.stop_watcher();
                    }
                }

                if resp.changed() {
                    self.dirty = true;
                    self.stop_watcher();
                }

                if ui
                    .checkbox(&mut self.persisted.watch_player_log, "Watch")
                    .changed()
                {
                    self.dirty = true;
                    self.stop_watcher(); // toggle changes => restart logic next frame
                }

                let p = self.persisted.player_log_path.trim();
                let path = expand_user_path(p);
                let missing = p.is_empty() || !path.exists();
                if missing {
                    ui.colored_label(egui::Color32::LIGHT_RED, "path missing");
                }
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
                            .num_columns(4)
                            .spacing([12.0, 2.0])
                            .striped(true)
                            .show(ui, |ui| {
                                ui.strong("Vendor");
                                ui.strong("Current money");
                                ui.strong("Remaining");
                                ui.strong("");
                                ui.end_row();

                                let mut idxs: Vec<usize> = self
                                    .persisted
                                    .vendors
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(i, v)| v.show_in_compact.then_some(i))
                                    .collect();
                                idxs.sort_by(|&ia, &ib| {
                                    let a = &self.persisted.vendors[ia];
                                    let b = &self.persisted.vendors[ib];
                                    let ra = VendorApp::remaining_minutes_clamped(a.reset_at);
                                    let rb = VendorApp::remaining_minutes_clamped(b.reset_at);

                                    match (ra, rb) {
                                        (Some(ma), Some(mb)) => {
                                            ma.cmp(&mb).then_with(|| a.name.cmp(&b.name))
                                        }
                                        (Some(_), None) => std::cmp::Ordering::Less,
                                        (None, Some(_)) => std::cmp::Ordering::Greater,
                                        (None, None) => a.name.cmp(&b.name),
                                    }
                                });

                                for i in idxs {
                                    let v = &mut self.persisted.vendors[i];

                                    if ui.link(v.name.clone()).clicked() {
                                        self.scroll_to_vendor = Some(i);
                                    }

                                    // interactive current money
                                    if ui
                                        .add(egui::DragValue::new(&mut v.money).speed(10))
                                        .changed()
                                    {
                                        self.dirty = true;
                                    }

                                    // remaining
                                    match VendorApp::remaining_minutes_clamped(v.reset_at) {
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
                                    let mins = VendorApp::effective_reset_minutes(
                                        self.persisted.default_reset_minutes,
                                        v,
                                    );

                                    // Only show Reset button when timer is ready (0) OR missing (—), adjust if you want
                                    let show_reset = matches!(
                                        VendorApp::remaining_minutes_clamped(v.reset_at),
                                        Some(0) | None
                                    );

                                    if show_reset && ui.small_button("Reset").clicked() {
                                        v.reset_at = Some(
                                            OffsetDateTime::now_utc() + Duration::minutes(mins),
                                        );
                                        v.ready_refilled = false;

                                        if let Some(max) = v.max_money {
                                            v.money = max;
                                        }

                                        self.dirty = true;
                                    } else {
                                        // keep grid alignment when button not shown
                                        ui.label("");
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
                        ui.label("Current money");
                        if ui
                            .add(egui::DragValue::new(&mut self.draft_money).speed(10))
                            .changed()
                        {
                            self.dirty = true;
                        }
                    });

                    ui.horizontal(|ui| {
                        let add_enabled = !self.draft_name.trim().is_empty();
                        if ui
                            .add_enabled(add_enabled, egui::Button::new("Add"))
                            .clicked()
                        {
                            let money = self.draft_money;
                            let v = Vendor {
                                name: self.draft_name.trim().to_string(),
                                money: money,
                                max_money: Some(money),
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
                self.vendor_card_y.clear();

                for (i, v) in self.persisted.vendors.iter_mut().enumerate() {
                    // Auto-refill when the timer reaches 0 (do it once)
                    if let Some(reset_at) = v.reset_at {
                        let now = OffsetDateTime::now_utc();
                        if now >= reset_at && !v.ready_refilled {
                            if let Some(max) = v.max_money {
                                v.money = max; // refill current money
                            }
                            v.ready_refilled = true;
                            self.dirty = true;
                        }
                    }

                    let y = ui.cursor().min.y;
                    self.vendor_card_y.insert(i, y);

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
                                ui.separator();
                                ui.label("Max money");
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut v.max_money_buf)
                                        .desired_width(70.0)
                                        .hint_text("blank = ?"),
                                );

                                // If user edited the field, sync it into Option<i64>
                                if resp.changed() {
                                    let s = v.max_money_buf.trim();

                                    if s.is_empty() {
                                        v.max_money = None;
                                        self.dirty = true;
                                    } else if let Ok(val) = s.parse::<i64>() {
                                        v.max_money = Some(val);
                                        self.dirty = true;
                                    } else {
                                        // invalid input: do nothing (keeps last valid max_money)
                                        // (Optional: show red warning)
                                    }
                                }
                                let s = v.max_money_buf.trim();
                                if !s.is_empty() && s.parse::<i64>().is_err() {
                                    ui.colored_label(egui::Color32::LIGHT_RED, "number pls");
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
                                    v.ready_refilled = false;
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
                                    v.ready_refilled = false;
                                    self.dirty = true;
                                }

                                if ui.button("Clear").clicked() {
                                    v.reset_at = None;
                                    self.dirty = true;
                                }

                                if ui.button("Refill money").clicked() {
                                    if let Some(max) = v.max_money {
                                        v.money = max;
                                    }
                                }
                                ui.separator();
                                if ui
                                    .checkbox(&mut v.show_in_compact, "Compact view")
                                    .changed()
                                {
                                    self.dirty = true;
                                }
                            });

                            ui.separator();

                            // Favor (1..11)
                            ui.horizontal(|ui| {
                                ui.label("Favor");
                                let text = format!(
                                    "{} ({})",
                                    v.favor_level,
                                    VendorApp::favor_label(v.favor_level)
                                );
                                ui.add_sized([220.0, 0.0], egui::Label::new(text));

                                if ui
                                    .add(
                                        egui::Slider::new(&mut v.favor_level, 1..=11)
                                            .show_value(false),
                                    )
                                    .changed()
                                {
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
                if let Some(target) = self.scroll_to_vendor.take() {
                    if let Some(&y) = self.vendor_card_y.get(&target) {
                        // Create a 1px-tall rect at that y and ask egui to scroll it into view.
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(ui.cursor().min.x, y),
                            egui::vec2(1.0, 1.0),
                        );
                        ui.scroll_to_rect(rect, Some(egui::Align::TOP));
                    } else {
                        // If we don't know y yet (first frame), try again next frame:
                        self.scroll_to_vendor = Some(target);
                    }
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

fn tail_player_log(path: PathBuf, stop: Arc<AtomicBool>, tx: mpsc::Sender<LogEvent>) {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut offset: u64 = 0;
    if let Ok(meta) = std::fs::metadata(&path) {
        offset = meta.len(); // start at end: only new events
    }

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(1000));

        let Ok(mut f) = File::open(&path) else {
            continue;
        };
        let Ok(meta) = f.metadata() else {
            continue;
        };
        let len = meta.len();

        if len < offset {
            offset = 0; // truncated/rotated
        }
        if len == offset {
            continue;
        }

        if f.seek(SeekFrom::Start(offset)).is_err() {
            continue;
        }

        let mut buf = Vec::with_capacity((len - offset) as usize);
        if f.read_to_end(&mut buf).is_err() {
            continue;
        }
        offset = len;

        let text = String::from_utf8_lossy(&buf);
        for line in text.lines() {
            if let Some(ev) = parse_log_line(line) {
                let _ = tx.send(ev);
            }
        }
    }
}

fn parse_log_line(line: &str) -> Option<LogEvent> {
    if line.contains("LocalPlayer: ProcessStartInteraction(") {
        return parse_start_interaction(line);
    }
    if line.contains("LocalPlayer: ProcessVendorScreen(") {
        return parse_vendor_screen(line);
    }
    if line.contains("LocalPlayer: ProcessVendorUpdateAvailableGold(") {
        return parse_vendor_update_gold(line);
    }
    None
}

fn parse_start_interaction(line: &str) -> Option<LogEvent> {
    let needle = "LocalPlayer: ProcessStartInteraction(";
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find(')')?;
    let inside = &rest[..end];

    let mut parts = inside.split(',').map(|s| s.trim());
    let npc_id: i64 = parts.next()?.parse().ok()?;

    // quoted last arg: "NPC_Raina" or ""
    let npc_key = inside
        .split('"')
        .nth(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some(LogEvent::StartInteraction { npc_id, npc_key })
}

fn parse_vendor_screen(line: &str) -> Option<LogEvent> {
    let needle = "LocalPlayer: ProcessVendorScreen(";
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find(')')?;
    let inside = &rest[..end];

    // first two tokens: npc_id, FavorWord
    let mut parts = inside.split(',').map(|s| s.trim());
    let npc_id: i64 = parts.next()?.parse().ok()?;
    let favor = parts.next().map(|s| s.to_string());

    Some(LogEvent::VendorScreen { npc_id, favor })
}

fn parse_vendor_update_gold(line: &str) -> Option<LogEvent> {
    let needle = "LocalPlayer: ProcessVendorUpdateAvailableGold(";
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find(')')?;
    let inside = &rest[..end];

    let mut parts = inside.split(',').map(|s| s.trim());
    let remaining: i64 = parts.next()?.parse().ok()?;
    let reset_at_ms: i64 = parts.next()?.parse().ok()?;
    let cap: i64 = parts.next()?.parse().ok()?;

    Some(LogEvent::VendorGoldUpdate {
        remaining,
        reset_at_ms,
        cap,
    })
}
fn expand_user_path(s: &str) -> PathBuf {
    let s = s.trim();
    if s.is_empty() {
        return PathBuf::new();
    }

    // Expand "~" and "~/" on unix-ish systems (also nice on Windows if user types it)
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }

    // Convenience: ".config/..." => "$HOME/.config/..." (Unix only)
    #[cfg(unix)]
    {
        if let Some(rest) = s.strip_prefix(".config/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(".config").join(rest);
            }
        }
    }

    PathBuf::from(s)
}

fn default_player_log_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        // %USERPROFILE%\AppData\LocalLow\Elder Game\Project Gorgon\Player.log
        let base = dirs::home_dir()?;
        Some(
            base.join("AppData")
                .join("LocalLow")
                .join("Elder Game")
                .join("Project Gorgon")
                .join("Player.log"),
        )
    }

    #[cfg(unix)]
    {
        // ~/.config/unity3d/Elder Game/Project Gorgon/Player.log
        let home = dirs::home_dir()?;
        Some(
            home.join(".config")
                .join("unity3d")
                .join("Elder Game")
                .join("Project Gorgon")
                .join("Player.log"),
        )
    }
}

fn default_player_log_hint() -> String {
    default_player_log_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Path to Player.log".to_string())
}

fn default_save_path() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("vendor_timers").join("vendors.json")
}

fn load(path: &PathBuf) -> Option<Persisted> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}
