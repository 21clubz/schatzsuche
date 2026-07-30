//! Native window interface.
//!
//! Same information as the terminal UI, in a real macOS window. The engine,
//! lookup, persistence and alerting layers are untouched — this module only
//! replaces the presentation, talking to the same [`Stats`], [`Control`] and
//! event channel the TUI uses.
//!
//! A terminal UI needs a terminal: the window is its drawing surface. This one
//! draws its own, so the app launches straight from the Finder.
//!
//! Farben, Abstände und Größen stehen hier nicht mehr: sie kommen aus
//! [`crate::ui::theme`], die wiederkehrenden Bausteine aus
//! [`crate::ui::widgets`], die Zahlenformatierung aus [`crate::ui::format`] und
//! die Frage „welcher Bildschirm?" aus [`crate::ui::screen`].

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, FontId, Layout, Pos2, RichText, Sense, Stroke, TextureHandle, Ui, Vec2,
};

use crate::config::physical_cores;
use crate::engine::Event;
use crate::hits::Hit;
use crate::startup::Progress;
use crate::stats::{Control, Priority, Rate, Stats};
use crate::tui::{expected_seeds_to_hit, universe_ages_to_hit, HANDLE, HANDLE_URL};
use crate::ui::screen::Screen;
use crate::ui::theme::{self, mono, pal};
use crate::ui::{format, widgets};
use crate::util;

// Weitergereicht, damit `startup` und die Tests die vertrauten Namen behalten.
pub use crate::ui::widgets::{columns_that_fit, CARD_CHROME_H, MIN_STAT_CARD, MIN_WIDE_CARD};

/// Ein echter Fund, den der Leser noch nicht zur Kenntnis genommen hat.
///
/// Absichtlich **kein** [`Screen`]: ein Fund kann eintreten, während jemand im
/// Wiederherstellungs-Assistenten steht oder in den Einstellungen blättert. Als
/// Bildschirmvariante modelliert müsste er das halb ausgefüllte Wortformular
/// wegwerfen oder verschachteln — genau die unmöglichen Kombinationen, die
/// [`crate::ui::screen`] beseitigt hat. Ein Fund ist quer zum Bildschirm, also
/// steht er auch quer daneben.
#[derive(Debug, Clone, Copy)]
struct Pending {
    /// Der jüngste echte Fund, als Platz in [`GuiApp::hits`].
    newest: usize,
    /// Wie viele seit dem letzten „Verstanden" dazugekommen sind.
    count: usize,
    /// Ob Dock und Ton für diesen Fund schon gerufen wurden.
    announced: bool,
}

/// Die Maße, die der Aufrufer für die Szene ausgerechnet hat.
///
/// Zusammengefasst, weil sie zusammengehören und einzeln übergeben nur die
/// Parameterliste verlängerten: erst dort oben ist bekannt, wie viel Höhe die
/// ganze Spalte zur Verfügung hat.
#[derive(Copy, Clone, Debug)]
struct SceneLayout {
    /// Höhe des Truhenstreifens.
    chest_h: f32,
    /// Abstand darüber, der die Szene senkrecht mittig setzt.
    pad: f32,
}

/// Was die Szene außer der Truhe senkrecht braucht.
///
/// Zahl, Bildunterschrift, Zählerzeile, der große Knopf, das Urteil, die
/// Wallet-Zeile und die Abstände dazwischen — zusammen rund so viel. Der Rest
/// der Höhe gehört der Truhe, die deshalb mitwächst und mitschrumpft, statt
/// dass unten etwas herausfällt.
///
/// Gemessen, nicht geraten: bei 640 Punkten Fensterhöhe bleiben damit noch
/// rund 150 für das Bild, und die Seite passt ohne Rollbalken.
const SCENE_FIXED_H: f32 = 390.0;

/// Breite der rechten Spalte — für die Details wie für die Einstellungen.
///
/// Dieselbe Zahl für beide, damit die Mitte nicht springt, wenn zwischen ihnen
/// gewechselt wird. Im kleinsten erlaubten Fenster (900 Punkte) bleiben damit
/// 560 für die Szene übrig, und die kommt mit 508 aus.
const DETAIL_W: f32 = 340.0;

/// Wie lange der Vorhang zwischen Laden und Oberfläche liegt.
///
/// Vorher waren es 3450 ms mit einem Countdown und einem Fortschrittsbalken —
/// auf einem Bildschirm, auf dem nachweislich nichts gewartet wird. Das ist die
/// Bildsprache von „gleich passiert etwas", also genau das Gegenteil der
/// Aussage dieses Programms, und es kostete bei jedem Start dreieinhalb
/// Sekunden. Geblieben ist ein kurzes Aufblenden, das man überspringen kann.
const INTRO: Duration = Duration::from_millis(900);

pub struct GuiApp {
    stats: Arc<Stats>,
    control: Arc<Control>,
    events: Receiver<Event>,
    hits: Vec<Hit>,
    selected: Option<usize>,
    rate: Rate,
    peak: f64,
    last_sample: Instant,
    /// Wann das Fenster aufging. Für den Screenshot-Auslöser und das Atmen des
    /// Ladebildschirms — nicht mehr für die Bildschirmauswahl.
    started: Instant,
    funded_count: u64,
    addresses_per_seed: u32,
    threads: usize,
    bloom_bytes: usize,
    db_bytes: usize,

    /// Der eine Zustand, der bestimmt, was im Fenster steht.
    ///
    /// Vorher waren das fünf unabhängige Felder plus eine Uhr, aufgelöst von der
    /// Reihenfolge einer if/else-Kette. Unmögliche Kombinationen — Gabelung und
    /// Assistent gleichzeitig — waren darstellbar; hier sind sie es nicht mehr.
    screen: Screen,

    /// Meldungen, die der Leser sehen muss: ein Treffer, der nicht gespeichert
    /// werden konnte, oder eine fehlgeschlagene Sicherungskopie.
    ///
    /// Wurden vorher gesammelt und nie gezeichnet. Damit war der eine
    /// Fehlerfall, den `engine.rs` ausdrücklich als „darf niemals verschluckt
    /// werden" bezeichnet, im Fenster unsichtbar — und der Treffer stand
    /// daneben in der Liste, als wäre er sicher.
    errors: Vec<String>,
    /// Die Kennungen der Treffer, die es nicht auf die Platte geschafft haben.
    unsaved: std::collections::HashSet<String>,
    /// Wo die Trefferdatei liegt, für das Fehlerband und den Daten-Abschnitt.
    hits_path: std::path::PathBuf,
    /// Wo die Adressdatenbank liegt.
    db_path: std::path::PathBuf,
    /// Wohin die Kontostandsabfrage geht, wenn jemand sie auslöst.
    balance_api: String,
    /// Ob die geladene Adressliste eine selbst erzeugte Übungsliste ist.
    practice_list: bool,
    /// Wo die Einstellungsdatei liegt — zum Speichern der Meldewege.
    config_path: std::path::PathBuf,
    /// Die Meldewege, wie sie im Fenster bearbeitet werden.
    ///
    /// Eine eigene Kopie und nicht die laufende Einstellung: Getipptes soll
    /// erst wirken, wenn jemand auf Speichern drückt, und ein halb
    /// eingetragener Bot-Token ist keine Einstellung, sondern ein Zwischenstand.
    alerts: crate::config::Alerts,
    /// Was beim letzten Speichern herauskam. `Ok` die Bestätigung, `Err` der
    /// Grund — beides gehört unter die Knöpfe und nicht in eine Protokollzeile,
    /// die niemand sieht.
    alerts_note: Option<Result<String, String>>,

    logo: Option<TextureHandle>,
    /// The map and the key on the opening fork, uploaded on first sight of
    /// that screen and not before: most runs never open it twice, and a run
    /// that goes straight to the dashboard never needs them at all.
    doors: Option<(TextureHandle, TextureHandle)>,
    /// Die Holzfaserung, einmal hochgeladen und dann behalten.
    wood: Option<TextureHandle>,
    /// Die Seekarte hinter der Gabelung — wie die Türbilder erst beim ersten
    /// Anblick dieses Bildschirms hochgeladen.
    map: Option<TextureHandle>,
    /// Set once the screenshot mode has captured its frame.
    shot_path: Option<std::path::PathBuf>,
    shot_at: Option<Instant>,
    /// The loader's progress handle, kept past the end of loading so a second
    /// attempt can report through the same screen.
    progress: Option<Arc<Progress>>,
    settings_open: bool,
    /// Which preset has its explanation open, if any.
    info_open: Option<usize>,
    /// Expert controls stay locked until the warning has been acknowledged.
    expert_unlocked: bool,
    expert_prompt: bool,
    /// Runs the whole load again. Held so the error screen can offer a second
    /// attempt after it has built the database that was missing.
    boot: Option<BootFn>,
    /// True from the click until loading takes over, so the offer cannot be
    /// accepted twice and start two engines.
    repairing: bool,
    /// Wie viele Datensätze die Übungsliste bekommt.
    ///
    /// Ein Feld statt einer Konstanten, damit die Tests nicht 145 MB schreiben
    /// müssen, um eine Verzweigung zu prüfen — sie liefen deshalb in ihr
    /// Zeitlimit.
    practice_records: usize,
    /// Ein echter Fund, der noch nicht zur Kenntnis genommen wurde.
    ///
    /// Wird ausschließlich in [`GuiApp::drain`] gesetzt und ist im Konstruktor
    /// immer `None` — sonst würde ein Treffer, der beim Start aus `hits.jsonl`
    /// nachgeladen wird, jedes Mal aufs Neue Alarm schlagen.
    pending: Option<Pending>,
    /// Ob das Ergebnis des Ladethreads noch abzuholen ist.
    loading_pending: bool,
    /// Filled by the loader; handed to the recovery screen when it opens.
    hunt: HuntSlot,
}

/// The loading routine, callable again after a failure. See the error screen.
pub type BootFn = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// Where the loader leaves the funded set once the search is running.
///
/// The window opens before the database is read, so it cannot be handed over
/// at construction. The recovery screen picks it up when it opens; if loading
/// has not finished yet the slot is empty and that screen says so.
pub type HuntSlot = Arc<std::sync::Mutex<Option<Arc<crate::engine::Shared>>>>;

/// Wo die Dinge liegen, die das Fenster benennen oder öffnen muss.
///
/// Zusammengefasst weitergereicht statt als fünf weitere Parameter. Das Fenster
/// braucht die Pfade an drei Stellen: im Fehlerband, im Daten-Abschnitt der
/// Einstellungen und für den Knopf, der sie im Finder zeigt. `balance_api` ist
/// kein Pfad, gehört aber aus demselben Grund hierher: es ist ein Ort, den das
/// Fenster nennen und auf Knopfdruck ansprechen muss.
#[derive(Debug, Clone)]
pub struct Paths {
    pub hits: std::path::PathBuf,
    pub backup: Option<std::path::PathBuf>,
    pub database: std::path::PathBuf,
    pub config: std::path::PathBuf,
    /// Esplora-Schnittstelle für die Kontostandsabfrage, aus `[balance] api`.
    pub balance_api: String,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            hits: "hits.txt".into(),
            backup: Some("hits_backup.txt".into()),
            database: "funded.scdb".into(),
            config: "config.toml".into(),
            balance_api: crate::config::Balance::default().api,
        }
    }
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
        threads: usize,
        bloom_bytes: usize,
        db_bytes: usize,
        shot_path: Option<std::path::PathBuf>,
        loading: Option<Arc<Progress>>,
        boot: Option<BootFn>,
        hunt: HuntSlot,
        paths: Paths,
    ) -> GuiApp {
        // Ein Screenshot-Lauf darf den Bildschirm vorgeben; sonst beginnt jeder
        // Start beim Laden. Einmal hier gelesen statt in jedem Bild mitten in
        // der Bildschirmauswahl.
        //
        // Ohne ausdrücklichen Schalter springt ein Screenshot-Lauf aufs
        // Dashboard: die vorhandenen Aufnahmen zeigen genau das, und ein Lauf,
        // der auf der Gabelung stehen bleibt, fotografiert das falsche Bild.
        let screen = crate::ui::screen::screenshot_override().unwrap_or({
            if shot_path.is_some() {
                Screen::Dashboard
            } else if loading.is_some() {
                Screen::Loading
            } else {
                Screen::Chooser
            }
        });
        let loading_pending = loading.is_some();

        GuiApp {
            stats,
            control,
            events,
            hits: existing,
            // Screenshot-Schalter aus derselben Familie: `SC_SHOT_WALLET=0`
            // klappt die erste Wallet auf, damit die Wörter fotografierbar sind.
            selected: std::env::var("SC_SHOT_WALLET")
                .ok()
                .and_then(|v| v.parse().ok()),
            rate: Rate::new(160),
            peak: 0.0,
            last_sample: Instant::now(),
            started: Instant::now(),
            funded_count,
            addresses_per_seed,
            threads,
            bloom_bytes,
            db_bytes,
            screen,
            errors: Vec::new(),
            unsaved: std::collections::HashSet::new(),
            hits_path: paths.hits,
            db_path: paths.database,
            balance_api: paths.balance_api,
            practice_list: false,
            // Von der Platte gelesen und nicht durchgereicht: das Fenster
            // bearbeitet genau die Datei, in die es gleich zurückschreibt.
            // Lässt sie sich nicht lesen, stehen hier die Voreinstellungen —
            // alle Meldewege aus. Eine kaputte Datei kommt hier ohnehin nicht
            // an, weil das Programm damit gar nicht erst startet.
            alerts: crate::config::Config::load_or_default(&paths.config)
                .unwrap_or_default()
                .alerts,
            config_path: paths.config,
            alerts_note: None,
            logo: None,
            doors: None,
            wood: None,
            map: None,
            shot_path,
            shot_at: None,
            progress: loading,
            settings_open: std::env::var("SC_SHOT_SETTINGS").is_ok(),
            // Screenshot hook, in the same family as the two above.
            info_open: std::env::var("SC_SHOT_INFO")
                .ok()
                .and_then(|v| v.parse().ok()),
            // SC_SHOT_DONATE opens the drawer with the expert controls closed,
            // so it is short enough that the donation note at its foot is in
            // frame without scrolling.
            expert_unlocked: std::env::var("SC_SHOT_SETTINGS").is_ok()
                && std::env::var("SC_SHOT_DONATE").is_err(),
            expert_prompt: false,
            boot,
            repairing: false,
            practice_records: crate::startup::PRACTICE_RECORDS,
            // Immer None, egal was in `existing` steht: ein Fund von letzter
            // Woche darf beim Öffnen nicht das Dock zum Hüpfen bringen.
            pending: None,
            loading_pending,
            hunt,
        }
    }

    /// Öffnet den Wiederherstellungs-Assistenten und merkt sich den Rückweg.
    ///
    /// Der Rückweg ist nicht fest das Dashboard: von der Gabelung aus führt er
    /// dorthin, vom Fehlerbildschirm aus aber zurück auf den Fehlerbildschirm —
    /// ein Dashboard ohne geladene Daten wäre dort eine Sackgasse.
    fn open_recover(&mut self, back: Screen) {
        self.screen = Screen::Recover {
            ui: Box::new(crate::recover_ui::RecoverUi::with_hunt(
                self.hunt.lock().ok().and_then(|h| h.clone()),
            )),
            back: Box::new(back),
        };
    }

    /// Geht von der Gabelung auf den Suchbildschirm — und **startet dabei
    /// nichts**.
    ///
    /// Eine eigene Methode für eine Zuweisung, weil genau das die Zusage ist,
    /// die nicht kaputtgehen darf: hier stand einmal ein
    /// `control.set_paused(false)`, und dann zählten die Zahlen schon, bevor
    /// jemand den großen Knopf gedrückt hatte. Rechenzeit und Strom fangen erst
    /// auf Ansage an. `entering_the_search_does_not_start_it` nagelt es fest.
    fn enter_dashboard(&mut self) {
        self.screen = Screen::Dashboard;
    }

    /// Verlässt den Assistenten und kehrt dorthin zurück, wo er geöffnet wurde.
    ///
    /// Der Zustand muss herausgenommen werden, um an `back` heranzukommen; das
    /// Dashboard dazwischen ist ein Platzhalter für genau diese eine Anweisung.
    fn leave_recover(&mut self) {
        if let Screen::Recover { ui, back } = std::mem::replace(&mut self.screen, Screen::Dashboard)
        {
            ui.cancel();
            self.screen = *back;
        }
    }

    /// Nimmt das Ergebnis des Ladethreads im ersten Bild danach entgegen.
    ///
    /// Am eigenen Merker aufgehängt statt am Bildschirm: ein Screenshot-Lauf
    /// steht von Anfang an auf dem Dashboard, und die Zahlen der Datenbank
    /// müssen ihn trotzdem erreichen.
    fn absorb_loading(&mut self) {
        if !self.loading_pending {
            return;
        }
        let Some(p) = self.progress.clone() else {
            self.loading_pending = false;
            return;
        };
        if !p.is_done() {
            return;
        }
        self.loading_pending = false;
        self.repairing = false;

        match p.error() {
            Some(message) => {
                // Eine fehlende Datei ist der eine Fehler, den das Programm
                // selbst beheben kann — und nur, wenn es auch ein zweites Mal
                // laden kann. Eine beschädigte Datei bekommt die Markierung
                // nie: sie zu überschreiben hieße, einen echten Adress-Auszug
                // zu vernichten.
                let repairable = p.missing_db().filter(|_| self.boot.is_some());
                self.screen = Screen::Failed {
                    message,
                    repairable,
                };
            }
            None => {
                self.funded_count = p.funded();
                self.bloom_bytes = p.bloom_bytes();
                self.db_bytes = p.db_bytes();
                // Nur weiterrücken, wenn der Ladebildschirm auch wirklich oben
                // steht — sonst würde ein Screenshot-Lauf vom Dashboard zurück
                // ins Intro geworfen.
                if matches!(self.screen, Screen::Loading) {
                    self.screen = Screen::Intro {
                        until: Instant::now() + INTRO,
                    };
                }
            }
        }
    }

    /// Lässt den Vorhang fallen, sobald seine Zeit um ist. Gibt zurück, ob das
    /// gerade passiert ist. Auf jedem anderen Bildschirm ein Nichtstun.
    fn tick_intro(&mut self) -> bool {
        if let Screen::Intro { until } = self.screen {
            if Instant::now() >= until {
                self.screen = Screen::Chooser;
                return true;
            }
        }
        false
    }

    /// Liest die Zähler und fortschreibt den gleitenden Durchschnitt.
    fn sample_rate(&mut self) {
        self.rate.note_paused(self.control.paused());
        if self.last_sample.elapsed() >= Duration::from_millis(250) && !self.control.paused() {
            let inst = self.rate.sample(self.stats.seeds());
            if inst > self.peak {
                self.peak = inst;
            }
            self.last_sample = Instant::now();
        }
    }

    /// Der Fehlerbildschirm, samt der beiden Auswege, die er anbietet.
    fn draw_failed(&mut self, ctx: &egui::Context) {
        let Screen::Failed {
            message,
            repairable,
        } = &self.screen
        else {
            return;
        };
        let (message, repairable) = (message.clone(), repairable.clone());

        match draw_error_panel(ctx, &message, repairable.as_deref()) {
            ErrorAction::None => {}
            ErrorAction::Build => self.build_practice_db(),
            // Die Wiederherstellung braucht mit Zieladresse gar keine
            // Datenbank. Sie hinter dem Fehlerbildschirm einzusperren hieß, die
            // nützliche Hälfte des Programms genau dann zu verstecken, wenn
            // sicher noch nichts eingerichtet ist — beim allerersten Start.
            ErrorAction::Recover => self.open_recover(Screen::Failed {
                message,
                repairable,
            }),
        }
    }

    /// Keyspace exponent of the mnemonic length currently being drawn.
    ///
    /// Read from the control rather than stored, because the length can be
    /// changed while the search runs and a cached copy would keep advertising
    /// the keyspace of a setting that is no longer in use.
    fn entropy_bits(&self) -> u32 {
        self.control.word_count().entropy_bits()
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
        Self::share_at(threads, physical_cores(), priority)
    }

    /// True when the chosen combination is self-defeating on this machine.
    fn is_counterproductive(&self, threads: usize, priority: Priority) -> bool {
        Self::counterproductive_at(threads, physical_cores(), priority)
    }

    /// The measured throughput curve, for a machine with `max_cores` cores.
    ///
    /// Takes the core count rather than reading it, because a question like "do
    /// more cores mean more throughput" only has an answer once the machine is
    /// named. Reading the count in here made the model untestable: on a
    /// four-core CI runner, four and eight threads both landed at the end of
    /// the curve, came out equal, and the test asserting that the curve rises
    /// failed on hardware that was never the point.
    ///
    /// Not assumed linear, because it is nothing of the sort:
    ///
    /// * Utility priority plateaus near 42% however many cores are given to it —
    ///   macOS keeps that tier off the performance cores.
    /// * Background peaks at six cores and gets *worse* at eight, since the work
    ///   is confined to four efficiency cores that then contend.
    /// * Only Normal scales the way one would naively expect.
    pub(crate) fn share_at(threads: usize, max_cores: usize, priority: Priority) -> f64 {
        // (threads, share of peak), measured on an idle eight-core M1.
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

        // Rescale to the machine's core count so the curve still means something
        // on hardware that is not an eight-core M1.
        let max = max_cores.max(1) as f64;
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

    /// True when the chosen combination is self-defeating: background priority is
    /// confined to the slow cores, so asking for most of the machine there buys
    /// contention rather than throughput.
    pub(crate) fn counterproductive_at(
        threads: usize,
        max_cores: usize,
        priority: Priority,
    ) -> bool {
        priority == Priority::Background && threads > max_cores.max(1) * 3 / 4
    }

    /// Nimmt entgegen, was die Arbeiter melden.
    ///
    /// Hier — und nur hier — wird [`GuiApp::pending`] gesetzt. Das ist der
    /// eigentliche Schutz davor, dass ein Fund von letzter Woche beim Öffnen
    /// des Programms das Dock hüpfen lässt: die aus `hits.jsonl` nachgeladenen
    /// Treffer kommen über den Konstruktor herein, nie über diesen Kanal.
    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(Event::Hit(h)) => {
                    self.hits.push(*h);
                    self.note_find(self.hits.len() - 1);
                }
                Ok(Event::PersistFailure { hit, error }) => {
                    self.errors.push(format!(
                        "Der Treffer {} konnte nicht gespeichert werden: {error}",
                        hit.address
                    ));
                    // Gemerkt, nicht bloß gezählt: dieser Treffer steht gleich
                    // in der Liste neben den echten, und ohne Markierung sieht
                    // er genauso aus wie einer, der sicher auf der Platte liegt.
                    self.unsaved.insert(hit.id.clone());
                    self.hits.push(*hit);
                    self.note_find(self.hits.len() - 1);
                }
                Ok(Event::BackupFailure { id, error }) => {
                    self.errors.push(format!(
                        "Die Sicherungskopie des Treffers {id} ist fehlgeschlagen: {error}"
                    ));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Die echten Funde, mit ihrem Platz in [`GuiApp::hits`].
    ///
    /// Testeinträge aus dem Selbsttest zählen nirgends mit: ein Platzhalter,
    /// der wie ein Vermögen aussieht, ist schlimmer als gar keine Anzeige.
    fn real_hits(&self) -> impl Iterator<Item = (usize, &Hit)> {
        self.hits
            .iter()
            .enumerate()
            .filter(|(_, h)| !h.is_synthetic())
    }

    /// Vermerkt einen Fund als noch nicht zur Kenntnis genommen.
    ///
    /// Ein Testeintrag löst nichts aus — weder Band noch Ton noch Dock. Das ist
    /// der zweite Riegel neben der Regel, dass nur [`GuiApp::drain`] hier
    /// hereinkommt.
    fn note_find(&mut self, index: usize) {
        if self.hits.get(index).is_none_or(|h| h.is_synthetic()) {
            return;
        }
        match &mut self.pending {
            Some(p) => {
                p.newest = index;
                p.count += 1;
                // Ein zweiter Fund verdient sein eigenes Klopfen.
                p.announced = false;
            }
            None => {
                self.pending = Some(Pending {
                    newest: index,
                    count: 1,
                    announced: false,
                })
            }
        }
    }

    /// Holt den Leser, wenn er woanders ist: Dock-Symbol und Ton.
    ///
    /// Einmal je Fund, nicht in jedem Bild — sonst hüpft das Dock im
    /// Sekundentakt. Und nur, wenn das Fenster nicht ohnehin vorne steht:
    /// AppKit würde die Anforderung dann zwar selbst verwerfen, aber die
    /// Absicht gehört in den Quelltext und nicht in die Systembibliothek.
    ///
    /// Bewusst **nicht** `ViewportCommand::Focus`: das reißt das Fenster nach
    /// vorne und nimmt dem Programm, in dem gerade jemand tippt, die Tastatur
    /// weg. Das Dock sagt „schau her", der Leser entscheidet wann.
    fn announce(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.pending else {
            return;
        };
        if p.announced {
            return;
        }
        p.announced = true;

        if !ctx.input(|i| i.focused) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
        }
        crate::ui::feel::alarm();
        crate::ui::feel::bump(crate::ui::feel::Bump::Switch);
    }

    /// The chest, for the header and the intro.
    fn logo_texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        self.logo
            .get_or_insert_with(|| upload(ctx, "mark", crate::icon_data::icon()))
            .clone()
    }

    /// The map and the key, for the two doors of the opening fork.
    fn door_textures(&mut self, ctx: &egui::Context) -> (TextureHandle, TextureHandle) {
        self.doors
            .get_or_insert_with(|| {
                (
                    upload(ctx, "door_search", crate::icon_data::door_search()),
                    upload(ctx, "door_recover", crate::icon_data::door_recover()),
                )
            })
            .clone()
    }

    /// Die Holzfaserung, gekachelt hinter die Flächen.
    ///
    /// `LINEAR_REPEAT` und nicht `LINEAR` wie bei den anderen Bildern: gekachelt
    /// wird über ein UV-Rechteck größer als 1.0, und mit dem sonst üblichen
    /// Klemmen am Rand würde daraus ein einziger verschmierter Streifen.
    fn wood_texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        self.wood
            .get_or_insert_with(|| {
                upload_with(
                    ctx,
                    "wood",
                    crate::icon_data::wood(),
                    egui::TextureOptions::LINEAR_REPEAT,
                )
            })
            .clone()
    }

    /// Legt die Seekarte hinter den Inhalt der Gabelung.
    ///
    /// **Nach** der Holzfaserung und **vor** allem anderen: sie liegt auf dem
    /// Tisch, nicht darunter und nicht auf den Türen.
    ///
    /// **In ihrer eigenen Farbe**, nur sehr dünn aufgetragen. Sie war einmal
    /// mit dem Holzton multipliziert und damit ein brauner Schatten ihrer
    /// selbst; das war vorsichtig, aber falsch: auf demselben Bildschirm
    /// liegen zwei Fotos in Echtfarbe (Würfel und Schlüssel), und ein
    /// einfarbig brauner Untergrund dazwischen war der Ausreißer. Pergament
    /// darf nach Pergament aussehen.
    ///
    /// Die Deckung ist eine **bewusste Entscheidung gegen die Zahlen**, und
    /// wer sie ändert, sollte wissen wogegen. Über der Karte stehen die
    /// Wortmarke und die Frage „Was möchtest du tun?", und wo das Pergament am
    /// hellsten ist, drückt es deren Kontrast auf rund 2,5 : 1 — die Regel im
    /// Rest des Programms sind 4,5 : 1, und die hielte hier nur eine Deckung
    /// um neun ein. Bei neun ist die Karte allerdings nur noch zu ahnen, und
    /// der Bildschirm soll nach Schatzkarte aussehen; das war die Ansage.
    ///
    /// Erträglich ist es, weil auf diesem Bildschirm nichts zu lesen ist, was
    /// man lesen *muss*: zwei Wörter Wortmarke und eine Frage, deren Antwort
    /// als zwei große Türen darunter steht. Auf jedem Bildschirm mit Zahlen
    /// oder Wörtern wäre dieselbe Deckung nicht zu verantworten — die Karte
    /// liegt darum ausschließlich hier.
    ///
    /// Der Renderer mischt im linearen Licht: ein helles Blatt über fast
    /// schwarzem Grund kommt um ein Vielfaches heller an, als die Zahl
    /// vermuten lässt. Wer daran dreht, misst im fertigen Bild nach, statt zu
    /// überschlagen.
    fn draw_map(&mut self, ui: &mut Ui, rect: egui::Rect) {
        if !crate::ui::theme::textured() {
            return;
        }
        let tex = self
            .map
            .get_or_insert_with(|| upload(ui.ctx(), "map_bg", crate::icon_data::map_bg()))
            .clone();

        // Quadratisch und mittig: das Blatt ist auf seinem Bogen zentriert,
        // und die durchsichtigen Ränder geben ihm seine Form. Auf die kürzere
        // Fensterseite bezogen, damit es in einem breiten Fenster nicht über
        // die Ränder wächst.
        let side = rect.width().min(rect.height()) * 0.95;
        let where_ = egui::Rect::from_center_size(rect.center(), Vec2::splat(side));
        ui.painter().image(
            tex.id(),
            where_,
            egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::from_white_alpha(45),
        );
    }

    /// Malt die Holzfaserung in ein Rechteck, und darüber eine leise Vignette.
    ///
    /// Muss der **erste** Malbefehl auf einer Fläche sein, damit alles andere
    /// darüber liegt. Tut nichts, wenn `design.grain` aus ist oder die
    /// Farbwelt keine hölzerne ist — hinter dem alten Blaugrau wäre eine
    /// Holzfaserung nur ein Fleck, und wer flach abgeschaltet hat, bekommt
    /// auch keine Vignette: flach heißt flach.
    fn draw_grain(&mut self, ui: &mut Ui, rect: egui::Rect) {
        if !crate::ui::theme::textured() {
            return;
        }
        let tex = self.wood_texture(ui.ctx());
        // Das UV-Rechteck sagt, wie oft die Kachel in das Ziel passt — gegen
        // ihre eigene Größe gerechnet, nicht gegen eine hingeschriebene Zahl,
        // damit eine feinere Kachel nicht gestaucht gekachelt wird.
        let side = tex.size()[0] as f32;
        let reps = Vec2::new(rect.width() / side, rect.height() / side);
        ui.painter().image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(reps.x, reps.y)),
            // Die Kachel ist deckend und bringt Struktur und Helligkeit
            // selbst mit; **dieser Ton** färbt sie zur Farbwelt. Vorher war
            // sie eine mit Weiß gemalte Lasur — auf einem fast schwarzen
            // Grund konnten ihre dunklen Fugen nichts mehr abdunkeln, und
            // übrig blieb ein blasses Linienmuster. Siehe [`Palette::wood`].
            crate::ui::theme::pal().wood,
        );

        // Die Vignette: vier Randverläufe zur Mitte hin. Eine Stelle für alle
        // sechs Bildschirme — das Licht über dem Tisch darf nicht je
        // Bildschirm anders fallen.
        //
        // Je Rand drei Segmente statt eines geraden Gefälles: ein lineares
        // Alpha-Gefälle bekommt auf dem Bildschirm an seinem inneren Ende
        // eine sichtbare Knickkante (Mach-Band). Die Stützstellen nähern ein
        // weiches Auslaufen an; die Stärke selbst kommt aus
        // [`crate::ui::theme::vignette`].
        let base = crate::ui::theme::vignette();
        let shade = |f: f32| Color32::from_black_alpha((base.a() as f32 * f) as u8);
        let stops = [(0.0_f32, 1.0_f32), (0.35, 0.45), (0.7, 0.15), (1.0, 0.0)];
        let d = (rect.width().min(rect.height()) * 0.22).min(130.0);
        let mut mesh = egui::Mesh::default();
        let mut quad = |pts: [(Pos2, Color32); 4]| {
            let i = mesh.vertices.len() as u32;
            for (pos, col) in pts {
                mesh.colored_vertex(pos, col);
            }
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        };
        let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
        for pair in stops.windows(2) {
            let ((f0, a0), (f1, a1)) = (pair[0], pair[1]);
            let (c0, c1) = (shade(a0), shade(a1));
            quad([
                (Pos2::new(l, t + d * f0), c0),
                (Pos2::new(r, t + d * f0), c0),
                (Pos2::new(r, t + d * f1), c1),
                (Pos2::new(l, t + d * f1), c1),
            ]);
            quad([
                (Pos2::new(l, b - d * f1), c1),
                (Pos2::new(r, b - d * f1), c1),
                (Pos2::new(r, b - d * f0), c0),
                (Pos2::new(l, b - d * f0), c0),
            ]);
            quad([
                (Pos2::new(l + d * f0, t), c0),
                (Pos2::new(l + d * f1, t), c1),
                (Pos2::new(l + d * f1, b), c1),
                (Pos2::new(l + d * f0, b), c0),
            ]);
            quad([
                (Pos2::new(r - d * f1, t), c1),
                (Pos2::new(r - d * f0, t), c0),
                (Pos2::new(r - d * f0, b), c0),
                (Pos2::new(r - d * f1, b), c1),
            ]);
        }
        ui.painter().add(egui::Shape::mesh(mesh));
    }
}

