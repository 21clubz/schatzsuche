//! Farben, Abstände, Textgrößen und Radien — an genau einer Stelle.
//!
//! Vorher lagen zwölf Farbkonstanten oben in `gui.rs` und einundzwanzig weitere
//! `Color32::from_rgb`-Aufrufe verstreut im Widget-Code, dazu achtzehn
//! verschiedene Abstandswerte und sechzehn Schriftgrößen. Ein Restyling hätte
//! rund hundertfünfzig Aufrufstellen bedeutet.
//!
//! Jetzt gilt: kein Widget nennt eine Farbe, einen Abstand oder eine Größe
//! selbst. Es fragt hier nach. Eine Änderung am Aussehen des Programms ist
//! damit eine Änderung an dieser Datei und an keiner anderen.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use eframe::egui::{self, Color32, FontId, Rounding, Stroke};

// --- Abstände ---------------------------------------------------------------

/// Das Abstandsraster. Jeder vertikale und horizontale Zwischenraum in der
/// Oberfläche ist einer dieser fünf Werte — nichts dazwischen.
///
/// Krumme Werte sind nicht bloß unordentlich: sie entstehen dadurch, dass
/// jemand einen einzelnen Bildschirm zurechtrückt, und der nächste Bildschirm
/// bekommt dann seine eigenen krummen Werte. Ein Raster macht die Frage „wie
/// viel Platz hier?" von einer Geschmacks- zu einer Auswahlfrage.
pub const S1: f32 = 4.0;
pub const S2: f32 = 8.0;
pub const S3: f32 = 16.0;
pub const S4: f32 = 24.0;
pub const S5: f32 = 32.0;

// --- Schriftgrößen ----------------------------------------------------------

/// Kleinster Grad: Bildunterschriften, Detailzeilen, Kartentitel (dort in
/// Großbuchstaben, was ihnen die Hierarchie gibt), Nebenbemerkungen.
pub const SMALL: f32 = 11.5;

/// Lesegrad: alles, was in ganzen Sätzen dasteht, und jede Beschriftung.
pub const BODY: f32 = 13.0;

/// Überschriftengrad: die Frage oben auf einem Bildschirm, die Wortmarke.
pub const TITLE: f32 = 16.0;

/// Keine vierte Textgröße, sondern die Anzeigegröße für **Zahlen**, die aus
/// einem Meter Abstand lesbar sein müssen: die Hochrechnung, Kennzahlen im
/// Detailbereich. Für Fließtext nie verwenden.
pub const DISPLAY: f32 = 26.0;

/// Die eine Zahl in der Mitte des Hauptfensters.
///
/// Auch das ist keine Textgröße, sondern ein Bild: der Blick fällt zuerst
/// hierhin, und was er findet, soll man von der anderen Seite des Zimmers
/// lesen können. Es gibt genau eine Stelle, die das benutzt — gäbe es zwei,
/// wäre keine mehr die Mitte.
pub const SCENE: f32 = 62.0;

/// Monospace in einem der obigen Grade.
///
/// Zahlen laufen grundsätzlich hierüber: Proportionalziffern haben
/// unterschiedliche Breiten, und eine Zahl, die sich mehrmals pro Sekunde
/// ändert, lässt damit die ganze Zeile zappeln.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

// --- Die Schrift der Wortmarke ----------------------------------------------

/// Ubuntu Bold, mitgeliefert, für den Namen des Programms.
///
/// Das Fenster zeichnet sonst alles in Ubuntu **Light** — der Schrift, die
/// egui mitbringt. Das ist derselbe Schriftschnitt-Verwandte, nur der dünnste:
/// die Wortmarke stand damit blass da, besonders seit die Seekarte hinter ihr
/// liegt. Hier steht dieselbe Familie in einem echten Fettschnitt, und „echt"
/// ist der Punkt — `RichText::strong` färbt in egui nur um, und Buchstaben
/// mehrfach versetzt zu malen war ein Notbehelf.
///
/// Warum keine der üblichen Verdächtigen: Inter, Roboto, Open Sans und Work
/// Sans gibt es nur noch als Variable Fonts, und `ab_glyph` unter egui kann
/// deren Gewichtsachse nicht stellen — sie kämen wieder dünn heraus. Lato Bold
/// gäbe es statisch, wiegt aber doppelt so viel und wäre eine fremde Familie
/// neben dem Rest der Oberfläche.
static WORDMARK_TTF: &[u8] = include_bytes!("../../assets/Ubuntu-Bold.ttf");

/// Der Name der Familie, unter dem egui sie führt.
const WORDMARK_FAMILY: &str = "wordmark";

