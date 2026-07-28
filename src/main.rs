//! Command-line entry point.

use std::io::{BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use schatzsuche::address::Kind;
use schatzsuche::alert::{AlertPayload, Dispatcher};
use schatzsuche::bip39::WordCount;
use schatzsuche::config::Config;
use schatzsuche::deriver::Deriver;
use schatzsuche::engine::{self, Event, Shared};
use schatzsuche::hits::{self, Hit, HitWriter};
use schatzsuche::lookup::{self, Database, Record};
use schatzsuche::startup::{self, Progress};
use schatzsuche::stats::{Control, Priority, Stats};
use schatzsuche::{tui, util};

#[derive(Parser)]
#[command(
    name = "schatzsuche",
    about = "Random BIP-39 seed search against a local set of funded addresses",
    long_about = "Generates mnemonics from OS entropy and tests their BIP-44/49/84 \
                  addresses against a local database.\n\n\
                  The expected time to a hit is displayed in multiples of the age of \
                  the universe. That number is the point of the program."
)]
struct Cli {
    #[arg(long, default_value = "config.toml", global = true)]
    config: PathBuf,

    /// Inject a synthetic hit and run the full persistence and alert chain.
    #[arg(long)]
    test_alert: bool,

    /// Write a dummy hit, read it back, verify permissions and the backup copy.
    #[arg(long)]
    test_persistence: bool,

    /// Send a periodic "still running" ping, e.g. 24h.
    #[arg(long, value_name = "INTERVAL")]
    heartbeat: Option<String>,

    /// Run without the TUI; useful for measuring raw throughput.
    #[arg(long)]
    headless: bool,

    /// Open a native window instead of drawing in the terminal.
    #[arg(long)]
    gui: bool,

    /// GUI only: capture one frame to a raw RGBA dump, then quit.
    #[arg(long, value_name = "PATH", hide = true)]
    screenshot: Option<PathBuf>,

    /// Directory holding the database, config and hit files.
    #[arg(long, value_name = "DIR", global = true)]
    data_dir: Option<PathBuf>,

    /// Stop after this many seconds. Mainly for benchmarking.
    #[arg(long, value_name = "SECS")]
    duration: Option<u64>,

    /// Start in the stopped state; press space or click START to begin.
    #[arg(long)]
    start_paused: bool,

    /// Override addresses derived per path per seed.
    #[arg(long, value_name = "N")]
    addresses: Option<u32>,

    /// Override worker count.
    #[arg(long, value_name = "N")]
    threads: Option<usize>,

    /// Scheduling priority: 0 = sparsam, 1 = normal, 2 = maximal.
    #[arg(long, value_name = "N")]
    priority: Option<u8>,

    /// Override mnemonic length (12 or 24).
    #[arg(long, value_name = "N")]
    words: Option<u8>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Stage-by-stage timing of the derivation pipeline.
    Bench {
        #[arg(long, default_value_t = 20)]
        addresses: u32,
        #[arg(long, default_value_t = 24)]
        words: u8,
    },
    /// Build a database from a dump of funded addresses.
    BuildDb {
        /// One address per line, optionally followed by a balance in satoshis.
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value = "funded.scdb")]
        output: PathBuf,
    },
    /// Generate a synthetic database, for testing without a real dump.
    SynthDb {
        #[arg(long, default_value_t = 1_000_000)]
        count: usize,
        #[arg(long, default_value = "funded.scdb")]
        output: PathBuf,
        /// Also insert the addresses of this mnemonic, so the lookup chain can
        /// be exercised end to end.
        #[arg(long)]
        plant: Option<String>,
    },
    /// Write a config.toml template.
    InitConfig,
    /// Prove the Bloom filter and on-disk lookup actually find a planted seed.
    VerifyLookup,
}

/// Standard per-user location for the database, config and hit files.
///
/// Each platform has its own convention, and putting a 145 MB database in the
/// wrong one is the kind of thing users notice. Written as three separate
/// functions rather than one with `cfg!` blocks, so each body is the whole
/// function on its platform and nothing is dead code anywhere.
#[cfg(target_os = "macos")]
fn app_support_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join("Library/Application Support/Schatzsuche")
}

