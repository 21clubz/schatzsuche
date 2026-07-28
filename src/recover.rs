//! Recovering *your own* seed when part of it is lost.
//!
//! This is the honest inverse of the collider. The main search is hopeless on
//! purpose: it looks for anyone's wallet across the whole keyspace, and the
//! numbers say it finds nothing. Here the target is a single wallet you can
//! prove is yours — you hold most of the words — so the space is small enough
//! that a hit is not only possible but likely.
//!
//! Three kinds of loss are handled:
//!
//! * **Missing words** — you wrote `?` where a word is gone. Each blank is one
//!   of 2048, so the raw space is 2048 to the power of the number of blanks;
//!   the checksum then throws all but one in 16..256 away before any
//!   derivation happens.
//! * **A wrong word** — every word is there but one is mistyped, and you do
//!   not know which. Every position is tried against all 2048 words.
//! * **Two words swapped** — the words are right but two adjacent ones changed
//!   places. Every adjacent pair is tried.
//!
//! What is deliberately *not* offered is a free permutation of all the words:
//! twenty-four words rearrange 6·10²³ ways, which is back in collider
//! territory, and pretending otherwise would be the dishonest thing this whole
//! program exists to argue against.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::address::{decode, script_hashes, Kind};
use crate::bip32::Node;
use crate::bip39::{indices_to_entropy, word_index, wordlist, Pbkdf2Ctx, WordCount};
use crate::deriver::external_chain;

/// How the seed was damaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Blanks (`?`) are filled in.
    Missing,
    /// One word is wrong; every position is retried against the whole list.
    Typo,
    /// Two adjacent words are swapped.
    Swap,
}

/// A recovery job, parsed and validated but not yet run.
pub struct Plan {
    pub mode: Mode,
    /// Word indices, with `None` for a blank. Length is the mnemonic length.
    words: Vec<Option<u16>>,
    pub word_count: WordCount,
    /// The address the recovered seed must produce, and its script kind.
    target: [u8; 20],
    target_kind: Kind,
    pub target_str: String,
    /// Addresses derived per BIP path before giving up on a candidate.
    pub depth: u32,
}

/// A found seed.
#[derive(Clone)]
pub struct Found {
    pub mnemonic: String,
    pub address: String,
    pub path: String,
}

/// The words half of a job: everything checkable before an address is given.
///
/// Split out so the window can show the search space and any input error while
/// the address field is still empty, rather than staying silent until the
/// whole form is filled.
pub struct Words {
    parsed: Vec<Option<u16>>,
    pub word_count: WordCount,
    pub mode: Mode,
}

impl Words {
    /// Parses and checks the words for a mode, without an address.
    pub fn parse(words: &str, mode: Mode) -> Result<Words, String> {
        let tokens: Vec<&str> = words.split_whitespace().collect();
        let word_count = WordCount::from_words(tokens.len() as u8).ok_or_else(|| {
            format!(
                "eine Seed hat 12, 15, 18, 21 oder 24 Wörter, hier sind es {}",
                tokens.len()
            )
        })?;

        let mut parsed = Vec::with_capacity(tokens.len());
        for (i, tok) in tokens.iter().enumerate() {
            if *tok == "?" {
                parsed.push(None);
            } else {
                let idx = word_index(tok).ok_or_else(|| {
                    format!("Wort {} („{tok}“) steht nicht auf der BIP-39-Liste", i + 1)
                })?;
                parsed.push(Some(idx));
            }
        }

        let blanks = parsed.iter().filter(|w| w.is_none()).count();
        match mode {
            Mode::Missing if blanks == 0 => {
                return Err("kein „?“ angegeben — im Modus „fehlende Wörter“ markiert \
                            man die Lücken mit einem Fragezeichen"
                    .into())
            }
            Mode::Missing if blanks > 4 => {
                return Err(format!(
                    "{blanks} fehlende Wörter sind zu viele: der Suchraum wächst je Lücke \
                     um das 2048-fache und wird jenseits von vier praktisch unendlich"
                ))
            }
            Mode::Typo | Mode::Swap if blanks > 0 => {
                return Err("in diesem Modus dürfen keine „?“ stehen — er sucht einen \
                            Fehler, kein fehlendes Wort"
                    .into())
            }
            _ => {}
        }

        Ok(Words {
            parsed,
            word_count,
            mode,
        })
    }