/// Woran [`wordmark`] erkennt, dass die Schrift in diesem Kontext angemeldet
/// ist.
fn wordmark_key() -> egui::Id {
    egui::Id::new("wordmark_font_installed")
}

/// Meldet die Schrift bei egui an. **Einmal beim Start.**
///
/// Nicht in [`apply`], obwohl dort der übrige Grundstil gesetzt wird: `apply`
/// läuft in jedem Bild, und `set_fonts` baut den Zeichenatlas neu auf. Einmal
/// je Bild wäre das ein spürbarer Preis für ein Ergebnis, das sich nie ändert.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        WORDMARK_FAMILY.to_owned(),
        egui::FontData::from_static(WORDMARK_TTF),
    );
    fonts.families.insert(
        egui::FontFamily::Name(WORDMARK_FAMILY.into()),
        vec![WORDMARK_FAMILY.to_owned()],
    );
    ctx.set_fonts(fonts);
    ctx.data_mut(|d| d.insert_temp(wordmark_key(), true));
}

/// Die Wortmarke in einem der obigen Grade.
///
/// Das Gegenstück zu [`mono`]: keine Aufrufstelle benennt eine Schrift selbst,
/// sie fragt hier nach — dieselbe Zusage wie bei den Farben.
///
/// Braucht den Kontext, weil die Antwort davon abhängt, ob [`install_fonts`]
/// dort schon gelaufen ist. Wenn nicht, kommt die gewöhnliche Proportionale
/// zurück statt eines Absturzes: egui bricht ab, wenn ein Text nach einer
/// Familie verlangt, die keine Schrift trägt — und `set_fonts` wirkt erst im
/// nächsten Bild, ein Nachinstallieren an dieser Stelle käme also zu spät. Ein
/// Fenster, dessen Wortmarke im dünnen Schnitt steht, ist ein Schönheitsfehler;
/// eines, das beim Zeichnen abstürzt, ist keiner mehr. Dieselbe Abwägung wie
/// bei den Bildern in [`crate::icon_data`].
pub fn wordmark(ctx: &egui::Context, size: f32) -> FontId {
    if ctx
        .data(|d| d.get_temp::<bool>(wordmark_key()))
        .unwrap_or(false)
    {
        FontId::new(size, egui::FontFamily::Name(WORDMARK_FAMILY.into()))
    } else {
        FontId::proportional(size)
    }
}

// --- Radien -----------------------------------------------------------------

/// Für schmale Balken und Segmente, wo ein größerer Radius die Form auffräße.
pub const R_XS: f32 = 3.0;
/// Knöpfe, Eingabefelder, Hinweiskästen.
pub const R_SM: f32 = 6.0;
/// Karten, Türen, alles Flächige.
pub const R_MD: f32 = 10.0;

pub fn r_xs() -> Rounding {
    Rounding::same(R_XS)
}
pub fn r_sm() -> Rounding {
    Rounding::same(R_SM)
}
pub fn r_md() -> Rounding {
    Rounding::same(R_MD)
}

/// Die Standard-Umrandung: ein Pixel in der Rahmenfarbe.
pub fn hairline() -> Stroke {
    Stroke::new(1.0_f32, pal().frame)
}

/// Der weiche Schlagschatten unter aufgelegten Flächen — Karten, Schubladen.
///
/// Hier und nicht im Widget-Code, aus demselben Grund wie jede andere Farbe:
/// ein Schatten ist eine Farbe mit Richtung. Er fällt nach unten, weil das
/// Licht im ganzen Fenster von oben kommt (die Glanzkanten der Tasten treffen
/// dieselbe Annahme), und er ist bewusst leise — er soll eine Karte vom Grund
/// abheben, nicht unter ihr kleben wie ein Aufkleber.
pub fn drop_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: egui::Vec2::new(0.0, 3.0),
        blur: 10.0,
        spread: 0.0,
        color: Color32::from_black_alpha(64),
    }
}

/// Die Randabdunklung der Vignette, die über der Holzfaserung liegt.
///
/// Leise mit Absicht: sie soll den Blick zur Mitte lenken wie das Licht über
/// einem Tisch, nicht wie ein Tunnel. In den Ecken addieren sich zwei Ränder
/// zum doppelten Wert — das ist einkalkuliert, die Ecke ist auch am Möbel die
/// dunkelste Stelle.
///
/// Die Zahl sieht kräftig aus und ist es nicht: der Renderer verrechnet
/// Lasuren linear auf einem sRGB-Bildschirm, und Abdunkeln auf fast schwarzem
/// Grund kommt dabei um ein Mehrfaches leiser heraus als die Arithmetik
/// verspricht — durchgemessen mit einer Kalibrierkachel durch `--screenshot`
/// (Schwarz mit 96 von 255 nimmt dem Walnuss-Grund etwa sechs
/// Helligkeitsstufen). Dasselbe, nur umgekehrt, steht bei den Alphas in
/// `make-wood.py`.
pub fn vignette() -> Color32 {
    Color32::from_black_alpha(96)
}

