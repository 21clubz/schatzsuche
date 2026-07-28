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

/// Local desktop notification. Cannot be lost to a network outage, which makes
/// it a useful floor even when everything else is configured.
pub struct Desktop;

impl Notifier for Desktop {
    fn name(&self) -> &str {
        "desktop"
    }

    fn send(&self, p: &AlertPayload) -> Result<(), String> {
        notify_rust::Notification::new()
            .summary(&p.title())
            .body(&p.body())
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
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
