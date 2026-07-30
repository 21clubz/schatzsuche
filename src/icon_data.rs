//! The artwork, carried inside the binary: the chest that is the application
//! icon, and the two pictures on the doors of the opening fork.
//!
//! Embedded rather than loaded from the bundle, so the icon does not depend on
//! macOS resolving the bundle resource — which it failed to do once already,
//! after the bundle had briefly shipped a shell script as its executable and
//! the icon cache remembered a placeholder.
//!
//! Stored as PNGs and decoded at startup rather than as raw RGBA arrays in the
//! source. The array worked at 128 pixels and was already 2700 lines; the
//! intro screen draws the mark at 420 physical pixels on a Retina display, so
//! 128 arrived visibly soft. The same picture at 512 would be a megabyte of
//! Rust source. Decoding costs well under a millisecond, once.
//!
//! The chest comes from `assets/chest-source.png` via `scripts/make-icon.py`;
//! die Würfel, der Schlüssel und die Seekarte aus
//! `assets/{dice,key,map}-source.png` via `scripts/make-door-art.py`. Alles
//! außer den Würfeln ist von seinem cremefarbenen Grund freigestellt und steht
//! damit auf dem, was hinter ihm liegt; die Würfel sind ein runder Ausschnitt
//! aus einer ganzen Szene — warum, steht im Kopf jenes Skripts.

/// Decoded icon: RGBA, eight bits per channel, `width * height * 4` bytes.
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

static ICON_PNG: &[u8] = include_bytes!("../assets/icon-512.png");
static DOOR_SEARCH_PNG: &[u8] = include_bytes!("../assets/door-search.png");
static DOOR_RECOVER_PNG: &[u8] = include_bytes!("../assets/door-recover.png");
static WOOD_PNG: &[u8] = include_bytes!("../assets/wood-256.png");
static MAP_BG_PNG: &[u8] = include_bytes!("../assets/map-bg.png");

/// Decodes the embedded icon: the chest, on nothing.
///
/// There is no tile behind it — not in the window, not in the Dock, not on the
/// desktop. macOS would normally expect a rounded square; this is deliberately
/// a cut-out object instead.
///
/// Returns `None` rather than panicking: a window without an icon is a small
/// blemish, and a program that refuses to start over one would be a
/// considerably larger one.
pub fn icon() -> Option<Icon> {
    decode(ICON_PNG)
}

/// Die Würfel, für die Tür „Wallets würfeln".
pub fn door_search() -> Option<Icon> {
    decode(DOOR_SEARCH_PNG)
}

/// The key, for the Seed retten door.
pub fn door_recover() -> Option<Icon> {
    decode(DOOR_RECOVER_PNG)
}

/// The tiling plank wood, painted behind the surfaces.
///
/// **Deckend** und mit eigener Helligkeit — Fugen fast schwarz, Brettkörper
/// im Mittelton, je Brett ein eigener Stich. Zwei frühere Fassungen waren
/// Lasuren über dem Palettengrund und kamen beide als blasses Linienmuster
/// heraus: ein fast schwarzer Grund lässt sich nicht sichtbar abdunkeln.
/// Farblos ist die Kachel trotzdem fast: `draw_grain` multipliziert sie beim
/// Malen mit [`crate::ui::theme::Palette::wood`], so dient eine Kachel jeder
/// Holz-Farbwelt. Built by `scripts/make-wood.py`, seamless in both
/// directions by construction.
pub fn wood() -> Option<Icon> {
    decode(WOOD_PNG)
}

/// Die Seekarte, die auf der Gabelung hinter den zwei Türen liegt.
///
/// Sie war einmal das Bild der linken Tür; dort liegen jetzt Würfel. Statt sie
/// wegzuwerfen, liegt sie eine Ebene tiefer — auf dem Bildschirm, der fragt
/// „Was möchtest du tun?", und das ist die Frage, für die es Karten gibt.
///
/// Freigestellt, also mit durchsichtigem Rand: was ankommt, ist ein Blatt
/// Pergament auf dem Holz und kein Rechteck.
pub fn map_bg() -> Option<Icon> {
    decode(MAP_BG_PNG)
}