// --- Palette ----------------------------------------------------------------

/// Alle Flächen- und Schriftfarben der Oberfläche.
///
/// Die Textfarben bilden eine Leiter mit Abstand zwischen den Sprossen
/// (`text` : `dim` : `muted` ≈ 10,5 : 6,5 : 4,5 gegen die Fläche, auf der sie
/// stehen), damit die Hierarchie lesbar bleibt und trotzdem jede Sprosse für
/// sich über WCAG AA liegt. Die Tests unten rechnen das nach.
pub struct Palette {
    /// Fensterhintergrund.
    pub bg: Color32,
    /// Karten, Schubladen, erhobene Flächen.
    pub panel: Color32,
    /// Vertiefte Fläche innerhalb eines Panels: Hinweiskästen, Adressfelder.
    pub inset: Color32,
    /// Fläche unter dem Zeiger.
    pub hover: Color32,
    /// Umrandungen und Trennlinien.
    pub frame: Color32,
    /// Tiefer als [`Palette::inset`]: die Bahn eines Schiebereglers und der
    /// Grund einer Texteingabe. Muss sich von der Fläche ringsum abheben, sonst
    /// verschwindet die Bahn.
    pub sunken: Color32,
    /// Der Körper eines Reglers oder Ankreuzkastens.
    pub control: Color32,
    /// Fläche hinter einer eingebetteten Grafik — die Kachel unter den beiden
    /// Türbildern.
    pub art_tile: Color32,
    /// Deren Rand.
    pub art_tile_edge: Color32,

    /// Der Ton, mit dem die Planken-Kachel beim Malen multipliziert wird.
    ///
    /// Die Kachel (`assets/wood-256.png`) ist **deckend** und bringt ihre
    /// Helligkeit selbst mit — Fugen fast schwarz, Brettkörper im Mittelton,
    /// je Brett ein eigener Stich. Vorher war sie eine Lasur, die den Grund
    /// nur auf- oder abdunkelte: auf einem fast schwarzen Grund gibt es aber
    /// nach unten keinen Raum, und übrig blieben allein die hellen Linien —
    /// ein blasses Wellenmuster statt Holz. Deckend gemalt kommt an, was in
    /// der Kachel steht; **dieser Ton** macht daraus Walnuss oder Mahagoni.
    ///
    /// Etwa `panel` mal 1,3: der Brettkörper landet damit knapp **unter**
    /// der Panel-Helligkeit (die Kachel-Mitte liegt bei zwei Dritteln der
    /// Vollaussteuerung). Karten heben sich also weiter vom Holz ab, und
    /// jede Textfarbe, die auf `panel` besteht, besteht auf dem etwas
    /// dunkleren Holz erst recht.
    pub wood: Color32,

    /// Die eine Akzentfarbe: Hauptaktionen, aktive Zustände, Diagramme.
    pub primary: Color32,
    /// Zweitfarbe, ausschließlich für die Wortmarke und den Zustand
    /// „verrutscht" im Wiederherstellungs-Assistenten.
    pub accent: Color32,

    /// Beschriftung auf einer mit `primary`, `warn` oder `gold` gefüllten
    /// Fläche. Diese Füllungen sind hell, also steht Schwarz darauf.
    pub on_fill: Color32,
    /// Die zweite Zeile auf derselben Füllung.
    pub on_fill_dim: Color32,
    /// Erloschene Segmente einer Anzeige auf gefüllter Fläche.
    pub on_fill_faint: Color32,

    /// Fließtext.
    pub text: Color32,
    /// Zweitrangig: Bildunterschriften, Erklärungen unter einer Einstellung.
    pub dim: Color32,
    /// Drittrangig: Nebenbemerkungen, Feldnummern, die leisesten Notizen.
    pub muted: Color32,

    /// Etwas ist schiefgegangen, oder die Rechnung ist aussichtslos.
    pub alert: Color32,
    /// Achtung, aber kein Fehler.
    pub warn: Color32,
    /// Der Schatz: Ladebalken, Münzen.
    pub gold: Color32,
    /// Mittlerer Ton des Farbverlaufs auf den gezeichneten Münzen.
    pub gold_mid: Color32,
    /// Bestätigung, geglückt.
    pub green: Color32,
}

