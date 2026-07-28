//! Recovering *your own* seed when part of it is lost.
//!
//! The honest inverse of the collider. The main search is hopeless on purpose;
//! recovering a wallet you can prove is yours is not, because you hold most of
//! the words and the space that remains is small.
//!
//! Rather than a fixed list of failure modes, every position carries a state:
//!
//! * **Sure** — the word is right and fixed.
//! * **Unknown / unsure** — the word could be anything; all 2048 are tried. An
//!   empty field is unknown; a filled field marked unsure is the same search,
//!   the text only a reminder.
//! * **Moved** — the word is right but its place is not. Adjacent moved words
//!   form a run that is tried in every order.
//!
//! These combine: two unknown words and a swapped pair is one search. The size
//! is the product — 2048 per unknown, k! per moved run of length k — and the
//! checksum throws almost all of it away before any derivation.
//!
//! A free permutation of *all* the words is refused by the size cap: 24 words
//! rearrange 6·10²³ ways, which is collider territory again.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::address::{decode, script_hashes, Kind};
use crate::bip32::Node;
use crate::bip39::{indices_to_entropy, wordlist, Pbkdf2Ctx, WordCount};
use crate::deriver::external_chain;

/// Refuse a search larger than this many candidates. About a day at the rate a
/// laptop checks them; past it, recovery is not the right tool.
pub const MAX_CANDIDATES: u64 = 20_000_000_000;

/// The state of one word in the seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Right and fixed.
    Sure,
    /// Present but doubtful, or missing entirely; all 2048 words are tried.
    Unsure,
    /// Right, but possibly out of place; tried in every order with its
    /// adjacent moved neighbours.
    Moved,
}

/// The shape of a recovery, without a target address.
///
/// Split from [`Plan`] so the window can price the search and surface input
/// errors while the address field is still empty.
pub struct Layout {
    /// One per position: the word index, or `None` where it is unknown.
    base: Vec<Option<u16>>,
    /// Runs of adjacent Moved positions, each tried in every order.
    swaps: Vec<Vec<usize>>,
    pub word_count: WordCount,
}

impl Layout {
    /// Builds a layout from a word and a state per position.
    ///
    /// `words` and `states` must be the same length, and that length a valid
    /// mnemonic size. An empty word is Unknown whatever its state says.
    pub fn build(words: &[String], states: &[State]) -> Result<Layout, String> {
        if words.len() != states.len() {
            return Err("interner Fehler: Wörter und Zustände unterschiedlich lang".into());
        }
        let word_count = WordCount::from_words(words.len() as u8).ok_or_else(|| {
            format!(
                "eine Seed hat 12, 15, 18, 21 oder 24 Wörter, hier sind es {}",
                words.len()
            )
        })?;

        let mut base = Vec::with_capacity(words.len());
        for (i, (w, st)) in words.iter().zip(states).enumerate() {
            let trimmed = w.trim();
            if trimmed.is_empty() || *st == State::Unsure {
                base.push(None);
                continue;
            }
            let idx = crate::bip39::word_index(trimmed)
                .ok_or_else(|| format!("Wort {} („{trimmed}“) ist kein BIP-39-Wort", i + 1))?;
            base.push(Some(idx));
        }

        // Adjacent Moved positions that carry a word form a swap run. A word
        // that is empty cannot be "moved" — there is nothing to place — so it
        // falls through to Unknown above and never joins a run.
        let mut swaps = Vec::new();
        let mut run: Vec<usize> = Vec::new();
        let flush = |run: &mut Vec<usize>, swaps: &mut Vec<Vec<usize>>| {
            if run.len() >= 2 {
                swaps.push(run.clone());
            }
            run.clear();
        };
        for (i, st) in states.iter().enumerate() {
            let is_run = *st == State::Moved && base[i].is_some();
            if is_run {
                run.push(i);
            } else {
                flush(&mut run, &mut swaps);
            }
        }
        flush(&mut run, &mut swaps);

        Ok(Layout {
            base,
            swaps,
            word_count,
        })
    }

    /// Candidate count before the checksum: the size to warn on.
    pub fn candidate_count(&self) -> u64 {
        let mut n: u64 = 1;
        for slot in &self.base {
            if slot.is_none() {
                n = n.saturating_mul(2048);
            }
        }
        // A Moved position is fixed in `base`; the ordering is what varies, so
        // each swap run multiplies by its factorial and its members are not
        // also counted as free above.
        for run in &self.swaps {
            n = n.saturating_mul(factorial(run.len() as u64));
        }
        n
    }

    /// True when nothing is actually being searched — every word sure and in
    /// place. The window uses this to keep Start dead on a complete seed.
    pub fn is_trivial(&self) -> bool {
        self.candidate_count() <= 1
    }
}

fn factorial(n: u64) -> u64 {
    (1..=n).product::<u64>().max(1)
}

/// A recovery job: a [`Layout`] plus the address it must produce.
pub struct Plan {
    base: Vec<Option<u16>>,
    swaps: Vec<Vec<usize>>,
    pub word_count: WordCount,
    target: [u8; 20],
    target_kind: Kind,
    pub depth: u32,
}

