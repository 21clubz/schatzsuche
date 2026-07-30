//! Die wiederkehrenden Bausteine der Oberfläche.
//!
//! Keiner von ihnen nennt eine Farbe, einen Abstand oder eine Größe selbst —
//! alles kommt aus [`crate::ui::theme`]. Wer hier eine Zahl findet, die nicht
//! aus dem Raster stammt, hat einen Fehler gefunden.

use eframe::egui::{
    self, Align, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Ui,
    Vec2,
};

use crate::ui::theme::{self, mono, pal};

// --- Karten -----------------------------------------------------------------

/// Bequeme Breite für eine der drei Kennzahlkarten und für eines der beiden
/// breiten Felder darunter. Gemessen an der längsten Zeile, die jede von ihnen
/// ohne Umbruch tragen muss.
pub const MIN_STAT_CARD: f32 = 240.0;
pub const MIN_WIDE_CARD: f32 = 330.0;

/// Der senkrechte Platz, den eine [`card`] für sich selbst verbraucht, bevor
/// irgendein Inhalt kommt: beide Innenränder, die Titelzeile, der Abstand
/// darunter und die Umrandung oben wie unten. Wer einer Karte eine exakte Höhe
/// gibt, muss das abziehen.
pub const CARD_CHROME_H: f32 = theme::S3 * 2.0 + 15.0 + theme::S2 + 2.0;

/// Wie viele von `max` Karten nebeneinander in `width` passen.
///
/// Karten hören lange vor dem Nichtmehrpassen auf, lesbar zu sein. Im kleinsten
/// Fenster, das das Programm zulässt, blieben mit offener Einstellungen-
/// Schublade rund 160 Punkte je Spalte: die Überschrift brach um, die Zeile
/// wurde ausgefranst, die Detailzeilen liefen ineinander. Unterhalb von
/// `min_card` lautet die Antwort „weniger Spalten", nicht „schmalere".
pub fn columns_that_fit(width: f32, max: usize, min_card: f32) -> usize {
    if !width.is_finite() || min_card <= 0.0 {
        return 1;
    }
    ((width / min_card).floor() as usize).clamp(1, max.max(1))
}

/// Eine Karte: Titel mit farbigem Rückgrat, darunter der Inhalt.
///
/// Schatten und Fase machen aus der Fläche eine aufgelegte Holztafel: der
/// Schatten hebt sie vom Grund, die helle Ober- und dunkle Unterkante geben
/// ihr eine Dicke. Beides ist bewusst leise — zwölf Karten mit lauten
/// Schatten wären ein Schachbrett, keines Fensters Ruhe wert.
pub fn card(ui: &mut Ui, title: &str, accent: Color32, add: impl FnOnce(&mut Ui)) {
    let resp = egui::Frame::none()
        .fill(pal().panel)
        .rounding(theme::r_md())
        .stroke(theme::hairline())
        .shadow(theme::drop_shadow())
        .inner_margin(egui::Margin::symmetric(theme::S3, theme::S3))
        .show(ui, |ui| {
            // Senkrechte Anordnung und volle Breite erzwingen: ein Frame erbt
            // die Richtung des Elternteils, in einer waagerechten Reihe stünde
            // der Inhalt sonst nebeneinander.
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                // Der Titel trägt einen kurzen Balken in seiner Farbe — ein
                // Rückgrat am Kopf jeder Karte, das aus einer flachen Fläche
                // etwas mit Struktur macht.
                ui.horizontal(|ui| {
                    let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 12.0), Sense::hover());
                    ui.painter().rect_filled(bar, theme::r_xs(), accent);
                    ui.add_space(theme::S2);
                    ui.label(
                        RichText::new(title)
                            .color(accent)
                            .size(theme::SMALL)
                            .strong(),
                    );
                });
                ui.add_space(theme::S2);
                add(ui);
            });
        });
    crate::ui::feel::bevel(ui.painter(), resp.response.rect, pal().panel);
}