/// Das dunkle Blaugrau, mit dem das Programm angefangen hat.
///
/// Unverändert die Werte, die schon einmal gegen die Panel-Farbe durchgemessen
/// wurden — die vier neuen Felder unten sind die Zahlen, die vorher im
/// Widget-Code und in [`apply`] als Ziffern standen. Bleibt als Rückweg: eine
/// Zeile in der `config.toml` holt sie zurück.
///
/// `static` und nicht `const`, alle drei: die Tests unten prüfen mit
/// `ptr::eq`, dass ein Name seine Palette trifft — und ein `const` hat keine
/// eigene Adresse, sondern verlässt sich darauf, dass der Compiler die an
/// jeder Stelle neu entstehenden Kopien zusammenlegt. Das tat er, bis eine
/// Wertänderung die Zusammenlegung kippte. Ein `static` ist ein Objekt mit
/// einer Adresse; genau das behaupten die Tests.
pub static NIGHT: Palette = Palette {
    bg: Color32::from_rgb(13, 15, 22),
    panel: Color32::from_rgb(24, 28, 41),
    inset: Color32::from_rgb(22, 27, 40),
    hover: Color32::from_rgb(34, 40, 56),
    frame: Color32::from_rgb(37, 43, 61),
    sunken: Color32::from_rgb(11, 13, 19),
    control: Color32::from_rgb(42, 49, 69),
    art_tile: Color32::from_rgb(34, 27, 18),
    art_tile_edge: Color32::from_rgb(70, 55, 32),

    // Ungenutzt, solange NIGHT ohne Holz gemalt wird — aber das Feld muss
    // etwas Sinnvolles tragen, und ein kühles Blaugrau passt zur Welt.
    wood: Color32::from_rgb(31, 36, 52),

    primary: Color32::from_rgb(125, 207, 255),
    accent: Color32::from_rgb(187, 154, 247),

    on_fill: Color32::BLACK,
    on_fill_dim: Color32::from_black_alpha(150),
    on_fill_faint: Color32::from_black_alpha(55),

    text: Color32::from_rgb(192, 202, 245),
    dim: Color32::from_rgb(142, 158, 222),
    muted: Color32::from_rgb(114, 129, 188),

    alert: Color32::from_rgb(247, 118, 142),
    warn: Color32::from_rgb(224, 175, 104),
    gold: Color32::from_rgb(232, 176, 84),
    gold_mid: Color32::from_rgb(226, 170, 78),
    green: Color32::from_rgb(129, 200, 149),
};

/// Dunkles Nussholz, Pergamentschrift, Messinggold — und eine **kühle**
/// Leitfarbe.
///
/// Das Programm heißt Schatzsuche und sein Symbol ist eine Truhe; das Fenster sah
/// aus wie ein Netzwerkmonitor. Hier wird es eine Holzkiste.
///
/// Der Trick, damit es dabei nicht seine Ordnung verliert: `primary` bleibt kühl.
/// Ein verwaschenes Seeblau, wie Grünspan auf Kupferbeschlägen. Kühl heißt weiter
/// „das Programm arbeitet", Gold heißt weiter „hier liegt Geld" — würde beides
/// Messing, sagten die Farben nichts mehr.
/// Die Flächen sind bewusst weiter gespreizt, als sie einmal waren: `bg` und
/// `sunken` tiefer, `panel` einen Hauch heller — eine Karte liegt dadurch
/// sichtbar **auf** dem Grund statt neben ihm. Und `gold`/`gold_mid` stehen
/// absichtlich weit auseinander: sie sind die zwei Stützpunkte des
/// Metallverlaufs (Glanzlicht und Bronzegrund), nicht zwei Varianten derselben
/// Farbe. Nah beieinander ergaben sie Beige; gespreizt ergeben sie Messing.
pub static WALNUT: Palette = Palette {
    bg: Color32::from_rgb(15, 11, 7),
    panel: Color32::from_rgb(38, 29, 20),
    inset: Color32::from_rgb(28, 21, 15),
    hover: Color32::from_rgb(55, 43, 29),
    frame: Color32::from_rgb(92, 70, 44),
    sunken: Color32::from_rgb(9, 6, 4),
    control: Color32::from_rgb(66, 51, 34),
    art_tile: Color32::from_rgb(48, 33, 18),
    art_tile_edge: Color32::from_rgb(122, 91, 48),

    wood: Color32::from_rgb(49, 37, 26),

    primary: Color32::from_rgb(126, 200, 222),
    accent: Color32::from_rgb(194, 149, 212),

    on_fill: Color32::BLACK,
    on_fill_dim: Color32::from_black_alpha(150),
    on_fill_faint: Color32::from_black_alpha(55),

    text: Color32::from_rgb(238, 226, 201),
    dim: Color32::from_rgb(197, 178, 143),
    muted: Color32::from_rgb(160, 141, 111),

    alert: Color32::from_rgb(242, 134, 127),
    warn: Color32::from_rgb(228, 168, 90),
    gold: Color32::from_rgb(248, 196, 84),
    gold_mid: Color32::from_rgb(214, 158, 58),
    green: Color32::from_rgb(147, 201, 138),
};

