//! The parallel search loop.
//!
//! Each worker owns everything it touches — a [`Deriver`], an entropy buffer
//! and a local counter block — so the inner loop performs no allocation, takes
//! no lock, and executes no atomic operation. Counters reach the shared
//! [`Stats`] only on batch boundaries.
//!
//! Entropy comes straight from the OS. `getrandom` is read in 4 KiB blocks
//! rather than 32 bytes at a time, purely to amortise the syscall: every byte
//! consumed is still OS randomness, nothing is expanded through a PRNG, and no
//! generator is seeded. That is what makes the search uniform over the keyspace
//! — and, incidentally, what makes it hopeless.

use std::io::Write;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::address::{encode, wif_compressed};
use crate::alert::{AlertPayload, Dispatcher};
use crate::bip39::WordCount;
use crate::deriver::{Deriver, Origin};
use crate::hits::{Hit, HitWriter};
use crate::lookup::{Bloom, Database};
use crate::stats::{apply_priority, Control, Local, Stats, FLUSH_EVERY};
use crate::util;

/// Bytes of OS entropy buffered per worker.
const ENTROPY_BUF: usize = 4096;

/// Messages from workers to whatever is driving the display.
pub enum Event {
    Hit(Box<Hit>),
    /// A hit was found but could not be fully persisted. This is the one
    /// failure mode that must never be swallowed.
    PersistFailure {
        hit: Box<Hit>,
        error: String,
    },
    /// The backup copy failed while the primary succeeded.
    BackupFailure {
        id: String,
        error: String,
    },
}

pub struct Shared {
    pub stats: Arc<Stats>,
    pub control: Arc<Control>,
    pub bloom: Arc<Bloom>,
    pub db: Arc<Database>,
    pub writer: Arc<HitWriter>,
    pub dispatcher: Arc<Dispatcher>,
    pub events: Sender<Event>,
    pub word_count: WordCount,
}

/// A block of OS entropy, consumed sequentially and refilled from the kernel.
struct Entropy {
    buf: [u8; ENTROPY_BUF],
    pos: usize,
}

impl Entropy {
    fn new() -> Entropy {
        Entropy {
            buf: [0u8; ENTROPY_BUF],
            // Start exhausted so the first draw refills.
            pos: ENTROPY_BUF,
        }
    }

    #[inline]
    fn fill(&mut self, out: &mut [u8]) {
        if self.pos + out.len() > self.buf.len() {
            // A failure here means the OS RNG is unavailable. Continuing with
            // degraded randomness would silently invalidate the whole run, so
            // this is one of the few places worth aborting over.
            getrandom::getrandom(&mut self.buf).expect("OS entropy source unavailable");
            self.pos = 0;
        }
        out.copy_from_slice(&self.buf[self.pos..self.pos + out.len()]);
        self.pos += out.len();
    }
}

/// Runs the search across `threads` workers until [`Stats::request_stop`].
///
/// rayon owns the pool as specified. Note that its work-stealing has nothing to
/// steal here: every worker runs an identical, unbounded loop, so the fastest
/// arrangement is precisely one long-lived task per thread with no shared
/// queue. `broadcast` gives exactly that.
pub fn run(shared: Arc<Shared>, threads: usize) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("collider-{i}"))
        .build()
        .expect("failed to build thread pool");

    pool.broadcast(|ctx| worker(&shared, ctx.index()));
}

