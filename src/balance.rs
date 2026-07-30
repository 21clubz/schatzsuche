//! What a recovered wallet holds.
//!
//! The recovery screen proves that a set of words derives a given address. That
//! is not the question the person in front of it actually has, which is "is my
//! money there". This module answers it, twice over, and keeps the two answers
//! apart because they are not the same kind of answer:
//!
//! * [`local`] asks the funded-address list on this disk. Free, instant and
//!   completely offline — but it only knows the addresses that were loaded, and
//!   the practice list holds nothing but random ones, so a `None` from it means
//!   "not in *your* list" and never "the wallet is empty".
//! * [`online`] asks an Esplora service over the network. It is the real number,
//!   and it costs a deliberate decision: the addresses go to whoever runs that
//!   service, which links them to this machine's address. Never call it without
//!   the user having asked for it in that specific moment.
//!
//! **The words never go anywhere.** Only derived addresses are ever sent, and
//! only by [`online`]. That is the same line `alert` draws, for the same reason.
//!
//! # Wie weit gezählt wird
//!
//! Eine Wallet ist keine Adresse. Ihr Geld kann auf jeder ihrer Adressen
//! liegen, und „alle" gibt es nicht: je Kette sind gut zwei Milliarden
//! möglich. Also wird gezählt, bis eine Reihe leerer kommt — [`GAP_LIMIT`]
//! leere hintereinander bedeuten das Ende der Kette. Das ist, was ein
//! Wallet-Programm tut, und BIP-44 nennt dieselbe Zahl.
//!
//! Hier standen einmal fest fünf Adressen je Schema. Das deckt die meisten
//! Wallets ab und war trotzdem die falsche Zusage: wer sein Geld auf Adresse
//! sieben liegen hat — weil er sechsmal empfangen hat —, bekam eine Null zu
//! sehen, und eine Null an dieser Stelle liest sich als „weg".

use crate::address::{encode, Kind};
use crate::bip39::WordCount;
use crate::deriver::Deriver;
use crate::lookup::Database;

/// Wie viele leere Adressen hintereinander das Ende einer Kette bedeuten.
///
/// Zwanzig ist die Zahl aus BIP-44, und sie ist nicht willkürlich: Wallets
/// vergeben Empfangsadressen der Reihe nach, und wer zwanzig davon ausgelassen
/// hat, hat mit sehr großer Wahrscheinlichkeit keine einundzwanzigste benutzt.
pub const GAP_LIMIT: u32 = 20;

/// Wo auch eine Kette voller Treffer aufhört.
///
/// Ohne Deckel könnte eine Wallet mit sehr vielen benutzten Adressen den
/// Zähler beliebig lange weiterlaufen lassen — bei der Online-Abfrage wären
/// das beliebig viele Anfragen. Zweihundert je Kette ist weit jenseits dessen,
/// was eine Wallet erreicht, die jemand von Hand wiederherstellt.
const MAX_DEPTH: u32 = 200;

/// Wie viele Adressen je Schema im Voraus abgeleitet werden.
///
/// Ableiten ist billig (eine Kurvenmultiplikation je Adresse) und passiert
/// darum auf einen Rutsch bis [`MAX_DEPTH`]; **gefragt** wird danach nur so
/// weit, wie das Gap-Limit erlaubt. Andernfalls müsste für jede Verlängerung
/// die ganze Kette neu abgeleitet werden.
const DERIVE_DEPTH: u32 = MAX_DEPTH;

/// What was found, and over how many addresses.
///
/// `checked` travels with the number so the screen can say what the number
/// covers. A balance without that context invites exactly the wrong conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sum {
    pub sats: u64,
    pub checked: usize,
}

/// Eine abgeleitete Empfangsadresse: welche Kette, welcher Platz darin, und
/// womit man sie nachschlägt.
struct Addr {
    chain: usize,
    index: u32,
    kind: Kind,
    hash: [u8; 20],
    text: String,
}

