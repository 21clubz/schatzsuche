//! Anfassgefühl: was passiert, wenn man etwas berührt, drückt und loslässt.
//!
//! Eine Oberfläche fühlt sich nicht deshalb gut an, weil sie hübsch ist,
//! sondern weil sie antwortet. Ein Knopf, der beim Drücken einsinkt, sagt
//! „angekommen", bevor irgendetwas passiert ist; eine Zahl, die hochläuft
//! statt zu springen, macht aus einem Zählerstand eine Bewegung.
//!
//! Hier liegt beides — plus das Klopfen im Trackpad, das auf einem Mac mit
//! Force-Touch dazukommt.

use eframe::egui::{self, Color32, Pos2, Rect, Response, Rounding, Sense, Stroke, Ui, Vec2};

use crate::ui::theme::{self, pal};

// --- Haptik -----------------------------------------------------------------

/// Wie kräftig es sich anfühlen soll.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Bump {
    /// Ein Knopf wurde gedrückt.
    Tap,
    /// Etwas hat umgeschaltet — Modus gewechselt, Bildschirm gewechselt.
    Switch,
}

/// Lässt das Trackpad klopfen.
///
/// Nur auf einem Mac mit Force-Touch-Trackpad spürbar; überall sonst — Maus,
/// ältere Geräte, andere Systeme — passiert schlicht nichts. Das ist der
/// Grund, warum jede Stelle, die das hier ruft, auch sichtbar antworten muss:
/// Haptik ist die Zugabe, nie die Rückmeldung selbst.
pub fn bump(kind: Bump) {
    #[cfg(target_os = "macos")]
    mac::perform(match kind {
        // NSHapticFeedbackPatternGeneric bzw. …LevelChange.
        Bump::Tap => 0,
        Bump::Switch => 2,
    });
    #[cfg(not(target_os = "macos"))]
    let _ = kind;
}

/// Der eine Ton, den das Programm kennt: der Warnton des Systems.
///
/// Die Terminal-Glocke aus `engine::report` verpufft im App-Bundle, weil an
/// stdout kein Terminal hängt. Das hier ist ihr Ersatz für das Fenster — und
/// damit der Unterschied zwischen „ich hätte es nachts gemerkt" und
/// „vielleicht".
///
/// `NSBeep()` ist eine gewöhnliche C-Funktion in AppKit, und AppKit ist für die
/// Haptik oben ohnehin schon verlinkt: der Ton kostet keine Zeile in
/// `Cargo.toml` und keinen Eintrag im Abhängigkeitsbaum eines Programms, das
/// Wallet-Schlüssel erzeugt. Er spielt den Warnton, den der Nutzer selbst
/// eingestellt hat, und respektiert dessen Lautstärke.
///
/// Außerhalb macOS passiert nichts — wie bei [`bump`] gilt: das ist die Zugabe,
/// nie die Rückmeldung selbst.
pub fn alarm() {
    #[cfg(target_os = "macos")]
    // SAFETY: `NSBeep` nimmt keine Argumente, gibt nichts zurück und hat keine
    // Vorbedingungen; das Symbol wird von AppKit exportiert.
    unsafe {
        mac::NSBeep()
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::os::raw::c_char;

    // AppKit hält NSHapticFeedbackManager und NSBeep; libobjc kommt darüber
    // mit herein.
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
        /// Spielt den eingestellten Warnton. Keine Objective-C-Nachricht,
        /// sondern eine schlichte C-Funktion — kein Klassen-Lookup nötig.
        pub fn NSBeep();
    }

    /// `[[NSHapticFeedbackManager defaultPerformer] performFeedbackPattern:p
    /// performanceTime:NSHapticFeedbackPerformanceTimeNow]`
    ///
    /// Von Hand über die Objective-C-Laufzeit statt über eine Crate: es sind
    /// zwei Nachrichten, und dafür lohnt keine neue Abhängigkeit im
    /// Abhängigkeitsbaum eines Programms, das Wallet-Schlüssel erzeugt.
    ///
    /// Darf nur vom Hauptthread laufen. Der Aufruf kommt aus dem Zeichencode,
    /// und der ist auf macOS der Hauptthread.
    pub fn perform(pattern: i64) {
        // SAFETY: Beide Selektoren existieren seit macOS 10.11. `objc_msgSend`
        // wird auf die exakte Signatur des jeweiligen Aufrufs gecastet, was der
        // vorgeschriebene Weg ist — die Funktion hat keine eigene aufrufbare
        // Signatur. Ein fehlender Performer (Gerät ohne Force Touch) kommt als
        // Nullzeiger zurück und wird abgefangen.
        unsafe {
            let cls = objc_getClass(c"NSHapticFeedbackManager".as_ptr());
            if cls.is_null() {
                return;
            }
            let send_obj: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let performer = send_obj(cls, sel_registerName(c"defaultPerformer".as_ptr()));
            if performer.is_null() {
                return;
            }
            let send_perform: extern "C" fn(*mut c_void, *mut c_void, i64, u64) =
                std::mem::transmute(objc_msgSend as *const ());
            send_perform(
                performer,
                sel_registerName(c"performFeedbackPattern:performanceTime:".as_ptr()),
                pattern,
                // NSHapticFeedbackPerformanceTimeNow
                1,
            );
        }
    }
}

