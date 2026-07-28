//! Live terminal UI.
//!
//! The interface is German, because its first reader is not an engineer.
//!
//! Every panel pairs a headline number with a plain-language caption saying
//! what it is, and keeps the precise figures on the rows beneath — a novice
//! should be able to read the screen without a glossary, and an expert should
//! not have to go looking for the real numbers.
//!
//! The mandatory line is "WIE LANGE BIS ZU EINEM TREFFER?". Everything else is
//! instrumentation; that panel is the result.

use std::io::{self, Stdout, Write};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Sparkline, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::engine::Event;
use crate::hits::Hit;
use crate::stats::{Control, Rate, Stats};

/// 13.787 billion years, in seconds.
pub const AGE_OF_UNIVERSE_SECS: f64 = 4.351e17;
/// HASH160 output space, 2^160; a derived address is uniform over it.
pub const ADDRESS_SPACE: f64 = 1.461_501_637_330_9e48;

/// Expected number of seeds to test before one derived address matches.
///
/// Every seed produces `addresses_per_seed` uniform 160-bit hashes, so the
/// per-seed success probability is `addresses * funded / 2^160`.
pub fn expected_seeds_to_hit(funded: u64, addresses_per_seed: u32) -> f64 {
    let p = addresses_per_seed as f64 * funded as f64 / ADDRESS_SPACE;
    if p <= 0.0 {
        f64::INFINITY
    } else {
        1.0 / p
    }
}

/// The headline figure: expected search time as a multiple of the age of the
/// universe.
pub fn universe_ages_to_hit(funded: u64, addresses_per_seed: u32, seeds_per_sec: f64) -> f64 {
    if seeds_per_sec <= 0.0 {
        return f64::INFINITY;
    }
    (expected_seeds_to_hit(funded, addresses_per_seed) / seeds_per_sec) / AGE_OF_UNIVERSE_SECS
}

// Palette. Two deliberate choices here:
//
// The background is set explicitly rather than inherited from the terminal, so
// the panels can sit a few shades lighter than the page and read as cards. A
// `Color::Reset` background makes that impossible — the contrast would invert
// on a light terminal profile.
//
// The accents are desaturated. Fully saturated cyan and magenta on black are
// what a terminal *can* do, not what is comfortable to look at for hours.
const C_BG: Color = Color::Rgb(13, 15, 22); // page
const C_PANEL: Color = Color::Rgb(24, 28, 41); // cards, lifted off the page
const C_FRAME: Color = Color::Rgb(37, 43, 61); // hairline borders
const C_PRIMARY: Color = Color::Rgb(125, 207, 255); // soft cyan
const C_ACCENT: Color = Color::Rgb(187, 154, 247); // soft violet
const C_TEXT: Color = Color::Rgb(192, 202, 245);
const C_DIM: Color = Color::Rgb(97, 108, 152);
const C_MUTED: Color = Color::Rgb(58, 66, 96); // rules and inactive marks
const C_ALERT: Color = Color::Rgb(247, 118, 142); // soft red
const C_WARN: Color = Color::Rgb(224, 175, 104); // amber

pub struct App {
    stats: Arc<Stats>,
    control: Arc<Control>,
    rate: Rate,
    events: Receiver<Event>,
    hits: Vec<Hit>,
    /// Index into `hits`, when the operator has selected one.
    selected: Option<usize>,
    list_state: ListState,
    /// Screen area of the hit list, so mouse clicks can be mapped to rows.
    list_area: Rect,
    /// Screen area of the start/stop button.
    button_area: Rect,
    errors: Vec<String>,
    peak: f64,
    funded_count: u64,
    addresses_per_seed: u32,
    entropy_bits: u32,
    threads: usize,
    bloom_bytes: usize,
    db_bytes: usize,
    /// Drives the marquee in the header.
    tick: u64,
}

impl App {
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
    ) -> App {
        App {
            stats,
            control,
            rate: Rate::new(120),
            events,
            hits: existing,
            selected: None,
            list_state: ListState::default(),
            list_area: Rect::default(),
            button_area: Rect::default(),
            errors: Vec::new(),
            peak: 0.0,
            funded_count,
            addresses_per_seed,
            entropy_bits,
            threads,
            bloom_bytes,
            db_bytes,
            tick: 0,
        }
    }

    fn expected_seeds(&self) -> f64 {
        expected_seeds_to_hit(self.funded_count, self.addresses_per_seed)
    }

    fn universe_ages_to_hit(&self, rate: f64) -> f64 {
        universe_ages_to_hit(self.funded_count, self.addresses_per_seed, rate)
    }

    /// Fraction of the seed keyspace covered so far.
    fn keyspace_fraction(&self, seeds: u64) -> f64 {
        seeds as f64 / 2f64.powi(self.entropy_bits as i32)
    }

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(Event::Hit(h)) => {
                    self.hits.push(*h);
                    // Keep the newest selected so the seed panel is populated.
                    self.selected = Some(self.hits.len() - 1);
                    self.list_state.select(self.selected);
                }
                Ok(Event::PersistFailure { hit, error }) => {
                    self.errors.push(format!(
                        "PERSIST FAILED for {} — {error}. Seed is NOT on disk.",
                        hit.address
                    ));
                    self.hits.push(*hit);
                }
                Ok(Event::BackupFailure { id, error }) => {
                    self.errors
                        .push(format!("backup copy failed for {id}: {error}"));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn select_delta(&mut self, delta: i32) {
        if self.hits.is_empty() {
            return;
        }
        let cur = self.selected.unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, self.hits.len() as i32 - 1) as usize;
        self.selected = Some(next);
        self.list_state.select(self.selected);
    }

    /// Routes a click to the button or the hit list.
    fn click(&mut self, col: u16, row: u16) {
        if hit_rect(self.button_area, col, row) {
            self.control.toggle_paused();
            return;
        }

        let a = self.list_area;
        if !hit_rect(a, col, row) {
            return;
        }
        let inner_top = a.y + 1;
        if row < inner_top {
            return;
        }
        let idx = (row - inner_top) as usize + self.list_state.offset();
        if idx < self.hits.len() {
            self.selected = Some(idx);
            self.list_state.select(self.selected);
        }
    }

    pub fn paused(&self) -> bool {
        self.control.paused()
    }
}