/// Hands one of the embedded pictures to the renderer.
///
/// Takes the `Option` the decoder returns rather than an `Icon`, so a picture
/// that will not decode costs a blank square and nothing else. None of these
/// can realistically fail — they are compiled into the binary and the tests
/// decode all three — but a program that refuses to open over a missing
/// decoration would be the larger fault.
fn upload(ctx: &egui::Context, name: &str, art: Option<crate::icon_data::Icon>) -> TextureHandle {
    upload_with(ctx, name, art, egui::TextureOptions::LINEAR)
}

/// Wie [`upload`], aber mit eigenen Optionen — für die Kachel, die wiederholt
/// werden muss statt am Rand zu klemmen.
fn upload_with(
    ctx: &egui::Context,
    name: &str,
    art: Option<crate::icon_data::Icon>,
    options: egui::TextureOptions,
) -> TextureHandle {
    let img = match art {
        Some(art) => egui::ColorImage {
            size: [art.width as usize, art.height as usize],
            pixels: art
                .rgba
                .chunks_exact(4)
                .map(|c| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                .collect(),
        },
        None => egui::ColorImage::new([1, 1], Color32::TRANSPARENT),
    };
    ctx.load_texture(name, img, options)
}

/// Die weiche Ankunft eines Bildschirms: blendet die Mitte in knapp einer
/// Viertelsekunde auf, statt sie ins Bild springen zu lassen.
///
/// Der Zeitstempel liegt im egui-Speicher unter `id` — wer ihn beim Verlassen
/// löscht, lässt den Bildschirm bei der Rückkehr wieder ankommen. Ein
/// Bildschirmwechsel ist im Fenster ein Raumwechsel, und ein Raum, der schon
/// da ist, bevor die Tür offen steht, fühlt sich nach Kulisse an.
fn arrival_fade(ui: &Ui, id: &str) -> f32 {
    let key = egui::Id::new(id);
    let now = ui.input(|i| i.time);
    let since: f64 = ui.memory(|m| m.data.get_temp(key)).unwrap_or_else(|| {
        ui.memory_mut(|m| m.data.insert_temp(key, now));
        now
    });
    let fade = (((now - since) as f32) / 0.22).clamp(0.0, 1.0);
    if fade < 1.0 {
        ui.ctx().request_repaint();
    }
    fade
}

/// The handle, as a link to the profile.
///
/// Underlined on hover rather than always, so it reads as a signature until it
/// is pointed at and as a link the moment it might be clicked.
fn handle_link(ui: &mut Ui, size: f32) {
    let resp = ui.add(
        egui::Label::new(RichText::new(HANDLE).color(pal().accent).size(size))
            .sense(Sense::click()),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        let r = resp.rect;
        ui.painter().line_segment(
            [
                Pos2::new(r.left(), r.bottom() - 1.0),
                Pos2::new(r.right(), r.bottom() - 1.0),
            ],
            Stroke::new(1.0_f32, pal().accent),
        );
    }
    if resp.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(HANDLE_URL));
    }
    resp.on_hover_text(HANDLE_URL);
}

/// Where the chest in the header leads.
///
/// An easter egg, so it is not labelled and carries no tooltip. The pointing
/// hand on hover is the only tell — a click target with no affordance at all
/// just feels like a broken window.
const EGG_URL: &str =
    "https://www.youtube.com/watch?v=eBGIQ7ZuuiU&list=RDeBGIQ7ZuuiU&start_radio=1";

/// What to call the machine in the window's texts: „Mac", „PC" or „Rechner",
/// fixed at compile time for the platform this build targets.
const NOUN: &str = crate::machine::noun();

/// The four modes the interface offers, as (workers, priority, duty cycle).
///
/// Public and shared with startup on purpose: the panel highlights a row only
/// on an exact match, so a configuration that lands between modes would leave
/// every row dark. Startup resolves to one of these first, and then the
/// highlight is always both present and true.
pub fn modes(m: &crate::machine::Machine) -> [(usize, Priority, u8); 4] {
    [
        (1, Priority::Background, 1),
        (m.economical_threads(), Priority::Background, 100),
        (m.recommended_threads(), Priority::Normal, 100),
        (m.max_threads(), Priority::Normal, 100),
    ]
}

/// The mode a configuration is in, or the one it is closest to.
///
/// Priority weighs heaviest, then the duty cycle, then the worker count: a
/// setting that differs in how politely it runs is further from a mode than
/// one that differs by a core. Ties go to the recommended mode, which is the
/// one a reader is least likely to be surprised by.
pub fn nearest_mode(
    m: &crate::machine::Machine,
    threads: usize,
    priority: Priority,
    throttle: u8,
) -> usize {
    const RECOMMENDED: usize = 2;
    let score = |(t, p, d): (usize, Priority, u8)| {
        let prio = (p as i32 - priority as i32).abs() * 100;
        let duty = if d == throttle { 0 } else { 50 };
        prio + duty + (t as i32 - threads as i32).abs()
    };
    let table = modes(m);
    let mut best = RECOMMENDED;
    for (i, mode) in table.iter().enumerate() {
        if score(*mode) < score(table[best]) {
            best = i;
        }
    }
    best
}

/// Die Aufschrift der Spendenzeile auf der Suche.
///
/// Der Wortlaut hängt sich an das Ergebnis des Programms: es findet nichts,
/// und das ist die gute Nachricht — wer hier fündig würde, hätte Bitcoin
/// kaputtgemacht. Zwei Fassungen davor sind daran vorbeigelaufen. „Gefällt dir
/// Schatzsuche?" ist die Frage, die jede Spendenzeile stellt und die in jeder
/// gleich klingt; eine im Piratenton („für die Mannschaft") las sich
/// verkleidet, weil das Holz ringsum den Witz schon erzählt und der Text ihn
/// dann bloß nachspricht.
const ASK_SEARCH: (&str, &str) = (
    "NICHTS GEFUNDEN?",
    "Gut so — sonst wäre Bitcoin kaputt. Das Programm bleibt gratis; wenn es dir \
     was wert war, freut mich ein Kaffee in Sats :)",
);

/// Dieselbe Bitte am Ende einer geglückten Seed-Rettung.
///
/// Sie braucht eigene Worte, weil dort das Gegenteil passiert ist: die Rettung
/// ist der eine Fall, in dem dieses Programm etwas findet, und „nichts
/// gefunden, gut so" wäre neben einer gerade zurückgeholten Wallet schlicht
/// falsch. Der Schlusssatz bleibt derselbe — zwei Bitten, die sich
/// widersprechen, klängen nach zwei Programmen.
const ASK_RECOVERED: (&str, &str) = (
    "HAT'S GEHOLFEN?",
    "Das Programm bleibt gratis; wenn es dir was wert war, freut mich ein Kaffee \
     in Sats :)",
);

/// A quiet donation note: heading, one sentence, the address and a copy button.
///
/// Deliberately where nobody trips over it — at the foot of the settings
/// drawer, and after a rescue once the words are safely on screen. The wording
/// asks once, softly, and never on a screen where nothing worked out.
///
/// Die zwei Griffe im Schlusssatz sind Absicht. „Wenn es dir was wert war"
/// stellt die Bitte als Gegenleistung hin statt als Bettelei, und „ein Kaffee
/// in Sats" beantwortet die Frage, an der die meisten hängenbleiben — nicht
/// ob, sondern wie viel. Beides bleibt beiläufig, und das ist keine
/// Bescheidenheit, sondern Rechnung: Druck wäre ausgerechnet neben einer
/// Suche, die offen zugibt, nie etwas zu finden, das Unglaubwürdigste.
///
/// Das Smiley am Ende gehört dazu. Getippt und nicht als Emoji, weil die
/// mitgelieferte Schrift keine kennt — dieselbe Lücke, die schon Haken und
/// Pfeile aus dem Fenster hält.
fn donation_note(ui: &mut Ui, (heading, body): (&str, &str)) {
    const ADDR: &str = "bc1q5dmjptzvzwv6q58u6eawgxdqrj4a5u66ugyxf6";

    ui.add_space(theme::S2);
    ui.separator();
    ui.add_space(theme::S2);
    ui.label(
        RichText::new(heading)
            .color(pal().primary)
            .size(theme::SMALL)
            .strong(),
    );
    ui.add_space(theme::S2);
    ui.label(RichText::new(body).color(pal().dim).size(theme::SMALL));
    ui.add_space(theme::S2);

    // The address, in a box that reads as a thing to copy rather than as body
    // text. Selectable, so it can be dragged out even without the button.
    egui::Frame::none()
        .fill(pal().bg)
        .rounding(theme::r_sm())
        .stroke(theme::hairline())
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    RichText::new(ADDR)
                        .color(pal().text)
                        .font(mono(theme::SMALL)),
                )
                .wrap()
                .sense(Sense::hover()),
            );
        });
    ui.add_space(theme::S2);

    // "kopiert!" lingers for a moment after a click, driven by a timestamp in
    // egui memory so the confirmation survives across frames.
    let id = egui::Id::new("donate_copied_at");
    let now = ui.input(|i| i.time);
    let recent = ui
        .memory(|m| m.data.get_temp::<f64>(id))
        .is_some_and(|t| now - t < 1.8);
    let (label, colour) = if recent {
        ("Adresse kopiert", pal().green)
    } else {
        ("Adresse kopieren", pal().primary)
    };
    if ui
        .add(
            egui::Button::new(
                RichText::new(label)
                    .color(pal().on_fill)
                    .size(theme::SMALL)
                    .strong(),
            )
            .fill(colour)
            .rounding(theme::r_sm())
            .min_size(Vec2::new(ui.available_width(), 30.0)),
        )
        .clicked()
    {
        ui.ctx().copy_text(ADDR.to_string());
        ui.memory_mut(|m| m.data.insert_temp(id, now));
    }
    if recent {
        ui.ctx().request_repaint();
    }
    // Hier stand einmal noch das Handle. Zweimal dasselbe Konto auf einem
    // Bildschirm — einmal in der Fußleiste, einmal unter der Spendenadresse —
    // liest sich nicht mehr als Signatur, sondern als Werbung. In der
    // Fußleiste steht es weiterhin.
}

// --- Recovery screen -------------------------------------------------------

/// A rough duration in German words. Matches the terminal wording.
fn format_estimate(secs: f64) -> String {
    if secs < 1.0 {
        "unter einer Sekunde".into()
    } else if secs < 90.0 {
        format!("etwa {} Sekunden", secs.round() as u64)
    } else if secs < 5400.0 {
        format!("etwa {} Minuten", (secs / 60.0).round() as u64)
    } else if secs < 172_800.0 {
        format!("etwa {} Stunden", (secs / 3600.0).round() as u64)
    } else {
        format!("etwa {} Tage", (secs / 86_400.0).round() as u64)
    }
}

/// The words worth offering under a field holding `typed`, if any.
///
/// Nothing for a blank field: that means "I do not know this one", and
/// answering it with the first six words of the list would be noise. Nothing
/// either when the only thing left to suggest is what is already written — but
/// note that a finished word is not always alone. The BIP-39 list contains
/// words that are prefixes of longer ones ("act" of "action", "add" of
/// "address"), and someone who has typed "act" may well be on their way to
/// "actress", so the longer ones must keep showing.
///
/// *Whether* the list appears is a separate question, and deliberately so: it
/// is held by egui's popup state rather than by the field's focus, because a
/// click on a suggestion takes the focus away and a focus-driven list would
/// disappear before the click could land.
pub(crate) fn suggestions_for(typed: &str) -> Vec<&'static str> {
    if typed.trim().is_empty() {
        return Vec::new();
    }
    let words = crate::bip39::words_starting_with(typed, 6);
    if words.len() == 1 && words[0] == typed.trim() {
        return Vec::new();
    }
    words
}

/// One word field: number, text box, a mark saying how the word was read, and
/// a state chip that cycles on click.
///
/// An empty field is always "unknown" and its chip is inert — there is nothing
/// to be sure or unsure about. A filled field cycles sure → unsure → moved.
///
/// The mark and the suggestion list are the point of this widget. Someone here
/// is copying two dozen words off paper, from a list of 2048, because their
/// money depends on getting it right — and until now the screen said nothing
/// at all about any single word. Worse, the lookup silently reads "abandonn"
/// as "abandon", so a typo could send the search after the wrong seed and
/// report only "nichts gefunden" at the end of it.
/// Gibt zurück, ob der **Text** von Hand geändert wurde. Ein Wechsel des Status
/// zählt nicht: an gewürfelten Übungswörtern die Zustände durchzuklicken ist
/// genau das Üben, um das es geht, und darf den Übungs-Streifen nicht löschen.
fn word_field(ui: &mut Ui, n: usize, slot: &mut crate::recover_ui::Slot) -> bool {
    use crate::recover::State;
    ui.horizontal(|ui| {
        ui.add_sized(
            Vec2::new(22.0, 22.0),
            egui::Label::new(
                RichText::new(format!("{n:>2}"))
                    .color(pal().muted)
                    .font(mono(theme::SMALL)),
            ),
        );
        let empty = slot.word.trim().is_empty();
        let typed = slot.word.trim().to_ascii_lowercase();
        let resolved = crate::bip39::resolve_word(&typed);

        let edit = ui.add(
            egui::TextEdit::singleline(&mut slot.word)
                .desired_width(118.0)
                .font(mono(theme::BODY))
                .text_color(if empty {
                    pal().dim
                } else if resolved.is_none() {
                    pal().alert
                } else {
                    pal().text
                }),
        );
        // Screenshot hook, in the SC_SHOT_* family: `SC_SHOT_FOCUS=<n>` puts
        // the keyboard in field n, which is the only way to photograph the
        // suggestion list — it exists precisely when a field has focus.
        if std::env::var("SC_SHOT_FOCUS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            == Some(n)
        {
            edit.request_focus();
        }

        // Suggestions, below the box so they never cover the neighbouring word.
        //
        // Whether the list is *shown* is egui's popup state, not this field's
        // focus. That distinction is the whole reason clicking a suggestion
        // works: the click takes the keyboard away from the text box, so a
        // list drawn only while the box had focus vanished on the very frame
        // the click was meant to land in, and the word was never filled in.
        let popup = ui.make_persistent_id(("word_suggest", n));
        let suggestions = suggestions_for(&typed);
        if edit.gained_focus() || edit.changed() {
            if suggestions.is_empty() {
                ui.memory_mut(|m| m.close_popup());
            } else {
                ui.memory_mut(|m| m.open_popup(popup));
            }
        }
        if !suggestions.is_empty() {
            let mut chosen: Option<&'static str> = None;
            egui::popup_below_widget(
                ui,
                popup,
                &edit,
                egui::PopupCloseBehavior::CloseOnClickOutside,
                |ui| {
                    ui.set_min_width(150.0);
                    for w in &suggestions {
                        // A button rather than a label: a row that lights up
                        // under the pointer is a row that looks clickable, and
                        // this one has to.
                        let hit = ui.add(
                            egui::Button::new(
                                RichText::new(*w).color(pal().text).font(mono(theme::BODY)),
                            )
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .min_size(Vec2::new(ui.available_width(), 0.0)),
                        );
                        if hit.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if hit.clicked() {
                            chosen = Some(w);
                        }
                    }
                },
            );
            if let Some(w) = chosen {
                slot.word = w.to_string();
                ui.memory_mut(|m| m.close_popup());
            }
        }

        // One pill per filled word, carrying both what the program made of the
        // word and how sure the reader said they are. Two separate indicators —
        // a validation dot and a state chip — said the same "green, good" twice
        // on the common row and sprawled the columns apart. An empty box gets
        // no pill at all: it is self-evidently a gap the search will fill, and
        // a column of "fehlt" chips made an untouched form look like a list of
        // errors.
        ui.add_space(theme::S2);
        if !empty {
            // Validation is the priority: a word that is not on the list is an
            // error whatever state it carries. Otherwise the pill shows the
            // state, and — when the program read something other than what was
            // typed — the word it actually understood.
            let (colour, label): (Color32, String) = match resolved {
                None => (pal().alert, "kein Wort".into()),
                Some(w) if w != typed => (pal().warn, format!("= {w}")),
                Some(_) => match slot.state {
                    State::Sure => (pal().green, "sicher".into()),
                    State::Unsure => (pal().warn, "unsicher".into()),
                    State::Moved => (pal().accent, "verrutscht".into()),
                },
            };
            let pill = egui::Button::new(RichText::new(label).color(colour).size(theme::SMALL))
                .fill(theme::tinted(colour, 26))
                .stroke(Stroke::new(1.0_f32, colour))
                .rounding(theme::r_sm())
                .min_size(Vec2::new(92.0, 22.0));
            // What this state actually does, in the words of someone who has
            // never met a wordlist — and what the next click will make of it.
            // "unsicher" says outright that it is the same search as an empty
            // box, because it is, and a reader who is not told that will hunt
            // for the difference.
            let explain = match slot.state {
                State::Sure => {
                    "Das Wort steht fest.\n\nKlick: unsicher — dann werden hier alle 2048 \
                     Wörter durchprobiert."
                }
                State::Unsure => {
                    "Hier werden alle 2048 Wörter durchprobiert — genau wie wenn du das Feld \
                     leer lässt. Dein Text bleibt nur als Notiz für dich stehen.\n\n\
                     Klick: verrutscht."
                }
                State::Moved => {
                    "Das Wort stimmt, nur sein Platz nicht.\n\nWirkt erst, wenn das Wort direkt \
                     davor oder dahinter auch verrutscht ist — allein bewirkt es nichts.\n\n\
                     Klick: sicher."
                }
            };
            if ui.add(pill).on_hover_text(explain).clicked() {
                slot.state = match slot.state {
                    State::Sure => State::Unsure,
                    State::Unsure => State::Moved,
                    State::Moved => State::Sure,
                };
            }
        }
        edit.changed()
    })
    .inner
}

/// Zeichnet Text mit einer dunklen Kontur ringsum.
///
/// Für die zwei Zeilen auf der Gabelung, die über der Seekarte stehen. Dort
/// ist der Untergrund hell **und** unruhig, und gegen unruhig hilft keine
/// Farbe: Ein Buchstabe, der zufällig auf einer Küstenlinie sitzt, verliert
/// seine Form, so hell er auch sein mag. Eine Kontur gibt ihm seine Kante
/// zurück, unabhängig davon, was darunter liegt — dieselbe Antwort, die
/// Bildunterschriften auf Fotos seit jeher geben.
///
/// Acht Durchgänge ringsum, einer obenauf. Das verdickt den Strich von selbst;
/// das frühere mehrfach-versetzte Malen in der Textfarbe entfällt damit.
///
/// Die Konturfarbe kommt aus der Palette und ist kein hingeschriebenes
/// Schwarz: `sunken` ist der tiefste Ton der jeweiligen Farbwelt, in Walnuss
/// also ein anderes Fast-Schwarz als in Mahagoni.
///
/// `tracking` sperrt die Buchstaben um so viele Punkte. Null heißt: das Wort
/// bleibt ein Satz aus der Schrift heraus, mit deren eigenen Abständen.
fn outlined_label(ui: &mut Ui, text: &str, colour: Color32, font: FontId, tracking: f32) {
    // Die Kontur wächst mit der Schrift. Ein fester Wert war der erste
    // Versuch und ein schlechter: 1,6 Punkte stehen einer Wortmarke von
    // sechsundzwanzig Punkten gut und drücken einer Zeile von sechzehn die
    // Punzen zu — die Löcher in a, e und ö liefen voll, und die Zeile wurde
    // unleserlicher als ganz ohne Kontur.
    let ring = font.size * 0.05;
    let (letters, size) = tracked_galleys(ui, text, font, tracking);
    // Der Platz für die Kontur gehört mit in die Zuteilung, sonst schneidet
    // die Zeile darüber oder darunter sie an.
    let (rect, _) = ui.allocate_exact_size(size + Vec2::splat(ring * 2.0), Sense::hover());
    let at = rect.min + Vec2::splat(ring);

    let dark = pal().sunken;
    // Die vier Diagonalen liegen um 1/√2 näher als die vier Geraden. Auf dem
    // vollen Radius stünden sie einundvierzig Prozent weiter draußen als die
    // Geraden, und die Kontur bekäme an jeder Rundung vier Zacken — sie soll
    // ein Ring sein, kein Quadrat.
    const D: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for (dx, dy) in [
        (0.0, -1.0),
        (0.0, 1.0),
        (-1.0, 0.0),
        (1.0, 0.0),
        (-D, -D),
        (D, -D),
        (-D, D),
        (D, D),
    ] {
        for (x, galley) in &letters {
            ui.painter().galley(
                at + Vec2::new(x + dx * ring, dy * ring),
                galley.clone(),
                dark,
            );
        }
    }
    for (x, galley) in &letters {
        ui.painter()
            .galley(at + Vec2::new(*x, 0.0), galley.clone(), colour);
    }
}

/// Wie [`outlined_label`], nur ohne Kontur: für die Wortmarke auf Vorhang und
/// Ladebildschirm, die auf ruhigem Grund steht und keine Kante braucht.
fn spaced_label(ui: &mut Ui, text: &str, colour: Color32, font: FontId, tracking: f32) {
    let (letters, size) = tracked_galleys(ui, text, font, tracking);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    for (x, galley) in &letters {
        ui.painter()
            .galley(rect.min + Vec2::new(*x, 0.0), galley.clone(), colour);
    }
}

/// Setzt `text` und gibt jeden Buchstaben mit seinem Abstand von links zurück,
/// dazu das Maß des Ganzen.
///
/// **Alle Buchstaben tragen [`Color32::PLACEHOLDER`].** Nur so entscheidet die
/// Farbe, die später an `Painter::galley` geht — jede echte Farbe aus dem
/// Satz hat dort Vorrang. Genau daran scheiterte die Kontur vorher: der Satz
/// stand schon in der Textfarbe, die acht dunklen Durchgänge kamen deshalb
/// ebenfalls in der Textfarbe heraus, und statt einer Kante bekam das Wort
/// achtmal sich selbst versetzt übereinander — aufgequollen, mit
/// zugelaufenen Punzen. Es sah nach einer schlechten Schrift aus und war ein
/// vertauschtes Argument.
///
/// Gesperrt wird buchstabenweise, weil egui keine Sperrung kennt. Der Behelf
/// davor waren Leerzeichen im Text selbst — ein Leerzeichen ist bei Ubuntu
/// Bold aber rund ein Viertel der Schriftgröße breit, also dreimal mehr, als
/// eine Wortmarke braucht, und es zerreißt das Wort in einzelne Buchstaben.
/// Ungesperrt bleibt es ein Satz: dann ist die Schrift mit ihren eigenen
/// Paaren am Zug, und die kann sie besser als eine Schleife hier.
fn tracked_galleys(
    ui: &Ui,
    text: &str,
    font: FontId,
    tracking: f32,
) -> (Vec<(f32, Arc<egui::Galley>)>, Vec2) {
    if tracking <= 0.0 {
        let galley = ui
            .painter()
            .layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
        let size = galley.size();
        return (vec![(0.0, galley)], size);
    }

    let mut letters = Vec::new();
    let mut x = 0.0;
    let mut height = 0.0_f32;
    for ch in text.chars() {
        let galley =
            ui.painter()
                .layout_no_wrap(ch.to_string(), font.clone(), Color32::PLACEHOLDER);
        let advance = galley.size().x;
        height = height.max(galley.size().y);
        letters.push((x, galley));
        x += advance + tracking;
    }
    // Hinter dem letzten Buchstaben steht keine Sperrung — sonst stünde das
    // Wort in einer zentrierten Zeile um eine halbe Sperrung zu weit links.
    (letters, Vec2::new((x - tracking).max(0.0), height))
}

/// Der Abstand, der einen Inhalt in die Mitte der Fläche rückt — oder null,
/// wenn er dafür zu hoch ist.
///
/// Gedacht für die Bildschirme, deren Inhalt kürzer ist als das Fenster: er
/// klebte oben am Rand und ließ darunter eine leere Hälfte stehen.
///
/// **Die Höhe stammt aus dem vorigen Bild**, denn eine Fläche in egui wird von
/// oben nach unten aufgebaut — wie hoch der Inhalt wird, steht erst fest,
/// wenn er gezeichnet ist, und da ist es für einen Abstand darüber zu spät.
/// Ein Bild Verzug fällt nicht auf, solange die Höhe steht; beim Wechsel auf
/// einen anderen Schritt rückt der Block ein einziges Bild später an seinen
/// Platz. Der Preis dafür, dass der Inhalt nicht zweimal je Bild durchlaufen
/// wird — die Formulare tragen Textfelder, und ein Probelauf würde deren
/// Eingaben ein zweites Mal verarbeiten.
///
/// Zwei Vorsichtsmaßnahmen gegen ein zappelndes Fenster: gemittet wird erst,
/// wenn der Inhalt mit vier Punkten Luft hineinpasst, und der Abstand wird auf
/// ganze Punkte abgerundet. Sonst käme der Block bei nahezu voller Höhe genau
/// auf die Kante, ein Rollbalken erschiene, die Spalte würde schmaler, der
/// Text brauchte eine Zeile mehr, der Balken verschwände wieder — jedes Bild
/// von neuem.
fn centring_lead(ui: &Ui, key: &'static str) -> f32 {
    let last: f32 = ui
        .ctx()
        .data(|d| d.get_temp(egui::Id::new(key)))
        .unwrap_or(0.0);
    centring_lead_for(last, ui.available_height())
}

/// Die Rechnung hinter [`centring_lead`], ohne Fenster — damit sie geprüft
/// werden kann, ohne eines zu öffnen.
///
/// `content` ist null, solange nichts gemessen wurde: dann bleibt der Inhalt
/// oben, wie bisher, und rückt im nächsten Bild an seinen Platz.
fn centring_lead_for(content: f32, room: f32) -> f32 {
    if content <= 0.0 || content + 4.0 >= room {
        return 0.0;
    }
    ((room - content) / 2.0).floor().max(0.0)
}

/// Legt die gemessene Höhe für [`centring_lead`] im nächsten Bild ab.
fn remember_body_height(ui: &Ui, key: &'static str, height: f32) {
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new(key), height));
}

