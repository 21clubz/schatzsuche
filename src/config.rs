//! `config.toml` handling.
//!
//! Every section has defaults, so a missing or partial file still produces a
//! runnable configuration. Only channels explicitly marked `enabled = true`
//! are constructed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::alert::channels::{Desktop, Ntfy, Smtp, Telegram, Webhook};
use crate::alert::Notifier;
use crate::bip39::WordCount;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub run: Run,
    pub lookup: Lookup,
    pub hits: Hits,
    pub alerts: Alerts,
    pub heartbeat: Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Run {
    /// 12 or 24.
    pub word_count: u8,
    /// Addresses derived per derivation path, per seed.
    pub addresses_per_path: u32,
    /// 0 means "ask the hardware" — see [`crate::machine::Machine`].
    pub threads: usize,
    /// Scheduling priority: 0 background, 1 utility, 2 normal.
    ///
    /// Background keeps the machine cool and responsive by preferring the
    /// efficiency cores, at the cost of throughput.
    pub priority: u8,
}

impl Default for Run {
    fn default() -> Self {
        Run {
            word_count: 24,
            addresses_per_path: 20,
            // Not a number: a fixed four was measured on an eight-core M1,
            // where it is exactly right, and is the entire machine on a dual
            // core laptop. Zero means the hardware decides at startup.
            threads: 0,
            priority: 2,
        }
    }
}

impl Run {
    pub fn word_count_enum(&self) -> WordCount {
        WordCount::from_words(self.word_count).unwrap_or(WordCount::W24)
    }

    /// Physical cores, falling back to logical if the count is unavailable.
    pub fn effective_threads(&self) -> usize {
        if self.threads > 0 {
            return self.threads.min(physical_cores());
        }
        crate::machine::Machine::detect().recommended_threads()
    }
}

