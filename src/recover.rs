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
//! No size is refused for being hopeless. Two missing words are four million
//! combinations; four are seventeen trillion, which nobody will finish — and
//! the screen says exactly that and starts anyway, because what a hopeless
//! search looks like is this program's whole subject. The one hard limit is on
//! a *run* of moved words, and it is about memory rather than odds: their
//! orderings are materialised, so k! has to stay small.
//!
//! Without a target address the search does not merely list possibilities. It
//! tests every candidate against the same funded-address set the main search
//! uses, and a seed that owns money is recorded exactly as a collider hit is —
//! on disk, then on screen, then out to the alert channels.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::address::{decode, script_hashes, Kind};
use crate::bip32::Node;
use crate::bip39::{indices_to_entropy, wordlist, Pbkdf2Ctx, WordCount};
use crate::deriver::external_chain;

/// Past this many candidates the search is no longer a recovery in any useful
/// sense — it is the collider again, with the same answer.
///
/// It is a **warning**, not a refusal. Somebody who has lost four words and
/// wants to watch the machine try anyway is entitled to; the screen says
/// plainly what the odds are and then does as it is told.
pub const HOPELESS_ABOVE: u64 = 20_000_000_000;

/// A single run of "moved" words may be at most this long.
///
/// This one *is* a refusal, and it is about memory rather than time: the
/// orderings of a run are materialised into a list, so a run of k words costs
/// k! vectors. Eight is 40 320 of them and costs nothing; twelve would be 479
/// million and would take the process down before the search began.
pub const MAX_MOVED_RUN: usize = 8;

/// After this many candidates a worker reports its progress and looks at the
/// cancel flag.
///
/// # Why this exists at all
///
/// It used to be 2048, and the progress it reported went into a **private**
/// atomic that the interfaces never saw: the counter they were handed got a
/// single value written to it after every worker had already finished. Both the
/// window and the terminal could therefore only ever display `0 / N`, which
/// looks exactly like a hung program — and a search that says it has done
/// nothing for a minute is one that gets killed.
///
/// 256 rather than 2048 for two reasons. A round of 2048 candidates is roughly
/// half a second of work, which is too coarse for a bar that should visibly
/// move; and on a search over a single unknown word — 2048 candidates split
/// across several workers — no worker ever reached the threshold, so the whole
/// run reported nothing until it ended. One `fetch_add` per 256 candidates is
/// nothing against the PBKDF2 rounds between them.
///
/// Cancellation rides along in the same branch and got sixteen times more
/// responsive for free.
pub const REPORT_EVERY: u64 = 256;

/// How many checksum-valid seeds a targetless run will list.
///
/// Only meaningful for a small space. Past this many the list would be no use
/// to anybody, so the run hunts the funded set instead of listing — which is
/// what made the address optional.
pub const MAX_LISTED: u64 = 30;

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
        let mut too_long: Option<usize> = None;
        let flush = |run: &mut Vec<usize>, swaps: &mut Vec<Vec<usize>>, bad: &mut Option<usize>| {
            if run.len() > MAX_MOVED_RUN {
                *bad = Some(run.len());
            } else if run.len() >= 2 {
                swaps.push(run.clone());
            }
            run.clear();
        };
        for (i, st) in states.iter().enumerate() {
            let is_run = *st == State::Moved && base[i].is_some();
            if is_run {
                run.push(i);
            } else {
                flush(&mut run, &mut swaps, &mut too_long);
            }
        }
        flush(&mut run, &mut swaps, &mut too_long);

        // Refused rather than warned about: this one would exhaust memory
        // before the search started. See [`MAX_MOVED_RUN`].
        if let Some(n) = too_long {
            return Err(format!(
                "{n} verrutschte Wörter am Stück sind zu viele — höchstens {MAX_MOVED_RUN}. \
                 Markiere ein paar davon lieber als „unsicher“."
            ));
        }

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

    /// Roughly how many candidates will pass the checksum. Without a target
    /// address this decides whether they can usefully be listed or whether the
    /// run should hunt the funded set instead.
    pub fn expected_valid(&self) -> u64 {
        let bits = self.word_count.checksum_bits();
        (self.candidate_count() >> bits).max(1)
    }
}

fn factorial(n: u64) -> u64 {
    (1..=n).product::<u64>().max(1)
}