#[cfg(target_os = "windows")]
fn app_support_dir() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(appdata) => PathBuf::from(appdata).join("Schatzsuche"),
        Err(_) => PathBuf::from("."),
    }
}

/// XDG: data belongs in `$XDG_DATA_HOME`, defaulting to `~/.local/share`.
#[cfg(all(unix, not(target_os = "macos")))]
fn app_support_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("schatzsuche");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local/share/schatzsuche")
}

/// Decides where the collider's files live, and moves there.
///
/// Finder starts an application in `/`, so a GUI launch cannot rely on the
/// working directory the way a terminal launch can. Rather than hard-coding a
/// path into the launcher — which breaks the moment the folder is moved — the
/// program resolves its own directory.
fn enter_data_dir(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let dir = match explicit {
        Some(d) => d.to_path_buf(),
        // A directory that already holds a setup wins: that is a terminal run
        // from inside the project.
        None if Path::new("config.toml").exists() || Path::new("funded.scdb").exists() => {
            return std::env::current_dir().map_err(|e| e.to_string())
        }
        None => app_support_dir(),
    };

    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::env::set_current_dir(&dir).map_err(|e| format!("cannot enter {}: {e}", dir.display()))?;
    Ok(dir)
}

/// True when nothing is attached to standard output.
///
/// A desktop launch carries no arguments, so this is how the program knows it
/// was double-clicked rather than run from a shell — and therefore that a
/// window, not terminal output, is the only way to say anything at all.
fn launched_without_terminal() -> bool {
    !std::io::stdout().is_terminal()
}