fn worker(shared: &Shared, index: usize) {
    let mut deriver = Deriver::new();
    let mut entropy_src = Entropy::new();
    let mut local = Local::default();

    // Scheduling priority is per thread and can be changed while running, so
    // the worker re-applies it whenever the setting moves.
    let mut priority = shared.control.priority();
    apply_priority(priority);

    // Read per candidate, not hoisted: that is what lets the mnemonic length
    // change while the search runs. A relaxed atomic load against the 700
    // microseconds of PBKDF2 that follow it does not register.
    let mut entropy = [0u8; 32];

    // Reused across candidates; a hit is so rare that this never grows.
    let mut found: Vec<([u8; 20], Origin, u64)> = Vec::new();
    let mut since_flush = 0u64;

    loop {
        let began = std::time::Instant::now();
        let wc = shared.control.word_count();
        let n_bytes = wc.entropy_bytes();
        entropy_src.fill(&mut entropy[..n_bytes]);
        deriver.stretch(&entropy[..n_bytes], wc);

        found.clear();
        let bloom = &shared.bloom;
        let db = &shared.db;
        let stats = &shared.stats;

        let produced = deriver.walk(shared.control.addresses_per_path(), |hash, origin| {
            // Stage 1: one cache line. This is the only lookup work the
            // overwhelming majority of candidates ever performs.
            if !bloom.contains(origin.kind(), hash) {
                return;
            }
            stats.note_bloom_hit();

            // Stage 2: confirm against the sorted file. Reached roughly once
            // per 1/fpr addresses, so its cost is irrelevant to throughput.
            //
            // An empty wallet is not a hit. Address dumps carry plenty of
            // addresses that were funded once and swept long ago; waking
            // somebody at four in the morning for a balance of zero is a false
            // alarm, and one recorded in hits.jsonl is worse — it makes the
            // file a place where real finds hide among noise.
            if let Some(balance) = db.lookup(origin.kind(), hash) {
                if balance > 0 {
                    found.push((*hash, origin, balance));
                }
            }
        });

        local.seeds += 1;
        local.addresses += produced as u64;
        since_flush += 1;

        if !found.is_empty() {
            for (hash, origin, balance) in found.drain(..) {
                report(
                    shared,
                    &deriver,
                    &entropy[..n_bytes],
                    &hash,
                    origin,
                    balance,
                );
            }
        }

        // Throttling: rest for as long as this candidate took, scaled to the
        // duty cycle. Sliced, so a stop is noticed within a tick rather than
        // after a full nap.
        let rest = shared.control.rest_after(began.elapsed());
        let throttled = !rest.is_zero();
        if throttled {
            let mut left = rest;
            let slice = std::time::Duration::from_millis(40);
            while !left.is_zero() && !shared.control.stopping() {
                let step = left.min(slice);
                std::thread::sleep(step);
                left -= step;
            }
        }

        // Throttled runs take the checks every candidate rather than every few
        // thousand: at a one-percent duty cycle the periodic path would come
        // round about once a minute, so the pause button, the core slider and
        // the counters would all appear dead. Against a nap measured in tens of
        // milliseconds, checking a handful of atomics costs nothing.
        if throttled || since_flush >= FLUSH_EVERY {
            local.flush(&shared.stats);
            since_flush = 0;

            if shared.control.stopping() {
                break;
            }

            // Priority is a per-thread setting, so it is re-applied here rather
            // than only at startup — the operator can change it mid-run.
            let want = shared.control.priority();
            if want != priority {
                priority = want;
                apply_priority(priority);
            }

            // Counters are published before parking, so the display shows the
            // true total while idle rather than a stale one. A worker parks
            // when the search is paused *or* when it sits above the active core
            // count, which is what makes the core count adjustable live.
            shared.control.wait_while_idle(index);
            if shared.control.stopping() {
                break;
            }
        }
    }

    local.flush(&shared.stats);
}

