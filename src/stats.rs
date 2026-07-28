//! Lock-free run statistics.
//!
//! Workers never touch the shared counters directly. Even a relaxed atomic
//! increment ping-pongs the cache line between cores when eight threads hammer
//! it, so each worker accumulates into a plain local struct and flushes on a
//! batch boundary. The hot loop then contains no atomics at all.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

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
        let e = self.start.elapsed().as_secs_f64();
        if e > 0.0 {
            seeds as f64 / e
        } else {
            0.0
        }
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
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