/// Kennzahl plus die Bildunterschrift, die sagt, was sie ist.
///
/// Die Schrift richtet sich nach dem Inhalt. Zahlen bekommen das
/// Monospace-Raster, weil sie sich mehrmals pro Sekunde ändern und
/// Proportionalziffern ungleicher Breite die ganze Zeile dabei zappeln lassen.
/// Eine Überschrift aus Worten ändert sich gar nicht, dort bringt das Raster
/// nichts und zieht den Text nur auseinander.
pub fn hero(ui: &mut Ui, value: &str, caption: &str, color: Color32) {
    let is_figure = !value.chars().any(|c| c.is_alphabetic());
    let face = if is_figure {
        mono(theme::DISPLAY)
    } else {
        FontId::proportional(theme::DISPLAY)
    };
    ui.label(RichText::new(value).color(color).font(face).strong());
    ui.label(RichText::new(caption).color(pal().dim).size(theme::SMALL));
    ui.add_space(theme::S2);
}

/// Detailzeile: hier stehen die Fachzahlen.
///
/// Der Wert kommt zuerst, der Name bekommt den Rest und wird gekürzt. Andersherum
/// gebaut — Name links, Wert rechtsbündig im Rest — zeichnete eine zu schmale
/// Zeile beide übereinander, und eine auf 160 Punkte gequetschte Karte las sich
/// als „Speiche25 MB + 145 MB". Von beiden ist die Zahl die, die sich nicht
/// zurückraten lässt, also behält sie ihren Platz.
pub fn kv(ui: &mut Ui, k: &str, v: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(v).color(color).font(mono(theme::SMALL)));
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(RichText::new(k).color(pal().dim).size(theme::SMALL))
                        .truncate(),
                );
            });
        });
    });
}

// --- Hinweise ---------------------------------------------------------------

/// Eine einzeilige farbige Notiz mit gemaltem Punkt, nicht mit einem Zeichen.
/// Die mitgelieferte Schrift kennt weder Haken noch Warndreieck und zeichnete
/// sie als leere Kästen; ein kleiner gefüllter Kreis steht dafür ein.
pub fn note(ui: &mut Ui, colour: Color32, text: &str) {
    let fill = theme::tinted(colour, if theme::is_dark() { 22 } else { 30 });
    let resp = egui::Frame::none()
        .fill(fill)
        .rounding(theme::r_sm())
        .inner_margin(egui::Margin::symmetric(theme::S2 + theme::S1, theme::S2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, colour);
                ui.add_space(theme::S1);
                ui.label(RichText::new(text).color(pal().text).size(theme::BODY));
            });
        });
    crate::ui::feel::bevel(ui.painter(), resp.response.rect, fill);
}

/// Ein Band über die volle Breite, für etwas, das der Leser nicht übersehen
/// darf. Gibt zurück, ob die angebotene Handlung angeklickt wurde.
///
/// Anders als [`note`] hat ein Band immer eine ausführbare Handlung. Eine
/// Fehlermeldung ohne Knopf ist eine Sackgasse, und eine Sackgasse ist genau
/// das, was jemanden dazu bringt, das Fenster zu schließen und zu hoffen.
pub fn banner(ui: &mut Ui, colour: Color32, title: &str, body: &str, action: Option<&str>) -> bool {
    let mut clicked = false;
    let fill = theme::tinted(colour, if theme::is_dark() { 30 } else { 36 });
    let resp = egui::Frame::none()
        .fill(fill)
        .rounding(theme::r_sm())
        .stroke(Stroke::new(1.0_f32, colour))
        .inner_margin(egui::Margin::symmetric(theme::S3, theme::S2 + theme::S1))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(title)
                    .color(colour)
                    .size(theme::BODY)
                    .strong(),
            );
            ui.add_space(theme::S1);
            ui.label(RichText::new(body).color(pal().text).size(theme::SMALL));
            if let Some(label) = action {
                ui.add_space(theme::S2);
                clicked = ui.add(button_quiet(label)).clicked();
            }
        });
    crate::ui::feel::bevel(ui.painter(), resp.response.rect, fill);
    clicked
}

/// „Was heißt das?" — eine Zeile, die sich auf Klick zu einer Erklärung öffnet.
///
/// Das Mittel gegen die zwei Dutzend Texte, die vorher dauerhaft in voller
/// Länge auf dem Bildschirm standen. Sichtbar, aber nicht im Weg: wer es weiß,
/// überliest eine Zeile; wer es nicht weiß, findet die Antwort an Ort und
/// Stelle statt in einer README.
pub fn disclosure(ui: &mut Ui, id: &str, detail: &str) {
    disclosure_labelled(ui, id, "Was heißt das?", detail);
}