/// Rötliches Mahagoni, Messing als Leitfarbe, Grünspan als einziger kühler
/// Akzent.
///
/// Voll auf Piraten gedreht, und die Rechnung dafür steht in den Farben: mit
/// Messing als `primary` liegen Leitfarbe, `gold` und `warn` alle im
/// Bernsteinbereich. Gegengehalten wird an zwei Stellen — `warn` ist ins
/// Rostorange gezogen und `accent` auf Grünspan gesetzt, damit überhaupt etwas
/// Kühles übrig bleibt. Es bleibt die Palette, die weniger Bedeutung trägt als
/// [`WALNUT`]; sie steht hier, weil Charakter auch ein Argument ist.
/// Auch hier gilt die Spreizung aus [`WALNUT`]: tieferer Grund, hellere
/// Tafeln, `gold`/`gold_mid` als Verlaufs-Stützpunkte statt Zwillinge.
pub static MAHOGANY: Palette = Palette {
    bg: Color32::from_rgb(17, 9, 5),
    panel: Color32::from_rgb(48, 29, 17),
    inset: Color32::from_rgb(37, 22, 13),
    hover: Color32::from_rgb(66, 42, 24),
    frame: Color32::from_rgb(110, 74, 41),
    sunken: Color32::from_rgb(11, 6, 3),
    control: Color32::from_rgb(82, 53, 31),
    art_tile: Color32::from_rgb(58, 35, 18),
    art_tile_edge: Color32::from_rgb(136, 96, 50),

    wood: Color32::from_rgb(61, 37, 22),

    primary: Color32::from_rgb(230, 170, 72),
    accent: Color32::from_rgb(127, 198, 174),

    on_fill: Color32::BLACK,
    on_fill_dim: Color32::from_black_alpha(150),
    on_fill_faint: Color32::from_black_alpha(55),

    text: Color32::from_rgb(242, 224, 190),
    dim: Color32::from_rgb(205, 177, 132),
    muted: Color32::from_rgb(167, 142, 102),

    alert: Color32::from_rgb(234, 116, 106),
    warn: Color32::from_rgb(230, 136, 66),
    gold: Color32::from_rgb(252, 216, 118),
    gold_mid: Color32::from_rgb(232, 184, 84),
    green: Color32::from_rgb(170, 196, 112),
};

/// Jede Palette mit ihrem Namen aus der `config.toml`.
///
/// Die Kontrasttests unten laufen darüber, damit ein Fehlschlag sagt, **welche**
/// Farbwelt er meint.
pub static ALL: [(&str, &Palette); 3] = [
    ("night", &NIGHT),
    ("walnut", &WALNUT),
    ("mahogany", &MAHOGANY),
];

/// Welche Farbwelt gilt.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Theme {
    Night,
    Walnut,
    Mahogany,
}

impl Theme {
    /// Aus dem Namen in der `config.toml`, Groß- und Kleinschreibung gleich.
    ///
    /// Ein unbekannter Name **bricht nicht ab**, sondern fällt auf
    /// [`Theme::Walnut`] zurück: eine falsch getippte Farbwelt darf niemandem das
    /// Programm verschließen. Ein Tippfehler kostet die falsche Farbe, nicht den
    /// Start.
    pub fn from_name(name: &str) -> Theme {
        match name.trim().to_ascii_lowercase().as_str() {
            "night" => Theme::Night,
            "mahogany" => Theme::Mahogany,
            _ => Theme::Walnut,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Night => "night",
            Theme::Walnut => "walnut",
            Theme::Mahogany => "mahogany",
        }
    }

    fn palette(self) -> &'static Palette {
        match self {
            Theme::Night => &NIGHT,
            Theme::Walnut => &WALNUT,
            Theme::Mahogany => &MAHOGANY,
        }
    }
}

/// Die gewählte Farbwelt, als Zahl.
///
/// Ein Atomic und keine `OnceLock`: [`pal`] wird einige hundert Mal je Bild
/// gerufen, und ein `Relaxed`-Ladevorgang ist billiger als eine
/// Initialisierungsprüfung. Dass es damit auch im Betrieb umschaltbar wäre, ist
/// ein Nebeneffekt — gesetzt wird einmal beim Start aus der `config.toml`.
static ACTIVE: AtomicU8 = AtomicU8::new(Theme::Walnut as u8);

