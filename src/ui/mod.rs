//! Die Fensteroberfläche.
//!
//! `gui.rs` war einmal 4781 Zeilen und trug vier Aufgaben gleichzeitig: die
//! Zustandsmaschine der Anwendung, das Dashboard, den kompletten
//! Wiederherstellungs-Assistenten und sämtliche Zeichenprimitiven. Hier ist das
//! auseinandergelegt — jede Datei beantwortet eine Frage:
//!
//! * [`theme`] — welche Farbe, welcher Abstand, welche Größe?
//! * [`format`] — wie sieht diese Zahl als Text aus?
//! * [`widgets`] — die wiederkehrenden Bausteine: Karte, Kennzahl, Detailzeile.
//! * [`screen`] — welcher Bildschirm ist gerade dran?
//!
//! Das Dashboard und der Assistent liegen weiterhin in `gui.rs`
//! beziehungsweise `recover_ui.rs`; sie zeichnen, aber sie entscheiden nichts
//! mehr über Farben, Maße oder Abläufe.

pub mod feel;
pub mod format;
pub mod screen;
pub mod theme;
pub mod widgets;