    /// Candidate count for these words alone — same figure [`Plan`] reports.
    pub fn candidate_count(&self) -> u64 {
        candidate_count(self.mode, &self.parsed)
    }
}

/// Seconds a search of `candidates` mnemonics is expected to take.
///
/// The checksum-fail path is a single SHA-256; the fraction that pass add
/// PBKDF2 and derivation. Both costs were measured on an M1, and the estimate
/// is deliberately high — better to promise ten minutes and finish in two.
/// Shared by the terminal command and the window so they agree.
pub fn estimate_secs(candidates: u64, wc: WordCount) -> f64 {
    const CHEAP_PER: f64 = 0.09e-6;
    const DERIVE_PER: f64 = 2.5e-3;
    let total = candidates as f64;
    let pass_fraction = 1.0 / 2f64.powi(wc.checksum_bits() as i32);
    total * CHEAP_PER + total * pass_fraction * DERIVE_PER
}

/// Shared by [`Words`] and [`Plan`]: the size of the space before the checksum.
fn candidate_count(mode: Mode, words: &[Option<u16>]) -> u64 {
    match mode {
        Mode::Missing => {
            let blanks = words.iter().filter(|w| w.is_none()).count() as u32;
            2048u64.saturating_pow(blanks)
        }
        Mode::Typo => words.len() as u64 * 2048,
        Mode::Swap => words.len().saturating_sub(1) as u64,
    }
}

impl Plan {
    /// Parses the words, the target address and the mode into a job.
    ///
    /// `words` is whitespace-separated, with `?` for a blank. Blanks are only
    /// meaningful in [`Mode::Missing`]; the other modes reject them, since a
    /// missing word and a wrong word are different problems.
    pub fn new(words: &str, target: &str, mode: Mode, depth: u32) -> Result<Plan, String> {
        let checked = Words::parse(words, mode)?;

        let (target_kind, target) =
            decode(target.trim()).ok_or("die Zieladresse ist keine gültige Bitcoin-Adresse")?;

        Ok(Plan {
            mode,
            words: checked.parsed,
            word_count: checked.word_count,
            target,
            target_kind,
            target_str: target.iter().map(|b| format!("{b:02x}")).collect(),
            depth,
        })
    }

    /// How many candidate mnemonics the search will examine.
    ///
    /// Before the checksum, which is the honest number to warn on: it is what
    /// bounds the time, even though only a fraction reach derivation.
    pub fn candidate_count(&self) -> u64 {
        candidate_count(self.mode, &self.words)
    }

    /// Seconds the search is expected to take. See [`estimate_secs`].
    pub fn estimate_secs(&self) -> f64 {
        estimate_secs(self.candidate_count(), self.word_count)
    }

    /// Runs the search, calling `progress` with the running candidate count and
    /// stopping early if `cancel` is set. Returns the first seed that produces
    /// the target address, or `None` if the space is exhausted.
    pub fn run(&self, cancel: &Arc<AtomicBool>, counter: &Arc<AtomicU64>) -> Option<Found> {
        let mut engine = Engine::new(self);
        let mut done = 0u64;
        let mut check = move |indices: &[u16]| -> Option<Found> {
            done += 1;
            if done.is_multiple_of(4096) {
                counter.store(done, Ordering::Relaxed);
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
            }
            engine.try_candidate(indices)
        };

        let mut buf: Vec<u16> = self.words.iter().map(|w| w.unwrap_or(0)).collect();

        match self.mode {
            Mode::Missing => {
                let blanks: Vec<usize> = self
                    .words
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| w.is_none())
                    .map(|(i, _)| i)
                    .collect();
                self.fill_blanks(&mut buf, &blanks, 0, cancel, &mut check)
            }
            Mode::Typo => {
                for pos in 0..buf.len() {
                    let original = buf[pos];
                    for w in 0..2048u16 {
                        if cancel.load(Ordering::Relaxed) {
                            return None;
                        }
                        buf[pos] = w;
                        if let Some(f) = check(&buf) {
                            return Some(f);
                        }
                    }
                    buf[pos] = original;
                }
                None
            }
            Mode::Swap => {
                for pos in 0..buf.len().saturating_sub(1) {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    buf.swap(pos, pos + 1);
                    if let Some(f) = check(&buf) {
                        return Some(f);
                    }
                    buf.swap(pos, pos + 1);
                }
                None
            }
        }
    }

    /// Recursively fills the blank positions with every combination.
    fn fill_blanks(
        &self,
        buf: &mut [u16],
        blanks: &[usize],
        depth: usize,
        cancel: &Arc<AtomicBool>,
        check: &mut impl FnMut(&[u16]) -> Option<Found>,
    ) -> Option<Found> {
        if depth == blanks.len() {
            return check(buf);
        }
        for w in 0..2048u16 {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            buf[blanks[depth]] = w;
            if let Some(f) = self.fill_blanks(buf, blanks, depth + 1, cancel, check) {
                return Some(f);
            }
        }
        None
    }
}