/// Der erste eingeschaltete Meldeweg, dem noch etwas zum Funktionieren fehlt.
///
/// `None` heißt: alles, was an ist, könnte auch etwas verschicken.
///
/// Warum das hier steht und nicht in [`crate::config::Config::validate`]: Was
/// dort steht, verhindert den **Start** des Programms. Ein Mailserver, der noch
/// `smtp.example.com` heißt, ist ein unfertiger Eintrag und kein Grund, jemanden
/// aus seinem eigenen Programm auszusperren — zumal der Bildschirm, auf dem er
/// das reparieren würde, dieser hier ist. Gefangen wird es darum beim
/// Speichern, wo der Mensch danebensitzt und es sofort richtigstellen kann.
///
/// Geprüft wird nur, was ohne Netz feststellbar ist: dass ein Feld leer ist
/// oder noch den Beispielwert trägt. Ob der Server antwortet, weiß man erst,
/// wenn man ihn fragt — und das tut dieses Programm erst, wenn es etwas zu
/// melden gibt.
fn missing_alert_field(a: &crate::config::Alerts) -> Option<String> {
    let blank = |s: &str| s.trim().is_empty();
    if a.ntfy.enabled && blank(&a.ntfy.base_url) {
        return Some(
            "ntfy: Ohne Server geht es nicht. Für den öffentlichen Dienst: \
                     https://ntfy.sh"
                .into(),
        );
    }
    if a.telegram.enabled && (blank(&a.telegram.bot_token) || blank(&a.telegram.chat_id)) {
        return Some(
            "Telegram: Es fehlt der Bot-Token oder die Chat-Kennung. Beides gibt dir \
             @BotFather in Telegram."
                .into(),
        );
    }
    if a.smtp.enabled {
        if blank(&a.smtp.host) || a.smtp.host.trim() == "smtp.example.com" {
            return Some(
                "E-Mail: Da steht noch der Beispielserver. Trag den Server deines \
                 Mailanbieters ein."
                    .into(),
            );
        }
        if blank(&a.smtp.from) || blank(&a.smtp.to) {
            return Some("E-Mail: Absender und Empfänger müssen ausgefüllt sein.".into());
        }
    }
    if a.webhook.enabled && blank(&a.webhook.url) {
        return Some("Webhook: Ohne Adresse geht die Meldung nirgendwohin.".into());
    }
    None
}

/// Der Name des Dienstes aus einer Schnittstellen-Adresse, für Sätze, in denen
/// er vorkommt: aus `https://mempool.space/api` wird `mempool.space`.
///
/// Wer gefragt wird, ob etwas „an den Dienst aus der config.toml" gehen darf,
/// hat nicht genug erfahren, um zu antworten.
fn api_host(api: &str) -> &str {
    let bare = api
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    bare.split('/')
        .next()
        .filter(|h| !h.is_empty())
        .unwrap_or(bare)
}

/// The editing form: length, word grid with per-word states, address,
/// live preview, warning, start.
fn recover_form(
    ui: &mut Ui,
    r: &mut crate::recover_ui::RecoverUi,
    _keep_open: &mut bool,
    depth: u32,
    api: &str,
) {
    use crate::recover_ui::Step;

    // The rail never fades — it is the fixed frame the steps move through.
    recover_rail(ui, r.step);
    ui.add_space(theme::S4);

    // Über dem Titel und damit auf allen vier Schritten — am wichtigsten auf dem
    // letzten, wo ein Fund erscheint: eine gewürfelte Seed darf nie mit einer
    // echten Wiederherstellung verwechselt werden. Farbe `accent`, nicht Gold
    // (heißt Fund) und nicht `warn` (heißt Gefahr): es ist eine Feststellung.
    if r.practice {
        let dismissed = widgets::banner(
            ui,
            pal().accent,
            "Übungsdaten — gewürfelt, nicht deine Seed",
            "Diese Wörter sind erfunden und gehören niemandem. Zum Ausprobieren \
             gedacht: es wird nichts gespeichert und nichts gemeldet. Für den \
             Ernstfall die Wörter überschreiben.",
            Some("Verstanden"),
        );
        if dismissed {
            r.practice = false;
        }
        ui.add_space(theme::S3);
    }

    // A short fade-and-rise whenever the step changes, so moving between screens
    // reads as one thing sliding in rather than an instant swap. A per-step
    // stopwatch in egui memory drives it: every arrival on a new step restarts
    // the clock, and nothing animates while a reader sits still on a screen.
    let id = egui::Id::new("recover_step_at");
    let now = ui.input(|i| i.time);
    let (last_step, since): (usize, f64) = ui
        .memory(|m| m.data.get_temp(id))
        .unwrap_or((usize::MAX, now));
    let since = if last_step != r.step.index() {
        ui.memory_mut(|m| m.data.insert_temp(id, (r.step.index(), now)));
        now
    } else {
        since
    };
    let e = (now - since) as f32;
    let fade = (e / 0.16).clamp(0.0, 1.0);
    if fade < 1.0 {
        ui.ctx().request_repaint();
    }
    // A few points of downward offset that melts away as it fades in.
    ui.add_space((1.0 - fade) * 10.0);
    ui.scope(|ui| {
        ui.multiply_opacity(fade);

        let (title, sub) = r.step.title();
        ui.label(
            RichText::new(title)
                .color(pal().text)
                .size(theme::DISPLAY)
                .strong(),
        );
        ui.add_space(theme::S2);
        ui.label(RichText::new(sub).color(pal().dim).size(theme::BODY));
        ui.add_space(theme::S4);

        match r.step {
            Step::Length => recover_step_length(ui, r),
            Step::Words => recover_step_words(ui, r),
            Step::Address => recover_step_address(ui, r),
            Step::Start => recover_step_start(ui, r, api),
        }
    });

    ui.add_space(theme::S4);
    recover_nav(ui, r, depth);
    ui.add_space(theme::S4);
}

/// The four steps as a rail across the top: where you are, what is behind you,
/// what is still to come.
///
/// Drawn rather than assembled from widgets so the circles, the labels and the
/// line between them sit on one grid.
fn recover_rail(ui: &mut Ui, current: crate::recover_ui::Step) {
    use crate::recover_ui::Step;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::hover());
    let p = ui.painter();
    let n = Step::ALL.len() as f32;
    let slot = rect.width() / n;
    let cy = rect.top() + 14.0;

    for (i, s) in Step::ALL.iter().enumerate() {
        let cx = rect.left() + slot * (i as f32 + 0.5);
        let done = s.index() < current.index();
        let here = *s == current;

        if i > 0 {
            let prev = rect.left() + slot * (i as f32 - 0.5);
            p.line_segment(
                [Pos2::new(prev + 18.0, cy), Pos2::new(cx - 18.0, cy)],
                Stroke::new(
                    2.0_f32,
                    if done || here {
                        pal().primary
                    } else {
                        pal().frame
                    },
                ),
            );
        }

        let (fill, ring, ink) = if here {
            (pal().primary, pal().primary, pal().on_fill)
        } else if done {
            (theme::wash(pal().primary), pal().primary, pal().primary)
        } else {
            (pal().panel, pal().frame, pal().muted)
        };
        p.circle_filled(Pos2::new(cx, cy), 14.0, fill);
        p.circle_stroke(Pos2::new(cx, cy), 14.0, Stroke::new(1.5_f32, ring));
        p.text(
            Pos2::new(cx, cy),
            egui::Align2::CENTER_CENTER,
            format!("{}", i + 1),
            FontId::proportional(theme::BODY),
            ink,
        );
        p.text(
            Pos2::new(cx, rect.top() + 38.0),
            egui::Align2::CENTER_CENTER,
            s.tab(),
            FontId::proportional(theme::SMALL),
            if here { pal().text } else { pal().muted },
        );
    }
}

/// Screen 1: how long the seed is. Five big targets, nothing else.
///
/// Drawn by hand rather than as plain buttons so each tile answers to the
/// pointer — a soft lift on hover, the chosen one filled — which is what makes
/// a choice feel like a choice rather than a form control.
fn recover_step_length(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi) {
    ui.horizontal_wrapped(|ui| {
        for wc in crate::bip39::ALL_WORD_COUNTS {
            let on = r.word_count == wc;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(92.0, 72.0), Sense::click());
            let hovered = resp.hovered();
            let p = ui.painter();
            let fill = if on {
                pal().primary
            } else if hovered {
                pal().hover
            } else {
                pal().panel
            };
            p.rect(
                rect,
                theme::r_md(),
                fill,
                Stroke::new(
                    if on { 0.0_f32 } else { 1.0 },
                    if on { pal().primary } else { pal().frame },
                ),
            );
            p.text(
                rect.center() - Vec2::new(0.0, 8.0),
                egui::Align2::CENTER_CENTER,
                format!("{}", wc.words()),
                FontId::proportional(theme::DISPLAY),
                if on { pal().on_fill } else { pal().text },
            );
            p.text(
                rect.center() + Vec2::new(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                "Wörter",
                FontId::proportional(theme::SMALL),
                if on { pal().on_fill_dim } else { pal().muted },
            );
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                r.resize(wc);
            }
            ui.add_space(theme::S3);
        }
    });
    ui.add_space(theme::S3);
    ui.label(
        RichText::new(
            "Nicht sicher? Nimm 12 — beim Einfügen stellt sich die Länge von selbst \
             richtig.",
        )
        .color(pal().muted)
        .size(theme::SMALL),
    );
}

/// Screen 2: the words themselves.
fn recover_step_words(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi) {
    use crate::recover::State;

    // The shortcut first: anyone who already has their words written down
    // somewhere should not have to discover the grid before the paste box.
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut r.bulk)
                .desired_width(ui.available_width() - 130.0)
                .hint_text("Alle Wörter auf einmal einfügen")
                .font(mono(theme::BODY)),
        );
        ui.add_space(theme::S2);
        let ready = !r.bulk.trim().is_empty();
        let btn = egui::Button::new(
            RichText::new("Verteilen")
                .color(if ready { pal().on_fill } else { pal().muted })
                .size(theme::BODY),
        )
        .fill(if ready { pal().primary } else { pal().inset })
        .rounding(theme::r_sm())
        .min_size(Vec2::new(0.0, 28.0));
        if ui.add_enabled(ready, btn).clicked() {
            let text = std::mem::take(&mut r.bulk);
            r.bulk_note = Some(match r.paste_all(&text) {
                Ok(n) => Ok(format!("{n} Wörter übernommen.")),
                Err(e) => {
                    r.bulk = text;
                    Err(e)
                }
            });
        }
    });
    // Der Würfel: eine eigene Zeile, nicht neben „Verteilen". Die Breite des
    // Feldes darüber ist fest für einen Knopf reserviert — und vor allem kann
    // ein nackter Würfel nicht sagen, dass es Übungsdaten sind. Die Aufschrift
    // kann es.
    ui.add_space(theme::S2);
    ui.horizontal(|ui| {
        // Die Seite dreht sich mit jedem Wurf, damit der Knopf sichtbar
        // antwortet, auch wenn die Wörter im Gitter darunter stehen.
        let face = (r.rolls % 6 + 1) as u8;
        if widgets::dice_button(ui, "Übungswörter würfeln", face) {
            r.bulk_note = Some(match r.roll_practice() {
                Ok(gap) => Ok(format!(
                    "Übungs-Seed gewürfelt: {} Wörter und die passende Adresse eingesetzt, \
                     Wort {gap} offen gelassen. Geh auf Weiter.",
                    r.word_count.words()
                )),
                Err(e) => Err(e),
            });
        }
        ui.add_space(theme::S2);
        ui.label(
            RichText::new(
                "Erfundene Seed samt Adresse, ein Wort offen — zum Ausprobieren, \
                 ohne deine eigenen Wörter anzufassen.",
            )
            .color(pal().dim)
            .size(theme::SMALL),
        );
    });

    if let Some(note) = &r.bulk_note {
        ui.add_space(theme::S2);
        match note {
            Ok(msg) => widgets::note(ui, pal().green, msg),
            Err(msg) => widgets::note(ui, pal().warn, msg),
        }
    }

    ui.add_space(theme::S3);
    ui.label(
        RichText::new(
            "Beim Tippen werden dir die passenden Wörter vorgeschlagen — anklicken genügt.",
        )
        .color(pal().dim)
        .size(theme::SMALL),
    );
    ui.add_space(theme::S3);

    let half = r.word_count.words().div_ceil(2);
    // Von Hand getippt heißt: das sind nicht mehr die gewürfelten Wörter.
    let mut edited = false;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            for i in 0..half {
                edited |= word_field(ui, i + 1, &mut r.slots[i]);
                ui.add_space(theme::S2);
            }
        });
        ui.add_space(theme::S3);
        ui.vertical(|ui| {
            for i in half..r.word_count.words() {
                edited |= word_field(ui, i + 1, &mut r.slots[i]);
                ui.add_space(theme::S2);
            }
        });
    });
    if edited {
        r.practice = false;
    }

    // The legend arrives the moment a pill is moved off "sicher", and not
    // before: on an untouched form it is three lines about states the reader
    // has not met, and the moment they set one it is the only thing they want
    // to read. What it has to say is that two of the three are the same
    // search — "unsicher" and an empty box both try all 2048 — because the
    // difference between them is the first thing anyone looks for and there
    // is none to find.
    let legend = r
        .slots
        .iter()
        .any(|s| s.state != State::Sure && !s.word.trim().is_empty());

    // One quiet line of guidance, and only once there is a word to explain it
    // on. The old four-colour legend named "unsicher", "verrutscht" and
    // "Reihenfolge unklar" — jargon a first-timer does not need, for states
    // they may never set. The one non-obvious thing is that the pill is a
    // button, so that is all this says — and once the legend below is up, even
    // that is spent: whoever is reading it has plainly found the button.
    if r.slots.iter().any(|s| !s.word.trim().is_empty()) {
        ui.add_space(theme::S3);
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, pal().green);
            ui.add_space(theme::S1);
            ui.label(
                RichText::new("Grün heißt: Wort erkannt.")
                    .color(pal().dim)
                    .size(theme::SMALL),
            );
            if !legend {
                ui.add_space(theme::S3);
                ui.label(
                    RichText::new(
                        "Bei einem Wort unsicher? Klick auf die grüne Markierung dahinter.",
                    )
                    .color(pal().muted)
                    .size(theme::SMALL),
                );
            }
        });
    }

    if legend {
        ui.add_space(theme::S3);
        for (colour, name, what) in [
            (pal().green, "sicher", "Das Wort steht fest."),
            (
                pal().warn,
                "unsicher",
                "Alle 2048 Wörter werden hier durchprobiert — genau wie bei einem leeren Feld. \
                 Dein Text bleibt nur als Notiz stehen.",
            ),
            (
                pal().accent,
                "verrutscht",
                "Das Wort stimmt, sein Platz nicht. Wirkt nur, wenn mindestens zwei \
                 nebeneinander so markiert sind — die werden dann in allen Reihenfolgen \
                 getauscht.",
            ),
        ] {
            ui.horizontal_top(|ui| {
                ui.add_space(theme::S1);
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, colour);
                ui.add_space(theme::S2);
                ui.label(
                    RichText::new(name)
                        .color(colour)
                        .size(theme::SMALL)
                        .strong(),
                );
                ui.add_space(theme::S2);
                ui.label(RichText::new(what).color(pal().muted).size(theme::SMALL));
            });
            ui.add_space(theme::S1);
        }
    }

    // A "verrutscht" with no neighbour to trade places with is the one setting
    // in this form that silently does nothing — `Layout::build` keeps runs of
    // two or more and drops the rest, which `a_single_moved_word_does_nothing`
    // pins down. Said here rather than left to be discovered, because the
    // search would otherwise start, run and finish as if the mark had never
    // been made.
    let trades: Vec<bool> = r
        .slots
        .iter()
        .map(|s| s.state == State::Moved && !s.word.trim().is_empty())
        .collect();
    if has_lonely_move(&trades) {
        ui.add_space(theme::S3);
        widgets::note(
            ui,
            pal().warn,
            "Ein einzelnes „verrutscht“ bewirkt nichts. Markiere auch das Wort davor \
             oder dahinter.",
        );
        ui.add_space(theme::S1);
        widgets::disclosure(
            ui,
            "verrutscht_allein",
            "Ein Wort kann seinen Platz nur mit einem anderen tauschen — verrutschen \
             heißt ja, dass zwei Wörter die Plätze getauscht haben. Eine Marke, deren \
             beide Nachbarn nicht markiert sind, hat deshalb keinen Tauschpartner und \
             wird bei der Suche wie „sicher“ behandelt.",
        );
    }
}

/// True when some position trades places with nobody.
///
/// `trades` marks the positions that count as moved for the search — marked
/// verrutscht *and* carrying a word, the same pair of conditions
/// `Layout::build` applies before it groups them into runs. It keeps runs of
/// two or more and drops the rest, so a marked position whose neighbours are
/// both unmarked has no effect at all; this is what warns about that.
fn has_lonely_move(trades: &[bool]) -> bool {
    trades
        .iter()
        .enumerate()
        .any(|(i, &m)| m && !(i > 0 && trades[i - 1]) && !(i + 1 < trades.len() && trades[i + 1]))
}

/// Screen 3: the address, which is optional and says so.
fn recover_step_address(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi) {
    ui.add(
        egui::TextEdit::singleline(&mut r.address)
            .desired_width(f32::INFINITY)
            .hint_text("leer lassen, oder  bc1q…  /  1…  /  3…")
            .font(mono(theme::BODY)),
    );
    ui.add_space(theme::S3);

    if r.address.trim().is_empty() {
        match r.without_address() {
            crate::recover_ui::Targetless::ListsSeeds(n) => widgets::note(
                ui,
                pal().dim,
                &format!(
                    "Du bekommst {n} passende Seeds aufgelistet, jede mit ihrer ersten Adresse."
                ),
            ),
            crate::recover_ui::Targetless::HuntsForMoney => {
                widgets::note(
                    ui,
                    pal().dim,
                    "Ohne Adresse prüft das Programm jede mögliche Seed gegen seine \
                     Adressliste.",
                );
                ui.add_space(theme::S1);
                widgets::disclosure(
                    ui,
                    "ohne_adresse_jagd",
                    "Es sind zu viele Möglichkeiten, um sie dir alle aufzulisten. Statt \
                     dessen probiert das Programm jede einzelne durch und vergleicht sie \
                     mit der Liste der Adressen, die Guthaben halten. Findet sich eine, \
                     bekommst du die Wörter und den Betrag — und beides wird sofort auf \
                     die Platte geschrieben, bevor irgendetwas anderes passiert.",
                );
                if r.hunt.is_none() {
                    ui.add_space(theme::S2);
                    widgets::note(
                        ui,
                        pal().warn,
                        "Die Adressliste lädt noch. Warte, bis die Suche im Hauptfenster läuft.",
                    );
                }
            }
        }
    } else {
        widgets::note(
            ui,
            pal().green,
            "Mit Adresse bleibt am Ende genau die eine Seed übrig, die zu dieser Wallet \
             gehört.",
        );
    }
}

/// Screen 4: how hard to push, what it will cost, and the one warning that has
/// to be read before anything starts.
fn recover_step_start(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi, api: &str) {
    use crate::recover_ui::Preview;

    let max = crate::config::physical_cores().max(1);
    r.threads = r.threads.clamp(1, max);
    ui.horizontal_wrapped(|ui| {
        for (label, sub, n) in [
            ("Schonend", "im Hintergrund", 1usize),
            ("Halbe Kraft", "empfohlen", (max / 2).max(1)),
            ("Volle Kraft", "wird laut", max),
        ] {
            let on = r.threads == n;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(150.0, 58.0), Sense::click());
            let p = ui.painter();
            p.rect(
                rect,
                theme::r_md(),
                if on {
                    pal().primary
                } else if resp.hovered() {
                    pal().hover
                } else {
                    pal().panel
                },
                Stroke::new(1.0_f32, if on { pal().primary } else { pal().frame }),
            );
            p.text(
                rect.left_top() + Vec2::new(14.0, 11.0),
                egui::Align2::LEFT_TOP,
                label,
                FontId::proportional(theme::BODY),
                if on { pal().on_fill } else { pal().text },
            );
            p.text(
                rect.left_top() + Vec2::new(14.0, 32.0),
                egui::Align2::LEFT_TOP,
                format!("{n} {} · {sub}", if n == 1 { "Kern" } else { "Kerne" }),
                FontId::proportional(theme::SMALL),
                if on { pal().on_fill_dim } else { pal().muted },
            );
            if resp.clicked() {
                r.threads = n;
            }
            ui.add_space(theme::S2);
        }
    });

    ui.add_space(theme::S3);

    // What this will actually cost, in the reader's terms.
    match r.preview() {
        Preview::Nothing => widgets::note(
            ui,
            pal().warn,
            "Es fehlt noch nichts. Geh zurück und lass mindestens ein Wort leer — sonst \
             gibt es nichts zu suchen.",
        ),
        Preview::Invalid(msg) => widgets::note(ui, pal().warn, &msg),
        Preview::Ready {
            candidates,
            secs,
            hopeless,
        } => {
            let secs = secs / r.threads.max(1) as f64;
            widgets::note(
                ui,
                if hopeless { pal().alert } else { pal().green },
                &format!(
                    "{} Möglichkeiten · geschätzte Dauer {}",
                    util::group_digits(candidates),
                    format_estimate(secs)
                ),
            );
            if hopeless {
                ui.add_space(theme::S2);
                widgets::note(
                    ui,
                    pal().alert,
                    "So viele fehlende Wörter sind praktisch nicht zu knacken. Erwarte \
                     nichts.",
                );
                ui.add_space(theme::S1);
                widgets::disclosure(
                    ui,
                    "retten_aussichtslos",
                    "Jedes fehlende Wort vervielfacht die Zahl der Möglichkeiten mit 2048. \
                     Bei drei oder vier fehlenden Wörtern ist das dieselbe \
                     Aussichtslosigkeit, die das Hauptfenster in Vielfachen des \
                     Universumsalters vorrechnet.\n\nDu darfst es trotzdem laufen lassen — \
                     das Programm hält dich nicht auf. Es sagt dir nur vorher, woran du \
                     bist.",
                );
            }
        }
    }

    ui.add_space(theme::S3);

    egui::Frame::none()
        .fill(theme::wash(pal().warn))
        .rounding(theme::r_md())
        .stroke(Stroke::new(1.0_f32, pal().warn))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Einmal lesen, bitte")
                    .color(pal().warn)
                    .size(theme::BODY)
                    .strong(),
            );
            ui.add_space(theme::S2);
            // Zwei Sätze, die jeder liest. Der Rest steht darunter für die, die
            // es genauer wissen wollen — vorher waren es drei Aufzählungspunkte
            // am Stück, und lange Warnungen werden weggeklickt statt gelesen.
            ui.label(
                RichText::new(
                    "Deine Wörter bleiben auf diesem Rechner. Gib sie niemals auf einer \
                     Webseite ein, die verspricht, sie wiederherzustellen.",
                )
                .color(pal().text)
                .size(theme::BODY),
            );
            ui.add_space(theme::S2);
            widgets::disclosure_labelled(
                ui,
                "retten_warnung",
                "Warum das wichtig ist",
                "Wer deine Wörter kennt, kann dein Guthaben ausgeben — sofort und ohne \
                 dass sich das rückgängig machen lässt. Webseiten, die eine \
                 „Wiederherstellung“ anbieten, sind der häufigste Weg, eine Wallet zu \
                 verlieren.\n\nDieses Programm probiert die Wörter nur hier auf der Platte \
                 aus und schickt sie nirgendwohin; nachprüfbar im Quelltext unter \
                 src/recover.rs. Wer ganz sichergehen will, macht so etwas an einem Gerät \
                 ohne Netzverbindung.",
            );
            ui.add_space(theme::S3);
            ui.checkbox(
                &mut r.acknowledged,
                RichText::new("Verstanden.")
                    .color(pal().text)
                    .size(theme::BODY),
            );

            // Die zweite Frage steht hier und nicht auf dem Ergebnisbildschirm,
            // weil sie hier noch eine Entscheidung ist. Hinterher — neben einer
            // gerade zurückgeholten Wallet — wäre sie nur noch eine Mitteilung,
            // und wer dort auf „ja" tippt, hat den Preis nicht abgewogen.
            // Voreinstellung aus: was den Rechner verlässt, verlässt ihn auf
            // Ansage.
            ui.add_space(theme::S2);
            ui.checkbox(
                &mut r.auto_balance,
                RichText::new("Kontostand nach dem Fund gleich online prüfen")
                    .color(pal().text)
                    .size(theme::BODY),
            );
            ui.add_space(theme::S1);
            ui.label(
                RichText::new(format!(
                    "Dabei gehen die Empfangsadressen dieser Wallet an {}. \
                     Ohne Haken steht danach ein Knopf dafür da.",
                    api_host(api)
                ))
                .color(pal().muted)
                .size(theme::SMALL),
            );
        });
}

/// Back and forward, in the same place on every screen.
fn recover_nav(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi, depth: u32) {
    use crate::recover_ui::Step;
    let last = r.step == Step::Start;
    let ready = r.can_start();

    ui.horizontal(|ui| {
        let first = r.step == Step::Length;
        let back = egui::Button::new(
            RichText::new("Zurück")
                .color(if first { pal().muted } else { pal().text })
                .size(theme::BODY),
        )
        .fill(pal().panel)
        .stroke(theme::hairline())
        .rounding(theme::r_sm())
        .min_size(Vec2::new(120.0, 42.0));
        if ui.add_enabled(!first, back).clicked() {
            r.step = r.step.prev();
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if last {
                let btn = egui::Button::new(
                    RichText::new("Suche starten")
                        .color(if ready { pal().on_fill } else { pal().muted })
                        .size(theme::BODY)
                        .strong(),
                )
                .fill(if ready { pal().primary } else { pal().inset })
                .rounding(theme::r_sm())
                .min_size(Vec2::new(190.0, 42.0));
                if ui.add_enabled(ready, btn).clicked() {
                    if let Err(e) = r.start(depth) {
                        eprintln!("recovery start failed: {e}");
                    }
                }
                if !ready {
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(match r.preview() {
                            crate::recover_ui::Preview::Ready { .. } => "Haken fehlt noch",
                            _ => "siehe Hinweis",
                        })
                        .color(pal().muted)
                        .size(theme::SMALL),
                    );
                }
            } else {
                let btn = egui::Button::new(
                    RichText::new("Weiter")
                        .color(pal().on_fill)
                        .size(theme::BODY)
                        .strong(),
                )
                .fill(pal().primary)
                .rounding(theme::r_sm())
                .min_size(Vec2::new(140.0, 42.0));
                if ui.add(btn).clicked() {
                    r.step = r.step.next();
                }
            }
        });
    });
}

/// The running screen: a progress bar, the count, elapsed time, and cancel.
/// Wie lange die laufende Suche noch braucht, aus der **gemessenen** Rate
/// dieses Rechners. `None`, solange es dafür zu früh ist.
///
/// Nicht [`crate::recover::estimate_secs`]: das rechnet mit zwei fest
/// verdrahteten Konstanten, die einmal auf einer bestimmten Maschine gemessen
/// wurden, und taugt für die Vorschau *vor* dem Start, wo es nichts zu messen
/// gibt. Sobald der Zähler läuft, kennt das Fenster die Wahrheit — „messen statt
/// schätzen", und hier ist das Messen sogar das Einfachere.
///
/// Die zwei Riegel sind der Grund für das `None`: unter zwei Sekunden und unter
/// einer vollen Melderunde ist die Rate ein Zufallswert, und eine Restzeit, die
/// mit „vier Stunden" anfängt und in der nächsten Sekunde „zwei Minuten" sagt,
/// ist schlimmer als keine.
fn remaining_secs(done: u64, total: u64, elapsed: f64) -> Option<f64> {
    if done < crate::recover::REPORT_EVERY || elapsed < 2.0 || done >= total {
        return None;
    }
    let rate = done as f64 / elapsed;
    if rate <= 0.0 {
        return None;
    }
    Some((total - done) as f64 / rate)
}

fn recover_running(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi) {
    use crate::recover_ui::Phase;
    let (done, total, secs) = match &r.phase {
        Phase::Running {
            counter,
            total,
            started,
            ..
        } => (
            counter.load(std::sync::atomic::Ordering::Relaxed),
            *total,
            // Sub-second precision, because the rate is computed from this.
            started.elapsed().as_secs_f64(),
        ),
        _ => return,
    };
    let frac = if total > 0 {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Beides geglättet, sonst zappelt es: der Zähler kommt in Sprüngen von
    // mehreren Arbeitern, und eine Restzeit, die viermal je Sekunde umspringt,
    // liest sich als Unsicherheit, nicht als Auskunft.
    let frac = crate::ui::feel::smooth(ui, "recover_frac", frac);
    let remaining = remaining_secs(done, total, secs)
        .map(|s| crate::ui::feel::smooth(ui, "recover_eta", s).max(0.0));

    ui.add_space(theme::S5);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new("Suche läuft …")
                .color(pal().text)
                .size(theme::TITLE)
                .strong(),
        );
        ui.add_space(theme::S1);
        ui.label(
            RichText::new("Das Fenster kann offen bleiben. Du kannst jederzeit abbrechen.")
                .color(pal().dim)
                .size(theme::SMALL),
        );
    });
    ui.add_space(theme::S4);

    // Progress bar. Die Rinne liegt tief, die Füllung trägt einen Glanz —
    // dieselbe Materialsprache wie der Ladebalken beim Start.
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
    ui.painter().rect_filled(rect, 7.0, pal().sunken);
    if frac > 0.0 {
        let mut filled = rect;
        filled.set_width((rect.width() * frac as f32).max(14.0));
        ui.painter().rect_filled(filled, 7.0, pal().primary);
        crate::ui::feel::sheen(ui.painter(), filled, pal().primary);
    }

    ui.add_space(theme::S2);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{:.1} %", frac * 100.0))
                .color(pal().primary)
                .font(mono(theme::TITLE))
                .strong(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Die Restzeit steht rechts außen, also am Ende der Zeile: sie ist
            // die Zahl, auf die jemand wartet, der sich fragt, ob es noch läuft.
            let rest = match remaining {
                Some(s) => format!("noch etwa {}", util::format_duration(s as u64)),
                None => "Restzeit wird gemessen …".to_string(),
            };
            ui.label(
                RichText::new(format!(
                    "{} / {}   ·   {}   ·   {rest}",
                    util::group_digits(done),
                    util::group_digits(total),
                    util::format_duration(secs as u64),
                ))
                .color(pal().dim)
                .size(theme::SMALL),
            );
        });
    });

    ui.add_space(theme::S4);
    ui.vertical_centered(|ui| {
        if ui
            .add(
                egui::Button::new(
                    RichText::new("Abbrechen")
                        .color(pal().text)
                        .size(theme::BODY),
                )
                .fill(pal().panel)
                .stroke(theme::hairline())
                .rounding(theme::r_sm())
                .min_size(Vec2::new(160.0, 34.0)),
            )
            .clicked()
        {
            r.cancel();
        }
    });
}