/// A recovery job: a [`Layout`] and, optionally, the address it must produce.
///
/// With a target the search returns the one seed that owns it. Without one it
/// either lists the possibilities, when there are few enough to read, or hunts
/// the funded set for a seed that owns money.
pub struct Plan {
    base: Vec<Option<u16>>,
    swaps: Vec<Vec<usize>>,
    pub word_count: WordCount,
    target: Option<(Kind, [u8; 20])>,
    pub depth: u32,
    /// Present when no target address was given and the main search's funded
    /// set is available: then every candidate seed is tested against it, and a
    /// seed that owns money is recorded exactly as a collider hit would be.
    ///
    /// This is what makes the address optional. Without a target the old code
    /// could only list checksum-valid seeds, which is useless past a handful —
    /// so it demanded an address. Hunting the funded set instead gives a
    /// targetless search something real to find at any size.
    hunt: Option<Arc<crate::engine::Shared>>,
    /// How many workers to split the walk across.
    pub threads: usize,
}

/// A found seed. Without a target address, `address` is the wallet's first
/// native-SegWit receive address, shown so the owner can recognise it.
#[derive(Clone)]
pub struct Found {
    pub mnemonic: String,
    pub address: String,
    pub path: String,
    /// Set when this seed was found because one of its addresses holds money.
    /// `None` means it is merely a checksum-valid possibility being listed.
    pub balance_sats: Option<u64>,
}

impl Found {
    /// True for a seed that owns something, as opposed to one that is only
    /// arithmetically possible. These are saved and alerted on; the others are
    /// a list to read.
    pub fn is_funded(&self) -> bool {
        self.balance_sats.is_some()
    }
}

/// The result of a run: the seeds found, and whether the list was cut short.
pub struct Outcome {
    pub hits: Vec<Found>,
    /// Set when, without an address, more valid seeds existed than were listed.
    pub truncated: bool,
}

/// A throwaway seed for a practice run: random, checksum-valid, and carrying its
/// own first address.
///
/// This screen is the one place in the program where somebody types their real
/// seed, which is exactly why nobody tries it out first — you would have to put
/// the real words in to see how it behaves. So the form can roll an invented one
/// instead, and the whole four-step walk can be rehearsed on a wallet that
/// belongs to nobody.
pub struct Practice {
    /// All the words, checksum-valid, space-separated.
    pub mnemonic: String,
    /// `m/84'/0'/0'/0/0` of that seed, so the search has a target to hit.
    pub address: String,
    /// Which word the form should leave open, counted from zero.
    pub gap: usize,
}

/// Rolls a practice seed, or `None` if the OS entropy source refuses.
///
/// # Why the address comes along
///
/// A practice seed with every word filled in is useless: [`Plan::with_hunt`]
/// refuses a complete layout, because there is genuinely nothing to search. So
/// one word has to stay open — and one open word is 2048 candidates, of which
/// roughly a hundred pass the checksum. That is past [`MAX_LISTED`], so without
/// a target the run would hunt the funded set, find nothing there, and end on
/// "nothing found". Correct, and a terrible demonstration.
///
/// With the address, the same run has exactly one answer: the seed that was
/// rolled. That is the thing worth showing somebody. It also happens to be the
/// safe path — a target address switches the funded-set hunt off
/// ([`Plan::with_hunt`]), so a practice run cannot write to `hits.jsonl`.
///
/// # No new cryptography
///
/// [`Deriver::stretch`](crate::deriver::Deriver::stretch) is the collider's own
/// hot path: it turns entropy into a checksum-valid word list and the stretched
/// seed in one call. `walk` then produces the same three addresses the search
/// checks, and the native-SegWit one is kept.
pub fn roll_practice(wc: WordCount) -> Option<Practice> {
    // 32 bytes covers the longest mnemonic; the 33rd picks the gap.
    let mut raw = [0u8; 33];
    // A button press must not take the window down, so a failure here is a
    // `None` the caller can put on screen — the same choice `config::suggest_topic`
    // makes. The collider's own draw panics instead, and rightly: there a
    // degraded source would silently invalidate the whole run.
    if getrandom::getrandom(&mut raw).is_err() {
        return None;
    }

    let mut deriver = crate::deriver::Deriver::new();
    deriver.stretch(&raw[..wc.entropy_bytes()], wc);
    let mnemonic = deriver.mnemonic().to_string();

    let mut address = None;
    deriver.walk(1, |hash, origin| {
        if origin.kind() == Kind::P2wpkh {
            address = Some(crate::address::encode(Kind::P2wpkh, hash));
        }
    });

    Some(Practice {
        mnemonic,
        address: address?,
        // Modulo over a byte is very slightly biased towards the low positions.
        // For choosing which word of twelve to blank that is beneath noticing.
        gap: raw[32] as usize % wc.words(),
    })
}

impl Plan {
    /// Combines a layout with an optional target address (empty string = none).
    ///
    /// Nothing here refuses a search for being large. A space of any size is
    /// allowed to run; how long it would take is the caller's to show, and the
    /// answer for a hopeless one is this program's whole subject.
    pub fn new(layout: Layout, target: &str, depth: u32) -> Result<Plan, String> {
        Plan::with_hunt(layout, target, depth, None, 1)
    }