/// Handles one confirmed match.
///
/// The ordering below is the durability contract from the spec and must not be
/// rearranged: persist and fsync first, surface locally second, alert last.
/// Persisting happens on the worker thread rather than being handed to a queue
/// precisely so that no hit can be sitting in a channel when the process dies.
pub(crate) fn report(
    shared: &Shared,
    deriver: &Deriver,
    entropy: &[u8],
    hash: &[u8; 20],
    origin: Origin,
    balance: u64,
) {
    shared.stats.note_confirmed();

    let address = encode(origin.kind(), hash);
    let path = origin.path();
    let now = util::unix_now();

    let wif = deriver
        .private_key_at(origin)
        .map(|sk| wif_compressed(&sk.secret_bytes()))
        .unwrap_or_else(|| "<derivation failed>".to_string());

    let hit = Hit {
        id: Hit::make_id(&address, &path),
        timestamp: util::rfc3339(now),
        timestamp_unix: now,
        hostname: util::hostname(),
        derivation_path: path,
        script_type: origin.kind().as_str().to_string(),
        address,
        balance_sats: balance,
        balance_btc: util::format_btc(balance),
        mnemonic: deriver.mnemonic().to_string(),
        entropy_hex: util::hex(entropy),
        private_key_wif: wif,
    };

    // 1-3. Write, fsync file and directory, mirror to the backup.
    match shared.writer.persist(&hit) {
        Ok(backup_err) => {
            if let Some(e) = backup_err {
                let _ = shared.events.send(Event::BackupFailure {
                    id: hit.id.clone(),
                    error: e.to_string(),
                });
            }
        }
        Err(e) => {
            // The primary write failed. Still alert — a notification without a
            // stored seed is nearly worthless, but it is strictly better than
            // silence, and it tells the operator to intervene immediately.
            let _ = shared.events.send(Event::PersistFailure {
                hit: Box::new(hit.clone()),
                error: e.to_string(),
            });
            // Auch hier läuten, und zwar vor dem Aussteigen. Vorher sprang die
            // Funktion an dieser Stelle heraus, bevor die Glocke kam — der
            // schlimmste Fall war damit der leiseste, was dem Kommentar drei
            // Zeilen weiter oben direkt widerspricht.
            bell();
            shared
                .dispatcher
                .dispatch_async(AlertPayload::from_hit(&hit));
            return;
        }
    }

    // 4. Surface it locally, where the terminal is ours and the seed is safe.
    let _ = shared.events.send(Event::Hit(Box::new(hit.clone())));

    // 5. Terminal bell, independent of whether a TUI is attached.
    bell();

    // 6. Only now the network. Asynchronous, so the worker returns to the loop
    // immediately — step 7, "the program keeps running".
    shared
        .dispatcher
        .dispatch_async(AlertPayload::from_hit(&hit));
}

