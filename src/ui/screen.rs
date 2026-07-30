//! Welcher Bildschirm gerade dran ist.
//!
//! Vorher entschied das eine if/else-Kette über fünf voneinander unabhängige
//! Felder — `loading`, `load_error`, ein Zeitstempel, `chooser`, `recover` —
//! plus vier Umgebungsvariablen, mitten im Zeichencode abgefragt. Unmögliche
//! Kombinationen waren darstellbar; einzig die Reihenfolge der Zweige löste sie
//! auf. Und das Intro wurde übersprungen, indem ein Zeitstempel rückdatiert
//! wurde.
//!
//! Jetzt ist es ein Feld. Ein Zustand ist entweder da oder nicht, und ein neuer
//! Bildschirm ist eine neue Variante statt eines weiteren Zweigs in einer
//! Kette, deren Korrektheit an ihrer Reihenfolge hängt.

use std::path::PathBuf;
use std::time::Instant;

use crate::recover_ui::RecoverUi;

/// Der eine Zustand, der bestimmt, was im Fenster steht.
pub enum Screen {
    /// Datenbank und Suchfilter werden gelesen. Läuft auf einem eigenen
    /// Thread, das Fenster steht schon.
    Loading,

    /// Das Laden ist gescheitert.
    ///
    /// `repairable` ist genau dann gesetzt, wenn schlicht keine Datenbank da
    /// war — der eine Fehler, den das Programm selbst beheben kann. Eine
    /// vorhandene, aber beschädigte Datei bekommt das nie: sie zu überschreiben
    /// würde einen echten Adress-Auszug vernichten, den jemand stundenlang
    /// geladen hat.
    Failed {
        message: String,
        repairable: Option<PathBuf>,
    },

    /// Der Vorhang zwischen Laden und Oberfläche. Es wird nichts gewartet —
    /// darum auch kein Fortschrittsbalken und kein Countdown, sondern nur ein
    /// kurzes Aufblenden.
    Intro { until: Instant },

    /// Die Gabelung: Schatzsuche links, Seed retten rechts.
    Chooser,

    /// Die laufende Suche.
    Dashboard,

    /// Der Wiederherstellungs-Assistent.
    ///
    /// `back` ist der Bildschirm, zu dem „Zurück" führt. Nicht fest das
    /// Dashboard: die Wiederherstellung ist auch vom Fehlerbildschirm aus
    /// erreichbar — sie braucht mit Zieladresse gar keine Datenbank —, und
    /// dorthin muss der Rückweg dann auch zeigen.
    Recover {
        ui: Box<RecoverUi>,
        back: Box<Screen>,
    },
}

impl Screen {
    pub fn is_recover(&self) -> bool {
        matches!(self, Screen::Recover { .. })
    }

    pub fn is_dashboard(&self) -> bool {
        matches!(self, Screen::Dashboard)
    }

    /// Ob die Tastenkürzel des Dashboards hier gelten.
    ///
    /// Weder auf der Gabelung — dort gibt es noch keine Suche zum Anhalten —
    /// noch während des Ladens oder des Intros, und im Assistenten gehört die
    /// Tastatur den Wortfeldern.
    pub fn takes_shortcuts(&self) -> bool {
        matches!(self, Screen::Dashboard)
    }

    /// Ein Name fürs Protokoll und für Testmeldungen.
    pub fn name(&self) -> &'static str {
        match self {
            Screen::Loading => "laden",
            Screen::Failed { .. } => "fehler",
            Screen::Intro { .. } => "intro",
            Screen::Chooser => "gabelung",
            Screen::Dashboard => "dashboard",
            Screen::Recover { .. } => "wiederherstellen",
        }
    }
}

/// Der Bildschirm, auf dem ein Screenshot-Lauf öffnen soll.
///
/// Die `SC_SHOT_*`-Schalter standen vorher einzeln mitten in der
/// Bildschirmauswahl und wurden in jedem Bild neu gelesen. Sie gehören nicht in
/// die Ablauflogik, sondern hierher: einmal beim Start gelesen, danach ist es
/// ein gewöhnlicher Zustand wie jeder andere.
///
/// `None` heißt: normaler Lauf, der Ablauf entscheidet selbst.
pub fn screenshot_override() -> Option<Screen> {
    use std::env::var;

    if var("SC_SHOT_INTRO").is_ok() {
        return Some(Screen::Intro {
            // Weit in der Zukunft: der Screenshot-Lauf soll auf diesem Bild
            // stehen bleiben, bis er es aufgenommen hat.
            until: Instant::now() + std::time::Duration::from_secs(3600),
        });
    }
    if var("SC_SHOT_CHOOSER").is_ok() {
        return Some(Screen::Chooser);
    }
    if var("SC_SHOT_LOADING").is_ok() {
        return Some(Screen::Loading);
    }
    if let Ok(v) = var("SC_SHOT_RECOVER") {
        return Some(Screen::Recover {
            ui: Box::new(recover_for_screenshot(&v)),
            back: Box::new(Screen::Dashboard),
        });
    }
    None
}