    /// The full form: a funded set to hunt through when no address is given,
    /// and how many workers to use.
    pub fn with_hunt(
        layout: Layout,
        target: &str,
        depth: u32,
        hunt: Option<Arc<crate::engine::Shared>>,
        threads: usize,
    ) -> Result<Plan, String> {
        if layout.candidate_count() <= 1 {
            return Err(
                "hier ist nichts zu suchen — markiere mindestens ein Wort als \
                        unbekannt, unsicher oder verrutscht"
                    .into(),
            );
        }

        let trimmed = target.trim();
        let target = if trimmed.is_empty() {
            None
        } else {
            Some(decode(trimmed).ok_or("die Zieladresse ist keine gültige Bitcoin-Adresse")?)
        };

        Ok(Plan {
            base: layout.base,
            swaps: layout.swaps,
            word_count: layout.word_count,
            target,
            depth,
            hunt: if target.is_none() { hunt } else { None },
            threads: threads.max(1),
        })
    }

    /// True when this run tests candidates against the funded set rather than
    /// listing them.
    pub fn hunts_for_money(&self) -> bool {
        self.hunt.is_some()
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
    /// `counter` and stopping when `cancel` is set.
    ///
    /// With a target address the outcome holds the one matching seed, if any.
    /// Without, it holds every checksum-valid seed up to [`MAX_LISTED`].
    pub fn run(&self, cancel: &Arc<AtomicBool>, counter: &Arc<AtomicU64>) -> Outcome {
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

        // Split across workers by the outermost dimension: worker t takes
        // every t-th value of it. Even shares without any coordination, and a
        // one-worker run walks exactly what it always did.
        let outer = match dims.first() {
            Some(Dim::Free(_)) => 2048,
            Some(Dim::Swap(_, orders)) => orders.len(),
            None => 1,
        };
        let workers = self.threads.max(1).min(outer.max(1));

        let hits = std::sync::Mutex::new(Vec::<Found>::new());
        let want_all = self.target.is_none() && !self.hunts_for_money();

        std::thread::scope(|scope| {
            for t in 0..workers {
                let counter = Arc::clone(counter);
                let hits = &hits;
                let dims = &dims;
                scope.spawn(move || {
                    let mut engine = Engine::new(self);
                    let mut buf: Vec<u16> = self.base.iter().map(|w| w.unwrap_or(0)).collect();
                    let mut local = 0u64;

                    let mut check = |indices: &[u16]| -> bool {
                        local += 1;
                        if local.is_multiple_of(REPORT_EVERY) {
                            counter.fetch_add(local, Ordering::Relaxed);
                            local = 0;
                            if cancel.load(Ordering::Relaxed) {
                                return true;
                            }
                        }
                        if let Some(f) = engine.try_candidate(indices) {
                            let mut h = hits.lock().unwrap();
                            h.push(f);
                            // A target search wants the one answer and stops.
                            // A hunt runs on: a second funded seed is not less
                            // interesting than the first. Listing stops at the
                            // cap, one over, so the caller can say "and more".
                            if !want_all {
                                return self.target.is_some();
                            }
                            if h.len() as u64 > MAX_LISTED {
                                return true;
                            }
                        }
                        false
                    };
                    shard(dims, t, workers, &mut buf, &mut check);
                    // Der Rest unter der Meldeschwelle, damit die Summe am Ende
                    // wirklich der abgelaufenen Anzahl entspricht.
                    counter.fetch_add(local, Ordering::Relaxed);
                });
            }
        });

        let mut hits = hits.into_inner().unwrap();

        // One over the cap means "there were more"; trim it back to the cap.
        let truncated = want_all && hits.len() as u64 > MAX_LISTED;
        if truncated {
            hits.truncate(MAX_LISTED as usize);
        }
        // Money first: on a hunt that turns up both, the funded seed is the
        // answer and the rest is arithmetic.
        hits.sort_by_key(|f| std::cmp::Reverse(f.balance_sats.unwrap_or(0)));
        Outcome { hits, truncated }
    }
}

/// Walks worker `t` of `n`'s share: every n-th value of the outermost
/// dimension, and all of every dimension under it.
fn shard(
    dims: &[Dim],
    t: usize,
    n: usize,
    buf: &mut [u16],
    check: &mut impl FnMut(&[u16]) -> bool,
) {
    match dims.first() {
        None => {
            check(buf);
        }
        Some(Dim::Free(pos)) => {
            let mut w = t as u16;
            while (w as usize) < 2048 {
                buf[*pos] = w;
                if recurse(&dims[1..], 0, buf, check) {
                    return;
                }
                w += n as u16;
            }
        }
        Some(Dim::Swap(positions, orderings)) => {
            let mut i = t;
            while i < orderings.len() {
                for (slot, &val) in positions.iter().zip(&orderings[i]) {
                    buf[*slot] = val;
                }
                if recurse(&dims[1..], 0, buf, check) {
                    return;
                }
                i += n;
            }
        }
    }
}

/// Walks the search dimensions, calling `check` at each full combination.
/// `check` returns true to stop the whole walk.
fn recurse(
    dims: &[Dim],
    depth: usize,
    buf: &mut [u16],
    check: &mut impl FnMut(&[u16]) -> bool,
) -> bool {
    if depth == dims.len() {
        return check(buf);
    }
    match &dims[depth] {
        Dim::Free(pos) => {
            for w in 0..2048u16 {
                buf[*pos] = w;
                if recurse(dims, depth + 1, buf, check) {
                    return true;
                }
            }
        }
        Dim::Swap(positions, orderings) => {
            for order in orderings {
                for (slot, &val) in positions.iter().zip(order) {
                    buf[*slot] = val;
                }
                if recurse(dims, depth + 1, buf, check) {
                    return true;
                }
            }
        }
    }
    false
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
    /// Built on first use, and only on a hunt: the main search's derivation
    /// state, so a funded seed is found and recorded the same way there as
    /// here.
    deriver: Option<crate::deriver::Deriver>,
    /// Only the BIP purpose whose script kind matches the target — a P2WPKH
    /// address can never come from a P2PKH derivation.
    purposes: Vec<u32>,
}

/// BIP-44/49/84 purposes paired with the script kind each produces.
const PURPOSES: [(u32, Kind); 3] = [(44, Kind::P2pkh), (49, Kind::P2sh), (84, Kind::P2wpkh)];

impl<'p> Engine<'p> {
    fn new(plan: &'p Plan) -> Engine<'p> {
        // With a target, only the purpose that produces its script kind can
        // match. Without one, try all three and report the modern (BIP-84)
        // receive address for the owner to recognise.
        let purposes: Vec<u32> = match plan.target {
            Some((kind, _)) => PURPOSES
                .iter()
                .filter(|(_, k)| *k == kind)
                .map(|(p, _)| *p)
                .collect(),
            None => vec![84],
        };
        Engine {
            plan,
            pbkdf2: Pbkdf2Ctx::new(),
            secp: secp256k1::Secp256k1::signing_only(),
            mnemonic: String::with_capacity(24 * 9),
            seed: [0u8; 64],
            buf: [0u8; 37],
            deriver: None,
            purposes,
        }
    }

    /// Checks one candidate: checksum first, then derivation.
    ///
    /// With a target, returns the seed only if some derived address matches it.
    /// On a hunt, only if one of its addresses holds money. Otherwise every
    /// checksum-valid seed is a hit, tagged with its first native-SegWit
    /// address.
    fn try_candidate(&mut self, indices: &[u16]) -> Option<Found> {
        let entropy = indices_to_entropy(indices, self.plan.word_count)?;
        if self.plan.hunt.is_some() {
            return self.hunt_candidate(&entropy);
        }

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
            let kind = PURPOSES.iter().find(|(p, _)| *p == purpose).unwrap().1;
            for i in 0..self.plan.depth {
                let pk = match chain.child_pubkey(&self.secp, i, &mut self.buf) {
                    Some(p) => p,
                    None => continue,
                };
                let hashes = script_hashes(&pk);
                let addr = &hashes[kind as usize];
                let matched = match self.plan.target {
                    Some((tk, ref t)) => tk == kind && addr == t,
                    // No target: the first address of the first purpose is the
                    // one to show; take it and stop.
                    None => i == 0,
                };
                if matched {
                    return Some(Found {
                        mnemonic: self.mnemonic.clone(),
                        address: crate::address::encode(kind, addr),
                        path: format!("m/{purpose}'/0'/0'/0/{i}"),
                        balance_sats: None,
                    });
                }
            }
        }
        None
    }

