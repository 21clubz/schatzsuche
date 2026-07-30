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
//! otherwise a newly created `hits.txt` can lose its directory entry and the
//! file becomes unreachable despite its contents being safe.
//!
//! **Die Datei ist eine Textdatei und keine Datenbank.** Sie hieß einmal
//! `hits.jsonl` und trug je Fund eine Zeile JSON. Das war für das Programm
//! bequem, das sie zurückliest, und für den Menschen davor eine Zumutung: wer
//! sie öffnete — und er öffnet sie genau einmal, nämlich an dem Tag, an dem
//! etwas gefunden wurde —, fand seine zwölf Wörter irgendwo zwischen
//! Anführungszeichen und geschweiften Klammern. Jetzt steht dort ein Absatz mit
//! beschrifteten Zeilen, den man liest, ohne etwas zu wissen. Zurücklesen kann
//! das Programm ihn trotzdem, siehe [`read_hits`].

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

// --- Das Dateiformat --------------------------------------------------------

/// Womit ein Fundsatz beginnt. Der Parser erkennt daran den Anfang eines
/// Satzes; alles davor — die Kopfzeilen der Datei — wird überlesen.
const RECORD_HEAD: &str = "TREFFER";

/// Wie breit die Beschriftungen stehen, damit die Werte untereinander fluchten.
const LABEL_W: usize = 22;

/// Was einmal ganz oben in einer neuen Fundliste steht.
///
/// Die Datei wird von jemandem geöffnet, der gerade erfahren hat, dass er eine
/// fremde Wallet in der Hand hält — der schlechteste denkbare Moment, um sich
/// die Regeln selbst zusammenzureimen. Also stehen sie dort, bevor der erste
/// Fund kommt.
const FILE_HEADER: &str = "\
Schatzsuche — gefundene Wallets
===============================

Hier steht alles, was zu einem Fund gehört: die Seed-Wörter, der private
Schlüssel, die Adresse und das Guthaben.

Wer diese Datei hat, hat das Geld. Sie gehört auf diesen Rechner und nirgendwo
sonst — nicht in eine Cloud, nicht in eine E-Mail, nicht in einen Chat. Das
Programm verschickt sie nie.

";

/// Ein Fund als lesbarer Absatz.
fn render(hit: &Hit) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "{RECORD_HEAD} — {}\n",
        util::human_utc(hit.timestamp_unix)
    ));
    s.push_str("--------------------------------------------------\n");
    let mut field = |name: &str, value: &str| {
        s.push_str(&format!("{:<LABEL_W$}{}\n", format!("{name}:"), value));
    };
    // Reihenfolge nach Dringlichkeit: zuerst, wie viel es ist, dann die zwei
    // Angaben, mit denen man drankommt. Der technische Rest steht unten.
    field("Guthaben", &hit.balance_btc);
    field("Adresse", &hit.address);
    field("Seed-Wörter", &hit.mnemonic);
    field("Privater Schlüssel", &hit.private_key_wif);
    field("Ableitungspfad", &hit.derivation_path);
    field("Adressart", &hit.script_type);
    field("Gefunden auf", &hit.hostname);
    field("Zeitstempel", &hit.timestamp_unix.to_string());
    field("Entropie", &hit.entropy_hex);
    s.push('\n');
    s
}

/// Liest eine Fundliste zurück.
///
/// Nimmt **beide** Formate: die beschrifteten Absätze von heute und die
/// JSON-Zeilen von früher, auch gemischt in derselben Datei. Das ist keine
/// Nachsicht, sondern der Umzugsweg — wessen `config.toml` weiter auf die alte
/// Datei zeigt, dem darf ein Fund nicht aus dem Fenster verschwinden, nur weil
/// das Format gewechselt hat.
///
/// Kaputte Sätze werden übersprungen statt den Lauf abzubrechen: eine halb
/// geschriebene Zeile — volle Platte, Stromausfall mitten im Schreiben — darf
/// nicht die Funde davor unlesbar machen.
pub fn read_hits(path: &Path) -> io::Result<Vec<Hit>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = File::open(path)?;
    let mut out = Vec::new();
    let mut fields: Option<Vec<(String, String)>> = None;

    for line in BufReader::new(f).lines() {
        let line = line?;
        let trimmed = line.trim();

        if trimmed.starts_with(RECORD_HEAD) {
            if let Some(done) = fields.replace(Vec::new()) {
                out.extend(hit_from_fields(&done));
            }
            continue;
        }
        // Eine Zeile aus der JSONL-Zeit.
        if trimmed.starts_with('{') {
            if let Ok(h) = serde_json::from_str::<Hit>(trimmed) {
                out.push(h);
            }
            continue;
        }
        let Some(fields) = fields.as_mut() else {
            continue; // Kopfzeilen, vor dem ersten Fund.
        };
        if let Some((k, v)) = trimmed.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                fields.push((k.to_string(), v.to_string()));
            }
        }
    }
    if let Some(done) = fields {
        out.extend(hit_from_fields(&done));
    }
    Ok(out)
}