fn hit_rect(r: Rect, col: u16, row: u16) -> bool {
    r.width > 0
        && r.height > 0
        && col >= r.x
        && col < r.x + r.width
        && row >= r.y
        && row < r.y + r.height
}

/// Scientific notation, German decimal comma.
fn sci(x: f64) -> String {
    if !x.is_finite() {
        return "unendlich".to_string();
    }
    // A literal "0,0000e0" at startup reads as a broken display rather than as
    // "nothing searched yet".
    if x == 0.0 {
        return "0".to_string();
    }
    format!("{x:.4e}").replace('.', ",")
}

fn thousands(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut parts = Vec::new();
    while n > 0 {
        parts.push(format!("{:03}", n % 1000));
        n /= 1000;
    }
    parts.reverse();
    let mut s = parts.join(" ");
    // Strip the zero padding on the leading group.
    while s.starts_with('0') && s.len() > 1 && !s.starts_with("0 ") {
        s.remove(0);
    }
    s
}

/// The handle shown in the intro and the footer.
pub const HANDLE: &str = "@21clubz";
/// Where clicking the handle leads. Kept beside it so the two cannot drift.
pub const HANDLE_URL: &str = "https://x.com/21clubz";

/// The treasure chest from the app icon, rasterised to half-blocks.
///
/// The chest's banding and lock survive only as negative space; in colour they
/// are gold on wood, and a flat silhouette without them reads as a loaf.
///
/// Half-blocks rather than full blocks — a terminal cell is about twice as tall
/// as it is wide, so splitting it vertically yields square pixels and doubles
/// the effective resolution.
const LOGO: [&str; 20] = [
    "           ▄▄▄▄██████████▄▄▄▄           ",
    "       ▄▄██████████████████████▄▄       ",
    "     ▄████████████████████████████▄     ",
    "   ▄████████████████████████████████▄   ",
    "  █████   ████████████████████   █████  ",
    " ██████▄▄▄████████████████████▄▄▄██████ ",
    "████████████████████████████████████████",
    "████████████████████████████████████████",
    "                                        ",
    "                                        ",
    " ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄ ",
    "████████████████████████████████████████",
    "████████████████████████████████████████",
    "███████   ████████████████████   ███████",
    "███████   ██████        ██████   ███████",
    "███████   ██████        ██████   ███████",
    "███████   ██████        ██████   ███████",
    "███████▄▄▄██████▄      ▄██████▄▄▄███████",
    "████████████████████████████████████████",
    "▀██████████████████████████████████████▀",
];

const INTRO_FRAMES: u32 = 46;
const INTRO_FRAME_MS: u64 = 33;

/// Blends `to` up from the page background. `t` runs 0..1.
fn fade(to: Color, t: f64) -> Color {
    let (r, g, b) = match to {
        Color::Rgb(r, g, b) => (r as f64, g as f64, b as f64),
        _ => (192.0, 202.0, 245.0),
    };
    let t = t.clamp(0.0, 1.0);
    let (br, bg, bb) = (13.0, 15.0, 22.0);
    Color::Rgb(
        (br + (r - br) * t).round() as u8,
        (bg + (g - bg) * t).round() as u8,
        (bb + (b - bb) * t).round() as u8,
    )
}

/// Opacity of the element that starts fading in at frame `start`.
fn stage(frame: u32, start: u32) -> f64 {
    ((frame as f64 - start as f64) / 9.0).clamp(0.0, 1.0)
}

/// Draws one intro frame. Split out from [`intro`] so it can be tested and
/// previewed without a terminal.
fn draw_intro(f: &mut Frame, frame: u32, funded: u64, threads: usize) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(C_BG)), area);

    const GOLD: Color = Color::Rgb(232, 176, 84);
    let mut lines: Vec<Line> = vec![Line::from("")];

    for row in LOGO {
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(fade(GOLD, stage(frame, 0))),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "S C H A T Z S U C H E",
        Style::default()
            .fg(fade(C_TEXT, stage(frame, 6)))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "────────────────────────────────",
        Style::default().fg(fade(C_FRAME, stage(frame, 10))),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{} Adressen geladen  ·  {} Kerne bereit",
            thousands(funded),
            threads
        ),
        Style::default().fg(fade(C_DIM, stage(frame, 14))),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        HANDLE,
        Style::default()
            .fg(fade(C_ACCENT, stage(frame, 19)))
            .add_modifier(Modifier::BOLD),
    )));

    // Centre the block vertically; on a short terminal it simply starts at the
    // top rather than being clipped from both ends.
    let top = area.height.saturating_sub(lines.len() as u16) / 2;
    let inner = Rect::new(area.x, area.y + top, area.width, area.height - top);

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

/// Plays the intro. Any key or click skips it.
fn intro(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    funded: u64,
    threads: usize,
) -> io::Result<()> {
    for frame in 0..INTRO_FRAMES {
        terminal.draw(|f| draw_intro(f, frame, funded, threads))?;
        // The poll doubles as the frame delay, so a keypress cuts the intro
        // short instead of merely being queued behind it.
        if event::poll(Duration::from_millis(INTRO_FRAME_MS))? {
            let _ = event::read()?;
            break;
        }
    }
    Ok(())
}

pub fn run(mut app: App, on_quit: impl Fn()) -> io::Result<()> {
    let mut terminal = setup()?;
    let (funded, threads) = (app.funded_count, app.threads);
    let res =
        intro(&mut terminal, funded, threads).and_then(|()| event_loop(&mut terminal, &mut app));
    restore(&mut terminal)?;
    on_quit();
    res
}

fn setup() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // Without this, a panic leaves the terminal in raw mode and the user's
    // shell unusable.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, DisableMouseCapture);
        let _ = disable_raw_mode();
        default_hook(info);
    }));

    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()
}