/// Legt die Farbwelt fest. Einmal beim Start, vor dem ersten Bild.
pub fn set_theme(t: Theme) {
    ACTIVE.store(t as u8, Ordering::Relaxed);
}

pub fn theme() -> Theme {
    match ACTIVE.load(Ordering::Relaxed) {
        0 => Theme::Night,
        2 => Theme::Mahogany,
        _ => Theme::Walnut,
    }
}

/// Die geltende Palette.
///
/// Hier stand einmal eine zweite für einen hellen Modus, der der
/// Systemeinstellung folgte. Der ist raus und kommt nicht zurück: das Programm
/// ist eine Schatztruhe im Dunkeln, und auf Weiß sah das aus wie ein
/// Steuerformular. Die drei Farbwelten, die es jetzt gibt, sind alle dunkel —
/// sie streiten über Holz gegen Blaugrau, nicht über hell gegen dunkel.
pub fn pal() -> &'static Palette {
    theme().palette()
}

/// Ob die Holzfaserung hinter den Flächen gemalt wird.
static GRAIN: AtomicBool = AtomicBool::new(true);

/// Schaltet die Holzfaserung.
///
/// Ein eigener Schalter neben der Farbwelt, weil Struktur hinter Monospace-Ziffern
/// ein Lesbarkeitsrisiko ist, das kein Test abfängt — nur Hinsehen. Wem sie beim
/// Lesen in die Quere kommt, dem soll es eine Zeile kosten und nicht einen
/// Neubau.
pub fn set_grain(on: bool) {
    GRAIN.store(on, Ordering::Relaxed);
}

pub fn grain() -> bool {
    GRAIN.load(Ordering::Relaxed)
}

/// Ob überhaupt Material gezeichnet wird — Holzfaserung, Vignette, die Karte
/// hinter der Gabelung.
///
/// Dieselbe Bedingung stand vorher an zwei Stellen ausgeschrieben, und die
/// dritte Stelle hätte sie zum dritten Mal abgeschrieben. Sie sagt: der Nutzer
/// hat flache Flächen nicht abbestellt, und die Farbwelt ist eine hölzerne —
/// hinter dem alten Blaugrau wäre eine Maserung nur ein Fleck.
pub fn textured() -> bool {
    grain() && theme() != Theme::Night
}

/// Alle Farbwelten sind dunkel. Die Funktion bleibt, weil ein paar Widgets ihre
/// Lasuren danach abstufen — und weil ein `if` an einer Stelle billiger zu lesen
/// ist als ein Sonderfall an fünf.
pub const fn is_dark() -> bool {
    true
}

/// Setzt den egui-Grundstil für dieses Bild.
pub fn apply(ctx: &egui::Context) {
    let p = pal();

    // Vom dunklen Grundstil ausgehen statt einzelne Farben zu flicken: die
    // Fensterdekoration — Titelleisten, Rollbalken, Widget-Hintergründe — wird
    // aus der Widget-Palette gezeichnet, und nur `window_fill` zu überschreiben
    // hinterließ der Einstellungen-Schublade eine helle Titelleiste.
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = p.bg;
    style.visuals.window_fill = p.panel;
    style.visuals.window_stroke = Stroke::new(1.0_f32, p.frame);
    // `extreme_bg_color` zeichnet die Schieberegler-Schiene; sie muss sich von
    // der Fläche ringsum unterscheiden, sonst verschwindet die Bahn.
    style.visuals.extreme_bg_color = p.sunken;
    style.visuals.faint_bg_color = p.inset;
    style.visuals.override_text_color = Some(p.text);

    // Textauswahl und Rollbalken standen bisher unbehandelt da und kamen damit
    // in egui-Blau aus `Visuals::dark()`. In einem Blaugrau fiel das nicht auf,
    // in einem Holzfenster schon.
    style.visuals.selection.bg_fill = wash(p.primary);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, p.primary);

    for w in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        // `bg_fill` ist die Reglerschiene und der Ankreuzkasten; sie muss sich
        // von der umgebenden Fläche abheben. `weak_bg_fill` ist die
        // Knopffläche, die die Knöpfe ohnehin selbst setzen.
        w.bg_fill = p.control;
        w.weak_bg_fill = p.panel;
        w.bg_stroke = Stroke::new(1.0_f32, p.frame);
    }
    style.visuals.widgets.hovered.weak_bg_fill = p.hover;

    ctx.set_style(style);
}

