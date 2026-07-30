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
    /// The "paste everything at once" box, and what came of the last attempt.
    pub bulk: String,
    pub bulk_note: Option<Result<String, String>>,
    /// How many cores this search may use. Its own setting rather than the
    /// dashboard's: somebody recovering their own wallet wants the machine
    /// pushed, and the same person may well have the background search set to
    /// one polite core.
    pub threads: usize,
    /// The main search's funded set and hit-writing path, once loading has
    /// finished. Without it a targetless search can only list seeds.
    pub hunt: Option<Arc<crate::engine::Shared>>,
    /// Which of the four screens is showing.
    pub step: Step,
    /// The form holds a rolled practice seed rather than somebody's own.
    ///
    /// Drives the banner across the wizard. It matters most on the last screen,
    /// where a found seed appears: a rolled one must never be mistaken for a
    /// real recovery.
    pub practice: bool,
    /// How many times the die has been rolled this session — picks the face
    /// shown on the button, so a second roll visibly does something.
    pub rolls: u32,
    /// What the recovered wallet holds, once there is something to ask about.
    pub balance: Balance,
    /// Where an online lookup reports back to. `Some` while one is in flight.
    balance_rx: Option<std::sync::mpsc::Receiver<Result<crate::balance::Sum, String>>>,
    /// Ob der Kontostand nach einem Fund von selbst online abgefragt wird.
    ///
    /// Vor dem Start anzukreuzen, neben der Warnung, und **aus** als
    /// Voreinstellung. Der Kontostand ist die Frage, mit der jeder herkommt,
    /// und ihn erst auf einen zweiten Klick zu zeigen ist unnötig zäh — aber
    /// die Abfrage schickt Adressen an einen fremden Dienst, und das darf
    /// niemandem passieren, der nicht vorher zugestimmt hat. Also die Frage
    /// **vorher**, wo sie noch eine Entscheidung ist, statt hinterher, wo sie
    /// nur noch eine Mitteilung wäre.
    pub auto_balance: bool,
    /// Ob die automatische Abfrage für diesen Fund schon losgeschickt wurde —
    /// der Ergebnisbildschirm wird sechzig Mal je Sekunde gezeichnet.
    pub auto_asked: bool,
}

impl Default for RecoverUi {
    fn default() -> Self {
        let mut ui = RecoverUi {
            word_count: WordCount::W12,
            slots: Vec::new(),
            address: String::new(),
            acknowledged: false,
            phase: Phase::Editing,
            bulk: String::new(),
            bulk_note: None,
            // Half the machine to begin with: fast, and still leaves the
            // rest of the desktop usable while it runs.
            threads: (crate::config::physical_cores() / 2).max(1),
            hunt: None,
            step: Step::Length,
            practice: false,
            rolls: 0,
            balance: Balance::Unknown,
            balance_rx: None,
            auto_balance: false,
            auto_asked: false,
        };
        ui.resize(WordCount::W12);
        ui
    }
}

/// One question per screen, with a way back.
///
/// Everything used to be on a single page: five length buttons, a paste box, a
/// colour legend, two dozen word fields, an address, a power setting, a warning
/// and a start button, all at once. Somebody who has just lost part of their
/// seed does not need to see all of that at the same time. Four screens, one
/// question each, and Zurück always available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Length,
    Words,
    Address,
    Start,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Length, Step::Words, Step::Address, Step::Start];

    /// The heading of this screen, and the short line under it.
    pub fn title(self) -> (&'static str, &'static str) {
        match self {
            Step::Length => (
                "Wie viele Wörter hat deine Seed?",
                "Die allermeisten Wallets benutzen 12 oder 24. Steht auf deinem Zettel.",
            ),
            Step::Words => (
                "Trag ein, was du noch hast",
                "Was du nicht mehr weißt, lässt du einfach leer.",
            ),
            Step::Address => (
                "Kennst du eine Adresse deiner Wallet?",
                "Musst du nicht. Ohne geht es auch — dann sucht das Programm selbst.",
            ),
            Step::Start => (
                "Bereit?",
                "Stell noch ein, wie stark der Rechner arbeiten soll.",
            ),
        }
    }

    /// Short label for the rail at the top.
    pub fn tab(self) -> &'static str {
        match self {
            Step::Length => "Länge",
            Step::Words => "Wörter",
            Step::Address => "Adresse",
            Step::Start => "Start",
        }
    }

    pub fn index(self) -> usize {
        Step::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Step {
        Step::ALL[(self.index() + 1).min(Step::ALL.len() - 1)]
    }

    pub fn prev(self) -> Step {
        Step::ALL[self.index().saturating_sub(1)]
    }
}

