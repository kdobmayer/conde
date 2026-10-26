//! Slack DMs for people on the shared board: a server-side consumer of the notification outbox
//! for the humans of the owners `CONDE_SLACK_NOTIFY` maps to a Slack member id. What notifies is
//! the board's push rule (a DM, an @mention, a human's channel post; never a reaction or a plain
//! post). A bridge only badges a human, so pending and badged notifications are both taken. Each
//! send is one attempt of [`SLACK_INSTANCE`]: delivered, refused (Slack refused it for good), or
//! failed and retried after the backoff or Slack's Retry-After. A notification older than
//! [`MAX_AGE_MS`] is left to the unread badge.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::dispatch::Dispatcher;
use crate::store::{Agent, Message, SLACK_INSTANCE, Tx, topics};
use crate::util::{iso_from_ms, new_ulid};

pub const API_BASE: &str = "https://slack.com/api";
pub const MAX_AGE_MS: i64 = 3_600_000;
/// How long a claimed send may run before another pass takes it over (a replica that died mid-send).
pub const LEASE_MS: i64 = 60_000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// A pass also runs this often without a store event, for retries falling due.
const TICK: Duration = Duration::from_secs(5);
const BATCH: i64 = 20;
const LINE_CHARS: usize = 140;
/// Slack's `error` codes worth another attempt; any other is final.
const RETRYABLE: &[&str] = &["ratelimited", "internal_error", "fatal_error", "service_unavailable", "request_timeout"];

/// The bot token; never printed.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub fn new(token: &str) -> Token {
        Token(token.trim().to_string())
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub struct SlackConfig {
    pub token: Token,
    /// Board owner (user name, lower-cased) to Slack member id.
    pub members: BTreeMap<String, String>,
    /// `https://slack.com/api` unless a test points it elsewhere.
    pub api_base: String,
    /// The board's name as the message shows it.
    pub board: String,
    /// The web app's origin for the link to the conversation; no link without it.
    pub public_url: Option<String>,
}

impl SlackConfig {
    /// From `CONDE_SLACK_BOT_TOKEN` and `CONDE_SLACK_NOTIFY`; `Ok(None)` (feature off) unless both are set.
    pub fn from_env(board: &str, public_url: Option<String>) -> anyhow::Result<Option<SlackConfig>> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        SlackConfig::from_vars(var("CONDE_SLACK_BOT_TOKEN"), var("CONDE_SLACK_NOTIFY"), board, public_url)
    }

    pub fn from_vars(
        token: Option<String>,
        notify: Option<String>,
        board: &str,
        public_url: Option<String>,
    ) -> anyhow::Result<Option<SlackConfig>> {
        let (token, notify) = match (token, notify) {
            (Some(t), Some(n)) => (t, n),
            (None, None) => return Ok(None),
            (t, _) => {
                let missing = if t.is_none() { "CONDE_SLACK_BOT_TOKEN" } else { "CONDE_SLACK_NOTIFY" };
                tracing::warn!("slack notifications off: {missing} is not set");
                return Ok(None);
            }
        };
        Ok(Some(SlackConfig {
            token: Token::new(&token),
            members: parse_members(&notify)?,
            api_base: API_BASE.to_string(),
            board: board.to_string(),
            public_url: public_url.map(|u| u.trim_end_matches('/').to_string()),
        }))
    }
}

/// `CONDE_SLACK_NOTIFY`: comma-separated `<owner>=<Slack member id>`.
pub fn parse_members(raw: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut members = BTreeMap::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (owner, member) = entry
            .split_once('=')
            .with_context(|| format!("CONDE_SLACK_NOTIFY entry '{entry}' is not <owner>=<Slack member id>"))?;
        let (owner, member) = (owner.trim().to_lowercase(), member.trim());
        if owner.is_empty() || owner.chars().any(|c| c.is_whitespace() || c == '/') {
            bail!("CONDE_SLACK_NOTIFY entry '{entry}': '{owner}' is not an owner name");
        }
        let id_like = member.len() >= 3
            && member.starts_with(['U', 'W'])
            && member.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        if !id_like {
            bail!("CONDE_SLACK_NOTIFY entry '{entry}': '{member}' is not a Slack member id (U... or W...)");
        }
        if members.insert(owner.clone(), member.to_string()).is_some() {
            bail!("CONDE_SLACK_NOTIFY names '{owner}' twice");
        }
    }
    if members.is_empty() {
        bail!("CONDE_SLACK_NOTIFY names nobody");
    }
    Ok(members)
}

/// One claimed send.
#[derive(Clone, Debug)]
pub struct Job {
    pub attempt_id: String,
    pub notification_id: String,
    pub channel: String,
    pub text: String,
}

/// What Slack made of one send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// Final: the notification is refused and never sent again.
    Refused(String),
    /// Sent again after `after_ms`, or the board's backoff when Slack named no wait.
    Retry { reason: String, after_ms: Option<i64> },
}

pub fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(crate::login::tls()?)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()?)
}

