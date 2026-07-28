//! Hit persistence. Written before anything else happens.
//!
//! The ordering is a durability contract, not a style choice: an alert that
//! arrives while the seed is still sitting in a page cache that a power cut
//! will discard is worse than useless, because it tells you a fortune existed
//! and that you no longer have it.
//!
//! On macOS `fsync(2)` returns once the data reaches the drive, but *not* once
//! the drive has committed it — the disk's own write cache is still volatile.
//! `F_FULLFSYNC` is the only call that forces a platter/flash commit, so that
//! is what [`append_durable`] issues. The containing directory is synced too,
//! otherwise a newly created `hits.jsonl` can lose its directory entry and the
//! file becomes unreachable despite its contents being safe.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::util;

/// Everything known about a match. The mnemonic never leaves this struct's
/// local files; see [`crate::alert::AlertPayload`] for what is allowed out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    /// Stable identity, derived from address and path, used to deduplicate
    /// alert retries.
    pub id: String,
    pub timestamp: String,
    pub timestamp_unix: u64,
    pub hostname: String,
    pub derivation_path: String,
    pub script_type: String,
    pub address: String,
    pub balance_sats: u64,
    pub balance_btc: String,
    /// LOCAL ONLY.
    pub mnemonic: String,
    /// LOCAL ONLY.
    pub entropy_hex: String,
    /// LOCAL ONLY.
    pub private_key_wif: String,
}

impl Hit {
    /// Deterministic id: the same address/path pair always yields the same id,
    /// so a retried alert is recognised as a duplicate rather than re-fired.
    pub fn make_id(address: &str, path: &str) -> String {
        let mut h = Sha256::new();
        h.update(address.as_bytes());
        h.update(b"|");
        h.update(path.as_bytes());
        util::hex(&h.finalize()[..12])
    }

    /// A synthetic hit for `--test-alert` and `--test-persistence`.
    ///
    /// The mnemonic is the published all-zero BIP-39 test vector, which is
    /// famously empty, so a leak of this record costs nothing.
    pub fn synthetic() -> Hit {
        let now = util::unix_now();
        let mnemonic = "abandon abandon abandon abandon abandon abandon \
                        abandon abandon abandon abandon abandon about"
            .to_string();
        let address = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu".to_string();
        let path = "m/84'/0'/0'/0/0".to_string();
        Hit {
            id: Hit::make_id(&address, &path),
            timestamp: util::rfc3339(now),
            timestamp_unix: now,
            hostname: util::hostname(),
            derivation_path: path,
            script_type: "p2wpkh".to_string(),
            address,
            balance_sats: 133_700_000,
            balance_btc: util::format_btc(133_700_000),
            mnemonic,
            entropy_hex: "00000000000000000000000000000000".to_string(),
            private_key_wif: "TEST-ONLY-NOT-A-REAL-KEY".to_string(),
        }
    }

    /// True for records produced by the self-tests.
    pub fn is_synthetic(&self) -> bool {
        self.entropy_hex == "00000000000000000000000000000000"
    }
}

pub struct HitWriter {
    primary: PathBuf,
    backup: Option<PathBuf>,
}

impl HitWriter {
    pub fn new(primary: PathBuf, backup: Option<PathBuf>) -> HitWriter {
        HitWriter { primary, backup }
    }

    pub fn primary_path(&self) -> &Path {
        &self.primary
    }

    pub fn backup_path(&self) -> Option<&Path> {
        self.backup.as_deref()
    }

    /// Writes and durably commits a hit to the primary file and, if configured,
    /// the backup.
    ///
    /// Returns only after both copies are on stable storage. A backup failure
    /// is reported but does not discard the primary write, which has already
    /// succeeded — losing the second copy is survivable, losing the first is
    /// not.
    pub fn persist(&self, hit: &Hit) -> io::Result<Option<io::Error>> {
        let mut line = serde_json::to_string(hit)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        append_durable(&self.primary, line.as_bytes())?;

        let backup_err = match &self.backup {
            Some(p) => append_durable(p, line.as_bytes()).err(),
            None => None,
        };
        Ok(backup_err)
    }

    /// Reads every persisted hit back. Used by `--test-persistence` and to
    /// repopulate the UI on restart.
    pub fn load_all(&self) -> io::Result<Vec<Hit>> {
        read_jsonl(&self.primary)
    }
}

/// Appends bytes to `path` and does not return until they are durable.
///
/// The file is created 0600 and forced back to 0600 if it already existed with
/// looser bits, so a hit file cannot silently be world-readable.
pub fn append_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }

    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(path)?;

    // Windows has no POSIX mode bits. Files created under the user's roaming
    // profile inherit an ACL that already excludes other users, so there is
    // nothing to tighten there; on Unix the bits are enforced even if the file
    // already existed with looser ones.
    #[cfg(unix)]
    {
        let perms = fs::metadata(path)?.permissions();
        if perms.mode() & 0o777 != 0o600 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }

    f.write_all(bytes)?;
    f.flush()?;
    full_fsync(&f)?;

    // Directory sync is a POSIX concept; Windows cannot open a directory as a
    // file and commits directory metadata with the file write.
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        fsync_dir(dir)?;
    }
    Ok(())
}

