//! Lock-free run statistics.
//!
//! Workers never touch the shared counters directly. Even a relaxed atomic
//! increment ping-pongs the cache line between cores when eight threads hammer
//! it, so each worker accumulates into a plain local struct and flushes on a
//! batch boundary. The hot loop then contains no atomics at all.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use crate::bip39::WordCount;

/// How many candidates a worker processes before publishing its counters.
pub const FLUSH_EVERY: u64 = 32;

/// Run/pause/stop control shared with the workers.
///
/// Pausing genuinely blocks the worker threads on a condition variable rather
/// than spinning on a flag, so a stopped collider draws no CPU at all. The
/// running path costs one relaxed atomic load per batch — roughly once every
/// ten milliseconds per worker — so the hot loop is unaffected.
/// Scheduling priority for the worker threads.
///
/// On Apple silicon this decides which cores the work lands on. Background
/// keeps the machine cool and responsive by preferring the efficiency cores;
/// Default competes with everything else for the performance cores.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Priority {
    /// Efficiency cores, throttled. The machine stays fully usable.
    Background = 0,
    /// Below foreground apps, but not throttled.
    Utility = 1,
    /// Competes normally. Fastest, warmest.
    Normal = 2,
}

impl Priority {
    pub fn from_u8(v: u8) -> Priority {
        match v {
            0 => Priority::Background,
            1 => Priority::Utility,
            _ => Priority::Normal,
        }
    }

    /// The matching Darwin QoS class from `<sys/qos.h>`.
    #[cfg(target_os = "macos")]
    fn qos_class(self) -> u32 {
        match self {
            Priority::Background => 0x09, // QOS_CLASS_BACKGROUND
            Priority::Utility => 0x11,    // QOS_CLASS_UTILITY
            Priority::Normal => 0x15,     // QOS_CLASS_DEFAULT
        }
    }

    /// Nice value for platforms that schedule by priority number.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn nice(self) -> i32 {
        match self {
            Priority::Background => 19, // maximum politeness
            Priority::Utility => 10,
            Priority::Normal => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Priority::Background => "Sparsam",
            Priority::Utility => "Normal",
            Priority::Normal => "Maximal",
        }
    }

    /// Whether this platform can act on the setting at all.
    pub const fn is_supported() -> bool {
        cfg!(unix)
    }
}

