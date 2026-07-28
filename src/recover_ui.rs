//! The window screen for [`crate::recover`].
//!
//! Only the state and the search plumbing live here; the drawing is in
//! `gui.rs`, next to the palette and the shared widgets it reuses. The search
//! runs on its own thread — a few million candidates take minutes, and the
//! window must stay responsive and cancellable throughout — and talks back
//! through a counter, a cancel flag and a one-shot channel.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Instant;

use crate::recover::{Found, Mode, Plan, Words};

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
        result: Receiver<Option<Found>>,
    },
    /// Finished: the seed, or nothing.
    Done(Option<Found>),
}

/// Everything the recovery screen needs between frames.
pub struct RecoverUi {
    pub mode: Mode,
    pub words: String,
    pub address: String,
    /// The user has read the warning and accepts the risk.
    pub acknowledged: bool,
    pub phase: Phase,
}

impl Default for RecoverUi {
    fn default() -> Self {
        RecoverUi {
            mode: Mode::Missing,
            words: String::new(),
            address: String::new(),
            acknowledged: false,
            phase: Phase::Editing,
        }
    }
}

/// What the form currently amounts to: shown live, under the inputs.
pub enum Preview {
    /// Nothing typed yet.
    Empty,
    /// The words do not parse. The string is the reason, in plain German.
    Invalid(String),
    /// The words are good; this many candidates and this rough duration,
    /// whether or not an address is present yet.
    Ready { candidates: u64, secs: f64 },
}

impl RecoverUi {
    /// Re-reads the form. Cheap enough to call every frame.
    pub fn preview(&self) -> Preview {
        if self.words.trim().is_empty() {
            return Preview::Empty;
        }
        match Words::parse(&self.words, self.mode) {
            Err(e) => Preview::Invalid(e),
            Ok(w) => Preview::Ready {
                candidates: w.candidate_count(),
                secs: crate::recover::estimate_secs(w.candidate_count(), w.word_count),
            },
        }
    }

    /// Whether the Start button should be live: valid words, a non-empty
    /// address, and the risk acknowledged.
    pub fn can_start(&self) -> bool {
        self.acknowledged
            && !self.address.trim().is_empty()
            && matches!(self.preview(), Preview::Ready { .. })
    }

    /// Builds the plan and spawns the search. On a bad plan the phase stays
    /// Editing and the error comes back for display.
    pub fn start(&mut self, depth: u32) -> Result<(), String> {
        let plan = Plan::new(&self.words, &self.address, self.mode, depth)?;
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
            if let Ok(found) = result.try_recv() {
                self.phase = Phase::Done(found);
            }
        }
    }

    /// Signals the search thread to stop. The thread returns `None` shortly
    /// after, which `poll` turns into `Done(None)`.
    pub fn cancel(&self) {
        if let Phase::Running { cancel, .. } = &self.phase {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Back to a blank form, cancelling any run first.
    pub fn reset(&mut self) {
        self.cancel();
        self.phase = Phase::Editing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::{entropy_to_mnemonic, word_index, WordCount};

    /// The address of the all-zero 12-word seed at m/84'/0'/0'/0/0.
    fn abandon_target() -> String {
        "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu".into()
    }

    fn abandon_words_missing_last() -> String {
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 16], WordCount::W12, &mut m);
        let mut w: Vec<String> = m.split_whitespace().map(str::to_string).collect();
        *w.last_mut().unwrap() = "?".into();
        w.join(" ")
    }

    #[test]
    fn preview_reflects_the_form() {
        let mut ui = RecoverUi::default();
        assert!(matches!(ui.preview(), Preview::Empty));

        "not enough words".clone_into(&mut ui.words);
        assert!(matches!(ui.preview(), Preview::Invalid(_)));

        ui.words = abandon_words_missing_last();
        match ui.preview() {
            Preview::Ready { candidates, .. } => assert_eq!(candidates, 2048),
            other => panic!("expected Ready, got {:?}", matches!(other, Preview::Empty)),
        }
    }

    #[test]
    fn start_button_needs_all_three() {
        let mut ui = RecoverUi {
            words: abandon_words_missing_last(),
            ..Default::default()
        };
        assert!(!ui.can_start(), "no address, no acknowledgement");
        ui.address = abandon_target();
        assert!(!ui.can_start(), "still not acknowledged");
        ui.acknowledged = true;
        assert!(ui.can_start(), "all three present");
    }

    #[test]
    fn the_search_runs_to_a_result_off_the_ui_thread() {
        let mut ui = RecoverUi {
            words: abandon_words_missing_last(),
            address: abandon_target(),
            acknowledged: true,
            ..Default::default()
        };
        ui.start(2).expect("plan should build");
        assert!(matches!(ui.phase, Phase::Running { .. }));

        // Poll until the worker thread delivers, as the window does each frame.
        for _ in 0..600 {
            ui.poll();
            if let Phase::Done(_) = ui.phase {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match &ui.phase {
            Phase::Done(Some(f)) => {
                assert_eq!(f.mnemonic.split_whitespace().last().unwrap(), "about")
            }
            _ => panic!("recovery did not finish with a hit"),
        }

        // The word index helper is what the form leans on; sanity-check it here.
        assert!(word_index("about").is_some());
    }
}