/// Eine Signalfarbe als lasierte Fläche: dieselbe Farbe, so weit heruntergesetzt,
/// dass Text darauf stehen kann.
///
/// Ersetzt die von Hand gemischten Sondertöne, die vorher im Widget-Code lagen
/// — ein dunkles Braun für den Warnkasten, ein dunkles Grün für den
/// Erfolgsfall. Die hingen an keiner Palette: wer `warn` änderte, bekam einen
/// Kasten, der nicht mehr dazu passte. Abgeleitet statt gemischt kann das nicht
/// mehr auseinanderlaufen.
pub fn wash(colour: Color32) -> Color32 {
    tinted(colour, 30)
}

/// Eine Signalfarbe, mit `alpha` über den Fensterhintergrund gelegt und **deckend**
/// zurückgegeben.
///
/// Deckend und nicht durchscheinend, und das ist der Punkt: solange dahinter eine
/// glatte Fläche lag, war beides dasselbe Bild. Seit die Holzfaserung dort liegt,
/// ist es nicht mehr dasselbe — eine Notiz mit 22 von 255 Deckung wurde vor Holz
/// zum Fliegengitter, und der Text darin schwer zu lesen. Ausgerechnet statt
/// durchgelassen sieht auf glatter Fläche genauso aus wie vorher und verdeckt die
/// Faserung dort, wo etwas zu lesen ist.
///
/// Gemischt wird gegen [`Palette::bg`] und nicht gegen die Fläche darunter: der
/// Unterschied zwischen `bg` und `panel` ist bei diesen Deckungen unter einem
/// Farbwert, und ein Helfer, der seinen Untergrund kennen müsste, müsste ihn sich
/// an zwei Dutzend Aufrufstellen sagen lassen.
pub fn tinted(colour: Color32, alpha: u8) -> Color32 {
    let base = pal().bg;
    let mix = |a: u8, b: u8| {
        let t = alpha as u32;
        ((a as u32 * t + b as u32 * (255 - t)) / 255) as u8
    };
    Color32::from_rgb(
        mix(colour.r(), base.r()),
        mix(colour.g(), base.g()),
        mix(colour.b(), base.b()),
    )
}