/// Läuft die Ketten ab und fragt jede Adresse, bis [`GAP_LIMIT`] leere
/// hintereinander stehen.
///
/// `ask` liefert den Betrag einer Adresse. Ein Fehler bricht ab — wer seinen
/// Node nicht erreicht, soll das erfahren und keine falsche Summe sehen.
///
/// Die drei Schemata werden getrennt gezählt: eine Wallet, die nur `m/84'`
/// benutzt hat, darf nicht dazu führen, dass bei `m/44'` nach der ersten
/// leeren Adresse aufgehört wird — und umgekehrt.
fn scan<F>(mnemonic: &str, wc: WordCount, mut ask: F) -> Result<Sum, String>
where
    F: FnMut(&Addr) -> Result<u64, String>,
{
    let all = addresses(mnemonic, wc);
    if all.is_empty() {
        return Err("Die Wörter ergeben keine gültige Seed.".into());
    }

    let mut sats = 0u64;
    let mut checked = 0usize;
    for chain in 0..3 {
        // Nach Index sortiert, statt sich auf die Reihenfolge zu verlassen, in
        // der die Ableitung sie ausspuckt: die Gap-Zählung ist nur richtig,
        // wenn sie die Kette der Reihe nach abgeht — bekäme sie die Adressen
        // durcheinander, zählte sie irgendwo mittendrin zwanzig leere zusammen
        // und hörte vor dem Geld auf.
        let mut chain_addrs: Vec<&Addr> = all.iter().filter(|a| a.chain == chain).collect();
        chain_addrs.sort_by_key(|a| a.index);

        let mut empty_run = 0u32;
        for a in chain_addrs {
            if empty_run >= GAP_LIMIT {
                break;
            }
            let found = ask(a)?;
            checked += 1;
            sats = sats.saturating_add(found);
            empty_run = if found == 0 { empty_run + 1 } else { 0 };
        }
    }
    Ok(Sum { sats, checked })
}

/// The addresses of a wallet that get looked at, in chain and index order.
fn addresses(mnemonic: &str, wc: WordCount) -> Vec<Addr> {
    // The words are already known to be a valid mnemonic — the search proved
    // it — so this is one PBKDF2 round and a handful of curve multiplications.
    let mut d = Deriver::new();
    let mut entropy = Vec::new();
    let indices: Vec<u16> = mnemonic
        .split_whitespace()
        .filter_map(crate::bip39::word_index)
        .collect();
    if let Some(e) = crate::bip39::indices_to_entropy(&indices, wc) {
        entropy = e;
    }
    if entropy.is_empty() {
        return Vec::new();
    }
    d.stretch(&entropy, wc);

    let mut out = Vec::with_capacity(DERIVE_DEPTH as usize * 3);
    d.walk(DERIVE_DEPTH, |hash, origin| {
        let kind = origin.kind();
        out.push(Addr {
            chain: origin.purpose,
            index: origin.index,
            kind,
            hash: *hash,
            text: encode(kind, hash),
        });
    });
    out
}

/// What the local funded list knows about this wallet.
///
/// `None` means **no address of this wallet is in the list** — which for the
/// practice list is always true, because it holds random addresses that belong
/// to nobody. It is not a balance of zero, and a caller that renders it as one
/// is lying.
pub fn local(db: &Database, mnemonic: &str, wc: WordCount) -> Option<Sum> {
    // Ein Nachschlagen ist eine binäre Suche über eine gemappte Datei, also
    // praktisch gratis — die Tiefe kostet hier nichts als ein paar Mikrosekunden.
    let mut hits = 0usize;
    let sum = scan(mnemonic, wc, |a| {
        Ok(match db.lookup(a.kind, &a.hash) {
            Some(b) => {
                hits += 1;
                b
            }
            None => 0,
        })
    })
    .ok()?;

    // Kein einziger Treffer heißt „steht nicht in deiner Liste" und **nicht**
    // „null" — siehe oben.
    if hits == 0 {
        return None;
    }
    Some(sum)
}

/// Asks an Esplora service what these addresses hold.
///
/// Blocking, one request per address; belongs on its own thread. Returns the
/// first error verbatim rather than a summary — a caller who cannot reach their
/// own node wants to know why.
///
/// The only thing that leaves this machine is the addresses.
pub fn online(api: &str, mnemonic: &str, wc: WordCount) -> Result<Sum, String> {
    // `trim` zuerst: eine Zeile aus der config.toml, die versehentlich nur
    // Leerzeichen enthält, ist keine Adresse und darf nicht als eine gelten.
    let base = api.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("In der config.toml steht unter [balance] keine Adresse.".into());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build();

    scan(mnemonic, wc, |a| {
        let url = format!("{base}/address/{}", a.text);
        let body = agent
            .get(&url)
            .call()
            .map_err(|e| describe(&url, e))?
            .into_string()
            .map_err(|e| format!("Antwort nicht lesbar: {e}"))?;
        parse_esplora(&body)
    })
}