/// Runs the worker until the store's event stream closes: a pass on every store event and every [`TICK`].
pub fn spawn(d: Arc<Dispatcher>, config: Arc<SlackConfig>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let http = client()?;
    let mut wakes = d.store().pending().events();
    Ok(tokio::spawn(async move {
        loop {
            run_once(&d, &config, &http).await;
            tokio::select! {
                r = wakes.recv() => if let Err(broadcast::error::RecvError::Closed) = r { return },
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }))
}

/// One pass: claim what is due, send each, settle each; the number of sends.
pub async fn run_once(d: &Arc<Dispatcher>, config: &Arc<SlackConfig>, http: &reqwest::Client) -> usize {
    let jobs = match claim_due(d, config).await {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("slack: claim failed: {e:#}");
            return 0;
        }
    };
    let sent = jobs.len();
    for job in jobs {
        deliver(d, config, http, job).await;
    }
    sent
}

/// Claims the due notifications as attempts, in one write transaction.
pub async fn claim_due(d: &Arc<Dispatcher>, config: &Arc<SlackConfig>) -> anyhow::Result<Vec<Job>> {
    let (d, config) = (d.clone(), config.clone());
    tokio::task::spawn_blocking(move || {
        let now = d.now_ms();
        // A read first: most passes find nothing, and a write transaction takes the board-wide lock.
        let due = d.with_conn(|tx| {
            for owner in config.members.keys() {
                if !tx.due_human_notifications(owner, now, now - MAX_AGE_MS, 1)?.is_empty() {
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
        if !due {
            return Ok(Vec::new());
        }
        d.with_tx(|tx| claim(tx, &config, now))
    })
    .await?
}

/// Sends one claimed job and records its outcome.
pub async fn deliver(d: &Arc<Dispatcher>, config: &Arc<SlackConfig>, http: &reqwest::Client, job: Job) {
    let outcome = post(http, config, &job).await;
    match &outcome {
        Outcome::Delivered => tracing::info!(notification = %job.notification_id, "slack: delivered"),
        Outcome::Refused(why) => tracing::warn!(notification = %job.notification_id, reason = %why, "slack: refused"),
        Outcome::Retry { reason, .. } => tracing::warn!(notification = %job.notification_id, %reason, "slack: retrying"),
    }
    let d = d.clone();
    let id = job.notification_id.clone();
    let settled = tokio::task::spawn_blocking(move || {
        let (now, retry_ms) = (d.now_ms(), d.retry_ms());
        d.with_tx(|tx| settle(tx, &job, &outcome, now, retry_ms))
    })
    .await;
    match settled {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(notification = %id, "slack: outcome not recorded: {e:#}"),
        Err(e) => tracing::warn!(notification = %id, "slack: outcome not recorded: {e}"),
    }
}

fn claim(tx: &dyn Tx, config: &SlackConfig, now: i64) -> anyhow::Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for (owner, member) in &config.members {
        for n in tx.due_human_notifications(owner, now, now - MAX_AGE_MS, BATCH)? {
            // A send still dispatching here outlived its lease: its replica died mid-send.
            if let Some(prev) = tx.latest_attempt(&n.id)?
                && tx.attempt(&prev)?.is_some_and(|(_, status)| status == "dispatching")
            {
                tx.update_attempt(&prev, "abandoned", Some("lease expired"), None, &iso_from_ms(now))?;
            }
            let (Some(m), Some(rcpt)) = (tx.message_by_id(&n.message_id)?, tx.agent_by_id(&n.recipient_id)?) else {
                tx.set_notification_status(&n.id, "gone")?;
                continue;
            };
            if read_by(tx, &m, &rcpt.id)? {
                tx.set_notification_status(&n.id, "read")?;
                continue;
            }
            let text = render(config, &n.kind, &m, &rcpt);
            let attempt_id = new_ulid();
            tx.insert_attempt(&attempt_id, &n.id, SLACK_INSTANCE, rcpt.binding_version, SLACK_INSTANCE, &text)?;
            tx.settle_notification(&n.id, "dispatching", Some(now + LEASE_MS))?;
            jobs.push(Job { attempt_id, notification_id: n.id, channel: member.clone(), text });
        }
    }
    if !jobs.is_empty() {
        tx.announce(topics::DELIVERY);
    }
    Ok(jobs)
}

fn read_by(tx: &dyn Tx, m: &Message, rcpt: &str) -> anyhow::Result<bool> {
    Ok(match &m.channel_id {
        Some(c) => tx.read_seq(c, rcpt)?.is_some_and(|s| s >= m.seq),
        None => tx.dm_read_at(&m.id, rcpt)?.is_some(),
    })
}

/// The attempt takes the outcome; the notification follows only while this attempt still holds it
/// (not read meanwhile, not taken over after the lease).
fn settle(tx: &dyn Tx, job: &Job, outcome: &Outcome, now: i64, retry_ms: i64) -> anyhow::Result<()> {
    let (attempt, reason, status, next) = match outcome {
        Outcome::Delivered => ("delivered", None, "delivered", None),
        Outcome::Refused(why) => ("refused", Some(why.as_str()), "refused", None),
        Outcome::Retry { reason, after_ms } => {
            ("failed", Some(reason.as_str()), "pending", Some(now + after_ms.unwrap_or(retry_ms)))
        }
    };
    tx.update_attempt(&job.attempt_id, attempt, reason, None, &iso_from_ms(now))?;
    if tx.latest_attempt(&job.notification_id)?.as_deref() == Some(job.attempt_id.as_str())
        && tx.notification_status(&job.notification_id)?.as_deref() == Some("dispatching")
    {
        tx.settle_notification(&job.notification_id, status, next)?;
    }
    tx.announce(topics::DELIVERY);
    Ok(())
}

/// Escapes Slack's mrkdwn control characters, so no text becomes a link or a mention.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// encodeURIComponent's unreserved set.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The first non-empty line, at most [`LINE_CHARS`] characters.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if line.chars().count() <= LINE_CHARS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(LINE_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Who, where, the first line and the link to the conversation in the recipient's web view
/// (`#/a/<agent>/dm/<owner>/<name>` or `#/a/<agent>/ch/<channel>`). Never the full body.
pub fn render(config: &SlackConfig, kind: &str, m: &Message, rcpt: &Agent) -> String {
    let (sender, place, board) = (escape(&m.sender), escape(&m.place()), escape(&config.board));
    let mut text = match kind {
        "dm" => format!("{sender} sent you a message on {board}:"),
        "mention" => format!("{sender} mentioned you in {place} on {board}:"),
        _ => format!("{sender} posted in {place} on {board}:"),
    };
    let line = first_line(&m.text);
    if !line.is_empty() {
        text.push_str(&format!("\n>{}", escape(&line)));
    }
    if let Some(origin) = &config.public_url {
        let conversation = match &m.channel {
            Some(c) => format!("ch/{}", encode(c)),
            None => {
                let (owner, name) = m.sender.split_once('/').unwrap_or(("", &m.sender));
                format!("dm/{}/{}", encode(owner), encode(name))
            }
        };
        let url = format!("{origin}/#/a/{}/{conversation}", encode(&rcpt.name));
        text.push_str(&format!("\n<{}|Open in conde>", escape(&url)));
    }
    text
}

async fn post(http: &reqwest::Client, config: &SlackConfig, job: &Job) -> Outcome {
    let url = format!("{}/chat.postMessage", config.api_base.trim_end_matches('/'));
    let body = json!({"channel": job.channel, "text": job.text, "unfurl_links": false, "unfurl_media": false});
    let response = match http.post(url).bearer_auth(&config.token.0).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return Outcome::Retry { reason: format!("slack unreachable: {}", e.without_url()), after_ms: None },
    };
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map(|s| s.max(1) * 1000);
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Outcome::Retry { reason: "slack: ratelimited".into(), after_ms: retry_after };
    }
    if status.is_server_error() {
        return Outcome::Retry { reason: format!("slack answered {status}"), after_ms: None };
    }
    if !status.is_success() {
        return Outcome::Refused(format!("slack answered {status}"));
    }
    let answer: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return Outcome::Retry { reason: "slack's answer is not JSON".into(), after_ms: None },
    };
    if answer["ok"] == true {
        return Outcome::Delivered;
    }
    let error: String = answer["error"]
        .as_str()
        .unwrap_or("unknown_error")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    if RETRYABLE.contains(&error.as_str()) {
        Outcome::Retry { reason: format!("slack: {error}"), after_ms: retry_after }
    } else {
        Outcome::Refused(format!("slack: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_unless_both_vars_are_set() {
        let some = |s: &str| Some(s.to_string());
        assert!(SlackConfig::from_vars(None, None, "b", None).unwrap().is_none());
        assert!(SlackConfig::from_vars(some("xoxb-1"), None, "b", None).unwrap().is_none());
        assert!(SlackConfig::from_vars(None, some("kdobmayer=U01"), "b", None).unwrap().is_none());
        let c = SlackConfig::from_vars(some("xoxb-1"), some(" KDobmayer = U0123ABC , other=W0456DEF "), "b", None)
            .unwrap()
            .unwrap();
        assert_eq!(c.members.get("kdobmayer").map(String::as_str), Some("U0123ABC"));
        assert_eq!(c.members.get("other").map(String::as_str), Some("W0456DEF"));
        assert!(!format!("{c:?}").contains("xoxb-1"));
    }

    #[test]
    fn a_bad_mapping_is_refused() {
        for bad in ["kdobmayer", "kdobmayer=", "=U0123", "kdobmayer=u0123", "kdobmayer=C0123", "a=U01,A=U02", ","] {
            assert!(parse_members(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_first_line_is_cut_at_140_characters() {
        let long = "é".repeat(200);
        let line = first_line(&format!("\n  \n{long}\nsecond"));
        assert_eq!(line.chars().count(), 140);
        assert!(line.ends_with('…'));
        assert_eq!(first_line("  short  \nnext"), "short");
    }
}