fn main() {
    // Finder has historically appended a process-serial-number argument.
    // clap would reject it, which would turn a double-click into an instant,
    // invisible failure.
    let args: Vec<std::ffi::OsString> = std::env::args_os()
        .filter(|a| !a.to_string_lossy().starts_with("-psn_"))
        .collect();

    let mut cli = Cli::parse_from(args);
    if !cli.headless
        && cli.screenshot.is_none()
        && cli.command.is_none()
        && launched_without_terminal()
    {
        cli.gui = true;
    }
    let gui = cli.gui;

    let result = enter_data_dir(cli.data_dir.as_deref()).and_then(|_| dispatch(cli));

    if let Err(e) = result {
        // Without a terminal there is nowhere for this to go, and a
        // double-clicked app that does nothing at all is the worst possible
        // failure mode. So in window mode, the error gets a window.
        if gui {
            schatzsuche::gui::show_error(&e);
        } else {
            eprintln!("error: {e}");
        }
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<(), String> {
    match &cli.command {
        Some(Command::Bench { addresses, words }) => {
            let wc = if *words == 12 {
                WordCount::W12
            } else {
                WordCount::W24
            };
            schatzsuche::bench::run(*addresses, wc);
            Ok(())
        }
        Some(Command::InitConfig) => {
            if cli.config.exists() {
                return Err(format!(
                    "{} already exists; remove it first",
                    cli.config.display()
                ));
            }
            Config::write_template(&cli.config)?;
            println!("wrote {}", cli.config.display());
            println!("Enable at least one alert channel, then run --test-alert.");
            Ok(())
        }
        Some(Command::BuildDb { input, output }) => build_db(input, output),
        Some(Command::SynthDb {
            count,
            output,
            plant,
        }) => synth_db(*count, output, plant.as_deref()),
        Some(Command::VerifyLookup) => verify_lookup(),
        None => {
            let cfg = load_config(&cli)?;
            if cli.test_persistence {
                return test_persistence(&cfg);
            }
            if cli.test_alert {
                return test_alert(&cfg);
            }
            run_collider(&cli, cfg)
        }
    }
}

fn load_config(cli: &Cli) -> Result<Config, String> {
    let mut cfg = Config::load_or_default(&cli.config)?;
    if let Some(n) = cli.addresses {
        cfg.run.addresses_per_path = n;
    }
    if let Some(n) = cli.threads {
        cfg.run.threads = n;
    }
    if let Some(p) = cli.priority {
        if p > 2 {
            return Err(format!("--priority must be 0, 1 or 2, got {p}"));
        }
        cfg.run.priority = p;
    }
    if let Some(w) = cli.words {
        if w != 12 && w != 24 {
            return Err(format!("--words must be 12 or 24, got {w}"));
        }
        cfg.run.word_count = w;
    }
    cfg.validate()?;
    if let Some(h) = &cli.heartbeat {
        if schatzsuche::config::parse_duration(h).is_none() {
            return Err(format!(
                "cannot parse --heartbeat {h:?}; try 24h, 90m or 3600"
            ));
        }
        cfg.heartbeat.enabled = true;
        cfg.heartbeat.interval = h.clone();
    }
    Ok(cfg)
}

fn make_dispatcher(cfg: &Config) -> Arc<Dispatcher> {
    Arc::new(Dispatcher::new(
        cfg.notifiers(),
        cfg.alerts.pending_path.clone(),
        cfg.alerts.max_attempts,
        cfg.retry_interval(),
    ))
}

fn make_writer(cfg: &Config) -> Arc<HitWriter> {
    Arc::new(HitWriter::new(
        cfg.hits.path.clone(),
        cfg.hits.backup_path.clone(),
    ))
}

// --- self tests -------------------------------------------------------------

fn test_persistence(cfg: &Config) -> Result<(), String> {
    let writer = make_writer(cfg);
    println!("--test-persistence");
    println!("  primary : {}", writer.primary_path().display());
    match writer.backup_path() {
        Some(p) => println!("  backup  : {}", p.display()),
        None => println!("  backup  : (not configured)"),
    }
    println!();

    let report = hits::self_test(&writer).map_err(|e| format!("persistence failed: {e}"))?;
    let mark = |ok: bool| if ok { "ok  " } else { "FAIL" };

    println!(
        "  [{}] primary written, fsync'd and read back",
        mark(report.primary_readback_ok)
    );
    let show_mode = |m: Option<u32>| match m {
        Some(m) => format!("{m:04o}"),
        None => "n/a (Windows kennt keine POSIX-Rechte)".to_string(),
    };
    println!(
        "  [{}] primary mode is 0600 (found {})",
        mark(report.primary_mode.map(|m| m == 0o600).unwrap_or(true)),
        show_mode(report.primary_mode)
    );
    if report.backup.is_some() {
        println!(
            "  [{}] backup written and read back",
            mark(report.backup_readback_ok)
        );
        println!(
            "  [{}] backup mode is 0600 (found {})",
            mark(
                report
                    .backup_mode
                    .flatten()
                    .map(|m| m == 0o600)
                    .unwrap_or(true)
            ),
            report
                .backup_mode
                .map(show_mode)
                .unwrap_or_else(|| "n/a".into())
        );
    }
    if let Some(e) = &report.backup_error {
        println!("  [FAIL] backup error: {e}");
    }

    println!();
    if report.ok() {
        println!("PASS - a hit written now would survive a power cut.");
        Ok(())
    } else {
        Err("persistence self-test failed; fix this before running".into())
    }
}

fn test_alert(cfg: &Config) -> Result<(), String> {
    let writer = make_writer(cfg);
    let dispatcher = make_dispatcher(cfg);

    println!("--test-alert");
    if dispatcher.is_empty() {
        return Err("no alert channels enabled in config.toml; \
                    enable at least one under [alerts]"
            .into());
    }
    println!("  channels: {}", dispatcher.channel_names().join(", "));
    println!();

    let hit = Hit::synthetic();

    // Exactly the production order: persist, then surface, then alert.
    print!("  writing and fsyncing hit ... ");
    let _ = std::io::stdout().flush();
    let backup_err = writer
        .persist(&hit)
        .map_err(|e| format!("persist failed: {e}"))?;
    println!("ok");
    if let Some(e) = backup_err {
        println!("  WARNING backup copy failed: {e}");
    }

    tui::print_hit_plain(&hit);
    print!("\x07");
    let _ = std::io::stdout().flush();

    println!("  firing all channels concurrently ...");
    let result = dispatcher.dispatch(&AlertPayload::from_hit(&hit));

    println!();
    let mut all_ok = true;
    for (name, r) in &result.per_channel {
        match r {
            Ok(()) => println!("  [ok  ] {name}"),
            Err(e) => {
                all_ok = false;
                println!("  [FAIL] {name}: {e}");
            }
        }
    }
    if result.queued_for_retry {
        println!(
            "\n  every channel failed; payload queued in {}",
            dispatcher.pending_path().display()
        );
        println!(
            "  the retry loop would keep trying every {}s until one succeeds",
            cfg.alerts.retry_interval_secs
        );
    }

    println!();
    println!("  the alert above carries no mnemonic, by construction.");
    println!("  the seed is in {}", writer.primary_path().display());

    if all_ok {
        Ok(())
    } else {
        Err("some channels failed; see above".into())
    }
}

/// Proves the two-stage lookup actually finds a known planted address.
fn verify_lookup() -> Result<(), String> {
    let dir = std::env::temp_dir().join("schatzsuche-verify");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("verify.scdb");

    println!("verify-lookup");
    println!("  planting the addresses of the published all-zero test mnemonic");

    let mut d = Deriver::new();
    d.stretch(&[0u8; 16], WordCount::W12);
    let mnemonic = d.mnemonic().to_string();

    let mut planted = Vec::new();
    d.walk(5, |h, o| planted.push((o.kind(), *h, o.path())));

    // Bury the planted entries in noise so the search is not trivial.
    let mut records: Vec<Record> = Vec::with_capacity(200_000 + planted.len());
    let mut noise = vec![0u8; 200_000 * 20];
    getrandom::getrandom(&mut noise).map_err(|e| e.to_string())?;
    for chunk in noise.chunks_exact(20) {
        let mut h = [0u8; 20];
        h.copy_from_slice(chunk);
        records.push(Record::new(Kind::P2wpkh, &h, 0));
    }
    for (kind, h, _) in &planted {
        records.push(Record::new(*kind, h, 133_700_000));
    }

    let n = lookup::write_database(&path, records).map_err(|e| e.to_string())?;
    println!("  database: {n} records");

    let db = Database::open(&path).map_err(|e| e.to_string())?;
    let bloom = db.build_bloom(1e-6);
    println!(
        "  bloom   : {:.1} MB, k={}",
        bloom.bytes() as f64 / 1e6,
        bloom.k()
    );
    println!();

    let mut failures = 0;
    for (kind, h, path_str) in &planted {
        let in_bloom = bloom.contains(*kind, h);
        let in_db = db.lookup(*kind, h);
        let ok = in_bloom && in_db == Some(133_700_000);
        if !ok {
            failures += 1;
        }
        println!(
            "  [{}] {path_str:<20} bloom={in_bloom} db={in_db:?}",
            if ok { "ok  " } else { "FAIL" }
        );
    }

    // A seed that was not planted must not match.
    let mut other = Deriver::new();
    other.stretch(&[0x11u8; 16], WordCount::W12);
    let mut false_hits = 0;
    other.walk(5, |h, o| {
        if bloom.contains(o.kind(), h) && db.lookup(o.kind(), h).is_some() {
            false_hits += 1;
        }
    });
    println!(
        "  [{}] an unplanted seed produced {false_hits} matches",
        if false_hits == 0 { "ok  " } else { "FAIL" }
    );

    let _ = std::fs::remove_file(&path);
    println!();
    if failures == 0 && false_hits == 0 {
        println!("PASS - the lookup chain finds a planted seed and nothing else.");
        println!("  planted mnemonic: {mnemonic}");
        Ok(())
    } else {
        Err("lookup verification failed".into())
    }
}

// --- database construction --------------------------------------------------

fn build_db(input: &PathBuf, output: &PathBuf) -> Result<(), String> {
    let f =
        std::fs::File::open(input).map_err(|e| format!("cannot open {}: {e}", input.display()))?;
    println!("reading {} ...", input.display());
    let t = Instant::now();

    let (records, skipped) = lookup::parse_dump(BufReader::with_capacity(1 << 20, f))
        .map_err(|e| format!("cannot parse dump: {e}"))?;
    println!(
        "  parsed {} usable addresses in {:.1}s ({skipped} skipped as underivable)",
        records.len(),
        t.elapsed().as_secs_f64()
    );
    if records.is_empty() {
        return Err("no usable addresses found; is this a mainnet address dump?".into());
    }

    println!("sorting and writing {} ...", output.display());
    let n = lookup::write_database(output, records).map_err(|e| e.to_string())?;
    let size = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    println!(
        "  {n} unique records, {:.1} MB, {:.1}s total",
        size as f64 / 1e6,
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

fn synth_db(count: usize, output: &PathBuf, plant: Option<&str>) -> Result<(), String> {
    println!("generating {count} synthetic records ...");
    let t = Instant::now();

    let mut records: Vec<Record> = Vec::with_capacity(count + 64);
    // Generated in blocks; a syscall per record would dominate the runtime.
    let mut block = vec![0u8; 1 << 20];
    let mut made = 0usize;
    while made < count {
        getrandom::getrandom(&mut block).map_err(|e| e.to_string())?;
        for chunk in block.chunks_exact(20) {
            if made >= count {
                break;
            }
            let mut h = [0u8; 20];
            h.copy_from_slice(chunk);
            let kind = match chunk[0] % 3 {
                0 => Kind::P2pkh,
                1 => Kind::P2sh,
                _ => Kind::P2wpkh,
            };
            records.push(Record::new(kind, &h, 100_000 + made as u64));
            made += 1;
        }
    }

    if let Some(m) = plant {
        let entropy = bip39::Mnemonic::parse(m)
            .map_err(|e| format!("cannot parse mnemonic: {e}"))?
            .to_entropy();
        let wc = if entropy.len() == 16 {
            WordCount::W12
        } else {
            WordCount::W24
        };
        let mut d = Deriver::new();
        d.stretch(&entropy, wc);
        let mut n = 0;
        d.walk(20, |h, o| {
            records.push(Record::new(o.kind(), h, 133_700_000));
            n += 1;
        });
        println!("  planted {n} addresses from the supplied mnemonic");
    }

    let n = lookup::write_database(output, records).map_err(|e| e.to_string())?;
    let size = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    println!(
        "  {n} records, {:.1} MB, {:.1}s",
        size as f64 / 1e6,
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

// --- the run ----------------------------------------------------------------

fn run_collider(cli: &Cli, cfg: Config) -> Result<(), String> {
    let threads = cfg.run.effective_threads();
    let wc = cfg.run.word_count_enum();
    let entropy_bits = (wc.entropy_bytes() * 8) as u32;
    let per_seed = cfg.run.addresses_per_path * 3;

    let stats = Arc::new(Stats::new());
    let control = Arc::new(Control::new(
        threads,
        cfg.run.addresses_per_path,
        Priority::from_u8(cfg.run.priority),
    ));
    if cli.start_paused {
        control.set_paused(true);
    }
    let (tx, rx) = channel::<Event>();
    let pool_threads = schatzsuche::config::physical_cores().max(threads);
    let gui = cli.gui || cli.screenshot.is_some();

    // In window mode the loading runs behind the already-visible window; in a
    // terminal it runs inline, because the printed lines serve the same purpose.
    let progress = Arc::new(Progress::new());
    let boot = {
        let cfg = cfg.clone();
        let progress = Arc::clone(&progress);
        let stats = Arc::clone(&stats);
        let control = Arc::clone(&control);
        let tx = tx.clone();
        move || -> Result<(), String> {
            let loaded = match startup::load(&cfg, &progress) {
                Ok(l) => l,
                Err(e) => {
                    progress.fail(e.clone());
                    return Err(e);
                }
            };

            let dispatcher = make_dispatcher(&cfg);
            dispatcher.spawn_retry_loop();
            if let Some(interval) = cfg.heartbeat_interval() {
                let d = Arc::clone(&dispatcher);
                let s = Arc::clone(&stats);
                let start = Instant::now();
                thread::spawn(move || loop {
                    thread::sleep(interval);
                    d.dispatch_async(AlertPayload::heartbeat(s.seeds(), start.elapsed()));
                });
            }

            progress.publish(
                loaded.db.count() as u64,
                loaded.bloom.bytes(),
                loaded.db.bytes(),
            );

            let shared = Arc::new(Shared {
                stats,
                control,
                bloom: Arc::new(loaded.bloom),
                db: Arc::new(loaded.db),
                writer: loaded.writer,
                dispatcher,
                events: tx,
                word_count: wc,
            });
            thread::spawn(move || engine::run(shared, pool_threads));
            progress.finish();
            Ok(())
        }
    };

    if gui {
        // The window must exist before loading starts, or a large database
        // means a long stretch with nothing on screen at all.
        let existing = make_writer(&cfg).load_all().unwrap_or_default();
        thread::spawn(boot);

        let app = schatzsuche::gui::GuiApp::new(
            Arc::clone(&stats),
            Arc::clone(&control),
            rx,
            existing,
            0,
            per_seed,
            entropy_bits,
            threads,
            0,
            0,
            cli.screenshot.clone(),
            Some(Arc::clone(&progress)),
        );
        let opts = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 800.0])
                .with_min_inner_size([900.0, 640.0])
                .with_title("Schatzsuche")
                .with_icon(eframe::egui::IconData {
                    rgba: schatzsuche::icon_data::ICON_RGBA.to_vec(),
                    width: schatzsuche::icon_data::ICON_W,
                    height: schatzsuche::icon_data::ICON_H,
                }),
            ..Default::default()
        };
        eframe::run_native("Schatzsuche", opts, Box::new(|_| Ok(Box::new(app))))
            .map_err(|e| format!("window error: {e}"))?;
    } else {
        println!("Schatzsuche");
        boot()?;
        println!(
            "  Datenbank  : {} Adressen, Filter {:.1} MB",
            progress.funded(),
            progress.bloom_bytes() as f64 / 1e6
        );
        println!(
            "  Arbeiter   : {threads} aktiv von {} Kernen, Priorität {}",
            schatzsuche::config::physical_cores(),
            Priority::from_u8(cfg.run.priority).label()
        );
        println!(
            "  Ableitung  : {} Adressen/Pfad x 3 = {} pro Seed",
            cfg.run.addresses_per_path, per_seed
        );
        println!();

        if let Some(secs) = cli.duration {
            let c = Arc::clone(&control);
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(secs));
                c.request_stop();
            });
        }
        run_headless(&stats, &control, rx, progress.funded(), per_seed);
    }

    if let Some(secs) = cli.duration {
        let c = Arc::clone(&control);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(secs));
            c.request_stop();
        });
    }
    control.request_stop();

    println!(
        "gestoppt nach {} Seeds ({} Adressen, {} Fehlalarme, {} bestätigt)",
        stats.seeds(),
        stats.addresses(),
        stats.bloom_hits(),
        stats.confirmed()
    );
    Ok(())
}

