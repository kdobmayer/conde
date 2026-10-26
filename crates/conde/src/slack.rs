//! Slack DMs for people on the shared board (docs/DESIGN-human-paging.md): a server-side reader
//! of the notification outbox that sends each page an agent sends its human (`human.page`) to the
//! human's owner as a Slack DM, once per page, only while the page is unread and younger than
//! [`MAX_AGE_MS`]. Nothing else reaches Slack: Slack is a one-way notification channel.
//!
//! Each owner's Slack member id is `users.slack_member_id`: imported once from
//! `CONDE_SLACK_NOTIFY` where empty, looked up by the sign-in email (`users.lookupByEmail`) at
//! sign-in, at startup and when a page waits for it, or set by the admin (never replaced by a
//! lookup). A send refused for an unknown or inactive user looks the id up once more and, when it
//! changed, sends the page again.
//!
//! The Slack copy is a side channel: it never changes the notification, so the web views and the
//! badges read as they would without it; only the page's sender sees its Slack state (`conde
//! sent`). Each send is one attempt of [`SLACK_INSTANCE`], and the attempt alone records it:
//! dispatching under a lease (`not_before`), then delivered, refused (final), dropped (read
//! meanwhile), abandoned (lease ran out), or failed and due again at `not_before` (the backoff or
//! Slack's Retry-After).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::dispatch::Dispatcher;
use crate::store::{Agent, Message, SLACK_INSTANCE, Tx, UserSlack};
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
/// The longest Retry-After honoured, in seconds.
const MAX_RETRY_AFTER: i64 = 3600;
/// Slack's `error` codes that mean the token or the member id is wrong, not this one message.
const CONFIG_ERRORS: &[&str] = &[
    "invalid_auth",
    "not_authed",
    "token_revoked",
    "account_inactive",
    "missing_scope",
    "channel_not_found",
    "no_permission",
];
/// Slack's `error` codes after which the member id is looked up again once (the person's id
/// changed, or the stored one was wrong).
const RELOOKUP: &[&str] = &["user_not_found", "account_inactive", "channel_not_found"];
/// Slack's `error` codes worth another attempt; any other is final.
const RETRYABLE: &[&str] = &["internal_error", "fatal_error", "service_unavailable", "request_timeout"];

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
    /// `CONDE_SLACK_NOTIFY` (board owner, lower-cased, to Slack member id): imported once into
    /// the users whose member id is empty.
    pub import: BTreeMap<String, String>,
    /// `https://slack.com/api` unless a test points it elsewhere.
    pub api_base: String,
    /// The board's name as the message shows it.
    pub board: String,
    /// The web app's origin for the link to the conversation; no link without it.
    pub public_url: Option<String>,
}

