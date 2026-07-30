//! The five delivery channels.
//!
//! Every network channel uses a short, explicit timeout. Threads isolate a
//! hung server from the others, but an unbounded socket wait would still pin a
//! thread for the life of the process and stall the retry queue behind it.

use std::time::Duration;

use super::{AlertKind, AlertPayload, Notifier};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .build()
}

/// Flattens a ureq error, including the response body when the server sent one
/// — that body is usually the only thing explaining a 4xx.
fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp
                .into_string()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>();
            format!("HTTP {code}: {body}")
        }
        ureq::Error::Transport(t) => format!("transport: {t}"),
    }
}

/// ntfy.sh, public or self-hosted. No account needed, which is why it is the
/// default recommendation for phone push.
pub struct Ntfy {
    pub base_url: String,
    pub topic: String,
    pub token: Option<String>,
}

impl Notifier for Ntfy {
    fn name(&self) -> &str {
        "ntfy"
    }

    fn send(&self, p: &AlertPayload) -> Result<(), String> {
        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.topic.trim_start_matches('/')
        );
        let priority = match p.kind {
            AlertKind::Hit => "urgent",
            AlertKind::Test => "high",
            AlertKind::Heartbeat => "low",
        };
        let tags = match p.kind {
            AlertKind::Hit => "rotating_light,moneybag",
            AlertKind::Test => "test_tube",
            AlertKind::Heartbeat => "green_heart",
        };

        let mut req = agent()
            .post(&url)
            .set("Title", &p.title())
            .set("Priority", priority)
            .set("Tags", tags);
        if let Some(t) = &self.token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        req.send_string(&p.body()).map(|_| ()).map_err(describe)
    }
}

pub struct Telegram {
    pub bot_token: String,
    pub chat_id: String,
}

impl Notifier for Telegram {
    fn name(&self) -> &str {
        "telegram"
    }

    fn send(&self, p: &AlertPayload) -> Result<(), String> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let text = format!("*{}*\n```\n{}\n```", p.title(), p.body());
        agent()
            .post(&url)
            .send_json(ureq::json!({
                "chat_id": self.chat_id,
                "text": text,
                "parse_mode": "Markdown",
            }))
            .map(|_| ())
            .map_err(describe)
    }
}

/// A generic JSON webhook. The body is the serialised [`AlertPayload`], which
/// by construction carries no secrets.
pub struct Webhook {
    pub url: String,
    pub auth_header: Option<String>,
}

impl Notifier for Webhook {
    fn name(&self) -> &str {
        "webhook"
    }

    fn send(&self, p: &AlertPayload) -> Result<(), String> {
        let mut req = agent()
            .post(&self.url)
            .set("Content-Type", "application/json");
        if let Some(h) = &self.auth_header {
            if let Some((k, v)) = h.split_once(':') {
                req = req.set(k.trim(), v.trim());
            }
        }
        let body = serde_json::to_string(p).map_err(|e| e.to_string())?;
        req.send_string(&body).map(|_| ()).map_err(describe)
    }
}

pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
    pub to: String,
    /// Implicit TLS on connect (port 465) instead of STARTTLS (587).
    pub tls_implicit: bool,
}

impl Notifier for Smtp {
    fn name(&self) -> &str {
        "smtp"
    }