/// Redraw cadence while searching. Four frames a second is plenty for a
/// counter display and costs a fraction of the terminal I/O that 10fps does.
const FRAME_RUNNING: Duration = Duration::from_millis(250);
/// While paused nothing changes, so back right off.
const FRAME_PAUSED: Duration = Duration::from_millis(1000);

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    // Start due, so the first frame paints immediately.
    let mut last_draw = Instant::now()
        .checked_sub(FRAME_PAUSED)
        .unwrap_or_else(Instant::now);

    loop {
        // An external stop (--duration, or a signal handler) must close the UI
        // too. Without this the engine winds down and the display sits there
        // showing frozen counters with no way to tell it apart from a stall.
        if app.control.stopping() {
            return Ok(());
        }

        let interval = if app.paused() {
            FRAME_PAUSED
        } else {
            FRAME_RUNNING
        };

        if last_draw.elapsed() >= interval {
            app.drain_events();
            if !app.paused() {
                let inst = app.rate.sample(app.stats.seeds());
                if inst > app.peak {
                    app.peak = inst;
                }
            }
            app.tick = app.tick.wrapping_add(1);
            terminal.draw(|f| draw(f, app))?;
            last_draw = Instant::now();
        }

        // Block in `poll` until input arrives or the next frame is due. An idle
        // collider therefore wakes this thread once a second, not ten times.
        let wait = interval
            .saturating_sub(last_draw.elapsed())
            .max(Duration::from_millis(5));
        if !event::poll(wait)? {
            continue;
        }

        // Force the next loop iteration to repaint, so input feels immediate
        // even at a one-second cadence.
        let mut force = || {
            last_draw = Instant::now()
                .checked_sub(FRAME_PAUSED)
                .unwrap_or_else(Instant::now)
        };

        match event::read()? {
            CEvent::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char(' ') | KeyCode::Enter => {
                    app.control.toggle_paused();
                    force();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.select_delta(-1);
                    force();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.select_delta(1);
                    force();
                }
                _ => {}
            },
            CEvent::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    app.click(m.column, m.row);
                    force();
                }
                MouseEventKind::ScrollUp => {
                    app.select_delta(-1);
                    force();
                }
                MouseEventKind::ScrollDown => {
                    app.select_delta(1);
                    force();
                }
                _ => {}
            },
            CEvent::Resize(_, _) => force(),
            _ => {}
        }
    }
}

// --- presentation helpers ---------------------------------------------------

/// German decimal notation: comma, fixed places.
fn de(x: f64, places: usize) -> String {
    format!("{x:.places$}", places = places).replace('.', ",")
}

/// Names a magnitude in German long-scale words.
///
/// The headline figure is an exponent nobody can feel. "5,7 Trillionen" is a
/// quantity a reader can at least place next to other quantities; `5.7e18` is
/// a string of characters. Both are shown — this one first.
fn german_scale(x: f64) -> String {
    if !x.is_finite() {
        return "unendlich".to_string();
    }
    if x < 1.0 {
        return de(x, 2);
    }
    if x < 1_000_000.0 {
        return thousands(x as u64);
    }

    // Long scale, as used in German: 10^9 is a Milliarde, not a Billion.
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

    // Past the last named scale a word form would only mislead: "10 Millionen
    // Quintilliarden" is not more graspable than the exponent.
    let largest = NAMES[NAMES.len() - 1].0;
    if x >= largest * 1000.0 {
        return format!("10 hoch {:.0}", x.log10());
    }

    let mut chosen = NAMES[0];
    for entry in NAMES {
        if x >= entry.0 {
            chosen = entry;
        }
    }
    let (scale, singular, plural) = chosen;
    let m = x / scale;
    let word = if (m - 1.0).abs() < 0.05 {
        singular
    } else {
        plural
    };
    format!("{} {}", de(m, 1), word)
}

fn frame_block(title: &str, accent: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(C_FRAME))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(C_PANEL))
}

/// A panel's headline number plus the plain-language line that says what it is.
fn hero<'a>(value: String, caption: &'a str, color: Color) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled(
            format!(" {value}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(" {caption}"),
            Style::default().fg(C_DIM),
        )),
        Line::from(""),
    ]
}

/// A detail row: dim label, bright value. This is where the expert numbers live.
fn kv<'a>(k: &'a str, v: String, color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!(" {k:<12}"), Style::default().fg(C_DIM)),
        Span::styled(v, Style::default().fg(color)),
    ])
}

fn draw(f: &mut Frame, app: &mut App) {
    let seeds = app.stats.seeds();
    let addresses = app.stats.addresses();
    let rate_now = app.rate.history().last().copied().unwrap_or(0) as f64;
    let rate_avg = app.rate.average();

    let area = f.area();
    // Paint the page first. Panels then sit a few shades above it, which is
    // what gives the layout depth without drawing a single extra border.
    f.render_widget(
        Block::default().style(Style::default().bg(C_BG).fg(C_TEXT)),
        area,
    );

    // Below this the explanatory text has nowhere to go, so it is dropped and
    // only the numbers remain.
    let roomy = area.height >= 32;
    let has_error = !app.errors.is_empty();

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if roomy { 3 } else { 2 }), // header
            Constraint::Length(8),                         // three metric panels
            Constraint::Length(3),                         // throughput history
            Constraint::Length(if roomy { 9 } else { 5 }), // the verdict
            Constraint::Min(7),                            // hits + seed
            Constraint::Length(if has_error { 4 } else { 1 }),
        ])
        .split(area);

    draw_header(f, rows[0], app, roomy);
    draw_metrics(f, rows[1], &*app, seeds, addresses, rate_now, rate_avg);
    draw_sparkline(f, rows[2], &*app);
    draw_verdict(f, rows[3], &*app, rate_avg, roomy);

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(rows[4]);
    let (hits_area, seed_area) = (bottom[0], bottom[1]);
    draw_hits(f, hits_area, app);
    draw_seed(f, seed_area, &*app);

    draw_footer(f, rows[5], &*app);
}

/// Width of the start/stop button, including its padding.
const BUTTON_W: u16 = 13;