// --- Zahlen, die laufen statt springen --------------------------------------

/// Nähert einen Wert weich an sein Ziel an.
///
/// Der Zwischenstand liegt in egui's Bildspeicher, damit die Aufrufstelle
/// zustandslos bleibt — sie schreibt `smooth(ui, "tempo", rate)` und bekommt
/// eine Zahl, die sich bewegt.
///
/// Ein Zähler, der viermal pro Sekunde von 1 480 auf 1 517 springt, liest sich
/// als Flackern. Derselbe Zähler, der in einer Viertelsekunde hinüberläuft,
/// liest sich als Tempo.
pub fn smooth(ui: &Ui, id: &str, target: f64) -> f64 {
    let key = egui::Id::new(("smooth", id));
    // Bei einem Sprung über mehrere Bilder hinweg nicht überschießen.
    let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.1) as f64;
    let current: f64 = ui.memory(|m| m.data.get_temp(key)).unwrap_or(target);

    // Exponentielle Annäherung: unabhängig von der Bildrate, weil die
    // verstrichene Zeit im Exponenten steht und nicht als fester Anteil.
    let next = current + (target - current) * (1.0 - (-dt * 7.0).exp());
    // Das letzte Stück ausrunden, sonst kriecht die Zahl ewig hinterher.
    let next = if (target - next).abs() < 0.5 {
        target
    } else {
        next
    };

    ui.memory_mut(|m| m.data.insert_temp(key, next));
    if (next - target).abs() > 0.01 {
        ui.ctx().request_repaint();
    }
    next
}

// --- Gedrückte Flächen ------------------------------------------------------

/// Wie weit eine Fläche gerade eingedrückt ist, in Punkten.
///
/// Über egui's Animation geführt statt hart geschaltet, damit das Zurückfedern
/// beim Loslassen zu sehen ist — genau das macht den Unterschied zwischen
/// „reagiert" und „fühlt sich an".
pub fn sink(ui: &Ui, resp: &Response) -> f32 {
    let held = resp.is_pointer_button_down_on();
    ui.ctx()
        .animate_bool_with_time(resp.id.with("sink"), held, 0.06)
        * PRESS_DEPTH
}

/// Wie weit eine Fläche unter dem Zeiger angehoben ist.
pub fn lift(ui: &Ui, resp: &Response) -> f32 {
    ui.ctx()
        .animate_bool_with_time(resp.id.with("lift"), resp.hovered(), 0.09)
}

/// Tiefe des Tastenhubs. Die Kante darunter ist genauso hoch, damit eine
/// gedrückte Taste bündig auf ihrem Sockel sitzt.
pub const PRESS_DEPTH: f32 = 3.0;