/// Die Terminal-Glocke — aber nur, wenn jemand sie hören kann.
///
/// Vorher schrieb das hier bedingungslos `\x07` auf stdout. Aus dem Finder
/// gestartet hängt an stdout kein Terminal: das Byte ging ins Leere und sah im
/// Quelltext trotzdem aus wie eine Benachrichtigung. Wer die App als Bundle
/// laufen ließ, hatte damit ein akustisches Signal, das es gar nicht gab.
///
/// Im Fenster übernimmt [`crate::ui::feel::alarm`] diese Aufgabe.
///
/// Bei einer Umleitung in eine Datei oder Pipe ist `is_terminal()` ebenfalls
/// falsch, und das ist richtig so — in eine Logdatei gehört kein Steuerzeichen.
fn bell() {
    use std::io::IsTerminal;
    let mut out = std::io::stdout();
    if !out.is_terminal() {
        return;
    }
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_refills_and_never_repeats_within_a_block() {
        let mut e = Entropy::new();
        let mut seen = std::collections::HashSet::new();
        let mut buf = [0u8; 32];
        // Draw well past one buffer to exercise the refill path.
        for _ in 0..((ENTROPY_BUF / 32) * 3) {
            e.fill(&mut buf);
            assert!(seen.insert(buf), "OS entropy repeated a 32-byte draw");
        }
    }

    #[test]
    fn entropy_handles_partial_final_draw() {
        // 4096 is not a multiple of 24, so the tail must trigger a refill
        // rather than reading past the buffer.
        let mut e = Entropy::new();
        let mut buf = [0u8; 24];
        for _ in 0..400 {
            e.fill(&mut buf);
        }
    }

    /// Der Marker, an dem der Elternprozess die Ziehungen seines Kindes
    /// wiedererkennt. Die Testausgabe drumherum interessiert nicht.
    const DRAW_LINE: &str = "SC_DRAW ";

    /// Zwei Läufe des Programms dürfen nie dieselben Wörter würfeln.
    ///
    /// Das ist die Frage „sucht jeder Rechner nach denselben Wörtern?", und
    /// innerhalb eines Prozesses ist sie nicht zu beantworten: ein Generator,
    /// der beim Start immer gleich anfängt, liefert in *einem* Lauf lauter
    /// verschiedene Werte und in jedem Lauf dieselbe Folge — genau der Fehler,
    /// der eine solche Suche wertlos machen würde, und genau der, den
    /// [`entropy_refills_and_never_repeats_within_a_block`] nicht sieht.
    ///
    /// Der Test startet darum das Testprogramm zweimal neu, lässt jedes Kind
    /// wie ein Arbeiter würfeln und vergleicht die zwei Folgen. Zwei Prozesse
    /// sind das Nächste an zwei Rechnern, was sich in einem Test nachstellen
    /// lässt; identisch wären sie nur, wenn die Entropie nicht vom Betriebs-
    /// system käme.
    #[test]
    fn two_runs_of_the_program_never_draw_the_same_words() {
        // Die Kindrolle: würfeln und ausgeben, ohne selbst wieder zu starten.
        if std::env::var("SC_DRAW_CHILD").is_ok() {
            let mut src = Entropy::new();
            let mut buf = [0u8; 16];
            for _ in 0..64 {
                src.fill(&mut buf);
                println!("{DRAW_LINE}{}", util::hex(&buf));
            }
            return;
        }

        let exe = std::env::current_exe().expect("das Testprogramm muss sich selbst finden");
        let run_again = || -> Vec<String> {
            let out = std::process::Command::new(&exe)
                // Der volle Pfad, nicht der kurze Name: mit `--exact` muss der
                // Filter den ganzen Namen treffen, sonst läuft im Kind kein
                // Test und es kommt gar nichts zurück.
                .args([
                    "engine::tests::two_runs_of_the_program_never_draw_the_same_words",
                    "--exact",
                    "--nocapture",
                ])
                .env("SC_DRAW_CHILD", "1")
                .output()
                .expect("das Testprogramm liess sich nicht erneut starten");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.strip_prefix(DRAW_LINE).map(str::to_owned))
                .collect()
        };

        let first = run_again();
        let second = run_again();
        assert_eq!(first.len(), 64, "der erste Lauf hat nicht gewürfelt");
        assert_eq!(second.len(), 64, "der zweite Lauf hat nicht gewürfelt");

        // Nicht bloß „die Folgen sind verschieden": kein einziger Wert aus dem
        // einen Lauf darf im anderen vorkommen. Ein Generator mit fester
        // Startzahl fiele hier mit allen 64 Werten auf.
        let overlap = first
            .iter()
            .filter(|d| second.contains(d))
            .collect::<Vec<_>>();
        assert!(
            overlap.is_empty(),
            "zwei Läufe zogen {} gemeinsame Werte, zum Beispiel {}",
            overlap.len(),
            overlap[0]
        );
    }

    /// Eindeutig heißt nicht zufällig.
    ///
    /// [`entropy_refills_and_never_repeats_within_a_block`] würde einen
    /// hochzählenden Zähler durchlassen — dessen Werte sind alle verschieden.
    /// Hier wird darum die Verteilung geprüft, nicht die Eindeutigkeit: über
    /// 32 KiB müssen etwa halb so viele Einsen wie Bits herauskommen, jede der
    /// acht Bitstellen muss für sich ausgewogen sein, und jeder der 256
    /// möglichen Bytewerte muss vorkommen.
    ///
    /// Die Schranken sind absichtlich weit. Bei 262 144 Bits liegt die
    /// Standardabweichung der Einsen bei 256; erlaubt ist ein Prozent, also
    /// 2621 — mehr als das Zehnfache. Ein Zähler, ein festgeklemmtes Bit oder
    /// ein wiederholter Block reißt das um Größenordnungen, echter Zufall nie.
    #[test]
    fn the_entropy_is_evenly_spread_not_merely_distinct() {
        let mut e = Entropy::new();
        let mut bytes = Vec::with_capacity(32 * 1024);
        let mut buf = [0u8; 32];
        while bytes.len() < 32 * 1024 {
            e.fill(&mut buf);
            bytes.extend_from_slice(&buf);
        }

        let bits = bytes.len() * 8;
        let ones: usize = bytes.iter().map(|b| b.count_ones() as usize).sum();
        let off = ones.abs_diff(bits / 2);
        assert!(
            off < bits / 100,
            "{ones} Einsen auf {bits} Bits — {off} neben der Mitte, erlaubt sind {}",
            bits / 100
        );

        for bit in 0..8 {
            let set = bytes.iter().filter(|b| *b & (1 << bit) != 0).count();
            let off = set.abs_diff(bytes.len() / 2);
            assert!(
                off < bytes.len() / 20,
                "Bitstelle {bit} steht {set} mal von {} — festgeklemmt?",
                bytes.len()
            );
        }

        let mut seen = [false; 256];
        for b in &bytes {
            seen[*b as usize] = true;
        }
        let missing = seen.iter().filter(|s| !**s).count();
        assert_eq!(
            missing, 0,
            "{missing} von 256 Bytewerten kamen in 32 KiB nie vor"
        );
    }
}