    fn send(&self, p: &AlertPayload) -> Result<(), String> {
        use lettre::message::header::ContentType;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{Message, SmtpTransport, Transport};

        let email = Message::builder()
            .from(self.from.parse().map_err(|e| format!("bad from: {e}"))?)
            .to(self.to.parse().map_err(|e| format!("bad to: {e}"))?)
            .subject(p.title())
            .header(ContentType::TEXT_PLAIN)
            .body(p.body())
            .map_err(|e| e.to_string())?;

        let builder = if self.tls_implicit {
            SmtpTransport::relay(&self.host).map_err(|e| e.to_string())?
        } else {
            SmtpTransport::starttls_relay(&self.host).map_err(|e| e.to_string())?
        };

        let mailer = builder
            .port(self.port)
            .credentials(Credentials::new(
                self.username.clone(),
                self.password.clone(),
            ))
            .timeout(Some(CALL_TIMEOUT))
            .build();

        mailer.send(&email).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Whether this kind of alert should make a noise.
///
/// A free function rather than an inline `matches!` so the rule can be pinned
/// down by a test on every platform, including the ones that never build the
/// notifier body.
fn wants_sound(kind: AlertKind) -> bool {
    match kind {
        AlertKind::Hit | AlertKind::Test => true,
        AlertKind::Heartbeat => false,
    }
}

/// Tells macOS who is sending, once per process.
///
/// Without this the first notification opens a **file chooser** titled "Choose
/// Application — Where is use_default?" and the program appears to hang on a
/// system dialog. The cause is two layers down: `mac-notification-sys` resolves
/// the sending application by running the AppleScript
/// `get id of application "use_default"` — a placeholder string its authors
/// never replaced. No such application exists, so the Apple Event Manager asks
/// the *user* to point at it, and the error is then discarded, which means there
/// is no silent escape from the dialog.
///
/// Once that dialog is dismissed the lookup falls back to `com.apple.Finder`, so
/// every alert this program has ever sent — a real hit included — arrived wearing
/// Finder's name and icon, governed by Finder's notification permission. A bundle
/// identifier of our own fixes both halves.
///
/// The call is deliberately here and not in `main`: `set_application` swizzles
/// `NSBundle.bundleIdentifier` process-wide, and a run that never sends a
/// notification should not pay for that. The result is deliberately discarded —
/// `Once` counts as completed either way, and that alone is what keeps
/// `use_default` from ever being looked up.
#[cfg(target_os = "macos")]
fn ensure_app_identity() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let id = if crate::util::in_app_bundle() {
            // Matches CFBundleIdentifier in scripts/make-macos-app.sh.
            "io.github.21clubz.schatzsuche"
        } else {
            // An unbundled binary has no identifier LaunchServices can resolve,
            // and a bare `cargo run` really is being watched from a terminal.
            "com.apple.Terminal"
        };
        let _ = notify_rust::set_application(id);
    });
}

/// Local desktop notification. Cannot be lost to a network outage, which makes
/// it a useful floor even when everything else is configured.
pub struct Desktop;

impl Notifier for Desktop {
    fn name(&self) -> &str {
        "desktop"
    }

    fn send(&self, p: &AlertPayload) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        ensure_app_identity();

        let mut n = notify_rust::Notification::new();
        n.summary(&p.title()).body(&p.body());

        // Ein Ton beim echten Fund — und beim Selbsttest `--test-alert`, denn
        // dessen ganzer Zweck ist zu hören, wie ein Fund klingt; ein stummer
        // Selbsttest beantwortet die Frage nicht, die ihn auslöst. Nur das
        // Lebenszeichen bleibt stumm: das kommt täglich von allein, und daran
        // gewöhnt man sich das Geräusch ab.
        //
        // Das ist der wichtigste Weg von allen: er läuft auf einem eigenen
        // Thread aus `engine::report` und hängt damit nicht daran, dass die
        // Zeichenschleife gerade drankommt — er funktioniert auch bei
        // minimiertem Fenster.
        //
        // `.urgency(...)` gibt es hier bewusst nicht: auf macOS hängt die
        // Methode an einem Feature, das notify-rust auf ein anderes
        // Benachrichtigungs-Backend umstellt, mit Signatur- und
        // Berechtigungsfolgen. Der Ton genügt.
        if wants_sound(p.kind) {
            n.sound_name("Glass");
        }

        n.show().map(|_| ()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hits::Hit;

    /// Channel bodies are built from the payload, so the no-seed guarantee has
    /// to survive the per-channel formatting too.
    #[test]
    fn channel_formatting_carries_no_secrets() {
        let mut hit = Hit::synthetic();
        hit.mnemonic = "sentinel mnemonic words here".into();
        let p = AlertPayload::from_hit(&hit);

        let telegram = format!("*{}*\n```\n{}\n```", p.title(), p.body());
        let webhook = serde_json::to_string(&p).unwrap();

        for s in [&telegram, &webhook, &p.body(), &p.title()] {
            assert!(!s.contains("sentinel mnemonic"), "seed leaked: {s}");
        }
    }

    /// Der Selbsttest muss klingen — genau das will man von ihm wissen. Nur
    /// das täglich von allein kommende Lebenszeichen bleibt stumm.
    #[test]
    fn only_the_heartbeat_stays_silent() {
        assert!(wants_sound(AlertKind::Hit));
        assert!(wants_sound(AlertKind::Test), "ein stummer Selbsttest");
        assert!(!wants_sound(AlertKind::Heartbeat));
    }

    #[test]
    fn ntfy_builds_a_sane_url() {
        // Trailing/leading slashes must not produce a double slash.
        let n = Ntfy {
            base_url: "https://ntfy.sh/".into(),
            topic: "/my-topic".into(),
            token: None,
        };
        let url = format!(
            "{}/{}",
            n.base_url.trim_end_matches('/'),
            n.topic.trim_start_matches('/')
        );
        assert_eq!(url, "https://ntfy.sh/my-topic");
    }
}
