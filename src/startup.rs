//! Loading, done off the main thread.
//!
//! Opening the database, building the Bloom filter and reading back existing
//! hits all happen before the search can begin. Doing that before opening the
//! window means the application shows nothing at all while it works — fine for
//! a 5M-record database at 0.3 seconds, indistinguishable from a crash at 50M.
//!
//! So the window opens first and watches [`Progress`] while this runs behind it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::hits::{Hit, HitWriter};
use crate::lookup::{Bloom, Database};

/// Records in the practice database the window offers to build.
///
/// Enough that the search behaves like the real thing — the filter is built,
/// the lookups miss the way they will always miss — and small enough that the
/// file is 145 MB and the wait is a second.
pub const PRACTICE_RECORDS: usize = 5_000_000;

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
    /// Where the database should have been, when that is what went wrong.
    ///
    /// A missing file is the one failure the window can repair by itself, and
    /// it has to be told apart from a corrupt or unreadable one — offering to
    /// overwrite a database that is merely damaged would destroy a real dump
    /// somebody spent hours downloading.
    missing_db: Mutex<Option<PathBuf>>,
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
            missing_db: Mutex::new(None),
        }
    }

    /// Clears a finished run so the same handle can carry a second attempt.
    ///
    /// The window builds a database and then loads again through the very
    /// screen that reported the failure; without this the loading screen would
    /// open already finished, already holding the old error.
    pub fn restart(&self) {
        if let Ok(mut s) = self.step.lock() {
            *s = "Startet …".to_string();
        }
        if let Ok(mut e) = self.error.lock() {
            *e = None;
        }
        if let Ok(mut m) = self.missing_db.lock() {
            *m = None;
        }
        self.permille.store(0, Ordering::Relaxed);
        self.done.store(false, Ordering::Relaxed);
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

    /// Notes that the database file simply is not there, and where it was
    /// looked for. Set by [`load`] before it returns the failure.
    pub fn note_missing_db(&self, path: &Path) {
        if let Ok(mut m) = self.missing_db.lock() {
            *m = Some(path.to_path_buf());
        }
    }

    /// The path a repairable "no database" failure named, if that is what
    /// happened. `None` for every other failure.
    pub fn missing_db(&self) -> Option<PathBuf> {
        self.missing_db.lock().ok().and_then(|m| m.clone())
    }
}

/// A path as the reader should see it: relative ones joined onto the working
/// directory, so a message never names a file without saying where it is.
///
/// Not `canonicalize`, which fails on a path that does not exist yet — which
/// is exactly the case every caller here is reporting on.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(dir) => dir.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// Writes a practice database of `count` random addresses to `path`.
///
/// Offered by the window when there is no database at all. Reports through the
/// same [`Progress`] the loading screen already watches, so the bar simply
/// keeps moving from here into the load that follows.
pub fn create_practice_db(path: &Path, count: usize, progress: &Progress) -> Result<(), String> {
    progress.set(
        &format!(
            "Übungs-Datenbank wird erzeugt ({} Adressen) …",
            crate::util::group_digits(count as u64)
        ),
        0.15,
    );
    let records = crate::lookup::synthetic_records(count)?;

    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| {
                format!("Der Ordner {} ließ sich nicht anlegen: {e}", dir.display())
            })?;
        }
    }

    progress.set("Übungs-Datenbank wird geschrieben …", 0.6);
    crate::lookup::write_database(path, records)
        .map_err(|e| format!("Die Übungs-Datenbank ließ sich nicht schreiben: {e}"))?;
    progress.set("Übungs-Datenbank steht", 0.9);
    Ok(())
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
        // A file that is not there can be built; a file that is there and
        // wrong must not be touched. Only the first gets the marker that
        // turns the window's error screen into an offer.
        if e.kind() == std::io::ErrorKind::NotFound {
            progress.note_missing_db(&cfg.lookup.database);
            return format!(
                "Es ist noch keine Adress-Datenbank da.\n\n\
                 Erwartet:  {}\n\n\
                 Ohne sie kann die Suche nichts nachschlagen.",
                // Spelled out in full. The configured value is usually a bare
                // file name, and "funded.scdb" on its own tells a reader
                // nothing about where the program was actually looking.
                absolute(&cfg.lookup.database).display()
            );
        }
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
    /// act on, not a bare io error — and must mark itself repairable, which is
    /// what puts the "build one" button on the window's error screen.
    #[test]
    fn missing_database_explains_itself_and_offers_repair() {
        let mut cfg = Config::default();
        cfg.lookup.database = "definitely-not-here.scdb".into();
        let p = Progress::new();
        let err = match load(&cfg, &p) {
            Err(e) => e,
            Ok(_) => panic!("a missing database must not load"),
        };
        assert!(err.contains("Datenbank"), "{err}");
        assert_eq!(
            p.missing_db(),
            Some(cfg.lookup.database.clone()),
            "a missing file is the one failure the window can repair"
        );
    }

    /// The counterpart, and the one that matters for somebody's data: a
    /// database that exists but is damaged must NOT be offered for rebuilding.
    /// Overwriting it would discard a real dump that took hours to fetch.
    #[test]
    fn a_damaged_database_is_never_offered_for_rebuilding() {
        let dir = std::env::temp_dir().join(format!("sc-damaged-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("funded.scdb");
        std::fs::write(&path, b"this is not a database at all").unwrap();

        let mut cfg = Config::default();
        cfg.lookup.database = path.clone();
        let p = Progress::new();
        assert!(load(&cfg, &p).is_err(), "garbage must not load");
        assert_eq!(
            p.missing_db(),
            None,
            "a file that exists must never be proposed for overwriting"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The repair path end to end: build a practice database, then load it.
    ///
    /// Into a folder that does not exist yet, which is the real first-launch
    /// case — nothing under Application Support has been created at that point.
    #[test]
    fn a_built_practice_database_then_loads() {
        let dir = std::env::temp_dir().join(format!("sc-practice-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("Schatzsuche").join("funded.scdb");
        assert!(
            !dir.exists(),
            "the folder must be missing for this to mean anything"
        );

        let p = Progress::new();
        create_practice_db(&path, 20_000, &p).expect("practice database builds");
        assert!(path.exists(), "the file must be there afterwards");

        let db = Database::open(&path).expect("and must open as a database");
        assert_eq!(db.count(), 20_000);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// After a failure the same handle has to be usable for the second try.
    #[test]
    fn restart_clears_a_failed_run() {
        let p = Progress::new();
        p.note_missing_db(Path::new("gone.scdb"));
        p.fail("kaputt".into());
        assert!(p.is_done() && p.error().is_some() && p.missing_db().is_some());

        p.restart();
        assert!(!p.is_done(), "must look unfinished again");
        assert_eq!(p.error(), None);
        assert_eq!(p.missing_db(), None);
        assert_eq!(p.fraction(), 0.0);
    }
}