/// Ein großer, greifbarer Knopf: eine Kappe auf einem Sockel.
///
/// Der Sockel ist eine dunklere Fläche, die unten hervorschaut. Beim Drücken
/// sinkt die Kappe darauf herunter, die Kante verschwindet, und das Trackpad
/// klopft. Das ist das Geheimnis eines Knopfes, der sich nach etwas anfühlt —
/// die Kante, die verschwindet, ist die Rückmeldung. Schatten und Glanz
/// darüber sind Material, keine Rückmeldung: sie sagen „Metall auf Holz",
/// bevor jemand drückt.
///
/// Gibt zurück, ob geklickt wurde.
pub fn key_button(ui: &mut Ui, size: Vec2, fill: Color32, label: &str, sub: Option<&str>) -> bool {
    let (outer, resp) =
        ui.allocate_exact_size(Vec2::new(size.x, size.y + PRESS_DEPTH), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let sunk = sink(ui, &resp);
    let raised = lift(ui, &resp);
    let p = ui.painter();

    // Der Sockel: dieselbe Farbe, deutlich abgedunkelt — und ein weicher
    // Schatten darunter, der beim Anheben wächst und beim Drücken schrumpft:
    // die Taste liegt auf dem Brett, sie klebt nicht darauf.
    let base = Rect::from_min_size(outer.min, Vec2::new(size.x, size.y + PRESS_DEPTH));
    shadow_under(
        p,
        base,
        theme::r_md(),
        0.45 + 0.4 * raised - 0.3 * (sunk / PRESS_DEPTH),
    );
    p.rect_filled(base, theme::r_md(), darken(fill, 0.45));

    // Die Kappe, um den gedrückten Betrag nach unten versetzt.
    let cap = Rect::from_min_size(
        Pos2::new(outer.min.x, outer.min.y + sunk),
        Vec2::new(size.x, size.y),
    );
    let face = if raised > 0.0 {
        lighten(fill, 0.10 * raised)
    } else {
        fill
    };
    p.rect_filled(cap, theme::r_md(), face);
    sheen(p, cap, face);

    // Ein heller Saum an der Oberkante — das Licht, das auf der Kappe liegt.
    let gloss = Rect::from_min_size(
        cap.min + Vec2::new(theme::S2, 1.5),
        Vec2::new(size.x - theme::S3, 1.0),
    );
    p.rect_filled(gloss, Rounding::ZERO, lighten(fill, 0.35));

    let text_col = pal().on_fill;
    match sub {
        None => {
            p.text(
                cap.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(theme::TITLE),
                text_col,
            );
        }
        Some(sub) => {
            p.text(
                Pos2::new(cap.center().x, cap.center().y - 9.0),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(theme::TITLE),
                text_col,
            );
            p.text(
                Pos2::new(cap.center().x, cap.center().y + 11.0),
                egui::Align2::CENTER_CENTER,
                sub,
                egui::FontId::proportional(theme::SMALL),
                pal().on_fill_dim,
            );
        }
    }

    if resp.clicked() {
        bump(Bump::Tap);
    }
    resp.clicked()
}

/// Eine anklickbare Fläche, die sich unter dem Zeiger hebt und beim Drücken
/// einsinkt — für Karten und Kacheln, die keine Knöpfe sind.
///
/// Malt nichts selbst; gibt den verschobenen Bereich zurück, in den der
/// Aufrufer zeichnet.
pub fn tactile(ui: &Ui, resp: &Response, rect: Rect) -> Rect {
    let sunk = sink(ui, resp);
    let raised = lift(ui, resp);
    rect.translate(Vec2::new(0.0, sunk - raised * 2.0))
}

/// Ein Rahmen, der beim Überfahren aufleuchtet.
pub fn glow_stroke(ui: &Ui, resp: &Response, colour: Color32) -> Stroke {
    let t = lift(ui, resp);
    Stroke::new(1.0 + t * 1.2, mix(pal().frame, colour, t))
}

// --- Material: Schatten, Glanz, Glühen ---------------------------------------
//
// Drei Handgriffe, die aus einer flachen Fläche Material machen. Alle drei
// arbeiten nur mit der Farbe, die man ihnen gibt — sie greifen nicht selbst in
// die Palette, damit dieselbe Funktion einer Goldfläche wie einer Holztafel
// dient.

/// Ein weicher Schlagschatten unter einer Fläche. **Vor** der Fläche malen,
/// damit er unter ihr liegt.
///
/// `strength` von 0 bis 1: wie weit die Fläche über dem Grund schwebt. Der
/// Schatten fällt nach unten statt in alle Richtungen — Licht von oben ist
/// die eine Lichtannahme, die sich durch das ganze Fenster zieht (die
/// Glanzkante der Tasten trifft dieselbe).
pub fn shadow_under(painter: &egui::Painter, rect: Rect, rounding: Rounding, strength: f32) {
    let s = strength.clamp(0.0, 1.0);
    if s <= 0.0 {
        return;
    }
    let shadow = egui::epaint::Shadow {
        offset: Vec2::new(0.0, 2.0 + 3.0 * s),
        blur: 9.0 + 8.0 * s,
        spread: 0.0,
        color: Color32::from_black_alpha((60.0 * s) as u8),
    };
    painter.add(shadow.as_shape(rect, rounding));
}

/// Ein senkrechter Glanzverlauf auf einer gefüllten Fläche: oben ein Saum aus
/// der aufgehellten Füllfarbe, zur Mitte hin nichts. **Nach** der Füllung
/// malen.
///
/// Das ist der Unterschied zwischen einem beigen Rechteck und poliertem
/// Messing: Metall zeigt Licht als Verlauf, nicht als Fläche. Der Verlauf ist
/// seitlich um den Eckradius eingerückt, damit er die Rundung der Fläche
/// darunter nicht überläuft.
pub fn sheen(painter: &egui::Painter, rect: Rect, colour: Color32) {
    let r = Rect::from_min_max(
        rect.min + Vec2::new(crate::ui::theme::R_SM, 1.5),
        Pos2::new(
            rect.max.x - crate::ui::theme::R_SM,
            rect.min.y + rect.height() * 0.55,
        ),
    );
    if !r.is_positive() {
        return;
    }
    let hi = lighten(colour, 0.45);
    let top = Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), 78);
    let gone = Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), 0);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(r.left_top(), top);
    mesh.colored_vertex(r.right_top(), top);
    mesh.colored_vertex(r.right_bottom(), gone);
    mesh.colored_vertex(r.left_bottom(), gone);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Eine 1-Punkt-Fase: ein Hauch Licht an der Oberkante, ein Hauch Schatten an