/// What a search without a target address will do with what it turns up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Targetless {
    /// Few enough possibilities to simply show them all, so the owner can
    /// recognise their own by its first address.
    ListsSeeds(u64),
    /// Too many to list. Every candidate is tested against the funded address
    /// set instead, and one that owns money is saved and alerted on.
    HuntsForMoney,
}

/// What is known about the balance of the recovered wallet.
///
/// Six states rather than an `Option<u64>`, because they need six different
/// sentences. The one that matters most is [`Balance::NotListed`]: the local
/// funded list only knows the addresses that were loaded into it, and the
/// practice list holds nothing but random ones — so "absent" is the normal
/// answer there and must never be drawn as a balance of zero.
#[derive(Debug, Clone)]
pub enum Balance {
    /// Not looked at yet.
    Unknown,
    /// No address of this wallet is in the local funded list.
    NotListed,
    /// The local funded list says this.
    Local(crate::balance::Sum),
    /// An online lookup is in flight.
    Asking,
    /// The service answered.
    Online(crate::balance::Sum),
    /// The online lookup failed; the string says why.
    Failed(String),
}

/// What the form currently amounts to: shown live, under the words.
#[derive(Debug)]
pub enum Preview {
    /// Words are still all sure and present — nothing to search yet.
    Nothing,
    /// A word does not parse. The string is the reason, in plain German.
    Invalid(String),
    /// Ready: this many candidates and this rough duration.
    Ready {
        candidates: u64,
        secs: f64,
        /// The space is past [`crate::recover::HOPELESS_ABOVE`]. The search is
        /// still allowed to run — it just will not finish in a human life, and
        /// the screen says so instead of refusing.
        hopeless: bool,
    },
}

impl RecoverUi {
    /// A fresh screen with the main search's funded set attached.
    ///
    /// A constructor rather than struct-update syntax at the call site, so the
    /// channel behind [`RecoverUi::ask_online`] can stay private: nothing
    /// outside this module has any business handing it a receiver.
    pub fn with_hunt(hunt: Option<Arc<crate::engine::Shared>>) -> RecoverUi {
        RecoverUi {
            hunt,
            ..Default::default()
        }
    }

    /// Grows or shrinks the slot list to a new mnemonic length, keeping the
    /// words already typed.
    pub fn resize(&mut self, wc: WordCount) {
        self.word_count = wc;
        self.slots.resize_with(wc.words(), Slot::default);
    }

    /// Spreads a whole seed, pasted or typed in one go, across the slots.
    ///
    /// Filling two dozen boxes one at a time is the tax this screen used to
    /// charge everyone who already had their words in a file. `?` marks a word
    /// that is gone, matching the marker the command line uses.
    ///
    /// A count that is itself a valid mnemonic length switches the form to it:
    /// somebody pasting 24 words into a form set to 12 means the form is wrong,
    /// not the paste.
    pub fn paste_all(&mut self, text: &str) -> Result<usize, String> {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        if tokens.is_empty() {
            return Err("Da war nichts zum Einfügen.".into());
        }
        if let Some(wc) = WordCount::from_words(tokens.len() as u8) {
            self.resize(wc);
        } else if tokens.len() > self.slots.len() {
            return Err(format!(
                "Das sind {} Wörter. Eine Seed hat 12, 15, 18, 21 oder 24 — \
                 stell oben die richtige Länge ein.",
                tokens.len()
            ));
        }

        for (slot, tok) in self.slots.iter_mut().zip(&tokens) {
            if *tok == "?" {
                slot.word.clear();
                slot.state = State::Unsure;
            } else {
                slot.word = tok.trim().to_ascii_lowercase();
                slot.state = State::Sure;
            }
        }
        // Whatever is in the form now, it is not the rolled seed any more. This
        // is why `roll_practice` sets the flag *after* calling through here.
        self.practice = false;
        Ok(tokens.len().min(self.slots.len()))
    }