    /// One candidate against the funded set.
    ///
    /// Deliberately routed through the very [`Deriver`] and the very reporting
    /// function the main search uses, rather than through the address walk
    /// above. A seed found here is a hit in every sense — it belongs on disk,
    /// in the alert queue and in the hit list — and the ordering that makes
    /// that safe (persist, fsync, surface, only then the network) is written
    /// down in exactly one place. Reimplementing it here would be a second
    /// place for it to drift.
    fn hunt_candidate(&mut self, entropy: &[u8]) -> Option<Found> {
        let shared = self.plan.hunt.as_ref()?.clone();
        let deriver = self
            .deriver
            .get_or_insert_with(crate::deriver::Deriver::new);
        deriver.stretch(entropy, self.plan.word_count);

        let mut found = Vec::new();
        deriver.walk(self.plan.depth, |hash, origin| {
            if !shared.bloom.contains(origin.kind(), hash) {
                return;
            }
            shared.stats.note_bloom_hit();
            // Money only. An address that is in the set but holds nothing is
            // not something to record or to wake anyone for — see the same
            // rule in the main search's worker.
            if let Some(balance) = shared.db.lookup(origin.kind(), hash) {
                if balance > 0 {
                    found.push((*hash, origin, balance));
                }
            }
        });

        let (hash, origin, balance) = *found.first()?;
        for (h, o, b) in &found {
            crate::engine::report(&shared, deriver, entropy, h, *o, *b);
        }
        Some(Found {
            mnemonic: deriver.mnemonic().to_string(),
            address: crate::address::encode(origin.kind(), &hash),
            path: origin.path(),
            balance_sats: Some(balance),
        })
    }
}

/// Type alias so [`crate::recover_ui`] can hold a search result channel.
pub type ResultRx = Receiver<Outcome>;

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