/// der Unterkante. **Nach** der Füllung malen; `fill` ist deren Farbe.
///
/// Das ist die kleinste Menge Geometrie, die aus „gemaltes Rechteck" ein
/// „geschnittenes Brett" macht: eine Kante, die Licht fängt, und eine, die
/// welches verliert. Seitlich um den Eckradius eingerückt, damit die Linien
/// nicht aus der Rundung laufen.
pub fn bevel(painter: &egui::Painter, rect: Rect, fill: Color32) {
    let inset = crate::ui::theme::R_SM;
    painter.line_segment(
        [
            Pos2::new(rect.left() + inset, rect.top() + 1.5),
            Pos2::new(rect.right() - inset, rect.top() + 1.5),
        ],
        Stroke::new(1.0_f32, lighten(fill, 0.07)),
    );
    painter.line_segment(
        [
            Pos2::new(rect.left() + inset, rect.bottom() - 1.5),
            Pos2::new(rect.right() - inset, rect.bottom() - 1.5),
        ],
        Stroke::new(1.0_f32, darken(fill, 0.22)),
    );
}

/// Ein warmes Glühen um eine Fläche, Stärke `t` aus [`lift`] — null kostet
/// nichts, eins ist volles Hover.
///
/// Drei auslaufende Konturen statt einer dicken: das Auge liest abnehmende
/// Deckung als Leuchten, gleichbleibende als Rahmen. `radius` ist der
/// Eckradius der Fläche selbst; die Konturen wachsen mit, damit das Glühen
/// der Form folgt statt sie zu umkasten.
pub fn glow_halo(painter: &egui::Painter, rect: Rect, radius: f32, colour: Color32, t: f32) {
    if t <= 0.02 {
        return;
    }
    for i in 1..=3u8 {
        let out = i as f32 * 1.6;
        let a = (t * 44.0 / (1.0 + i as f32 * 0.9)) as u8;
        painter.rect_stroke(
            rect.expand(out),
            Rounding::same(radius + out),
            Stroke::new(
                2.4_f32,
                Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), a),
            ),
        );
    }
}