/// Physical core count.
///
/// `std::thread::available_parallelism` reports *logical* CPUs. On hardware
/// with SMT two hyperthreads share one set of execution units, and
/// oversubscribing them costs more in contention than it gains — so the
/// physical count is what the defaults are built on. `num_cpus` knows how to
/// ask each platform; the fallback keeps this working if it ever cannot.
pub fn physical_cores() -> usize {
    let n = num_cpus::get_physical();
    if n > 0 {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lookup {
    /// Path to the database built by `build-db` or `synth-db`.
    pub database: PathBuf,
    /// Target false-positive rate for the Bloom filter.
    pub bloom_fpr: f64,
}

impl Default for Lookup {
    fn default() -> Self {
        Lookup {
            database: PathBuf::from("funded.scdb"),
            bloom_fpr: 1e-6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hits {
    pub path: PathBuf,
    /// Strongly recommended to live on a different physical drive.
    pub backup_path: Option<PathBuf>,
}

impl Default for Hits {
    fn default() -> Self {
        Hits {
            path: PathBuf::from("hits.jsonl"),
            backup_path: Some(PathBuf::from("hits_backup.jsonl")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Alerts {
    pub pending_path: PathBuf,
    pub max_attempts: u32,
    pub retry_interval_secs: u64,
    pub ntfy: NtfyCfg,
    pub telegram: TelegramCfg,
    pub smtp: SmtpCfg,
    pub webhook: WebhookCfg,
    pub desktop: DesktopCfg,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts {
            pending_path: PathBuf::from("pending_alerts.jsonl"),
            max_attempts: 5,
            retry_interval_secs: 60,
            ntfy: NtfyCfg::default(),
            telegram: TelegramCfg::default(),
            smtp: SmtpCfg::default(),
            webhook: WebhookCfg::default(),
            desktop: DesktopCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NtfyCfg {
    pub enabled: bool,
    pub base_url: String,
    pub topic: String,
    pub token: Option<String>,
}

/// The placeholder shipped in the config template.
pub const NTFY_PLACEHOLDER: &str = "change-me-to-something-unguessable";

impl Default for NtfyCfg {
    fn default() -> Self {
        NtfyCfg {
            enabled: false,
            base_url: "https://ntfy.sh".into(),
            topic: NTFY_PLACEHOLDER.into(),
            token: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TelegramCfg {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpCfg {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
    pub to: String,
    /// True for implicit TLS (port 465); false for STARTTLS (587).
    pub tls_implicit: bool,
}

impl Default for SmtpCfg {
    fn default() -> Self {
        SmtpCfg {
            enabled: false,
            host: "smtp.example.com".into(),
            port: 587,
            username: String::new(),
            password: String::new(),
            from: String::new(),
            to: String::new(),
            tls_implicit: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct WebhookCfg {
    pub enabled: bool,
    pub url: String,
    /// Optional extra header, `Name: value`.
    pub auth_header: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopCfg {
    pub enabled: bool,
}

impl Default for DesktopCfg {
    fn default() -> Self {
        DesktopCfg { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Heartbeat {
    pub enabled: bool,
    /// e.g. `"24h"`, `"90m"`, `"3600s"`.
    pub interval: String,
}

impl Default for Heartbeat {
    fn default() -> Self {
        Heartbeat {
            enabled: false,
            interval: "24h".into(),
        }
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
    }

    /// Loads `path`, or returns defaults if it does not exist.
    pub fn load_or_default(path: &std::path::Path) -> Result<Config, String> {
        if path.exists() {
            Config::load(path)
        } else {
            Ok(Config::default())
        }
    }

    pub fn write_template(path: &std::path::Path) -> Result<(), String> {
        let text = toml::to_string_pretty(&Config::default())
            .map_err(|e| format!("cannot serialise config: {e}"))?;
        let header = "\
# Schatzsuche configuration.
#
# The seed of a hit is written ONLY to the local files in [hits]. Alert
# channels receive timestamp, hostname, derivation path, address and balance,
# never the mnemonic — those services run on infrastructure you do not control.
#
# Enable at least one alert channel and verify it with `--test-alert`.
#
# run.threads = 0 means the hardware decides: the performance cores on a
# machine that has separate fast and slow ones, half the cores otherwise. Set a
# number here to override that permanently; the interface can also change it
# while the search runs, without restarting.
#
# run.word_count is any BIP-39 length: 12, 15, 18, 21 or 24. Also changeable
# while running. A shorter mnemonic searches a smaller space, not a more
# promising one — a hit needs a collision in the 160-bit address space either
# way.

";
        write_owner_only(path, &format!("{header}{text}"))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Builds the notifiers for every enabled channel.
    pub fn notifiers(&self) -> Vec<Arc<dyn Notifier>> {
        let a = &self.alerts;
        let mut v: Vec<Arc<dyn Notifier>> = Vec::new();
        if a.ntfy.enabled {
            v.push(Arc::new(Ntfy {
                base_url: a.ntfy.base_url.clone(),
                topic: a.ntfy.topic.clone(),
                token: a.ntfy.token.clone(),
            }));
        }
        if a.telegram.enabled {
            v.push(Arc::new(Telegram {
                bot_token: a.telegram.bot_token.clone(),
                chat_id: a.telegram.chat_id.clone(),
            }));
        }
        if a.smtp.enabled {
            v.push(Arc::new(Smtp {
                host: a.smtp.host.clone(),
                port: a.smtp.port,
                username: a.smtp.username.clone(),
                password: a.smtp.password.clone(),
                from: a.smtp.from.clone(),
                to: a.smtp.to.clone(),
                tls_implicit: a.smtp.tls_implicit,
            }));
        }
        if a.webhook.enabled {
            v.push(Arc::new(Webhook {
                url: a.webhook.url.clone(),
                auth_header: a.webhook.auth_header.clone(),
            }));
        }
        if a.desktop.enabled {
            v.push(Arc::new(Desktop));
        }
        v
    }

    /// Rejects configurations that would leak.
    ///
    /// ntfy topics are public: anyone who knows or guesses the name can
    /// subscribe. The seed never travels, but the address and balance do, and
    /// a topic left at the shipped placeholder is readable by every other
    /// person who also left it there. Refusing to start is the only honest
    /// response — a warning printed to a window nobody reads is not enough.
    pub fn validate(&self) -> Result<(), String> {
        let n = &self.alerts.ntfy;
        if n.enabled {
            if n.topic == NTFY_PLACEHOLDER || n.topic.trim().is_empty() {
                return Err(format!(
                    "ntfy ist aktiviert, aber das Thema steht noch auf dem Beispielwert.\n\n\
                     ntfy-Themen sind öffentlich: jeder, der den Namen kennt oder errät,\n\
                     kann mitlesen. Trage in config.toml unter [alerts.ntfy] ein eigenes,\n\
                     nicht erratbares Thema ein — zum Beispiel:\n\n    \
                     topic = \"{}\"",
                    suggest_topic()
                ));
            }
            if n.topic.len() < 16 {
                return Err(format!(
                    "Das ntfy-Thema \"{}\" ist zu kurz und damit erratbar.\n\n\
                     Mindestens 16 Zeichen, zum Beispiel:\n\n    topic = \"{}\"",
                    n.topic,
                    suggest_topic()
                ));
            }
        }
        if WordCount::from_words(self.run.word_count).is_none() {
            return Err(format!(
                "word_count muss 12, 15, 18, 21 oder 24 sein, steht aber auf {}",
                self.run.word_count
            ));
        }
        if !(0.0..=0.5).contains(&self.lookup.bloom_fpr) || self.lookup.bloom_fpr <= 0.0 {
            return Err(format!(
                "bloom_fpr muss zwischen 0 und 0,5 liegen, steht aber auf {}",
                self.lookup.bloom_fpr
            ));
        }
        Ok(())
    }

    pub fn retry_interval(&self) -> Duration {
        Duration::from_secs(self.alerts.retry_interval_secs.max(5))
    }

    pub fn heartbeat_interval(&self) -> Option<Duration> {
        if !self.heartbeat.enabled {
            return None;
        }
        parse_duration(&self.heartbeat.interval)
    }
}

/// A random, unguessable ntfy topic, offered when the user has to pick one.
pub fn suggest_topic() -> String {
    let mut raw = [0u8; 12];
    if getrandom::getrandom(&mut raw).is_err() {
        return "schatzsuche-bitte-eigenes-thema-waehlen".to_string();
    }
    let mut s = String::from("schatzsuche-");
    for b in raw {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parses `"24h"`, `"90m"`, `"30s"`, `"2d"` or a bare number of seconds.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.chars().last()? {
        'd' | 'D' => (&s[..s.len() - 1], 86_400),
        'h' | 'H' => (&s[..s.len() - 1], 3_600),
        'm' | 'M' => (&s[..s.len() - 1], 60),
        's' | 'S' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().ok()?;
    Some(Duration::from_secs(n.saturating_mul(mult)))
}

/// Writes a file only its owner can read.
///
/// The template ships with an SMTP password field and a Telegram bot token,
/// which are credentials like any other. Hit files are created 0600 for that
/// reason, and a world-readable config on a shared machine would hand away the
/// notification channels while the seeds themselves stayed locked.
#[cfg(unix)]
fn write_owner_only(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    // `mode` above only applies while creating. An existing file keeps whatever
    // it had, so tighten it explicitly rather than trusting the open flags.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Windows has no mode bits; the per-user profile directory is the protection.
#[cfg(not(unix))]
fn write_owner_only(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_through_toml() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.run.word_count, 24);
        assert_eq!(parsed.run.addresses_per_path, 20);
        assert_eq!(parsed.alerts.max_attempts, 5);
        assert_eq!(parsed.alerts.retry_interval_secs, 60);
    }

    /// The template holds an SMTP password and a bot token once filled in, so
    /// it must not be readable by other users of the machine.
    #[cfg(unix)]
    #[test]
    fn config_template_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("schatzsuche-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);

        Config::write_template(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(mode, 0o600, "config template is readable by others");
    }

    /// Every length BIP-39 defines has to survive the round trip through the
    /// config, since the interface now offers all five.
    #[test]
    fn all_bip39_lengths_are_accepted() {
        for wc in crate::bip39::ALL_WORD_COUNTS {
            let mut c = Config::default();
            c.run.word_count = wc.words() as u8;
            assert!(c.validate().is_ok(), "{wc:?} rejected");
            assert_eq!(c.run.word_count_enum(), wc);
        }
    }

    /// A partial file must not wipe out the other sections.
    #[test]
    fn partial_config_keeps_defaults() {
        let c: Config = toml::from_str(
            r#"
            [run]
            word_count = 12
            "#,
        )
        .unwrap();
        assert_eq!(c.run.word_count, 12);
        assert_eq!(c.run.addresses_per_path, 20, "default preserved");
        assert_eq!(c.lookup.bloom_fpr, 1e-6);
        assert!(c.alerts.desktop.enabled);
    }

    /// A typo in a key should be an error, not silently ignored — a misspelt
    /// `topic` would otherwise mean alerts vanish into a default topic.
    #[test]
    fn unknown_keys_are_rejected() {
        let r: Result<Config, _> = toml::from_str(
            r#"
            [alerts.ntfy]
            enabled = true
            topik = "oops"
            "#,
        );
        assert!(r.is_err(), "unknown key must be rejected");
    }

    /// The placeholder topic must be refused, not merely warned about.
    #[test]
    fn placeholder_ntfy_topic_is_rejected() {
        let mut c = Config::default();
        assert!(c.validate().is_ok(), "disabled ntfy is fine");

        c.alerts.ntfy.enabled = true;
        let err = c.validate().unwrap_err();
        assert!(err.contains("öffentlich"), "must explain why: {err}");
        assert!(err.contains("topic ="), "must show the fix: {err}");

        c.alerts.ntfy.topic = "kurz".into();
        assert!(c.validate().is_err(), "short topics are guessable too");

        c.alerts.ntfy.topic = suggest_topic();
        assert!(c.validate().is_ok(), "a generated topic must pass");
    }

    #[test]
    fn suggested_topics_are_long_and_unique() {
        let a = suggest_topic();
        let b = suggest_topic();
        assert_ne!(a, b);
        assert!(a.len() >= 32, "too short: {a}");
    }

    #[test]
    fn nonsense_values_are_rejected() {
        let mut c = Config::default();
        c.run.word_count = 13;
        assert!(c.validate().is_err());

        let mut c = Config::default();
        c.lookup.bloom_fpr = 0.0;
        assert!(c.validate().is_err());
        c.lookup.bloom_fpr = 2.0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("90m"), Some(Duration::from_secs(5_400)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("2d"), Some(Duration::from_secs(172_800)));
        assert_eq!(parse_duration("3600"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("nonsense"), None);
    }

    #[test]
    fn only_enabled_channels_are_built() {
        let mut c = Config::default();
        assert_eq!(c.notifiers().len(), 1, "desktop is on by default");

        c.alerts.ntfy.enabled = true;
        c.alerts.telegram.enabled = true;
        let names: Vec<String> = c.notifiers().iter().map(|n| n.name().to_string()).collect();
        assert_eq!(names, vec!["ntfy", "telegram", "desktop"]);
    }

    #[test]
    fn physical_cores_is_sane() {
        let n = physical_cores();
        assert!((1..=1024).contains(&n), "implausible core count {n}");
    }
}