/// Baut einen Fund aus den gelesenen Zeilen — oder `None`, wenn das
/// Wesentliche fehlt.
///
/// Wesentlich sind Adresse und Wörter. Ohne sie ist der Satz kein Fund,
/// sondern Text; alles andere wird notfalls hergeleitet oder bleibt leer.
fn hit_from_fields(fields: &[(String, String)]) -> Option<Hit> {
    let get = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let address = get("Adresse")?.to_string();
    let mnemonic = get("Seed-Wörter")?.to_string();
    let derivation_path = get("Ableitungspfad").unwrap_or_default().to_string();
    // Aus dem BTC-Betrag zurückgerechnet statt zusätzlich in Satoshi
    // hingeschrieben: zwei Zahlen für denselben Betrag in einer Datei, die
    // jemand von Hand lesen soll, sind eine Frage zu viel.
    let balance_sats = get("Guthaben").and_then(sats_from_btc).unwrap_or(0);
    let timestamp_unix = get("Zeitstempel")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    Some(Hit {
        id: Hit::make_id(&address, &derivation_path),
        timestamp: util::rfc3339(timestamp_unix),
        timestamp_unix,
        hostname: get("Gefunden auf").unwrap_or("unknown").to_string(),
        derivation_path,
        script_type: get("Adressart").unwrap_or_default().to_string(),
        address,
        balance_sats,
        balance_btc: util::format_btc(balance_sats),
        mnemonic,
        entropy_hex: get("Entropie").unwrap_or_default().to_string(),
        private_key_wif: get("Privater Schlüssel").unwrap_or_default().to_string(),
    })
}

/// `"1.33700000 BTC"` zurück in Satoshi.
///
/// Nachkommastellen werden auf acht gebracht, statt eine krumme Angabe
/// abzulehnen: wer die Datei von Hand bearbeitet und `0.5 BTC` hineinschreibt,
/// meint eine halbe, nicht fünf Satoshi.
fn sats_from_btc(s: &str) -> Option<u64> {
    let num = s.split_whitespace().next()?;
    let (whole, frac) = num.split_once('.').unwrap_or((num, "0"));
    let mut frac: String = frac.chars().take(8).collect();
    while frac.len() < 8 {
        frac.push('0');
    }
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(100_000_000)?
        .checked_add(frac.parse::<u64>().ok()?)
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
        let block = render(hit);

        append_record(&self.primary, &block)?;

        let backup_err = match &self.backup {
            Some(p) => append_record(p, &block).err(),
            None => None,
        };
        Ok(backup_err)
    }

    /// Reads every persisted hit back. Used by `--test-persistence` and to
    /// repopulate the UI on restart.
    pub fn load_all(&self) -> io::Result<Vec<Hit>> {
        read_hits(&self.primary)
    }
}