fn draw_header(f: &mut Frame, area: Rect, app: &mut App, roomy: bool) {
    let paused = app.control.paused();

    let label_w: u16 = 18;
    let status_w: u16 = 14;
    let bar_w = area
        .width
        .saturating_sub(label_w + status_w + BUTTON_W + 2)
        .max(1) as usize;

    // A thin rule with a travelling highlight, rather than a block-character
    // scanner. Frozen when stopped: an animation running under a "STOPP" label
    // reads as a bug.
    let bar: Vec<Span> = if paused {
        vec![Span::styled(
            "┄".repeat(bar_w),
            Style::default().fg(C_MUTED),
        )]
    } else {
        let pos = app.tick as usize % bar_w.max(1);
        (0..bar_w)
            .map(|i| {
                let (ch, col) = match i.abs_diff(pos) {
                    0 => ('━', C_PRIMARY),
                    1 => ('━', C_ACCENT),
                    2 => ('─', C_FRAME),
                    _ => ('─', C_MUTED),
                };
                Span::styled(ch.to_string(), Style::default().fg(col))
            })
            .collect()
    };

    let (dot, status, status_color) = if paused {
        ("◦", "ANGEHALTEN", C_WARN)
    } else {
        ("●", "LÄUFT", C_PRIMARY)
    };

    let mut title = vec![Span::styled(
        " ✦ SCHATZSUCHE  ",
        Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
    )];
    title.extend(bar);
    title.push(Span::styled(
        format!("  {dot} {status}  "),
        Style::default().fg(status_color),
    ));

    let mut lines = vec![Line::from(title)];

    if roomy {
        let explain = if paused {
            "Angehalten. Die Prozessorkerne schlafen — es wird gerade kein Strom verbraucht."
        } else {
            "Würfelt zufällige Bitcoin-Wallets und prüft, ob eine davon Guthaben besitzt."
        };
        lines.push(Line::from(Span::styled(
            format!("   {explain}"),
            Style::default().fg(C_DIM),
        )));
    }

    // A single hairline under the header, instead of boxing it in. The heavy
    // bordered banner was the loudest thing on the screen and carried no
    // information the title line does not already carry.
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(C_FRAME))
                .style(Style::default().bg(C_BG)),
        ),
        area,
    );

    // Painted over the header; the rect is remembered so clicks route back.
    if area.width > BUTTON_W + 2 {
        let btn = Rect::new(area.x + area.width - BUTTON_W - 1, area.y, BUTTON_W, 1);
        app.button_area = btn;

        let (text, bg) = if paused {
            ("  ▶  START  ", C_PRIMARY)
        } else {
            ("  ■  STOPP  ", C_WARN)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                text,
                Style::default()
                    .fg(Color::Black)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ))),
            btn,
        );
    } else {
        app.button_area = Rect::default();
    }
}

fn draw_metrics(
    f: &mut Frame,
    area: Rect,
    app: &App,
    seeds: u64,
    addresses: u64,
    now: f64,
    avg: f64,
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);

    // --- Tempo
    let mut tempo = hero(thousands(now as u64), "Wallets pro Sekunde", C_PRIMARY);
    tempo.push(kv(
        "Durchschnitt",
        format!("{:>10} /s", thousands(avg as u64)),
        C_TEXT,
    ));
    tempo.push(kv(
        "Spitze",
        format!("{:>10} /s", thousands(app.peak as u64)),
        C_TEXT,
    ));
    tempo.push(kv(
        "Pro Kern",
        format!(
            "{:>10} /s ×{}",
            thousands((avg / app.threads.max(1) as f64) as u64),
            app.threads
        ),
        C_DIM,
    ));
    f.render_widget(
        Paragraph::new(tempo).block(frame_block("TEMPO", C_PRIMARY)),
        cols[0],
    );

    // --- Geprüft
    let mut done = hero(thousands(seeds), "Wallets seit dem Start", C_TEXT);
    done.push(kv(
        "Adressen",
        format!("{:>13}", thousands(addresses)),
        C_TEXT,
    ));
    done.push(kv(
        "Laufzeit",
        format!(
            "{:>13}",
            crate::util::format_duration(app.rate.elapsed().as_secs())
        ),
        C_TEXT,
    ));
    done.push(kv(
        "Fehlalarme",
        format!("{:>13}", thousands(app.stats.bloom_hits())),
        C_DIM,
    ));
    f.render_widget(
        Paragraph::new(done).block(frame_block("GEPRÜFT", C_PRIMARY)),
        cols[1],
    );

    // --- Suchraum
    let frac = app.keyspace_fraction(seeds) * 100.0;
    let mut space = hero(
        format!("{} %", sci(frac)),
        "des Suchraums abgesucht",
        C_WARN,
    );
    space.push(kv(
        "Suchraum",
        format!("{:>13}", format!("2^{}", app.entropy_bits)),
        C_DIM,
    ));
    space.push(kv(
        "Datenbank",
        format!("{:>13}", thousands(app.funded_count)),
        C_DIM,
    ));
    space.push(kv(
        "Speicher",
        format!(
            "{:>4.0} MB + {:.0} MB",
            app.bloom_bytes as f64 / 1e6,
            app.db_bytes as f64 / 1e6
        ),
        C_DIM,
    ));
    f.render_widget(
        Paragraph::new(space).block(frame_block("SUCHRAUM", C_PRIMARY)),
        cols[2],
    );
}

fn draw_sparkline(f: &mut Frame, area: Rect, app: &App) {
    f.render_widget(
        Sparkline::default()
            .block(frame_block("TEMPO-VERLAUF", C_PRIMARY))
            .data(app.rate.history())
            .style(Style::default().fg(C_PRIMARY)),
        area,
    );
}

/// The mandatory line, and the only part of the screen that answers the
/// question the program exists to answer.
fn draw_verdict(f: &mut Frame, area: Rect, app: &App, rate: f64, roomy: bool) {
    let ages = app.universe_ages_to_hit(rate);
    let expected = app.expected_seeds();
    let seconds = if rate > 0.0 {
        expected / rate
    } else {
        f64::INFINITY
    };

    let headline = Line::from(vec![
        Span::styled(
            german_scale(ages),
            Style::default().fg(C_ALERT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  ×  das Alter des Universums",
            Style::default().fg(C_ALERT).add_modifier(Modifier::BOLD),
        ),
    ]);

    let expert = Line::from(Span::styled(
        format!(
            "Fachwerte:  {} Seeds erwartet  ·  {} s  ·  p = {} pro Seed  ·  {} Adressen/Seed",
            sci(expected),
            sci(seconds),
            sci(1.0 / expected),
            app.addresses_per_seed
        ),
        Style::default().fg(C_DIM),
    ));

    let mut lines = vec![headline];

    if roomy {
        // Share of the work one full age of the universe would complete.
        let after_one_age = 100.0 / ages;
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Selbst wenn dieser Mac seit dem Urknall durchgehend rechnete, hätte er erst {} % davon geschafft.",
                sci(after_one_age)
            ),
            Style::default().fg(C_TEXT),
        )));
        lines.push(Line::from(Span::styled(
            "Das ist kein Fehler — es ist das Ergebnis. Es zeigt, warum Bitcoin-Wallets sicher sind.",
            Style::default().fg(C_TEXT),
        )));
        lines.push(Line::from(""));
    }
    lines.push(expert);

    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(frame_block("WIE LANGE BIS ZU EINEM TREFFER?", C_ALERT)),
        area,
    );
}