/// Ein wandernder Lichtstreifen auf einer gefüllten Fläche — das Funkeln, das
/// poliertes Metall von gestrichener Farbe unterscheidet. `time` ist die
/// egui-Uhr (`ui.input(|i| i.time)`).
///
/// Nur für Flächen, die ohnehin in jedem Bild neu gemalt werden (die Szene,
/// der Ladebalken): die Funktion fordert selbst **kein** Repaint an, sonst
/// würde ein Schimmer auf einer ruhenden Fläche das ganze Fenster dauerhaft
/// wachhalten.
pub fn gleam(painter: &egui::Painter, rect: Rect, colour: Color32, time: f64) {
    if !rect.is_positive() {
        return;
    }
    // Ein Durchlauf alle paar Sekunden, dazwischen Ruhe: der Streifen läuft
    // über die doppelte Breite hinaus, steht also die meiste Zeit im Off.
    let period = 4.5;
    let frac = ((time / period) % 1.0) as f32 * 2.0 - 0.5;
    let w = (rect.width() * 0.35).clamp(24.0, 140.0);
    let x = rect.left() + frac * rect.width();
    let hi = lighten(colour, 0.6);
    let lit = Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), 56);
    let off = Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), 0);
    let p = painter.with_clip_rect(rect);
    let mut mesh = egui::Mesh::default();
    for (x0, c0, x1, c1) in [(x - w, off, x, lit), (x, lit, x + w, off)] {
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(Pos2::new(x0, rect.top()), c0);
        mesh.colored_vertex(Pos2::new(x1, rect.top()), c1);
        mesh.colored_vertex(Pos2::new(x1, rect.bottom()), c1);
        mesh.colored_vertex(Pos2::new(x0, rect.bottom()), c0);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    p.add(egui::Shape::mesh(mesh));
}

// --- Farbrechnerei ----------------------------------------------------------

pub fn darken(c: Color32, amount: f32) -> Color32 {
    let f = (1.0 - amount).clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

pub fn lighten(c: Color32, amount: f32) -> Color32 {
    let t = amount.clamp(0.0, 1.0);
    Color32::from_rgb(
        (c.r() as f32 + (255.0 - c.r() as f32) * t) as u8,
        (c.g() as f32 + (255.0 - c.g() as f32) * t) as u8,
        (c.b() as f32 + (255.0 - c.b() as f32) * t) as u8,
    )
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darken_and_lighten_stay_in_range() {
        let c = Color32::from_rgb(125, 207, 255);
        assert_eq!(darken(c, 0.0), c);
        assert_eq!(darken(c, 1.0), Color32::from_rgb(0, 0, 0));
        assert_eq!(lighten(c, 1.0), Color32::from_rgb(255, 255, 255));
        assert_eq!(lighten(c, 0.0), c);
        // Übersteuerte Werte dürfen nicht überlaufen.
        assert_eq!(darken(c, 5.0), Color32::from_rgb(0, 0, 0));
        assert_eq!(lighten(c, 5.0), Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn mix_walks_from_one_end_to_the_other() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(200, 100, 50);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 50, 25));
    }

    /// Die Kappe muss auf ihrem Sockel bündig aufsitzen, wenn sie ganz
    /// eingedrückt ist — sonst steht eine Taste im gedrückten Zustand schief.
    #[test]
    fn a_fully_pressed_key_sits_flush_on_its_base() {
        // Der Sockel ist genau um den Hub höher als die Kappe, also sitzt eine
        // ganz eingedrückte Taste bündig darauf. Ein Hub von null hieße: kein
        // Tastengefühl; ein Hub größer als der Sockel hieße: die Kappe fällt
        // unten heraus.
        let cap_h = 48.0_f32;
        let base_h = cap_h + PRESS_DEPTH;
        assert_eq!(base_h - PRESS_DEPTH, cap_h, "Kappe sitzt nicht bündig");
        assert!(
            (1.0..=6.0).contains(&PRESS_DEPTH),
            "Hub {PRESS_DEPTH} unbrauchbar"
        );
    }
}
