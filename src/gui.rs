//! Native window interface.
//!
//! Same information as the terminal UI, in a real macOS window. The engine,
//! lookup, persistence and alerting layers are untouched — this module only
//! replaces the presentation, talking to the same [`Stats`], [`Control`] and
//! event channel the TUI uses.
//!
//! A terminal UI needs a terminal: the window is its drawing surface. This one
//! draws its own, so the app launches straight from the Finder.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, FontId, Layout, Pos2, RichText, Rounding, Sense, Stroke, TextureHandle,
    Ui, Vec2,
};

use crate::config::physical_cores;
use crate::engine::Event;
use crate::hits::Hit;
use crate::startup::Progress;
use crate::stats::{Control, Priority, Rate, Stats};
use crate::tui::{expected_seeds_to_hit, universe_ages_to_hit, HANDLE, HANDLE_URL};
use crate::util;

const BG: Color32 = Color32::from_rgb(13, 15, 22);
const PANEL: Color32 = Color32::from_rgb(24, 28, 41);
const FRAME: Color32 = Color32::from_rgb(37, 43, 61);
const PRIMARY: Color32 = Color32::from_rgb(125, 207, 255);
const ACCENT: Color32 = Color32::from_rgb(187, 154, 247);
const TEXT: Color32 = Color32::from_rgb(192, 202, 245);
const DIM: Color32 = Color32::from_rgb(97, 108, 152);
const MUTED: Color32 = Color32::from_rgb(58, 66, 96);
const ALERT: Color32 = Color32::from_rgb(247, 118, 142);
const WARN: Color32 = Color32::from_rgb(224, 175, 104);
const GOLD: Color32 = Color32::from_rgb(232, 176, 84);

/// How long the intro is shown before the dashboard takes over.
const INTRO: Duration = Duration::from_millis(1900);

pub struct GuiApp {
    stats: Arc<Stats>,
    control: Arc<Control>,
    events: Receiver<Event>,
    hits: Vec<Hit>,
    selected: Option<usize>,
    rate: Rate,
    peak: f64,
    last_sample: Instant,
    started: Instant,
    funded_count: u64,
    addresses_per_seed: u32,
    entropy_bits: u32,
    threads: usize,
    bloom_bytes: usize,
    db_bytes: usize,
    errors: Vec<String>,
    logo: Option<TextureHandle>,
    /// Set once the screenshot mode has captured its frame.
    shot_path: Option<std::path::PathBuf>,
    shot_at: Option<Instant>,
    /// Set while loading runs behind the window; cleared when the search starts.
    loading: Option<Arc<Progress>>,
    /// Populated by the loader thread when loading fails.
    load_error: Option<String>,
    settings_open: bool,
    /// Expert controls stay locked until the warning has been acknowledged.
    expert_unlocked: bool,
    expert_prompt: bool,
}

impl GuiApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        stats: Arc<Stats>,
        control: Arc<Control>,
        events: Receiver<Event>,
        existing: Vec<Hit>,
        funded_count: u64,
        addresses_per_seed: u32,
        entropy_bits: u32,
        threads: usize,
        bloom_bytes: usize,
        db_bytes: usize,
        shot_path: Option<std::path::PathBuf>,
        loading: Option<Arc<Progress>>,
    ) -> GuiApp {
        GuiApp {
            stats,
            control,
            events,
            hits: existing,
            selected: None,
            rate: Rate::new(160),
            peak: 0.0,
            last_sample: Instant::now(),
            started: Instant::now(),
            funded_count,
            addresses_per_seed,
            entropy_bits,
            threads,
            bloom_bytes,
            db_bytes,
            errors: Vec::new(),
            logo: None,
            shot_path,
            shot_at: None,
            loading,
            load_error: None,
            settings_open: std::env::var("SC_SHOT_SETTINGS").is_ok(),
            expert_unlocked: std::env::var("SC_SHOT_SETTINGS").is_ok(),
            expert_prompt: false,
        }
    }

    /// Throughput as a fraction of what this machine delivers at full tilt.
    ///
    /// Interpolated from measurements on an M1 (4 performance + 4 efficiency
    /// cores) rather than assumed linear, because it is emphatically not:
    ///
    /// * the four efficiency cores contribute far less than the performance
    ///   ones, so the curve flattens after four threads; and
    /// * at background priority macOS confines the work to the efficiency
    ///   cores, so **eight threads are slower than four** — 509/s against
    ///   637/s measured. A linear estimate would have promised the opposite.
    fn expected_share(&self, threads: usize, priority: Priority) -> f64 {
        // (threads, share of this machine's peak), measured on an idle M1.
        // Not assumed linear, because it is nothing of the sort:
        //
        // * Utility priority plateaus near 42% however many cores are given to
        //   it — macOS keeps that tier off the performance cores.
        // * Background peaks at six cores and gets *worse* at eight, since the
        //   work is confined to four efficiency cores that then contend.
        // * Only Normal scales the way one would naively expect.
        const BACKGROUND: [(f64, f64); 5] = [
            (1.0, 0.057),
            (2.0, 0.096),
            (4.0, 0.153),
            (6.0, 0.171),
            (8.0, 0.152),
        ];
        const UTILITY: [(f64, f64); 5] = [
            (1.0, 0.181),
            (2.0, 0.305),
            (4.0, 0.362),
            (6.0, 0.400),
            (8.0, 0.419),
        ];
        const NORMAL: [(f64, f64); 5] = [
            (1.0, 0.181),
            (2.0, 0.362),
            (4.0, 0.725),
            (6.0, 0.886),
            (8.0, 1.000),
        ];

        let curve: &[(f64, f64)] = match priority {
            Priority::Background => &BACKGROUND,
            Priority::Utility => &UTILITY,
            Priority::Normal => &NORMAL,
        };

        // Rescale to this machine's core count so the curve still means
        // something on hardware that is not an eight-core M1.
        let max = physical_cores().max(1) as f64;
        let t = (threads.max(1) as f64) * 8.0 / max;

        if t <= curve[0].0 {
            return curve[0].1;
        }
        for w in curve.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            if t <= x1 {
                return y0 + (y1 - y0) * (t - x0) / (x1 - x0);
            }
        }
        curve[curve.len() - 1].1
    }

    /// True when the chosen combination is self-defeating.
    fn is_counterproductive(&self, threads: usize, priority: Priority) -> bool {
        priority == Priority::Background && threads > physical_cores() * 3 / 4
    }

    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(Event::Hit(h)) => {
                    self.hits.push(*h);
                    self.selected = Some(self.hits.len() - 1);
                }
                Ok(Event::PersistFailure { hit, error }) => {
                    self.errors.push(format!(
                        "Treffer konnte nicht gespeichert werden ({}): {error}",
                        hit.address
                    ));
                    self.hits.push(*hit);
                }
                Ok(Event::BackupFailure { id, error }) => {
                    self.errors
                        .push(format!("Sicherungskopie fehlgeschlagen ({id}): {error}"));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// The full-colour app icon, for the header and the intro.
    fn logo_texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        self.logo
            .get_or_insert_with(|| {
                let px: Vec<Color32> = crate::icon_data::ICON_RGBA
                    .chunks_exact(4)
                    .map(|c| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                    .collect();
                let img = egui::ColorImage {
                    size: [
                        crate::icon_data::ICON_W as usize,
                        crate::icon_data::ICON_H as usize,
                    ],
                    pixels: px,
                };
                ctx.load_texture("mark", img, egui::TextureOptions::LINEAR)
            })
            .clone()
    }
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// The handle, as a link to the profile.
///
/// Underlined on hover rather than always, so it reads as a signature until it
/// is pointed at and as a link the moment it might be clicked.
fn handle_link(ui: &mut Ui, size: f32) {
    let resp = ui.add(
        egui::Label::new(RichText::new(HANDLE).color(ACCENT).size(size)).sense(Sense::click()),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let r = resp.rect;
        ui.painter().line_segment(
            [
                Pos2::new(r.left(), r.bottom() - 1.0),
                Pos2::new(r.right(), r.bottom() - 1.0),
            ],
            Stroke::new(1.0_f32, ACCENT),
        );
    }
    if resp.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(HANDLE_URL));
    }
    resp.on_hover_text(HANDLE_URL);
}