fn draw_hits(f: &mut Frame, area: Rect, app: &mut App) {
    app.list_area = area;

    if app.hits.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Noch kein Treffer.",
                    Style::default().fg(C_DIM),
                )),
                Line::from(Span::styled(
                    "  Das ist der erwartete Zustand — siehe oben.",
                    Style::default().fg(C_DIM).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(frame_block("TREFFER", C_PRIMARY)),
            area,
        );
        return;
    }

    let real = app.hits.iter().filter(|h| !h.is_synthetic()).count();
    let tests = app.hits.len() - real;
    let title = if tests > 0 {
        format!("TREFFER [{real}] · {tests} Testeintrag — anklicken zeigt den Seed")
    } else {
        format!("TREFFER [{real}] — anklicken zeigt den Seed")
    };
    let items: Vec<ListItem> = app
        .hits
        .iter()
        .map(|h| {
            let synthetic = h.is_synthetic();
            ListItem::new(Line::from(vec![
                Span::styled(
                    if synthetic { " TEST " } else { " ● " },
                    Style::default().fg(if synthetic { C_MUTED } else { C_ALERT }),
                ),
                Span::styled(
                    format!("{:>13} ", h.balance_btc.trim_end_matches(" BTC")),
                    Style::default()
                        .fg(if synthetic { C_MUTED } else { C_WARN })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<22} ", truncate(&h.address, 22)),
                    Style::default().fg(C_TEXT),
                ),
                Span::styled(h.derivation_path.clone(), Style::default().fg(C_DIM)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(frame_block(&title, C_ALERT))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(38, 30, 44))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▎");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n.saturating_sub(3)).collect();
        format!("{head}...")
    }
}

fn draw_seed(f: &mut Frame, area: Rect, app: &App) {
    let Some(hit) = app.selected.and_then(|i| app.hits.get(i)) else {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Sobald ein Treffer da ist, stehen hier",
                    Style::default().fg(C_DIM),
                )),
                Line::from(Span::styled(
                    "  seine Wörter im Klartext.",
                    Style::default().fg(C_DIM),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Sie verlassen diesen Rechner nie.",
                    Style::default().fg(C_DIM).add_modifier(Modifier::ITALIC),
                )),
            ])
            .block(frame_block("SEED — NUR HIER LOKAL", C_DIM)),
            area,
        );
        return;
    };

    let mut lines = Vec::new();
    if hit.is_synthetic() {
        lines.push(Line::from(Span::styled(
            " TESTEINTRAG — kein echter Fund.",
            Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            " Aus dem Selbsttest; die Wörter sind das öffentliche BIP-39-Beispiel.",
            Style::default().fg(C_MUTED),
        )));
        lines.push(Line::from(""));
    }
    lines.extend(vec![
        Line::from(vec![
            Span::styled(" Adresse  ", Style::default().fg(C_DIM)),
            Span::styled(hit.address.clone(), Style::default().fg(C_TEXT)),
        ]),
        Line::from(vec![
            Span::styled(" Guthaben ", Style::default().fg(C_DIM)),
            Span::styled(
                hit.balance_btc.clone(),
                Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Pfad     ", Style::default().fg(C_DIM)),
            Span::styled(hit.derivation_path.clone(), Style::default().fg(C_DIM)),
        ]),
        Line::from(""),
    ]);

    // The mnemonic, numbered, four to a row.
    let words: Vec<&str> = hit.mnemonic.split_whitespace().collect();
    for (row, chunk) in words.chunks(4).enumerate() {
        let mut spans = vec![Span::raw(" ")];
        for (j, w) in chunk.iter().enumerate() {
            spans.push(Span::styled(
                format!("{:>2}.", row * 4 + j + 1),
                Style::default().fg(C_MUTED),
            ));
            spans.push(Span::styled(
                format!("{w:<10}"),
                Style::default().fg(C_WARN).add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Gespeichert in hits.jsonl, nur für dich lesbar.",
        Style::default().fg(C_DIM).add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(Span::styled(
        " Wird an keinen Benachrichtigungsdienst gesendet.",
        Style::default().fg(C_DIM).add_modifier(Modifier::ITALIC),
    )));

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(frame_block("SEED — NUR HIER LOKAL", C_ALERT)),
        area,
    );
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if app.errors.is_empty() {
        let key = |k: &'static str, fg: Color| {
            Span::styled(
                format!(" {k} "),
                Style::default()
                    .fg(fg)
                    .bg(C_PANEL)
                    .add_modifier(Modifier::BOLD),
            )
        };
        let txt = |t: &'static str| Span::styled(t, Style::default().fg(C_MUTED));

        let start_stop = if app.control.paused() {
            " starten   "
        } else {
            " anhalten   "
        };

        let width = |v: &[Span]| -> usize { v.iter().map(|s| s.content.chars().count()).sum() };

        let full = vec![
            Span::raw(" "),
            key("Leertaste", C_WARN),
            txt(start_stop),
            key("↑↓", C_PRIMARY),
            txt(" Treffer wählen   "),
            key("Klick", C_PRIMARY),
            txt(" Seed anzeigen   "),
            key("q", C_PRIMARY),
            txt(" beenden"),
        ];

        // A truncated hint row is worse than a shorter one: "beenden" is the
        // hint a novice most needs, and it sits at the end. So drop detail in
        // tiers and take the first that fits rather than letting the row run
        // off the edge.
        let tiers: Vec<Vec<Span>> = vec![
            full,
            vec![
                Span::raw(" "),
                key("Leertaste", C_WARN),
                txt(" start/stopp  "),
                key("↑↓", C_PRIMARY),
                txt(" wählen  "),
                key("q", C_PRIMARY),
                txt(" beenden"),
            ],
            vec![
                Span::raw(" "),
                key("Leertaste", C_WARN),
                txt(" stopp  "),
                key("q", C_PRIMARY),
                txt(" beenden"),
            ],
            vec![Span::raw(" "), key("q", C_PRIMARY), txt(" beenden")],
        ];

        let mut spans = Vec::new();
        for tier in tiers {
            let fits = width(&tier) <= area.width as usize;
            spans = tier;
            if fits {
                break;
            }
        }

        // Push the handle to the right edge. Dropped rather than wrapped when
        // the terminal is too narrow, so it can never displace the key hints.
        let used = width(&spans);
        let need = HANDLE.chars().count() + 2;
        if area.width as usize > used + need {
            spans.push(Span::raw(" ".repeat(area.width as usize - used - need)));
            spans.push(Span::styled(HANDLE, Style::default().fg(C_ACCENT)));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let lines: Vec<Line> = app
        .errors
        .iter()
        .rev()
        .take(3)
        .map(|e| {
            Line::from(Span::styled(
                format!("  {e}"),
                Style::default().fg(C_ALERT).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).block(frame_block("FEHLER", C_ALERT)),
        area,
    );
}

/// Prints a hit to stdout for headless runs, where there is no TUI to pin it to.
pub fn print_hit_plain(hit: &Hit) {
    let mut out = io::stdout();
    let _ = writeln!(out, "\n\x1b[1;31m{}\x1b[0m", "=".repeat(72));
    let _ = writeln!(out, "\x1b[1;31mFUNDED SEED FOUND\x1b[0m");
    let _ = writeln!(out, "  address : {}", hit.address);
    let _ = writeln!(out, "  balance : {}", hit.balance_btc);
    let _ = writeln!(out, "  path    : {}", hit.derivation_path);
    let _ = writeln!(out, "  mnemonic: \x1b[1;33m{}\x1b[0m", hit.mnemonic);
    let _ = writeln!(out, "\x1b[1;31m{}\x1b[0m\n", "=".repeat(72));
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn app_with(funded: u64, per_seed: u32, bits: u32) -> App {
        let (_tx, rx) = channel();
        App::new(
            Arc::new(Stats::new()),
            Arc::new(Control::new(8, 20, crate::stats::Priority::Normal)),
            rx,
            Vec::new(),
            funded,
            per_seed,
            bits,
            8,
            0,
            0,
        )
    }

    /// The headline number must be astronomically large for realistic inputs.
    /// If a refactor ever makes this look achievable, something is wrong.
    #[test]
    fn expected_time_is_astronomical() {
        let app = app_with(50_000_000, 60, 256);
        let expected = app.expected_seeds();
        // 2^160 / (60 * 5e7) is on the order of 1e39.
        assert!(
            expected > 1e38 && expected < 1e40,
            "implausible expectation {expected:e}"
        );

        // Even at a wildly optimistic billion seeds per second.
        let ages = app.universe_ages_to_hit(1e9);
        assert!(ages > 1e10, "universe ages {ages:e} is suspiciously small");
    }

    #[test]
    fn keyspace_fraction_is_effectively_zero() {
        let app = app_with(50_000_000, 60, 256);
        // A trillion seeds against 2^256.
        let f = app.keyspace_fraction(1_000_000_000_000);
        assert!(f < 1e-60, "fraction {f:e} is implausibly large");
        assert_eq!(&sci(f)[..1], "8", "sanity: 1e12 / 2^256 ≈ 8.6e-66");
    }

    #[test]
    fn twelve_word_keyspace_is_smaller_but_still_hopeless() {
        let app = app_with(50_000_000, 60, 128);
        let f = app.keyspace_fraction(1_000_000_000_000);
        assert!(f < 1e-25 && f > 1e-30);
    }

    #[test]
    fn zero_rate_does_not_divide_by_zero() {
        let app = app_with(50_000_000, 60, 256);
        assert!(app.universe_ages_to_hit(0.0).is_infinite());
        assert_eq!(sci(f64::INFINITY), "unendlich");
    }

    #[test]
    fn empty_funded_set_is_infinite_not_nan() {
        let app = app_with(0, 60, 256);
        assert!(app.expected_seeds().is_infinite());
    }

    #[test]
    fn click_outside_the_list_selects_nothing() {
        let mut app = app_with(1, 60, 256);
        app.hits.push(Hit::synthetic());
        app.list_area = Rect::new(10, 10, 40, 5);
        app.click(0, 0);
        assert_eq!(app.selected, None);
        app.click(12, 11); // first row inside the border
        assert_eq!(app.selected, Some(0));
    }

    #[test]
    fn clicking_the_button_toggles_the_run_state() {
        let mut app = app_with(1, 60, 256);
        app.button_area = Rect::new(50, 1, 13, 1);
        assert!(!app.paused());
        app.click(55, 1);
        assert!(app.paused(), "button click must stop the search");
        app.click(55, 1);
        assert!(!app.paused(), "second click must resume");
    }

    /// A zero-sized button rect (terminal too narrow) must not swallow clicks
    /// meant for the hit list.
    #[test]
    fn empty_button_rect_is_never_hit() {
        let mut app = app_with(1, 60, 256);
        app.hits.push(Hit::synthetic());
        app.button_area = Rect::default();
        app.list_area = Rect::new(0, 0, 40, 5);
        app.click(0, 0);
        assert!(!app.paused());
    }

    #[test]
    fn thousands_formatting() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1 000");
        assert_eq!(thousands(1_234_567), "1 234 567");
    }

    /// Renders a frame off-screen and returns it as plain text.
    fn render(app: &mut App, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();

        let buf = t.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// The mandatory panel must reach the screen, in words and in figures.
    #[test]
    fn renders_the_verdict_panel() {
        let mut app = app_with(50_000_000, 60, 256);
        let screen = render(&mut app, 130, 34);
        assert!(
            screen.contains("WIE LANGE BIS ZU EINEM TREFFER?"),
            "verdict panel missing:\n{screen}"
        );
        assert!(
            screen.contains("Alter des Universums"),
            "universe comparison missing:\n{screen}"
        );
        // The plain-language explanation and the expert figures must coexist.
        assert!(
            screen.contains("Urknall"),
            "plain-language explanation missing:\n{screen}"
        );
        assert!(
            screen.contains("Fachwerte"),
            "expert figures missing:\n{screen}"
        );
        assert!(screen.contains("TEMPO"));
        assert!(screen.contains("SUCHRAUM"));
        assert!(screen.contains("GEPR"));
    }

    /// Every headline number needs its plain-language caption, or the panels
    /// are just unlabelled digits again.
    #[test]
    fn every_panel_has_a_plain_caption() {
        let mut app = app_with(50_000_000, 60, 256);
        let screen = render(&mut app, 130, 34);
        for caption in [
            "Wallets pro Sekunde",
            "Wallets seit dem Start",
            "des Suchraums abgesucht",
        ] {
            assert!(
                screen.contains(caption),
                "caption {caption:?} missing:\n{screen}"
            );
        }
    }

    /// The expert rows must survive alongside the explanations.
    #[test]
    fn expert_figures_are_still_present() {
        let mut app = app_with(50_000_000, 60, 256);
        let screen = render(&mut app, 130, 34);
        for label in [
            "Durchschnitt",
            "Spitze",
            "Pro Kern", // throughput detail
            "Adressen",
            "Laufzeit",
            "Fehlalarme", // progress detail
            "Suchraum",
            "Datenbank",
            "Speicher", // lookup detail
        ] {
            assert!(
                screen.contains(label),
                "expert row {label:?} missing:\n{screen}"
            );
        }
    }

    /// The button must render, and must swap label with the run state.
    #[test]
    fn renders_start_stop_button() {
        let mut app = app_with(1_000, 60, 256);

        let running = render(&mut app, 130, 34);
        assert!(
            running.contains("STOPP"),
            "STOPP button missing:\n{running}"
        );
        assert!(running.contains("LÄUFT"));

        app.control.set_paused(true);
        let paused = render(&mut app, 130, 34);
        assert!(paused.contains("START"), "START button missing:\n{paused}");
        assert!(paused.contains("ANGEHALTEN"));
        // Stopping is the power-saving state, and the header should say so.
        assert!(
            paused.contains("Strom"),
            "paused state should explain the power saving:\n{paused}"
        );
    }

    /// A rendered frame must place the button rect where clicks are routed.
    #[test]
    fn button_rect_matches_rendered_position() {
        let mut app = app_with(1_000, 60, 256);
        render(&mut app, 130, 34);
        assert!(app.button_area.width > 0, "button rect was never set");

        assert!(!app.paused());
        let (cx, cy) = (
            app.button_area.x + app.button_area.width / 2,
            app.button_area.y,
        );
        app.click(cx, cy);
        assert!(
            app.paused(),
            "click at the rendered button did not register"
        );
    }

    /// Selecting a hit must reveal its words, numbered.
    /// A self-test artefact must never present as a find. This is the bug that
    /// briefly convinced a user they had struck gold.
    #[test]
    fn synthetic_hits_are_labelled_not_celebrated() {
        let mut app = app_with(1_000, 60, 256);
        app.hits.push(Hit::synthetic());
        app.selected = Some(0);
        app.list_state.select(Some(0));

        let screen = render(&mut app, 130, 34);
        assert!(
            screen.contains("TREFFER [0]"),
            "a test entry must not raise the hit count:\n{screen}"
        );
        assert!(
            screen.contains("Testeintrag"),
            "the entry must be named as a test:\n{screen}"
        );
        assert!(
            screen.contains("TESTEINTRAG"),
            "the seed panel must say so too:\n{screen}"
        );
    }

    #[test]
    fn real_hits_still_count() {
        let mut app = app_with(1_000, 60, 256);
        let mut real = Hit::synthetic();
        real.entropy_hex = "a3f1".repeat(8);
        real.private_key_wif = "L1Real".into();
        assert!(!real.is_synthetic());
        app.hits.push(real);
        app.selected = Some(0);
        app.list_state.select(Some(0));

        let screen = render(&mut app, 130, 34);
        assert!(
            screen.contains("TREFFER [1]"),
            "a real hit must count:\n{screen}"
        );
        assert!(!screen.contains("TESTEINTRAG"));
    }

    #[test]
    fn renders_mnemonic_words_for_selected_hit() {
        let mut app = app_with(1_000, 60, 256);
        // A real hit, so the panel is not taken up by the test-entry warning.
        let mut hit = Hit::synthetic();
        hit.entropy_hex = "a3f1".repeat(8);
        hit.private_key_wif = "L1Real".into();
        app.hits.push(hit);
        app.selected = Some(0);
        app.list_state.select(Some(0));

        let screen = render(&mut app, 130, 34);
        assert!(screen.contains("SEED"), "seed panel missing:\n{screen}");
        assert!(screen.contains("NUR HIER LOKAL"), "locality note missing");
        assert!(screen.contains("abandon"), "words missing:\n{screen}");
        assert!(screen.contains("about"), "last word missing:\n{screen}");
        assert!(screen.contains(" 1."), "word numbering missing:\n{screen}");
    }

    /// The headline figure is useless as an exponent; the word form is what a
    /// non-specialist can actually place.
    #[test]
    fn german_long_scale_naming() {
        assert_eq!(german_scale(5.7e18), "5,7 Trillionen");
        assert_eq!(german_scale(1.0e9), "1,0 Milliarde");
        assert_eq!(german_scale(2.5e12), "2,5 Billionen");
        assert_eq!(german_scale(3.0e6), "3,0 Millionen");
        // Below a million, plain digit grouping reads better than words.
        assert_eq!(german_scale(1234.0), "1 234");
        // Past the named scales, fall back rather than invent a word.
        assert!(german_scale(1e40).starts_with("10 hoch"));
        assert_eq!(german_scale(f64::INFINITY), "unendlich");
    }

    #[test]
    fn german_decimals_use_a_comma() {
        assert_eq!(de(5.75, 1), "5,8");
        assert_eq!(de(1.0, 2), "1,00");
    }

    /// Dumps a representative frame as TSV (char, fg, bg, bold) so it can be
    /// rendered to an image outside the terminal. Run with:
    /// `cargo test --release dump_demo_frame -- --ignored --nocapture`
    /// Dumps the final intro frame instead of the main screen when
    /// SC_DUMP_INTRO is set.
    #[test]
    #[ignore = "writes a frame dump for external rendering"]
    fn dump_demo_frame() {
        use ratatui::backend::TestBackend;

        fn rgb(c: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
            match c {
                Color::Rgb(r, g, b) => (r, g, b),
                Color::Black => (0, 0, 0),
                _ => fallback,
            }
        }

        let mut app = demo_app();
        let (w, h) = (124u16, 40u16);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        if std::env::var("SC_DUMP_INTRO").is_ok() {
            t.draw(|f| draw_intro(f, INTRO_FRAMES - 1, 52_000_000, 8))
                .unwrap();
        } else {
            t.draw(|f| draw(f, &mut app)).unwrap();
        }
        let buf = t.backend().buffer();

        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                let cell = &buf[(x, y)];
                let f = rgb(cell.fg, (192, 202, 245));
                let b = rgb(cell.bg, (13, 15, 22));
                let bold = cell.modifier.contains(Modifier::BOLD) as u8;
                out.push_str(&format!(
                    "{}\t{},{},{}\t{},{},{}\t{}\n",
                    cell.symbol(),
                    f.0,
                    f.1,
                    f.2,
                    b.0,
                    b.1,
                    b.2,
                    bold
                ));
            }
        }
        std::fs::write("/tmp/frame.tsv", out).unwrap();
        println!("wrote /tmp/frame.tsv ({w}x{h})");
    }

    /// Prints a representative frame. Run with:
    /// `cargo test --release print_demo_frame -- --ignored --nocapture`
    #[test]
    #[ignore = "prints a layout preview rather than asserting"]
    fn print_demo_frame() {
        let mut app = demo_app();
        // The launcher sizes the window to 124x40; preview at that size.
        println!("\n{}", render(&mut app, 124, 40));
    }

    /// Representative state for the layout previews.
    fn demo_app() -> App {
        let mut app = app_with(52_000_000, 60, 256);
        app.hits.push(Hit::synthetic());
        let mut second = Hit::synthetic();
        second.address = "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA".into();
        second.derivation_path = "m/44'/0'/0'/0/7".into();
        second.balance_btc = crate::util::format_btc(4_206_900_000);
        second.id = Hit::make_id(&second.address, &second.derivation_path);
        app.hits.push(second);
        app.selected = Some(0);
        app.list_state.select(Some(0));
        app.peak = 2_140.0;
        for v in [
            1900u64, 1980, 2010, 1950, 2030, 1990, 2005, 1975, 2040, 1960,
        ] {
            app.rate.history.push(v);
        }
        app.rate.ewma = 1987.0;
        app.bloom_bytes = 24_500_000;
        app.db_bytes = 145_000_016;
        app.stats.note_bloom_hit();
        app.stats.note_bloom_hit();
        let mut counters = crate::stats::Local {
            seeds: 30_208,
            addresses: 1_812_480,
        };
        counters.flush(&app.stats);
        app
    }

    /// The intro must build up and land on a complete final frame.
    #[test]
    fn intro_fades_in_and_completes() {
        use ratatui::backend::TestBackend;

        let frame_text = |n: u32| {
            let mut t = Terminal::new(TestBackend::new(80, 30)).unwrap();
            t.draw(|f| draw_intro(f, n, 52_000_000, 8)).unwrap();
            let buf = t.backend().buffer();
            let mut out = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    out.push_str(buf[(x, y)].symbol());
                }
                out.push('\n');
            }
            out
        };

        let last = frame_text(INTRO_FRAMES - 1);
        assert!(
            last.contains("S C H A T Z S U C H E"),
            "wordmark missing:\n{last}"
        );
        assert!(last.contains(HANDLE), "handle missing:\n{last}");
        assert!(
            last.contains("52 000 000 Adressen"),
            "tagline missing:\n{last}"
        );
        assert!(last.contains("█"), "logo missing:\n{last}");

        // Every frame must be drawable, including the first.
        for n in 0..INTRO_FRAMES {
            let _ = frame_text(n);
        }
    }

    /// Elements start invisible and are fully lit by the end, or the fade is
    /// not actually happening.
    #[test]
    fn intro_stages_are_staggered() {
        assert_eq!(stage(0, 0), 0.0);
        assert_eq!(stage(9, 0), 1.0);
        assert_eq!(stage(0, 19), 0.0, "the handle must not be lit at frame 0");
        assert_eq!(stage(INTRO_FRAMES - 1, 19), 1.0, "handle must finish lit");

        // At t=0 an element is indistinguishable from the background.
        assert_eq!(fade(C_ACCENT, 0.0), C_BG);
        assert_eq!(fade(C_ACCENT, 1.0), C_ACCENT);
    }

    #[test]
    fn footer_carries_the_handle() {
        let mut app = app_with(1_000, 60, 256);
        let screen = render(&mut app, 130, 34);
        assert!(
            screen.contains(HANDLE),
            "handle missing from footer:\n{screen}"
        );
    }

    /// A narrow terminal falls back to short hints rather than truncating the
    /// row — "beenden" is the hint a novice most needs to keep.
    #[test]
    fn narrow_footer_keeps_the_quit_hint() {
        for w in [50u16, 64, 78, 90, 130] {
            let mut app = app_with(1_000, 60, 256);
            let screen = render(&mut app, w, 30);
            assert!(
                screen.contains("beenden"),
                "quit hint lost at width {w}:\n{screen}"
            );
        }
    }

    /// The handle appears once there is room for it without crowding the hints.
    #[test]
    fn handle_appears_when_there_is_room() {
        let mut wide = app_with(1_000, 60, 256);
        assert!(render(&mut wide, 130, 34).contains(HANDLE));

        let mut tiny = app_with(1_000, 60, 256);
        let screen = render(&mut tiny, 44, 30);
        assert!(screen.contains("beenden"), "hints must win over the handle");
    }

    /// A narrow terminal must not panic or paint a button nobody can hit.
    #[test]
    fn survives_a_tiny_terminal() {
        let mut app = app_with(1_000, 60, 256);
        app.hits.push(Hit::synthetic());
        for (w, h) in [(20u16, 10u16), (40, 12), (12, 6), (200, 60)] {
            let mut a = app_with(1_000, 60, 256);
            a.hits.push(Hit::synthetic());
            let _ = render(&mut a, w, h);
        }
        let _ = render(&mut app, 24, 8);
    }
}