/// Forces a real hardware commit.
///
/// `File::sync_all` maps to `fsync(2)`, which on macOS leaves the data in the
/// drive's volatile cache. `F_FULLFSYNC` is the documented way to get a genuine
/// barrier. Some filesystems (notably network mounts) reject it, so a refusal
/// falls back to `fsync` rather than failing the write.
fn full_fsync(f: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: fd is owned by `f` and valid for the duration of the call.
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC) };
        if rc != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            // Not supported on this filesystem: fsync is the best available.
            Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::EPERM) => {}
            _ => return Err(err),
        }
    }
    f.sync_all()
}

/// Syncs a directory so newly created entries survive a crash.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> io::Result<()> {
    let d = File::open(dir)?;
    match d.sync_all() {
        Ok(()) => Ok(()),
        // Some filesystems refuse to sync a directory handle; the file data
        // itself is already committed, so this is not fatal.
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTSUP)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Reads a JSONL file, skipping malformed lines rather than failing the run.
pub fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = File::open(path)?;
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<T>(&line) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Result of `--test-persistence`.
pub struct PersistenceReport {
    pub primary: PathBuf,
    /// POSIX mode bits, or `None` where the platform has none.
    pub primary_mode: Option<u32>,
    pub primary_readback_ok: bool,
    pub backup: Option<PathBuf>,
    pub backup_mode: Option<Option<u32>>,
    pub backup_readback_ok: bool,
    pub backup_error: Option<String>,
}

impl PersistenceReport {
    pub fn ok(&self) -> bool {
        let mode_ok = |m: Option<u32>| m.map(|m| m == 0o600).unwrap_or(true);
        self.primary_readback_ok
            && mode_ok(self.primary_mode)
            && self.backup_error.is_none()
            && (self.backup.is_none()
                || (self.backup_readback_ok && mode_ok(self.backup_mode.flatten())))
    }
}

/// Writes a dummy hit, reads it back, and checks permissions on both copies.
pub fn self_test(writer: &HitWriter) -> io::Result<PersistenceReport> {
    let hit = Hit::synthetic();
    let backup_err = writer.persist(&hit)?;

    let mode_of = |_p: &Path| -> io::Result<Option<u32>> {
        #[cfg(unix)]
        {
            Ok(Some(fs::metadata(_p)?.permissions().mode() & 0o777))
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    };
    let contains = |p: &Path| -> io::Result<bool> {
        let hits: Vec<Hit> = read_jsonl(p)?;
        Ok(hits
            .iter()
            .any(|h| h.id == hit.id && h.mnemonic == hit.mnemonic))
    };

    let primary_mode = mode_of(&writer.primary)?;
    let primary_readback_ok = contains(&writer.primary)?;

    let (backup_mode, backup_readback_ok) = match (&writer.backup, &backup_err) {
        (Some(p), None) => (Some(mode_of(p)?), contains(p)?),
        _ => (None, false),
    };

    Ok(PersistenceReport {
        primary: writer.primary.clone(),
        primary_mode,
        primary_readback_ok,
        backup: writer.backup.clone(),
        backup_mode,
        backup_readback_ok,
        backup_error: backup_err.map(|e| e.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sc-hits-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn persists_with_backup_and_correct_mode() {
        let d = tmpdir("basic");
        let w = HitWriter::new(d.join("hits.jsonl"), Some(d.join("backup.jsonl")));

        let report = self_test(&w).unwrap();
        assert!(
            report.ok(),
            "self-test failed: mode={:?} primary_ok={} backup_ok={} err={:?}",
            report.primary_mode,
            report.primary_readback_ok,
            report.backup_readback_ok,
            report.backup_error
        );
        #[cfg(unix)]
        {
            assert_eq!(report.primary_mode, Some(0o600));
            assert_eq!(report.backup_mode, Some(Some(0o600)));
        }

        fs::remove_dir_all(&d).ok();
    }

    /// A pre-existing world-readable file must be tightened, not trusted.
    #[cfg(unix)]
    #[test]
    fn tightens_loose_permissions() {
        let d = tmpdir("perms");
        let p = d.join("hits.jsonl");
        File::create(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();

        append_durable(&p, b"{}\n").unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn appends_rather_than_truncates() {
        let d = tmpdir("append");
        let w = HitWriter::new(d.join("hits.jsonl"), None);
        w.persist(&Hit::synthetic()).unwrap();
        let mut second = Hit::synthetic();
        second.address = "bc1qsecond".into();
        second.id = Hit::make_id(&second.address, &second.derivation_path);
        w.persist(&second).unwrap();

        let all = w.load_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_ne!(all[0].id, all[1].id);

        fs::remove_dir_all(&d).ok();
    }

    /// A broken backup path must not cost us the primary record.
    #[test]
    fn backup_failure_preserves_primary() {
        let d = tmpdir("badbackup");
        // A path whose parent is an existing *file* cannot be created.
        let blocker = d.join("blocker");
        File::create(&blocker).unwrap();
        let w = HitWriter::new(d.join("hits.jsonl"), Some(blocker.join("nope.jsonl")));

        let err = w.persist(&Hit::synthetic()).unwrap();
        assert!(
            err.is_some(),
            "expected the backup write to report an error"
        );
        assert_eq!(
            w.load_all().unwrap().len(),
            1,
            "primary must still hold the hit"
        );

        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = Hit::make_id("bc1qaaa", "m/84'/0'/0'/0/0");
        let b = Hit::make_id("bc1qaaa", "m/84'/0'/0'/0/1");
        assert_eq!(a, Hit::make_id("bc1qaaa", "m/84'/0'/0'/0/0"));
        assert_ne!(a, b);
    }
}