    /// Fills the form with a rolled practice seed, one word left open.
    ///
    /// Returns which word was left open, counted from one, for the note on
    /// screen. `Err` only when the OS entropy source refuses.
    ///
    /// Deliberately built on [`RecoverUi::paste_all`] rather than writing the
    /// slots itself: that path already turns `?` into an empty
    /// [`State::Unsure`] field, lowercases the rest, and re-sizes the form to
    /// the length it was handed. Two ways of filling this form would be one too
    /// many.
    ///
    /// The address is filled in too, and that is not a convenience — see
    /// [`crate::recover::roll_practice`]. Without it the run has nothing to find
    /// and the rehearsal ends on "nothing found".
    pub fn roll_practice(&mut self) -> Result<usize, String> {
        let rolled = crate::recover::roll_practice(self.word_count).ok_or(
            "Der Zufallsgenerator des Systems hat nicht geantwortet. \
             Probier es noch einmal.",
        )?;

        let mut tokens: Vec<&str> = rolled.mnemonic.split_whitespace().collect();
        // The gap is what gives the search something to do.
        tokens[rolled.gap] = "?";
        self.paste_all(&tokens.join(" "))?;

        self.address = rolled.address;
        self.practice = true;
        self.rolls += 1;
        Ok(rolled.gap + 1)
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
                Preview::Ready {
                    candidates,
                    secs: estimate_secs(candidates, layout.word_count),
                    hopeless: candidates > crate::recover::HOPELESS_ABOVE,
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

    /// What a run without an address would do.
    ///
    /// The address used to be compulsory past [`MAX_LISTED`] possible seeds,
    /// because listing was the only thing a targetless search could do. It no
    /// longer is: without an address every candidate is tested against the
    /// funded set instead, which works at any size. So the field is optional,
    /// full stop — this only decides what the screen promises.
    pub fn without_address(&self) -> Targetless {
        match self.expected_without_address() {
            Some(n) if n <= MAX_LISTED => Targetless::ListsSeeds(n),
            _ => Targetless::HuntsForMoney,
        }
    }

    /// Whether Start should be live: something to search and the risk
    /// acknowledged. No address is required, ever.
    pub fn can_start(&self) -> bool {
        self.acknowledged && matches!(self.preview(), Preview::Ready { .. })
    }

    /// Builds the plan and spawns the search. On a bad plan the phase stays
    /// Editing and the error comes back for display.
    pub fn start(&mut self, depth: u32) -> Result<(), String> {
        let layout = Layout::build(&self.words(), &self.states())?;
        let plan = Plan::with_hunt(
            layout,
            &self.address,
            depth,
            self.hunt.clone(),
            self.threads,
        )?;
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

    /// Called every frame while running: collects the result when it lands, and
    /// the answer of an online balance lookup when that lands.
    pub fn poll(&mut self) {
        if let Phase::Running { result, .. } = &self.phase {
            if let Ok(outcome) = result.try_recv() {
                self.phase = Phase::Done(outcome);
                // The local list is free to ask and needs no permission: one
                // PBKDF2 round and fifteen binary searches over an mmap.
                self.look_up_locally();
            }
        }

        if let Some(rx) = &self.balance_rx {
            match rx.try_recv() {
                Ok(Ok(sum)) => {
                    self.balance = Balance::Online(sum);
                    self.balance_rx = None;
                }
                Ok(Err(e)) => {
                    self.balance = Balance::Failed(e);
                    self.balance_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.balance = Balance::Failed("Die Abfrage ist abgebrochen.".into());
                    self.balance_rx = None;
                }
            }
        }
    }

    /// The first found seed, if the search produced one.
    fn found_mnemonic(&self) -> Option<String> {
        match &self.phase {
            Phase::Done(o) => o.hits.first().map(|f| f.mnemonic.clone()),
            _ => None,
        }
    }

    /// Asks the local funded list about the recovered wallet.
    ///
    /// Skipped for a practice run: a rolled seed belongs to nobody, so there is
    /// nothing to look up and a result would only invite the wrong conclusion.
    fn look_up_locally(&mut self) {
        if self.practice {
            return;
        }
        let Some(mnemonic) = self.found_mnemonic() else {
            return;
        };
        let Some(shared) = &self.hunt else {
            // No database loaded — nothing to say, so say nothing.
            return;
        };
        self.balance = match crate::balance::local(&shared.db, &mnemonic, self.word_count) {
            Some(sum) => Balance::Local(sum),
            None => Balance::NotListed,
        };
    }

    /// Starts an online balance lookup on its own thread.
    ///
    /// Only ever called from a button press. Refused for a practice run, and
    /// never started twice at once.
    ///
    /// The words stay here; `crate::balance::online` sends derived addresses and
    /// nothing else.
    pub fn ask_online(&mut self, api: &str) {
        if self.practice || self.balance_rx.is_some() {
            return;
        }
        let Some(mnemonic) = self.found_mnemonic() else {
            return;
        };

        let (tx, rx) = channel();
        let (api, wc) = (api.to_string(), self.word_count);
        std::thread::spawn(move || {
            let _ = tx.send(crate::balance::online(&api, &mnemonic, wc));
        });
        self.balance = Balance::Asking;
        self.balance_rx = Some(rx);
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

    /// The wizard walks both ways and stops at the ends rather than falling
    /// off them — a Zurück on the first screen or a Weiter on the last must be
    /// a no-op, not a panic or a jump to nowhere.
    #[test]
    fn the_steps_walk_forwards_and_back_without_falling_off() {
        assert_eq!(Step::Length.prev(), Step::Length, "first must not go back");
        assert_eq!(Step::Start.next(), Step::Start, "last must not go on");

        // Forwards through all four, then back again, landing where it began.
        let mut s = Step::Length;
        for expected in [Step::Words, Step::Address, Step::Start] {
            s = s.next();
            assert_eq!(s, expected);
        }
        for expected in [Step::Address, Step::Words, Step::Length] {
            s = s.prev();
            assert_eq!(s, expected);
        }

        // Every screen has a heading, a subtitle and a rail label, and the
        // indices run 0..4 in order.
        for (i, step) in Step::ALL.iter().enumerate() {
            assert_eq!(step.index(), i);
            let (title, sub) = step.title();
            assert!(
                !title.is_empty() && !sub.is_empty(),
                "{step:?} has no words"
            );
            assert!(!step.tab().is_empty());
        }
    }

    /// A fresh screen opens on the first question, not in the middle.
    #[test]
    fn the_wizard_opens_at_the_beginning() {
        assert_eq!(RecoverUi::default().step, Step::Length);
    }

    #[test]
    fn pasting_a_whole_seed_fills_the_form() {
        let mut ui = RecoverUi::default();
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 32], WordCount::W24, &mut m);

        // 24 words into a form set to 12: the form follows the paste.
        assert_eq!(ui.paste_all(&m).unwrap(), 24);
        assert_eq!(ui.word_count, WordCount::W24);
        assert_eq!(ui.slots[0].word, "abandon");
        assert_eq!(ui.slots[23].word, "art");
        assert!(ui.slots.iter().all(|s| s.state == State::Sure));
    }

    #[test]
    fn pasting_honours_the_question_mark_and_tidies_the_words() {
        let mut ui = RecoverUi::default();
        let n = ui
            .paste_all(
                "  ABANDON  abandon abandon abandon abandon abandon \
                 abandon abandon abandon abandon abandon ?  ",
            )
            .unwrap();
        assert_eq!(n, 12);
        assert_eq!(ui.slots[0].word, "abandon", "case is tidied up");
        assert!(ui.slots[11].word.is_empty(), "? is a word that is gone");
        assert_eq!(ui.slots[11].state, State::Unsure);
        assert_eq!(ui.slots[10].state, State::Sure);
    }

    #[test]
    fn pasting_nonsense_says_so_rather_than_half_filling() {
        let mut ui = RecoverUi::default();
        assert!(ui.paste_all("").is_err());
        assert!(ui.paste_all("   \n  ").is_err());

        // Not a mnemonic length, and longer than the form: refused with a
        // sentence that says what to do.
        let thirteen = "abandon ".repeat(13);
        let err = ui.paste_all(&thirteen).unwrap_err();
        assert!(err.contains("13"), "{err}");
        assert!(
            ui.slots.iter().all(|s| s.word.is_empty()),
            "nothing written"
        );

        // Fewer words than the form is fine — a partial seed is the whole
        // point of this screen.
        assert_eq!(ui.paste_all("abandon ability able").unwrap(), 3);
        assert_eq!(ui.slots[2].word, "able");
        assert!(ui.slots[3].word.is_empty());
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

    /// The address is optional at every size. It used to be demanded as soon
    /// as a targetless run would list more than a handful of seeds, which is
    /// what stopped two missing words from being searchable at all.
    #[test]
    fn the_address_is_never_required() {
        // 128 possible seeds — comfortably past what is worth listing.
        let mut ui = RecoverUi::default();
        fill_abandon(&mut ui);
        ui.slots[11].word.clear();
        ui.slots[11].state = State::Unsure;
        assert!(matches!(ui.preview(), Preview::Ready { .. }));
        assert_eq!(ui.without_address(), Targetless::HuntsForMoney);
        assert!(!ui.can_start(), "the tick is still missing");
        ui.acknowledged = true;
        assert!(ui.can_start(), "and that was the only thing missing");

        // Two missing words: four million combinations, the case that was
        // refused outright before.
        ui.slots[10].word.clear();
        ui.slots[10].state = State::Unsure;
        match ui.preview() {
            Preview::Ready {
                candidates,
                hopeless,
                ..
            } => {
                assert_eq!(candidates, 2048 * 2048);
                assert!(!hopeless, "four million is long, not hopeless");
            }
            _ => panic!("two missing words must still be a runnable search"),
        }
        assert!(ui.can_start(), "two missing words must be startable");
    }

    /// A space nobody could finish is allowed to run, and says so.
    #[test]
    fn a_hopeless_space_is_offered_rather_than_refused() {
        let mut ui = RecoverUi::default();
        fill_abandon(&mut ui);
        for i in 8..12 {
            ui.slots[i].word.clear();
            ui.slots[i].state = State::Unsure;
        }
        ui.acknowledged = true;
        match ui.preview() {
            Preview::Ready {
                candidates,
                hopeless,
                ..
            } => {
                assert!(candidates > crate::recover::HOPELESS_ABOVE);
                assert!(hopeless, "four missing words must be flagged as hopeless");
            }
            _ => panic!("a hopeless space must still be Ready, not Invalid"),
        }
        assert!(
            ui.can_start(),
            "the reader is allowed to watch it try — that is the point"
        );
    }

    #[test]
    fn a_small_space_is_listed_rather_than_hunted() {
        // One missing word in a 24-word seed: 8 candidates, worth listing.
        let mut ui = RecoverUi::default();
        ui.resize(WordCount::W24);
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 32], WordCount::W24, &mut m);
        for (slot, w) in ui.slots.iter_mut().zip(m.split_whitespace()) {
            slot.word = w.to_string();
        }
        ui.slots[23].word.clear();
        ui.slots[23].state = State::Unsure;
        assert_eq!(ui.without_address(), Targetless::ListsSeeds(8));
        ui.acknowledged = true;
        assert!(ui.can_start());
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

    /// A roll fills everything and leaves exactly one word open — no more, no
    /// fewer. Two gaps would be four million candidates instead of two thousand,
    /// and none at all would be the dead end below.
    #[test]
    fn rolling_leaves_exactly_one_word_open() {
        let mut ui = RecoverUi::default();
        let gap = ui.roll_practice().expect("OS entropy");

        assert!((1..=12).contains(&gap), "Lücke {gap} außerhalb");
        let open: Vec<usize> = ui
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.state == State::Unsure)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            open,
            vec![gap - 1],
            "genau ein Wort offen, an der genannten Stelle"
        );
        assert!(
            ui.slots[gap - 1].word.is_empty(),
            "das offene Feld ist leer"
        );

        for (i, s) in ui.slots.iter().enumerate() {
            if i == gap - 1 {
                continue;
            }
            assert_eq!(s.state, State::Sure, "Wort {} sollte sicher sein", i + 1);
            assert!(!s.word.is_empty(), "Wort {} ist leer", i + 1);
        }

        assert!(ui.address.starts_with("bc1q"), "Adresse: {}", ui.address);
        assert!(ui.practice, "der Streifen muss angehen");
        assert_eq!(ui.rolls, 1);
    }

