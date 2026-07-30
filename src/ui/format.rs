//! Zahlen als deutscher Text.
//!
//! Getrennt vom Zeichencode, weil hier nichts von egui vorkommt und weil genau
//! diese Funktionen die Aussage des Programms tragen: die Hochrechnung ist eine
//! Zahl, die in wissenschaftlicher Schreibweise für die meisten Leser gar
//! nichts bedeutet. Sie in Worte zu übersetzen ist kein Schmuck, sondern der
//! Unterschied zwischen einer Aussage und einer Zeichenfolge.

/// Der abgesuchte Anteil, als Überschrift der SUCHRAUM-Karte.
///
/// Der Wert selbst ist `4,0901e-72 %` — die ehrliche Antwort, und unlesbar für
/// die Leute, für die dieses Fenster gebaut ist. Es ist die Zahl, für die das
/// ganze Programm existiert, geschrieben in einer Notation, die kaum jemand
/// entziffert. Also bekommt der große Platz das Urteil in Worten, und der
/// genaue Wert rutscht eine Zeile tiefer zu den anderen Fachzahlen.
///
/// Gerechnet statt fest eingetragen: der kleinste angebotene Suchraum ist
/// 2^128, in der Praxis steht hier also immer „praktisch 0 %" — aber eine
/// Schwelle, die ausgesprochen ist, kann nicht unbemerkt zur Lüge werden.
pub fn share_headline(percent: f64) -> String {
    if !percent.is_finite() || percent <= 0.0 {
        return "0 %".into();
    }
    if percent < 0.001 {
        return "praktisch 0 %".into();
    }
    format!("{} %", de(percent, 3))
}