/// Wie [`disclosure`], mit eigener Aufschrift.
pub fn disclosure_labelled(ui: &mut Ui, id: &str, summary: &str, detail: &str) {
    let key = egui::Id::new(("disclosure", id));
    let mut open = ui.memory(|m| m.data.get_temp::<bool>(key).unwrap_or(false));

    // Das Dreieck wird gemalt, nicht gesetzt. Die mitgelieferte Schrift kennt
    // weder ▸ noch ▾ und zeichnete beide als leeren Kasten — dieselbe Lücke,
    // die schon die Haken und die Pfeile aus den Notizen fernhält.
    let resp = ui
        .horizontal(|ui| {
            let (mark, resp) =
                ui.allocate_exact_size(Vec2::new(theme::S2, theme::S2), Sense::click());
            let c = mark.center();
            let r = 3.5_f32;
            let pts = if open {
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
            ui.painter().add(egui::Shape::convex_polygon(
                pts,
                pal().primary,
                Stroke::NONE,
            ));
            ui.add_space(theme::S1);
            let label = ui.add(
                egui::Label::new(
                    RichText::new(summary)
                        .color(pal().primary)
                        .size(theme::SMALL),
                )
                .sense(Sense::click()),
            );
            resp.union(label)
        })
        .inner;

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        open = !open;
        ui.memory_mut(|m| m.data.insert_temp(key, open));
    }

    if open {
        ui.add_space(theme::S1);
        egui::Frame::none()
            .fill(pal().inset)
            .rounding(theme::r_sm())
            .stroke(theme::hairline())
            .inner_margin(egui::Margin::symmetric(theme::S2 + theme::S1, theme::S2))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(detail).color(pal().text).size(theme::SMALL));
            });
    }
}

// --- Schaltflächen ----------------------------------------------------------

/// Die eine Hauptaktion eines Bildschirms: gefüllt, in der Akzentfarbe.
pub fn button_primary(label: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_string())
            .color(pal().on_fill)
            .size(theme::BODY)
            .strong(),
    )
    .fill(pal().primary)
    .rounding(theme::r_sm())
    .min_size(Vec2::new(0.0, 40.0))
}

/// Alles andere: umrandet statt gefüllt, damit klar ist, was die Hauptaktion
/// ist. Ein Bildschirm mit zwei gefüllten Knöpfen hat keine.
pub fn button_quiet(label: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_string())
            .color(pal().text)
            .size(theme::BODY),
    )
    .fill(pal().panel)
    .stroke(theme::hairline())
    .rounding(theme::r_sm())
    .min_size(Vec2::new(0.0, 32.0))
}

/// Ein Knopf für die Kopfzeile, der antwortet, bevor er gedrückt wird.
///
/// [`button_quiet`] ist ein gewöhnlicher egui-Knopf und färbt sich beim
/// Überfahren einen Hauch um — auf einer Holzmaserung sieht man davon fast
/// nichts, und die zwei Knöpfe oben rechts wirkten deshalb wie aufgemalt. Hier
/// stehen dieselben drei Handgriffe wie an den Türen und Karten: Er hebt sich
/// unter dem Zeiger, wirft dabei einen Schatten, sein Rahmen leuchtet auf —
/// und beim Drücken sinkt er darunter. Alles über [`feel`], also weich
/// angenähert statt geschaltet; das Zurückfedern beim Loslassen ist der
/// Unterschied zwischen „reagiert" und „fühlt sich an".
///
/// Der Zeiger wird zur Hand. Das ist die Rückmeldung, die zuerst ankommt —
/// noch bevor sich irgendetwas bewegt hat.
pub fn header_button(ui: &mut Ui, label: &str) -> egui::Response {
    let font = egui::FontId::proportional(theme::BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER);

    // Der Platz muss den Hub mitzählen: Was sich hebt, braucht darüber Luft,
    // sonst schneidet die Kopfzeile die angehobene Kante ab.
    let pad = Vec2::new(14.0, 7.0);
    let size = Vec2::new(
        galley.size().x + pad.x * 2.0,
        (galley.size().y + pad.y * 2.0).max(32.0) + LIFT_ROOM,
    );
    let (outer, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let face = crate::ui::feel::tactile(
        ui,
        &resp,
        Rect::from_min_size(
            outer.min + Vec2::new(0.0, LIFT_ROOM),
            Vec2::new(outer.width(), outer.height() - LIFT_ROOM),
        ),
    );
    let t = crate::ui::feel::lift(ui, &resp);

    // `hover` aus der Palette und keine selbst gemischte Aufhellung: Die
    // Farbwelten bringen ihren eigenen Überfahr-Ton mit, und er ist auf den
    // Text darauf abgestimmt. Der erste Versuch hellte die Fläche um die Hälfte
    // Richtung Weiß auf — daraus wurde ein graues Feld, auf dem die Schrift
    // ihren Kontrast verlor. Die Schriftfarbe bleibt darum, wie sie ist: was
    // antwortet, sind Fläche, Rahmen und Schatten.
    crate::ui::feel::shadow_under(ui.painter(), face, theme::r_sm(), t);
    ui.painter().rect_filled(
        face,
        theme::r_sm(),
        crate::ui::feel::mix(pal().panel, pal().hover, t),
    );
    ui.painter().rect_stroke(
        face,
        theme::r_sm(),
        crate::ui::feel::glow_stroke(ui, &resp, pal().primary),
    );

    let at = face.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, pal().text);
    resp
}