/// Hängt einen Fundsatz an; in eine noch leere Datei kommen zuerst die
/// Kopfzeilen.
///
/// Kopfzeilen und Satz gehen als **ein** Schreibvorgang hinaus, damit im
/// Anhänge-Modus nichts dazwischenrutschen kann. Zwei Funde in derselben
/// Millisekunde könnten die Kopfzeilen theoretisch doppelt sehen — bei einem
/// Programm, dessen erwarteter Abstand zwischen zwei Funden das Alter des
/// Universums übersteigt, ist das eine Zeile Kommentar wert und keine Sperre.
fn append_record(path: &Path, block: &str) -> io::Result<()> {
    let fresh = fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true);
    let mut bytes = String::new();
    if fresh {
        bytes.push_str(FILE_HEADER);
    }
    bytes.push_str(block);
    append_durable(path, bytes.as_bytes())
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
        self.problem().is_none()
    }

    /// Which part is broken, in one sentence — or `None` when everything holds.
    ///
    /// [`PersistenceReport::ok`] answers "may I trust the disk?", which is the
    /// only thing the search itself needs. A window has to answer "what exactly
    /// went wrong?", and a bare `false` cannot. The order is by severity: losing
    /// the first copy is not survivable, losing the second one is.
    pub fn problem(&self) -> Option<String> {
        let mode_ok = |m: Option<u32>| m.map(|m| m == 0o600).unwrap_or(true);

        if !self.primary_readback_ok {
            return Some(format!(
                "{} konnte nicht zurückgelesen werden",
                self.primary.display()
            ));
        }
        if !mode_ok(self.primary_mode) {
            return Some(format!(
                "Rechte an {} sind {:o}, erwartet 600 — die Datei ist für andere lesbar",
                self.primary.display(),
                self.primary_mode.unwrap_or(0)
            ));
        }
        if let Some(e) = &self.backup_error {
            return Some(format!("Sicherungskopie fehlgeschlagen: {e}"));
        }
        let Some(backup) = &self.backup else {
            return None;
        };
        if !self.backup_readback_ok {
            return Some(format!(
                "Sicherungskopie {} konnte nicht zurückgelesen werden",
                backup.display()
            ));
        }
        if !mode_ok(self.backup_mode.flatten()) {
            return Some(format!(
                "Rechte an der Sicherungskopie {} sind {:o}, erwartet 600",
                backup.display(),
                self.backup_mode.flatten().unwrap_or(0)
            ));
        }
        None
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
        let hits = read_hits(p)?;
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

    /// Ein Bericht muss den kaputten Teil benennen können. `false` allein
    /// reicht dem Fenster nicht: der Nutzer erfährt sonst, dass etwas nicht
    /// stimmt, aber nicht was.
    #[test]
    fn problem_names_the_broken_part() {
        let healthy = || PersistenceReport {
            primary: PathBuf::from("/tmp/hits.txt"),
            primary_mode: Some(0o600),
            primary_readback_ok: true,
            backup: Some(PathBuf::from("/tmp/backup.jsonl")),
            backup_mode: Some(Some(0o600)),
            backup_readback_ok: true,
            backup_error: None,
        };

        assert_eq!(healthy().problem(), None);
        assert!(healthy().ok());

        let loose = PersistenceReport {
            primary_mode: Some(0o644),
            ..healthy()
        };
        let msg = loose.problem().expect("lockere Rechte müssen auffallen");
        assert!(msg.contains("644"), "{msg}");
        assert!(msg.contains("600"), "{msg}");
        assert!(!loose.ok());

        let unread = PersistenceReport {
            primary_readback_ok: false,
            ..healthy()
        };
        assert!(unread.problem().unwrap().contains("hits.txt"));

        let no_backup = PersistenceReport {
            backup_error: Some("Platte voll".into()),
            ..healthy()
        };
        assert!(no_backup.problem().unwrap().contains("Platte voll"));

        // Ohne konfigurierte Sicherungskopie darf deren Fehlen nichts melden.
        let single = PersistenceReport {
            backup: None,
            backup_mode: None,
            backup_readback_ok: false,
            ..healthy()
        };
        assert_eq!(single.problem(), None);
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

    /// **Der Zweck der ganzen Datei.** Wer sie öffnet, hat gerade eine fremde
    /// Wallet gefunden und sucht drei Dinge: die Wörter, die Adresse und die
    /// Auskunft, wie viel darauf liegt. Alle drei müssen ohne Werkzeug lesbar
    /// dastehen — und die Klammern von früher dürfen weg bleiben.
    #[test]
    fn the_file_reads_like_a_note_to_a_human() {
        let d = tmpdir("readable");
        let p = d.join("hits.txt");
        let w = HitWriter::new(p.clone(), None);
        w.persist(&Hit::synthetic()).unwrap();

        let text = fs::read_to_string(&p).unwrap();
        assert!(
            !text.contains('{'),
            "es steht wieder JSON in der Datei:\n{text}"
        );
        for expected in [
            "Wer diese Datei hat, hat das Geld.", // die Warnung im Kopf
            "TREFFER",
            "Guthaben:",
            "1.33700000 BTC",
            "Seed-Wörter:",
            "abandon abandon",
            "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
        ] {
            assert!(text.contains(expected), "„{expected}“ fehlt in:\n{text}");
        }

        fs::remove_dir_all(&d).ok();
    }

    /// Lesbar allein genügt nicht: das Fenster liest die Datei beim Start
    /// zurück, und ein Fund, der dabei seine Wörter oder sein Guthaben
    /// verliert, ist schlimmer als keiner.
    #[test]
    fn every_field_survives_the_round_trip() {
        let d = tmpdir("roundtrip");
        let w = HitWriter::new(d.join("hits.txt"), None);
        let mut hit = Hit::synthetic();
        hit.private_key_wif = "L1aW4aubDFB7yfras2S1mN3bqg9nwySY8nkoLmJebSLD5BWv3ENZ".into();
        hit.hostname = "meiner".into();
        w.persist(&hit).unwrap();

        let back = w.load_all().unwrap();
        assert_eq!(back.len(), 1);
        let b = &back[0];
        assert_eq!(b.mnemonic, hit.mnemonic);
        assert_eq!(b.address, hit.address);
        assert_eq!(b.private_key_wif, hit.private_key_wif);
        assert_eq!(b.balance_sats, hit.balance_sats);
        assert_eq!(b.balance_btc, hit.balance_btc);
        assert_eq!(b.derivation_path, hit.derivation_path);
        assert_eq!(b.script_type, hit.script_type);
        assert_eq!(b.hostname, hit.hostname);
        assert_eq!(b.timestamp_unix, hit.timestamp_unix);
        assert_eq!(b.timestamp, hit.timestamp);
        assert_eq!(b.entropy_hex, hit.entropy_hex);
        assert_eq!(b.id, hit.id);
        // Und der Selbsttest-Eintrag muss als solcher erkennbar bleiben, sonst
        // steht er beim nächsten Start als echter Fund im Fenster.
        assert!(b.is_synthetic());

        fs::remove_dir_all(&d).ok();
    }

    /// Die Datei aus der JSONL-Zeit muss weiter lesbar sein — auch gemischt
    /// mit neuen Sätzen. Sonst verschwindet ein alter Fund bei einem Update
    /// wortlos aus dem Fenster.
    #[test]
    fn hits_written_by_the_old_version_still_load() {
        let d = tmpdir("legacy");
        let p = d.join("hits.jsonl");
        let mut old = Hit::synthetic();
        old.address = "bc1qalt".into();
        old.id = Hit::make_id(&old.address, &old.derivation_path);
        let mut line = serde_json::to_string(&old).unwrap();
        line.push('\n');
        fs::write(&p, &line).unwrap();

        let w = HitWriter::new(p.clone(), None);
        w.persist(&Hit::synthetic()).unwrap();

        let all = w.load_all().unwrap();
        assert_eq!(all.len(), 2, "beide Formate müssen ankommen");
        assert!(all.iter().any(|h| h.address == "bc1qalt"));
        assert!(all
            .iter()
            .any(|h| h.address == "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"));

        fs::remove_dir_all(&d).ok();
    }

    /// Ein abgeschnittener letzter Satz — volle Platte, Stromausfall — darf
    /// die Funde davor nicht mitnehmen.
    #[test]
    fn a_truncated_record_costs_only_itself() {
        let d = tmpdir("truncated");
        let p = d.join("hits.txt");
        let w = HitWriter::new(p.clone(), None);
        w.persist(&Hit::synthetic()).unwrap();

        let mut text = fs::read_to_string(&p).unwrap();
        text.push_str("TREFFER — 30.07.2026, 05:54 Uhr (UTC)\nGuthaben:  1.0");
        fs::write(&p, text).unwrap();

        let all = w.load_all().unwrap();
        assert_eq!(all.len(), 1, "der vollständige Fund muss überleben");

        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn btc_amounts_convert_back_to_satoshi() {
        assert_eq!(sats_from_btc("1.33700000 BTC"), Some(133_700_000));
        assert_eq!(sats_from_btc("0.00000001 BTC"), Some(1));
        assert_eq!(sats_from_btc("21.00000000"), Some(2_100_000_000));
        // Von Hand gekürzt geschrieben: eine halbe, nicht fünf Satoshi.
        assert_eq!(sats_from_btc("0.5 BTC"), Some(50_000_000));
        assert_eq!(sats_from_btc("nichts"), None);
        assert_eq!(sats_from_btc(""), None);
        // Unsinn darf überlaufen wollen, aber nicht dürfen.
        assert_eq!(sats_from_btc("99999999999999999999 BTC"), None);
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = Hit::make_id("bc1qaaa", "m/84'/0'/0'/0/0");
        let b = Hit::make_id("bc1qaaa", "m/84'/0'/0'/0/1");
        assert_eq!(a, Hit::make_id("bc1qaaa", "m/84'/0'/0'/0/0"));
        assert_ne!(a, b);
    }
}