/// The confirmed balance out of an Esplora `/address/:addr` answer.
///
/// `funded_txo_sum - spent_txo_sum` over `chain_stats`; the mempool is
/// deliberately left out, because an unconfirmed number that changes under the
/// reader is worse than a slightly stale one.
///
/// A free function, and public to the crate, so the shape of somebody else's
/// JSON can be pinned down by a test without a network.
pub(crate) fn parse_esplora(body: &str) -> Result<u64, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Antwort ist kein JSON: {e}"))?;
    let stats = v
        .get("chain_stats")
        .ok_or("Antwort enthält kein chain_stats — ist das eine Esplora-Schnittstelle?")?;
    let funded = stats
        .get("funded_txo_sum")
        .and_then(serde_json::Value::as_u64)
        .ok_or("chain_stats.funded_txo_sum fehlt")?;
    let spent = stats
        .get("spent_txo_sum")
        .and_then(serde_json::Value::as_u64)
        .ok_or("chain_stats.spent_txo_sum fehlt")?;
    Ok(funded.saturating_sub(spent))
}

/// A network failure in words somebody can act on, with the URL that failed —
/// most of these are a typo in `[balance] api` or a node that is not running.
fn describe(url: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("{url} antwortete mit HTTP {code}"),
        ureq::Error::Transport(t) => format!("{url} nicht erreichbar: {t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::entropy_to_mnemonic;
    use crate::lookup::{write_database, Record};

    /// The all-zero test vector, whose addresses are known constants.
    fn abandon() -> String {
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 16], WordCount::W12, &mut m);
        m
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sc-balance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The addresses looked at must be the ones the search itself derives —
    /// otherwise a wallet could hold money on an address the recovery matched
    /// and the balance would still read zero.
    #[test]
    fn the_same_addresses_the_search_would_check() {
        let addrs = addresses(&abandon(), WordCount::W12);
        assert_eq!(addrs.len(), DERIVE_DEPTH as usize * 3, "drei Ketten");

        // The order is the purpose order: legacy, nested, native.
        let first_of = |chain: usize| &addrs[chain * DERIVE_DEPTH as usize];
        assert_eq!(first_of(0).kind, Kind::P2pkh);
        assert_eq!(first_of(1).kind, Kind::P2sh);
        assert_eq!(first_of(2).kind, Kind::P2wpkh);

        // Innerhalb einer Kette stehen sie in Indexreihenfolge — daran hängt
        // die Gap-Zählung, die sonst die falschen Adressen überspringt.
        for chain in 0..3 {
            for (want, a) in addrs.iter().filter(|a| a.chain == chain).enumerate() {
                assert_eq!(a.index as usize, want);
            }
        }

        // The known first native-SegWit address of this seed — the same
        // constant the recovery tests use as their target.
        assert_eq!(
            first_of(2).text,
            "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"
        );

        // Nonsense words give nothing rather than a panic.
        assert!(addresses("nicht mal wörter", WordCount::W12).is_empty());
    }

    /// Das Gap-Limit ist der Grund, warum überhaupt tief abgeleitet wird:
    /// gefragt werden darf nur bis zur Reihe leerer Adressen, sonst wären es
    /// bei der Online-Abfrage sechshundert Anfragen statt sechzig.
    #[test]
    fn the_scan_stops_after_a_run_of_empty_addresses() {
        let mut asked = 0usize;
        let sum = scan(&abandon(), WordCount::W12, |_| {
            asked += 1;
            Ok(0)
        })
        .unwrap();
        assert_eq!(sum.sats, 0);
        assert_eq!(
            asked,
            GAP_LIMIT as usize * 3,
            "leere Wallet: je Kette genau einmal das Gap-Limit"
        );
        assert_eq!(sum.checked, asked, "gezählt wird, was gefragt wurde");

        // Ein Treffer weiter hinten verschiebt die Grenze mit: nach Adresse 7
        // müssen noch einmal zwanzig leere folgen, bevor Schluss ist.
        let mut asked = 0usize;
        let sum = scan(&abandon(), WordCount::W12, |a| {
            asked += 1;
            Ok(if a.chain == 2 && a.index == 7 {
                5_000
            } else {
                0
            })
        })
        .unwrap();
        assert_eq!(sum.sats, 5_000, "das Geld auf Adresse sieben zählt mit");
        assert_eq!(
            asked,
            GAP_LIMIT as usize * 2 + 8 + GAP_LIMIT as usize,
            "die Kette mit dem Treffer läuft acht Adressen weiter"
        );

        // Ein Fehler bricht ab, statt eine zu kleine Summe zu melden.
        let err = scan(&abandon(), WordCount::W12, |_| Err("Node weg".to_string()));
        assert_eq!(err, Err("Node weg".to_string()));
    }

    /// A wallet whose address sits in the local list is found and summed; one
    /// that does not is `None` — not zero.
    #[test]
    fn a_planted_seed_is_found_in_the_local_list() {
        const PLANTED: u64 = 133_700_000;
        let d = tmpdir("planted");
        let path = d.join("funded.scdb");

        // Plant the first native-SegWit address of the abandon seed, plus some
        // unrelated noise, exactly as the practice database does.
        let addrs = addresses(&abandon(), WordCount::W12);
        let planted = &addrs[DERIVE_DEPTH as usize * 2];
        let mut records = crate::lookup::synthetic_records(64).unwrap();
        records.push(Record::new(planted.kind, &planted.hash, PLANTED));
        write_database(&path, records).unwrap();

        let db = Database::open(&path).unwrap();
        let sum = local(&db, &abandon(), WordCount::W12).expect("die gepflanzte Adresse");
        assert_eq!(sum.sats, PLANTED);
        // Zwei leere Ketten je zwanzig, und die dritte einundzwanzig: der
        // Treffer auf Adresse null setzt die Zählung zurück, danach müssen
        // erst wieder zwanzig leere kommen.
        assert_eq!(sum.checked, GAP_LIMIT as usize * 3 + 1);

        // A seed nobody planted is absent, and that is `None` rather than 0 —
        // the whole point of the distinction.
        let other = crate::recover::roll_practice(WordCount::W12).unwrap();
        assert_eq!(local(&db, &other.mnemonic, WordCount::W12), None);
    }

    /// Somebody else's JSON is not a promise. A broken or foreign answer has to
    /// come back as a sentence, not as a zero balance.
    #[test]
    fn a_broken_api_answer_is_reported_not_swallowed() {
        let good = r#"{"address":"bc1q…","chain_stats":
            {"funded_txo_count":3,"funded_txo_sum":250000,
             "spent_txo_count":1,"spent_txo_sum":50000,"tx_count":4},
            "mempool_stats":{"funded_txo_sum":999,"spent_txo_sum":0}}"#;
        assert_eq!(parse_esplora(good), Ok(200_000), "bestätigt, ohne Mempool");

        // A swept wallet is a real zero, and must not be an error.
        let swept = r#"{"chain_stats":{"funded_txo_sum":7,"spent_txo_sum":7}}"#;
        assert_eq!(parse_esplora(swept), Ok(0));

        // More spent than funded cannot happen, but must not underflow.
        let odd = r#"{"chain_stats":{"funded_txo_sum":1,"spent_txo_sum":9}}"#;
        assert_eq!(parse_esplora(odd), Ok(0));

        for bad in [
            "",
            "not json at all",
            r#"{"address":"bc1q…"}"#,
            r#"{"chain_stats":{"funded_txo_sum":5}}"#,
            r#"{"chain_stats":{"funded_txo_sum":"viel","spent_txo_sum":0}}"#,
        ] {
            let e = parse_esplora(bad).expect_err(&format!("{bad:?} müsste scheitern"));
            assert!(!e.is_empty(), "der Fehler braucht Worte");
        }
    }

    /// An empty API setting is a configuration mistake and says so, without
    /// touching the network.
    #[test]
    fn an_empty_api_setting_is_named() {
        let e = online("   ", &abandon(), WordCount::W12).expect_err("muss scheitern");
        assert!(e.contains("[balance]"), "{e}");
    }
}