/// A found seed.
#[derive(Clone)]
pub struct Found {
    pub mnemonic: String,
    pub address: String,
    pub path: String,
}

impl Plan {
    /// Combines a layout with a target address.
    pub fn new(layout: Layout, target: &str, depth: u32) -> Result<Plan, String> {
        let count = layout.candidate_count();
        if count <= 1 {
            return Err(
                "hier ist nichts zu suchen — markiere mindestens ein Wort als \
                        unbekannt, unsicher oder verrutscht"
                    .into(),
            );
        }
        if count > MAX_CANDIDATES {
            return Err(format!(
                "der Suchraum ist zu groß ({} Kombinationen). Markiere weniger Wörter \
                 als unbekannt.",
                crate::util::group_digits(count)
            ));
        }
        let (target_kind, target) =
            decode(target.trim()).ok_or("die Zieladresse ist keine gültige Bitcoin-Adresse")?;

        Ok(Plan {
            base: layout.base,
            swaps: layout.swaps,
            word_count: layout.word_count,
            target,
            target_kind,
            depth,
        })
    }

    pub fn candidate_count(&self) -> u64 {
        Layout {
            base: self.base.clone(),
            swaps: self.swaps.clone(),
            word_count: self.word_count,
        }
        .candidate_count()
    }

    /// Seconds the search is expected to take. See [`estimate_secs`].
    pub fn estimate_secs(&self) -> f64 {
        estimate_secs(self.candidate_count(), self.word_count)
    }

    /// Runs the search on the calling thread, reporting progress through
    /// `counter` and stopping when `cancel` is set. Returns the first seed
    /// that hits the target, or `None`.
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

        // The search dimensions: each free position, then each swap run with
        // its precomputed orderings. A run's positions carry their base word.
        let mut dims = Vec::new();
        for (i, slot) in self.base.iter().enumerate() {
            if slot.is_none() && !self.swaps.iter().any(|r| r.contains(&i)) {
                dims.push(Dim::Free(i));
            }
        }
        for run in &self.swaps {
            let vals: Vec<u16> = run.iter().map(|&i| self.base[i].unwrap()).collect();
            dims.push(Dim::Swap(run.clone(), permutations(&vals)));
        }

        let mut buf: Vec<u16> = self.base.iter().map(|w| w.unwrap_or(0)).collect();
        recurse(&dims, 0, &mut buf, cancel, &mut check)
    }
}

/// Walks the search dimensions, calling `check` at each full combination.
fn recurse(
    dims: &[Dim],
    depth: usize,
    buf: &mut [u16],
    cancel: &Arc<AtomicBool>,
    check: &mut impl FnMut(&[u16]) -> Option<Found>,
) -> Option<Found> {
    if depth == dims.len() {
        return check(buf);
    }
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    match &dims[depth] {
        Dim::Free(pos) => {
            for w in 0..2048u16 {
                buf[*pos] = w;
                if let Some(f) = recurse(dims, depth + 1, buf, cancel, check) {
                    return Some(f);
                }
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
            }
        }
        Dim::Swap(positions, orderings) => {
            for order in orderings {
                for (slot, &val) in positions.iter().zip(order) {
                    buf[*slot] = val;
                }
                if let Some(f) = recurse(dims, depth + 1, buf, cancel, check) {
                    return Some(f);
                }
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
            }
        }
    }
    None
}

/// The search dimensions of a run, used only inside [`Plan::run`].
enum Dim {
    Free(usize),
    Swap(Vec<usize>, Vec<Vec<u16>>),
}

/// Every ordering of a slice. Heap's algorithm; fine for the small runs a
/// human marks by hand (the size cap keeps a run's factorial in check).
fn permutations(items: &[u16]) -> Vec<Vec<u16>> {
    let mut result = Vec::new();
    let mut a = items.to_vec();
    let n = a.len();
    let mut c = vec![0usize; n];
    result.push(a.clone());
    let mut i = 0;
    while i < n {
        if c[i] < i {
            if i % 2 == 0 {
                a.swap(0, i);
            } else {
                a.swap(c[i], i);
            }
            result.push(a.clone());
            c[i] += 1;
            i = 0;
        } else {
            c[i] = 0;
            i += 1;
        }
    }
    result
}

/// Seconds a search of `candidates` mnemonics is expected to take.
///
/// The checksum-fail path is a single SHA-256; the fraction that pass add
/// PBKDF2 and derivation. Both measured on an M1, and the estimate is
/// deliberately high — better to promise ten minutes and finish in two.
pub fn estimate_secs(candidates: u64, wc: WordCount) -> f64 {
    const CHEAP_PER: f64 = 0.09e-6;
    const DERIVE_PER: f64 = 2.5e-3;
    let total = candidates as f64;
    let pass_fraction = 1.0 / 2f64.powi(wc.checksum_bits() as i32);
    total * CHEAP_PER + total * pass_fraction * DERIVE_PER
}