/// Wie viel Luft über einem Kopfzeilen-Knopf für seinen Hub reserviert ist.
///
/// [`crate::ui::feel::tactile`] hebt um das Doppelte des Hub-Anteils; drei
/// Punkte decken das mit einem Rest ab, der den Schatten nicht abschneidet.
const LIFT_ROOM: f32 = 3.0;

/// Die Augen eines Würfels, als Anteile der Kantenlänge.
///
/// Ausgelagert, damit der Test die Anzahl je Seite prüfen kann, ohne zu malen —
/// ein Würfel, der bei einer Fünf vier Augen zeigt, ist ein Fehler, den niemand
/// im Code sieht, aber jeder auf dem Bildschirm.
pub fn dice_pips(face: u8) -> &'static [(f32, f32)] {
    const A: f32 = 0.28; // links / oben
    const B: f32 = 0.5; // Mitte
    const C: f32 = 0.72; // rechts / unten
    match face.clamp(1, 6) {
        1 => &[(B, B)],
        2 => &[(A, A), (C, C)],
        3 => &[(A, A), (B, B), (C, C)],
        4 => &[(A, A), (C, A), (A, C), (C, C)],
        5 => &[(A, A), (C, A), (B, B), (A, C), (C, C)],
        _ => &[(A, A), (C, A), (A, B), (C, B), (A, C), (C, C)],
    }
}

/// Ein Knopf mit gemaltem Würfel und Aufschrift. `face` von 1 bis 6.
///
/// Der Würfel ist gemalt und nicht gesetzt, aus demselben Grund wie das Dreieck
/// in [`disclosure`]: die mitgelieferte Schrift kennt kein Würfelzeichen und
/// zeichnete es als leeren Kasten.
///
/// Gibt zurück, ob geklickt wurde.
pub fn dice_button(ui: &mut Ui, label: &str, face: u8) -> bool {
    let side = theme::BODY + 4.0;
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        FontId::proportional(theme::BODY),
        pal().text,
    );

    let pad = theme::S2;
    let size = Vec2::new(
        pad * 2.0 + side + theme::S2 + galley.size().x,
        (side + pad * 2.0).max(32.0),
    );
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // Dieselbe Anfassbarkeit wie bei den großen Tasten: hebt sich unter dem
    // Zeiger, sinkt beim Drücken ein.
    let face_rect = crate::ui::feel::tactile(ui, &resp, rect);
    let p = ui.painter();
    p.rect_filled(face_rect, theme::r_sm(), pal().panel);
    p.rect_stroke(face_rect, theme::r_sm(), theme::hairline());

    // Der Würfel selbst, senkrecht mittig im Knopf.
    let cube = egui::Rect::from_min_size(
        Pos2::new(face_rect.min.x + pad, face_rect.center().y - side / 2.0),
        Vec2::splat(side),
    );
    p.rect_filled(cube, theme::r_sm(), pal().inset);
    p.rect_stroke(cube, theme::r_sm(), theme::hairline());
    let r = (side * 0.09).max(1.3);
    for (fx, fy) in dice_pips(face) {
        p.circle_filled(
            Pos2::new(cube.min.x + side * fx, cube.min.y + side * fy),
            r,
            pal().text,
        );
    }

    p.galley(
        Pos2::new(
            cube.max.x + theme::S2,
            face_rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        pal().text,
    );

    if resp.clicked() {
        crate::ui::feel::bump(crate::ui::feel::Bump::Tap);
    }
    resp.clicked()
}