/// The result screen: the one seed, a short list of possibles, or nothing.
/// Was auf der wiederhergestellten Wallet liegt.
///
/// Zwei Auskünfte, bewusst getrennt gehalten: die lokale Fundliste kostet nichts
/// und bleibt auf diesem Rechner, weiß aber nur von den Adressen, die geladen
/// wurden — bei der Übungsliste also von keiner echten. Die Online-Abfrage nennt
/// die Wahrheit und kostet eine Entscheidung, die niemand ungefragt für den
/// Leser trifft.
///
/// Beim Übungslauf gibt es keinen Knopf: eine erfundene Adresse an einen fremden
/// Dienst zu schicken hat keinen Zweck.
fn draw_balance(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi, api: &str) {
    use crate::recover_ui::Balance;

    /// Was die Zahl abdeckt — und was nicht. Ohne diesen Satz liest sich eine
    /// Null als „die Wallet ist leer", und das steht hier nicht.
    ///
    /// Seit die Suche dem Gap-Limit folgt, ist die Zahl nicht mehr durch drei
    /// teilbar: jede der drei Ketten hört für sich auf, sobald zwanzig leere
    /// Adressen hintereinander stehen. Darum die Gesamtzahl und der Grund
    /// dafür, statt einer Zahl je Pfad, die es so nicht mehr gibt.
    fn scope_note(checked: usize) -> String {
        format!(
            "Geprüft: {checked} Adressen — je Pfad so weit, bis {} leere \
             hintereinander kamen. Genauso sucht ein Wallet-Programm.",
            crate::balance::GAP_LIMIT
        )
    }

    /// Der Betrag als erste Zeile der Karte: die Zahl groß und rechts, darunter
    /// leise, woher sie kommt. Ohne die Herkunft sind „laut deiner Fundliste"
    /// und „laut mempool.space" dieselbe Zahl — und das sind sie nicht.
    fn amount(ui: &mut Ui, sats: u64, source: &str) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Guthaben")
                    .color(pal().dim)
                    .size(theme::SMALL),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(util::format_btc(sats))
                        .color(if sats > 0 { pal().gold } else { pal().dim })
                        .font(mono(theme::TITLE))
                        .strong(),
                );
            });
        });
        quiet(ui, source);
    }

    /// Eine leise Zeile unter einem Wert — kein eigener Kasten mehr.
    fn quiet(ui: &mut Ui, text: &str) {
        ui.label(RichText::new(text).color(pal().muted).size(theme::SMALL));
        ui.add_space(theme::S1);
    }

    if r.practice {
        widgets::note(
            ui,
            pal().accent,
            "Übungs-Seed — dazu gibt es keine echte Wallet, also auch keinen Kontostand.",
        );
        return;
    }

    // Der Haken von Schritt vier: einmal, sobald es etwas zu fragen gibt.
    // Hier und nicht in `poll`, weil erst der Zeichencode weiß, welcher Dienst
    // eingetragen ist — und weil dieser Bildschirm ohnehin der einzige ist,
    // auf dem ein Kontostand je gezeigt wird.
    if r.auto_balance && !r.auto_asked && matches!(r.balance, Balance::Unknown | Balance::NotListed)
    {
        r.auto_asked = true;
        r.ask_online(api);
    }

    match r.balance.clone() {
        Balance::Unknown => {}
        Balance::Local(sum) => {
            amount(ui, sum.sats, "laut deiner lokalen Fundliste");
            quiet(ui, &scope_note(sum.checked));
        }
        Balance::Online(sum) => {
            amount(ui, sum.sats, &format!("laut {}", api_host(api)));
            quiet(ui, &scope_note(sum.checked));
        }
        Balance::Asking => {
            widgets::kv(ui, "Guthaben", "wird abgefragt …", pal().dim);
            quiet(
                ui,
                &format!(
                    "Bis zu {} Adressen je Pfad — das dauert einen Moment.",
                    crate::balance::GAP_LIMIT
                ),
            );
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        Balance::Failed(e) => {
            widgets::kv(ui, "Guthaben", "nicht abrufbar", pal().warn);
            quiet(ui, &e);
            quiet(
                ui,
                "Das sagt nichts über die Seed: dass die Wörter stimmen, hat die Suche \
                 schon bewiesen.",
            );
            balance_button(ui, r, api, "Nochmal versuchen");
        }
        Balance::NotListed => {
            widgets::kv(ui, "Guthaben", "unbekannt", pal().dim);
            quiet(
                ui,
                "Steht nicht in deiner lokalen Fundliste. Das heißt nicht, dass die Wallet \
                 leer ist — die Liste kennt nur die Adressen, die du geladen hast.",
            );
            balance_button(ui, r, api, "Kontostand online prüfen");
        }
    }
}

/// Der Knopf für die Online-Abfrage, samt Aufklapper, der vorher sagt, was
/// passiert.
///
/// Der Aufklapper ist keine Höflichkeit: hier verlässt zum ersten Mal etwas den
/// Rechner, das aus der Seed des Lesers abgeleitet ist. Wer das drückt, soll
/// wissen, wer es zu sehen bekommt und wie man das ändert.
fn balance_button(ui: &mut Ui, r: &mut crate::recover_ui::RecoverUi, api: &str, label: &str) {
    ui.add_space(theme::S2);
    if ui.add(widgets::button_signal(label, pal().warn)).clicked() {
        r.ask_online(api);
    }
    ui.add_space(theme::S1);
    widgets::disclosure(
        ui,
        "kontostand_online",
        &format!(
            "Dabei gehen die ersten Adressen dieser Wallet an {api} — und nur die. \
             Deine Wörter bleiben hier; sie werden nirgendwohin geschickt, weder jetzt \
             noch sonst.\n\n\
             Wer diesen Dienst betreibt, sieht dadurch, dass jemand von deiner \
             Internetverbindung nach genau diesen Adressen gefragt hat. Hast du einen \
             eigenen Node, trag ihn in der config.toml unter [balance] als api ein — dann \
             sieht das niemand außer dir.\n\n\
             Der Kontostand ist kein Beweis für irgendetwas: dass die Wörter zur Adresse \
             passen, hat die Suche schon gezeigt."
        ),
    );
}

fn recover_done(
    ui: &mut Ui,
    r: &mut crate::recover_ui::RecoverUi,
    _keep_open: &mut bool,
    balance_api: &str,
) {
    use crate::recover_ui::Phase;
    let (hits, truncated) = match &r.phase {
        Phase::Done(o) => (o.hits.clone(), o.truncated),
        _ => (Vec::new(), false),
    };

    ui.add_space(theme::S4);
    if hits.is_empty() {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Nichts gefunden")
                    .color(pal().warn)
                    .size(theme::DISPLAY)
                    .strong(),
            );
        });
        ui.add_space(theme::S3);
        widgets::note(
            ui,
            pal().dim,
            "So lässt sich die Seed nicht rekonstruieren. Prüf die Wörter, oder \
             markiere mehr als unsicher.",
        );
    } else if hits.len() == 1 || hits[0].is_funded() {
        let f = &hits[0];
        let funded = f.is_funded();
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(if funded {
                    "Wallet mit Guthaben gefunden!"
                } else {
                    "Gefunden!"
                })
                .color(if funded { pal().gold } else { pal().green })
                .size(theme::DISPLAY)
                .strong(),
            );
            ui.add_space(theme::S1);
            ui.label(
                RichText::new("Schreib die Wörter jetzt auf Papier. Nirgends sonst.")
                    .color(pal().dim)
                    .size(theme::BODY),
            );
        });
        ui.add_space(theme::S3);
        if let Some(sats) = f.balance_sats {
            // The number somebody came here for, given its own line.
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(util::format_btc(sats))
                        .color(pal().gold)
                        .font(mono(theme::DISPLAY))
                        .strong(),
                );
            });
            ui.add_space(theme::S3);
        }
        egui::Frame::none()
            .fill(theme::wash(pal().green))
            .rounding(theme::r_md())
            .stroke(Stroke::new(
                1.0_f32,
                if funded { pal().gold } else { pal().green },
            ))
            .inner_margin(egui::Margin::symmetric(16.0, 14.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                widgets::seed_grid(ui, "seed_words", &f.mnemonic, 3);
            });
        ui.add_space(theme::S3);
        // Pfad, Adresse und Kontostand standen hier einmal als drei einzeln
        // gerahmte Notizen untereinander, mit der großen Zahl dazwischen —
        // vier Kästen für vier Angaben, die zusammengehören. Jetzt ist es eine
        // Karte mit einer Werteliste: gleiche Beschriftungsspalte, gleiche
        // Zahlenspalte, und die Zahl, deretwegen jemand hier ist, oben in Gold.
        widgets::card(ui, "DIESE WALLET", pal().green, |ui| {
            draw_balance(ui, r, balance_api);
            widgets::kv(ui, "Adresse", &f.address, pal().text);
            widgets::kv(ui, "Pfad", &f.path, pal().dim);
        });
        if funded {
            widgets::note(
                ui,
                pal().green,
                "Gespeichert, samt Sicherungskopie — auf der Platte, bevor irgendeine \
                 Meldung rausging.",
            );
        }
    } else {
        // No target address was given, so several seeds are mathematically
        // valid. The owner picks theirs by the first address, or tries them.
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(format!("{} mögliche Seeds", hits.len()))
                    .color(pal().green)
                    .size(theme::DISPLAY)
                    .strong(),
            );
        });
        ui.add_space(theme::S2);
        widgets::note(
            ui,
            pal().dim,
            "Vergleich die erste Adresse jeder Seed mit deiner Wallet — oder gib eine \
             Adresse an, dann bleibt genau eine übrig.",
        );
        if truncated {
            widgets::note(
                ui,
                pal().warn,
                "Es gibt noch mehr als die hier gezeigten. Grenze mit einer Adresse ein.",
            );
        }
        ui.add_space(theme::S2);
        for (n, f) in hits.iter().enumerate() {
            egui::Frame::none()
                .fill(pal().panel)
                .rounding(theme::r_sm())
                .stroke(theme::hairline())
                .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("#{}", n + 1))
                                .color(pal().muted)
                                .size(theme::SMALL),
                        );
                        ui.label(
                            RichText::new(&f.address)
                                .color(pal().primary)
                                .font(mono(theme::SMALL)),
                        );
                    });
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(&f.mnemonic)
                            .color(pal().text)
                            .font(mono(theme::BODY)),
                    );
                });
            ui.add_space(theme::S2);
        }
    }

    ui.add_space(theme::S3);
    ui.vertical_centered(|ui| {
        if ui
            .add(
                egui::Button::new(
                    RichText::new("Neue Suche")
                        .color(pal().on_fill)
                        .size(theme::BODY)
                        .strong(),
                )
                .fill(pal().primary)
                .rounding(theme::r_sm())
                .min_size(Vec2::new(180.0, 36.0)),
            )
            .clicked()
        {
            let wc = r.word_count;
            *r = crate::recover_ui::RecoverUi::default();
            r.resize(wc);
        }
    });

    // Die Bitte steht ganz zuletzt, unter allem — die Wörter, der Kontostand
    // und die Aufforderung, sie auf Papier zu schreiben, gehen vor. Und sie
    // steht nur da, wenn wirklich etwas zurückkam: nach einem Fehlschlag um
    // Geld zu bitten wäre schäbig, und ein Übungslauf hat nichts gerettet,
    // worüber sich jemand freuen könnte.
    if !hits.is_empty() && !r.practice {
        donation_note(ui, ASK_RECOVERED);
    }
    ui.add_space(theme::S3);
}

impl eframe::App for GuiApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::clear_color()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // An external stop (--duration, a signal) must close the window too,
        // or it sits there with frozen counters and no way to tell that apart
        // from a stall.
        if self.control.stopping() && self.shot_path.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Farben und Grundstil für dieses Bild, der Systemeinstellung folgend.
        theme::apply(ctx);

        self.absorb_loading();
        self.drain();
        self.announce(ctx);
        self.sample_rate();

        // Repaint on a timer rather than continuously: the numbers change a few
        // times a second and the cores are needed elsewhere.
        ctx.request_repaint_after(Duration::from_millis(120));

        // Vor dem Zeichnen, damit eine Taste, die einen Bildschirm schließt, im
        // selben Bild wirkt statt ihn noch eines länger stehen zu lassen.
        if self.shot_path.is_none() && takes_keys(&self.screen) {
            self.handle_keys(ctx);
        }

        // Vor dem `match` und damit über jedem Bildschirm — auch über solchen,
        // die es noch nicht gibt. Ein Fund ist quer zum Bildschirm, also darf
        // seine Meldung nicht an einem einzelnen hängen.
        self.draw_find_band(ctx);

        // Ein Feld, ein `match`. Ein neuer Bildschirm ist eine neue Variante,
        // kein weiterer Zweig in einer Kette, deren Korrektheit an ihrer
        // Reihenfolge hängt.
        match &self.screen {
            Screen::Loading => self.draw_loading(ctx),
            Screen::Failed { .. } => self.draw_failed(ctx),
            Screen::Intro { until } => {
                let until = *until;
                if self.tick_intro() {
                    ctx.request_repaint();
                } else {
                    self.draw_intro(ctx, until);
                }
            }
            Screen::Chooser => self.draw_chooser(ctx),
            Screen::Dashboard => self.draw_dashboard(ctx),
            Screen::Recover { .. } => self.draw_recover(ctx),
        }

        self.handle_screenshot(ctx);
    }
}

/// Ob die Tastaturbehandlung auf diesem Bildschirm überhaupt etwas zu tun hat.
///
/// Nicht beim Laden und nicht auf dem Fehlerbildschirm — dort gibt es nichts zu
/// bedienen —, und nicht auf der Gabelung, wo noch keine Suche läuft, die eine
/// Leertaste anhalten könnte.
fn takes_keys(screen: &Screen) -> bool {
    matches!(
        screen,
        Screen::Dashboard | Screen::Intro { .. } | Screen::Recover { .. }
    )
}

/// Eine Abschnittsüberschrift in der Detailspalte.
///
/// Großbuchstaben, klein, in der Farbe des Abschnitts — dieselbe Hierarchie,
/// die eine Karte über ihren Titel herstellt, nur ohne die 57 Punkte Rahmen
/// darum herum.
fn section(ui: &mut Ui, title: &str, colour: Color32) {
    ui.add_space(theme::S3);
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 11.0), Sense::hover());
        ui.painter().rect_filled(bar, theme::r_xs(), colour);
        ui.add_space(theme::S2);
        ui.label(
            RichText::new(title)
                .color(colour)
                .size(theme::SMALL)
                .strong(),
        );
    });
    ui.add_space(theme::S1);
}

/// Kürzt eine lange Zeichenkette in der Mitte: `bc1qcr8te4kr609…306fyu`.
///
/// Für Adressen in einer schmalen Spalte. Anfang und Ende sind die Teile, an
/// denen man eine bech32-Adresse wiedererkennt — die Mitte ist Prüfsumme und
/// Nutzlast und sagt dem Auge nichts. Wer die vollständige Adresse braucht,
/// klappt die Zeile auf.
///
/// Passt die Zeichenkette ohnehin, bleibt sie unverändert; es wird also nie
/// etwas gekürzt, um dann drei Zeichen zu sparen.
fn shorten_middle(s: &str, head: usize, tail: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= head + tail + 1 {
        return s.to_string();
    }
    let front: String = chars[..head].iter().collect();
    let back: String = chars[chars.len() - tail..].iter().collect();
    format!("{front}…{back}")
}

/// Was im leeren Fundfach steht.
///
/// Als freie Funktion, damit ein Test den Wortlaut gegen die Ehrlichkeitsregel
/// prüfen kann: Konjunktiv statt Ankündigung, und **keine Zahl**. Der Satz darf
/// beschreiben, was passieren *würde* — er darf nicht andeuten, dass es
/// passiert.
fn find_section_empty_text() -> (&'static str, &'static str) {
    (
        "Hier stünde eine Wallet mit Guthaben.",
        // Kurz genug für eine Zeile in der 308 Punkte schmalen Spalte. Was
        // genau passiert, steht ausführlich im Aufklapper darunter.
        "Passiert das, hörst du es.",
    )
}

/// Überschrift und Text des Fundbands.
///
/// Eine freie Funktion, damit der Wortlaut ohne Fenster prüfbar ist — und damit
/// die Fallunterscheidung an einer Stelle steht statt verteilt im Zeichencode.
fn find_band_text(count: usize, unsaved: bool, in_recover: bool) -> (String, String) {
    let title = if unsaved {
        "Eine Wallet mit Guthaben — aber nicht gespeichert".to_string()
    } else if count == 1 {
        "Eine Wallet mit Guthaben wurde gefunden.".to_string()
    } else {
        format!("{count} Wallets mit Guthaben wurden gefunden.")
    };

    let body = if unsaved {
        // Der Fall, in dem es auf jede Sekunde ankommt: die Wörter stehen nur
        // noch im Arbeitsspeicher dieses Prozesses.
        "Das Speichern ist fehlgeschlagen. Sieh sie dir jetzt an und schreib sie ab.".to_string()
    } else if in_recover {
        "Sie liegt sicher auf der Platte und wartet, bis du hier fertig bist.".to_string()
    } else {
        "Gespeichert in deiner Fundliste, samt Sicherungskopie.".to_string()
    };

    (title, body)
}

/// Überschrift und Text des Fehlerbands.
///
/// Eine freie Funktion, damit der Wortlaut ohne Fenster prüfbar ist. Die Anzahl
/// macht den Unterschied: „ein Treffer" ist ein Vorfall, „vier" ist ein Zustand.
///
/// Der wichtigste Satz ist der letzte. Wenn das Speichern scheitert, existiert
/// die Seed nur noch im Arbeitsspeicher dieses Programms — wer das Fenster
/// schließt, ohne sie abzuschreiben, hat sie verloren.
fn error_banner_text(errors: &[String]) -> (String, String) {
    let title = match errors.len() {
        0 | 1 => "Ein Treffer wurde nicht gespeichert".to_string(),
        n => format!("{n} Meldungen — es wurde nicht alles gespeichert"),
    };
    let newest = errors.last().map(String::as_str).unwrap_or("");
    let body = format!(
        "{newest}\n\nMeist ist die Platte voll oder der Ordner nicht beschreibbar. \
         Die Wörter stehen unten im Fenster — schreib sie ab, bevor du das Programm \
         schließt."
    );
    (title, body)
}

/// Where an arrow key moves the selection in the hit list.
///
/// Pulled out of the key handler so the edges can be tested without a window:
/// an empty list, nothing selected yet, and both ends of a full one. Stops at
/// the ends rather than wrapping — a list of found wallets is not a carousel,
/// and silently jumping from the last to the first would misread as a new hit.
pub(crate) fn step_selection(current: Option<usize>, len: usize, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    Some(match current {
        // Nothing chosen yet: enter the list from the end the key points at.
        None => {
            if down {
                0
            } else {
                last
            }
        }
        Some(i) if down => (i + 1).min(last),
        Some(i) => i.saturating_sub(1),
    })
}

impl GuiApp {
    /// Keyboard control of the window.
    ///
    /// The README promised these keys long before the window had any; they
    /// were the terminal interface's, and pressing them here did nothing.
    ///
    /// The hazard worth naming is text input. The recovery screen is two dozen
    /// text fields, and a space typed into a word must land in the word rather
    /// than stopping the search behind it — so while anything has keyboard
    /// focus, every key belongs to it and none of the rules below apply.
    ///
    /// There is deliberately no bare "q" for quit. In a terminal that is the
    /// convention and the cost of a mis-press is a restart; here the platform
    /// already has ⌘Q, and a single stray keystroke ending a search that has
    /// run for days is not a trade worth making.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Consumed rather than read, so the platform shortcut never also
        // reaches whatever widget happens to be under the pointer. Deliberately
        // ahead of the focus check below: ⌘, opens preferences on this platform
        // whatever is being typed into.
        //
        // Ignored while the recovery screen is up, where the settings drawer
        // does not exist: it would set a flag nobody asked for and spring the
        // drawer open on return.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma))
            && !self.screen.is_recover()
        {
            self.settings_open = !self.settings_open;
        }

        if ctx.wants_keyboard_input() {
            // Escape leaves the field first. A second Escape then reaches the
            // rules below and leaves the screen, which is the order every
            // other application on the machine uses.
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
            return;
        }