    /// **The test this feature exists for.** A rolled form must be ready to
    /// search. Filling every word would make `Plan` refuse it — "hier ist nichts
    /// zu suchen" — and the rehearsal would end in a dead end one screen before
    /// the interesting part.
    #[test]
    fn a_rolled_form_has_something_to_search() {
        let mut ui = RecoverUi::default();
        ui.roll_practice().expect("OS entropy");

        match ui.preview() {
            Preview::Ready {
                candidates,
                hopeless,
                ..
            } => {
                assert_eq!(candidates, 2048, "ein offenes Wort sind 2048 Möglichkeiten");
                assert!(!hopeless, "2048 Kandidaten sind nicht aussichtslos");
            }
            other => panic!("nicht startklar: {other:?}"),
        }

        assert!(!ui.can_start(), "ohne Haken nicht");
        ui.acknowledged = true;
        assert!(ui.can_start(), "mit Haken schon");

        // And the plan really builds — `can_start` only checks the preview.
        ui.start(2).expect("der Plan muss stehen");
    }

    /// Rolling at 24 words gives a 24-word form, not a 12-word one.
    #[test]
    fn rolling_respects_the_chosen_length() {
        let mut ui = RecoverUi::default();
        ui.resize(WordCount::W24);
        ui.roll_practice().expect("OS entropy");

        assert_eq!(ui.word_count, WordCount::W24);
        assert_eq!(ui.slots.len(), 24);
        assert_eq!(
            ui.slots.iter().filter(|s| s.state == State::Unsure).count(),
            1
        );
    }