/// Ein Knopf in einer Signalfarbe — Warnung, Abbruch, Achtung.
pub fn button_signal(label: &str, colour: Color32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_string())
            .color(pal().on_fill)
            .size(theme::BODY)
            .strong(),
    )
    .fill(colour)
    .rounding(theme::r_sm())
    .min_size(Vec2::new(0.0, 32.0))
}

// --- Dateien ----------------------------------------------------------------

/// Zeigt eine Datei im Dateimanager des Systems an.
///
/// Der Ordner, in dem das Programm seine Daten ablegt, ist auf macOS
/// standardmäßig ausgeblendet. Einen Pfad hinzuschreiben und den Leser damit
/// allein zu lassen, hilft genau denen nicht, die es brauchen.
pub fn reveal(path: &std::path::Path) {
    let path = path.to_path_buf();
    // Auf einem eigenen Thread: `open` kehrt schnell zurück, aber ein
    // hängender Dateimanager darf das Fenster nicht einfrieren.
    std::thread::spawn(move || {
        // Existiert die Datei noch nicht, wird stattdessen der Ordner geöffnet,
        // in dem sie entstehen wird — sonst passiert auf einen Klick nichts.
        let (program, args): (&str, Vec<std::ffi::OsString>) = if cfg!(target_os = "macos") {
            if path.exists() {
                ("open", vec!["-R".into(), path.clone().into_os_string()])
            } else {
                ("open", vec![folder_of(&path).into_os_string()])
            }
        } else if cfg!(target_os = "windows") {
            if path.exists() {
                (
                    "explorer",
                    vec![format!("/select,{}", path.display()).into()],
                )
            } else {
                ("explorer", vec![folder_of(&path).into_os_string()])
            }
        } else {
            ("xdg-open", vec![folder_of(&path).into_os_string()])
        };
        let _ = std::process::Command::new(program).args(args).spawn();
    });
}

fn folder_of(path: &std::path::Path) -> std::path::PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

// --- Gezeichnete Flächen ----------------------------------------------------