/// Headless mode: a periodic one-line status, so throughput can be measured
/// without the terminal UI in the way.
fn run_headless(
    stats: &Arc<Stats>,
    control: &Arc<Control>,
    rx: std::sync::mpsc::Receiver<Event>,
    funded: u64,
    per_seed: u32,
) {
    let start = Instant::now();
    let mut last = (Instant::now(), 0u64);

    while !control.stopping() {
        thread::sleep(Duration::from_secs(1));

        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::Hit(h) => tui::print_hit_plain(&h),
                Event::PersistFailure { hit, error } => {
                    eprintln!("PERSIST FAILED for {}: {error}", hit.address);
                    tui::print_hit_plain(&hit);
                }
                Event::BackupFailure { id, error } => {
                    eprintln!("backup failed for {id}: {error}");
                }
            }
        }

        let seeds = stats.seeds();
        let now = Instant::now();
        let dt = now.duration_since(last.0).as_secs_f64().max(1e-9);
        let rate = (seeds - last.1) as f64 / dt;
        last = (now, seeds);

        let ages = tui::universe_ages_to_hit(funded, per_seed, rate.max(1.0));

        println!(
            "{:>12} seeds  {:>8.0} seeds/s  {:>11} addr/s  {:>12}  |  {:.3e} universe ages to a hit",
            seeds,
            rate,
            (rate * per_seed as f64) as u64,
            util::format_duration(start.elapsed().as_secs()),
            ages
        );
        let _ = std::io::stdout().flush();
    }
}