/// Per-run derivation state, so the candidate loop allocates nothing.
struct Engine<'p> {
    plan: &'p Plan,
    pbkdf2: Pbkdf2Ctx,
    secp: secp256k1::Secp256k1<secp256k1::SignOnly>,
    mnemonic: String,
    seed: [u8; 64],
    buf: [u8; 37],
    /// Which BIP purposes to try: only the one that matches the target's script
    /// kind, since a P2WPKH address can never come from a P2PKH derivation.
    purposes: Vec<(u32, usize)>,
}

/// BIP-44/49/84 purposes paired with the script kind each produces.
const PURPOSES: [(u32, Kind); 3] = [(44, Kind::P2pkh), (49, Kind::P2sh), (84, Kind::P2wpkh)];

impl<'p> Engine<'p> {
    fn new(plan: &'p Plan) -> Engine<'p> {
        // Only the purpose whose script kind matches the target can produce it.
        let purposes = PURPOSES
            .iter()
            .enumerate()
            .filter(|(_, (_, kind))| *kind == plan.target_kind)
            .map(|(i, (purpose, _))| (*purpose, i))
            .collect();
        Engine {
            plan,
            pbkdf2: Pbkdf2Ctx::new(),
            secp: secp256k1::Secp256k1::signing_only(),
            mnemonic: String::with_capacity(24 * 9),
            seed: [0u8; 64],
            buf: [0u8; 37],
            purposes,
        }
    }

    /// Checks one candidate: checksum first, then derivation, then the address.
    fn try_candidate(&mut self, indices: &[u16]) -> Option<Found> {
        // The checksum throws out all but one candidate in 16..256, cheaply.
        indices_to_entropy(indices, self.plan.word_count)?;

        self.mnemonic.clear();
        let list = wordlist();
        for (i, &idx) in indices.iter().enumerate() {
            if i > 0 {
                self.mnemonic.push(' ');
            }
            self.mnemonic.push_str(list[idx as usize]);
        }

        self.pbkdf2.seed(&self.mnemonic, "", &mut self.seed);
        let master = Node::master(&self.seed)?;

        for &(purpose, _) in &self.purposes {
            let chain = match external_chain(&self.secp, &master, purpose, &mut self.buf) {
                Some(c) => c,
                None => continue,
            };
            for i in 0..self.plan.depth {
                let pk = match chain.child_pubkey(&self.secp, i, &mut self.buf) {
                    Some(p) => p,
                    None => continue,
                };
                let hashes = script_hashes(&pk);
                if hashes[self.plan.target_kind as usize] == self.plan.target {
                    return Some(Found {
                        mnemonic: self.mnemonic.clone(),
                        address: self.plan.target_str_readable(),
                        path: format!("m/{purpose}'/0'/0'/0/{i}"),
                    });
                }
            }
        }
        None
    }
}

impl Plan {
    fn target_str_readable(&self) -> String {
        crate::address::encode(self.target_kind, &self.target)
    }
}

/// A crude but honest reading of the search space, for the warning text.
pub fn describe_space(plan: &Plan) -> String {
    let n = plan.candidate_count();
    let kind = match plan.mode {
        Mode::Missing => "fehlende Wörter",
        Mode::Typo => "ein falsches Wort",
        Mode::Swap => "zwei vertauschte Nachbarn",
    };
    format!(
        "{kind}: {} Kombinationen zu prüfen",
        crate::util::group_digits(n)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::{entropy_to_mnemonic, indices_to_entropy};

    /// A known 12-word vector: all zero entropy is "abandon" eleven times then
    /// "about". Recovering its last word from a blank must return it.
    fn abandon_12() -> Vec<u16> {
        let entropy = [0u8; 16];
        let mut m = String::new();
        entropy_to_mnemonic(&entropy, WordCount::W12, &mut m);
        m.split_whitespace()
            .map(|w| word_index(w).unwrap())
            .collect()
    }

    /// The address that seed produces at m/84'/0'/0'/0/0, so the recovery has
    /// a real target to hit.
    fn target_for(indices: &[u16]) -> String {
        let entropy = indices_to_entropy(indices, WordCount::W12).unwrap();
        let mut m = String::new();
        entropy_to_mnemonic(&entropy, WordCount::W12, &mut m);
        let mut pb = Pbkdf2Ctx::new();
        let mut seed = [0u8; 64];
        pb.seed(&m, "", &mut seed);
        let secp = secp256k1::Secp256k1::signing_only();
        let master = Node::master(&seed).unwrap();
        let mut buf = [0u8; 37];
        let chain = external_chain(&secp, &master, 84, &mut buf).unwrap();
        let pk = chain.child_pubkey(&secp, 0, &mut buf).unwrap();
        let hashes = script_hashes(&pk);
        crate::address::encode(Kind::P2wpkh, &hashes[Kind::P2wpkh as usize])
    }

    #[test]
    fn recovers_a_missing_last_word() {
        let full = abandon_12();
        let target = target_for(&full);

        let mut words: Vec<String> = full
            .iter()
            .map(|&i| wordlist()[i as usize].to_string())
            .collect();
        *words.last_mut().unwrap() = "?".into();

        let plan = Plan::new(&words.join(" "), &target, Mode::Missing, 2).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        let found = plan.run(&cancel, &counter).expect("word not recovered");
        assert_eq!(found.mnemonic.split_whitespace().last().unwrap(), "about");
    }

    #[test]
    fn recovers_a_typo() {
        let full = abandon_12();
        let target = target_for(&full);
        let mut words: Vec<String> = full
            .iter()
            .map(|&i| wordlist()[i as usize].to_string())
            .collect();
        // Break the third word.
        words[2] = "zebra".into();

        let plan = Plan::new(&words.join(" "), &target, Mode::Typo, 2).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        assert!(plan.run(&cancel, &counter).is_some(), "typo not corrected");
    }

    #[test]
    fn recovers_a_swap() {
        let full = abandon_12();
        let target = target_for(&full);
        let mut words: Vec<String> = full
            .iter()
            .map(|&i| wordlist()[i as usize].to_string())
            .collect();
        words.swap(4, 5);

        let plan = Plan::new(&words.join(" "), &target, Mode::Swap, 2).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        assert!(plan.run(&cancel, &counter).is_some(), "swap not undone");
    }

    fn err_of(r: Result<Plan, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    #[test]
    fn wrong_length_is_rejected() {
        let e = err_of(Plan::new("abandon abandon", "bc1qxyz", Mode::Missing, 2));
        assert!(e.contains("Wörter"), "{e}");
    }

    #[test]
    fn counts_the_space() {
        let words =
            "? ? abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        // Two blanks, before checksum: 2048 squared.
        let plan = Plan::new(words, &target_for(&abandon_12()), Mode::Missing, 2).unwrap();
        assert_eq!(plan.candidate_count(), 2048 * 2048);
    }

    #[test]
    fn unknown_word_is_named() {
        let words = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon zzzznotaword";
        let e = err_of(Plan::new(words, "bc1qxyz", Mode::Missing, 2));
        assert!(e.contains("BIP-39"), "{e}");
    }
}