/// Per-run derivation state, so the candidate loop allocates nothing.
struct Engine<'p> {
    plan: &'p Plan,
    pbkdf2: Pbkdf2Ctx,
    secp: secp256k1::Secp256k1<secp256k1::SignOnly>,
    mnemonic: String,
    seed: [u8; 64],
    buf: [u8; 37],
    /// Only the BIP purpose whose script kind matches the target — a P2WPKH
    /// address can never come from a P2PKH derivation.
    purposes: Vec<u32>,
}

/// BIP-44/49/84 purposes paired with the script kind each produces.
const PURPOSES: [(u32, Kind); 3] = [(44, Kind::P2pkh), (49, Kind::P2sh), (84, Kind::P2wpkh)];

impl<'p> Engine<'p> {
    fn new(plan: &'p Plan) -> Engine<'p> {
        let purposes = PURPOSES
            .iter()
            .filter(|(_, kind)| *kind == plan.target_kind)
            .map(|(purpose, _)| *purpose)
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

        for &purpose in &self.purposes {
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
                        address: crate::address::encode(self.plan.target_kind, &self.plan.target),
                        path: format!("m/{purpose}'/0'/0'/0/{i}"),
                    });
                }
            }
        }
        None
    }
}

/// Type alias so [`crate::recover_ui`] can hold a search result channel.
pub type ResultRx = Receiver<Option<Found>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::{entropy_to_mnemonic, word_index};

    fn abandon_words() -> Vec<String> {
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 16], WordCount::W12, &mut m);
        m.split_whitespace().map(str::to_string).collect()
    }

    /// The address of that seed at m/84'/0'/0'/0/0.
    const TARGET: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

    fn run(layout: Layout) -> Option<Found> {
        let plan = Plan::new(layout, TARGET, 2).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        plan.run(&cancel, &counter)
    }

    #[test]
    fn recovers_an_unknown_word() {
        let mut words = abandon_words();
        let mut states = vec![State::Sure; 12];
        words[11].clear(); // last word gone
        states[11] = State::Unsure;
        let f = run(Layout::build(&words, &states).unwrap()).expect("not found");
        assert_eq!(f.mnemonic.split_whitespace().last().unwrap(), "about");
    }

    #[test]
    fn recovers_an_unsure_word() {
        let mut words = abandon_words();
        let mut states = vec![State::Sure; 12];
        words[2] = "zebra".into(); // wrong word, but present
        states[2] = State::Unsure;
        assert!(run(Layout::build(&words, &states).unwrap()).is_some());
    }

    #[test]
    fn recovers_a_swap() {
        let mut words = abandon_words();
        words.swap(4, 5);
        let mut states = vec![State::Sure; 12];
        states[4] = State::Moved;
        states[5] = State::Moved;
        assert!(run(Layout::build(&words, &states).unwrap()).is_some());
    }

    #[test]
    fn counts_combine() {
        let words = abandon_words();
        let mut states = vec![State::Sure; 12];
        states[0] = State::Unsure; // 2048 (leer)
        states[3] = State::Unsure; // 2048
        states[6] = State::Moved;
        states[7] = State::Moved; // 2! = 2
        let layout = Layout::build(&words, &states).unwrap();
        assert_eq!(layout.candidate_count(), 2048u64 * 2048 * 2);
    }

    #[test]
    fn a_single_moved_word_does_nothing() {
        let words = abandon_words();
        let mut states = vec![State::Sure; 12];
        states[3] = State::Moved; // isolated: no run, no effect
        let layout = Layout::build(&words, &states).unwrap();
        assert!(layout.is_trivial());
    }

    #[test]
    fn a_complete_seed_has_nothing_to_search() {
        let layout = Layout::build(&abandon_words(), &[State::Sure; 12]).unwrap();
        assert!(layout.is_trivial());
        let err = match Plan::new(layout, TARGET, 2) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(err.contains("nichts zu suchen"), "{err}");
    }

    #[test]
    fn an_oversized_space_is_refused() {
        let words = abandon_words();
        let states = vec![State::Unsure; 12]; // 2048^12, absurd
        let layout = Layout::build(&words, &states).unwrap();
        let err = match Plan::new(layout, TARGET, 2) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(err.contains("zu groß"), "{err}");
    }

    #[test]
    fn a_bad_word_is_named() {
        let mut words = abandon_words();
        words[5] = "zzzznotaword".into();
        let states = vec![State::Sure; 12];
        let e = match Layout::build(&words, &states) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(e.contains("BIP-39"), "{e}");
    }

    #[test]
    fn permutations_are_complete() {
        let p = permutations(&[1, 2, 3]);
        assert_eq!(p.len(), 6);
        let unique: std::collections::HashSet<_> = p.into_iter().collect();
        assert_eq!(unique.len(), 6);
    }

    #[test]
    fn word_index_still_resolves() {
        assert_eq!(word_index("about"), word_index("abou")); // 4-letter stub
    }
}