fn card(ui: &mut Ui, title: &str, accent: Color32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::none()
        .fill(PANEL)
        .rounding(Rounding::same(10.0))
        .stroke(Stroke::new(1.0_f32, FRAME))
        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
        .show(ui, |ui| {
            // Force top-down layout and full width: a Frame inherits the
            // parent's direction, so inside a horizontal row the contents
            // would otherwise be laid out side by side.
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title).color(accent).size(11.0).strong());
                ui.add_space(8.0);
                add(ui);
            });
        });
}

/// Headline number plus the plain-language caption that says what it is.
fn hero(ui: &mut Ui, value: &str, caption: &str, color: Color32) {
    ui.label(RichText::new(value).color(color).font(mono(27.0)).strong());
    ui.label(RichText::new(caption).color(DIM).size(12.0));
    ui.add_space(10.0);
}

/// Detail row: the expert figures live here.
fn kv(ui: &mut Ui, k: &str, v: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).color(DIM).size(12.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(v).color(color).font(mono(12.0)));
        });
    });
}

fn sci(x: f64) -> String {
    if !x.is_finite() {
        return "unendlich".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    format!("{x:.4e}").replace('.', ",")
}

fn de(x: f64, places: usize) -> String {
    format!("{x:.places$}", places = places).replace('.', ",")
}

fn thousands(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut parts = Vec::new();
    while n > 0 {
        parts.push(format!("{:03}", n % 1000));
        n /= 1000;
    }
    parts.reverse();
    let mut s = parts.join(" ");
    while s.starts_with('0') && s.len() > 1 && !s.starts_with("0 ") {
        s.remove(0);
    }
    s
}

/// German long-scale name for a magnitude. See the TUI's equivalent for why
/// the headline is given in words before figures.
fn german_scale(x: f64) -> String {
    if !x.is_finite() {
        return "unendlich".into();
    }
    if x < 1_000_000.0 {
        return thousands(x.max(0.0) as u64);
    }
    const NAMES: [(f64, &str, &str); 10] = [
        (1e6, "Million", "Millionen"),
        (1e9, "Milliarde", "Milliarden"),
        (1e12, "Billion", "Billionen"),
        (1e15, "Billiarde", "Billiarden"),
        (1e18, "Trillion", "Trillionen"),
        (1e21, "Trilliarde", "Trilliarden"),
        (1e24, "Quadrillion", "Quadrillionen"),
        (1e27, "Quadrilliarde", "Quadrilliarden"),
        (1e30, "Quintillion", "Quintillionen"),
        (1e33, "Quintilliarde", "Quintilliarden"),
    ];
    if x >= NAMES[NAMES.len() - 1].0 * 1000.0 {
        return format!("10 hoch {:.0}", x.log10());
    }
    let mut chosen = NAMES[0];
    for e in NAMES {
        if x >= e.0 {
            chosen = e;
        }
    }
    let m = x / chosen.0;
    let word = if (m - 1.0).abs() < 0.05 {
        chosen.1
    } else {
        chosen.2
    };
    format!("{} {}", de(m, 1), word)
}

impl eframe::App for GuiApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [13.0 / 255.0, 15.0 / 255.0, 22.0 / 255.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // An external stop (--duration, a signal) must close the window too,
        // or it sits there with frozen counters and no way to tell that apart
        // from a stall.
        if self.control.stopping() && self.shot_path.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Loading runs on another thread; absorb its results the first frame
        // after it finishes.
        if let Some(p) = self.loading.clone() {
            if p.is_done() {
                match p.error() {
                    Some(e) => self.load_error = Some(e),
                    None => {
                        self.funded_count = p.funded();
                        self.bloom_bytes = p.bloom_bytes();
                        self.db_bytes = p.db_bytes();
                    }
                }
                self.loading = None;
                self.started = Instant::now();
            }
        }

        if let Some(err) = self.load_error.clone() {
            draw_error_panel(ctx, &err);
            return;
        }

        self.drain();

        if self.last_sample.elapsed() >= Duration::from_millis(250) && !self.control.paused() {
            let inst = self.rate.sample(self.stats.seeds());
            if inst > self.peak {
                self.peak = inst;
            }
            self.last_sample = Instant::now();
        }

        // Repaint on a timer rather than continuously: the numbers change a few
        // times a second and the cores are needed elsewhere.
        ctx.request_repaint_after(Duration::from_millis(120));

        // Start from the dark theme rather than patching individual colours:
        // window chrome (title bars, scrollbars, widget backgrounds) is drawn
        // from the widget palette, and overriding only `window_fill` left the
        // settings window with a light title bar.
        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = PANEL;
        style.visuals.window_stroke = Stroke::new(1.0_f32, FRAME);
        // The slider rail is drawn with `extreme_bg_color`; it has to differ
        // from the surrounding panel or the track vanishes.
        style.visuals.extreme_bg_color = Color32::from_rgb(11, 13, 19);
        style.visuals.faint_bg_color = Color32::from_rgb(30, 35, 50);
        style.visuals.override_text_color = Some(TEXT);
        for w in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            // `bg_fill` is the slider rail and checkbox body; it must differ
            // from the surrounding panel or those controls become invisible.
            // `weak_bg_fill` is the button surface, which buttons override
            // explicitly anyway.
            w.bg_fill = Color32::from_rgb(42, 49, 69);
            w.weak_bg_fill = PANEL;
            w.bg_stroke = Stroke::new(1.0_f32, FRAME);
        }
        style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(34, 40, 56);
        ctx.set_style(style);

        // The loading screen stands in for the dashboard until the data is
        // there, and the intro plays for a moment afterwards so the transition
        // is not an abrupt swap.
        let loading = self.loading.is_some();

        if self.shot_path.is_some() && std::env::var("SC_SHOT_LOADING").is_err() {
            self.draw_dashboard(ctx);
        } else if loading {
            self.draw_loading(ctx);
        } else if self.started.elapsed() < INTRO {
            self.draw_intro(ctx);
        } else {
            self.draw_dashboard(ctx);
        }

        self.draw_settings(ctx);
        self.handle_screenshot(ctx);
    }
}