    /// Typing or pasting your own words over a practice seed means it is not a
    /// practice seed any more, and the banner must go with it.
    #[test]
    fn pasting_over_a_practice_seed_clears_the_marker() {
        let mut ui = RecoverUi::default();
        ui.roll_practice().expect("OS entropy");
        assert!(ui.practice);

        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 16], WordCount::W12, &mut m);
        ui.paste_all(&m).expect("gültige Länge");
        assert!(!ui.practice, "die Wörter sind jetzt fremde");
    }

    /// The whole point, end to end: roll, run, and the seed that comes back is
    /// the seed that was rolled. Same shape as
    /// `the_search_runs_off_thread_to_a_hit` above — 2048 candidates, a second
    /// or two in release.
    #[test]
    fn a_practice_run_finds_its_own_seed() {
        let mut ui = RecoverUi::default();
        let gap = ui.roll_practice().expect("OS entropy");
        // Remember the answer before the search goes looking for it.
        let expected: Vec<String> = ui.slots.iter().map(|s| s.word.clone()).collect();
        ui.acknowledged = true;

        ui.start(2).expect("plan builds");
        for _ in 0..1200 {
            ui.poll();
            if matches!(ui.phase, Phase::Done(_)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        match &ui.phase {
            Phase::Done(out) => {
                let f = out
                    .hits
                    .first()
                    .expect("der Übungslauf fand seine eigene Seed nicht");
                let found: Vec<&str> = f.mnemonic.split_whitespace().collect();
                assert_eq!(found.len(), 12);
                for (i, w) in found.iter().enumerate() {
                    if i == gap - 1 {
                        continue; // das war die Lücke
                    }
                    assert_eq!(*w, expected[i], "Wort {} weicht ab", i + 1);
                }
                assert!(
                    !f.is_funded(),
                    "eine erfundene Seed darf nie als Guthaben gemeldet werden"
                );
            }
            _ => panic!("did not finish"),
        }
    }
}