impl SlackConfig {
    /// From `CONDE_SLACK_BOT_TOKEN` (and `CONDE_SLACK_NOTIFY` for the import); `Ok(None)`
    /// (feature off) without a token.
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
        let Some(token) = token else {
            if notify.is_some() {
                tracing::warn!("slack notifications off: CONDE_SLACK_BOT_TOKEN is not set");
            }
            return Ok(None);
        };
        Ok(Some(SlackConfig {
            token: Token::new(&token),
            import: notify.as_deref().map(parse_members).transpose()?.unwrap_or_default(),
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
        if !is_member_id(member) {
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

/// A Slack member id as Slack writes one: `U...` or `W...`, upper-case letters and digits.
pub fn is_member_id(id: &str) -> bool {
    id.len() >= 3 && id.starts_with(['U', 'W']) && id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Copies `CONDE_SLACK_NOTIFY` into `users.slack_member_id` where it is empty (`slack_source =
/// env`), in the caller's write transaction; the owners imported.
pub fn import_env(tx: &dyn Tx, config: &SlackConfig) -> anyhow::Result<Vec<String>> {
    let mut imported = Vec::new();
    for (owner, member) in &config.import {
        let Some(user_id) = tx.user_id(owner)? else {
            tracing::warn!(%owner, "slack: CONDE_SLACK_NOTIFY names an owner the board does not have");
            continue;
        };
        if tx.user_slack(user_id)?.is_some_and(|u| u.slack_member_id.is_none()) {
            tx.set_user_slack(user_id, member, "env")?;
            tracing::info!(%owner, "slack: member id imported from CONDE_SLACK_NOTIFY");
            imported.push(owner.clone());
        }
    }
    Ok(imported)
}

/// One claimed send.
#[derive(Clone, Debug)]
pub struct Job {
    pub owner: String,
    pub owner_id: i64,
    pub attempt_id: String,
    pub notification_id: String,
    pub channel: String,
    pub text: String,
}

/// What Slack made of one send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// Final: this message is never sent to this owner again.
    Refused(String),
    /// Sent again after the board's backoff.
    Retry(String),
    /// Slack's rate limit: sent again after `after_ms` (Retry-After) or the backoff; the rest of
    /// the owner's pass waits too.
    RateLimited { after_ms: Option<i64> },
}

pub fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(crate::login::tls()?)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()?)
}

/// Imports `CONDE_SLACK_NOTIFY`, then runs the worker until the store's event stream closes,
/// restarted after a panic.
pub async fn spawn(d: Arc<Dispatcher>, config: Arc<SlackConfig>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let http = client()?;
    let (d2, c2) = (d.clone(), config.clone());
    tokio::task::spawn_blocking(move || d2.with_tx(|tx| import_env(tx, &c2))).await??;
    d.set_slack(true);
    Ok(tokio::spawn(supervise(TICK, move || {
        let mut w = Worker::new(d.clone(), config.clone(), http.clone());
        async move { w.run().await }
    })))
}

/// Runs the task `make` builds until it returns; a panic is logged and a new one started after `pause`.
pub async fn supervise<F, T>(pause: Duration, mut make: F)
where
    F: FnMut() -> T,
    T: Future<Output = ()> + Send + 'static,
{
    loop {
        match tokio::spawn(make()).await {
            Ok(()) => return,
            Err(e) => {
                tracing::error!("slack worker stopped: {e}; restarting");
                tokio::time::sleep(pause).await;
            }
        }
    }
}

/// The worker's state between passes.
pub struct Worker {
    d: Arc<Dispatcher>,
    config: Arc<SlackConfig>,
    http: reqwest::Client,
    /// The first pass looks up every user without a member id.
    startup: bool,
    /// Pages whose owner's member id was looked up while they waited, with when.
    looked_up: BTreeMap<String, i64>,
}

impl Worker {
    pub fn new(d: Arc<Dispatcher>, config: Arc<SlackConfig>, http: reqwest::Client) -> Worker {
        Worker { d, config, http, startup: true, looked_up: BTreeMap::new() }
    }

    /// A pass on every store event and every [`TICK`].
    async fn run(&mut self) {
        let mut wakes = self.d.store().pending().events();
        loop {
            self.pass().await;
            tokio::select! {
                r = wakes.recv() => if let Err(broadcast::error::RecvError::Closed) = r { return },
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }

    /// One pass: the member ids to look up, then claim what is due and send each; the number of
    /// posts made.
    pub async fn pass(&mut self) -> usize {
        if let Err(e) = self.lookups().await {
            tracing::warn!("slack: member id lookups failed: {e:#}");
        }
        let (d, config, http) = (&self.d, &self.config, &self.http);
        let jobs = match claim_due(d, config).await {
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::warn!("slack: claim failed: {e:#}");
                return 0;
            }
        };
        let mut posted = 0;
        let mut limited: BTreeMap<String, Option<i64>> = BTreeMap::new();
        for job in jobs {
            if let Some(after_ms) = limited.get(&job.owner) {
                // Not sent: due again with the rate-limited one.
                record(d, job, Outcome::RateLimited { after_ms: *after_ms }).await;
                continue;
            }
            let owner = job.owner.clone();
            match deliver(d, config, http, job).await {
                Some(Outcome::RateLimited { after_ms }) => {
                    posted += 1;
                    limited.insert(owner, after_ms);
                }
                Some(_) => posted += 1,
                None => {}
            }
        }
        posted
    }

    /// Looks up the member id by email of the users without one: the sign-ins since the last
    /// pass, every such user on the first pass, and the owner of each page waiting for it (once
    /// per page).
    async fn lookups(&mut self) -> anyhow::Result<()> {
        let d = self.d.clone();
        let now = d.now_ms();
        let (users, due) = tokio::task::spawn_blocking(move || {
            d.with_conn(|tx| Ok((tx.users_slack()?, tx.slack_due(now, now - MAX_AGE_MS, BATCH)?)))
        })
        .await??;
        let missing = |id: &i64| users.iter().any(|u| u.user_id == *id && u.slack_member_id.is_none());
        let mut wanted = self.d.take_slack_lookups();
        if std::mem::take(&mut self.startup) {
            wanted.extend(users.iter().filter(|u| u.slack_member_id.is_none()).map(|u| u.user_id));
        }
        self.looked_up.retain(|_, at| now - *at <= MAX_AGE_MS);
        for n in &due {
            if missing(&n.owner_id) && !self.looked_up.contains_key(&n.message_id) {
                self.looked_up.insert(n.message_id.clone(), now);
                wanted.insert(n.owner_id);
            }
        }
        for user in users.iter().filter(|u| wanted.contains(&u.user_id) && u.slack_member_id.is_none()) {
            self.look_up(user, None).await;
        }
        Ok(())
    }

    /// Looks `user`'s member id up by email and stores it (`lookup`) unless the admin set one; the
    /// id found when it differs from `current`.
    async fn look_up(&self, user: &UserSlack, current: Option<&str>) -> Option<String> {
        if user.slack_source.as_deref() == Some("admin") {
            return None;
        }
        let email = user.email.as_deref()?;
        let found = match lookup_by_email(&self.http, &self.config, email).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                tracing::info!(owner = %user.name, "slack: no Slack user has the owner's email");
                return None;
            }
            Err(why) => {
                tracing::warn!(owner = %user.name, %why, "slack: member id lookup failed");
                return None;
            }
        };
        if current == Some(found.as_str()) {
            return None;
        }
        let (d, user_id, id) = (self.d.clone(), user.user_id, found.clone());
        let stored = tokio::task::spawn_blocking(move || {
            d.with_tx(|tx| {
                // the admin's id is never replaced; a concurrent lookup's is
                if tx.user_slack(user_id)?.is_some_and(|u| u.slack_source.as_deref() != Some("admin")) {
                    tx.set_user_slack(user_id, &id, "lookup")?;
                    return Ok(true);
                }
                Ok(false)
            })
        })
        .await;
        match stored {
            Ok(Ok(true)) => {
                tracing::info!(owner = %user.name, "slack: member id found by email");
                Some(found)
            }
            Ok(Ok(false)) => None,
            Ok(Err(e)) => {
                tracing::warn!(owner = %user.name, "slack: member id not stored: {e:#}");
                None
            }
            Err(e) => {
                tracing::warn!(owner = %user.name, "slack: member id not stored: {e}");
                None
            }
        }
    }
}

/// Claims the due pages as attempts, in one write transaction.
pub async fn claim_due(d: &Arc<Dispatcher>, config: &Arc<SlackConfig>) -> anyhow::Result<Vec<Job>> {
    let (d, config) = (d.clone(), config.clone());
    tokio::task::spawn_blocking(move || {
        let now = d.now_ms();
        // A read first: most passes find nothing, and a write transaction takes the board-wide lock.
        let due = d.with_conn(|tx| {
            let reachable: BTreeSet<i64> =
                tx.users_slack()?.into_iter().filter(|u| u.slack_member_id.is_some()).map(|u| u.user_id).collect();
            Ok(tx.slack_due(now, now - MAX_AGE_MS, BATCH)?.iter().any(|n| reachable.contains(&n.owner_id)))
        })?;
        if !due {
            return Ok(Vec::new());
        }
        d.with_tx(|tx| claim(tx, &config, now))
    })
    .await?
}

/// Sends one claimed job and records its outcome; None when the job was no longer this pass's to
/// send (taken over after its lease, or the message read meanwhile) and nothing was posted.
pub async fn deliver(d: &Arc<Dispatcher>, config: &Arc<SlackConfig>, http: &reqwest::Client, mut job: Job) -> Option<Outcome> {
    if !still_ours(d, &job).await {
        return None;
    }
    let mut outcome = post(http, config, &job).await;
    if let Outcome::Refused(why) = &outcome
        && why.strip_prefix("slack: ").is_some_and(|code| RELOOKUP.contains(&code))
        && let Some(id) = relookup(d, config, http, &job).await
    {
        tracing::info!(owner = %job.owner, notification = %job.notification_id, "slack: member id changed; sending again");
        job.channel = id;
        if !still_ours(d, &job).await {
            return None;
        }
        outcome = post(http, config, &job).await;
    }
    match &outcome {
        Outcome::Delivered => tracing::info!(notification = %job.notification_id, "slack: delivered"),
        Outcome::Refused(why) => match why.strip_prefix("slack: ").filter(|code| CONFIG_ERRORS.contains(code)) {
            Some(code) => tracing::error!(
                owner = %job.owner, code, notification = %job.notification_id,
                "slack: refused for a configuration fault (bot token or member id), every message to this owner fails"
            ),
            None => tracing::warn!(notification = %job.notification_id, reason = %why, "slack: refused"),
        },
        Outcome::Retry(why) => tracing::warn!(notification = %job.notification_id, reason = %why, "slack: retrying"),
        Outcome::RateLimited { after_ms } => {
            tracing::warn!(notification = %job.notification_id, ?after_ms, "slack: rate limited")
        }
    }
    record(d, job, outcome.clone()).await;
    Some(outcome)
}

/// [`fence`] in its own transaction: whether the job may post now.
async fn still_ours(d: &Arc<Dispatcher>, job: &Job) -> bool {
    let (d2, j) = (d.clone(), job.clone());
    let fenced = tokio::task::spawn_blocking(move || {
        let now = d2.now_ms();
        d2.with_tx(|tx| fence(tx, &j, now))
    })
    .await;
    match fenced {
        Ok(Ok(true)) => true,
        Ok(Ok(false)) => {
            tracing::info!(notification = %job.notification_id, "slack: not sent (taken over or read)");
            false
        }
        Ok(Err(e)) => {
            tracing::warn!(notification = %job.notification_id, "slack: not sent, the board did not answer: {e:#}");
            false
        }
        Err(e) => {
            tracing::warn!(notification = %job.notification_id, "slack: not sent: {e}");
            false
        }
    }
}

/// After a send refused for an unknown or inactive user: the owner's member id looked up once
/// more, when it changed.
async fn relookup(d: &Arc<Dispatcher>, config: &Arc<SlackConfig>, http: &reqwest::Client, job: &Job) -> Option<String> {
    let (d2, owner_id) = (d.clone(), job.owner_id);
    let user = tokio::task::spawn_blocking(move || d2.with_conn(|tx| tx.user_slack(owner_id))).await.ok()?.ok()??;
    let w = Worker::new(d.clone(), config.clone(), http.clone());
    w.look_up(&user, Some(&job.channel)).await
}

async fn record(d: &Arc<Dispatcher>, job: Job, outcome: Outcome) {
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
    let members: BTreeMap<i64, String> =
        tx.users_slack()?.into_iter().filter_map(|u| Some((u.user_id, u.slack_member_id?))).collect();
    let mut jobs = Vec::new();
    let mut messages = BTreeSet::new();
    for n in tx.slack_due(now, now - MAX_AGE_MS, BATCH)? {
        let Some(member) = members.get(&n.owner_id) else { continue };
        // One DM per page, whichever of the owner's human rows it reached.
        if !messages.insert((n.owner_id, n.message_id.clone())) {
            continue;
        }
        let (Some(m), Some(rcpt)) = (tx.message_by_id(&n.message_id)?, tx.agent_by_id(&n.recipient_id)?) else {
            continue;
        };
        // A send of this page still dispatching outlived its lease: its replica died mid-send.
        tx.slack_abandon(&n.message_id, &n.owner, &iso_from_ms(now))?;
        let text = render(config, &m, &rcpt);
        let attempt_id = new_ulid();
        tx.insert_attempt(&attempt_id, &n.id, SLACK_INSTANCE, rcpt.binding_version, SLACK_INSTANCE, &text)?;
        tx.set_attempt_not_before(&attempt_id, now + LEASE_MS)?;
        jobs.push(Job {
            owner: n.owner,
            owner_id: n.owner_id,
            attempt_id,
            notification_id: n.id,
            channel: member.clone(),
            text,
        });
    }
    Ok(jobs)
}

/// Just before a post: the attempt must still be dispatching inside its lease (then the lease is
/// renewed for the post, so no other pass can take it over meanwhile), and the message still
/// unread; otherwise the attempt is closed and nothing is posted.
fn fence(tx: &dyn Tx, job: &Job, now: i64) -> anyhow::Result<bool> {
    let at = iso_from_ms(now);
    if tx.attempt(&job.attempt_id)?.is_none_or(|(_, status)| status != "dispatching") {
        return Ok(false);
    }
    if tx.attempt_not_before(&job.attempt_id)?.is_none_or(|lease| lease <= now) {
        tx.update_attempt(&job.attempt_id, "abandoned", Some("lease expired"), None, &at)?;
        return Ok(false);
    }
    if let Some((_, message_id, rcpt)) = tx.attempt_target(&job.attempt_id)?
        && let Some(m) = tx.message_by_id(&message_id)?
        && read_by(tx, &m, &rcpt)?
    {
        tx.update_attempt(&job.attempt_id, "dropped", Some("read"), None, &at)?;
        return Ok(false);
    }
    tx.set_attempt_not_before(&job.attempt_id, now + LEASE_MS)?;
    Ok(true)
}

fn read_by(tx: &dyn Tx, m: &Message, rcpt: &str) -> anyhow::Result<bool> {
    Ok(match &m.channel_id {
        Some(c) => tx.read_seq(c, rcpt)?.is_some_and(|s| s >= m.seq),
        None => tx.dm_read_at(&m.id, rcpt)?.is_some(),
    })
}

/// Records the outcome on the attempt while it is still dispatching (not taken over); the
/// notification is never touched.
fn settle(tx: &dyn Tx, job: &Job, outcome: &Outcome, now: i64, retry_ms: i64) -> anyhow::Result<()> {
    if tx.attempt(&job.attempt_id)?.is_none_or(|(_, status)| status != "dispatching") {
        return Ok(());
    }
    let (status, reason, again) = match outcome {
        Outcome::Delivered => ("delivered", None, None),
        Outcome::Refused(why) => ("refused", Some(why.as_str()), None),
        Outcome::Retry(why) => ("failed", Some(why.as_str()), Some(retry_ms)),
        Outcome::RateLimited { after_ms } => ("failed", Some("slack: ratelimited"), Some(after_ms.unwrap_or(retry_ms))),
    };
    tx.update_attempt(&job.attempt_id, status, reason, None, &iso_from_ms(now))?;
    if let Some(wait) = again {
        tx.set_attempt_not_before(&job.attempt_id, now + wait)?;
    }
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

/// Who and what kind of page (`is blocked on you:` or `finished:`), the line, and the link to
/// the conversation in the human's web view (`#/a/<human>/dm/<owner>/<name>`) on the board.
pub fn render(config: &SlackConfig, m: &Message, rcpt: &Agent) -> String {
    let (sender, board) = (escape(&m.sender), escape(&config.board));
    let mut text = match m.page.as_deref() {
        Some("done") => format!("{sender} finished:"),
        _ => format!("{sender} is blocked on you:"),
    };
    let line = first_line(&m.text);
    if !line.is_empty() {
        text.push_str(&format!("\n>{}", escape(&line)));
    }
    match &config.public_url {
        Some(origin) => {
            let conversation = match &m.channel {
                Some(c) => format!("ch/{}", encode(c)),
                None => {
                    let (owner, name) = m.sender.split_once('/').unwrap_or(("", &m.sender));
                    format!("dm/{}/{}", encode(owner), encode(name))
                }
            };
            let url = format!("{origin}/#/a/{}/{conversation}", encode(&rcpt.name));
            text.push_str(&format!("\n<{}|Open in conde on {board}>", escape(&url)));
        }
        None => text.push_str(&format!("\non {board}")),
    }
    text
}

/// A Retry-After header in milliseconds: whole seconds, clamped to 1..=[`MAX_RETRY_AFTER`].
pub fn retry_after_ms(header: Option<&str>) -> Option<i64> {
    header.and_then(|v| v.trim().parse::<i64>().ok()).map(|s| s.clamp(1, MAX_RETRY_AFTER) * 1000)
}

/// `users.lookupByEmail`: the member id of the Slack user with this email, None when Slack has
/// none (`users_not_found`); Err with why when the lookup itself failed.
pub async fn lookup_by_email(http: &reqwest::Client, config: &SlackConfig, email: &str) -> Result<Option<String>, String> {
    let url = format!("{}/users.lookupByEmail?email={}", config.api_base.trim_end_matches('/'), encode(email));
    let response = http.get(url).bearer_auth(&config.token.0).send().await.map_err(|e| format!("slack unreachable: {}", e.without_url()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("slack answered {status}"));
    }
    let answer: Value = response.json().await.map_err(|_| "slack's answer is not JSON".to_string())?;
    if answer["ok"] == true {
        return match answer["user"]["id"].as_str() {
            Some(id) if is_member_id(id) => Ok(Some(id.to_string())),
            _ => Err("slack's answer has no member id".into()),
        };
    }
    match answer["error"].as_str().unwrap_or("unknown_error") {
        "users_not_found" => Ok(None),
        other => Err(format!("slack: {}", other.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect::<String>())),
    }
}

async fn post(http: &reqwest::Client, config: &SlackConfig, job: &Job) -> Outcome {
    let url = format!("{}/chat.postMessage", config.api_base.trim_end_matches('/'));
    let body = json!({"channel": job.channel, "text": job.text, "unfurl_links": false, "unfurl_media": false});
    let response = match http.post(url).bearer_auth(&config.token.0).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return Outcome::Retry(format!("slack unreachable: {}", e.without_url())),
    };
    let status = response.status();
    let retry_after =
        retry_after_ms(response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()));
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Outcome::RateLimited { after_ms: retry_after };
    }
    if status.is_server_error() {
        return Outcome::Retry(format!("slack answered {status}"));
    }
    if !status.is_success() {
        return Outcome::Refused(format!("slack answered {status}"));
    }
    let answer: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return Outcome::Retry("slack's answer is not JSON".into()),
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
    if error == "ratelimited" {
        Outcome::RateLimited { after_ms: retry_after }
    } else if RETRYABLE.contains(&error.as_str()) {
        Outcome::Retry(format!("slack: {error}"))
    } else {
        Outcome::Refused(format!("slack: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_with_a_token_and_the_map_is_only_an_import() {
        let some = |s: &str| Some(s.to_string());
        assert!(SlackConfig::from_vars(None, None, "b", None).unwrap().is_none());
        assert!(SlackConfig::from_vars(None, some("kdobmayer=U01"), "b", None).unwrap().is_none());
        assert!(SlackConfig::from_vars(some("xoxb-1"), None, "b", None).unwrap().unwrap().import.is_empty());
        let c = SlackConfig::from_vars(some("xoxb-1"), some(" KDobmayer = U0123ABC , other=W0456DEF "), "b", None)
            .unwrap()
            .unwrap();
        assert_eq!(c.import.get("kdobmayer").map(String::as_str), Some("U0123ABC"));
        assert_eq!(c.import.get("other").map(String::as_str), Some("W0456DEF"));
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

    #[test]
    fn retry_after_is_clamped_without_overflow() {
        assert_eq!(retry_after_ms(Some("120")), Some(120_000));
        assert_eq!(retry_after_ms(Some("0")), Some(1000));
        assert_eq!(retry_after_ms(Some("-5")), Some(1000));
        assert_eq!(retry_after_ms(Some("9223372036854775807")), Some(3_600_000));
        assert_eq!(retry_after_ms(Some("99999999999999999999")), None);
        assert_eq!(retry_after_ms(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        assert_eq!(retry_after_ms(None), None);
    }

    #[test]
    fn a_panicking_worker_is_logged_and_restarted() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = runs.clone();
        rt.block_on(supervise(Duration::from_millis(1), move || {
            let n = r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if n == 0 {
                    panic!("first run fails");
                }
            }
        }));
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