/// Eine gefüllte Ellipse mit Farbverlauf von der Mitte zum Rand, als Fächer aus
/// Dreiecken. Die eine Grundform, aus der die Münzen und der Schlüssel gebaut
/// sind — sie gibt einer flachen Fläche das runde, belichtete Aussehen von
/// etwas mit Volumen.
pub fn ellipse_gradient(
    painter: &egui::Painter,
    center: Pos2,
    rx: f32,
    ry: f32,
    inner: Color32,
    outer: Color32,
) {
    const SEG: usize = 44;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, inner);
    for i in 0..=SEG {
        let a = i as f32 / SEG as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + Vec2::new(a.cos() * rx, a.sin() * ry), outer);
    }
    for i in 0..SEG as u32 {
        mesh.add_triangle(0, 1 + i, 2 + i);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Eine große anklickbare Fläche auf dem Eröffnungsbildschirm: Bild,
/// Überschrift, eine Zeile Klartext und eine Handlungsaufforderung, alle auf
/// den Zeiger reagierend.
///
/// Von Hand gezeichnet statt aus Widgets zusammengesetzt, damit die ganze Karte
/// ein einziges Klickziel ist, das sich als Ganzes hebt — eine Tür, die man
/// drückt, kein Formular, das man ausfüllt.
#[allow(clippy::too_many_arguments)]
pub fn door(
    ui: &mut Ui,
    size: Vec2,
    accent: Color32,
    art: &TextureHandle,
    title: &str,
    desc: &str,
    cta: &str,
) -> bool {
    let p_ = pal();
    let (slot, resp) = ui.allocate_exact_size(size, Sense::click());
    // Die Tür hebt sich unter dem Zeiger und sinkt beim Drücken ein — dieselbe
    // Bewegung wie bei den großen Knöpfen, damit sich das ganze Programm gleich
    // anfasst.
    let rect = crate::ui::feel::tactile(ui, &resp, slot);
    let hovered = resp.hovered();
    if resp.clicked() {
        crate::ui::feel::bump(crate::ui::feel::Bump::Switch);
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let raised = crate::ui::feel::lift(ui, &resp);
    let p = ui.painter();

    // Die Karte hebt sich unter dem Zeiger: eine Spur heller, der Rand nimmt
    // die Akzentfarbe an, der Schatten wächst mit dem Anheben, und um die
    // Tür legt sich ein Glühen in ihrer Farbe — die Antwort auf den Zeiger
    // soll aussehen wie Licht, nicht wie ein Zustandswechsel.
    let fill = if hovered { p_.hover } else { p_.panel };
    crate::ui::feel::shadow_under(p, rect, theme::r_md(), 0.55 + 0.45 * raised);
    crate::ui::feel::glow_halo(p, rect, theme::R_MD, accent, raised);
    p.rect(
        rect,
        theme::r_md(),
        fill,
        Stroke::new(
            if hovered { 1.6_f32 } else { 1.0_f32 },
            if hovered { accent } else { p_.frame },
        ),
    );
    crate::ui::feel::bevel(p, rect, fill);

    // Eine warm belichtete Kachel — das Innere einer Truhe — mit goldenem
    // Schimmer hinter dem, was darauf liegt. Die Bilder sind auf beiden Türen
    // golden; die Akzentfarbe zeigt sich am Rand und am Knopf, also behalten
    // links und rechts ihre Farbe, ohne den Schatz-Eindruck zu brechen.
    let icon_c = Pos2::new(rect.center().x, rect.top() + 74.0);
    let tile = egui::Rect::from_center_size(icon_c, Vec2::splat(76.0));
    p.rect(
        tile,
        theme::r_sm(),
        p_.art_tile,
        Stroke::new(1.0_f32, p_.art_tile_edge),
    );
    // Der Rahmen der Kachel: eine zweite, innere Linie und vier
    // Messing-Winkel in den Ecken — die Beschläge, die aus „Kachel mit Rand"
    // „gerahmtes Bild auf einem Truhendeckel" machen. Die Winkel liegen um
    // die Rundung eingerückt, damit sie auf der Fläche sitzen statt in der
    // Luft daneben.
    p.rect_stroke(
        tile.shrink(3.0),
        theme::r_xs(),
        Stroke::new(1.0_f32, crate::ui::feel::darken(p_.art_tile_edge, 0.4)),
    );
    let arm = 9.0;
    let brass = Stroke::new(2.0_f32, p_.gold_mid);
    for (corner, dx, dy) in [
        (tile.left_top(), 1.0_f32, 1.0_f32),
        (tile.right_top(), -1.0, 1.0),
        (tile.left_bottom(), 1.0, -1.0),
        (tile.right_bottom(), -1.0, -1.0),
    ] {
        let c = corner + Vec2::new(dx * theme::S1, dy * theme::S1);
        p.line_segment([c, c + Vec2::new(dx * arm, 0.0)], brass);
        p.line_segment([c, c + Vec2::new(0.0, dy * arm)], brass);
    }
    let g = p_.gold_mid;
    ellipse_gradient(
        p,
        icon_c + Vec2::new(0.0, 2.0),
        40.0,
        34.0,
        Color32::from_rgba_unmultiplied(g.r(), g.g(), g.b(), 60),
        Color32::from_rgba_unmultiplied(g.r(), g.g(), g.b(), 0),
    );
    // Das Bild dieser Tür — die Karte oder der Schlüssel — aus denselben
    // Renderings, aus denen auch das Programmsymbol stammt.
    p.image(
        art.id(),
        egui::Rect::from_center_size(icon_c, Vec2::splat(68.0)),
        egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );

    p.text(
        Pos2::new(rect.center().x, rect.top() + 138.0),
        egui::Align2::CENTER_CENTER,
        title,
        FontId::proportional(theme::TITLE + 4.0),
        p_.text,
    );

    // Die Beschreibung bricht in einer bequemen Spalte um, jede Zeile zentriert,
    // damit ein zweizeiliger Block symmetrisch unter der Überschrift sitzt.
    let wrap = rect.width() - theme::S5 - theme::S3;
    let mut job = egui::text::LayoutJob::simple(
        desc.to_string(),
        FontId::proportional(theme::BODY),
        p_.dim,
        wrap,
    );
    job.halign = Align::Center;
    let galley = p.layout_job(job);
    p.galley(
        Pos2::new(rect.center().x, rect.top() + 162.0),
        galley,
        p_.dim,
    );

    // Die Handlungsaufforderung als Pille am Fuß, die sich unter dem Zeiger
    // füllt.
    let cta_rect = egui::Rect::from_center_size(
        Pos2::new(rect.center().x, rect.bottom() - 34.0),
        Vec2::new(rect.width() - theme::S5 - theme::S3, 38.0),
    );
    // Ungeklickt eine Lasur in der Akzentfarbe, nicht die neutrale Grundfläche:
    // ein Knopf in Panelgrau liest sich — im hellen Modus besonders — wie ein
    // deaktiviertes Feld, und dieser hier ist die Hauptaktion des Bildschirms.
    p.rect(
        cta_rect,
        theme::r_sm(),
        if hovered { accent } else { theme::wash(accent) },
        Stroke::new(1.0_f32, accent),
    );
    // Gefüllt bekommt die Pille einen Glanz — sie ist in dem Moment ein
    // Metallknopf, keine Lasur mehr.
    if hovered {
        crate::ui::feel::sheen(p, cta_rect, accent);
    }
    p.text(
        cta_rect.center(),
        egui::Align2::CENTER_CENTER,
        cta,
        FontId::proportional(theme::BODY),
        if hovered { p_.on_fill } else { accent },
    );

    resp.clicked()
}

/// Eine Seed als nummeriertes Wortraster.
///
/// `cols` statt fest drei: in der schmalen Detailspalte bleiben rund 308 Punkte,
/// und drei Spalten passen dort nur bei kurzen Wörtern — „24. abandon" braucht
/// allein schon 86.
pub fn seed_grid(ui: &mut Ui, id: &str, mnemonic: &str, cols: usize) {
    let cols = cols.max(1);
    let words: Vec<&str> = mnemonic.split_whitespace().collect();
    egui::Grid::new(id)
        .num_columns(cols)
        .spacing(Vec2::new(theme::S3, theme::S2))
        .show(ui, |ui| {
            for (i, w) in words.iter().enumerate() {
                ui.label(
                    RichText::new(format!("{:>2}. {}", i + 1, w))
                        .color(pal().text)
                        .font(mono(theme::BODY)),
                );
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
}

/// Eine Zeile der Leistungsliste: wie sie heißt, was sie kostet, und eine
/// fünfteilige Anzeige für das Tempo.
///
/// Gezeichnet statt aus Widgets zusammengesetzt, damit die drei Teile auf einem
/// Raster stehen. Der Punkt ist, dass die Wahl durch Hinsehen möglich sein soll:
/// vorher trug die Zeile Name, Kernzahl und Prozentwert nebeneinander mit einem
/// Satz darunter, und vier davon zu lesen, um eine auszusuchen, ist Arbeit.
///
/// Gibt zurück, ob die Zeile und ob das Fragezeichen angeklickt wurde.
pub fn preset_row(ui: &mut Ui, name: &str, sub: &str, level: u8, active: bool) -> (bool, bool) {
    let p_ = pal();
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::click());

    // Das Fragezeichen bekommt sein eigenes Klickfeld am rechten Ende. Nach der
    // Zeile abgefragt, damit es obenauf liegt; der Aufrufer prüft es zuerst und
    // ignoriert dann die Zeile darunter.
    let info_rect = egui::Rect::from_center_size(
        Pos2::new(rect.right() - theme::S4, rect.center().y),
        Vec2::splat(theme::S4),
    );
    let info = ui.interact(info_rect, ui.id().with(name), Sense::click());

    let hovered = resp.hovered() && !info.hovered();
    let p = ui.painter();

    let fill = if active {
        p_.primary
    } else if hovered {
        p_.hover
    } else {
        p_.bg
    };
    p.rect(
        rect,
        theme::r_sm(),
        fill,
        Stroke::new(1.0_f32, if active { p_.primary } else { p_.frame }),
    );

    let (fg, sub_fg) = if active {
        (p_.on_fill, p_.on_fill_dim)
    } else {
        (p_.text, p_.muted)
    };
    p.text(
        rect.left_top() + Vec2::new(theme::S3, theme::S2),
        egui::Align2::LEFT_TOP,
        name,
        FontId::proportional(theme::BODY),
        fg,
    );
    p.text(
        rect.left_top() + Vec2::new(theme::S3, theme::S4 + theme::S1),
        egui::Align2::LEFT_TOP,
        sub,
        FontId::proportional(theme::SMALL),
        sub_fg,
    );

    let (w, h, gap) = (11.0_f32, 6.0_f32, theme::S1);
    let x0 = rect.right() - theme::S5 - theme::S2 - (5.0 * w + 4.0 * gap);
    let y = rect.center().y - h / 2.0;
    for i in 0..5u8 {
        let seg =
            egui::Rect::from_min_size(Pos2::new(x0 + i as f32 * (w + gap), y), Vec2::new(w, h));
        let colour = match (i < level, active) {
            (true, true) => p_.on_fill,
            (true, false) => p_.primary,
            (false, true) => p_.on_fill_faint,
            (false, false) => p_.frame,
        };
        p.rect_filled(seg, theme::r_xs(), colour);
    }

    let mark = if active {
        p_.on_fill
    } else if info.hovered() {
        p_.primary
    } else {
        p_.dim
    };
    p.circle_stroke(info_rect.center(), 8.5, Stroke::new(1.4_f32, mark));
    p.text(
        info_rect.center(),
        egui::Align2::CENTER_CENTER,
        "i",
        FontId::proportional(theme::SMALL),
        mark,
    );

    (resp.clicked(), info.clicked())
}

/// Die Erklärung, die ein Fragezeichen öffnet: Klartext, höchstens drei Zeilen,
/// und über das *wann* statt über das technische *was*.
pub fn preset_help(ui: &mut Ui, text: &str) {
    egui::Frame::none()
        .fill(pal().inset)
        .rounding(theme::r_sm())
        .stroke(theme::hairline())
        .inner_margin(egui::Margin::symmetric(theme::S2 + theme::S1, theme::S2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(pal().text).size(theme::SMALL));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_shrink_before_cards_do() {
        assert_eq!(columns_that_fit(1200.0, 3, MIN_STAT_CARD), 3);
        assert_eq!(columns_that_fit(600.0, 3, MIN_STAT_CARD), 2);
        assert_eq!(columns_that_fit(300.0, 3, MIN_STAT_CARD), 1);
        // Entartete Eingaben dürfen nicht null Spalten ergeben.
        assert_eq!(columns_that_fit(f32::NAN, 3, MIN_STAT_CARD), 1);
        assert_eq!(columns_that_fit(1200.0, 3, 0.0), 1);
        assert_eq!(columns_that_fit(10.0, 3, MIN_STAT_CARD), 1);
    }

    /// Ein gemalter Würfel muss so viele Augen zeigen, wie er heißt — ein Fehler,
    /// den im Quelltext niemand sieht und auf dem Bildschirm jeder.
    #[test]
    fn a_painted_die_shows_as_many_pips_as_it_claims() {
        for face in 1..=6u8 {
            assert_eq!(
                dice_pips(face).len(),
                face as usize,
                "Seite {face} hat die falsche Augenzahl"
            );
        }
        // Unsinnige Werte dürfen nicht in einen leeren Würfel laufen.
        assert_eq!(dice_pips(0).len(), 1);
        assert_eq!(dice_pips(9).len(), 6);

        // Alle Augen liegen innerhalb der Kante, sonst ragen sie heraus.
        for face in 1..=6u8 {
            for (x, y) in dice_pips(face) {
                assert!((0.1..=0.9).contains(x), "Auge bei x={x} liegt am Rand");
                assert!((0.1..=0.9).contains(y), "Auge bei y={y} liegt am Rand");
            }
        }
    }

    /// Der Ordner ist der Rückfall, wenn die Datei noch nicht existiert — sonst
    /// passiert auf einen Klick sichtbar nichts.
    #[test]
    fn a_path_without_a_parent_still_names_a_folder() {
        assert_eq!(
            folder_of(std::path::Path::new("hits.jsonl")),
            std::path::PathBuf::from(".")
        );
        assert_eq!(
            folder_of(std::path::Path::new("/tmp/x/hits.jsonl")),
            std::path::PathBuf::from("/tmp/x")
        );
    }
}