fn decode(bytes: &[u8]) -> Option<Icon> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;

    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(Icon {
        width: info.width,
        height: info.height,
        rgba: buf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon has to decode, be square, and be big enough for the intro
    /// screen, which draws it at 420 physical pixels on a Retina display.
    #[test]
    fn icon_decodes_at_a_useful_size() {
        let icon = icon().expect("embedded icon does not decode");
        assert_eq!(icon.width, icon.height, "icon is not square");
        assert!(
            icon.width >= 256,
            "icon is {}px; the intro screen would show it soft",
            icon.width
        );
        assert_eq!(
            icon.rgba.len() as u32,
            icon.width * icon.height * 4,
            "pixel buffer does not match the stated size"
        );
        assert!(
            icon.rgba.chunks_exact(4).any(|p| p[3] > 0),
            "icon is fully transparent"
        );
    }

    /// The grain has to join itself, or tiling it shows a grid of seams across
    /// the whole window.
    ///
    /// Measured rather than eyeballed, and against a control: the step across the
    /// boundary must be no worse than the average step anywhere inside. The tile
    /// carries speckle, so "no difference at all" is the wrong bar — "no more
    /// difference than anywhere else" is the right one.
    ///
    /// Measured on the summed brightness of each pixel. Die Kachel ist seit
    /// dem Planken-Umbau **deckend** — ihr Alpha trägt keine Information
    /// mehr, ihre eigene Helligkeit ist, was das Auge (getönt) bekommt. It
    /// also has to be one control over the whole tile rather than one
    /// column: the grain deliberately swells and fades, and a single column
    /// can land in a quiet stretch and set an unreachably low bar.
    #[test]
    fn the_wood_tile_has_no_seam() {
        let w = wood().expect("embedded wood does not decode");
        assert_eq!(w.width, w.height, "tile is not square");
        assert_eq!(w.rgba.len() as u32, w.width * w.height * 4);

        // Die Helligkeit eines Pixels, Rot plus Grün plus Blau (0 bis 765).
        let effect = |x: u32, y: u32| {
            let o = ((y * w.width + x) * 4) as usize;
            w.rgba[o] as i32 + w.rgba[o + 1] as i32 + w.rgba[o + 2] as i32
        };
        let mean = |v: Vec<i32>| v.iter().sum::<i32>() as f64 / v.len() as f64;
        let last = w.width - 1;

        let across = mean(
            (0..w.height)
                .map(|y| (effect(last, y) - effect(0, y)).abs())
                .collect(),
        );
        let down = mean(
            (0..w.width)
                .map(|x| (effect(x, last) - effect(x, 0)).abs())
                .collect(),
        );
        // Die Vergleichsgröße, **je Richtung getrennt**. Solange die Kachel
        // isotropes Furnier war, reichte ein gemeinsamer Innenwert; seit sie
        // waagerechte Planken zeigt, sind senkrechte Schritte von Natur aus
        // steiler als waagerechte — die Maserungslinien stapeln sich in y.
        // Ein senkrechter Randschritt gegen den waagerechten Innenschnitt
        // gemessen schlug fehl, obwohl der Umlauf pixelgenau weiterlief:
        // Äpfel gegen Birnen, nicht Naht gegen Holz.
        let inside_x = mean(
            (0..w.height)
                .flat_map(|y| (0..last).map(move |x| (effect(x, y) - effect(x + 1, y)).abs()))
                .collect(),
        );
        let inside_y = mean(
            (0..last)
                .flat_map(|y| (0..w.width).map(move |x| (effect(x, y) - effect(x, y + 1)).abs()))
                .collect(),
        );

        assert!(
            across <= inside_x * 1.6,
            "senkrechte Naht springt um {across:.2}, im Bild nur um {inside_x:.2}"
        );
        assert!(
            down <= inside_y * 1.6,
            "waagerechte Naht springt um {down:.2}, im Bild nur um {inside_y:.2}"
        );

        // Die Kachel muss **deckend** sein. Zwei frühere Fassungen waren
        // Lasuren über dem Palettengrund, und beide endeten als blasses
        // Linienmuster: ein Grund bei RGB um 15 hat bis Schwarz nur zwölf
        // Stufen — eine „fast schwarze Fuge" kann dort nicht dunkler
        // aussehen als der Grund selbst, und der Renderer staucht dunkle
        // Lasuren obendrein (Kalibrierkachel durch `--screenshot`: dunkle
        // Lasur mit Alpha 24 nimmt dem Grund höchstens zwei Stufen, helle
        // mit Alpha 1 gibt ihm elf). Deckend gemalt kommt an, was hier
        // steht; die Farbwelt liefert `Palette::wood` als Tönung.
        assert!(
            w.rgba.chunks_exact(4).all(|p| p[3] == 255),
            "die Kachel ist nicht mehr deckend — Lasur war das Modell, das zweimal blass endete"
        );

        // Und sie muss wirklich Planken zeigen, keine bedruckte Fläche: die
        // tiefsten Partien (Fugen, Astkerne) liegen weit unter dem
        // Brettkörper, die Maserungskämme darüber. Der Körper selbst bleibt
        // im Mittelfeld — die Tönung kann nur abdunkeln, eine zu dunkle
        // Kachel kann keine Farbwelt mehr retten, und eine zu helle säße
        // über den Karten.
        let mut lumas: Vec<i32> = (0..w.height)
            .flat_map(|y| (0..w.width).map(move |x| effect(x, y)))
            .collect();
        lumas.sort_unstable();
        let pct = |p: f64| lumas[((lumas.len() - 1) as f64 * p) as usize];
        let body = pct(0.5);
        assert!(
            (250..=700).contains(&body),
            "Brettkörper bei {body} von 765 — passt nicht zu einer Tönung um panel · 1,3"
        );
        let deep = pct(0.02);
        assert!(
            deep < body / 2,
            "keine Fugen: die tiefsten Partien ({deep}) liegen kaum unter dem Körper ({body})"
        );
        let ridge = pct(0.995);
        assert!(
            ridge > body + body / 10,
            "keine Kämme: die hellsten Partien ({ridge}) heben sich nicht vom Körper ({body}) ab"
        );
    }

    /// Die Seekarte hinter der Gabelung ist ein freigestelltes Blatt, kein
    /// Rechteck.
    ///
    /// Daran hängt, dass sie als Pergament auf dem Holz liegt und nicht als
    /// Kachel darübergelegt wirkt: die Ecken müssen durchsichtig sein, und
    /// zwar ganz — bei der Deckung, mit der das Fenster sie aufträgt, würde
    /// schon ein Rest Hintergrund als sichtbare Kante ankommen.
    #[test]
    fn the_map_behind_the_fork_is_a_cut_out_sheet() {
        let map = map_bg().expect("embedded map does not decode");
        assert_eq!(map.width, map.height, "map sheet is not square");
        assert!(
            map.width >= 512,
            "map is {}px; it is drawn across most of the window",
            map.width
        );

        let alpha_at = |x: u32, y: u32| map.rgba[((y * map.width + x) * 4 + 3) as usize];
        let (w, h) = (map.width, map.height);
        for (x, y) in [(2, 2), (w - 3, 2), (2, h - 3), (w - 3, h - 3)] {
            assert_eq!(
                alpha_at(x, y),
                0,
                "die Karte bringt bei ({x}, {y}) Grund mit"
            );
        }

        // Und sie muss den Bogen füllen: ein Blatt, das nach dem Freistellen
        // nur noch aus Fetzen besteht, wäre auf dem Bildschirm ein Fleck.
        let total = (w * h) as usize;
        let opaque = map.rgba.chunks_exact(4).filter(|c| c[3] > 200).count();
        assert!(
            opaque * 2 > total,
            "vom Blatt ist zu wenig übrig: {opaque}/{total}"
        );
    }

    /// The icon is a cut-out chest, not a plate.
    ///
    /// This is the whole point of it: there is no rounded square behind the
    /// chest, so the background it is dropped on — a window, the Dock, a
    /// desktop picture — shows through.
    #[test]
    fn the_icon_carries_no_tile() {
        let icon = icon().expect("icon");
        let alpha_at = |x: u32, y: u32| icon.rgba[((y * icon.width + x) * 4 + 3) as usize];

        // Points along the edges, where a tile would be solid and the chest is
        // not. Not the corners: the chest is scaled to a small margin and its
        // gold brackets reach into them, so a corner proves nothing.
        //
        // A trace of alpha is allowed rather than exactly none — the warm glow
        // behind the chest is a wide, soft radial and reaches this far at a
        // value of two or three. A plate would be an order of magnitude more.
        const GLOW_TRACE: u8 = 12;
        for (x, y) in [(256, 20), (256, 492), (100, 25), (400, 25), (25, 100)] {
            let a = alpha_at(x, y);
            assert!(
                a <= GLOW_TRACE,
                "the icon still has a tile at ({x}, {y}): alpha {a}"
            );
        }
    }

    /// The two door pictures decode, are square, and are big enough for the
    /// box the opening fork draws them into — 68 points, so 136 physical
    /// pixels on a Retina display.
    #[test]
    fn door_art_decodes_at_a_useful_size() {
        for (what, art) in [("search", door_search()), ("recover", door_recover())] {
            let art = art.unwrap_or_else(|| panic!("door art `{what}` does not decode"));
            assert_eq!(art.width, art.height, "door art `{what}` is not square");
            assert!(
                art.width >= 128,
                "door art `{what}` is {}px; the door would show it soft",
                art.width
            );
            assert_eq!(
                art.rgba.len() as u32,
                art.width * art.height * 4,
                "door art `{what}`: pixel buffer does not match the stated size"
            );

            // Enough of it survived the cut to be a picture rather than a
            // sliver — the flood fill eats everything it can reach, and a
            // threshold slipped too far would quietly return an empty sheet.
            //
            // The bar is deliberately low. These are cut-out objects on a
            // square sheet, and a thin one covers far less of it than it
            // looks: the key is a diagonal with filigree holes through the
            // bow and fills a sixth of its sheet. A tenth still catches the
            // failure worth catching, which is a sheet with nothing on it.
            let total = (art.width * art.height) as usize;
            let opaque = art.rgba.chunks_exact(4).filter(|c| c[3] > 200).count();
            assert!(
                opaque * 10 > total,
                "door art `{what}` is mostly transparent: {opaque}/{total}"
            );
        }
    }

    /// The door pictures are cut out, like the chest: the tile they sit on is
    /// painted by the window, and the artwork must not bring its own.
    #[test]
    fn door_art_carries_no_backdrop() {
        for (what, art) in [("search", door_search()), ("recover", door_recover())] {
            let art = art.expect("door art");
            let alpha_at = |x: u32, y: u32| art.rgba[((y * art.width + x) * 4 + 3) as usize];
            let (w, h) = (art.width, art.height);
            for (x, y) in [(2, 2), (w - 3, 2), (2, h - 3), (w - 3, h - 3)] {
                let a = alpha_at(x, y);
                assert_eq!(
                    a, 0,
                    "door art `{what}` still has its backdrop at ({x}, {y}): alpha {a}"
                );
            }
        }
    }
}