    fn run_with(layout: Layout, target: &str) -> Outcome {
        let plan = Plan::new(layout, target, 2).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        plan.run(&cancel, &counter)
    }

    fn run(layout: Layout) -> Option<Found> {
        run_with(layout, TARGET).hits.into_iter().next()
    }

    /// Words and states for a 12-word seed with `unsure` at the given
    /// positions, counted from zero.
    fn abandon_with_gaps(gaps: &[usize]) -> (Vec<String>, Vec<State>) {
        let words = abandon_words();
        let mut states = vec![State::Sure; words.len()];
        for g in gaps {
            states[*g] = State::Unsure;
        }
        (words, states)
    }

    /// **The test that was missing.** Progress has to be readable *while* the
    /// search runs, from another thread.
    ///
    /// It was not: `run` reported into a private atomic and wrote the caller's
    /// counter exactly once, after every worker had joined. Both interfaces
    /// could only ever show `0 / N`, which is indistinguishable from a hung
    /// program — and that is what it was taken for.
    #[test]
    fn progress_is_visible_while_the_search_runs() {
        // Two unknown words: four million candidates, far more than can finish
        // inside this test, so anything the counter shows is mid-run by
        // construction.
        let (words, states) = abandon_with_gaps(&[5, 9]);
        let layout = Layout::build(&words, &states).unwrap();
        assert_eq!(layout.candidate_count(), 2048 * 2048);
        let plan = Plan::new(layout, TARGET, 1).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        let (c2, ct2) = (Arc::clone(&cancel), Arc::clone(&counter));
        let worker = std::thread::spawn(move || plan.run(&c2, &ct2));

        // Watch from outside until something is reported.
        let mut seen = 0u64;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            seen = counter.load(Ordering::Relaxed);
            if seen > 0 {
                break;
            }
        }
        cancel.store(true, Ordering::Relaxed);
        let out = worker.join().expect("worker panicked");