/// Die Hintergrundfarbe, die eframe vor dem ersten Zeichnen ins Fenster füllt.
pub fn clear_color() -> [f32; 4] {
    let c = pal().bg;
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative Leuchtdichte nach WCAG 2.1.
    fn luminance(c: Color32) -> f64 {
        let f = |v: u8| {
            let s = v as f64 / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Jede Textfarbe muss auf beiden Flächen, auf denen sie vorkommt, über
    /// WCAG AA liegen. Genau das war einmal nicht so: die dritte Sprosse kam
    /// gegen das Panel auf 1,7 : 1 — und trug ganze Sätze, keine Verzierung.
    #[test]
    fn every_text_colour_clears_wcag_aa() {
        for (name, p) in ALL {
            for surface in [("bg", p.bg), ("panel", p.panel), ("inset", p.inset)] {
                for fg in [("text", p.text), ("dim", p.dim), ("muted", p.muted)] {
                    let c = contrast(fg.1, surface.1);
                    assert!(c >= 4.5, "{name}: {} auf {} nur {c:.2}:1", fg.0, surface.0);
                }
            }
        }
    }

    /// Die Signalfarben tragen ebenfalls Text, nicht bloß Punkte.
    #[test]
    fn signal_colours_are_readable_on_their_surfaces() {
        for (name, p) in ALL {
            for surface in [("bg", p.bg), ("panel", p.panel)] {
                for fg in [
                    ("primary", p.primary),
                    ("alert", p.alert),
                    ("warn", p.warn),
                    ("green", p.green),
                    ("accent", p.accent),
                ] {
                    let c = contrast(fg.1, surface.1);
                    assert!(c >= 4.5, "{name}: {} auf {} nur {c:.2}:1", fg.0, surface.0);
                }
            }
        }
    }

    /// Und die Beschriftung auf einer gefüllten Schaltfläche.
    #[test]
    fn labels_on_filled_controls_are_readable() {
        for (name, p) in ALL {
            for fill in [("primary", p.primary), ("warn", p.warn), ("gold", p.gold)] {
                let c = contrast(p.on_fill, fill.1);
                assert!(c >= 4.5, "{name}: Beschriftung auf {} nur {c:.2}:1", fill.0);
            }
        }
    }

    /// Die Leiter muss eine Leiter bleiben: gleich helle Sprossen wären zwar
    /// lesbar, würden aber keine Hierarchie mehr abbilden.
    #[test]
    fn the_text_ladder_has_daylight_between_its_rungs() {
        for (name, p) in ALL {
            let (t, d, m) = (
                contrast(p.text, p.panel),
                contrast(p.dim, p.panel),
                contrast(p.muted, p.panel),
            );
            assert!(t > d && d > m, "{name}: Leiter nicht monoton: {t} {d} {m}");
            assert!(
                t / m >= 1.8,
                "{name}: Abstand zwischen erster und dritter Sprosse zu klein"
            );
        }
    }

    /// Die mitgelieferte Schrift muss eine sein.
    ///
    /// Eine leere oder falsch abgelegte Datei fällt sonst erst im Fenster auf,
    /// und zwar als Wortmarke, die gar nicht erscheint — `set_fonts` beschwert
    /// sich nicht, es zeichnet dann nichts.
    #[test]
    fn the_wordmark_font_travels_with_the_program() {
        assert!(
            WORDMARK_TTF.len() > 50_000,
            "die Schriftdatei ist nur {} Bytes groß",
            WORDMARK_TTF.len()
        );
        // TrueType beginnt mit 0x00010000, OpenType mit „OTTO".
        assert!(
            WORDMARK_TTF.starts_with(&[0x00, 0x01, 0x00, 0x00])
                || WORDMARK_TTF.starts_with(b"OTTO"),
            "das ist keine Schriftdatei: {:02x?}",
            &WORDMARK_TTF[..4]
        );
    }

    #[test]
    fn the_spacing_scale_is_the_one_that_was_agreed() {
        assert_eq!([S1, S2, S3, S4, S5], [4.0, 8.0, 16.0, 24.0, 32.0]);
    }

    /// **Der Wächter über die Zusage im Kopf dieser Datei.**
    ///
    /// „Kein Widget nennt eine Farbe selbst" stand dort schon, war aber nicht
    /// wahr: vier Farben lagen als Zahlen im Widget-Code, und eine davon war die
    /// Leitfarbe, die beim Umfärben zurückgeblieben wäre. Sie sind weg — und
    /// dieser Test hält sie draußen.
    ///
    /// Geprüft wird auf `from_rgb(` **mit einer Ziffer dahinter**. Genau das
    /// unterscheidet eine hingeschriebene Farbe von den erlaubten Fällen: eine
    /// Lasur aus einer Palettenfarbe (`from_rgba_unmultiplied(c.r(), …)`) und die
    /// Umrechnung von Bildpunkten (`from_rgba_unmultiplied(c[0], …)`).
    #[test]
    fn no_palette_is_bypassed() {
        for (file, src) in [
            ("gui.rs", include_str!("../gui.rs")),
            ("widgets.rs", include_str!("widgets.rs")),
            ("screen.rs", include_str!("screen.rs")),
        ] {
            for (n, line) in src.lines().enumerate() {
                for start in ["Color32::from_rgb(", "Color32::from_rgba_unmultiplied("] {
                    let Some(at) = line.find(start) else { continue };
                    let rest = &line[at + start.len()..];
                    assert!(
                        !rest.starts_with(|c: char| c.is_ascii_digit()),
                        "{file}:{}: hingeschriebene Farbe — gehört in die Palette:\n  {}",
                        n + 1,
                        line.trim()
                    );
                }
            }
        }
    }

    /// Der Name aus der `config.toml` muss seine Palette treffen — und ein
    /// Tippfehler darf niemandem das Programm verschließen.
    #[test]
    fn every_theme_name_round_trips() {
        for (name, palette) in ALL {
            let t = Theme::from_name(name);
            assert_eq!(t.name(), name);
            assert!(
                std::ptr::eq(t.palette(), palette),
                "{name} trifft die falsche Palette"
            );
        }

        // Groß- und Kleinschreibung und Leerraum sind gleichgültig.
        assert_eq!(Theme::from_name("  WALNUT "), Theme::Walnut);
        assert_eq!(Theme::from_name("Night"), Theme::Night);

        // Unsinn fällt zurück statt abzubrechen.
        assert_eq!(Theme::from_name("kirschholz"), Theme::Walnut);
        assert_eq!(Theme::from_name(""), Theme::Walnut);
    }

    /// Der Umschalter greift, und `pal()` folgt ihm.
    ///
    /// Läuft auf demselben globalen Zustand wie alles andere, also am Ende wieder
    /// auf die Voreinstellung zurücksetzen — sonst hängt das Ergebnis davon ab,
    /// in welcher Reihenfolge die Tests laufen.
    #[test]
    fn setting_the_theme_changes_the_palette() {
        let before = theme();
        for (name, palette) in ALL {
            set_theme(Theme::from_name(name));
            assert!(std::ptr::eq(pal(), palette), "{name} greift nicht");
        }
        set_theme(before);
    }
}
