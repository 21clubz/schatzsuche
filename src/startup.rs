//! Loading, done off the main thread.
//!
//! Opening the database, building the Bloom filter and reading back existing
//! hits all happen before the search can begin. Doing that before opening the
//! window means the application shows nothing at all while it works — fine for
//! a 5M-record database at 0.3 seconds, indistinguishable from a crash at 50M.
//!
//! So the window opens first and watches [`Progress`] while this runs behind it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::hits::{Hit, HitWriter};
use crate::lookup::{Bloom, Database};

/// Shared state a loading screen can poll.
pub struct Progress {
    step: Mutex<String>,
    /// Completion in thousandths, so it fits an atomic.
    permille: AtomicU32,
    done: AtomicBool,
    /// Figures the window cannot know until loading finishes.
    funded: AtomicU64,
    bloom_bytes: AtomicU64,
    db_bytes: AtomicU64,
    /// Set instead of `done` when loading fails.
    error: Mutex<Option<String>>,
}

impl Default for Progress {
    fn default() -> Self {
        Progress::new()
    }
}

impl Progress {
    pub fn new() -> Progress {
        Progress {
            step: Mutex::new("Startet …".to_string()),
            permille: AtomicU32::new(0),
            done: AtomicBool::new(false),
            funded: AtomicU64::new(0),
            bloom_bytes: AtomicU64::new(0),
            db_bytes: AtomicU64::new(0),
            error: Mutex::new(None),
        }
    }

    pub fn set(&self, step: &str, fraction: f32) {
        if let Ok(mut s) = self.step.lock() {
            *s = step.to_string();
        }
        self.permille.store(
            (fraction.clamp(0.0, 1.0) * 1000.0) as u32,
            Ordering::Relaxed,
        );
    }

    pub fn advance(&self, fraction: f32) {
        self.permille.store(
            (fraction.clamp(0.0, 1.0) * 1000.0) as u32,
            Ordering::Relaxed,
        );
    }

    pub fn step(&self) -> String {
        self.step
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| String::new())
    }

    pub fn fraction(&self) -> f32 {
        self.permille.load(Ordering::Relaxed) as f32 / 1000.0
    }

    pub fn finish(&self) {
        self.permille.store(1000, Ordering::Relaxed);
        self.done.store(true, Ordering::Relaxed);
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub fn publish(&self, funded: u64, bloom_bytes: usize, db_bytes: usize) {
        self.funded.store(funded, Ordering::Relaxed);
        self.bloom_bytes
            .store(bloom_bytes as u64, Ordering::Relaxed);
        self.db_bytes.store(db_bytes as u64, Ordering::Relaxed);
    }

    pub fn funded(&self) -> u64 {
        self.funded.load(Ordering::Relaxed)
    }
    pub fn bloom_bytes(&self) -> usize {
        self.bloom_bytes.load(Ordering::Relaxed) as usize
    }
    pub fn db_bytes(&self) -> usize {
        self.db_bytes.load(Ordering::Relaxed) as usize
    }

    /// Records a failure. The window shows this instead of the dashboard.
    pub fn fail(&self, message: String) {
        if let Ok(mut e) = self.error.lock() {
            *e = Some(message);
        }
        self.done.store(true, Ordering::Relaxed);
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
}

/// Everything the search needs, once loading has finished.
pub struct Loaded {
    pub db: Database,
    pub bloom: Bloom,
    pub writer: Arc<HitWriter>,
    pub existing: Vec<Hit>,
}

/// Performs the load, reporting each stage.
///
/// The Bloom filter dominates the time, so it gets the bulk of the bar; the
/// other stages are near-instant and would otherwise make the bar jump.
pub fn load(cfg: &Config, progress: &Progress) -> Result<Loaded, String> {
    progress.set("Adress-Datenbank wird geöffnet …", 0.02);
    let db = Database::open(&cfg.lookup.database).map_err(|e| {
        format!(
            "Die Adress-Datenbank konnte nicht geöffnet werden.\n\n\
             Datei:  {}\n\
             Ordner: {}\n\
             Grund:  {e}\n\n\
             Im Terminal neu anlegen mit:\n\
             schatzsuche synth-db --count 5000000",
            cfg.lookup.database.display(),
            std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|_| "?".into())
        )
    })?;

    progress.set(
        &format!(
            "Suchfilter wird aufgebaut ({} Adressen) …",
            crate::util::group_digits(db.count() as u64)
        ),
        0.06,
    );
    let bloom = db.build_bloom_with_progress(cfg.lookup.bloom_fpr, |f| {
        progress.advance(0.06 + f * 0.88);
    });

    progress.set("Trefferdateien werden geprüft …", 0.96);
    let writer = Arc::new(HitWriter::new(
        cfg.hits.path.clone(),
        cfg.hits.backup_path.clone(),
    ));
    let existing = writer.load_all().unwrap_or_default();

    progress.set("Bereit", 1.0);
    Ok(Loaded {
        db,
        bloom,
        writer,
        existing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_clamps_and_reports() {
        let p = Progress::new();
        assert!(!p.is_done());
        p.set("Test", 0.5);
        assert_eq!(p.step(), "Test");
        assert!((p.fraction() - 0.5).abs() < 0.01);

        p.advance(2.0);
        assert_eq!(p.fraction(), 1.0, "must clamp above one");
        p.advance(-1.0);
        assert_eq!(p.fraction(), 0.0, "must clamp below zero");

        p.finish();
        assert!(p.is_done());
        assert_eq!(p.fraction(), 1.0);
    }

    /// A missing database must produce a message a non-technical reader can
    /// act on, not a bare io error.
    #[test]
    fn missing_database_explains_itself() {
        let mut cfg = Config::default();
        cfg.lookup.database = "definitely-not-here.scdb".into();
        let p = Progress::new();
        let err = match load(&cfg, &p) {
            Err(e) => e,
            Ok(_) => panic!("a missing database must not load"),
        };
        assert!(err.contains("Datenbank"), "{err}");
        assert!(err.contains("synth-db"), "should name the fix: {err}");
    }
}