/// Applies a scheduling priority to the calling thread.
///
/// A failure is not worth reporting: the search runs correctly at any priority,
/// it just runs hotter.
///
/// * macOS uses quality-of-service classes, which also steer work between
///   performance and efficiency cores.
/// * Other Unixes use the nice value, which Linux applies per thread.
/// * Windows is a no-op for now; [`Priority::is_supported`] says so, and the
///   settings panel hides the control rather than pretending.
#[cfg(target_os = "macos")]
pub fn apply_priority(p: Priority) {
    extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    // SAFETY: both arguments are plain scalars and the call only affects the
    // calling thread's scheduling.
    unsafe {
        pthread_set_qos_class_self_np(p.qos_class(), 0);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn apply_priority(p: Priority) {
    // SAFETY: setpriority with a pid of 0 targets the calling thread on Linux,
    // where threads are scheduling entities in their own right.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, p.nice());
    }
}

#[cfg(not(unix))]
pub fn apply_priority(_p: Priority) {}

pub struct Control {
    stop: AtomicBool,
    paused: AtomicBool,
    /// Workers whose index is at or above this park themselves, which makes
    /// the core count adjustable while the search is running — no thread pool
    /// has to be torn down and rebuilt.
    active_threads: AtomicUsize,
    /// Addresses derived per path, adjustable live.
    addresses_per_path: AtomicU32,
    /// A [`Priority`] discriminant.
    priority: AtomicU8Wrapper,
    /// Mnemonic length as a word count, adjustable live. Stored as the number
    /// a human would say rather than an enum discriminant, so a bad value can
    /// only ever fall back to 24 instead of meaning something else.
    word_count: AtomicU8Wrapper,
    /// Share of the time a worker is allowed to spend working, in percent.
    ///
    /// 100 is flat out. Below that the worker measures how long a candidate
    /// took and sleeps for the matching remainder, so the duty cycle holds on
    /// a fast machine and a slow one alike. Priority alone cannot go this low:
    /// the lowest scheduling tier still runs continuously, it just runs on the
    /// quiet cores.
    throttle: AtomicU8Wrapper,
    /// Guards the state transitions so a pause cannot be missed between the
    /// flag check and the wait.
    lock: Mutex<()>,
    wake: Condvar,
}

/// `AtomicU8` under a name that documents why it is not a plain `Priority`.
type AtomicU8Wrapper = std::sync::atomic::AtomicU8;

impl Default for Control {
    fn default() -> Self {
        Control::new(1, 20, Priority::Normal)
    }
}

impl Control {
    pub fn new(threads: usize, addresses_per_path: u32, priority: Priority) -> Control {
        Control {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            active_threads: AtomicUsize::new(threads.max(1)),
            addresses_per_path: AtomicU32::new(addresses_per_path.max(1)),
            priority: AtomicU8Wrapper::new(priority as u8),
            word_count: AtomicU8Wrapper::new(WordCount::W24.words() as u8),
            throttle: AtomicU8Wrapper::new(100),
            lock: Mutex::new(()),
            wake: Condvar::new(),
        }
    }

    pub fn active_threads(&self) -> usize {
        self.active_threads.load(Ordering::Relaxed)
    }

    pub fn set_active_threads(&self, n: usize) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.active_threads.store(n.max(1), Ordering::Relaxed);
        self.wake.notify_all();
    }

    pub fn addresses_per_path(&self) -> u32 {
        self.addresses_per_path.load(Ordering::Relaxed)
    }

    pub fn set_addresses_per_path(&self, n: u32) {
        self.addresses_per_path
            .store(n.clamp(1, 200), Ordering::Relaxed);
    }

    pub fn priority(&self) -> Priority {
        Priority::from_u8(self.priority.load(Ordering::Relaxed))
    }

    pub fn set_priority(&self, p: Priority) {
        self.priority.store(p as u8, Ordering::Relaxed);
    }

    /// The mnemonic length the workers are currently drawing.
    ///
    /// Read once per candidate rather than hoisted out of the loop, which is
    /// what lets the length change without restarting the search. One relaxed
    /// atomic load against 700 microseconds of PBKDF2 is not measurable.
    #[inline]
    pub fn word_count(&self) -> WordCount {
        WordCount::from_words(self.word_count.load(Ordering::Relaxed)).unwrap_or(WordCount::W24)
    }

    pub fn set_word_count(&self, wc: WordCount) {
        self.word_count.store(wc.words() as u8, Ordering::Relaxed);
    }

    /// Percent of the time a worker may spend working. Always 1..=100.
    #[inline]
    pub fn throttle(&self) -> u8 {
        self.throttle.load(Ordering::Relaxed).clamp(1, 100)
    }

    pub fn set_throttle(&self, percent: u8) {
        self.throttle
            .store(percent.clamp(1, 100), Ordering::Relaxed);
    }

    /// How long to rest after working for `busy`, to hold the duty cycle.
    ///
    /// Derived from measured work rather than a fixed sleep, so the same
    /// setting means the same share of a core whatever the machine. Capped:
    /// a very large `addresses_per_path` would otherwise buy minute-long naps,
    /// and a worker that never wakes cannot notice a stop.
    pub fn rest_after(&self, busy: std::time::Duration) -> std::time::Duration {
        let t = self.throttle();
        if t >= 100 {
            return std::time::Duration::ZERO;
        }
        let factor = (100.0 - t as f64) / t as f64;
        busy.mul_f64(factor).min(std::time::Duration::from_secs(2))
    }

    /// True when the worker with this index should be doing work.
    #[inline]
    pub fn should_work(&self, index: usize) -> bool {
        !self.paused.load(Ordering::Relaxed) && index < self.active_threads()
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Signals every worker to wind down at its next batch boundary, and wakes
    /// any that are parked.
    pub fn request_stop(&self) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.paused.store(paused, Ordering::Relaxed);
        self.wake.notify_all();
    }

    pub fn toggle_paused(&self) -> bool {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let next = !self.paused.load(Ordering::Relaxed);
        self.paused.store(next, Ordering::Relaxed);
        self.wake.notify_all();
        next
    }

    /// Blocks while this worker should be idle — either the search is paused,
    /// or the worker is above the active core count. Returns immediately in the
    /// common case, and only takes the mutex once idleness has been observed,
    /// so a running worker never contends on it.
    #[inline]
    pub fn wait_while_idle(&self, index: usize) {
        if self.should_work(index) {
            return;
        }
        let mut g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        while !self.should_work(index) && !self.stop.load(Ordering::Relaxed) {
            g = self.wake.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[derive(Default)]
pub struct Stats {
    seeds: AtomicU64,
    addresses: AtomicU64,
    bloom_hits: AtomicU64,
    confirmed: AtomicU64,
}

impl Stats {
    pub fn new() -> Stats {
        Stats::default()
    }

    pub fn seeds(&self) -> u64 {
        self.seeds.load(Ordering::Relaxed)
    }
    pub fn addresses(&self) -> u64 {
        self.addresses.load(Ordering::Relaxed)
    }
    pub fn bloom_hits(&self) -> u64 {
        self.bloom_hits.load(Ordering::Relaxed)
    }
    pub fn confirmed(&self) -> u64 {
        self.confirmed.load(Ordering::Relaxed)
    }

    pub fn note_bloom_hit(&self) {
        self.bloom_hits.fetch_add(1, Ordering::Relaxed);
    }
    pub fn note_confirmed(&self) {
        self.confirmed.fetch_add(1, Ordering::Relaxed);
    }
}

/// Per-worker counters, flushed into [`Stats`] on batch boundaries.
#[derive(Default)]
pub struct Local {
    pub seeds: u64,
    pub addresses: u64,
}

impl Local {
    #[inline]
    pub fn flush(&mut self, stats: &Stats) {
        if self.seeds != 0 {
            stats.seeds.fetch_add(self.seeds, Ordering::Relaxed);
            stats.addresses.fetch_add(self.addresses, Ordering::Relaxed);
            self.seeds = 0;
            self.addresses = 0;
        }
    }
}

/// Rolling throughput derived by sampling [`Stats`] from the UI thread.
pub struct Rate {
    start: Instant,
    /// When the current pause began, if the search is paused.
    paused_since: Option<Instant>,
    /// Time already spent paused, before the current pause.
    paused_total: std::time::Duration,
    last_at: Instant,
    last_seeds: u64,
    /// Exponentially weighted average, in seeds/sec.
    pub(crate) ewma: f64,
    /// Recent instantaneous samples, for the sparkline.
    pub(crate) history: Vec<u64>,
    cap: usize,
}

impl Rate {
    pub fn new(cap: usize) -> Rate {
        let now = Instant::now();
        Rate {
            start: now,
            paused_since: None,
            paused_total: std::time::Duration::ZERO,
            last_at: now,
            last_seeds: 0,
            ewma: 0.0,
            history: Vec::with_capacity(cap),
            cap,
        }
    }

    /// Folds a new counter reading in. Returns the instantaneous rate.
    pub fn sample(&mut self, seeds: u64) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_at).as_secs_f64();
        if dt <= 0.0 {
            return self.ewma;
        }
        let inst = (seeds - self.last_seeds) as f64 / dt;
        self.last_at = now;
        self.last_seeds = seeds;

        // Weight chosen so the average settles over roughly ten samples.
        self.ewma = if self.ewma == 0.0 {
            inst
        } else {
            self.ewma * 0.8 + inst * 0.2
        };

        if self.history.len() == self.cap {
            self.history.remove(0);
        }
        self.history.push(inst as u64);
        inst
    }

    pub fn average(&self) -> f64 {
        self.ewma
    }

    /// Mean rate over the whole run, which is what "gleitender Durchschnitt"
    /// should be compared against.
    pub fn lifetime(&self, seeds: u64) -> f64 {
        let e = self.elapsed().as_secs_f64();
        if e > 0.0 {
            seeds as f64 / e
        } else {
            0.0
        }
    }

    /// Tracks the pause state. Called every frame; only transitions matter.
    pub fn note_paused(&mut self, paused: bool) {
        match (paused, self.paused_since) {
            (true, None) => self.paused_since = Some(Instant::now()),
            (false, Some(at)) => {
                self.paused_total += at.elapsed();
                self.paused_since = None;
            }
            _ => {}
        }
    }

    /// Time the search has actually been running.
    ///
    /// Wall clock minus every pause. It used to be the raw clock, so a run
    /// left paused overnight claimed to have searched all night — and the
    /// lifetime average, which divides by this, was wrong by the same factor.
    pub fn elapsed(&self) -> std::time::Duration {
        let raw = self.start.elapsed().saturating_sub(self.paused_total);
        match self.paused_since {
            Some(at) => raw.saturating_sub(at.elapsed()),
            None => raw,
        }
    }

    pub fn history(&self) -> &[u64] {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_flush_accumulates() {
        let s = Stats::new();
        let mut l = Local {
            seeds: 10,
            addresses: 600,
        };
        l.flush(&s);
        l.flush(&s); // second flush is a no-op
        assert_eq!(s.seeds(), 10);
        assert_eq!(s.addresses(), 600);
    }

    /// The duty cycle is the whole point of the unobtrusive mode: rest time is
    /// derived from measured work, so one percent means one percent on a fast
    /// machine and a slow one alike.
    #[test]
    fn throttling_rests_in_proportion_to_the_work() {
        use std::time::Duration;
        let c = Control::new(1, 20, Priority::Background);

        assert_eq!(c.rest_after(Duration::from_millis(5)), Duration::ZERO);

        c.set_throttle(50);
        assert_eq!(
            c.rest_after(Duration::from_millis(10)),
            Duration::from_millis(10),
            "half duty means resting as long as working"
        );

        c.set_throttle(1);
        let rest = c.rest_after(Duration::from_micros(700));
        assert!(
            rest >= Duration::from_millis(68) && rest <= Duration::from_millis(70),
            "one percent of a 700µs candidate should rest ~69ms, rested {rest:?}"
        );

        // A very large addresses-per-path setting must not buy minute-long
        // naps: a worker that never wakes cannot notice a stop.
        assert_eq!(
            c.rest_after(Duration::from_secs(10)),
            Duration::from_secs(2),
            "rest is capped"
        );
    }

    /// A paused search is not running, and the clock has to say so. It used to
    /// keep counting, so a run left paused overnight claimed the whole night
    /// as search time — and the lifetime average divides by exactly this.
    #[test]
    fn the_clock_stops_while_paused() {
        use std::time::Duration;
        let mut r = Rate::new(8);
        std::thread::sleep(Duration::from_millis(40));

        r.note_paused(true);
        let at_pause = r.elapsed();
        std::thread::sleep(Duration::from_millis(60));
        let during = r.elapsed();
        assert!(
            during.saturating_sub(at_pause) < Duration::from_millis(15),
            "clock advanced by {:?} while paused",
            during.saturating_sub(at_pause)
        );

        r.note_paused(false);
        std::thread::sleep(Duration::from_millis(40));
        let after = r.elapsed();
        assert!(after > during, "clock did not restart");
        assert!(
            after < Duration::from_millis(120),
            "the pause was counted after all: {after:?}"
        );
    }

    #[test]
    fn rate_tracks_instantaneous_throughput() {
        let mut r = Rate::new(8);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let inst = r.sample(100);
        // 100 seeds in ~50ms is ~2000/s; allow a wide band for CI jitter.
        assert!(inst > 500.0 && inst < 10_000.0, "implausible rate {inst}");
    }

    #[test]
    fn history_is_capped() {
        let mut r = Rate::new(4);
        for i in 1..=10 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            r.sample(i * 10);
        }
        assert_eq!(r.history().len(), 4);
    }

    #[test]
    fn control_starts_running() {
        let c = Control::new(1, 20, Priority::Normal);
        assert!(!c.paused());
        assert!(!c.stopping());
        // Must not block.
        c.wait_while_idle(0);
    }

    #[test]
    fn toggle_flips_state() {
        let c = Control::new(1, 20, Priority::Normal);
        assert!(c.toggle_paused());
        assert!(c.paused());
        assert!(!c.toggle_paused());
        assert!(!c.paused());
    }

    /// A paused worker must actually block, and resume promptly when released.
    #[test]
    fn pause_blocks_and_resume_releases() {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        let c = Arc::new(Control::new(1, 20, Priority::Normal));
        let counter = Arc::new(AtomicU32::new(0));
        c.set_paused(true);

        let c2 = Arc::clone(&c);
        let n2 = Arc::clone(&counter);
        let h = std::thread::spawn(move || {
            c2.wait_while_idle(0);
            n2.store(1, Ordering::SeqCst);
        });

        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(counter.load(Ordering::SeqCst), 0, "worker ran while paused");

        c.set_paused(false);
        h.join().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// A worker above the active core count must park, and start again the
    /// moment the count is raised — that is what makes the core slider work
    /// without rebuilding the thread pool.
    #[test]
    fn workers_above_the_active_count_park() {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        let c = Arc::new(Control::new(2, 20, Priority::Normal));
        assert!(c.should_work(0));
        assert!(c.should_work(1));
        assert!(!c.should_work(2), "worker 2 is above the active count");

        let counter = Arc::new(AtomicU32::new(0));
        let c2 = Arc::clone(&c);
        let n2 = Arc::clone(&counter);
        let h = std::thread::spawn(move || {
            c2.wait_while_idle(3);
            n2.store(1, Ordering::SeqCst);
        });

        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "worker 3 ran while parked"
        );

        c.set_active_threads(8);
        h.join().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn live_tunables_clamp_to_sane_ranges() {
        let c = Control::new(4, 20, Priority::Utility);
        assert_eq!(c.addresses_per_path(), 20);
        c.set_addresses_per_path(0);
        assert_eq!(c.addresses_per_path(), 1, "zero addresses would be useless");
        c.set_addresses_per_path(9999);
        assert_eq!(c.addresses_per_path(), 200, "clamped to the upper bound");

        c.set_active_threads(0);
        assert_eq!(c.active_threads(), 1, "at least one worker must run");

        assert_eq!(c.throttle(), 100, "unthrottled by default");
        c.set_throttle(0);
        assert_eq!(c.throttle(), 1, "zero would stop the search entirely");
        c.set_throttle(200);
        assert_eq!(c.throttle(), 100, "clamped to flat out");

        assert_eq!(c.priority(), Priority::Utility);
        c.set_priority(Priority::Background);
        assert_eq!(c.priority(), Priority::Background);
        assert_eq!(Priority::from_u8(0), Priority::Background);
        assert_eq!(Priority::from_u8(2), Priority::Normal);
    }

    /// Pausing must override the core count: a paused worker stays parked even
    /// if it is within the active range.
    #[test]
    fn pause_beats_the_active_count() {
        let c = Control::new(8, 20, Priority::Normal);
        assert!(c.should_work(0));
        c.set_paused(true);
        assert!(!c.should_work(0));
    }

    /// Stopping while paused must not deadlock the worker.
    #[test]
    fn stop_releases_a_paused_worker() {
        use std::sync::Arc;

        let c = Arc::new(Control::new(1, 20, Priority::Normal));
        c.set_paused(true);

        let c2 = Arc::clone(&c);
        let h = std::thread::spawn(move || {
            c2.wait_while_idle(0);
        });

        std::thread::sleep(std::time::Duration::from_millis(50));
        c.request_stop();
        // Would hang forever if the wait loop ignored the stop flag.
        h.join().unwrap();
    }
}