        let (space, esc, up, down) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
            )
        });

        // Das Intro ist ein Vorhang, keine Wartezeit. Wer es einmal gesehen hat,
        // darf es überspringen — jetzt, indem der Zustand weiterrückt, statt
        // eine Uhr zurückzudatieren.
        if matches!(self.screen, Screen::Intro { .. }) {
            if space || esc || up || down {
                self.screen = Screen::Chooser;
            }
            return;
        }

        // Escape backs out of wherever the reader is, innermost first.
        if esc {
            if self.screen.is_recover() {
                self.leave_recover();
            } else if self.info_open.is_some() {
                self.info_open = None;
            } else if self.settings_open {
                self.settings_open = false;
            }
            return;
        }

        // The recovery screen owns the rest of the keyboard. Pausing a search
        // that is not on screen, or walking a hit list nobody can see, would
        // be action at a distance.
        if !self.screen.takes_shortcuts() {
            return;
        }

        if space {
            self.control.toggle_paused();
        }
        if up || down {
            if let Some(next) = step_selection(self.selected, self.hits.len(), down) {
                self.selected = Some(next);
            }
        }
    }

    /// Builds the practice database and then loads again, both on one thread
    /// off the interface, reporting through the loading screen the reader has
    /// already seen once.
    ///
    /// Failure here comes back the same way any load failure does — through
    /// [`Progress::fail`], onto this same screen — except that the second time
    /// the file exists, so no further offer is made and the reader is not sent
    /// round the loop again.
    fn build_practice_db(&mut self) {
        let Screen::Failed {
            repairable: Some(path),
            ..
        } = &self.screen
        else {
            return;
        };
        let path = path.clone();
        let (Some(progress), Some(boot)) = (self.progress.clone(), self.boot.clone()) else {
            return;
        };
        if self.repairing {
            return;
        }
        self.repairing = true;
        let records = self.practice_records;

        progress.restart();
        self.screen = Screen::Loading;
        self.loading_pending = true;
        self.started = Instant::now();
        // Von hier an ist die geladene Liste nachweislich eine Übungsliste. Das
        // Dashboard sagt es dauerhaft — vorher stand es einmal auf diesem
        // Bildschirm und danach nie wieder, und erfundene Adressen waren von
        // echten nicht mehr zu unterscheiden.
        self.practice_list = true;

        std::thread::spawn(move || {
            if let Err(e) = crate::startup::create_practice_db(&path, records, &progress) {
                progress.fail(e);
                return;
            }
            // Straight on into the normal load, which finishes the bar and
            // starts the engine exactly as it would have on a good first run.
            let _ = boot();
        });
    }

    /// The opening fork: Schatzsuche on the left, Seed retten on the right.
    ///
    /// Two equal doors rather than one door with the second mode tucked into a
    /// header button. Recovery is the honest, useful half — someone getting
    /// their own wallet back — and it deserves to be met as an equal the moment
    /// the program opens.
    fn draw_chooser(&mut self, ctx: &egui::Context) {
        let logo = self.logo_texture(ctx);
        let (art_search, art_recover) = self.door_textures(ctx);

        let mut pick_search = false;
        let mut pick_recover = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(pal().bg))
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
                self.draw_map(ui, ui.clip_rect());
                // A gentle fade-and-rise on arrival, matching the wizard.
                let fade = arrival_fade(ui, "chooser_at");
                ui.multiply_opacity(fade);
                ui.vertical_centered(|ui| {
                    let h = ui.available_height();
                    ui.add_space((h * 0.12 + (1.0 - fade) * 12.0).max(18.0));
                    ui.add(egui::Image::new(&logo).fit_to_exact_size(Vec2::splat(58.0)));
                    ui.add_space(theme::S3);
                    // Versalien wollen gesperrt werden: sie haben keine
                    // Ober- und Unterlängen, an denen das Auge die
                    // Buchstaben trennt, und stehen dicht gesetzt als Block
                    // da. Acht Prozent der Schriftgröße ist das übliche Maß
                    // für eine Wortmarke — bei sechsundzwanzig Punkten also
                    // gut zwei.
                    outlined_label(
                        ui,
                        "SCHATZSUCHE",
                        pal().accent,
                        theme::wordmark(ui.ctx(), theme::DISPLAY),
                        theme::DISPLAY * 0.08,
                    );
                    ui.add_space(theme::S2);
                    // Die Frage bleibt ungesperrt: gemischter Satz braucht
                    // keine Sperrung, und gesperrte Kleinbuchstaben lesen
                    // sich langsamer.
                    outlined_label(
                        ui,
                        "Was möchtest du tun?",
                        pal().dim,
                        FontId::proportional(theme::TITLE),
                        0.0,
                    );
                    ui.add_space(theme::S5);

                    // Two doors, side by side, centred as a pair.
                    let gap = 26.0;
                    let total = ui.available_width().min(760.0);
                    let pw = ((total - gap) / 2.0).clamp(150.0, 360.0);
                    let ph = 316.0_f32.min((ui.available_height() - 40.0).max(220.0));
                    ui.horizontal(|ui| {
                        let pad = (ui.available_width() - (pw * 2.0 + gap)) / 2.0;
                        ui.add_space(pad.max(0.0));
                        if widgets::door(
                            ui,
                            Vec2::new(pw, ph),
                            pal().primary,
                            &art_search,
                            // Nicht „Schatzsuche": so heißt das ganze Programm.
                            // Eine der zwei Türen mit dem Namen des Hauses zu
                            // beschriften machte sie zur eigentlichen und die
                            // andere zum Anhängsel — und gerade das sollen sie
                            // nicht sein.
                            "Wallets würfeln",
                            "Der Rechner errät Bitcoin-Wallets, so schnell er kann — und \
                             rechnet vor, warum nie eine dabei ist.",
                            // Nicht „Starten": die Tür startet die Suche nicht
                            // mehr, das tut erst der Knopf dahinter. Eine
                            // Aufschrift, die etwas anderes tut als sie sagt,
                            // ist genau der Fehler, den dieses Fenster nicht
                            // machen soll.
                            "Öffnen",
                        ) {
                            pick_search = true;
                        }
                        ui.add_space(gap);
                        if widgets::door(
                            ui,
                            Vec2::new(pw, ph),
                            pal().green,
                            &art_recover,
                            "Seed retten",
                            "Dir fehlt ein Teil deiner eigenen Seed? Trag ein, was du noch \
                             hast — den Rest findet das Programm.",
                            "Wiederherstellen",
                        ) {
                            pick_recover = true;
                        }
                    });
                });
            });

        if pick_search {
            self.enter_dashboard();
        } else if pick_recover {
            self.open_recover(Screen::Dashboard);
        }
    }

    /// Der Vorhang zwischen Laden und Oberfläche.
    ///
    /// Hier stand einmal ein Fortschrittsbalken, der sich über drei Sekunden
    /// füllte, und ein Countdown von drei auf eins — auf einem Bildschirm, auf
    /// dem nichts gewartet wird, weil die Datenbank längst geladen ist. Das ist
    /// die Bildsprache von „gleich passiert etwas", und sie steht bei einem
    /// Programm, dessen ganze Aussage „hier passiert nie etwas" lautet, an der
    /// denkbar falschesten Stelle: auf dem ersten Bild, das jemand sieht.
    ///
    /// Geblieben ist ein Aufblenden von knapp einer Sekunde. Jede Taste und
    /// jeder Klick überspringt es.
    fn draw_intro(&mut self, ctx: &egui::Context, until: Instant) {
        ctx.request_repaint();
        let left = until
            .saturating_duration_since(Instant::now())
            .as_secs_f32();
        let t = INTRO.as_secs_f32() - left;
        let tex = self.logo_texture(ctx);
        let funded = format::thousands(self.funded_count);
        let threads = self.threads;

        let alpha = |start: f32| ((t - start) / 0.30).clamp(0.0, 1.0);
        let tint = |c: Color32, a: f32| {
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8)
        };

        let mut skip = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(pal().bg))
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
                if ui
                    .interact(ui.max_rect(), ui.id().with("intro_skip"), Sense::click())
                    .clicked()
                {
                    skip = true;
                }
                ui.vertical_centered(|ui| {
                    let h = ui.available_height();
                    ui.add_space((h * 0.24).max(theme::S3));

                    ui.add(
                        egui::Image::new(&tex)
                            .fit_to_exact_size(Vec2::splat(180.0))
                            .tint(Color32::from_white_alpha((alpha(0.0) * 255.0) as u8)),
                    );

                    ui.add_space(theme::S4);
                    // Auf dem Vorhang darf die Marke weiter atmen als auf der
                    // Gabelung — achtzehn Prozent statt acht. Vorher standen
                    // dafür Leerzeichen im Text; die sind bei dieser Schrift
                    // ein Viertel der Schriftgröße breit und machten aus dem
                    // Namen eine Reihe einzelner Buchstaben.
                    spaced_label(
                        ui,
                        "SCHATZSUCHE",
                        tint(pal().text, alpha(0.12)),
                        theme::wordmark(ui.ctx(), theme::DISPLAY),
                        theme::DISPLAY * 0.18,
                    );
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(format!(
                            "{funded} Adressen geladen  ·  {threads} Kerne bereit"
                        ))
                        .color(tint(pal().dim, alpha(0.25)))
                        .size(theme::SMALL),
                    );
                });
            });
        if skip {
            self.screen = Screen::Chooser;
        }
    }

    /// The seed-recovery screen. Owns the whole window while open.
    ///
    /// The state lives in [`crate::recover_ui`]; this only draws it. The search
    /// runs on its own thread, so every frame polls for its result and, while
    /// it runs, asks for a repaint to keep the progress bar moving.
    fn draw_recover(&mut self, ctx: &egui::Context) {
        use crate::recover_ui::Phase;

        // Herausgenommen, um ihn veränderlich zeichnen zu können, und am Ende
        // zurückgelegt — oder durch den Rückweg ersetzt, wenn geschlossen wird.
        let (mut r, back) = match std::mem::replace(&mut self.screen, Screen::Dashboard) {
            Screen::Recover { ui, back } => (ui, back),
            other => {
                self.screen = other;
                return;
            }
        };
        r.poll();
        if matches!(r.phase, Phase::Running { .. }) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let logo = self.logo_texture(ctx);
        let depth = self.control.addresses_per_path().max(20);
        // Wie `depth` vorab herausgeholt: die Zeichen-Abschlüsse unten dürfen
        // `self` nicht mehr ausleihen.
        let api = self.balance_api.clone();
        // Von hier führt derselbe Weg zurück wie von der Suche: auf die
        // Startseite mit den zwei Türen. Vorher hieß der Knopf „Zurück zur
        // Suche" und sprang in den anderen Betriebsmodus — dieselbe Abkürzung,
        // die auf der Suche jetzt auch nicht mehr genommen wird.
        //
        // Eine Ausnahme: Wer vom Fehlerbildschirm kommt, hat keine
        // Adressdatenbank. Die Gabelung führte ihn dann auf ein Dashboard ohne
        // Daten, also in dieselbe Sackgasse, aus der er gerade kam. Für den
        // führt „Zurück" weiterhin dorthin, wo das Programm anbietet, die Liste
        // anzulegen.
        let from_failure = matches!(*back, Screen::Failed { .. });
        let back_label = if from_failure { "Zurück" } else { "Hub" };
        let back_hint = if from_failure {
            "Zurück zum Hinweis, dass die Adressliste fehlt"
        } else {
            "Zurück zur Startseite mit den zwei Türen"
        };

        // Set false to close the screen; set to restart with a blank form.
        let mut keep_open = true;

        egui::TopBottomPanel::top("recover_head")
            .frame(
                egui::Frame::none()
                    .fill(pal().bg)
                    .inner_margin(egui::Margin::symmetric(16.0, 12.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new(&logo).fit_to_exact_size(Vec2::splat(30.0)));
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new("SEED WIEDERHERSTELLEN")
                            .color(pal().text)
                            .size(theme::TITLE)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::header_button(ui, back_label)
                            .on_hover_text(back_hint)
                            .clicked()
                        {
                            crate::ui::feel::bump(crate::ui::feel::Bump::Switch);
                            keep_open = false;
                        }
                        // Auch hier der Wegweiser, damit ein Fund nicht
                        // deshalb unbemerkt bleibt, weil jemand gerade seine
                        // eigene Seed rettet. Er führt hier bewusst nirgendwo
                        // hin — er sagt nur, dass etwas da ist und wartet.
                        if self.draw_find_chip(ui) {
                            crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
                        }
                        ui.add_space(theme::S2);
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(pal().bg)
                    .inner_margin(egui::Margin::symmetric(16.0, 10.0)),
            )
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
                let fade = arrival_fade(ui, "recover_at");
                ui.multiply_opacity(fade);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let lead = centring_lead(ui, "recover_body_h");
                        ui.add_space(lead);
                        // A comfortable reading column, centred.
                        let width = ui.available_width().min(680.0);
                        let pad = (ui.available_width() - width) / 2.0;
                        let body = ui.horizontal(|ui| {
                            ui.add_space(pad.max(0.0));
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                match &r.phase {
                                    Phase::Editing => {
                                        recover_form(ui, &mut r, &mut keep_open, depth, &api)
                                    }
                                    Phase::Running { .. } => recover_running(ui, &mut r),
                                    Phase::Done(_) => {
                                        recover_done(ui, &mut r, &mut keep_open, &api)
                                    }
                                }
                            });
                        });
                        remember_body_height(ui, "recover_body_h", body.response.rect.height());
                    });
            });

        if keep_open {
            self.screen = Screen::Recover { ui: r, back };
        } else {
            // Stop any running search before dropping the state.
            r.cancel();
            // Alle Ankunftsstempel weg, damit der nächste Besuch in jeder
            // Richtung wieder aufblendet statt hart zu erscheinen.
            ctx.memory_mut(|m| {
                m.data.remove::<f64>(egui::Id::new("recover_at"));
                m.data.remove::<f64>(egui::Id::new("dashboard_at"));
                m.data.remove::<f64>(egui::Id::new("chooser_at"));
            });
            // Der Rückweg ist die Gabelung — außer für den, der aus dem
            // Fehlerbildschirm kam: den führte sie auf ein Dashboard ohne
            // Adressliste, also zurück in seine Sackgasse.
            self.screen = if from_failure { *back } else { Screen::Chooser };
        }
    }

    /// Das Hauptfenster.
    ///
    /// Hier standen einmal sieben Felder mit rund fünfzehn Zahlen gleichzeitig:
    /// Tempo, Durchschnitt, Spitze, pro Kern, Adressen, Laufzeit, Fehlalarme,
    /// abgesuchter Anteil, genauer Anteil, Wortlänge, Suchraum, Datenbank,
    /// Speicher, Verlauf, Fachwerte. Wer das Programm zum ersten Mal öffnet,
    /// liest davon nichts — er wird davon erschlagen.
    ///
    /// Die Mitte beantwortet vier Fragen und sonst keine: Läuft es? Wie
    /// schnell? Wird es etwas finden? Wie halte ich es an. Die Fachzahlen
    /// stehen daneben in einer eigenen Spalte, wo sie niemanden erschlagen —
    /// aber ständig da sind, wenn man hinsieht.
    fn draw_dashboard(&mut self, ctx: &egui::Context) {
        let seeds = self.stats.seeds();
        let now_rate = self.rate.history().last().copied().unwrap_or(0) as f64;
        let avg = self.rate.average();
        let paused = self.control.paused();
        let tex = self.logo_texture(ctx);
        // Innerhalb der Panel-Closures ist `self` ausgeliehen; der Wechsel des
        // Bildschirms muss also bis danach warten.
        let mut to_hub = false;

        let mut jump_to_find = false;
        self.draw_head(ctx, &tex, paused, &mut to_hub, &mut jump_to_find);
        self.draw_foot(ctx);
        // Zwischen den Leisten und dem Inhalt: egui gibt jedem Panel den Platz,
        // den die vorherigen übrig lassen, und eine später geöffnete Schublade
        // würde sonst über die Szene gemalt.
        //
        // Die rechte Spalte trägt entweder die Einstellungen oder die Details,
        // nie beides. Zwei Spalten nebeneinander ließen im kleinsten erlaubten
        // Fenster keine 200 Punkte für die Szene übrig, und die Breite der
        // Mitte würde bei jedem Öffnen der Einstellungen springen.
        if self.settings_open {
            self.draw_settings(ctx);
        } else {
            self.draw_details_panel(ctx, seeds, avg);
        }

        // Was der Mitte nach Kopfleiste, Fußleiste und rechter Spalte bleibt.
        // Von hier gemessen, weil es drinnen zu spät ist: die verschachtelten
        // Layouts haben ihren Platz dann schon unter sich aufgeteilt.
        let avail = ctx.available_rect().height() - theme::S3;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(pal().bg)
                    .inner_margin(egui::Margin::symmetric(theme::S3, theme::S2)),
            )
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
                // Dieselbe weiche Ankunft wie auf der Gabelung; der
                // Zeitstempel wird beim Wechsel in den Assistenten gelöscht,
                // damit die Rückkehr wieder eine Ankunft ist.
                let fade = arrival_fade(ui, "dashboard_at");
                ui.multiply_opacity(fade);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if !self.errors.is_empty() {
                            let (title, body) = error_banner_text(&self.errors);
                            if widgets::banner(
                                ui,
                                pal().alert,
                                &title,
                                &body,
                                Some("Ordner im Finder zeigen"),
                            ) {
                                widgets::reveal(&self.hits_path);
                            }
                            ui.add_space(theme::S3);
                        }

                        // Eine Lesespalte in der Mitte. Das Fenster darf breit
                        // sein, der Text nicht.
                        let width = ui.available_width().min(720.0);
                        let side_pad = (ui.available_width() - width) / 2.0;

                        // Wie viel die Spalte außer der Truhe wirklich braucht,
                        // wird gemessen statt geschätzt: Schriftmetriken,
                        // Umbrüche und die Wallet-Zeile ändern das je nach
                        // Inhalt, und jede feste Zahl war entweder zu klein
                        // (unten rutschte etwas heraus) oder zu groß (die Truhe
                        // blieb winzig).
                        //
                        // Das vorige Bild liefert den Wert, dieses rechnet
                        // damit. Weil der gemessene Wert die Truhe nicht
                        // enthält, hängt er nicht von ihr ab — es schaukelt sich
                        // also nichts auf, sondern sitzt nach einem Bild.
                        let key = egui::Id::new("scene_fixed_h");
                        let fixed: f32 =
                            ui.memory(|m| m.data.get_temp(key)).unwrap_or(SCENE_FIXED_H);
                        // Der Grundabstand oben gehört ins Budget. Ohne ihn lief
                        // die Seite um genau seinen Betrag über: die Truhe bekam
                        // `avail - fixed`, und der Abstand kam obendrauf.
                        let base_pad = theme::S2;
                        let chest_h = (avail - fixed - base_pad).clamp(56.0, 190.0);
                        // Was übrig bleibt, kommt zur Hälfte nach oben: die
                        // Szene steht dann mittig statt oben zu kleben.
                        let slack = (avail - chest_h - fixed - base_pad).max(0.0);
                        let top_pad = base_pad + slack * 0.5;

                        let block = ui.horizontal(|ui| {
                            ui.add_space(side_pad.max(0.0));
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                self.draw_scene(
                                    ui,
                                    &tex,
                                    now_rate,
                                    seeds,
                                    paused,
                                    SceneLayout {
                                        chest_h,
                                        pad: top_pad,
                                    },
                                );
                                ui.add_space(theme::S4);
                                self.draw_verdict_scene(ui, avg, seeds);
                            });
                        });

                        let used = block.response.rect.height();
                        ui.memory_mut(|m| m.data.insert_temp(key, used - chest_h - top_pad));
                    });
            });

        if to_hub {
            // Der Stempel weg, damit die Gabelung wieder aufblendet statt
            // hart zu erscheinen — derselbe Handgriff wie beim Verlassen der
            // Seed-Rettung. Die Suche läuft dabei weiter: niemand hat sie
            // angehalten, und sie wieder anzuhalten wäre eine Entscheidung,
            // die dieser Knopf nicht trifft.
            ctx.memory_mut(|m| m.data.remove::<f64>(egui::Id::new("chooser_at")));
            self.screen = Screen::Chooser;
        } else if jump_to_find {
            // Der Klick auf den Wegweiser ist das bewusste Aufdecken.
            self.settings_open = false;
            self.selected = self.real_hits().last().map(|(i, _)| i);
            self.pending = None;
        }
    }

    /// Das Band, das einen Fund über jeden Bildschirm legt.
    ///
    /// Kein eigenes Fenster: ein zweites Fenster kann hinter anderen
    /// verschwinden und wäre damit *weniger* auffällig als ein Streifen in dem
    /// Fenster, das ohnehin schon nach vorne geholt wurde.
    ///
    /// Wer gerade seine eigene Seed rettet, bekommt **keinen** Knopf, der ihn
    /// dort wegführt. Das halb ausgefüllte Wortformular ist wertvoller als die
    /// Neugier auf einen Fund, der ohnehin sicher auf der Platte liegt — und
    /// weil der Knopf dort schlicht nicht existiert, ist die Regel strukturell
    /// erfüllt und nicht bloß vorgesehen.
    fn draw_find_band(&mut self, ctx: &egui::Context) {
        let Some(p) = self.pending else {
            return;
        };
        let Some(hit) = self.hits.get(p.newest) else {
            self.pending = None;
            return;
        };

        let unsaved = self.unsaved.contains(&hit.id);
        let colour = if unsaved { pal().alert } else { pal().gold };
        let in_recover = self.screen.is_recover();
        let (title, body) = find_band_text(p.count, unsaved, in_recover);
        let index = p.newest;

        let mut dismiss = false;
        let mut show = false;

        egui::TopBottomPanel::top("find")
            .frame(
                egui::Frame::none()
                    .fill(theme::wash(colour))
                    .stroke(Stroke::new(1.0_f32, colour))
                    .inner_margin(egui::Margin::symmetric(theme::S3, theme::S2)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(title)
                                .color(colour)
                                .size(theme::BODY)
                                .strong(),
                        );
                        ui.label(RichText::new(body).color(pal().text).size(theme::SMALL));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(widgets::button_quiet("Verstanden")).clicked() {
                            dismiss = true;
                        }
                        if !in_recover {
                            ui.add_space(theme::S2);
                            if ui.add(widgets::button_signal("Ansehen", colour)).clicked() {
                                show = true;
                            }
                        }
                    });
                });
            });

        if dismiss {
            crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
            self.pending = None;
        } else if show {
            crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
            // Der Klick ist das bewusste Aufdecken: erst hier werden die Wörter
            // sichtbar, nie von selbst.
            self.settings_open = false;
            self.screen = Screen::Dashboard;
            self.selected = Some(index);
            self.pending = None;
        }
    }

    /// Der Wegweiser in der Kopfleiste — er existiert erst ab einem echten Fund.
    ///
    /// Weil er bei null gar nicht gezeichnet wird, kann er nie „0 Funde"
    /// anzeigen. Das ist der Unterschied zwischen einem Wegweiser und einem
    /// Punktestand: eine Null bedeutet nur etwas, wenn man mit einer Eins
    /// rechnet, und genau diese Erwartung darf das Programm nicht wecken.
    ///
    /// Gibt zurück, ob er angeklickt wurde.
    fn draw_find_chip(&self, ui: &mut Ui) -> bool {
        let n = self.real_hits().count();
        if n == 0 {
            return false;
        }
        let label = if n == 1 {
            "1 Wallet mit Guthaben".to_string()
        } else {
            format!("{n} Wallets mit Guthaben")
        };
        ui.add(widgets::button_signal(&label, pal().gold)).clicked()
    }

    /// Die Kopfleiste: Marke, Zustand, und die beiden Nebenwege.
    ///
    /// Der Start-Knopf sitzt bewusst **nicht** hier, sondern in der Mitte der
    /// Szene. Ein Bildschirm hat eine offensichtliche Hauptaktion, und die
    /// gehört dorthin, wo der Blick ohnehin hinfällt — nicht in eine Ecke
    /// zwischen zwei gleich aussehende Nebenknöpfe.
    fn draw_head(
        &mut self,
        ctx: &egui::Context,
        tex: &TextureHandle,
        paused: bool,
        to_hub: &mut bool,
        jump_to_find: &mut bool,
    ) {
        egui::TopBottomPanel::top("head")
            .frame(
                egui::Frame::none()
                    .fill(pal().bg)
                    .inner_margin(egui::Margin::symmetric(theme::S3, theme::S2)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Die Marke ist zugleich der Weg zu [`EGG_URL`] — nach dem
                    // Zeichnen abgefragt, weil ein Bild allein nichts spürt.
                    let mark = ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(32.0)));
                    let egg = ui.interact(mark.rect, ui.id().with("egg"), Sense::click());
                    if egg.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if egg.clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(EGG_URL));
                    }
                    ui.add_space(theme::S2);
                    // Auch hier gesperrt, nur knapper: in der Kopfzeile steht
                    // die Marke bei dreizehn Punkten, und dicht gesetzte
                    // Versalien werden in der Größe zum Klumpen.
                    spaced_label(
                        ui,
                        "SCHATZSUCHE",
                        pal().accent,
                        theme::wordmark(ui.ctx(), theme::BODY),
                        theme::BODY * 0.06,
                    );
                    ui.add_space(theme::S3);

                    // Der Zustandspunkt atmet, solange gesucht wird. Ein
                    // stehender Punkt und ein laufendes Programm sehen sonst
                    // gleich aus.
                    let dot_col = if paused { pal().warn } else { pal().primary };
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    if paused {
                        ui.painter().circle_stroke(
                            dot.center(),
                            4.0,
                            Stroke::new(1.5_f32, dot_col),
                        );
                    } else {
                        let t = ui.input(|i| i.time) as f32;
                        let pulse = 3.4 + 1.0 * (t * 2.6).sin();
                        ui.painter().circle_filled(
                            dot.center(),
                            pulse + 3.0,
                            Color32::from_rgba_unmultiplied(
                                dot_col.r(),
                                dot_col.g(),
                                dot_col.b(),
                                40,
                            ),
                        );
                        ui.painter().circle_filled(dot.center(), 4.0, dot_col);
                        ui.ctx().request_repaint();
                    }
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(if paused { "Angehalten" } else { "Läuft" })
                            .color(dot_col)
                            .size(theme::SMALL),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::header_button(ui, "Einstellungen").clicked() {
                            crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
                            self.settings_open = !self.settings_open;
                        }
                        // Der Wegweiser steht rechts außen, wo er auffällt —
                        // aber nur, wenn es etwas zu weisen gibt.
                        if self.draw_find_chip(ui) {
                            crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
                            *jump_to_find = true;
                        }
                        ui.add_space(theme::S2);
                        ui.add_space(theme::S2);
                        // Führt zur Gabelung zurück, nicht in die Seed-Rettung.
                        // Vorher sprang dieser Knopf sofort in den anderen
                        // Betriebsmodus: ein Klick, und der Bildschirm war ein
                        // ganz anderer, ohne Zwischenschritt und ohne dass
                        // jemand gesagt hätte, wohin es geht. Der Weg über die
                        // Startseite kostet einen Klick mehr und zeigt dafür
                        // beide Türen — mitsamt der, aus der man gerade kommt.
                        if widgets::header_button(ui, "Hub")
                            .on_hover_text("Zurück zur Startseite mit den zwei Türen")
                            .clicked()
                        {
                            crate::ui::feel::bump(crate::ui::feel::Bump::Switch);
                            *to_hub = true;
                        }
                    });
                });
            });
    }

    fn draw_foot(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("foot")
            .frame(
                egui::Frame::none()
                    .fill(pal().bg)
                    .inner_margin(egui::Margin::symmetric(theme::S3, theme::S2)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Ausgeschrieben statt mit ↑ ↓ gezeichnet: die mitgelieferte
                    // Schrift hat keine Pfeilzeichen und malte leere Kästen.
                    ui.label(
                        RichText::new("Leertaste hält an  ·  ⌘ ,  öffnet die Einstellungen")
                            .color(pal().muted)
                            .size(theme::SMALL),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        handle_link(ui, theme::SMALL);
                    });
                });
            });
    }

    /// Die Szene: Truhe, die eine Zahl, der eine Knopf.
    fn draw_scene(
        &mut self,
        ui: &mut Ui,
        tex: &TextureHandle,
        now_rate: f64,
        seeds: u64,
        paused: bool,
        layout: SceneLayout,
    ) {
        let SceneLayout { chest_h, pad } = layout;
        let t = ui.input(|i| i.time) as f32;

        ui.vertical_centered(|ui| {
            ui.add_space(pad);

            // Die Truhe mit einem Schimmer dahinter, der langsam atmet.
            // Angehalten erlischt er — man sieht am Bild, dass nichts läuft,
            // bevor man ein Wort gelesen hat.
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), chest_h), Sense::hover());
            let centre = rect.center();
            let breath = if paused {
                0.25
            } else {
                0.55 + 0.20 * (t * 1.3).sin()
            };
            let g = pal().gold;
            // Bild und Schimmer folgen der zugeteilten Höhe. Fest eingetragene
            // 140 Punkte ragten in einem niedrigen Fenster über den Streifen
            // hinaus und legten sich der großen Zahl über den Kopf.
            widgets::ellipse_gradient(
                ui.painter(),
                centre,
                chest_h * 0.86,
                chest_h * 0.60,
                Color32::from_rgba_unmultiplied(g.r(), g.g(), g.b(), (54.0 * breath) as u8),
                Color32::from_rgba_unmultiplied(g.r(), g.g(), g.b(), 0),
            );
            let tint = if paused { 130 } else { 255 };
            ui.painter().image(
                tex.id(),
                egui::Rect::from_center_size(centre, Vec2::splat(chest_h)),
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_white_alpha(tint),
            );
            if !paused {
                ui.ctx().request_repaint();
            }

            ui.add_space(theme::S2);

            // Die eine Zahl. Läuft weich hoch statt zu springen — und ist
            // geprägt statt gedruckt: ein dunkler Abdruck knapp unter der
            // Ziffer, dann die Ziffer selbst. Zwei Malgänge, die aus einer
            // Systemschrift einen Zählerstand auf einem Deckel machen. Das
            // Licht kommt von oben, wie überall im Fenster.
            let shown = if paused {
                0.0
            } else {
                crate::ui::feel::smooth(ui, "tempo", now_rate)
            };
            let colour = if paused { pal().muted } else { pal().primary };
            let galley = ui.painter().layout_no_wrap(
                format::thousands(shown as u64),
                mono(theme::SCENE),
                colour,
            );
            let (num_rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
            let mut stamp = egui::epaint::TextShape::new(
                num_rect.min + Vec2::new(0.0, 2.0),
                galley.clone(),
                colour,
            );
            stamp.override_text_color = Some(crate::ui::feel::darken(colour, 0.72));
            ui.painter().add(stamp);
            let mut face = egui::epaint::TextShape::new(num_rect.min, galley, colour);
            face.override_text_color = Some(colour);
            ui.painter().add(face);
            ui.label(
                RichText::new("Wallets pro Sekunde")
                    .color(pal().dim)
                    .size(theme::BODY),
            );
            ui.add_space(theme::S1);
            // Geprüfte Wallets und Laufzeit stehen zusammen, weil die eine Zahl
            // ohne die andere nichts aussagt: 37 760 geprüft ist viel in einer
            // Minute und wenig an einem Tag. Monospace, damit die Zeile beim
            // Hochzählen nicht springt.
            //
            // Die Laufzeit zählt nur, während wirklich gesucht wird — eine über
            // Nacht angehaltene Suche behauptet nicht, die ganze Nacht
            // gerechnet zu haben.
            ui.label(
                RichText::new(format!(
                    "{} geprüft   ·   Laufzeit {}",
                    format::thousands(seeds),
                    util::format_duration(self.rate.elapsed().as_secs())
                ))
                .color(pal().muted)
                .font(mono(theme::SMALL)),
            );

            ui.add_space(theme::S3);

            // Die Hauptaktion, groß und in der Mitte.
            let (label, sub, colour) = if paused {
                ("Suche starten", "Leertaste", pal().primary)
            } else {
                ("Anhalten", "Leertaste", pal().warn)
            };
            if crate::ui::feel::key_button(ui, Vec2::new(240.0, 56.0), colour, label, Some(sub)) {
                crate::ui::feel::bump(crate::ui::feel::Bump::Switch);
                self.control.toggle_paused();
            }
        });
    }

    /// Das Urteil — die Antwort, für die das Programm existiert.
    ///
    /// Ohne Karte, ohne Rahmen, ohne Fachwerte: die Zahl, ein Satz, und der
    /// abgesuchte Anteil des Suchraums. Der steht hier und nicht im
    /// Detailbereich, obwohl er dort weniger Platz kostete: er ist die
    /// eigentliche Unmöglichkeitsrechnung, und die gehört dauerhaft ins
    /// Hauptfenster. Beim Aufräumen war er einmal nach unten gerutscht — das
    /// war der eine Handgriff, den dieses Programm sich nicht leisten kann.
    fn draw_verdict_scene(&self, ui: &mut Ui, rate: f64, seeds: u64) {
        let ages = universe_ages_to_hit(self.funded_count, self.addresses_per_seed, rate);
        let frac = seeds as f64 / 2f64.powi(self.entropy_bits() as i32) * 100.0;
        let expected = expected_seeds_to_hit(self.funded_count, self.addresses_per_seed);
        let real = self.hits.iter().filter(|h| !h.is_synthetic()).count();

        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Wie lange bis zu einem Treffer?")
                    .color(pal().dim)
                    .size(theme::SMALL),
            );
            ui.add_space(theme::S2);
            ui.label(
                RichText::new(format!("{} ×", format::german_scale(ages)))
                    .color(pal().alert)
                    .size(theme::DISPLAY)
                    .strong(),
            );
            ui.label(
                RichText::new("das Alter des Universums")
                    .color(pal().alert)
                    .size(theme::TITLE),
            );
            ui.add_space(theme::S2);
            // Das Bild zur Zahl. „9,4 Trillionen mal das Alter des Universums"
            // ist wahr und für die meisten Leser bedeutungslos; ein Sandkorn
            // kennt jeder. Gerechnet, nicht hingeschrieben — mit einer anderen
            // Adressliste ändert sich der Vergleich mit.
            ui.label(
                RichText::new(format::odds_picture(expected))
                    .color(pal().text)
                    .size(theme::BODY),
            );
            ui.add_space(theme::S2);
            ui.label(
                RichText::new(
                    "Kein Fehler — das ist das Ergebnis, und es zeigt, warum Bitcoin sicher ist.",
                )
                .color(pal().dim)
                .size(theme::SMALL),
            );
            ui.add_space(theme::S2);
            // Der abgesuchte Anteil und der Trefferstand in einer Zeile. Der
            // Trefferstand hatte vorher einen eigenen Balken am Fuß des
            // Fensters — ein Kasten quer über die Breite, nur um „nichts" zu
            // sagen. Hier steht er da, wo der Blick ohnehin schon ist.
            ui.label(
                RichText::new(format!(
                    "{} des Suchraums abgesucht   ·   {}",
                    format::share_headline(frac),
                    if real == 0 {
                        "noch kein Treffer".to_string()
                    } else if real == 1 {
                        "1 Treffer — siehe unten".to_string()
                    } else {
                        format!("{real} Treffer — siehe unten")
                    }
                ))
                .color(pal().warn)
                .size(theme::SMALL),
            );
        });
    }

    /// Das Fundfach: wo eine gefundene Wallet auftauchen würde.
    ///
    /// Im Gegensatz zu allem anderen hier ist dieser Abschnitt **auch leer
    /// sichtbar**, und das ist der Punkt. Wer das Programm nachts laufen lässt,
    /// will wissen, ob er einen Fund überhaupt mitbekäme — diese Frage lässt
    /// sich beantworten, ohne Erfolg in Aussicht zu stellen. Der Aufklapper
    /// darunter sagt, was dann passiert; nachstellen lässt es sich im Terminal
    /// mit `schatzsuche --test-alert`.
    ///
    /// Der Leerzustand zeigt bewusst **keine Zahl**. Eine Null bedeutet nur
    /// etwas, wenn man mit einer Eins rechnet; das wäre die Bildsprache eines
    /// Spielautomaten. Der Trefferstand steht genau einmal im Fenster, unter
    /// der Unmöglichkeitsrechnung, und dort gehört er hin.
    ///
    /// Gezeichnet wird ausschließlich über [`GuiApp::real_hits`], nicht über
    /// `hits`. Testeinträge flogen schon immer aus allen Zählungen, standen aber
    /// trotzdem in der Liste — und weil `hits.jsonl` beim Start eingelesen wird,
    /// genügte ein einziger alter Selbsttest aus dem Terminal, damit dieses Fach
    /// dauerhaft als „nicht leer" galt. Gelöscht wird dabei nichts; die Einträge
    /// sind nur nicht mehr im Weg.
    fn draw_find_section(&mut self, ui: &mut Ui) {
        let real: Vec<usize> = self.real_hits().map(|(i, _)| i).collect();
        section(
            ui,
            "FUNDFACH",
            if real.is_empty() {
                pal().dim
            } else {
                pal().gold
            },
        );

        if real.is_empty() {
            let (what, how) = find_section_empty_text();
            ui.label(RichText::new(what).color(pal().text).size(theme::SMALL));
            ui.add_space(theme::S1);
            ui.label(RichText::new(how).color(pal().muted).size(theme::SMALL));
            ui.add_space(theme::S2);
            widgets::disclosure(
                ui,
                "fundfach",
                "Wird eine Wallet mit Guthaben gefunden, schreibt das Programm die Wörter \
                 zuerst auf die Platte — mit erzwungenem Schreibbefehl bis auf die Hardware, \
                 Rechten nur für dich, und einer zweiten Kopie. Erst danach meldet es sich: \
                 Systemton, hüpfendes Symbol im Dock und ein Band quer über dieses Fenster.",
            );
            return;
        }

        for (n, i) in real.into_iter().enumerate() {
            if n > 0 {
                ui.add_space(theme::S1);
            }
            self.draw_wallet_row(ui, i);
        }
    }

    /// Eine Zeile der Wallet-Liste: Guthaben, Adresse — und auf Klick die
    /// Wörter direkt darunter.
    ///
    /// Die Wörter standen vorher in einem zweiten Feld daneben, das nur
    /// erschien, wenn es überhaupt Treffer gab. Aufgeklappt an Ort und Stelle
    /// ist der Zusammenhang zwischen „diese Wallet" und „diese Wörter" nicht
    /// mehr zu verwechseln — und bei mehreren Funden auch nicht mehr zu
    /// verlieren.
    fn draw_wallet_row(&mut self, ui: &mut Ui, i: usize) {
        let hit = self.hits[i].clone();
        let synthetic = hit.is_synthetic();
        let unsaved = self.unsaved.contains(&hit.id);
        let open = self.selected == Some(i);

        // Farbe und Marke sagen in dieser Reihenfolge, was die Zeile ist: nicht
        // gespeichert schlägt alles, dann Testeintrag, dann echter Fund.
        let (mark, colour) = if unsaved {
            ("NICHT GESPEICHERT", pal().alert)
        } else if synthetic {
            ("TEST", pal().muted)
        } else {
            ("FUND", pal().gold)
        };

        let (slot, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
            self.selected = if open { None } else { Some(i) };
        }

        let raised = crate::ui::feel::lift(ui, &resp);
        let p = ui.painter();
        p.rect(
            slot,
            theme::r_sm(),
            if open || raised > 0.0 {
                pal().hover
            } else {
                pal().inset
            },
            Stroke::new(1.0_f32, if open { colour } else { pal().frame }),
        );

        // Zwei Zeilen statt einer. Vorher standen Guthaben, Adresse und Marke
        // nebeneinander auf festen Pixelpositionen, ausgelegt für eine 720
        // Punkte breite Spalte — in der 340 Punkte schmalen Detailspalte lagen
        // sie übereinander.
        let top = slot.top() + 14.0;
        let bottom = slot.bottom() - 14.0;

        // Aufklapp-Dreieck, gemalt statt gesetzt: die Schrift kennt kein ▸.
        let c = Pos2::new(slot.left() + theme::S3, top);
        let r = 4.0_f32;
        let tri = if open {
            vec![
                Pos2::new(c.x - r, c.y - r * 0.6),
                Pos2::new(c.x + r, c.y - r * 0.6),
                Pos2::new(c.x, c.y + r * 0.8),
            ]
        } else {
            vec![
                Pos2::new(c.x - r * 0.6, c.y - r),
                Pos2::new(c.x - r * 0.6, c.y + r),
                Pos2::new(c.x + r * 0.8, c.y),
            ]
        };
        p.add(egui::Shape::convex_polygon(tri, colour, Stroke::NONE));

        // Zeile 1: Guthaben links, Marke rechts. Beide kurz genug.
        p.text(
            Pos2::new(slot.left() + theme::S4 + theme::S1, top),
            egui::Align2::LEFT_CENTER,
            &hit.balance_btc,
            mono(theme::BODY),
            if synthetic { pal().muted } else { pal().gold },
        );
        p.text(
            Pos2::new(slot.right() - theme::S2, top),
            egui::Align2::RIGHT_CENTER,
            mark,
            egui::FontId::proportional(theme::SMALL),
            colour,
        );
        // Zeile 2: die Adresse, in der Mitte gekürzt. Anfang und Ende sind die
        // Teile, an denen man eine bech32-Adresse wiedererkennt; vollständig
        // steht sie eine Zeile tiefer, sobald aufgeklappt ist.
        p.text(
            Pos2::new(slot.left() + theme::S4 + theme::S1, bottom),
            egui::Align2::LEFT_CENTER,
            shorten_middle(&hit.address, 14, 6),
            mono(theme::SMALL),
            pal().dim,
        );

        if !open {
            return;
        }

        // Aufgeklappt: alles zu dieser einen Wallet.
        ui.add_space(theme::S1);
        egui::Frame::none()
            .fill(pal().inset)
            .rounding(theme::r_sm())
            .stroke(Stroke::new(1.0_f32, colour))
            .inner_margin(egui::Margin::symmetric(theme::S3, theme::S3))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                if unsaved {
                    ui.label(
                        RichText::new(
                            "Diese Wörter konnten nicht gespeichert werden. Schreib sie \
                             jetzt ab — beim Schließen sind sie weg.",
                        )
                        .color(pal().alert)
                        .size(theme::BODY)
                        .strong(),
                    );
                    ui.add_space(theme::S2);
                } else if synthetic {
                    ui.label(
                        RichText::new("Kein echter Fund — ein Eintrag aus dem Selbsttest.")
                            .color(pal().warn)
                            .size(theme::BODY),
                    );
                    ui.add_space(theme::S1);
                    widgets::disclosure(
                        ui,
                        "testeintrag",
                        "Die Wörter unten sind das öffentliche BIP-39-Testbeispiel, mit dem \
                         die Speicher- und Alarmkette geprüft wird. Diese Wallet ist leer, \
                         und ihr Schlüssel ist weltweit bekannt.",
                    );
                    ui.add_space(theme::S2);
                }

                widgets::kv(ui, "Guthaben", &hit.balance_btc, pal().gold);
                widgets::kv(ui, "Pfad", &hit.derivation_path, pal().dim);

                // Die Adresse bekommt eine eigene, umbrechende Zeile statt
                // einer Wertspalte. Als `kv` gesetzt sprengte sie die 308
                // Punkte der Detailspalte, kürzte dabei ihre eigene
                // Beschriftung auf „.." und schob alles darunter aus dem
                // Rahmen — vierzig Zeichen bech32 passen dort nicht neben ein
                // Wort.
                ui.add_space(theme::S2);
                ui.label(RichText::new("Adresse").color(pal().dim).size(theme::SMALL));
                ui.add(
                    egui::Label::new(
                        RichText::new(&hit.address)
                            .color(pal().text)
                            .font(mono(theme::SMALL)),
                    )
                    .wrap(),
                );
                ui.add_space(theme::S3);

                ui.label(
                    RichText::new("Die Wörter dieser Wallet")
                        .color(pal().dim)
                        .size(theme::SMALL),
                );
                ui.add_space(theme::S2);
                widgets::seed_grid(ui, &format!("seed_{i}"), &hit.mnemonic, 2);

                ui.add_space(theme::S3);
                ui.label(
                    RichText::new("Sie verlassen diesen Rechner nie.")
                        .color(pal().muted)
                        .size(theme::SMALL)
                        .italics(),
                );
                if !unsaved {
                    ui.label(
                        RichText::new("Gespeichert in deiner Fundliste, nur für dich lesbar.")
                            .color(pal().muted)
                            .size(theme::SMALL)
                            .italics(),
                    );
                }
            });
    }

    /// Die Fachzahlen, als feste Spalte rechts.
    ///
    /// Ohne Karten. Vier Karten übereinander kosteten allein 228 Punkte an
    /// Rahmen, Titelzeilen und Innenrändern — mehr als ein Drittel der Höhe,
    /// die überhaupt zur Verfügung steht, für nichts als Umrandung. Flache
    /// Abschnitte mit einer Überschrift sagen dasselbe und passen auf die
    /// Seite.
    ///
    /// Es gibt bewusst keinen Knopf, der die Spalte zuklappt. Was immer da
    /// ist, muss man nicht suchen.
    fn draw_details_panel(&mut self, ctx: &egui::Context, seeds: u64, avg: f64) {
        let bits = self.entropy_bits();
        let frac = seeds as f64 / 2f64.powi(bits as i32) * 100.0;
        let expected = expected_seeds_to_hit(self.funded_count, self.addresses_per_seed);

        egui::SidePanel::right("details")
            .resizable(false)
            .exact_width(DETAIL_W)
            .frame(
                egui::Frame::none()
                    .fill(pal().bg)
                    .stroke(theme::hairline())
                    .inner_margin(egui::Margin::symmetric(theme::S3, theme::S3)),
            )
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
                // Die Rollfläche ist ein Netz, kein Bedienelement: bei den
                // Größen unten greift sie nie. egui blendet den Balken aus,
                // solange der Inhalt passt — wer das Fenster trotzdem auf
                // Briefmarkengröße zieht, kommt noch an alles heran.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Ganz oben: was man im Ernstfall braucht. Die
                        // Fachzahlen darunter dürfen wegrollen, das Fundfach
                        // nicht.
                        self.draw_find_section(ui);

                        section(ui, "TEMPO", pal().primary);
                        widgets::kv(
                            ui,
                            "Durchschnitt",
                            &format!("{} /s", format::thousands(avg as u64)),
                            pal().text,
                        );
                        widgets::kv(
                            ui,
                            "Spitze",
                            &format!("{} /s", format::thousands(self.peak as u64)),
                            pal().text,
                        );
                        widgets::kv(
                            ui,
                            "Pro Kern",
                            &format!(
                                "{} /s ×{}",
                                format::thousands((avg / self.threads.max(1) as f64) as u64),
                                self.threads
                            ),
                            pal().dim,
                        );
                        widgets::kv(
                            ui,
                            "Adressen geprüft",
                            &format::thousands(self.stats.addresses()),
                            pal().dim,
                        );

                        section(ui, "TEMPO-VERLAUF", pal().primary);
                        self.draw_sparkline(ui);

                        section(ui, "SUCHRAUM", pal().warn);
                        widgets::kv(ui, "Abgesucht", &format::share_headline(frac), pal().warn);
                        widgets::kv(
                            ui,
                            "Wortlänge",
                            &format!("{} Wörter", self.control.word_count().words()),
                            pal().dim,
                        );
                        widgets::kv(ui, "Suchraum", &format!("2^{bits}"), pal().dim);
                        // Dauerhaft beschriftet: eine selbst gebaute Übungsliste
                        // war nach dem ersten Start sonst von einer echten nicht
                        // zu unterscheiden.
                        widgets::kv(
                            ui,
                            if self.practice_list {
                                "Übungsliste"
                            } else {
                                "Adressliste"
                            },
                            &format::thousands(self.funded_count),
                            pal().dim,
                        );
                        widgets::kv(
                            ui,
                            "Speicher",
                            &format!(
                                "{:.0} + {:.0} MB",
                                self.bloom_bytes as f64 / 1e6,
                                self.db_bytes as f64 / 1e6
                            ),
                            pal().dim,
                        );

                        // Der Rechenweg stand einmal als eigener Abschnitt
                        // darunter. Er gehört hierher: „wie groß ist das
                        // Problem" und „wie lange dauert es dann" sind
                        // dieselbe Frage, und die eigene Überschrift kostete
                        // dreißig Punkte, die der Spalte am unteren Rand
                        // fehlten.
                        widgets::kv(
                            ui,
                            "Seeds bis zum Treffer",
                            &format::sci(expected),
                            pal().dim,
                        );
                        widgets::kv(
                            ui,
                            "Chance je Seed",
                            &format::sci(1.0 / expected),
                            pal().dim,
                        );
                        widgets::kv(
                            ui,
                            "Adressen je Seed",
                            &self.addresses_per_seed.to_string(),
                            pal().dim,
                        );
                    });
            });
    }

    /// Der Tempo-Verlauf, ohne eigene Karte.
    ///
    /// Die Überschrift kommt von [`section`]; sie hier nochmal zu setzen ergab
    /// „TEMPO-VERLAUF" zweimal übereinander, einmal als Abschnitt und einmal
    /// als Kartentitel.
    fn draw_sparkline(&self, ui: &mut Ui) {
        {
            // Flach genug, dass die Spalte ohne Rollbalken auskommt, hoch
            // genug, dass die Kurve eine Form hat.
            let h = 30.0;
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
            let painter = ui.painter();
            painter.line_segment(
                [
                    Pos2::new(rect.left(), rect.bottom()),
                    Pos2::new(rect.right(), rect.bottom()),
                ],
                theme::hairline(),
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

            // A single mesh under the trace, its colour fading from the line
            // down to nothing at the baseline. Each segment is a trapezoid —
            // the trace is not convex, so one polygon would cross itself — and
            // the vertex colours do the gradient for free. This is the touch
            // that turns a flat shaded area into something with depth.
            //
            // Aus der Palette und nicht als Ziffern: hier stand die Leitfarbe
            // einmal doppelt, als Zahlenwerte kopiert. Beim Umfärben wäre die
            // Fläche unter der Kurve himmelblau geblieben, während die Linie
            // darüber mitgewandert wäre.
            let c = pal().primary;
            let top = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 96);
            let bot = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 0);
            let mut mesh = egui::Mesh::default();
            for w in pts.windows(2) {
                let base = mesh.vertices.len() as u32;
                mesh.colored_vertex(w[0], top);
                mesh.colored_vertex(w[1], top);
                mesh.colored_vertex(Pos2::new(w[1].x, rect.bottom()), bot);
                mesh.colored_vertex(Pos2::new(w[0].x, rect.bottom()), bot);
                mesh.add_triangle(base, base + 1, base + 2);
                mesh.add_triangle(base, base + 2, base + 3);
            }
            painter.add(egui::Shape::mesh(mesh));
            painter.add(egui::Shape::line(pts, Stroke::new(1.6_f32, pal().primary)));
        }
    }

    /// The loading screen.
    ///
    /// Shown from the first frame, while the database and filter are still
    /// being read on another thread. The bar tracks real work rather than a
    /// timer — a fake progress bar that finishes before the work does is worse
    /// than none.
    fn draw_loading(&mut self, ctx: &egui::Context) {
        let tex = self.logo_texture(ctx);
        let (step, frac) = match &self.progress {
            Some(p) => (p.step(), p.fraction()),
            None => (String::new(), 0.0),
        };
        let t = self.started.elapsed().as_secs_f32();

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(pal().bg))
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
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

                    ui.add_space(theme::S4);
                    spaced_label(
                        ui,
                        "SCHATZSUCHE",
                        pal().text,
                        theme::wordmark(ui.ctx(), theme::DISPLAY),
                        theme::DISPLAY * 0.18,
                    );
                    ui.add_space(theme::S4);

                    // Progress bar, drawn by hand so it matches the palette.
                    // The fill is brass, not paint: a gradient anchored on
                    // `gold_mid`, a sheen along the top, and a slow gleam
                    // running across — the screen repaints every 33 ms anyway.
                    let w = 380.0_f32.min(ui.available_width() - 40.0);
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 8.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(rect, theme::r_xs(), pal().sunken);
                    if frac > 0.0 {
                        let mut fill = rect;
                        fill.set_width(rect.width() * frac.clamp(0.02, 1.0));
                        p.rect_filled(fill, theme::r_xs(), pal().gold_mid);
                        crate::ui::feel::sheen(p, fill, pal().gold);
                        crate::ui::feel::gleam(p, fill, pal().gold, ui.input(|i| i.time));
                    }

                    ui.add_space(theme::S3);
                    ui.label(RichText::new(step).color(pal().dim).size(theme::BODY));
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(format!("{:.0} %", frac * 100.0))
                            .color(pal().muted)
                            .font(mono(theme::SMALL)),
                    );
                });
            });

        // Repaint briskly: the bar and the breathing both need it.
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    /// Settings window: presets for everyone, raw knobs behind a warning.
    fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let machine = crate::machine::Machine::detect();
        let max_cores = machine.max_threads();

        egui::SidePanel::right("settings")
            .resizable(false)
            .exact_width(DETAIL_W)
            .frame(
                egui::Frame::none()
                    .fill(pal().panel)
                    .stroke(theme::hairline())
                    .inner_margin(egui::Margin::symmetric(16.0, 14.0)),
            )
            .show(ctx, |ui| {
                self.draw_grain(ui, ui.clip_rect());
                let threads = self.control.active_threads();

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("EINSTELLUNGEN")
                            .color(pal().text)
                            .size(theme::BODY)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Schließen").color(pal().dim).size(theme::SMALL))
                                    .fill(pal().bg)
                                    .stroke(theme::hairline())
                                    .rounding(theme::r_sm()),
                            )
                            .clicked()
                        {
                            self.settings_open = false;
                        }
                    });
                });
                ui.add_space(theme::S3);

                // Auch hier, nicht nur in der Detailspalte: die Schublade
                // verdeckt sie, und ein Fund darf nicht deshalb unsichtbar
                // sein, weil jemand gerade an den Reglern war.
                self.draw_find_section(ui);
                ui.add_space(theme::S3);

                // Everything below the header scrolls. With the expert
                // section open the panel is taller than the window, and the
                // last row was simply unreachable.
                //
                // `SC_SHOT_SCROLL=<punkte>` rollt die Schublade für eine
                // Aufnahme an eine Stelle weiter unten. Ein Screenshot kann
                // nicht scrollen, das Fenster nicht höher werden als der
                // Bildschirm — und damit war der Fuß der Schublade
                // („DEINE DATEN", die Spendenzeile) der einzige Teil der
                // Oberfläche, den niemand vor dem Ausliefern ansehen konnte.
                // `SC_SHOT_DONATE` allein reichte, bis oben Inhalt dazukam.
                let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(px) = std::env::var("SC_SHOT_SCROLL")
                    .ok()
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    area = area.vertical_scroll_offset(px);
                }
                area.show(ui, |ui| {
                    ui.label(RichText::new("LEISTUNG").color(pal().primary).size(theme::SMALL).strong());
                    ui.add_space(theme::S1);
                    // What the hardware said. The sentence that used to sit here —
                    // more cores, more heat — was telling the reader what the rows
                    // below already show.
                    ui.label(RichText::new(machine.describe()).color(pal().dim).size(theme::SMALL));
                    ui.add_space(theme::S2);

                    // Each row says what it costs in its own words: how much of
                    // the machine, and what that feels like. The meter carries the
                    // speed, so nobody has to compare four percentages.
                    let cores = |n: usize| {
                        if n == 1 {
                            "1 Kern".to_string()
                        } else {
                            format!("{n} Kerne")
                        }
                    };
                    let quiet_cores = if machine.efficiency > 0 {
                        format!("{} sparsame", machine.economical_threads())
                    } else {
                        cores(machine.economical_threads())
                    };
                    let fast_cores = if machine.efficiency > 0 {
                        format!("{} schnelle", machine.recommended_threads())
                    } else {
                        cores(machine.recommended_threads())
                    };

                    let table = modes(&machine);
                    let presets = [
                        (
                            "Unauffällig",
                            table[0].0,
                            table[0].1,
                            table[0].2,
                            "1 Kern · läuft unbemerkt mit".to_string(),
                            1u8,
                            "Für nebenher. Der Rechner arbeitet nur ein Prozent der Zeit \
                             daran — kein Lüfter, kein spürbarer Akkuverbrauch, nichts, was \
                             du merkst. Nimm das, wenn du es einfach monatelang mitlaufen \
                             lassen willst.",
                        ),
                        (
                            "Sparsam",
                            table[1].0,
                            table[1].1,
                            table[1].2,
                            format!("{quiet_cores} Kerne · kühl und leise"),
                            2u8,
                            "Leise, aber schon deutlich schneller. Läuft auf den sparsamen \
                             Kernen, die dein Gerät auch für Hintergrundaufgaben benutzt. \
                             Gut, wenn der Laptop auf dem Schoß steht oder du in Ruhe \
                             arbeiten willst.",
                        ),
                        (
                            "Ausgewogen",
                            table[2].0,
                            table[2].1,
                            table[2].2,
                            format!("{fast_cores} Kerne · empfohlen"),
                            4u8,
                            "Die Voreinstellung, und für die meisten die richtige. Nutzt die \
                             schnelle Hälfte des Rechners und lässt die andere frei — du \
                             kannst nebenher normal weiterarbeiten, ohne dass etwas hakt.",
                        ),
                        (
                            "Maximum",
                            table[3].0,
                            table[3].1,
                            table[3].2,
                            format!("{} · {NOUN} wird warm und laut", cores(max_cores)),
                            5u8,
                            "Alles, was die Maschine hat. Nimm das nur, wenn du den Rechner \
                             gerade nicht brauchst — er wird warm, der Lüfter läuft, und am \
                             Akku hältst du es nicht lange durch.",
                        ),
                    ];

                    // Ob überhaupt eine Zeile leuchtet. Früher war das sicher:
                    // der Start rastete jede Einstellung auf einen dieser vier
                    // Modi ein. Seit eine ausdrücklich genannte Einstellung
                    // stehen bleibt (`--threads 6`, oder eine Kernzahl in der
                    // `config.toml`), kann sie zu keinem Modus gehören — dann
                    // steht sie unter den Zeilen ausgeschrieben, statt dass das
                    // Fenster verschweigt, was läuft.
                    let mut any_active = false;
                    for (idx, (name, t, prio, duty, sub, level, help)) in
                    presets.into_iter().enumerate()
                {
                    let t = t.min(max_cores);
                    // Exact match only.
                    let active = threads == t
                        && self.control.priority() == prio
                        && self.control.throttle() == duty;
                    any_active |= active;
                    let (row_clicked, info_clicked) =
                        widgets::preset_row(ui, name, &sub, level, active);
                    if info_clicked {
                        // Second click on the same mark closes it again.
                        self.info_open = if self.info_open == Some(idx) {
                            None
                        } else {
                            Some(idx)
                        };
                    } else if row_clicked {
                        self.control.set_active_threads(t);
                        self.control.set_priority(prio);
                        self.control.set_throttle(duty);
                    }
                    if self.info_open == Some(idx) {
                        ui.add_space(theme::S1);
                        widgets::preset_help(ui, help);
                    }
                    ui.add_space(theme::S2);
                }

                    // Eine eigene Einstellung gehört zu keiner der vier Zeilen.
                    // Sie wird darum ausgeschrieben — mit derselben Auskunft,
                    // die die Zeilen geben: Kerne, Priorität, Einschaltdauer.
                    if !any_active {
                        ui.label(
                            RichText::new(format!(
                                "Eigene Einstellung: {threads} von {max_cores} Kernen · \
                                 Priorität {} · {} % der Zeit",
                                self.control.priority().label(),
                                self.control.throttle()
                            ))
                            .color(pal().dim)
                            .size(theme::SMALL),
                        );
                        ui.label(
                            RichText::new(
                                "So steht es in deiner config.toml oder kam beim Start mit. \
                                 Ein Klick auf eine Zeile darüber ersetzt sie.",
                            )
                            .color(pal().muted)
                            .size(theme::SMALL),
                        );
                        ui.add_space(theme::S2);
                    }

                ui.add_space(theme::S2);
                    ui.separator();
                    ui.add_space(theme::S2);

                    // Mnemonic length. Not behind the expert gate: it is a plain
                    // question about what is being searched, not a knob that can
                    // make the machine unpleasant to use.
                    ui.label(
                        RichText::new("WORTLÄNGE")
                            .color(pal().primary)
                            .size(theme::SMALL)
                            .strong(),
                    );
                    ui.add_space(theme::S2);
                    ui.horizontal_wrapped(|ui| {
                        for wc in crate::bip39::ALL_WORD_COUNTS {
                            let on = self.control.word_count() == wc;
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("{}", wc.words()))
                                            .color(if on { pal().on_fill } else { pal().text })
                                            .size(theme::SMALL),
                                    )
                                    .fill(if on { pal().primary } else { pal().bg })
                                    .stroke(theme::hairline())
                                    .rounding(theme::r_sm()),
                                )
                                .clicked()
                            {
                                self.control.set_word_count(wc);
                            }
                        }
                    });
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(format!(
                            "Suchraum 2^{} — kürzer heißt kleiner, nicht aussichtsreicher.",
                            self.entropy_bits()
                        ))
                        .color(pal().muted)
                        .size(theme::SMALL),
                    );

                    ui.add_space(theme::S2);
                    ui.separator();
                    ui.add_space(theme::S2);

                    self.draw_alert_section(ui);

                    ui.add_space(theme::S2);
                    ui.separator();
                    ui.add_space(theme::S2);

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
                                .color(if self.expert_unlocked { pal().warn } else { pal().dim })
                                .size(theme::BODY)
                                .strong(),
                        );
                    });

                    if !self.expert_unlocked {
                        ui.label(
                            RichText::new(
                                "Direkte Regler für Kerne, Priorität und Adressen pro Wallet.",
                            )
                            .color(pal().muted)
                            .size(theme::SMALL),
                        );
                        // The ask lives at the very bottom in both cases, so it
                        // is the last thing in the drawer whether or not the
                        // expert controls are open.
                        self.draw_data_section(ui);
                        donation_note(ui, ASK_SEARCH);
                        return;
                    }

                    ui.add_space(theme::S2);
                    let mut t = self.control.active_threads();
                    ui.label(RichText::new("Kerne").color(pal().dim).size(theme::SMALL));
                    if ui
                        .add_sized(
                            Vec2::new(ui.available_width(), 20.0),
                            egui::Slider::new(&mut t, 1..=max_cores),
                        )
                        .changed()
                    {
                        self.control.set_active_threads(t);
                    }

                    ui.add_space(theme::S2);
                    let mut n = self.control.addresses_per_path();
                    ui.label(RichText::new("Adressen pro Pfad").color(pal().dim).size(theme::SMALL));
                    if ui
                        .add_sized(
                            Vec2::new(ui.available_width(), 20.0),
                            egui::Slider::new(&mut n, 1..=50),
                        )
                        .changed()
                    {
                        self.control.set_addresses_per_path(n);
                    }
                    ui.add_space(theme::S1);
                    ui.label(
                        RichText::new(format!(
                            "{} Adressen je Wallet. Weniger = mehr Wallets/s, aber weniger Adressen/s.",
                            n * 3
                        ))
                        .color(pal().muted)
                        .size(theme::SMALL),
                    );

                    ui.add_space(theme::S1);
                    let prio = self.control.priority();
                    ui.label(
                        RichText::new(format!(
                            "Geschätztes Tempo: ca. {:.0} % des Maximums",
                            self.expected_share(t, prio) * 100.0
                        ))
                        .color(pal().text)
                        .size(theme::SMALL),
                    );
                    if self.is_counterproductive(t, prio) {
                        ui.label(
                            RichText::new(
                                "Achtung: so viele Kerne machen es bei „Sparsam“ langsamer, \
                                 nicht schneller.",
                            )
                            .color(pal().warn)
                            .size(theme::SMALL),
                        );
                        widgets::disclosure(
                            ui,
                            "sparsam_gegenlaeufig",
                            "Bei „Sparsam“ weist das Betriebssystem die Arbeit ausschließlich \
                             den Effizienzkernen zu — den langsamen. Mehr Threads teilen sich \
                             dann dieselben wenigen Kerne und stehen sich gegenseitig im Weg. \
                             Gemessen auf einem M1: acht Threads schafften 509 Wallets pro \
                             Sekunde, vier schafften 637.",
                        );
                    }

                    ui.add_space(theme::S2);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Priorität").color(pal().dim).size(theme::SMALL));
                        for p in [Priority::Background, Priority::Utility, Priority::Normal] {
                            let on = self.control.priority() == p;
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(p.label())
                                            .color(if on { pal().on_fill } else { pal().text })
                                            .size(theme::SMALL),
                                    )
                                    .fill(if on { pal().warn } else { pal().bg })
                                    .stroke(theme::hairline())
                                    .rounding(theme::r_sm()),
                                )
                                .clicked()
                            {
                                self.control.set_priority(p);
                            }
                        }
                    });

                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new("Wirkt sofort, wird nicht gespeichert.")
                            .color(pal().muted)
                            .size(theme::SMALL),
                    );

                    self.draw_data_section(ui);
                    donation_note(ui, ASK_SEARCH);
                    });
            });

        // The gate. Deliberately modal and deliberately not pre-answered.
        if self.expert_prompt {
            egui::Window::new("Expertenmodus einschalten?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(pal().panel)
                        .stroke(Stroke::new(1.0_f32, pal().warn)),
                )
                .show(ctx, |ui| {
                    ui.set_max_width(430.0);
                    ui.label(
                        RichText::new(format!("Diese Regler können deinen {NOUN} stark belasten."))
                            .color(pal().warn)
                            .size(theme::BODY)
                            .strong(),
                    );
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(format!(
                            "Alle Kerne auf voller Priorität heißt: dauerhaft 100 % Auslastung, \
                             spürbare Wärme, lauter Lüfter und bei einem Laptop deutlich kürzere \
                             Akkulaufzeit. Schaden nimmt der {NOUN} nicht — er drosselt sich \
                             selbst, bevor etwas passiert — aber angenehm ist es nicht.",
                        ))
                        .color(pal().text)
                        .size(theme::BODY),
                    );
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(
                            "Am Ergebnis ändert das nichts: gefunden wird so oder so nichts.",
                        )
                        .color(pal().dim)
                        .size(theme::SMALL)
                        .italics(),
                    );
                    ui.add_space(theme::S3);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Verstanden, einschalten")
                                        .color(pal().on_fill)
                                        .size(theme::BODY)
                                        .strong(),
                                )
                                .fill(pal().warn)
                                .rounding(theme::r_sm())
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
                                    RichText::new("Abbrechen")
                                        .color(pal().text)
                                        .size(theme::BODY),
                                )
                                .fill(pal().bg)
                                .stroke(theme::hairline())
                                .rounding(theme::r_sm())
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

    /// Die Meldewege: wer erfährt von einem Fund, und wie.
    ///
    /// Bis hierher war das nur zu ändern, indem jemand `config.toml` in einem
    /// Texteditor aufmachte — bei einem Programm, dessen ganzer Anspruch der
    /// Doppelklick ist. Die Folge war nicht bloß Unbequemlichkeit: Die
    /// Systemmeldung war ab Werk an, ohne dass ihr je jemand zugestimmt hätte,
    /// und die vier Wege ins Netz waren aus, ohne dass es jemand erfahren
    /// hätte. Beides stand nur in einer Datei, die niemand aufmacht.
    ///
    /// Was hier eingetragen wird, wirkt **beim nächsten Start**. Die Meldekette
    /// wird beim Start einmal aus der Einstellung gebaut und dann von den
    /// Arbeitern geteilt; sie mitten im Lauf auszutauschen wäre eine
    /// Änderung an der Maschinerie für einen Knopf, den man einmal im Jahr
    /// drückt. Der Hinweis steht deshalb im Fenster, statt dass jemand rätselt.
    fn draw_alert_section(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("BENACHRICHTIGUNGEN")
                .color(pal().primary)
                .size(theme::SMALL)
                .strong(),
        );
        ui.add_space(theme::S1);
        ui.label(
            RichText::new(
                "Wer erfährt es, wenn etwas gefunden wird. Verschickt werden Zeitpunkt, \
                 Rechnername, Adresse und Guthaben — die Seed-Wörter nie.",
            )
            .color(pal().muted)
            .size(theme::SMALL),
        );
        widgets::disclosure(
            ui,
            "warum_keine_woerter",
            "Die Wörter sind das Geld. Ein Meldedienst läuft auf fremden Rechnern, und eine \
             Nachricht, die dort liegen bleibt, wäre die Wallet. Darum stehen sie nur in der \
             Fundliste auf dieser Platte — die Meldung sagt bloß, dass du nachsehen sollst.",
        );
        ui.add_space(theme::S2);

        // Die Systemmeldung zuerst: sie ist die einzige, die nichts verlässt,
        // die einzige ohne Einrichtung — und die einzige, die ab Werk an ist.
        ui.checkbox(
            &mut self.alerts.desktop.enabled,
            RichText::new("Meldung auf diesem Rechner")
                .color(pal().text)
                .size(theme::BODY),
        );
        ui.label(
            RichText::new("Bleibt hier. Kein Konto, keine Einrichtung, kein Netz.")
                .color(pal().muted)
                .size(theme::SMALL),
        );
        ui.add_space(theme::S2);

        let field = |ui: &mut Ui, label: &str, value: &mut String, hint: &str, secret: bool| {
            ui.label(RichText::new(label).color(pal().dim).size(theme::SMALL));
            ui.add(
                egui::TextEdit::singleline(value)
                    .desired_width(ui.available_width())
                    .hint_text(hint)
                    .password(secret)
                    .font(mono(theme::SMALL)),
            );
            ui.add_space(theme::S1);
        };

        // ntfy: der einzige Weg ohne Konto, darum der erste der vier.
        ui.checkbox(
            &mut self.alerts.ntfy.enabled,
            RichText::new("ntfy — Meldung aufs Handy")
                .color(pal().text)
                .size(theme::BODY),
        );
        if self.alerts.ntfy.enabled {
            ui.label(
                RichText::new(
                    "Kostenlos und ohne Anmeldung: In der ntfy-App ein Thema abonnieren und \
                     denselben Namen hier eintragen. Wer den Namen kennt, liest mit — nimm \
                     etwas Langes, das niemand rät.",
                )
                .color(pal().muted)
                .size(theme::SMALL),
            );
            ui.add_space(theme::S1);
            field(
                ui,
                "Thema",
                &mut self.alerts.ntfy.topic,
                "mindestens 16 Zeichen, nicht zu erraten",
                false,
            );
            field(
                ui,
                "Server",
                &mut self.alerts.ntfy.base_url,
                "https://ntfy.sh",
                false,
            );
        }
        ui.add_space(theme::S1);

        ui.checkbox(
            &mut self.alerts.telegram.enabled,
            RichText::new("Telegram")
                .color(pal().text)
                .size(theme::BODY),
        );
        if self.alerts.telegram.enabled {
            ui.label(
                RichText::new(
                    "Braucht einen eigenen Bot (über @BotFather) und die Kennung des Chats, \
                     in den er schreiben soll.",
                )
                .color(pal().muted)
                .size(theme::SMALL),
            );
            ui.add_space(theme::S1);
            field(
                ui,
                "Bot-Token",
                &mut self.alerts.telegram.bot_token,
                "123456:ABC…",
                true,
            );
            field(
                ui,
                "Chat-Kennung",
                &mut self.alerts.telegram.chat_id,
                "z. B. 987654321",
                false,
            );
        }
        ui.add_space(theme::S1);

        ui.checkbox(
            &mut self.alerts.smtp.enabled,
            RichText::new("E-Mail").color(pal().text).size(theme::BODY),
        );
        if self.alerts.smtp.enabled {
            ui.label(
                RichText::new(
                    "Die Zugangsdaten deines Mailanbieters. Sie stehen danach in der \
                     Einstellungsdatei auf dieser Platte — nur für dich lesbar, aber im \
                     Klartext. Nimm ein App-Passwort, wenn dein Anbieter eines anbietet.",
                )
                .color(pal().muted)
                .size(theme::SMALL),
            );
            ui.add_space(theme::S1);
            field(
                ui,
                "Server",
                &mut self.alerts.smtp.host,
                "smtp.beispiel.de",
                false,
            );
            let mut port = self.alerts.smtp.port.to_string();
            ui.label(RichText::new("Port").color(pal().dim).size(theme::SMALL));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut port)
                        .desired_width(80.0)
                        .font(mono(theme::SMALL)),
                )
                .changed()
            {
                // Unlesbares stehen lassen statt auf null zu setzen: wer die
                // 587 löscht, um 465 zu tippen, hat zwischendurch ein leeres
                // Feld, und ein Port 0 wäre eine falsche Antwort darauf.
                if let Ok(p) = port.trim().parse::<u16>() {
                    self.alerts.smtp.port = p;
                }
            }
            ui.add_space(theme::S1);
            field(
                ui,
                "Benutzername",
                &mut self.alerts.smtp.username,
                "meist die Mailadresse",
                false,
            );
            field(ui, "Passwort", &mut self.alerts.smtp.password, "", true);
            field(
                ui,
                "Absender",
                &mut self.alerts.smtp.from,
                "ich@beispiel.de",
                false,
            );
            field(
                ui,
                "Empfänger",
                &mut self.alerts.smtp.to,
                "ich@beispiel.de",
                false,
            );
            ui.checkbox(
                &mut self.alerts.smtp.tls_implicit,
                RichText::new("Verschlüsselt ab dem ersten Byte (Port 465)")
                    .color(pal().dim)
                    .size(theme::SMALL),
            );
        }
        ui.add_space(theme::S1);

        ui.checkbox(
            &mut self.alerts.webhook.enabled,
            RichText::new("Webhook").color(pal().text).size(theme::BODY),
        );
        if self.alerts.webhook.enabled {
            ui.label(
                RichText::new("Für eigene Zwecke: die Meldung geht als JSON an diese Adresse.")
                    .color(pal().muted)
                    .size(theme::SMALL),
            );
            ui.add_space(theme::S1);
            field(
                ui,
                "Adresse",
                &mut self.alerts.webhook.url,
                "https://…",
                false,
            );
        }

        ui.add_space(theme::S2);
        if ui.add(widgets::button_primary("Speichern")).clicked() {
            self.alerts_note = Some(self.save_alerts());
        }
        ui.add_space(theme::S1);
        match &self.alerts_note {
            Some(Ok(msg)) => widgets::note(ui, pal().green, msg),
            Some(Err(msg)) => widgets::note(ui, pal().alert, msg),
            None => {
                ui.label(
                    RichText::new("Änderungen wirken beim nächsten Start des Programms.")
                        .color(pal().muted)
                        .size(theme::SMALL),
                );
            }
        }
    }

    /// Schreibt die bearbeiteten Meldewege in die Einstellungsdatei.
    ///
    /// Gelesen wird dafür zuerst noch einmal von der Platte, und nur der
    /// Abschnitt der Meldewege wird ersetzt: alles andere in der Datei gehört
    /// nicht diesem Bildschirm, und eine im Speicher gehaltene Kopie wäre
    /// älter als das, was inzwischen dort steht.
    ///
    /// Geprüft wird mit derselben Prüfung, die auch der Start benutzt. Sonst
    /// ließe sich hier eine Datei schreiben, mit der das Programm danach nicht
    /// mehr hochkommt — und der Bildschirm, auf dem man das repariert, ist
    /// genau dieser.
    fn save_alerts(&mut self) -> Result<String, String> {
        if let Some(missing) = missing_alert_field(&self.alerts) {
            return Err(missing);
        }
        let mut cfg = crate::config::Config::load_or_default(&self.config_path)
            .map_err(|e| format!("Die Einstellungsdatei ließ sich nicht lesen.\n\n{e}"))?;
        cfg.alerts = self.alerts.clone();
        cfg.validate()
            .map_err(|e| format!("So geht es nicht:\n\n{e}"))?;
        cfg.save(&self.config_path)
            .map_err(|e| format!("Gespeichert werden konnte es nicht.\n\n{e}"))?;

        let on = cfg.notifiers().len() + usize::from(cfg.alerts.desktop.enabled);
        Ok(match on {
            0 => "Gespeichert. Es ist kein Meldeweg an — ein Fund stünde dann nur in der \
                  Fundliste auf dieser Platte."
                .to_string(),
            1 => "Gespeichert. Ein Meldeweg ist an. Er wirkt beim nächsten Start.".to_string(),
            n => format!("Gespeichert. {n} Meldewege sind an. Sie wirken beim nächsten Start."),
        })
    }

    /// Wo die Dateien des Programms liegen — mit einem Knopf, der sie zeigt.
    ///
    /// Vorher stand der Pfad nirgends. Das Programm legt eine 145-MB-Datenbank
    /// und Seeds im Klartext unter „Library/Application Support" ab, einen
    /// Ordner, den der Finder standardmäßig ausblendet — und die einzige
    /// Erwähnung war „Gespeichert in hits.jsonl", ein Dateiname ohne Ort, was
    /// schlechter ist als gar nichts.
    ///
    /// Dann stand er zweimal in voller Länge da, jeder in einem eigenen Kasten
    /// über zwei Zeilen: „/Users/…/Library/Application Support/Schatzsuche/…".
    /// Das ist die andere Übertreibung. Niemand tippt einen solchen Pfad ab,
    /// und der Knopf daneben führt genau dorthin. Geblieben sind die zwei
    /// Dateinamen — die sagen, **was** dort liegt — und ein Knopf, der den
    /// Ordner öffnet und die Fundliste darin markiert.
    fn draw_data_section(&self, ui: &mut Ui) {
        ui.add_space(theme::S3);
        ui.separator();
        ui.add_space(theme::S3);
        ui.label(
            RichText::new("DEINE DATEN")
                .color(pal().primary)
                .size(theme::SMALL)
                .strong(),
        );
        ui.add_space(theme::S2);

        let name_of = |p: &std::path::Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        widgets::kv(ui, "Gefundene Seeds", &name_of(&self.hits_path), pal().text);
        widgets::kv(ui, "Adressliste", &name_of(&self.db_path), pal().text);
        ui.add_space(theme::S2);

        if ui
            .add(
                widgets::button_quiet("Im Finder zeigen")
                    .min_size(Vec2::new(ui.available_width(), 30.0)),
            )
            .clicked()
        {
            widgets::reveal(&self.hits_path);
        }
        ui.add_space(theme::S2);
        ui.label(
            RichText::new(
                "Die Fundliste ist eine einfache Textdatei — Doppelklick genügt. \
                 Beides liegt nur auf dieser Platte und geht nirgendwohin.",
            )
            .color(pal().muted)
            .size(theme::SMALL),
        );
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
/// When `repairable` names a path, the panel stops being a dead end: the one
/// failure the program can fix by itself — no database at all — is presented
/// as an offer with a button. Returns true when that offer was accepted.
///
/// The alternative was a Close button and a line telling the reader to open a
/// terminal and run a command, on a program whose whole point is that it is
/// double-clicked. That is where most first launches ended.
/// Was der Leser auf dem Fehlerbildschirm angeklickt hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorAction {
    None,
    /// Die Übungsliste anlegen und erneut laden.
    Build,
    /// In den Wiederherstellungs-Assistenten wechseln.
    Recover,
}

