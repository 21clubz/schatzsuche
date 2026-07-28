//! The window screen for [`crate::recover`].
//!
//! State and search plumbing only; the drawing is in `gui.rs`. The search runs
//! on its own thread and reports back through a counter, a cancel flag and a
//! one-shot channel, so the window stays responsive and cancellable.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Instant;

use crate::bip39::WordCount;
use crate::recover::{estimate_secs, Layout, Outcome, Plan, ResultRx, State, MAX_LISTED};

/// Where the recovery screen is in its flow.
pub enum Phase {
    /// Filling in the form.
    Editing,
    /// Searching, with a way to watch and stop it.
    Running {
        cancel: Arc<AtomicBool>,
        counter: Arc<AtomicU64>,
        total: u64,
        started: Instant,
        result: ResultRx,
    },
    /// Finished: the seeds found (empty if none).
    Done(Outcome),
}

/// One word slot the user edits: the text, and what they know about it.
pub struct Slot {
    pub word: String,
    pub state: State,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            word: String::new(),
            state: State::Sure,
        }
    }
}

/// Everything the recovery screen needs between frames.
pub struct RecoverUi {
    pub word_count: WordCount,
    pub slots: Vec<Slot>,
    pub address: String,
    /// The user has read the warning and accepts the risk.
    pub acknowledged: bool,
    pub phase: Phase,
}

impl Default for RecoverUi {
    fn default() -> Self {
        let mut ui = RecoverUi {
            word_count: WordCount::W12,
            slots: Vec::new(),
            address: String::new(),
            acknowledged: false,
            phase: Phase::Editing,
        };
        ui.resize(WordCount::W12);
        ui
    }
}

/// What the form currently amounts to: shown live, under the words.
pub enum Preview {
    /// Words are still all sure and present — nothing to search yet.
    Nothing,
    /// A word does not parse. The string is the reason, in plain German.
    Invalid(String),
    /// Ready: this many candidates and this rough duration.
    Ready { candidates: u64, secs: f64 },
}

impl RecoverUi {
    /// Grows or shrinks the slot list to a new mnemonic length, keeping the
    /// words already typed.
    pub fn resize(&mut self, wc: WordCount) {
        self.word_count = wc;
        self.slots.resize_with(wc.words(), Slot::default);
    }

    fn words(&self) -> Vec<String> {
        self.slots.iter().map(|s| s.word.clone()).collect()
    }

    fn states(&self) -> Vec<State> {
        self.slots.iter().map(|s| s.state).collect()
    }

    /// Re-reads the form. Cheap enough to call every frame.
    pub fn preview(&self) -> Preview {
        // A blank form is the neutral starting state, not a giant search.
        if self.slots.iter().all(|s| s.word.trim().is_empty()) {
            return Preview::Nothing;
        }
        match Layout::build(&self.words(), &self.states()) {
            Err(e) => Preview::Invalid(e),
            Ok(layout) if layout.is_trivial() => Preview::Nothing,
            Ok(layout) => {
                let candidates = layout.candidate_count();
                if candidates > crate::recover::MAX_CANDIDATES {
                    return Preview::Invalid(format!(
                        "Der Suchraum ist zu groß ({} Kombinationen). Markiere weniger \
                         Wörter als unbekannt oder unsicher.",
                        crate::util::group_digits(candidates)
                    ));
                }
                Preview::Ready {
                    candidates,
                    secs: estimate_secs(candidates, layout.word_count),
                }
            }
        }
    }

    /// How many seeds an addressless search would list, if the words are valid
    /// and searchable. `None` when there is nothing to search.
    pub fn expected_without_address(&self) -> Option<u64> {
        match Layout::build(&self.words(), &self.states()) {
            Ok(l) if !l.is_trivial() => Some(l.expected_valid()),
            _ => None,
        }
    }

    /// True when the address field must be filled: the words describe a real,
    /// runnable search, but without an address it would list too many seeds.
    /// False in the blank or oversized states, where the address is moot.
    pub fn address_required(&self) -> bool {
        matches!(self.preview(), Preview::Ready { .. })
            && self.address.trim().is_empty()
            && self
                .expected_without_address()
                .is_some_and(|n| n > MAX_LISTED)
    }

    /// Whether Start should be live: something to search, the risk
    /// acknowledged, and — only if the space is large — an address.
    pub fn can_start(&self) -> bool {
        self.acknowledged
            && matches!(self.preview(), Preview::Ready { .. })
            && !self.address_required()
    }