impl GuiApp {
    fn draw_intro(&mut self, ctx: &egui::Context) {
        let t = self.started.elapsed().as_secs_f32();
        let tex = self.logo_texture(ctx);
        let funded = thousands(self.funded_count);
        let threads = self.threads;

        let alpha = |start: f32| ((t - start) / 0.42).clamp(0.0, 1.0);
        let tint = |c: Color32, a: f32| {
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8)
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let h = ui.available_height();
                    ui.add_space((h * 0.22).max(20.0));

                    ui.add(
                        egui::Image::new(&tex)
                            .fit_to_exact_size(Vec2::new(210.0, 210.0))
                            .tint(Color32::from_white_alpha((alpha(0.0) * 255.0) as u8)),
                    );

                    ui.add_space(22.0);
                    ui.label(
                        RichText::new("S C H A T Z S U C H E")
                            .color(tint(TEXT, alpha(0.25)))
                            .size(22.0)
                            .strong(),
                    );
                    ui.add_space(14.0);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(300.0, 1.0), Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 0.0, tint(FRAME, alpha(0.45)));
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(format!(
                            "{funded} Adressen geladen  ·  {threads} Kerne bereit"
                        ))
                        .color(tint(DIM, alpha(0.65)))
                        .size(13.0),
                    );
                    ui.add_space(16.0);
                    // Faded in with everything else, but clickable throughout.
                    if alpha(0.95) > 0.9 {
                        handle_link(ui, 14.0);
                    } else {
                        ui.label(
                            RichText::new(HANDLE)
                                .color(tint(ACCENT, alpha(0.95)))
                                .size(14.0)
                                .strong(),
                        );
                    }
                });
            });
    }

    fn draw_dashboard(&mut self, ctx: &egui::Context) {
        let seeds = self.stats.seeds();
        let addresses = self.stats.addresses();
        let now_rate = self.rate.history().last().copied().unwrap_or(0) as f64;
        let avg = self.rate.average();
        let paused = self.control.paused();
        let tex = self.logo_texture(ctx);

        egui::TopBottomPanel::top("head")
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(16.0, 12.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // The mark sits top-left at a size where it is actually
                    // legible, not as a decorative speck.
                    ui.add(
                        egui::Image::new(&tex)
                            .fit_to_exact_size(Vec2::new(46.0, 46.0)),
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new("SCHATZSUCHE").color(ACCENT).size(19.0).strong());
                    ui.add_space(14.0);
                    let dot_col = if paused { WARN } else { PRIMARY };
                    let (dot, _) = ui.allocate_exact_size(Vec2::new(9.0, 9.0), Sense::hover());
                    if paused {
                        ui.painter()
                            .circle_stroke(dot.center(), 4.0, Stroke::new(1.5_f32, dot_col));
                    } else {
                        ui.painter().circle_filled(dot.center(), 4.0, dot_col);
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(if paused { "ANGEHALTEN" } else { "LÄUFT" })
                            .color(dot_col)
                            .size(12.0),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (label, col) = if paused {
                            ("START", PRIMARY)
                        } else {
                            ("STOPP", WARN)
                        };
                        let btn = egui::Button::new(
                            RichText::new(label).color(Color32::BLACK).size(13.0).strong(),
                        )
                        .fill(col)
                        .rounding(Rounding::same(7.0))
                        .min_size(Vec2::new(112.0, 30.0));
                        if ui.add(btn).clicked() {
                            self.control.toggle_paused();
                        }
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Einstellungen").color(TEXT).size(12.5),
                                )
                                .fill(PANEL)
                                .stroke(Stroke::new(1.0_f32, FRAME))
                                .rounding(Rounding::same(7.0))
                                .min_size(Vec2::new(112.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.settings_open = !self.settings_open;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(if paused {
                        "Angehalten. Die Prozessorkerne schlafen — es wird gerade kein Strom verbraucht."
                    } else {
                        "Würfelt zufällige Bitcoin-Wallets und prüft, ob eine davon Guthaben besitzt."
                    })
                    .color(DIM)
                    .size(12.0),
                );
            });

        egui::TopBottomPanel::bottom("foot")
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Treffer anklicken zeigt den Seed")
                            .color(MUTED)
                            .size(11.0),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        handle_link(ui, 12.0);
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(16.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.columns(3, |c| {
                    card(&mut c[0], "TEMPO", PRIMARY, |ui| {
                        hero(
                            ui,
                            &thousands(now_rate as u64),
                            "Wallets pro Sekunde",
                            PRIMARY,
                        );
                        kv(
                            ui,
                            "Durchschnitt",
                            &format!("{} /s", thousands(avg as u64)),
                            TEXT,
                        );
                        kv(
                            ui,
                            "Spitze",
                            &format!("{} /s", thousands(self.peak as u64)),
                            TEXT,
                        );
                        kv(
                            ui,
                            "Pro Kern",
                            &format!(
                                "{} /s ×{}",
                                thousands((avg / self.threads.max(1) as f64) as u64),
                                self.threads
                            ),
                            DIM,
                        );
                    });
                    card(&mut c[1], "GEPRÜFT", PRIMARY, |ui| {
                        hero(ui, &thousands(seeds), "Wallets seit dem Start", TEXT);
                        kv(ui, "Adressen", &thousands(addresses), TEXT);
                        kv(
                            ui,
                            "Laufzeit",
                            &util::format_duration(self.rate.elapsed().as_secs()),
                            TEXT,
                        );
                        kv(ui, "Fehlalarme", &thousands(self.stats.bloom_hits()), DIM);
                    });
                    card(&mut c[2], "SUCHRAUM", PRIMARY, |ui| {
                        let frac = seeds as f64 / 2f64.powi(self.entropy_bits as i32) * 100.0;
                        hero(
                            ui,
                            &format!("{} %", sci(frac)),
                            "des Suchraums abgesucht",
                            WARN,
                        );
                        kv(ui, "Suchraum", &format!("2^{}", self.entropy_bits), DIM);
                        kv(ui, "Datenbank", &thousands(self.funded_count), DIM);
                        kv(
                            ui,
                            "Speicher",
                            &format!(
                                "{:.0} MB + {:.0} MB",
                                self.bloom_bytes as f64 / 1e6,
                                self.db_bytes as f64 / 1e6
                            ),
                            DIM,
                        );
                    });
                });

                ui.add_space(10.0);
                self.draw_sparkline(ui);
                ui.add_space(10.0);
                self.draw_verdict(ui, avg);
                ui.add_space(10.0);

                let rest = (ui.available_height() - 6.0).max(120.0);
                ui.allocate_ui(Vec2::new(ui.available_width(), rest), |ui| {
                    ui.columns(2, |c| {
                        c[0].set_min_height(rest);
                        c[1].set_min_height(rest);
                        let inner = rest - 26.0;
                        self.draw_hits(&mut c[0], inner);
                        self.draw_seed(&mut c[1], inner);
                    });
                });
            });
    }

    fn draw_sparkline(&self, ui: &mut Ui) {
        card(ui, "TEMPO-VERLAUF", PRIMARY, |ui| {
            let h = 40.0;
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
            let painter = ui.painter();
            painter.line_segment(
                [
                    Pos2::new(rect.left(), rect.bottom()),
                    Pos2::new(rect.right(), rect.bottom()),
                ],
                Stroke::new(1.0_f32, FRAME),
            );

            let data = self.rate.history();
            if data.len() < 2 {
                return;
            }
            // Plotted as a filled trace across the full width. Drawing one bar
            // per sample made the first few readings span half the panel.
            let max = data.iter().copied().max().unwrap_or(1).max(1) as f32;
            let n = data.len() as f32;
            let pts: Vec<Pos2> = data
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    Pos2::new(
                        rect.left() + rect.width() * (i as f32 / (n - 1.0)),
                        rect.bottom() - (v as f32 / max) * (h - 3.0),
                    )
                })
                .collect();

            // Filled column-by-column rather than as one polygon: a throughput
            // trace is not convex, and `convex_polygon` renders non-convex
            // input as crossing triangles.
            let shade = Color32::from_rgba_unmultiplied(125, 207, 255, 34);
            for p in &pts {
                painter.line_segment(
                    [Pos2::new(p.x, rect.bottom()), *p],
                    Stroke::new(rect.width() / (n - 1.0) + 1.0, shade),
                );
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.6_f32, PRIMARY)));
        });
    }

    /// The loading screen.
    ///
    /// Shown from the first frame, while the database and filter are still
    /// being read on another thread. The bar tracks real work rather than a
    /// timer — a fake progress bar that finishes before the work does is worse
    /// than none.
    fn draw_loading(&mut self, ctx: &egui::Context) {
        let tex = self.logo_texture(ctx);
        let (step, frac) = match &self.loading {
            Some(p) => (p.step(), p.fraction()),
            None => (String::new(), 0.0),
        };
        let t = self.started.elapsed().as_secs_f32();

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.20).max(16.0));

                    // A slow breath on the mark, so the screen reads as alive
                    // even while a long filter build produces no visible change.
                    let breath = 0.90 + 0.10 * (t * 1.7).sin();
                    ui.add(
                        egui::Image::new(&tex)
                            .fit_to_exact_size(Vec2::new(190.0, 190.0))
                            .tint(Color32::from_white_alpha((breath * 255.0) as u8)),
                    );

                    ui.add_space(26.0);
                    ui.label(
                        RichText::new("S C H A T Z S U C H E")
                            .color(TEXT)
                            .size(24.0)
                            .strong(),
                    );
                    ui.add_space(30.0);

                    // Progress bar, drawn by hand so it matches the palette.
                    let w = 380.0_f32.min(ui.available_width() - 40.0);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 6.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, Rounding::same(3.0), Color32::from_rgb(28, 33, 47));
                    if frac > 0.0 {
                        let mut fill = rect;
                        fill.set_width(rect.width() * frac.clamp(0.02, 1.0));
                        p.rect_filled(fill, Rounding::same(3.0), GOLD);
                    }

                    ui.add_space(14.0);
                    ui.label(RichText::new(step).color(DIM).size(13.0));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{:.0} %", frac * 100.0))
                            .color(MUTED)
                            .font(mono(12.0)),
                    );

                    ui.add_space(30.0);
                    handle_link(ui, 13.0);
                });
            });

        // Repaint briskly: the bar and the breathing both need it.
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    /// The mandatory panel: the answer the program exists to give.
    fn draw_verdict(&self, ui: &mut Ui, rate: f64) {
        let ages = universe_ages_to_hit(self.funded_count, self.addresses_per_seed, rate);
        let expected = expected_seeds_to_hit(self.funded_count, self.addresses_per_seed);
        let seconds = if rate > 0.0 {
            expected / rate
        } else {
            f64::INFINITY
        };

        card(ui, "WIE LANGE BIS ZU EINEM TREFFER?", ALERT, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}  ×  das Alter des Universums",
                        german_scale(ages)
                    ))
                    .color(ALERT)
                    .size(19.0)
                    .strong(),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "Selbst wenn dieser Mac seit dem Urknall durchgehend rechnete, hätte er erst {} % davon geschafft.",
                        sci(100.0 / ages)
                    ))
                    .color(TEXT)
                    .size(12.5),
                );
                ui.label(
                    RichText::new(
                        "Das ist kein Fehler — es ist das Ergebnis. Es zeigt, warum Bitcoin-Wallets sicher sind.",
                    )
                    .color(TEXT)
                    .size(12.5),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "Fachwerte:  {} Seeds erwartet  ·  {} s  ·  p = {} pro Seed  ·  {} Adressen/Seed",
                        sci(expected),
                        sci(seconds),
                        sci(1.0 / expected),
                        self.addresses_per_seed
                    ))
                    .color(DIM)
                    .font(mono(11.5)),
                );
            });
        });
    }

    fn draw_hits(&mut self, ui: &mut Ui, min_h: f32) {
        // Self-test entries are counted and coloured separately. A dummy hit
        // that looks like a real one is worse than no display at all: it
        // promises a fortune that was never there.
        let real = self.hits.iter().filter(|h| !h.is_synthetic()).count();
        let tests = self.hits.len() - real;

        let title = match (real, tests) {
            (0, 0) => "TREFFER".to_string(),
            (0, t) => format!("TREFFER [0]  ·  {t} Testeintrag"),
            (r, 0) => format!("TREFFER [{r}]"),
            (r, t) => format!("TREFFER [{r}]  ·  {t} Testeintrag"),
        };
        let accent = if real > 0 { ALERT } else { PRIMARY };

        card(ui, &title, accent, |ui| {
            ui.set_min_height(min_h);
            if self.hits.is_empty() {
                ui.label(RichText::new("Noch kein Treffer.").color(DIM).size(12.5));
                ui.label(
                    RichText::new("Das ist der erwartete Zustand — siehe oben.")
                        .color(MUTED)
                        .size(12.0)
                        .italics(),
                );
                return;
            }
            if real == 0 {
                ui.label(
                    RichText::new(
                        "Kein echter Fund. Unten stehen nur Einträge aus dem Selbsttest.",
                    )
                    .color(DIM)
                    .size(12.0)
                    .italics(),
                );
                ui.add_space(6.0);
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for i in 0..self.hits.len() {
                        let h = &self.hits[i];
                        let synthetic = h.is_synthetic();
                        let text = format!(
                            "{} {:>14}   {:<24} {}",
                            if synthetic { "TEST" } else { "●" },
                            h.balance_btc.trim_end_matches(" BTC"),
                            h.address,
                            h.derivation_path
                        );
                        let sel = self.selected == Some(i);
                        let colour = if synthetic {
                            MUTED
                        } else if sel {
                            WARN
                        } else {
                            TEXT
                        };
                        if ui
                            .selectable_label(
                                sel,
                                RichText::new(text).font(mono(12.0)).color(colour),
                            )
                            .clicked()
                        {
                            self.selected = Some(i);
                        }
                    }
                });
        });
    }

    fn draw_seed(&self, ui: &mut Ui, min_h: f32) {
        let Some(hit) = self.selected.and_then(|i| self.hits.get(i)) else {
            card(ui, "SEED — NUR HIER LOKAL", DIM, |ui| {
                ui.set_min_height(min_h);
                ui.label(
                    RichText::new(
                        "Sobald ein Treffer da ist, stehen hier seine Wörter im Klartext.",
                    )
                    .color(DIM)
                    .size(12.5),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Sie verlassen diesen Rechner nie.")
                        .color(MUTED)
                        .size(12.0)
                        .italics(),
                );
            });
            return;
        };

        let synthetic = hit.is_synthetic();
        let (title, accent) = if synthetic {
            ("TESTEINTRAG — KEIN ECHTER FUND", MUTED)
        } else {
            ("SEED — NUR HIER LOKAL", ALERT)
        };

        card(ui, title, accent, |ui| {
            ui.set_min_height(min_h);
            if synthetic {
                ui.label(
                    RichText::new(
                        "Dieser Eintrag stammt aus dem Selbsttest der Speicher- und Alarmkette.",
                    )
                    .color(WARN)
                    .size(12.5),
                );
                ui.label(
                    RichText::new(
                        "Die Wörter unten sind das öffentliche BIP-39-Testbeispiel. Diese Wallet \
                         ist leer und ihr Schlüssel ist weltweit bekannt.",
                    )
                    .color(MUTED)
                    .size(11.5),
                );
                ui.add_space(10.0);
            }
            kv(ui, "Adresse", &hit.address, TEXT);
            kv(ui, "Guthaben", &hit.balance_btc, WARN);
            kv(ui, "Pfad", &hit.derivation_path, DIM);
            ui.add_space(10.0);

            let words: Vec<&str> = hit.mnemonic.split_whitespace().collect();
            for (row, chunk) in words.chunks(4).enumerate() {
                ui.horizontal(|ui| {
                    for (j, w) in chunk.iter().enumerate() {
                        ui.label(
                            RichText::new(format!("{:>2}.", row * 4 + j + 1))
                                .color(MUTED)
                                .font(mono(12.0)),
                        );
                        ui.label(
                            RichText::new(format!("{w:<10}"))
                                .color(WARN)
                                .font(mono(13.0))
                                .strong(),
                        );
                    }
                });
            }

            ui.add_space(10.0);
            ui.label(
                RichText::new("Gespeichert in hits.jsonl, nur für dich lesbar.")
                    .color(MUTED)
                    .size(11.5)
                    .italics(),
            );
            ui.label(
                RichText::new("Wird an keinen Benachrichtigungsdienst gesendet.")
                    .color(MUTED)
                    .size(11.5)
                    .italics(),
            );
        });
    }

    /// Settings window: presets for everyone, raw knobs behind a warning.
    fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let max_cores = physical_cores().max(1);

        egui::SidePanel::right("settings")
            .resizable(false)
            .exact_width(360.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, FRAME))
                    .inner_margin(egui::Margin::symmetric(16.0, 14.0)),
            )
            .show(ctx, |ui| {
                let threads = self.control.active_threads();

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("EINSTELLUNGEN")
                            .color(TEXT)
                            .size(13.0)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Schließen").color(DIM).size(12.0))
                                    .fill(BG)
                                    .stroke(Stroke::new(1.0_f32, FRAME))
                                    .rounding(Rounding::same(5.0)),
                            )
                            .clicked()
                        {
                            self.settings_open = false;
                        }
                    });
                });
                ui.add_space(12.0);

                ui.label(RichText::new("LEISTUNG").color(PRIMARY).size(11.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Mehr Kerne heißt mehr Tempo, aber auch mehr Strom und Wärme.")
                        .color(DIM)
                        .size(12.0),
                );
                ui.add_space(10.0);

                // Presets carry the measured trade-off in their labels, so the
                // choice does not require reading a benchmark first.
                let presets = [
                    (
                        "Sparsam",
                        4usize,
                        Priority::Background,
                        "Nur Effizienzkerne, Mac bleibt kühl und leise",
                    ),
                    (
                        "Ausgewogen",
                        4,
                        Priority::Normal,
                        "Empfohlen — halbe Kerne, volles Tempo darauf",
                    ),
                    (
                        "Maximum",
                        max_cores,
                        Priority::Normal,
                        "Alle Kerne, Mac wird warm und lauter",
                    ),
                ];
                for (name, t, prio, hint) in presets {
                    let t = t.min(max_cores);
                    let active = threads == t && self.control.priority() == prio;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!(
                                    "{name}   ·   {t} Kerne   ·   ca. {:.0} % Tempo",
                                    self.expected_share(t, prio) * 100.0
                                ))
                                .color(if active { Color32::BLACK } else { TEXT })
                                .size(12.5)
                                .strong(),
                            )
                            .fill(if active { PRIMARY } else { BG })
                            .stroke(Stroke::new(1.0_f32, FRAME))
                            .rounding(Rounding::same(6.0))
                            .min_size(Vec2::new(ui.available_width(), 28.0)),
                        )
                        .clicked()
                    {
                        self.control.set_active_threads(t);
                        self.control.set_priority(prio);
                    }
                    ui.label(RichText::new(hint).color(MUTED).size(11.0));
                    ui.add_space(8.0);
                }

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    let mut on = self.expert_unlocked;
                    // Unlocking is gated; switching off never is.
                    if ui.checkbox(&mut on, "").changed() {
                        if on {
                            self.expert_prompt = true;
                        } else {
                            self.expert_unlocked = false;
                        }
                    }
                    ui.label(
                        RichText::new("Expertenmodus")
                            .color(if self.expert_unlocked { WARN } else { DIM })
                            .size(13.0)
                            .strong(),
                    );
                });

                if !self.expert_unlocked {
                    ui.label(
                        RichText::new(
                            "Direkte Regler für Kerne, Priorität und Adressen pro Wallet.",
                        )
                        .color(MUTED)
                        .size(11.0),
                    );
                    return;
                }

                ui.add_space(10.0);
                let mut t = self.control.active_threads();
                ui.label(RichText::new("Kerne").color(DIM).size(12.0));
                if ui
                    .add_sized(
                        Vec2::new(ui.available_width(), 20.0),
                        egui::Slider::new(&mut t, 1..=max_cores),
                    )
                    .changed()
                {
                    self.control.set_active_threads(t);
                }

                ui.add_space(8.0);
                let mut n = self.control.addresses_per_path();
                ui.label(RichText::new("Adressen pro Pfad").color(DIM).size(12.0));
                if ui
                    .add_sized(
                        Vec2::new(ui.available_width(), 20.0),
                        egui::Slider::new(&mut n, 1..=50),
                    )
                    .changed()
                {
                    self.control.set_addresses_per_path(n);
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{} Adressen je Wallet. Weniger = mehr Wallets/s, aber weniger Adressen/s.",
                        n * 3
                    ))
                    .color(MUTED)
                    .size(11.0),
                );

                ui.add_space(4.0);
                let prio = self.control.priority();
                ui.label(
                    RichText::new(format!(
                        "Geschätztes Tempo: ca. {:.0} % des Maximums",
                        self.expected_share(t, prio) * 100.0
                    ))
                    .color(TEXT)
                    .size(12.0),
                );
                if self.is_counterproductive(t, prio) {
                    ui.label(
                        RichText::new(
                            "Achtung: bei Priorität „Sparsam\" laufen alle Threads auf den \
                             Effizienzkernen. Mehr als die Hälfte der Kerne macht es dort \
                             langsamer, nicht schneller.",
                        )
                        .color(WARN)
                        .size(11.5),
                    );
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Priorität").color(DIM).size(12.0));
                    for p in [Priority::Background, Priority::Utility, Priority::Normal] {
                        let on = self.control.priority() == p;
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(p.label())
                                        .color(if on { Color32::BLACK } else { TEXT })
                                        .size(12.0),
                                )
                                .fill(if on { WARN } else { BG })
                                .stroke(Stroke::new(1.0_f32, FRAME))
                                .rounding(Rounding::same(5.0)),
                            )
                            .clicked()
                        {
                            self.control.set_priority(p);
                        }
                    }
                });

                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Änderungen wirken sofort und werden nicht gespeichert. \
                         Für dauerhafte Werte config.toml bearbeiten.",
                    )
                    .color(MUTED)
                    .size(11.0),
                );
            });

        // The gate. Deliberately modal and deliberately not pre-answered.
        if self.expert_prompt {
            egui::Window::new("Expertenmodus einschalten?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0_f32, WARN)),
                )
                .show(ctx, |ui| {
                    ui.set_max_width(430.0);
                    ui.label(
                        RichText::new("Diese Regler können deinen Mac stark belasten.")
                            .color(WARN)
                            .size(14.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Alle Kerne auf voller Priorität heißt: dauerhaft 100 % Auslastung, \
                             spürbare Wärme, lauter Lüfter und bei einem Laptop deutlich kürzere \
                             Akkulaufzeit. Schaden nimmt der Mac nicht — er drosselt sich selbst, \
                             bevor etwas passiert — aber angenehm ist es nicht.",
                        )
                        .color(TEXT)
                        .size(12.5),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Am Ergebnis ändert das nichts: gefunden wird so oder so nichts.",
                        )
                        .color(DIM)
                        .size(12.0)
                        .italics(),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Verstanden, einschalten")
                                        .color(Color32::BLACK)
                                        .size(13.0)
                                        .strong(),
                                )
                                .fill(WARN)
                                .rounding(Rounding::same(6.0))
                                .min_size(Vec2::new(190.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.expert_unlocked = true;
                            self.expert_prompt = false;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Abbrechen").color(TEXT).size(13.0),
                                )
                                .fill(BG)
                                .stroke(Stroke::new(1.0_f32, FRAME))
                                .rounding(Rounding::same(6.0))
                                .min_size(Vec2::new(120.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.expert_prompt = false;
                        }
                    });
                });
        }
    }

    /// Screenshot mode: capture one frame to a raw RGBA dump, then quit. Used
    /// to review the layout without a display attached to the session.
    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot_path.clone() else {
            return;
        };
        match self.shot_at {
            None => {
                // Let a few frames settle so fonts and textures are resident.
                let delay = if std::env::var("SC_SHOT_LOADING").is_ok() {
                    Duration::from_millis(700)
                } else {
                    Duration::from_millis(4200)
                };
                if self.started.elapsed() > delay {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                    self.shot_at = Some(Instant::now());
                }
                ctx.request_repaint();
            }
            Some(_) => {
                let image = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                });
                if let Some(img) = image {
                    let mut out = Vec::with_capacity(8 + img.pixels.len() * 4);
                    out.extend_from_slice(&(img.size[0] as u32).to_le_bytes());
                    out.extend_from_slice(&(img.size[1] as u32).to_le_bytes());
                    for p in &img.pixels {
                        out.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
                    }
                    let _ = std::fs::write(&path, out);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ctx.request_repaint();
            }
        }
    }
}