/// Wissenschaftliche Schreibweise mit Komma statt Punkt.
pub fn sci(x: f64) -> String {
    if !x.is_finite() {
        return "unendlich".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    format!("{x:.4e}").replace('.', ",")
}

/// Festkomma mit deutschem Dezimalzeichen.
pub fn de(x: f64, places: usize) -> String {
    format!("{x:.places$}", places = places).replace('.', ",")
}

/// Tausendergruppen mit schmalem Abstand statt Punkt, damit die Zahl nicht wie
/// eine Dezimalzahl aussieht.
pub fn thousands(mut n: u64) -> String {
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

/// Größenordnung als deutsches Zahlwort der langen Leiter.
///
/// „5,7 Trillionen" statt „5,7e18". Wer die Schreibweise liest, verliert nichts;
/// wer sie nicht liest, gewinnt die Aussage.
pub fn german_scale(x: f64) -> String {
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

/// Sandkörner auf der Erde, nach der gängigen Schätzung: rund 7,5 Trillionen.
///
/// Eine Größenordnung, keine Messung — und genau als solche unten benutzt. Die
/// Aussage bliebe auch dann richtig, wenn es zehnmal so viele wären.
pub const SAND_GRAINS: f64 = 7.5e18;

/// Die Trefferchance als Bild statt als Zahl.
///
/// „Einer von 4,87e39" ist die ehrliche Antwort und für die meisten Leser
/// keine. Ein Sandkorn ist es: jeder war schon einmal an einem Strand, jeder
/// weiß, dass er kein bestimmtes Korn wiederfinden würde — und genau dieses
/// aussichtslose Unterfangen ist gegen diese Suche noch die leichtere Übung.
///
/// Gerechnet statt hingeschrieben: die Zahl hängt an der Größe der Adressliste
/// und an der Wortlänge. Ein fest eingetragener Vergleich würde still zur Lüge,
/// sobald jemand eine andere Liste lädt.
pub fn odds_picture(expected_seeds: f64) -> String {
    if !expected_seeds.is_finite() || expected_seeds <= 0.0 {
        return String::new();
    }
    let ratio = expected_seeds / SAND_GRAINS;
    if ratio < 2.0 {
        return "Etwa so wahrscheinlich, wie blind ein bestimmtes Sandkorn auf der \
                Erde zu greifen."
            .into();
    }
    format!(
        "Blind ein bestimmtes Sandkorn auf der Erde zu greifen wäre {} Mal wahrscheinlicher.",
        german_scale(ratio)
    )
}

/// Eine grobe Dauer in deutschen Worten. Absichtlich ungenau: „etwa 3 Stunden"
/// ist eine Auskunft, „2 h 47 min 12 s" wäre eine Behauptung.
pub fn estimate(secs: f64) -> String {
    if !secs.is_finite() {
        return "unabsehbar".into();
    }
    if secs < 1.0 {
        "unter einer Sekunde".into()
    } else if secs < 90.0 {
        format!("etwa {} Sekunden", secs.round() as u64)
    } else if secs < 5400.0 {
        format!("etwa {} Minuten", (secs / 60.0).round() as u64)
    } else if secs < 172_800.0 {
        format!("etwa {} Stunden", (secs / 3600.0).round() as u64)
    } else if secs < 31_536_000.0 {
        format!("etwa {} Tage", (secs / 86_400.0).round() as u64)
    } else {
        format!("etwa {} Jahre", (secs / 31_536_000.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Der abgesuchte Anteil darf nie als runde Null erscheinen, solange er es
    /// nicht ist — „praktisch 0 %" sagt dasselbe, ohne zu lügen.
    #[test]
    fn a_vanishing_share_says_so_without_claiming_zero() {
        assert_eq!(share_headline(0.0), "0 %");
        assert_eq!(share_headline(-1.0), "0 %");
        assert_eq!(share_headline(f64::NAN), "0 %");
        assert_eq!(share_headline(1e-70), "praktisch 0 %");
        assert_eq!(share_headline(12.5), "12,500 %");
    }

    /// Das Bild muss mit der Zahl mitwandern. Ein fest eingetragener Vergleich
    /// wäre still falsch geworden, sobald jemand eine andere Adressliste lädt.
    #[test]
    fn the_picture_follows_the_number() {
        // Der reale Fall: rund 4,9e39 erwartete Seeds bei 5 Mio. Adressen.
        let s = odds_picture(4.87e39);
        assert!(s.contains("Sandkorn"), "{s}");
        assert!(
            s.contains("Trillionen"),
            "erwartet ~6,5e20 als Verhältnis: {s}"
        );

        // Zehnmal kleinerer Suchraum heißt zehnmal kleineres Verhältnis, und
        // das muss sich im Text niederschlagen.
        assert_ne!(odds_picture(4.87e39), odds_picture(4.87e38));

        // Genau in der Größenordnung eines Sandkorns: dann ist der Vergleich
        // ein Gleichnis, keine Steigerung.
        let s = odds_picture(SAND_GRAINS);
        assert!(s.starts_with("Etwa so wahrscheinlich"), "{s}");

        // Entartete Eingaben ergeben kein Bild statt eines unsinnigen.
        assert_eq!(odds_picture(f64::INFINITY), "");
        assert_eq!(odds_picture(0.0), "");
        assert_eq!(odds_picture(-1.0), "");
        assert_eq!(odds_picture(f64::NAN), "");
    }

    /// Eine Dauer, die niemand mehr erlebt, muss trotzdem eine Auskunft geben
    /// und darf nicht in Tagen weiterzählen, bis die Zahl unlesbar wird.
    #[test]
    fn a_duration_stays_sayable_at_every_size() {
        assert_eq!(estimate(0.4), "unter einer Sekunde");
        assert_eq!(estimate(30.0), "etwa 30 Sekunden");
        assert_eq!(estimate(600.0), "etwa 10 Minuten");
        assert_eq!(estimate(7200.0), "etwa 2 Stunden");
        assert_eq!(estimate(864_000.0), "etwa 10 Tage");
        assert_eq!(estimate(31_536_000.0 * 4.0), "etwa 4 Jahre");
        assert_eq!(estimate(f64::INFINITY), "unabsehbar");
    }
}