    /// Builds the plan and spawns the search. On a bad plan the phase stays
    /// Editing and the error comes back for display.
    pub fn start(&mut self, depth: u32) -> Result<(), String> {
        let layout = Layout::build(&self.words(), &self.states())?;
        let plan = Plan::new(layout, &self.address, depth)?;
        let total = plan.candidate_count();

        let cancel = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicU64::new(0));
        let (tx, rx) = channel();

        let c2 = Arc::clone(&cancel);
        let ct2 = Arc::clone(&counter);
        std::thread::spawn(move || {
            let found = plan.run(&c2, &ct2);
            let _ = tx.send(found);
        });

        self.phase = Phase::Running {
            cancel,
            counter,
            total,
            started: Instant::now(),
            result: rx,
        };
        Ok(())
    }

    /// Called every frame while running: collects the result when it lands.
    pub fn poll(&mut self) {
        if let Phase::Running { result, .. } = &self.phase {
            if let Ok(outcome) = result.try_recv() {
                self.phase = Phase::Done(outcome);
            }
        }
    }

    /// Signals the search thread to stop.
    pub fn cancel(&self) {
        if let Phase::Running { cancel, .. } = &self.phase {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::entropy_to_mnemonic;

    const TARGET: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

    fn fill_abandon(ui: &mut RecoverUi) {
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 16], WordCount::W12, &mut m);
        for (slot, w) in ui.slots.iter_mut().zip(m.split_whitespace()) {
            slot.word = w.to_string();
            slot.state = State::Sure;
        }
    }

    #[test]
    fn resizing_keeps_typed_words() {
        let mut ui = RecoverUi::default();
        ui.slots[0].word = "legal".into();
        ui.resize(WordCount::W24);
        assert_eq!(ui.slots.len(), 24);
        assert_eq!(ui.slots[0].word, "legal");
        ui.resize(WordCount::W12);
        assert_eq!(ui.slots.len(), 12);
        assert_eq!(ui.slots[0].word, "legal");
    }

    #[test]
    fn a_complete_seed_previews_as_nothing() {
        let mut ui = RecoverUi::default();
        fill_abandon(&mut ui);
        assert!(matches!(ui.preview(), Preview::Nothing));
        assert!(!ui.can_start());
    }

    #[test]
    fn one_missing_word_needs_only_the_checkbox() {
        // A 12-word seed with one missing word lists 128 candidates without an
        // address — too many — so here the address is what's required.
        let mut ui = RecoverUi::default();
        fill_abandon(&mut ui);
        ui.slots[11].word.clear();
        ui.slots[11].state = State::Unsure;
        assert!(matches!(ui.preview(), Preview::Ready { .. }));
        assert!(ui.address_required(), "128 candidates need an address");
        assert!(!ui.can_start(), "not acknowledged, no address");
        ui.acknowledged = true;
        assert!(!ui.can_start(), "still needs the address");
        ui.address = TARGET.into();
        assert!(ui.can_start());
    }

    #[test]
    fn a_small_space_can_run_without_an_address() {
        // One missing word in a 24-word seed: 8 candidates, listable.
        let mut ui = RecoverUi::default();
        ui.resize(WordCount::W24);
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 32], WordCount::W24, &mut m);
        for (slot, w) in ui.slots.iter_mut().zip(m.split_whitespace()) {
            slot.word = w.to_string();
        }
        ui.slots[23].word.clear();
        ui.slots[23].state = State::Unsure;
        assert!(!ui.address_required(), "8 seeds can be listed");
        ui.acknowledged = true;
        assert!(ui.can_start(), "no address needed for a small space");
    }

    #[test]
    fn the_search_runs_off_thread_to_a_hit() {
        let mut ui = RecoverUi::default();
        fill_abandon(&mut ui);
        ui.slots[11].word.clear();
        ui.slots[11].state = State::Unsure;
        ui.address = TARGET.into();
        ui.acknowledged = true;

        ui.start(2).expect("plan builds");
        for _ in 0..600 {
            ui.poll();
            if matches!(ui.phase, Phase::Done(_)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match &ui.phase {
            Phase::Done(out) => {
                let f = out.hits.first().expect("no hit");
                assert_eq!(f.mnemonic.split_whitespace().last().unwrap(), "about")
            }
            _ => panic!("did not finish"),
        }
    }
}