/// Füllt den Assistenten so, wie die Screenshot-Schalter es verlangen.
fn recover_for_screenshot(words: &str) -> RecoverUi {
    let mut r = RecoverUi::default();

    // `SC_SHOT_RECOVER=<wörter>` füllt das Formular, damit die Marken je Wort
    // fotografierbar sind. Alles andere lässt es leer.
    if words.split_whitespace().count() > 1 {
        let _ = r.paste_all(words);
    }

    // `SC_SHOT_MOVED=1,2` markiert diese Stellen als verrutscht, von eins an
    // gezählt. Das Einfügefeld kennt für diesen Zustand kein Zeichen — er wird
    // durch Klick auf die Marke erreicht —, und die Warnung über eine Marke
    // ohne Tauschpartner muss fotografierbar sein.
    if let Ok(list) = std::env::var("SC_SHOT_MOVED") {
        for n in list
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
        {
            if let Some(slot) = n.checked_sub(1).and_then(|i| r.slots.get_mut(i)) {
                slot.state = crate::recover::State::Moved;
            }
        }
    }

    // `SC_SHOT_ROLLED=1` würfelt eine Übungs-Seed ins Formular, damit der
    // Würfelknopf, die Notiz und der Übungs-Streifen fotografierbar sind.
    // Dieselbe Notiz wie beim echten Klick, sonst zeigt das Bild einen Zustand,
    // den es im Betrieb nicht gibt. Ein Fehlschlag des Zufallsgenerators ist
    // hier belanglos: dann steht das Formular eben leer da.
    if std::env::var("SC_SHOT_ROLLED").is_ok() {
        if let Ok(gap) = r.roll_practice() {
            r.bulk_note = Some(Ok(format!(
                "Übungs-Seed gewürfelt: {} Wörter und die passende Adresse eingesetzt, \
                 Wort {gap} offen gelassen. Geh auf Weiter.",
                r.word_count.words()
            )));
        }
    }

    // `SC_SHOT_DONE=1` stellt einen fertigen Fund her, damit der
    // Ergebnisbildschirm fotografierbar ist, ohne eine echte Suche laufen zu
    // lassen. Die Wörter sind der Nullvektor, die Adresse seine erste.
    if std::env::var("SC_SHOT_DONE").is_ok() {
        let mut m = String::new();
        crate::bip39::entropy_to_mnemonic(&[0u8; 16], crate::bip39::WordCount::W12, &mut m);
        r.phase = crate::recover_ui::Phase::Done(crate::recover::Outcome {
            hits: vec![crate::recover::Found {
                mnemonic: m,
                address: "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu".into(),
                path: "m/84'/0'/0'/0/0".into(),
                balance_sats: None,
            }],
            truncated: false,
        });
        // Ein gewürfelter Satz wäre hier ein Übungslauf und würde den
        // Kontostand-Bereich unterdrücken; ein fotografierter Fund ist keiner.
        r.practice = false;
    }

    // `SC_SHOT_BALANCE=<notlisted|local|online|asking|failed>` setzt den
    // Kontostand-Zustand, damit alle fünf Auskünfte einzeln aufgenommen werden
    // können — ohne Netz und ohne echte Wallet.
    if let Ok(v) = std::env::var("SC_SHOT_BALANCE") {
        use crate::balance::Sum;
        use crate::recover_ui::Balance;
        // So viele Adressen, wie ein echter Lauf bei einer Wallet ansieht, die
        // nur ihre erste Adresse benutzt hat: je Kette einmal das Gap-Limit.
        let checked = crate::balance::GAP_LIMIT as usize * 3;
        r.balance = match v.as_str() {
            "local" => Balance::Local(Sum {
                sats: 133_700_000,
                checked,
            }),
            "online" => Balance::Online(Sum {
                sats: 2_500_000,
                checked,
            }),
            "asking" => Balance::Asking,
            "failed" => Balance::Failed(
                "https://mempool.space/api/address/bc1q… nicht erreichbar: \
                 Netzwerk nicht verfügbar"
                    .into(),
            ),
            _ => Balance::NotListed,
        };
    }

    // `SC_SHOT_STEP=<0-3>` öffnet den Assistenten auf diesem Schritt, damit
    // jeder einzeln aufgenommen werden kann.
    if let Some(i) = std::env::var("SC_SHOT_STEP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        r.step = crate::recover_ui::Step::ALL[i.min(crate::recover_ui::Step::ALL.len() - 1)];
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_dashboard_answers_to_the_shortcuts() {
        assert!(Screen::Dashboard.takes_shortcuts());
        assert!(!Screen::Chooser.takes_shortcuts());
        assert!(!Screen::Loading.takes_shortcuts());
        assert!(!Screen::Intro {
            until: Instant::now()
        }
        .takes_shortcuts());
        assert!(!Screen::Failed {
            message: String::new(),
            repairable: None,
        }
        .takes_shortcuts());
        assert!(!Screen::Recover {
            ui: Box::new(RecoverUi::default()),
            back: Box::new(Screen::Dashboard),
        }
        .takes_shortcuts());
    }

    /// Der Rückweg aus dem Assistenten ist Teil des Zustands, nicht fest
    /// verdrahtet — sonst landet jemand, der vom Fehlerbildschirm kam, auf
    /// einem Dashboard ohne Daten.
    #[test]
    fn the_way_back_out_of_recovery_is_carried_along() {
        let s = Screen::Recover {
            ui: Box::new(RecoverUi::default()),
            back: Box::new(Screen::Failed {
                message: "keine Datenbank".into(),
                repairable: None,
            }),
        };
        match s {
            Screen::Recover { back, .. } => assert_eq!(back.name(), "fehler"),
            _ => panic!("falscher Zustand"),
        }
    }

    #[test]
    fn every_screen_has_a_name() {
        for s in [
            Screen::Loading,
            Screen::Failed {
                message: String::new(),
                repairable: None,
            },
            Screen::Intro {
                until: Instant::now(),
            },
            Screen::Chooser,
            Screen::Dashboard,
            Screen::Recover {
                ui: Box::new(RecoverUi::default()),
                back: Box::new(Screen::Dashboard),
            },
        ] {
            assert!(!s.name().is_empty());
        }
    }
}