        assert!(
            seen > 0,
            "der Zähler stand nach zwei Sekunden noch auf 0 — genau der Fehler"
        );
        assert!(
            seen < 2048 * 2048,
            "gemeldet wurde {seen}, das wäre schon das Ende"
        );
        // Cancelled searches report what they walked, not what they found.
        assert!(out.hits.is_empty() || out.hits.len() == 1);
    }

    /// The counter must add up to the space actually walked, so a percentage
    /// built on it reaches 100 rather than stopping short — and the tail below
    /// the reporting threshold must be added too.
    ///
    /// The target is Bitcoin's genesis address, which no candidate here can
    /// derive: with a target the run stops the moment it matches, so an
    /// unreachable one is what makes it walk the whole space. A run *without* a
    /// target would not do — it lists checksum-valid seeds and stops at
    /// [`MAX_LISTED`], after a few hundred candidates.
    #[test]
    fn the_counter_ends_at_the_number_of_candidates_walked() {
        const NEVER: &str = "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";

        // Several worker counts: the split is by the outermost dimension, and
        // each worker adds its own remainder at the end.
        for threads in [1usize, 3, 4] {
            let (words, states) = abandon_with_gaps(&[7]);
            let layout = Layout::build(&words, &states).unwrap();
            let total = layout.candidate_count();
            assert_eq!(total, 2048);
            let plan = Plan::with_hunt(layout, NEVER, 1, None, threads).unwrap();

            let cancel = Arc::new(AtomicBool::new(false));
            let counter = Arc::new(AtomicU64::new(0));
            let out = plan.run(&cancel, &counter);

            assert!(
                out.hits.is_empty(),
                "die Genesis-Adresse darf nicht auftauchen"
            );
            assert_eq!(
                counter.load(Ordering::Relaxed),
                total,
                "mit {threads} Arbeitern fehlt am Ende etwas"
            );
        }
    }

    /// The threshold has to be fine enough that even the smallest useful search
    /// reports something on the way.
    ///
    /// One unknown word is 2048 candidates. Split across the cores of an
    /// ordinary machine that is a few hundred each — under the old threshold of
    /// 2048 no worker ever reached it, so the bar sat at zero for the whole run
    /// and then disappeared.
    #[test]
    fn the_report_threshold_is_fine_enough_for_a_small_search() {
        let smallest_useful = 2048u64;
        let plausible_cores = 8u64;
        assert!(
            REPORT_EVERY * plausible_cores <= smallest_useful,
            "Meldeschwelle {REPORT_EVERY} × {plausible_cores} Kerne passt nicht in \
             {smallest_useful} Kandidaten — der Balken bliebe bei null"
        );
    }

    /// A practice seed has to be a real one. If the checksum were wrong the
    /// search would walk the whole space and come back empty, and the person
    /// trying the screen out would conclude the feature is broken.
    #[test]
    fn a_rolled_practice_seed_is_checksum_valid() {
        for wc in crate::bip39::ALL_WORD_COUNTS {
            let p = roll_practice(wc).expect("OS entropy");
            let words: Vec<&str> = p.mnemonic.split_whitespace().collect();
            assert_eq!(words.len(), wc.words(), "falsche Länge für {wc:?}");

            let indices: Vec<u16> = words
                .iter()
                .map(|w| word_index(w).unwrap_or_else(|| panic!("{w} ist kein BIP-39-Wort")))
                .collect();
            // Returns None precisely when the checksum does not hold.
            assert!(
                indices_to_entropy(&indices, wc).is_some(),
                "Prüfsumme stimmt nicht für {wc:?}"
            );
            assert!(p.gap < wc.words(), "Lücke {} außerhalb", p.gap);
        }
    }

    /// The address has to belong to the seed it came with. Otherwise the practice
    /// run searches for a wallet that is not there — the one failure that would
    /// look exactly like a broken recovery.
    #[test]
    fn a_rolled_address_belongs_to_its_own_seed() {
        let p = roll_practice(WordCount::W12).expect("OS entropy");

        // Derive again from the words alone, the long way round.
        let words: Vec<String> = p.mnemonic.split_whitespace().map(str::to_string).collect();
        let indices: Vec<u16> = words.iter().map(|w| word_index(w).unwrap()).collect();
        let entropy = indices_to_entropy(&indices, WordCount::W12).expect("gültige Prüfsumme");

        let mut d = crate::deriver::Deriver::new();
        d.stretch(&entropy, WordCount::W12);
        assert_eq!(
            d.mnemonic(),
            p.mnemonic,
            "andere Wörter aus derselben Entropie"
        );

        let mut again = None;
        d.walk(1, |hash, origin| {
            if origin.kind() == Kind::P2wpkh {
                again = Some(crate::address::encode(Kind::P2wpkh, hash));
            }
        });
        assert_eq!(again.as_deref(), Some(p.address.as_str()));
        assert!(p.address.starts_with("bc1q"), "{}", p.address);
    }

    /// Two rolls must not give the same seed — a die that always shows the same
    /// face is not a die.
    #[test]
    fn two_rolls_differ() {
        let a = roll_practice(WordCount::W12).expect("OS entropy");
        let b = roll_practice(WordCount::W12).expect("OS entropy");
        assert_ne!(a.mnemonic, b.mnemonic);
        assert_ne!(a.address, b.address);
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
    fn without_an_address_it_lists_the_valid_seeds() {
        // One missing word in a 24-word seed: 8 checksum-valid candidates.
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 32], WordCount::W24, &mut m);
        let mut words: Vec<String> = m.split_whitespace().map(str::to_string).collect();
        let mut states = vec![State::Sure; 24];
        words[23].clear();
        states[23] = State::Unsure;

        let layout = Layout::build(&words, &states).unwrap();
        let out = run_with(layout, "");
        assert!(!out.hits.is_empty(), "no candidates listed");
        assert!(out.hits.len() <= MAX_LISTED as usize);
        // The real seed (…, "art") must be among them.
        assert!(out
            .hits
            .iter()
            .any(|f| f.mnemonic.split_whitespace().last() == Some("art")));
        // Every listing carries a first address to compare against.
        assert!(out.hits.iter().all(|f| f.address.starts_with("bc1")));
    }

    /// The address is optional whatever the size. Two missing words are far
    /// more than can be listed, and used to be refused outright for it; now
    /// they simply run without a target.
    #[test]
    fn without_an_address_any_size_is_allowed() {
        let words = abandon_words();
        let mut states = vec![State::Sure; 12];
        states[0] = State::Unsure;
        states[1] = State::Unsure;
        let layout = Layout::build(&words, &states).unwrap();
        let plan = Plan::new(layout, "", 2).expect("no address must be fine at any size");
        assert_eq!(plan.candidate_count(), 2048 * 2048);
        assert!(
            !plan.hunts_for_money(),
            "with no funded set handed in it can only list"
        );
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

    /// Splitting the walk across workers must not lose a single candidate.
    ///
    /// The failure this guards against is the quiet one: a shard that skips
    /// part of the space would still finish, still report, and simply never
    /// mention the seed somebody was trying to get back. So the same search is
    /// run at one, two, three and five workers and the answers must be
    /// identical — including at counts that do not divide 2048 evenly.
    #[test]
    fn every_worker_count_walks_the_same_space() {
        let mut words: Vec<String> = Vec::new();
        let mut m = String::new();
        crate::bip39::entropy_to_mnemonic(&[0u8; 32], WordCount::W24, &mut m);
        for w in m.split_whitespace() {
            words.push(w.to_string());
        }
        let mut states = vec![State::Sure; 24];
        states[23] = State::Unsure;

        let seeds_at = |threads: usize| -> Vec<String> {
            let layout = Layout::build(&words, &states).unwrap();
            let plan = Plan::with_hunt(layout, "", 1, None, threads).unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let counter = Arc::new(AtomicU64::new(0));
            let mut got: Vec<String> = plan
                .run(&cancel, &counter)
                .hits
                .into_iter()
                .map(|f| f.mnemonic)
                .collect();
            got.sort();
            got
        };

        let one = seeds_at(1);
        assert!(
            !one.is_empty(),
            "the single-threaded run must find something"
        );
        for n in [2, 3, 5, 8] {
            assert_eq!(
                seeds_at(n),
                one,
                "{n} workers must find exactly what one worker finds"
            );
        }
    }

    /// The whole reason the address could be made optional: with no target,
    /// the search tests every candidate against the funded set and a seed that
    /// owns money comes back with its balance — and lands on disk on the way.
    ///
    /// Built end to end: a database with this seed's addresses planted in it,
    /// a real Bloom filter, a real hit writer. One word is blanked out, no
    /// address is given, and the run has to find its way back.
    #[test]
    fn a_targetless_run_finds_a_funded_seed_and_records_it() {
        use crate::alert::Dispatcher;
        use crate::deriver::Deriver;
        use crate::hits::HitWriter;
        use crate::lookup::{Database, Record};
        use crate::stats::{Control, Priority, Stats};

        let dir = std::env::temp_dir().join(format!("sc-hunt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("funded.scdb");
        let hits_path = dir.join("hits.jsonl");

        // A database of noise, with this one wallet's addresses planted in it.
        const PLANTED: u64 = 133_700_000;
        let mut records = crate::lookup::synthetic_records(2_000).unwrap();
        let mut d = Deriver::new();
        d.stretch(&[0u8; 16], WordCount::W12);
        d.walk(4, |hash, origin| {
            records.push(Record::new(origin.kind(), hash, PLANTED));
        });
        crate::lookup::write_database(&db_path, records).unwrap();

        let db = Database::open(&db_path).unwrap();
        let bloom = db.build_bloom(0.0001);
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(crate::engine::Shared {
            stats: Arc::new(Stats::new()),
            control: Arc::new(Control::new(1, 4, Priority::Normal)),
            bloom: Arc::new(bloom),
            db: Arc::new(db),
            writer: Arc::new(HitWriter::new(hits_path.clone(), None)),
            dispatcher: Arc::new(Dispatcher::new(
                Vec::new(),
                dir.join("pending.jsonl"),
                1,
                std::time::Duration::from_secs(60),
            )),
            events: tx,
            word_count: WordCount::W12,
        });

        // The seed with its last word missing, and no address to aim at.
        let words = abandon_words();
        let mut states = vec![State::Sure; 12];
        states[11] = State::Unsure;
        let layout = Layout::build(&words, &states).unwrap();
        let plan = Plan::with_hunt(layout, "", 4, Some(shared), 2).unwrap();
        assert!(plan.hunts_for_money());

        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        let out = plan.run(&cancel, &counter);

        let f = out.hits.first().expect("the funded seed must be found");
        assert!(f.is_funded(), "and must be reported as funded");
        assert_eq!(f.balance_sats, Some(PLANTED));
        assert_eq!(
            f.mnemonic.split_whitespace().last(),
            Some("about"),
            "the missing word must be filled back in"
        );

        // Invariant: on disk before anything else. The file must hold it.
        let saved = std::fs::read_to_string(&hits_path).unwrap();
        assert!(saved.contains(&f.address), "the hit must be persisted");
        assert!(
            saved.contains("about"),
            "and the words with it — that is what makes it worth anything"
        );
        // And it must have been surfaced to the window.
        assert!(
            rx.try_recv().is_ok(),
            "the hit must reach the event channel"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A wallet in the set that holds nothing is not a find.
    ///
    /// Dumps are full of addresses that were funded once and swept years ago.
    /// Reporting those would put noise in hits.jsonl and send an alert for a
    /// balance of zero, which is worse than saying nothing: it teaches the
    /// owner to ignore the alerts that matter.
    #[test]
    fn an_empty_wallet_is_neither_recorded_nor_reported() {
        use crate::alert::Dispatcher;
        use crate::deriver::Deriver;
        use crate::hits::HitWriter;
        use crate::lookup::{Database, Record};
        use crate::stats::{Control, Priority, Stats};

        let dir = std::env::temp_dir().join(format!("sc-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("funded.scdb");
        let hits_path = dir.join("hits.jsonl");

        // The same wallet as the funded test, planted with a balance of zero.
        let mut records = crate::lookup::synthetic_records(2_000).unwrap();
        let mut d = Deriver::new();
        d.stretch(&[0u8; 16], WordCount::W12);
        d.walk(4, |hash, origin| {
            records.push(Record::new(origin.kind(), hash, 0));
        });
        crate::lookup::write_database(&db_path, records).unwrap();

        let db = Database::open(&db_path).unwrap();
        let bloom = db.build_bloom(0.0001);
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(crate::engine::Shared {
            stats: Arc::new(Stats::new()),
            control: Arc::new(Control::new(1, 4, Priority::Normal)),
            bloom: Arc::new(bloom),
            db: Arc::new(db),
            writer: Arc::new(HitWriter::new(hits_path.clone(), None)),
            dispatcher: Arc::new(Dispatcher::new(
                Vec::new(),
                dir.join("pending.jsonl"),
                1,
                std::time::Duration::from_secs(60),
            )),
            events: tx,
            word_count: WordCount::W12,
        });

        let words = abandon_words();
        let mut states = vec![State::Sure; 12];
        states[11] = State::Unsure;
        let layout = Layout::build(&words, &states).unwrap();
        let plan = Plan::with_hunt(layout, "", 4, Some(shared), 1).unwrap();

        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        let out = plan.run(&cancel, &counter);

        assert!(
            out.hits.is_empty(),
            "an empty wallet must not be reported as a find"
        );
        assert!(
            !hits_path.exists()
                || std::fs::read_to_string(&hits_path)
                    .unwrap()
                    .trim()
                    .is_empty(),
            "and nothing may be written to the hit file"
        );
        assert!(rx.try_recv().is_err(), "and nothing sent to the window");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An absurd space is allowed to start. It will not finish, and the screen
    /// says so, but refusing was the wrong answer for a program whose whole
    /// subject is what a hopeless search looks like.
    #[test]
    fn an_oversized_space_is_allowed_and_flagged() {
        let words = abandon_words();
        let states = vec![State::Unsure; 12]; // 2048^12, absurd
        let layout = Layout::build(&words, &states).unwrap();
        assert!(layout.candidate_count() > HOPELESS_ABOVE);
        assert!(
            Plan::new(layout, TARGET, 2).is_ok(),
            "size alone must not refuse a plan any more"
        );
    }

    /// The one size that *is* refused, and for a different reason: the
    /// orderings of a moved run are materialised, so a long run would exhaust
    /// memory before the search began.
    #[test]
    fn an_overlong_moved_run_is_refused() {
        let words = abandon_words();
        let mut states = vec![State::Sure; 12];
        for s in states.iter_mut().take(MAX_MOVED_RUN + 1) {
            *s = State::Moved;
        }
        let err = match Layout::build(&words, &states) {
            Err(e) => e,
            Ok(_) => panic!("a run of {} must be refused", MAX_MOVED_RUN + 1),
        };
        assert!(err.contains("verrutschte"), "{err}");

        // Exactly at the limit is fine.
        let mut ok = vec![State::Sure; 12];
        for s in ok.iter_mut().take(MAX_MOVED_RUN) {
            *s = State::Moved;
        }
        assert!(
            Layout::build(&words, &ok).is_ok(),
            "the limit itself is fine"
        );
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