/// Paints a failure over the whole window.
///
/// Used both by the standalone error window and when loading fails after the
/// main window is already up.
pub(crate) fn draw_error_panel(ctx: &egui::Context, message: &str) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(BG)
                .inner_margin(egui::Margin::symmetric(24.0, 20.0)),
        )
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Die Schatzsuche konnte nicht starten")
                    .color(ALERT)
                    .size(17.0)
                    .strong(),
            );
            ui.add_space(14.0);
            egui::Frame::none()
                .fill(PANEL)
                .rounding(Rounding::same(8.0))
                .stroke(Stroke::new(1.0_f32, FRAME))
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(message).color(TEXT).font(mono(12.5)));
                });
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Schließen")
                                .color(Color32::BLACK)
                                .size(13.0)
                                .strong(),
                        )
                        .fill(PRIMARY)
                        .rounding(Rounding::same(6.0))
                        .min_size(Vec2::new(120.0, 30.0)),
                    )
                    .clicked()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
}

/// Shows a failure in a window.
///
/// A double-clicked application writes its stderr nowhere anybody will look, so
/// an error that only reaches the console is an application that silently does
/// nothing. This is the fallback for that case.
pub fn show_error(message: &str) {
    let msg = message.to_string();
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([620.0, 340.0])
            .with_resizable(false)
            .with_title("Schatzsuche — Fehler"),
        ..Default::default()
    };

    struct ErrorApp {
        msg: String,
    }

    impl eframe::App for ErrorApp {
        fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
            [13.0 / 255.0, 15.0 / 255.0, 22.0 / 255.0, 1.0]
        }

        fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
            let mut style = (*ctx.style()).clone();
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = BG;
            style.visuals.override_text_color = Some(TEXT);
            ctx.set_style(style);

            draw_error_panel(ctx, &self.msg);
        }
    }

    let _ = eframe::run_native(
        "Schatzsuche — Fehler",
        opts,
        Box::new(move |_| Ok(Box::new(ErrorApp { msg }))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_model_reflects_the_measurements() {
        use std::sync::mpsc::channel;
        let (_tx, rx) = channel();
        let app = GuiApp::new(
            Arc::new(Stats::new()),
            Arc::new(Control::new(4, 20, Priority::Utility)),
            rx,
            Vec::new(),
            5_000_000,
            60,
            256,
            8,
            0,
            0,
            None,
            None,
        );

        // More cores must mean more throughput at normal priority.
        let normal: Vec<f64> = [1usize, 2, 4, 8]
            .iter()
            .map(|&t| app.expected_share(t, Priority::Normal))
            .collect();
        assert!(
            normal.windows(2).all(|w| w[1] > w[0]),
            "normal priority should scale up: {normal:?}"
        );

        // Background priority is the exception: past half the cores it gets
        // worse, because the work is confined to the efficiency cores.
        let bg8 = app.expected_share(8, Priority::Background);
        let bg4 = app.expected_share(4, Priority::Background);
        assert!(
            bg8 < bg4,
            "background at 8 cores measured slower than at 4: {bg8} vs {bg4}"
        );
        assert!(app.is_counterproductive(8, Priority::Background));
        assert!(!app.is_counterproductive(4, Priority::Background));
        assert!(!app.is_counterproductive(8, Priority::Normal));

        // Background must always be well below normal at the same core count.
        for t in [2usize, 4] {
            assert!(
                app.expected_share(t, Priority::Background)
                    < app.expected_share(t, Priority::Normal) * 0.7,
                "background should be clearly slower at {t} cores"
            );
        }
    }

    /// The previous name must not survive anywhere a user can see it.
    ///
    /// Renaming missed the letter-spaced wordmark on the intro screen and four
    /// strings that go out in notifications, so this checks the sources rather
    /// than trusting a search-and-replace. The needles are assembled from
    /// fragments so this test does not match itself.
    #[test]
    fn the_old_name_is_gone() {
        let needles = [
            concat!("SE", "ED", " COLL", "IDER"),
            concat!("S E", " E D"),
            concat!("C O L", " L I D E R"),
            concat!("seed", "-coll", "ider"),
            concat!("Seed ", "Coll", "ider"),
        ];
        for (name, body) in [
            ("gui.rs", include_str!("gui.rs")),
            ("tui.rs", include_str!("tui.rs")),
            ("alert/mod.rs", include_str!("alert/mod.rs")),
            ("alert/channels.rs", include_str!("alert/channels.rs")),
            ("bench.rs", include_str!("bench.rs")),
            ("config.rs", include_str!("config.rs")),
            ("main.rs", include_str!("main.rs")),
        ] {
            for needle in needles {
                assert!(
                    !body.contains(needle),
                    "{name} still carries the old name {needle:?}"
                );
            }
        }
    }

    /// The link target and the displayed handle must not drift apart.
    #[test]
    fn handle_and_link_agree() {
        assert!(HANDLE.starts_with('@'));
        let name = HANDLE.trim_start_matches('@');
        assert!(
            HANDLE_URL.ends_with(name),
            "{HANDLE_URL} does not point at {name}"
        );
        assert!(HANDLE_URL.starts_with("https://"), "must be https");
    }

    #[test]
    fn german_scale_matches_the_tui() {
        assert_eq!(german_scale(5.7e18), "5,7 Trillionen");
        assert_eq!(german_scale(1.0e9), "1,0 Milliarde");
        assert!(german_scale(1e40).starts_with("10 hoch"));
        assert_eq!(german_scale(f64::INFINITY), "unendlich");
    }

    #[test]
    fn formatting_matches_the_tui() {
        assert_eq!(sci(0.0), "0");
        assert_eq!(sci(f64::INFINITY), "unendlich");
        assert!(sci(1.5e-9).contains(','));
        assert_eq!(thousands(1_234_567), "1 234 567");
    }

    /// The embedded icon must be a sane, non-empty image.
    #[test]
    fn icon_data_is_well_formed() {
        use crate::icon_data;
        assert_eq!(
            icon_data::ICON_RGBA.len(),
            (icon_data::ICON_W * icon_data::ICON_H * 4) as usize
        );
        // Opaque enough to be a real icon rather than a blank sheet.
        let opaque = icon_data::ICON_RGBA
            .chunks_exact(4)
            .filter(|c| c[3] > 200)
            .count();
        let total = (icon_data::ICON_W * icon_data::ICON_H) as usize;
        assert!(
            opaque > total / 2,
            "icon is mostly transparent: {opaque}/{total}"
        );
        // And not a single flat colour.
        let distinct: std::collections::HashSet<[u8; 3]> = icon_data::ICON_RGBA
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        assert!(
            distinct.len() > 32,
            "icon has only {} colours",
            distinct.len()
        );
    }
}