pub(crate) fn draw_error_panel(
    ctx: &egui::Context,
    message: &str,
    repairable: Option<&std::path::Path>,
) -> ErrorAction {
    let mut action = ErrorAction::None;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(pal().bg)
                .inner_margin(egui::Margin::symmetric(theme::S4, theme::S3)),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(if repairable.is_some() {
                            "Noch ein Schritt, dann kann es losgehen"
                        } else {
                            "Die Schatzsuche konnte nicht starten"
                        })
                        .color(if repairable.is_some() {
                            pal().warn
                        } else {
                            pal().alert
                        })
                        .size(theme::TITLE)
                        .strong(),
                    );
                    ui.add_space(theme::S3);
                    egui::Frame::none()
                        .fill(pal().panel)
                        .rounding(theme::r_sm())
                        .stroke(theme::hairline())
                        .inner_margin(egui::Margin::same(theme::S3))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                RichText::new(message)
                                    .color(pal().text)
                                    .font(mono(theme::SMALL)),
                            );
                        });

                    if repairable.is_some() {
                        ui.add_space(theme::S3);
                        // Zwei Zeilen. Was eine Übungsliste ist und warum sie
                        // nichts kostet, stand hier vorher in 302 Zeichen am
                        // Stück; es steht jetzt hinter dem Aufklapper darunter.
                        ui.label(
                            RichText::new(
                                "Die Suche braucht eine Liste von Adressen zum Vergleichen. \
                                 Das Programm kann sich eine Übungsliste selbst bauen.",
                            )
                            .color(pal().text)
                            .size(theme::BODY),
                        );
                        ui.add_space(theme::S2);
                        widgets::disclosure(
                            ui,
                            "uebungsliste",
                            "Die Übungsliste besteht aus ausgedachten Adressen, nicht aus \
                             echten Wallets. Für den Zweck des Programms macht das keinen \
                             Unterschied: die Suche läuft genauso schnell und findet genauso \
                             wenig — das ist ja die Aussage. Wer mit echten Adressen rechnen \
                             will, lädt einen Adress-Auszug herunter und baut die Liste mit \
                             dem Befehl „build-db“; wie das geht, steht in der README.",
                        );

                        ui.add_space(theme::S3);
                        if ui
                            .add(widgets::button_primary("Übungsliste anlegen"))
                            .clicked()
                        {
                            action = ErrorAction::Build;
                        }
                        ui.add_space(theme::S1);
                        ui.label(
                            RichText::new(format!(
                                "{} ausgedachte Adressen · etwa 145 MB · rund eine Sekunde",
                                util::group_digits(crate::startup::PRACTICE_RECORDS as u64)
                            ))
                            .color(pal().dim)
                            .size(theme::SMALL),
                        );
                    }

                    // Der zweite Ausweg, und er steht hier unabhängig davon, ob
                    // sich die Datenbank reparieren lässt: die Wiederherstellung
                    // einer eigenen Seed braucht mit Zieladresse überhaupt keine
                    // Datenbank. Sie hinter diesem Bildschirm einzusperren hieß,
                    // die nützliche Hälfte des Programms genau dann unerreichbar
                    // zu machen, wenn sicher noch nichts eingerichtet ist.
                    ui.add_space(theme::S4);
                    ui.separator();
                    ui.add_space(theme::S3);
                    ui.label(
                        RichText::new("Du wolltest eine eigene Seed wiederherstellen?")
                            .color(pal().text)
                            .size(theme::BODY)
                            .strong(),
                    );
                    ui.add_space(theme::S1);
                    ui.label(
                        RichText::new(
                            "Das geht auch ohne Adressliste. Nichts davon verlässt \
                             diesen Rechner.",
                        )
                        .color(pal().dim)
                        .size(theme::SMALL),
                    );
                    ui.add_space(theme::S2);
                    if ui.add(widgets::button_quiet("Seed retten")).clicked() {
                        action = ErrorAction::Recover;
                    }

                    ui.add_space(theme::S4);
                    // Align::TOP, not Center: inside a scroll area that fills
                    // the window, centring drops the button into the middle of
                    // the empty space below the text instead of under it.
                    ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                        if ui.add(widgets::button_quiet("Schließen")).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
        });
    action
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
            theme::clear_color()
        }

        fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
            theme::apply(ctx);
            // Das eigenständige Fehlerfenster kennt weder Datenbank noch
            // Assistenten; hier gibt es nur „Schließen".
            let _ = draw_error_panel(ctx, &self.msg, None);
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

    /// Ein Fenster auf dem Dashboard, für die Tests unten.
    ///
    /// Absichtlich nicht im Intro: ein frisch gebautes `GuiApp` steht auf der
    /// Gabelung, und ein Tastaturtest, der dort beginnt, verbraucht seinen
    /// ersten Tastendruck fürs Weiterrücken und beweist nichts — so waren zwei
    /// von diesen beim ersten Mal geschrieben.
    fn blank_app(boot: Option<BootFn>) -> GuiApp {
        let mut app = raw_app(boot);
        app.screen = Screen::Dashboard;
        app
    }

    fn raw_app(boot: Option<BootFn>) -> GuiApp {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut app = GuiApp::new(
            Arc::new(Stats::new()),
            Arc::new(Control::new(1, 20, Priority::Normal)),
            rx,
            Vec::new(),
            0,
            60,
            1,
            0,
            0,
            None,
            Some(Arc::new(Progress::new())),
            boot,
            Arc::new(std::sync::Mutex::new(None)),
            Paths::default(),
        );
        // Eine Handvoll Datensätze statt fünf Millionen. Die Tests, die den
        // Reparaturweg prüfen, liefen vorher in ihr Zeitlimit, weil sie dafür
        // 145 MB schreiben mussten — eine Verzweigung zu prüfen braucht das
        // nicht.
        app.practice_records = 2_000;
        app
    }

    /// Ein Fenster auf dem Fehlerbildschirm, mit reparierbarer Datenbank.
    fn failed_app(boot: Option<BootFn>, missing: &std::path::Path) -> GuiApp {
        let mut app = raw_app(boot);
        app.screen = Screen::Failed {
            message: "keine Datenbank".into(),
            repairable: Some(missing.to_path_buf()),
        };
        app
    }

    /// The whole point of the offer: pressing it must leave a usable database
    /// behind and then run the load again, without the reader touching a
    /// terminal. Exercised through the same method the button calls.
    #[test]
    fn the_offer_builds_a_database_and_loads_again() {
        let dir = std::env::temp_dir().join(format!("sc-repair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("funded.scdb");

        let booted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&booted);
        let mut app = failed_app(
            Some(Arc::new(move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })),
            &path,
        );

        app.build_practice_db();
        assert!(
            matches!(app.screen, Screen::Loading),
            "der Bildschirm muss sofort weiterrücken, war {}",
            app.screen.name()
        );

        let progress = app.progress.clone().unwrap();
        for _ in 0..600 {
            if booted.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(progress.error(), None, "building must not fail");
        assert!(path.exists(), "a database must be on disk now");
        assert!(
            booted.load(std::sync::atomic::Ordering::SeqCst),
            "and the load must have re-run"
        );
        assert!(
            crate::lookup::Database::open(&path).is_ok(),
            "and what was written must actually open"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Nach dem Anlegen einer Übungsliste muss das Fenster dauerhaft wissen,
    /// dass die geladenen Adressen ausgedacht sind — sonst steht auf dem
    /// Dashboard eine Zahl, die nach echten Wallets aussieht.
    #[test]
    fn building_the_practice_list_marks_it_as_practice() {
        let dir = std::env::temp_dir().join(format!("sc-mark-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = failed_app(Some(Arc::new(|| Ok(()))), &dir.join("funded.scdb"));

        assert!(!app.practice_list, "vorher ist nichts bekannt");
        app.build_practice_db();
        assert!(app.practice_list, "danach steht es fest");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Presses keys at a real egui context and lets the window handle them,
    /// which is the only way to prove the wiring rather than the intent.
    ///
    /// `focus_a_field` puts a text box on the frame and gives it the keyboard,
    /// standing in for the recovery screen's word fields.
    fn press(app: &mut GuiApp, keys: &[egui::Key], focus_a_field: bool) {
        press_with(app, keys, egui::Modifiers::default(), focus_a_field);
    }

    fn press_with(
        app: &mut GuiApp,
        keys: &[egui::Key],
        modifiers: egui::Modifiers,
        focus_a_field: bool,
    ) {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput {
            modifiers,
            ..Default::default()
        };
        for key in keys {
            input.events.push(egui::Event::Key {
                key: *key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
        }
        let _ = ctx.run(input, |ctx| {
            if focus_a_field {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut text = String::new();
                    ui.text_edit_singleline(&mut text).request_focus();
                });
            }
            app.handle_keys(ctx);
        });
    }

    /// Die Restzeit kommt aus der gemessenen Rate — und schweigt, solange es
    /// dafür zu früh ist. Eine Schätzung, die mit „vier Stunden" anfängt und
    /// gleich darauf „zwei Minuten" sagt, ist schlimmer als keine.
    #[test]
    fn the_remaining_time_is_measured_and_waits_for_data() {
        // Zu früh: keine volle Melderunde, oder keine zwei Sekunden.
        assert_eq!(remaining_secs(0, 4_194_304, 10.0), None, "nichts gezählt");
        assert_eq!(
            remaining_secs(crate::recover::REPORT_EVERY - 1, 4_194_304, 10.0),
            None,
            "unter einer Melderunde"
        );
        assert_eq!(
            remaining_secs(100_000, 4_194_304, 1.9),
            None,
            "unter zwei Sekunden"
        );

        // Halb durch in zehn Sekunden heißt: noch etwa zehn.
        let half = remaining_secs(2_000_000, 4_000_000, 10.0).expect("genug Daten");
        assert!((half - 10.0).abs() < 0.001, "{half}");

        // Ein Viertel in zehn Sekunden heißt: noch etwa dreißig.
        let quarter = remaining_secs(1_000_000, 4_000_000, 10.0).expect("genug Daten");
        assert!((quarter - 30.0).abs() < 0.001, "{quarter}");

        // Am Ziel gibt es nichts mehr zu warten, und nichts darf negativ werden.
        assert_eq!(remaining_secs(4_000_000, 4_000_000, 10.0), None);
        assert_eq!(remaining_secs(4_000_001, 4_000_000, 10.0), None);
    }

    /// During the intro the first key spends itself skipping it, and must not
    /// also stop the search that is only just starting.
    #[test]
    fn a_key_skips_the_intro_and_nothing_else() {
        let mut app = raw_app(None);
        app.screen = Screen::Intro {
            until: Instant::now() + INTRO,
        };

        press(&mut app, &[egui::Key::Space], false);
        assert!(
            matches!(app.screen, Screen::Chooser),
            "der Vorhang muss weg sein, war {}",
            app.screen.name()
        );
        assert!(
            !app.control.paused(),
            "the key that skipped the intro must not also stop the search"
        );
    }

    /// Und das Intro läuft von selbst ab, ohne dass jemand eine Taste drückt.
    #[test]
    fn the_intro_lifts_by_itself() {
        let mut app = raw_app(None);
        app.screen = Screen::Intro {
            until: Instant::now() + Duration::from_secs(30),
        };
        assert!(!app.tick_intro(), "noch nicht");
        assert!(matches!(app.screen, Screen::Intro { .. }));

        app.screen = Screen::Intro {
            until: Instant::now(),
        };
        assert!(app.tick_intro(), "jetzt");
        assert!(matches!(app.screen, Screen::Chooser));

        // Auf jedem anderen Bildschirm ist es ein Nichtstun.
        app.screen = Screen::Dashboard;
        assert!(!app.tick_intro());
        assert!(matches!(app.screen, Screen::Dashboard));
    }

    /// Durch die Tür zu gehen heißt nicht, die Suche zu starten.
    ///
    /// Ein Fensterstart hält den Collider an (`run_collider`), und die Gabelung
    /// darf ihn nicht heimlich loslassen: wer auf dem Suchbildschirm ankommt,
    /// soll vor stehenden Zahlen und einem Knopf „Suche starten" stehen. Vorher
    /// lief sie ab dem Türklick, und die Laufzeit zählte schon, bevor jemand
    /// zugestimmt hatte.
    #[test]
    fn entering_the_search_does_not_start_it() {
        let mut app = raw_app(None);
        app.control.set_paused(true);
        app.screen = Screen::Chooser;

        app.enter_dashboard();

        assert!(
            matches!(app.screen, Screen::Dashboard),
            "die Tür muss auf den Suchbildschirm führen, war {}",
            app.screen.name()
        );
        assert!(
            app.control.paused(),
            "die Suche muss angehalten bleiben, bis jemand sie startet"
        );
    }

    /// The README has promised the space bar since before the window existed.
    #[test]
    fn space_starts_and_stops_the_search() {
        let mut app = blank_app(None);
        assert!(!app.control.paused());

        press(&mut app, &[egui::Key::Space], false);
        assert!(app.control.paused(), "space must stop the search");

        press(&mut app, &[egui::Key::Space], false);
        assert!(!app.control.paused(), "and start it again");
    }

    /// The one that would actually hurt: a space typed into a word of somebody's
    /// seed must land in the word, not stop the search behind the screen.
    #[test]
    fn typing_into_a_field_never_reaches_the_shortcuts() {
        let mut app = blank_app(None);

        press(&mut app, &[egui::Key::Space], true);
        assert!(
            !app.control.paused(),
            "a space belongs to the field that has the keyboard"
        );
    }

    /// Escape backs out of the recovery screen rather than quitting.
    #[test]
    fn escape_leaves_the_recovery_screen() {
        let mut app = blank_app(None);
        app.open_recover(Screen::Dashboard);

        press(&mut app, &[egui::Key::Escape], false);
        assert!(!app.screen.is_recover(), "escape must return to the search");
    }

    /// The platform shortcut for preferences, and it works mid-typing — but
    /// not on a screen that has no settings drawer to open.
    #[test]
    fn the_platform_shortcut_opens_the_settings() {
        let mut app = blank_app(None);
        assert!(!app.settings_open);

        press_with(
            &mut app,
            &[egui::Key::Comma],
            egui::Modifiers::COMMAND,
            false,
        );
        assert!(app.settings_open, "⌘, must open the settings");

        press_with(
            &mut app,
            &[egui::Key::Comma],
            egui::Modifiers::COMMAND,
            true,
        );
        assert!(!app.settings_open, "and close them again while typing");

        app.open_recover(Screen::Dashboard);
        press_with(
            &mut app,
            &[egui::Key::Comma],
            egui::Modifiers::COMMAND,
            false,
        );
        assert!(
            !app.settings_open,
            "a drawer that is not on this screen must stay shut"
        );
    }

    /// Escape out of a focused word field leaves the field, and only the next
    /// one leaves the screen. Run across two frames of one context, because
    /// that is the whole point: the first press must be absorbed by the field.
    #[test]
    fn escape_leaves_the_field_before_it_leaves_the_screen() {
        let mut app = blank_app(None);
        app.open_recover(Screen::Dashboard);

        let ctx = egui::Context::default();
        let mut text = String::new();
        let frame = |app: &mut GuiApp, focus: bool, text: &mut String| {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            });
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.text_edit_singleline(text);
                    if focus {
                        r.request_focus();
                    }
                });
                app.handle_keys(ctx);
            });
        };

        frame(&mut app, true, &mut text);
        assert!(
            app.screen.is_recover(),
            "the first escape belongs to the field, not the screen"
        );

        frame(&mut app, false, &mut text);
        assert!(
            !app.screen.is_recover(),
            "the second escape must reach the screen"
        );
    }

    /// And closes the settings drawer when that is what is open.
    #[test]
    fn escape_closes_the_settings_drawer() {
        let mut app = blank_app(None);
        app.settings_open = true;

        press(&mut app, &[egui::Key::Escape], false);
        assert!(!app.settings_open);
    }

    /// While the recovery screen is up, the search behind it is not the
    /// keyboard's business.
    #[test]
    fn the_recovery_screen_owns_the_keyboard() {
        let mut app = blank_app(None);
        app.open_recover(Screen::Dashboard);

        press(&mut app, &[egui::Key::Space], false);
        assert!(
            !app.control.paused(),
            "space must not reach a search that is not on screen"
        );
    }

    #[test]
    fn arrows_walk_the_hit_list_and_stop_at_its_ends() {
        // Nothing to walk.
        assert_eq!(step_selection(None, 0, true), None);
        assert_eq!(step_selection(Some(3), 0, false), None);

        // Entering the list from either end.
        assert_eq!(step_selection(None, 5, true), Some(0));
        assert_eq!(step_selection(None, 5, false), Some(4));

        // Walking, and stopping rather than wrapping.
        assert_eq!(step_selection(Some(2), 5, true), Some(3));
        assert_eq!(step_selection(Some(2), 5, false), Some(1));
        assert_eq!(step_selection(Some(4), 5, true), Some(4), "must not wrap");
        assert_eq!(step_selection(Some(0), 5, false), Some(0), "must not wrap");
    }

    /// Two clicks before the first has taken effect must not start two engines.
    #[test]
    fn the_offer_cannot_be_accepted_twice() {
        let dir = std::env::temp_dir().join(format!("sc-twice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = Arc::clone(&runs);
        let mut app = failed_app(
            Some(Arc::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })),
            &dir.join("funded.scdb"),
        );

        app.build_practice_db();
        app.build_practice_db();
        app.build_practice_db();

        // Wait for the one run that should happen — building the records takes
        // about a second — and only then check that no second one followed.
        for _ in 0..600 {
            if runs.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the load may only be started once"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Without a way to load again the offer must not appear at all; a button
    /// that builds a file and then leaves the reader on the same dead screen
    /// is worse than no button.
    #[test]
    fn no_offer_without_a_way_to_retry() {
        let mut app = failed_app(None, std::path::Path::new("somewhere.scdb"));
        app.build_practice_db();
        assert!(!app.repairing, "nothing may have been started");
        assert!(
            matches!(app.screen, Screen::Failed { .. }),
            "und der Bildschirm bleibt stehen"
        );
    }

    /// Ein Treffer, der nicht gespeichert werden konnte, muss im Fenster
    /// ankommen. Vorher wurde die Meldung in ein Feld gelegt, das nie gezeichnet
    /// wurde — der eine Fehlerfall, den `engine.rs` ausdrücklich als „darf
    /// niemals verschluckt werden" bezeichnet, war im Fenster unsichtbar, und
    /// der Treffer stand daneben in der Liste, als läge er sicher.
    #[test]
    fn a_hit_that_could_not_be_saved_reaches_the_window() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = raw_app(None);
        app.events = rx;

        let hit = crate::hits::Hit::synthetic();
        let id = hit.id.clone();
        tx.send(Event::PersistFailure {
            hit: Box::new(hit),
            error: "No space left on device".into(),
        })
        .unwrap();

        app.drain();

        assert_eq!(app.hits.len(), 1, "der Treffer steht in der Liste");
        assert!(
            app.unsaved.contains(&id),
            "und ist als nicht gespeichert vermerkt"
        );
        assert_eq!(app.errors.len(), 1, "und es gibt eine Meldung dazu");
        assert!(
            app.errors[0].contains("No space left on device"),
            "die den Grund nennt: {}",
            app.errors[0]
        );
    }

    /// Eine fehlgeschlagene Sicherungskopie ist eine Meldung, aber kein
    /// verlorener Treffer — die Hauptdatei hat ihn ja.
    #[test]
    fn a_failed_backup_is_reported_without_condemning_the_hit() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = raw_app(None);
        app.events = rx;

        tx.send(Event::BackupFailure {
            id: "abc123".into(),
            error: "Read-only file system".into(),
        })
        .unwrap();
        app.drain();

        assert_eq!(app.errors.len(), 1);
        assert!(app.unsaved.is_empty(), "nichts ist verloren gegangen");
        assert!(app.hits.is_empty());
    }

    /// Ein Treffer mit echtem Guthaben, für die Tests unten.
    fn real_hit(address: &str) -> crate::hits::Hit {
        let mut h = crate::hits::Hit::synthetic();
        h.address = address.to_string();
        h.id = crate::hits::Hit::make_id(address, &h.derivation_path);
        // `is_synthetic` hängt an der Entropie, nicht an der Adresse.
        h.entropy_hex = "9f86d081884c7d659a2feaa0c55ad015".into();
        h
    }

    /// Ein Testeintrag darf niemanden wecken: Dock, Ton und Band bleiben stumm,
    /// und im Fundfach steht er auch nicht.
    #[test]
    fn ein_testeintrag_loest_keinen_alarm_aus() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = raw_app(None);
        app.events = rx;

        tx.send(Event::Hit(Box::new(crate::hits::Hit::synthetic())))
            .unwrap();
        app.drain();

        assert!(app.pending.is_none(), "er meldet sich nicht");
        assert_eq!(app.real_hits().count(), 0, "und er zählt nicht mit");
    }

    /// `hits.jsonl` wird beim Start eingelesen, und ein einziger alter
    /// Selbsttest aus dem Terminal (`--test-alert`) ließ das Fundfach als „nicht
    /// leer" gelten — der Leerzustand samt Erklärung verschwand dann für immer.
    /// Er hängt jetzt an den echten Funden, nicht an der Länge der Liste.
    #[test]
    fn ein_alter_testeintrag_fuellt_das_fundfach_nicht() {
        let mut app = raw_app(None);
        app.hits = vec![crate::hits::Hit::synthetic(), crate::hits::Hit::synthetic()];

        assert!(!app.hits.is_empty(), "in der Datei stehen sie sehr wohl");
        assert_eq!(
            app.real_hits().count(),
            0,
            "das Fundfach gilt trotzdem als leer"
        );

        // Ein echter Fund dazwischen muss auffindbar bleiben, und zwar mit
        // seinem Platz in `hits` — daran hängen Auswahl und Band.
        app.hits.insert(1, real_hit("bc1qecht"));
        let real: Vec<usize> = app.real_hits().map(|(i, _)| i).collect();
        assert_eq!(real, vec![1]);
    }

    #[test]
    fn ein_echter_fund_meldet_sich() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = raw_app(None);
        app.events = rx;

        tx.send(Event::Hit(Box::new(real_hit("bc1qecht")))).unwrap();
        app.drain();

        let p = app.pending.expect("ein echter Fund muss sich melden");
        assert_eq!(p.newest, 0);
        assert_eq!(p.count, 1);
        assert!(!p.announced, "Dock und Ton kommen erst im nächsten Bild");
    }

    /// Der Test, der den Fehlalarm beim Neustart verhindert: was beim Öffnen aus
    /// hits.jsonl nachgeladen wird, ist alt und darf nicht das Dock hüpfen
    /// lassen.
    #[test]
    fn nachgeladene_treffer_wecken_niemanden() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let app = GuiApp::new(
            Arc::new(Stats::new()),
            Arc::new(Control::new(1, 20, Priority::Normal)),
            rx,
            vec![real_hit("bc1qvonletzterwoche")],
            0,
            60,
            1,
            0,
            0,
            None,
            Some(Arc::new(Progress::new())),
            None,
            Arc::new(std::sync::Mutex::new(None)),
            Paths::default(),
        );
        assert_eq!(app.hits.len(), 1);
        assert!(
            app.pending.is_none(),
            "ein Fund von letzter Woche darf beim Öffnen nicht Alarm schlagen"
        );
    }

    /// Ein Treffer, der nicht gespeichert werden konnte, muss **lauter** sein
    /// als ein normaler — dort existieren die Wörter nur noch im Speicher.
    #[test]
    fn ein_unspeicherbarer_treffer_meldet_sich_genauso_laut() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = raw_app(None);
        app.events = rx;

        let hit = real_hit("bc1qverloren");
        let id = hit.id.clone();
        tx.send(Event::PersistFailure {
            hit: Box::new(hit),
            error: "No space left on device".into(),
        })
        .unwrap();
        app.drain();

        assert!(app.pending.is_some(), "er muss sich melden");
        assert!(app.unsaved.contains(&id), "und als verloren vermerkt sein");
        assert_eq!(app.errors.len(), 1, "und eine Meldung tragen");
    }

    /// Die Klartextregel: ein Fund darf die Wörter nicht von selbst aufdecken.
    /// Vorher setzte `drain()` die Auswahl, und damit standen zwölf bis
    /// vierundzwanzig Wörter auf dem Bildschirm, ohne dass jemand geklickt
    /// hatte — auf einem Rechner, der gerade geteilt sein könnte.
    #[test]
    fn ein_fund_deckt_die_woerter_nicht_von_selbst_auf() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = raw_app(None);
        app.events = rx;

        tx.send(Event::Hit(Box::new(real_hit("bc1qgeheim"))))
            .unwrap();
        app.drain();

        assert_eq!(
            app.selected, None,
            "die Wörter erscheinen erst auf einen bewussten Klick"
        );
    }

    /// Wer seine eigene Seed rettet, wird von einem Fund nicht aus dem
    /// Assistenten geworfen — das halb ausgefüllte Wortformular ist wertvoller
    /// als die Neugier.
    #[test]
    fn das_band_wirft_niemanden_aus_dem_assistenten() {
        let mut app = raw_app(None);
        app.open_recover(Screen::Dashboard);
        if let Screen::Recover { ui, .. } = &mut app.screen {
            ui.slots[0].word = "legal".into();
        }

        app.hits.push(real_hit("bc1qwaehrenddessen"));
        app.note_find(0);
        assert!(app.pending.is_some());

        // Der Wortlaut des Bands kennt den Assistenten und bietet dort keinen
        // Knopf an, der wegführt.
        let (_, body) = find_band_text(1, false, true);
        assert!(body.contains("wartet"), "{body}");

        match &app.screen {
            Screen::Recover { ui, .. } => {
                assert_eq!(ui.slots[0].word, "legal", "das Formular steht noch")
            }
            other => panic!("aus dem Assistenten geworfen: {}", other.name()),
        }
    }

    #[test]
    fn eine_adresse_wird_in_der_mitte_gekuerzt() {
        let a = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";
        let s = shorten_middle(a, 14, 6);
        assert!(s.starts_with("bc1qcr8te4kr60"), "{s}");
        assert!(s.ends_with("306fyu"), "{s}");
        assert!(s.len() < a.len());

        // Was ohnehin passt, bleibt unangetastet.
        assert_eq!(shorten_middle("kurz", 14, 6), "kurz");
        assert_eq!(shorten_middle("", 14, 6), "");
        assert_eq!(shorten_middle("x", 3, 3), "x");
    }

    /// Der Wächter über die Ehrlichkeitsregel: der Leerzustand beschreibt im
    /// Konjunktiv, was passieren *würde*, und nennt keine Zahl. Eine Null
    /// bedeutet nur etwas, wenn man mit einer Eins rechnet.
    #[test]
    fn der_leerzustand_verspricht_nichts() {
        let (what, how) = find_section_empty_text();
        let both = format!("{what} {how}");

        assert!(what.contains("stünde"), "Konjunktiv fehlt: {what}");
        for verboten in ["0", "noch kein", "bisher", "erste", "bald"] {
            assert!(
                !both.to_lowercase().contains(verboten),
                "„{verboten}“ weckt Erwartung: {both}"
            );
        }
    }

    /// Der Wortlaut des Bands. Der letzte Satz ist der wichtige: wenn das
    /// Speichern scheitert, existiert die Seed nur noch im Arbeitsspeicher.
    #[test]
    fn the_banner_says_what_happened_and_what_to_do() {
        let (t1, b1) = error_banner_text(&["Platte voll".to_string()]);
        assert!(t1.contains("Ein Treffer"), "{t1}");
        assert!(b1.contains("Platte voll"), "der Grund steht drin");
        assert!(b1.contains("schreib sie ab"), "und die Handlung: {b1}");

        let (t2, _) = error_banner_text(&["a".into(), "b".into(), "c".into()]);
        assert!(t2.starts_with('3'), "mehrere werden gezählt: {t2}");

        // Entartet, aber darf nicht in Panik enden.
        let (t3, _) = error_banner_text(&[]);
        assert!(!t3.is_empty());
    }

    /// Der zweite Ausweg vom Fehlerbildschirm: die Wiederherstellung braucht
    /// mit Zieladresse keine Datenbank, also darf sie nicht dahinter
    /// eingesperrt sein — und „Zurück" muss wieder auf den Fehlerbildschirm
    /// führen statt auf ein Dashboard, hinter dem nichts geladen ist.
    #[test]
    fn recovery_is_reachable_from_the_error_screen_and_leads_back_to_it() {
        let mut app = failed_app(None, std::path::Path::new("nirgends.scdb"));

        app.open_recover(Screen::Failed {
            message: "keine Datenbank".into(),
            repairable: None,
        });
        assert!(app.screen.is_recover(), "der Assistent muss offen sein");

        app.leave_recover();
        assert!(
            matches!(app.screen, Screen::Failed { .. }),
            "der Rückweg führt auf den Fehlerbildschirm, war {}",
            app.screen.name()
        );
    }

    #[test]
    fn throughput_model_reflects_the_measurements() {
        // The eight-core M1 the curve was measured on, named explicitly. The
        // machine running the test is irrelevant here and must stay that way:
        // asking for eight threads on a four-core runner used to fold the top
        // of the curve onto itself and fail an assertion about the curve.
        const M1: usize = 8;
        let share = |t: usize, p: Priority| GuiApp::share_at(t, M1, p);

        // More cores must mean more throughput at normal priority.
        let normal: Vec<f64> = [1usize, 2, 4, 8]
            .iter()
            .map(|&t| share(t, Priority::Normal))
            .collect();
        assert!(
            normal.windows(2).all(|w| w[1] > w[0]),
            "normal priority should scale up: {normal:?}"
        );

        // Background priority is the exception: past half the cores it gets
        // worse, because the work is confined to the efficiency cores.
        let bg8 = share(8, Priority::Background);
        let bg4 = share(4, Priority::Background);
        assert!(
            bg8 < bg4,
            "background at 8 cores measured slower than at 4: {bg8} vs {bg4}"
        );
        assert!(GuiApp::counterproductive_at(8, M1, Priority::Background));
        assert!(!GuiApp::counterproductive_at(4, M1, Priority::Background));
        assert!(!GuiApp::counterproductive_at(8, M1, Priority::Normal));

        // Background must always be well below normal at the same core count.
        for t in [2usize, 4] {
            assert!(
                share(t, Priority::Background) < share(t, Priority::Normal) * 0.7,
                "background should be clearly slower at {t} cores"
            );
        }
    }

    /// Startup has to land on one of the four modes whatever the config says,
    /// or the panel shows four unlit rows and no answer to "what is running".
    #[test]
    fn every_configuration_lands_on_a_mode() {
        let m = crate::machine::Machine {
            physical: 8,
            performance: 4,
            efficiency: 4,
        };
        let table = modes(&m);

        // A configuration that already names a mode stays where it is.
        for (i, (t, p, d)) in table.iter().enumerate() {
            assert_eq!(
                nearest_mode(&m, *t, *p, *d),
                i,
                "mode {i} did not match itself"
            );
        }

        // The middle priority belongs to no mode at all. It has to resolve
        // somewhere, and the recommended mode is the least surprising answer.
        assert_eq!(nearest_mode(&m, 4, Priority::Utility, 100), 2);

        // Off by a core or two: nearest by worker count within the priority.
        assert_eq!(nearest_mode(&m, 2, Priority::Normal, 100), 2);
        assert_eq!(nearest_mode(&m, 7, Priority::Normal, 100), 3);
        assert_eq!(nearest_mode(&m, 8, Priority::Background, 100), 1);

        // A throttled single worker is the unobtrusive mode, not the quiet one.
        assert_eq!(nearest_mode(&m, 1, Priority::Background, 1), 0);
    }

    /// The model has to behave on machines that are not the one it was
    /// measured on — including the ones the tests run on.
    #[test]
    fn the_model_holds_on_other_machines() {
        for max in [1usize, 2, 3, 4, 6, 8, 16, 64] {
            let full = GuiApp::share_at(max, max, Priority::Normal);
            let half = GuiApp::share_at((max / 2).max(1), max, Priority::Normal);
            assert!(
                (0.0..=1.0).contains(&full) && full > 0.0,
                "share out of range on {max} cores: {full}"
            );
            assert!(
                half <= full,
                "half a {max}-core machine cannot beat all of it: {half} vs {full}"
            );
            // Whatever the machine, using all of it at background priority is
            // the case the interface warns about.
            assert!(GuiApp::counterproductive_at(max, max, Priority::Background));
            assert!(!GuiApp::counterproductive_at(max, max, Priority::Normal));
        }
    }

    /// Ein Meldeweg, der an ist, aber nichts verschicken kann, muss beim
    /// Speichern auffallen — nicht erst an dem Tag, an dem es darauf ankäme.
    #[test]
    fn a_half_filled_alert_channel_is_caught_before_it_is_saved() {
        use crate::config::Alerts;

        // Ab Werk ist nur die Meldung auf diesem Rechner an, und die braucht
        // nichts. Daran darf die Prüfung nicht hängen bleiben.
        assert!(super::missing_alert_field(&Alerts::default()).is_none());

        // Der Beispielserver aus der Vorlage ist kein Mailserver.
        let mut a = Alerts::default();
        a.smtp.enabled = true;
        let complained = super::missing_alert_field(&a).expect("smtp.example.com muss auffallen");
        assert!(
            complained.contains("Beispielserver"),
            "unerwarteter Text: {complained}"
        );

        // Server richtig, aber ohne Absender und Empfänger geht es trotzdem
        // nicht — und die Prüfung muss den zweiten Mangel auch dann noch
        // sehen, wenn der erste behoben ist.
        a.smtp.host = "smtp.posteo.de".into();
        assert!(super::missing_alert_field(&a).is_some());
        a.smtp.from = "ich@posteo.de".into();
        a.smtp.to = "ich@posteo.de".into();
        assert!(super::missing_alert_field(&a).is_none());

        // Telegram ohne Token, Webhook ohne Adresse.
        let mut t = Alerts::default();
        t.telegram.enabled = true;
        assert!(super::missing_alert_field(&t).is_some());

        let mut w = Alerts::default();
        w.webhook.enabled = true;
        assert!(super::missing_alert_field(&w).is_some());

        // Ein ausgeschalteter Weg darf unvollständig sein, ohne zu stören:
        // so sieht ein Eintrag aus, den jemand vorbereitet und noch nicht
        // benutzt.
        let mut off = Alerts::default();
        off.smtp.host.clear();
        off.telegram.bot_token.clear();
        assert!(super::missing_alert_field(&off).is_none());
    }

    /// Was das Fenster speichert, muss das Programm danach wieder laden können
    /// — und alles daneben muss die Runde überstehen.
    #[test]
    fn saved_alerts_survive_a_round_trip_with_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("sc-alerts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        // Eine Datei, in der auch außerhalb der Meldewege etwas Eigenes steht.
        let mut cfg = crate::config::Config::default();
        cfg.run.threads = 6;
        cfg.design.theme = "mahogany".into();
        cfg.save(&path).expect("schreiben");

        // Wie das Fenster es tut: laden, nur die Meldewege ersetzen, sichern.
        let mut edited = crate::config::Config::load(&path).expect("lesen");
        edited.alerts.desktop.enabled = false;
        edited.alerts.ntfy.enabled = true;
        edited.alerts.ntfy.topic = "ein-thema-das-lang-genug-ist".into();
        edited.save(&path).expect("zurückschreiben");

        let again = crate::config::Config::load(&path).expect("erneut lesen");
        assert!(again.alerts.ntfy.enabled);
        assert_eq!(again.alerts.ntfy.topic, "ein-thema-das-lang-genug-ist");
        assert!(!again.alerts.desktop.enabled);
        // Und das, was diesen Bildschirm nichts angeht, steht unverändert da.
        assert_eq!(again.run.threads, 6);
        assert_eq!(again.design.theme, "mahogany");
        // Die geschriebene Datei muss auch geprüft durchgehen, sonst hätte das
        // Fenster den Start unmöglich gemacht.
        assert!(again.validate().is_ok());

        // In dieser Datei steht künftig ein Mailpasswort und ein Bot-Token,
        // weil das Fenster sie dort hineinschreibt. Also darf sie niemandem
        // sonst auf diesem Rechner gehören.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "die Einstellungsdatei steht auf {mode:o}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Der Abstand, der einen Inhalt in die Mitte rückt, und die zwei Fälle,
    /// in denen es keine Mitte gibt.
    #[test]
    fn content_finds_the_middle_only_when_it_fits() {
        // Vier von zehn Punkten belegt: drei über, drei unter dem Inhalt.
        assert_eq!(super::centring_lead_for(400.0, 1000.0), 300.0);

        // Höher als die Fläche: der Inhalt fängt oben an und wird gerollt.
        assert_eq!(super::centring_lead_for(1200.0, 1000.0), 0.0);

        // Noch nichts gemessen — erstes Bild nach dem Öffnen.
        assert_eq!(super::centring_lead_for(0.0, 1000.0), 0.0);

        // Knapp unter der Höhe der Fläche: hier wird nicht gemittet, sonst
        // stünde der Block genau auf der Kante, ein Rollbalken erschiene, die
        // Spalte würde schmaler, der Text bräuchte eine Zeile mehr — und im
        // nächsten Bild ginge es von vorn los.
        assert_eq!(super::centring_lead_for(998.0, 1000.0), 0.0);

        // Und wo gemittet wird, bleibt Platz für den Inhalt selbst: Abstand
        // plus Inhalt muss unter die Fläche passen, sonst war die Mitte einen
        // Rollbalken wert.
        for content in [10.0_f32, 100.0, 500.0, 900.0, 995.0] {
            let lead = super::centring_lead_for(content, 1000.0);
            assert!(
                lead + content <= 1000.0,
                "{content} Punkte Inhalt plus {lead} Punkte Abstand sprengen die Fläche"
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
        assert_eq!(format::german_scale(5.7e18), "5,7 Trillionen");
        assert_eq!(format::german_scale(1.0e9), "1,0 Milliarde");
        assert!(format::german_scale(1e40).starts_with("10 hoch"));
        assert_eq!(format::german_scale(f64::INFINITY), "unendlich");
    }

    #[test]
    fn formatting_matches_the_tui() {
        assert_eq!(format::sci(0.0), "0");
        assert_eq!(format::sci(f64::INFINITY), "unendlich");
        assert!(format::sci(1.5e-9).contains(','));
        assert_eq!(format::thousands(1_234_567), "1 234 567");
    }

    /// Clicking a suggested word fills it into the field.
    ///
    /// Driven with a real pointer against a real context across three frames,
    /// because the bug this guards against was invisible to any test of the
    /// suggestion *list*: the words were right, the popup appeared, and the
    /// click did nothing. Taking the pointer to the popup takes the keyboard
    /// off the text box, and a list drawn only while the box had focus was
    /// gone by the frame the click arrived in.
    #[test]
    fn a_suggested_word_can_be_clicked_into_the_field() {
        use crate::recover::State;
        let ctx = egui::Context::default();
        let mut slot = crate::recover_ui::Slot {
            word: "av".into(),
            state: State::Sure,
        };

        // Measured against this layout rather than guessed: the row sits at
        // the top of the panel, and the three suggestions follow under it.
        let field = Pos2::new(80.0, 16.0);
        let second_suggestion = Pos2::new(80.0, 60.0);

        let frame = |events: Vec<egui::Event>, slot: &mut crate::recover_ui::Slot| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| word_field(ui, 1, slot));
                });
            });
        };
        let click_at = |p: Pos2| {
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ]
        };

        // Lay out, then click the field so it takes the keyboard and opens
        // the list.
        frame(vec![], &mut slot);
        frame(click_at(field), &mut slot);
        frame(vec![], &mut slot);

        assert_eq!(
            suggestions_for("av"),
            vec!["average", "avocado", "avoid"],
            "the list from the report that prompted this"
        );

        frame(click_at(second_suggestion), &mut slot);
        frame(vec![], &mut slot);

        assert_eq!(
            slot.word, "avocado",
            "clicking the second suggestion must put that word in the field"
        );
    }

    /// Clicking the chest in the header opens the easter egg.
    ///
    /// Driven through a real context and a real pointer, because the whole
    /// feature is one click landing on one rectangle. Two frames: egui decides
    /// what was hit from the previous frame's layout, so the first press would
    /// land on a window that has not been drawn yet.
    #[test]
    fn the_chest_in_the_header_leads_somewhere() {
        let mut app = blank_app(None);
        let ctx = egui::Context::default();

        // The mark sits at the panel's top-left: 16 and 12 of margin, then
        // half of its 46 points.
        let on_the_chest = Pos2::new(16.0 + 23.0, 12.0 + 23.0);

        let frame = |events: Vec<egui::Event>, app: &mut GuiApp| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            ctx.run(input, |ctx| app.draw_dashboard(ctx))
        };

        // Lay the window out once.
        let out = frame(vec![egui::Event::PointerMoved(on_the_chest)], &mut app);
        assert!(
            out.platform_output.open_url.is_none(),
            "hovering must not open anything"
        );

        // Then click it.
        let click = vec![
            egui::Event::PointerMoved(on_the_chest),
            egui::Event::PointerButton {
                pos: on_the_chest,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: on_the_chest,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let out = frame(click.clone(), &mut app);
        assert_eq!(
            out.platform_output.open_url.map(|u| u.url),
            Some(EGG_URL.to_string()),
            "a click on the chest must open the egg"
        );

        // And the same click somewhere with nothing under it must not — so a
        // pass above means the rectangle was hit, not that everything opens
        // the egg.
        let elsewhere = Pos2::new(600.0, 300.0);
        let miss: Vec<egui::Event> = click
            .into_iter()
            .map(|e| match e {
                egui::Event::PointerMoved(_) => egui::Event::PointerMoved(elsewhere),
                egui::Event::PointerButton {
                    button,
                    pressed,
                    modifiers,
                    ..
                } => egui::Event::PointerButton {
                    pos: elsewhere,
                    button,
                    pressed,
                    modifiers,
                },
                other => other,
            })
            .collect();
        let out = frame(miss, &mut app);
        assert!(
            out.platform_output.open_url.is_none(),
            "clicking the dashboard at large must open nothing"
        );
    }

    /// When the recovery form offers word suggestions, and when it keeps quiet.
    #[test]
    fn suggestions_appear_only_where_they_help() {
        // A blank field is a question, not a prefix.
        assert!(suggestions_for("").is_empty());
        assert!(suggestions_for("   ").is_empty());

        // A stub gets the ways it could end.
        assert_eq!(suggestions_for("aban"), vec!["abandon"]);

        // A word typed out in full, with nothing longer behind it, needs no
        // list telling it what it already is.
        assert!(
            suggestions_for("abandon").is_empty(),
            "a finished word must not be offered back to itself"
        );

        // But a finished word that is also the start of longer ones keeps
        // offering them: "act" is a word, and so are four words beginning
        // with it.
        let act = suggestions_for("act");
        assert_eq!(act.first(), Some(&"act"));
        assert!(act.contains(&"actress"), "got {act:?}");

        // The case from the screenshot that started this: two letters, three
        // ways to finish them, all of them clickable.
        assert_eq!(suggestions_for("av"), vec!["average", "avocado", "avoid"]);

        // Nonsense offers nothing rather than everything.
        assert!(suggestions_for("zzzz").is_empty());

        // Never more than fits under a field.
        assert!(suggestions_for("a").len() <= 6);
    }

    /// The card row must thin out rather than squeeze, at the widths this
    /// window actually opens at.
    #[test]
    fn the_card_row_thins_out_before_it_squeezes() {
        // Default window, 1180 wide, less the panel's own margins.
        assert_eq!(widgets::columns_that_fit(1148.0, 3, MIN_STAT_CARD), 3);
        // Smallest allowed window, 900 wide, drawer shut.
        assert_eq!(widgets::columns_that_fit(868.0, 3, MIN_STAT_CARD), 3);
        // The case that was broken: 900 wide with the 360-point drawer open,
        // which used to leave about 160 points per card.
        let cramped = 508.0_f32;
        let n = widgets::columns_that_fit(cramped, 3, MIN_STAT_CARD);
        assert_eq!(
            n, 2,
            "three cards must not be crammed into half a small window"
        );
        assert!(
            cramped / n as f32 >= MIN_STAT_CARD,
            "and whatever it returns must genuinely fit"
        );

        // The wide pair at the bottom stacks in the same situation.
        assert_eq!(widgets::columns_that_fit(1148.0, 2, MIN_WIDE_CARD), 2);
        assert_eq!(widgets::columns_that_fit(508.0, 2, MIN_WIDE_CARD), 1);

        // Never zero columns, whatever it is handed.
        assert_eq!(widgets::columns_that_fit(0.0, 3, MIN_STAT_CARD), 1);
        assert_eq!(widgets::columns_that_fit(-100.0, 3, MIN_STAT_CARD), 1);
        assert_eq!(widgets::columns_that_fit(f32::NAN, 3, MIN_STAT_CARD), 1);
        assert_eq!(widgets::columns_that_fit(1000.0, 3, 0.0), 1);
        // And never more than it was asked for.
        assert_eq!(widgets::columns_that_fit(10_000.0, 3, MIN_STAT_CARD), 3);
    }

    /// The headline of the SUCHRAUM card must be readable without knowing what
    /// an exponent is — that card carries the point of the whole program.
    #[test]
    fn the_keyspace_headline_is_words_not_exponents() {
        // What the card actually shows after a few seconds of searching: the
        // figure that used to stand here in full was "4,0901e-72 %".
        let realistic = 5_000.0 / 2f64.powi(256) * 100.0;
        assert_eq!(format::share_headline(realistic), "praktisch 0 %");
        // And on the smallest keyspace the program offers, too.
        assert_eq!(
            format::share_headline(1e12 / 2f64.powi(128) * 100.0),
            "praktisch 0 %"
        );

        assert_eq!(format::share_headline(0.0), "0 %");
        assert_eq!(format::share_headline(-1.0), "0 %");
        assert_eq!(format::share_headline(f64::NAN), "0 %");

        // Above the threshold a real number comes back, in German notation, so
        // the wording cannot quietly outlive the case it was written for.
        assert_eq!(format::share_headline(12.5), "12,500 %");
        assert_eq!(format::share_headline(100.0), "100,000 %");
        for s in [
            format::share_headline(realistic),
            format::share_headline(12.5),
            format::share_headline(0.0),
        ] {
            assert!(!s.contains('e'), "no exponent may reach the headline: {s}");
        }
    }

    /// The warning fires on exactly the marks the search throws away.
    ///
    /// `Layout::build` keeps runs of two or more adjacent moved words and
    /// drops anything shorter, so a lone mark changes nothing. The pairing
    /// here is against `recover::tests::a_single_moved_word_does_nothing`,
    /// which pins the behaviour this warning describes.
    #[test]
    fn a_move_with_no_neighbour_is_warned_about() {
        // Nothing marked, and a pair that really does trade places.
        assert!(!has_lonely_move(&[false, false, false]));
        assert!(!has_lonely_move(&[false, true, true, false]));
        assert!(!has_lonely_move(&[true, true, true]));
        // Alone in the middle, at either end, and two that never touch.
        assert!(has_lonely_move(&[false, true, false]));
        assert!(has_lonely_move(&[true, false, false]));
        assert!(has_lonely_move(&[false, false, true]));
        assert!(has_lonely_move(&[true, false, true]));
        // A good pair does not excuse a stray elsewhere.
        assert!(has_lonely_move(&[true, true, false, true, false]));
        // One position cannot trade with anybody.
        assert!(has_lonely_move(&[true]));
        assert!(!has_lonely_move(&[]));
    }

    /// The embedded icon must be a sane, non-empty image. Size and shape are
    /// checked where it is decoded; this is about the picture itself.
    #[test]
    fn icon_data_is_well_formed() {
        let icon = crate::icon_data::icon().expect("embedded icon does not decode");
        let total = (icon.width * icon.height) as usize;

        // Opaque enough to be a real icon rather than a blank sheet.
        let opaque = icon.rgba.chunks_exact(4).filter(|c| c[3] > 200).count();
        assert!(
            opaque > total / 2,
            "icon is mostly transparent: {opaque}/{total}"
        );

        // And not a single flat colour.
        let distinct: std::collections::HashSet<[u8; 3]> = icon
            .rgba
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
