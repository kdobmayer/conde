//! Postgres backend (cloud mode): any number of stateless server replicas
//! share one database.
//!
//! - Writes are serialized board-wide by `pg_advisory_xact_lock`, the
//!   equivalent of SQLite's single writer: ops keep their read-then-write
//!   shape and `commit_seq` order equals commit order.
//! - Migrations are embedded and run at startup under a session advisory
//!   lock, so replicas starting together apply each migration once.
//! - A write that makes a notification pending sends `NOTIFY conde_pending,
//!   '<host>'` inside its transaction (delivered on commit only); a listener
//!   thread per replica turns those into [`Wake`]s for local bridge pollers.
//! - TLS is rustls with the system CA bundle; chain and hostname are always
//!   verified, so `sslmode=verify-full` (mapped to tokio-postgres' `require`)
//!   means what libpq means by it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use postgres::config::{Host, SslMode};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::ToSql;
use postgres::{Client, Config, Row, Statement};
use tokio_postgres_rustls::MakeRustlsConnect;

use super::{
    Agent, Attachment, AttachmentMeta, AttemptRow, Channel, ChannelSummary, CredentialRow, DmDelivery, DueNotification,
    ImportState, ImportedAgent, Message, Mode, NewMessage, NewNotification, NotificationRow, OpenAttempt,
    PendingSignal, SearchScope, Store, StoreError, StoreResult, Tx, UserRow, Wake, Wakes, WebSessionRow, address_of,
};
use crate::address::Address;
use crate::util::sha256_hex;

impl From<postgres::Error> for StoreError {
    /// With the cause chain: "error connecting to server" alone does not say DNS, TLS or auth.
    fn from(e: postgres::Error) -> StoreError {
        let mut text = e.to_string();
        let mut cause = std::error::Error::source(&e);
        while let Some(c) = cause {
            text.push_str(&format!(": {c}"));
            cause = c.source();
        }
        StoreError(text)
    }
}

impl From<r2d2::Error> for StoreError {
    fn from(e: r2d2::Error) -> StoreError {
        StoreError(format!("connection pool: {e}"))
    }
}

/// Embedded migrations: (version, name, sql). Append only; an applied
/// migration must never change (its checksum is verified at startup).
pub const MIGRATIONS: &[(i32, &str, &str)] = &[
    (1, "init", include_str!("migrations/0001_init.sql")),
    (2, "channel_topic", include_str!("migrations/0002_channel_topic.sql")),
    (3, "web", include_str!("migrations/0003_web.sql")),
];

/// Session lock held while migrating.
const MIGRATION_LOCK: i64 = 0x636f_6e64_655f_6d69;
/// Transaction lock taken by every write transaction.
const WRITER_LOCK: i64 = 0x636f_6e64_655f_7772;
pub const PENDING_CHANNEL: &str = "conde_pending";
/// How long opening a store waits for its listener's first LISTEN.
const LISTEN_READY_WAIT: Duration = Duration::from_secs(15);
pub const FENCE_CHANNEL: &str = "conde_fence";
/// Hosts whose keys were revoked or whose user was disabled.
pub const REVOKE_CHANNEL: &str = "conde_revoke";
const POOL_SIZE: u32 = 8;
/// Session settings unless DATABASE_URL sets them: a client that dies inside
/// a write transaction must not hold the board's writer lock until TCP
/// keepalive notices; no op idles inside a transaction.
const SESSION_DEFAULTS: &[(&str, &str)] =
    &[("idle_in_transaction_session_timeout", "15s"), ("statement_timeout", "60s")];

// --- configuration ----------------------------------------------------------

/// DATABASE_URL taken apart: the postgres crate's Config plus what rustls needs.
pub struct PgUrl {
    pub config: Config,
    /// PEM bundle trusted instead of the system bundle (libpq `sslrootcert`).
    pub root_cert: Option<PathBuf>,
}

/// Parameters the postgres crate parses itself, passed through unchanged.
const PASSTHROUGH: &[&str] = &[
    "host",
    "hostaddr",
    "port",
    "user",
    "password",
    "dbname",
    "options",
    "application_name",
    "connect_timeout",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_retries",
    "target_session_attrs",
    "channel_binding",
    "load_balance_hosts",
    "sslnegotiation",
];

/// Parses a libpq connection URL (`postgres[ql]://...?k=v&...`) or key=value
/// string. The libpq TLS keys are consumed here: sslmode (verify-full,
/// verify-ca and require all mean full chain + hostname verification, the
/// only thing rustls does), sslrootcert (a PEM bundle, or "system"); client
/// certificates are refused; any other unknown key is a named error. A TCP
/// host other than loopback must use TLS. Errors never carry the password.
pub fn parse_url(url: &str) -> StoreResult<PgUrl> {
    let secrets = password_forms(url);
    parse_url_unmasked(url).map_err(|e| {
        let text = secrets.iter().fold(e.0, |t, p| t.replace(p.as_str(), "***"));
        StoreError(text)
    })
}

fn parse_url_unmasked(url: &str) -> StoreResult<PgUrl> {
    let is_url = url.starts_with("postgres://") || url.starts_with("postgresql://");
    let (base, pairs) = if is_url {
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let pairs = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (percent_decode(k), percent_decode(v), p.to_string())
            })
            .collect::<Vec<_>>();
        (base.to_string(), pairs)
    } else {
        (String::new(), keyword_pairs(url)?)
    };
    let mut kept: Vec<String> = Vec::new();
    let mut root_cert = None;
    let mut ssl_mode = None;
    for (key, value, raw) in pairs {
        match key.as_str() {
            "sslmode" => {
                ssl_mode = Some(match value.as_str() {
                    "disable" => "disable",
                    "allow" | "prefer" => "prefer",
                    "require" | "verify-ca" | "verify-full" => "require",
                    other => {
                        return Err(StoreError(format!(
                            "DATABASE_URL: sslmode '{other}' is not one of disable, allow, prefer, require, \
                             verify-ca, verify-full"
                        )));
                    }
                });
            }
            "sslrootcert" => root_cert = (value != "system" && !value.is_empty()).then(|| PathBuf::from(value)),
            "sslcert" | "sslkey" | "sslpassword" => {
                return Err(StoreError(format!(
                    "DATABASE_URL: {key} (client certificates) is not supported; remove it"
                )));
            }
            k if PASSTHROUGH.contains(&k) => kept.push(raw),
            other => return Err(StoreError(format!("DATABASE_URL: unknown parameter '{other}'"))),
        }
    }
    if let Some(mode) = ssl_mode {
        kept.push(format!("sslmode={mode}"));
    }
    let rebuilt = if is_url {
        if kept.is_empty() { base } else { format!("{base}?{}", kept.join("&")) }
    } else {
        kept.join(" ")
    };
    let mut config: Config =
        rebuilt.parse().map_err(|e| StoreError(format!("DATABASE_URL: invalid connection string: {e}")))?;
    if config.get_ssl_mode() != SslMode::Require
        && let Some(remote) = remote_destination(&config)
    {
        return Err(StoreError(format!(
            "refusing a plaintext connection to {remote}; set sslmode=verify-full in DATABASE_URL"
        )));
    }
    if config.get_application_name().is_none() {
        config.application_name("conde");
    }
    if config.get_connect_timeout().is_none() {
        config.connect_timeout(Duration::from_secs(10));
    }
    config.keepalives_idle(Duration::from_secs(60));
    let mut options = config.get_options().unwrap_or_default().to_string();
    for (name, value) in SESSION_DEFAULTS {
        if !options.contains(name) {
            options = format!("{options} -c {name}={value}").trim_start().to_string();
        }
    }
    config.options(&options);
    Ok(PgUrl { config, root_cert })
}

/// A destination that is neither a unix socket nor loopback: every host, and
/// every hostaddr (the postgres crate dials a hostaddr in preference to its host).
fn remote_destination(config: &Config) -> Option<String> {
    let names = config.get_hosts().iter().filter_map(|h| match h {
        Host::Tcp(name) if !matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1") => Some(name.clone()),
        _ => None,
    });
    let addrs = config.get_hostaddrs().iter().filter(|a| !a.is_loopback()).map(ToString::to_string);
    names.chain(addrs).next()
}

/// The password as written and as decoded, for masking (URL userinfo or `password=`).
fn password_forms(url: &str) -> Vec<String> {
    let raw = if let Some(rest) = url.strip_prefix("postgres://").or_else(|| url.strip_prefix("postgresql://")) {
        let authority = rest.split(['/', '?']).next().unwrap_or("");
        authority.rsplit_once('@').and_then(|(userinfo, _)| userinfo.split_once(':')).map(|(_, p)| p.to_string())
    } else {
        keyword_pairs(url).ok().and_then(|pairs| pairs.into_iter().find(|(k, _, _)| k == "password").map(|(_, v, _)| v))
    };
    let mut forms: Vec<String> = raw.into_iter().filter(|p| !p.is_empty()).collect();
    if let Some(p) = forms.first().map(|p| percent_decode(p))
        && !forms.contains(&p)
    {
        forms.push(p);
    }
    forms
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |i: usize| bytes.get(i).and_then(|b| (*b as char).to_digit(16));
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// libpq key=value pairs as (key, decoded value, raw `key=value` text).
fn keyword_pairs(s: &str) -> StoreResult<Vec<(String, String, String)>> {
    let mut out = Vec::new();
    let mut chars = s.char_indices().peekable();
    loop {
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let Some(&(start, _)) = chars.peek() else { return Ok(out) };
        let mut key = String::new();
        while let Some((_, c)) = chars.next_if(|(_, c)| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        if chars.next_if(|(_, c)| *c == '=').is_none() {
            return Err(StoreError(format!("DATABASE_URL: expected '=' after '{key}'")));
        }
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        let quoted = chars.next_if(|(_, c)| *c == '\'').is_some();
        loop {
            match chars.next() {
                None if quoted => return Err(StoreError(format!("DATABASE_URL: unterminated quote in '{key}'"))),
                None => break,
                Some((_, '\\')) => value.extend(chars.next().map(|(_, c)| c)),
                Some((_, '\'')) if quoted => break,
                Some((_, c)) if c.is_whitespace() && !quoted => break,
                Some((_, c)) => value.push(c),
            }
        }
        let end = chars.peek().map_or(s.len(), |(i, _)| *i);
        out.push((key, value, s[start..end].trim_end().to_string()));
    }
}

/// rustls with the ring provider, trusting `root_cert` (a PEM bundle) or else the system bundle.
fn tls(config: &Config, root_cert: Option<&Path>) -> StoreResult<MakeRustlsConnect> {
    let mut roots = rustls::RootCertStore::empty();
    match root_cert {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| StoreError(format!("sslrootcert {}: cannot read it: {e}", path.display())))?;
            let certs = CertificateDer::pem_slice_iter(&pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| StoreError(format!("sslrootcert {}: not a PEM bundle: {e}", path.display())))?;
            roots.add_parsable_certificates(certs);
            if roots.is_empty() {
                return Err(StoreError(format!("sslrootcert {}: no certificates in it", path.display())));
            }
        }
        None => {
            roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        }
    }
    if roots.is_empty() && config.get_ssl_mode() == SslMode::Require {
        return Err(StoreError("no CA certificates found in the system bundle (install ca-certificates)".into()));
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| StoreError(format!("tls: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(MakeRustlsConnect::new(config))
}

fn connect(config: &Config, tls: &MakeRustlsConnect) -> StoreResult<Client> {
    Ok(config.connect(tls.clone())?)
}

// --- connection pool ----------------------------------------------------------

/// A pooled connection with its prepared statements.
pub struct Conn {
    client: Client,
    statements: HashMap<String, Statement>,
    broken: bool,
}

impl Conn {
    fn statement(&mut self, sql: &str) -> Result<Statement, postgres::Error> {
        if let Some(s) = self.statements.get(sql) {
            return Ok(s.clone());
        }
        let s = self.client.prepare(sql)?;
        self.statements.insert(sql.to_string(), s.clone());
        Ok(s)
    }
}

struct Manager {
    config: Config,
    tls: MakeRustlsConnect,
}

impl r2d2::ManageConnection for Manager {
    type Connection = Conn;
    type Error = StoreError;

    fn connect(&self) -> StoreResult<Conn> {
        Ok(Conn { client: connect(&self.config, &self.tls)?, statements: HashMap::new(), broken: false })
    }

    fn is_valid(&self, conn: &mut Conn) -> StoreResult<()> {
        Ok(conn.client.simple_query("").map(|_| ())?)
    }

    fn has_broken(&self, conn: &mut Conn) -> bool {
        conn.broken || conn.client.is_closed()
    }
}

// --- store ------------------------------------------------------------------------

pub struct PgStore {
    /// None only while dropping (see Drop).
    pool: Option<r2d2::Pool<Manager>>,
    signal: Arc<PendingSignal>,
    stop: Arc<AtomicBool>,
}

/// How a process uses the database: the server migrates and listens; a dry
/// run touches nothing; an admin command migrates but needs no wake-ups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PgOpen {
    pub migrate: bool,
    pub listen: bool,
}

impl PgOpen {
    pub const SERVER: PgOpen = PgOpen { migrate: true, listen: true };
    pub const COMMAND: PgOpen = PgOpen { migrate: true, listen: false };
    pub const INSPECT: PgOpen = PgOpen { migrate: false, listen: false };
}

impl PgStore {
    /// Connects, migrates under the migration lock, and starts the
    /// LISTEN thread. Call from a blocking context.
    pub fn connect(url: &str) -> StoreResult<PgStore> {
        PgStore::open(url, PgOpen::SERVER)
    }

    pub fn open(url: &str, how: PgOpen) -> StoreResult<PgStore> {
        let PgUrl { config, root_cert } = parse_url(url)?;
        let tls = tls(&config, root_cert.as_deref())?;
        // One direct connection first: its error names the cause (DNS, TLS, auth), the pool's does not.
        let mut first = connect(&config, &tls)?;
        if how.migrate {
            migrate(&mut first)?;
        }
        drop(first);
        let pool = r2d2::Pool::builder()
            .max_size(POOL_SIZE)
            .min_idle(Some(1))
            .test_on_check_out(false)
            .connection_timeout(Duration::from_secs(15))
            .idle_timeout(Some(Duration::from_secs(300)))
            .max_lifetime(Some(Duration::from_secs(1800)))
            .build(Manager { config: config.clone(), tls: tls.clone() })?;
        let signal = Arc::new(PendingSignal::default());
        let stop = Arc::new(AtomicBool::new(false));
        if how.listen {
            let (s, st) = (signal.clone(), stop.clone());
            let (ready_tx, ready) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("conde-pg-listen".into())
                .spawn(move || listen(&config, &tls, &s, &st, ready_tx))
                .map_err(|e| StoreError(format!("starting the listener: {e}")))?;
            // A store handed out before LISTEN would miss the first commits' wake-ups.
            if ready.recv_timeout(LISTEN_READY_WAIT).is_err() {
                tracing::warn!("pending listener not ready after {LISTEN_READY_WAIT:?}; wake-ups start when it connects");
            }
        }
        Ok(PgStore { pool: Some(pool), signal, stop })
    }

    /// Whether the board's tables exist (a dry run against a fresh database sees none).
    pub fn has_schema(&self) -> StoreResult<bool> {
        let mut conn = self.pool().get()?;
        Ok(conn.client.query_one("SELECT to_regclass('messages') IS NOT NULL", &[])?.get(0))
    }
}

impl PgStore {
    fn pool(&self) -> &r2d2::Pool<Manager> {
        self.pool.as_ref().expect("pool present until drop")
    }
}

impl Drop for PgStore {
    /// A postgres client blocks on its own runtime when it closes, which panics
    /// on an async worker: the pool is closed on a thread of its own instead.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(pool) = self.pool.take() {
            let closed = std::thread::Builder::new().name("conde-pg-close".into()).spawn(move || drop(pool));
            if let Err(e) = closed {
                tracing::warn!("no thread to close the pool on ({e}); it was closed in place");
            }
        }
    }
}

impl Store for PgStore {
    fn begin(&self, mode: Mode) -> StoreResult<Box<dyn Tx + '_>> {
        let mut conn = self.pool().get()?;
        let begin = match mode {
            Mode::Read => "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY".to_string(),
            Mode::Write => format!("BEGIN; SELECT pg_advisory_xact_lock({WRITER_LOCK})"),
        };
        if let Err(e) = conn.client.batch_execute(&begin) {
            conn.broken = true;
            return Err(e.into());
        }
        Ok(Box::new(PgTx {
            conn: RefCell::new(conn),
            open: Cell::new(true),
            failed: Cell::new(false),
            wakes: RefCell::default(),
        }))
    }

    fn pending(&self) -> &PendingSignal {
        &self.signal
    }

    fn backend(&self) -> &'static str {
        "postgres"
    }

    fn ping(&self) -> bool {
        self.pool().get().is_ok_and(|mut c| c.client.simple_query("").is_ok())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// --- migrations ---------------------------------------------------------------

/// Applies the embedded migrations not yet in `schema_migrations`, holding
/// the migration lock so concurrent starters wait instead of racing. Returns
/// the versions this call applied.
pub fn migrate(client: &mut Client) -> StoreResult<Vec<i32>> {
    client.execute("SELECT pg_advisory_lock($1)", &[&MIGRATION_LOCK])?;
    let result = apply_migrations(client);
    let unlocked = client.execute("SELECT pg_advisory_unlock($1)", &[&MIGRATION_LOCK]);
    let applied = result?;
    unlocked?;
    Ok(applied)
}

fn apply_migrations(client: &mut Client) -> StoreResult<Vec<i32>> {
    client.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations(
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            checksum TEXT NOT NULL,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT now())",
    )?;
    let done: BTreeMap<i32, String> = client
        .query("SELECT version, checksum FROM schema_migrations", &[])?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let known = MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0);
    if let Some(newer) = done.keys().find(|v| **v > known) {
        tracing::warn!(version = newer, known, "database schema is newer than this binary; continuing");
    }
    let mut applied = Vec::new();
    for (version, name, sql) in MIGRATIONS {
        let checksum = sha256_hex(sql.as_bytes());
        match done.get(version) {
            Some(stored) if *stored == checksum => continue,
            Some(_) => {
                return Err(StoreError(format!("migration {version} ({name}) changed after it was applied")));
            }
            None => {}
        }
        let mut tx = client.transaction()?;
        tx.batch_execute(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations(version, name, checksum) VALUES ($1, $2, $3)",
            &[version, name, &checksum],
        )?;
        tx.commit()?;
        tracing::info!(version, name, "applied migration");
        applied.push(*version);
    }
    Ok(applied)
}

// --- LISTEN ---------------------------------------------------------------------

/// Runs until `stop`: one dedicated connection LISTENs on the pending
/// channel and raises a wake per notification. Reconnects with backoff and
/// raises `Wake::All` around every gap, since NOTIFYs sent then are lost.
fn listen(
    config: &Config,
    tls: &MakeRustlsConnect,
    signal: &PendingSignal,
    stop: &AtomicBool,
    ready: std::sync::mpsc::Sender<()>,
) {
    let mut backoff = Duration::from_millis(250);
    while !stop.load(Ordering::Relaxed) {
        let listening = connect(config, tls).and_then(|mut c| {
            c.batch_execute(&format!("LISTEN {PENDING_CHANNEL}; LISTEN {FENCE_CHANNEL}; LISTEN {REVOKE_CHANNEL}"))?;
            Ok(c)
        });
        match listening {
            Ok(mut client) => {
                backoff = Duration::from_millis(250);
                let _ = ready.send(());
                signal.raise(Wake::All);
                let reason = relay(&mut client, signal, stop);
                if let Some(reason) = reason {
                    tracing::warn!("pending listener lost its connection: {reason}");
                    signal.raise(Wake::All);
                }
            }
            Err(e) => tracing::warn!("pending listener cannot connect: {e}"),
        }
        let until = Instant::now() + backoff;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

/// Relays notifications until the connection fails (Some(reason)) or `stop` (None).
fn relay(client: &mut Client, signal: &PendingSignal, stop: &AtomicBool) -> Option<String> {
    let mut quiet_since = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let next = client.notifications().timeout_iter(Duration::from_millis(250)).next();
        match next {
            Ok(Some(n)) => {
                let payload = n.payload();
                signal.raise(match n.channel() {
                    FENCE_CHANNEL => Wake::Fenced(payload.to_string()),
                    REVOKE_CHANNEL => Wake::Revoked(payload.to_string()),
                    _ if payload.is_empty() => Wake::All,
                    _ => Wake::Host(payload.to_string()),
                });
                quiet_since = Instant::now();
            }
            Ok(None) if client.is_closed() => return Some("closed".into()),
            Ok(None) => {
                if quiet_since.elapsed() > Duration::from_secs(30) {
                    if let Err(e) = client.is_valid(Duration::from_secs(5)) {
                        return Some(e.to_string());
                    }
                    quiet_since = Instant::now();
                }
            }
            Err(e) => return Some(e.to_string()),
        }
    }
}

// --- transactions ---------------------------------------------------------------

pub struct PgTx {
    conn: RefCell<r2d2::PooledConnection<Manager>>,
    open: Cell<bool>,
    failed: Cell<bool>,
    wakes: RefCell<Wakes>,
}

impl Drop for PgTx {
    fn drop(&mut self) {
        if self.open.get() {
            let conn = self.conn.get_mut();
            if conn.client.batch_execute("ROLLBACK").is_err() {
                conn.broken = true;
            }
        }
    }
}

type Params<'a> = &'a [&'a (dyn ToSql + Sync)];

const AGENT_SELECT: &str = "SELECT a.agent_id, a.owner_id, u.name, a.name_lower, a.display, a.address_kind,
    a.address_target, a.host_id, a.binding_version, a.pinned_address
    FROM agents a JOIN users u ON u.user_id = a.owner_id";

fn agent_row(r: &Row) -> Agent {
    Agent {
        id: r.get(0),
        owner_id: r.get(1),
        owner: r.get(2),
        name: r.get(3),
        display: r.get(4),
        address: address_of(r.get(5), r.get(6), r.get(7)),
        binding_version: r.get(8),
        pinned: r.get(9),
    }
}

const MSG_SELECT: &str = "SELECT m.commit_seq, m.message_id, m.sender_agent_id, su.name || '/' || s.name_lower,
    m.recipient_agent_id, ru.name || '/' || r.name_lower, m.channel_id, c.name_lower, m.text, m.reply_to, m.created_at
    FROM messages m
    JOIN agents s ON s.agent_id = m.sender_agent_id JOIN users su ON su.user_id = s.owner_id
    LEFT JOIN agents r ON r.agent_id = m.recipient_agent_id LEFT JOIN users ru ON ru.user_id = r.owner_id
    LEFT JOIN channels c ON c.channel_id = m.channel_id";

fn msg_row(r: &Row) -> Message {
    Message {
        seq: r.get(0),
        id: r.get(1),
        sender_id: r.get(2),
        sender: r.get(3),
        recipient_id: r.get(4),
        recipient: r.get(5),
        channel_id: r.get(6),
        channel: r.get(7),
        text: r.get(8),
        reply_to: r.get(9),
        created_at: r.get(10),
    }
}

const USER_SELECT: &str = "SELECT u.user_id, u.name, u.disabled, i.user_id IS NOT NULL
    FROM users u LEFT JOIN identities i ON i.user_id = u.user_id";

fn user_row(r: &Row) -> UserRow {
    UserRow { user_id: r.get(0), name: r.get(1), disabled: r.get(2), bound: r.get(3) }
}

impl PgTx {
    /// A failed statement aborts a Postgres transaction, after which COMMIT
    /// silently rolls back; remember it so `commit` reports it instead.
    fn guard<T>(&self, r: Result<T, postgres::Error>) -> StoreResult<T> {
        r.map_err(|e| {
            self.failed.set(true);
            e.into()
        })
    }

    fn query(&self, sql: &str, params: Params) -> StoreResult<Vec<Row>> {
        let mut conn = self.conn.borrow_mut();
        let statement = self.guard(conn.statement(sql))?;
        self.guard(conn.client.query(&statement, params))
    }

    fn query_opt(&self, sql: &str, params: Params) -> StoreResult<Option<Row>> {
        let mut conn = self.conn.borrow_mut();
        let statement = self.guard(conn.statement(sql))?;
        self.guard(conn.client.query_opt(&statement, params))
    }

    fn query_one(&self, sql: &str, params: Params) -> StoreResult<Row> {
        let mut conn = self.conn.borrow_mut();
        let statement = self.guard(conn.statement(sql))?;
        self.guard(conn.client.query_one(&statement, params))
    }

    fn execute(&self, sql: &str, params: Params) -> StoreResult<u64> {
        let mut conn = self.conn.borrow_mut();
        let statement = self.guard(conn.statement(sql))?;
        self.guard(conn.client.execute(&statement, params))
    }

    fn msgs(&self, where_clause: &str, params: Params) -> StoreResult<Vec<Message>> {
        Ok(self.query(&format!("{MSG_SELECT} WHERE {where_clause}"), params)?.iter().map(msg_row).collect())
    }

    fn set<T: Eq + Hash>(&self, sql: &str, params: Params, f: impl FnMut(&Row) -> T) -> StoreResult<HashSet<T>> {
        Ok(self.query(sql, params)?.iter().map(f).collect())
    }

    fn pairs<A, B>(&self, sql: &str, params: Params) -> StoreResult<Vec<(A, B)>>
    where
        A: for<'r> postgres::types::FromSql<'r>,
        B: for<'r> postgres::types::FromSql<'r>,
    {
        Ok(self.query(sql, params)?.iter().map(|r| (r.get(0), r.get(1))).collect())
    }
}

impl Tx for PgTx {
    fn commit(self: Box<Self>) -> StoreResult<()> {
        if self.failed.get() {
            return Err(StoreError("transaction aborted by an earlier failed statement".into()));
        }
        let wakes = std::mem::take(&mut *self.wakes.borrow_mut());
        if !wakes.is_empty() {
            let notifications: Vec<String> = wakes.notifications.into_iter().collect();
            let hosts: Vec<String> = wakes.hosts.into_iter().collect();
            let fenced: Vec<String> = wakes.fenced.into_iter().collect();
            let revoked: Vec<String> = wakes.revoked.into_iter().collect();
            self.query(
                "SELECT pg_notify('conde_pending', h) FROM (
                    SELECT a.host_id AS h FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
                    WHERE n.status = 'pending' AND a.host_id IS NOT NULL AND n.notification_id = ANY($1)
                    UNION SELECT unnest($2::text[])) wake
                 UNION ALL SELECT pg_notify('conde_fence', i) FROM unnest($3::text[]) i
                 UNION ALL SELECT pg_notify('conde_revoke', r) FROM unnest($4::text[]) r",
                &[&notifications, &hosts, &fenced, &revoked],
            )?;
        }
        self.conn.borrow_mut().client.batch_execute("COMMIT")?;
        self.open.set(false);
        Ok(())
    }

    // principals and credentials

    fn ensure_user(&self, name: &str) -> StoreResult<i64> {
        self.execute("INSERT INTO users(name) VALUES ($1) ON CONFLICT DO NOTHING", &[&name])?;
        Ok(self.query_one("SELECT user_id FROM users WHERE name = $1", &[&name])?.get(0))
    }

    fn ensure_host(&self, host: &str, user_id: i64) -> StoreResult<i64> {
        self.execute("INSERT INTO hosts(host_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", &[&host, &user_id])?;
        Ok(self.query_one("SELECT user_id FROM hosts WHERE host_id = $1", &[&host])?.get(0))
    }

    fn user_id(&self, name: &str) -> StoreResult<Option<i64>> {
        Ok(self.query_opt("SELECT user_id FROM users WHERE name = $1", &[&name])?.map(|r| r.get(0)))
    }

    fn host_owner(&self, host: &str) -> StoreResult<Option<(i64, String)>> {
        Ok(self
            .query_opt(
                "SELECT u.user_id, u.name FROM hosts h JOIN users u ON u.user_id = h.user_id WHERE h.host_id = $1",
                &[&host],
            )?
            .map(|r| (r.get(0), r.get(1))))
    }

    fn insert_credential(
        &self,
        digest: &str,
        user_id: i64,
        host: &str,
        label: &str,
        created_at: &str,
    ) -> StoreResult<()> {
        self.execute(
            "INSERT INTO device_credentials(token_digest, user_id, host_id, label, created_at)
             VALUES ($1, $2, $3, $4, $5)",
            &[&digest, &user_id, &host, &label, &created_at],
        )?;
        Ok(())
    }

    fn credentials(&self) -> StoreResult<Vec<CredentialRow>> {
        Ok(self
            .query(
                "SELECT c.token_digest, u.name, c.host_id, c.label, c.created_at, c.revoked_at
                 FROM device_credentials c JOIN users u ON u.user_id = c.user_id
                 ORDER BY COALESCE(c.created_at, ''), c.host_id",
                &[],
            )?
            .iter()
            .map(|r| CredentialRow {
                digest: r.get(0),
                user: r.get(1),
                host: r.get(2),
                label: r.get(3),
                created_at: r.get(4),
                revoked_at: r.get(5),
            })
            .collect())
    }

    fn revoke_credential(&self, key_id: &str, at: &str) -> StoreResult<u64> {
        let hosts: Vec<String> = self
            .query(
                "UPDATE device_credentials SET revoked_at = $2
                 WHERE substr(token_digest, 1, length($1)) = $1 AND revoked_at IS NULL RETURNING host_id",
                &[&key_id, &at],
            )?
            .iter()
            .map(|r| r.get(0))
            .collect();
        let n = hosts.len() as u64;
        self.wakes.borrow_mut().revoked.extend(hosts);
        Ok(n)
    }

    fn revoke_host_credentials(&self, host: &str, at: &str) -> StoreResult<u64> {
        let n = self.execute(
            "UPDATE device_credentials SET revoked_at = $2 WHERE host_id = $1 AND revoked_at IS NULL",
            &[&host, &at],
        )?;
        if n > 0 {
            self.wakes.borrow_mut().revoked.insert(host.to_string());
        }
        Ok(n)
    }

    fn host_last_seen(&self, host: &str) -> StoreResult<Option<String>> {
        Ok(self.query_one("SELECT MAX(updated_at) FROM bridge_instances WHERE host_id = $1", &[&host])?.get(0))
    }

    fn authenticate(&self, digest: &str) -> StoreResult<Option<(i64, String)>> {
        Ok(self
            .query_opt(
                "SELECT c.user_id, c.host_id FROM device_credentials c JOIN users u ON u.user_id = c.user_id
                 WHERE c.token_digest = $1 AND c.revoked_at IS NULL AND NOT u.disabled",
                &[&digest],
            )?
            .map(|r| (r.get(0), r.get(1))))
    }

    // users, Entra identities and browser sign-in

    fn user_by_identity(&self, tid: &str, oid: &str) -> StoreResult<Option<UserRow>> {
        Ok(self.query_opt(&format!("{USER_SELECT} WHERE i.tid = $1 AND i.oid = $2"), &[&tid, &oid])?.map(|r| user_row(&r)))
    }

    fn user_by_name(&self, name: &str) -> StoreResult<Option<UserRow>> {
        Ok(self.query_opt(&format!("{USER_SELECT} WHERE u.name = $1"), &[&name])?.map(|r| user_row(&r)))
    }

    fn user_by_id(&self, user_id: i64) -> StoreResult<Option<UserRow>> {
        Ok(self.query_opt(&format!("{USER_SELECT} WHERE u.user_id = $1"), &[&user_id])?.map(|r| user_row(&r)))
    }

    fn bind_identity(
        &self,
        user_id: i64,
        tid: &str,
        oid: &str,
        display: Option<&str>,
        upn: Option<&str>,
    ) -> StoreResult<()> {
        self.execute(
            "INSERT INTO identities(tid, oid, user_id, display_name, preferred_username) VALUES ($1, $2, $3, $4, $5)",
            &[&tid, &oid, &user_id, &display, &upn],
        )?;
        Ok(())
    }

    fn set_identity_profile(&self, user_id: i64, display: Option<&str>, upn: Option<&str>) -> StoreResult<()> {
        self.execute(
            "UPDATE identities SET display_name = $2, preferred_username = $3 WHERE user_id = $1",
            &[&user_id, &display, &upn],
        )?;
        Ok(())
    }

    fn set_user_disabled(&self, user_id: i64, disabled: bool) -> StoreResult<Vec<String>> {
        self.execute("UPDATE users SET disabled = $2 WHERE user_id = $1", &[&user_id, &disabled])?;
        if disabled {
            self.execute("DELETE FROM web_sessions WHERE user_id = $1", &[&user_id])?;
        }
        let hosts: Vec<String> = self
            .query("SELECT host_id FROM hosts WHERE user_id = $1 ORDER BY host_id", &[&user_id])?
            .iter()
            .map(|r| r.get(0))
            .collect();
        if disabled {
            self.wakes.borrow_mut().revoked.extend(hosts.iter().cloned());
        }
        Ok(hosts)
    }

    fn insert_web_session(&self, id_digest: &str, w: &WebSessionRow) -> StoreResult<()> {
        self.execute(
            "INSERT INTO web_sessions(id_digest, user_id, display, csrf, created_at, last_seen)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[&id_digest, &w.user_id, &w.display, &w.csrf, &w.created_at, &w.last_seen],
        )?;
        Ok(())
    }

    fn web_session(&self, id_digest: &str) -> StoreResult<Option<WebSessionRow>> {
        Ok(self
            .query_opt(
                "SELECT s.user_id, u.name, s.display, s.csrf, s.created_at, s.last_seen
                 FROM web_sessions s JOIN users u ON u.user_id = s.user_id WHERE s.id_digest = $1",
                &[&id_digest],
            )?
            .map(|r| WebSessionRow {
                user_id: r.get(0),
                user_name: r.get(1),
                display: r.get(2),
                csrf: r.get(3),
                created_at: r.get(4),
                last_seen: r.get(5),
            }))
    }

    fn touch_web_session(&self, id_digest: &str, at: i64) -> StoreResult<()> {
        self.execute("UPDATE web_sessions SET last_seen = $2 WHERE id_digest = $1", &[&id_digest, &at])?;
        Ok(())
    }

    fn delete_web_session(&self, id_digest: &str) -> StoreResult<()> {
        self.execute("DELETE FROM web_sessions WHERE id_digest = $1", &[&id_digest])?;
        Ok(())
    }

    fn put_web_token(&self, kind: &str, digest: &str, payload: &str, created_at: i64) -> StoreResult<()> {
        self.execute(
            "INSERT INTO web_tokens(kind, digest, payload, created_at) VALUES ($1, $2, $3, $4)",
            &[&kind, &digest, &payload, &created_at],
        )?;
        Ok(())
    }

    fn take_web_token(&self, kind: &str, digest: &str) -> StoreResult<Option<(String, i64)>> {
        Ok(self
            .query_opt(
                "DELETE FROM web_tokens WHERE kind = $1 AND digest = $2 RETURNING payload, created_at",
                &[&kind, &digest],
            )?
            .map(|r| (r.get(0), r.get(1))))
    }

    fn purge_web(&self, now: i64, session_idle: i64, session_max: i64, ttls: &[(&str, i64)]) -> StoreResult<()> {
        self.execute(
            "DELETE FROM web_sessions WHERE last_seen < $1::BIGINT - $2::BIGINT OR created_at < $1::BIGINT - $3::BIGINT",
            &[&now, &session_idle, &session_max],
        )?;
        for (kind, ttl) in ttls {
            self.execute(
                "DELETE FROM web_tokens WHERE kind = $1 AND created_at < $2::BIGINT - $3::BIGINT",
                &[kind, &now, ttl],
            )?;
        }
        Ok(())
    }

    // idempotency

    fn idempotency_get(&self, actor_id: &str, op: &str, key: &str) -> StoreResult<Option<(String, String)>> {
        Ok(self
            .query_opt(
                "SELECT request_hash, result_json FROM idempotency WHERE actor_id = $1 AND op = $2 AND key = $3",
                &[&actor_id, &op, &key],
            )?
            .map(|r| (r.get(0), r.get(1))))
    }

    fn idempotency_put(
        &self,
        actor_id: &str,
        op: &str,
        key: &str,
        request_hash: &str,
        result_json: &str,
        created_at: &str,
    ) -> StoreResult<()> {
        self.execute(
            "INSERT INTO idempotency(actor_id, op, key, request_hash, result_json, created_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[&actor_id, &op, &key, &request_hash, &result_json, &created_at],
        )?;
        Ok(())
    }

    // agents

    fn agent_by_id(&self, id: &str) -> StoreResult<Option<Agent>> {
        Ok(self.query_opt(&format!("{AGENT_SELECT} WHERE a.agent_id = $1"), &[&id])?.as_ref().map(agent_row))
    }

    fn agent_by_name(&self, owner_id: i64, name: &str) -> StoreResult<Option<Agent>> {
        Ok(self
            .query_opt(&format!("{AGENT_SELECT} WHERE a.owner_id = $1 AND a.name_lower = $2"), &[&owner_id, &name])?
            .as_ref()
            .map(agent_row))
    }

    fn agents_at_address(&self, owner_id: i64, address: &Address) -> StoreResult<Vec<Agent>> {
        Ok(self
            .query(
                &format!(
                    "{AGENT_SELECT} WHERE a.owner_id = $1 AND a.address_kind = $2 AND a.address_target = $3
                     AND a.host_id = $4 ORDER BY a.name_lower"
                ),
                &[&owner_id, &address.kind.as_str(), &address.target, &address.host],
            )?
            .iter()
            .map(agent_row)
            .collect())
    }

    fn bound_agents(&self) -> StoreResult<Vec<Agent>> {
        Ok(self
            .query(&format!("{AGENT_SELECT} WHERE a.address_kind IS NOT NULL ORDER BY u.name, a.name_lower"), &[])?
            .iter()
            .map(agent_row)
            .collect())
    }

    fn insert_agent(&self, id: &str, owner_id: i64, name: &str) -> StoreResult<()> {
        self.execute(
            "INSERT INTO agents(agent_id, owner_id, name_lower) VALUES ($1, $2, $3)",
            &[&id, &owner_id, &name],
        )?;
        Ok(())
    }

    fn set_binding(&self, agent_id: &str, address: Option<&Address>) -> StoreResult<()> {
        self.execute(
            "UPDATE agents SET address_kind = $2, address_target = $3, host_id = $4,
             binding_version = binding_version + 1 WHERE agent_id = $1",
            &[
                &agent_id,
                &address.map(|a| a.kind.as_str()),
                &address.map(|a| a.target.as_str()),
                &address.map(|a| a.host.as_str()),
            ],
        )?;
        if let Some(a) = address {
            self.wakes.borrow_mut().hosts.insert(a.host.clone());
        }
        Ok(())
    }

    fn set_pin(&self, agent_id: &str, pinned: Option<&str>) -> StoreResult<()> {
        self.execute("UPDATE agents SET pinned_address = $2 WHERE agent_id = $1", &[&agent_id, &pinned])?;
        Ok(())
    }

    fn set_display(&self, agent_id: &str, display: Option<&str>) -> StoreResult<()> {
        self.execute("UPDATE agents SET display = $2 WHERE agent_id = $1", &[&agent_id, &display])?;
        Ok(())
    }

    // channels

    fn channel_by_name(&self, name: &str) -> StoreResult<Option<Channel>> {
        Ok(self
            .query_opt("SELECT channel_id, name_lower, created_by, topic FROM channels WHERE name_lower = $1", &[&name])?
            .map(|r| Channel { id: r.get(0), name: r.get(1), created_by: r.get(2), topic: r.get(3) }))
    }

    fn channel_id_by_name(&self, name: &str) -> StoreResult<Option<String>> {
        Ok(self.query_opt("SELECT channel_id FROM channels WHERE name_lower = $1", &[&name])?.map(|r| r.get(0)))
    }

    fn insert_channel(&self, channel: &Channel) -> StoreResult<()> {
        self.execute(
            "INSERT INTO channels(channel_id, name_lower, created_by, topic) VALUES ($1, $2, $3, $4)",
            &[&channel.id, &channel.name, &channel.created_by, &channel.topic],
        )?;
        Ok(())
    }

    fn set_channel_topic(&self, channel_id: &str, topic: Option<&str>) -> StoreResult<()> {
        self.execute("UPDATE channels SET topic = $2 WHERE channel_id = $1", &[&channel_id, &topic])?;
        Ok(())
    }

    fn channel_member_ids(&self, channel_id: &str) -> StoreResult<Vec<String>> {
        Ok(self
            .query("SELECT agent_id FROM memberships WHERE channel_id = $1 ORDER BY agent_id", &[&channel_id])?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }

    fn read_seq(&self, channel_id: &str, agent_id: &str) -> StoreResult<Option<i64>> {
        Ok(self
            .query_opt(
                "SELECT read_seq FROM memberships WHERE channel_id = $1 AND agent_id = $2",
                &[&channel_id, &agent_id],
            )?
            .map(|r| r.get(0)))
    }

    fn advance_cursor(&self, channel_id: &str, agent_id: &str, seq: i64) -> StoreResult<()> {
        self.execute(
            "UPDATE memberships SET read_seq = GREATEST(read_seq, $3) WHERE channel_id = $1 AND agent_id = $2",
            &[&channel_id, &agent_id, &seq],
        )?;
        Ok(())
    }

    fn channel_last_seq(&self, channel_id: &str) -> StoreResult<i64> {
        Ok(self
            .query_one("SELECT COALESCE(MAX(commit_seq), 0) FROM messages WHERE channel_id = $1", &[&channel_id])?
            .get(0))
    }

    fn insert_membership(&self, channel_id: &str, agent_id: &str, read_seq: i64) -> StoreResult<()> {
        self.execute(
            "INSERT INTO memberships(channel_id, agent_id, read_seq) VALUES ($1, $2, $3)",
            &[&channel_id, &agent_id, &read_seq],
        )?;
        Ok(())
    }

    fn delete_membership(&self, channel_id: &str, agent_id: &str) -> StoreResult<()> {
        self.execute("DELETE FROM memberships WHERE channel_id = $1 AND agent_id = $2", &[&channel_id, &agent_id])?;
        Ok(())
    }

    fn channel_summaries(&self) -> StoreResult<Vec<ChannelSummary>> {
        Ok(self
            .query(
                "SELECT c.name_lower, c.topic,
                    (SELECT COUNT(*) FROM memberships x WHERE x.channel_id = c.channel_id),
                    (SELECT COUNT(*) FROM messages m WHERE m.channel_id = c.channel_id)
                 FROM channels c ORDER BY c.name_lower",
                &[],
            )?
            .iter()
            .map(|r| ChannelSummary { name: r.get(0), topic: r.get(1), members: r.get(2), messages: r.get(3) })
            .collect())
    }

    fn channel_member_status(&self, channel_id: &str) -> StoreResult<Vec<(String, i64)>> {
        self.pairs(
            "SELECT u.name || '/' || a.name_lower,
                (SELECT COUNT(*) FROM messages m WHERE m.channel_id = x.channel_id AND m.commit_seq > x.read_seq)
             FROM memberships x JOIN agents a ON a.agent_id = x.agent_id JOIN users u ON u.user_id = a.owner_id
             WHERE x.channel_id = $1 ORDER BY 1",
            &[&channel_id],
        )
    }

    fn channel_message_ids(&self, channel_id: &str) -> StoreResult<Vec<String>> {
        Ok(self
            .query("SELECT message_id FROM messages WHERE channel_id = $1 ORDER BY commit_seq", &[&channel_id])?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }

    fn channel_seq_at(&self, channel_id: &str, message_id: &str) -> StoreResult<i64> {
        Ok(self
            .query_one(
                "SELECT COALESCE(MAX(commit_seq), 0) FROM messages WHERE channel_id = $1 AND message_id <= $2",
                &[&channel_id, &message_id],
            )?
            .get(0))
    }

    // messages

    fn insert_message_if_absent(&self, m: &NewMessage) -> StoreResult<bool> {
        Ok(self.execute(
            "INSERT INTO messages(message_id, sender_agent_id, recipient_agent_id, channel_id, text, reply_to,
             created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (message_id) DO NOTHING",
            &[&m.id, &m.sender_id, &m.recipient_id, &m.channel_id, &m.text, &m.reply_to, &m.created_at],
        )? > 0)
    }

    fn attachment_sizes(&self, message_id: &str, filename: &str) -> StoreResult<Option<(i64, i64)>> {
        Ok(self
            .query_opt(
                "SELECT size, octet_length(bytes)::bigint FROM attachments WHERE message_id = $1 AND filename = $2
                 ORDER BY attachment_id LIMIT 1",
                &[&message_id, &filename],
            )?
            .map(|r| (r.get(0), r.get(1))))
    }

    fn insert_reaction_if_absent(
        &self,
        message_id: &str,
        actor_id: &str,
        emoji: &str,
        present: bool,
    ) -> StoreResult<bool> {
        Ok(self.execute(
            "INSERT INTO reactions(message_id, actor_id, emoji, present) VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
            &[&message_id, &actor_id, &emoji, &present],
        )? > 0)
    }

    fn insert_message(&self, m: &NewMessage) -> StoreResult<i64> {
        Ok(self
            .query_one(
                "INSERT INTO messages(message_id, sender_agent_id, recipient_agent_id, channel_id, text, reply_to,
                 created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING commit_seq",
                &[&m.id, &m.sender_id, &m.recipient_id, &m.channel_id, &m.text, &m.reply_to, &m.created_at],
            )?
            .get(0))
    }

    fn insert_attachment(&self, id: &str, message_id: &str, filename: &str, bytes: &[u8]) -> StoreResult<()> {
        self.execute(
            "INSERT INTO attachments(attachment_id, message_id, filename, bytes, size) VALUES ($1, $2, $3, $4, $5)",
            &[&id, &message_id, &filename, &bytes, &(bytes.len() as i64)],
        )?;
        Ok(())
    }

    fn message_by_id(&self, id: &str) -> StoreResult<Option<Message>> {
        Ok(self.query_opt(&format!("{MSG_SELECT} WHERE m.message_id = $1"), &[&id])?.as_ref().map(msg_row))
    }

    fn inbox(&self, agent_id: &str, after: i64, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.commit_seq > $2 AND (
                (m.recipient_agent_id = $1 AND NOT EXISTS
                    (SELECT 1 FROM dm_read d WHERE d.message_id = m.message_id AND d.agent_id = $1))
                OR (m.sender_agent_id <> $1 AND EXISTS (SELECT 1 FROM memberships x
                    WHERE x.channel_id = m.channel_id AND x.agent_id = $1 AND m.commit_seq > x.read_seq)))
             ORDER BY m.commit_seq LIMIT $3",
            &[&agent_id, &after, &limit],
        )
    }

    fn unread_in_channel(&self, channel_id: &str, agent_id: &str, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.channel_id = $1 AND m.commit_seq > (SELECT read_seq FROM memberships
                WHERE channel_id = $1 AND agent_id = $2) ORDER BY m.commit_seq LIMIT $3",
            &[&channel_id, &agent_id, &limit],
        )
    }

    fn unread_all(&self, agent_id: &str, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "(m.recipient_agent_id = $1 AND NOT EXISTS
                (SELECT 1 FROM dm_read d WHERE d.message_id = m.message_id AND d.agent_id = $1))
             OR EXISTS (SELECT 1 FROM memberships x
                WHERE x.channel_id = m.channel_id AND x.agent_id = $1 AND m.commit_seq > x.read_seq)
             ORDER BY m.commit_seq LIMIT $2",
            &[&agent_id, &limit],
        )
    }

    fn sent_by(&self, agent_id: &str, after: i64, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.sender_agent_id = $1 AND m.commit_seq > $2 ORDER BY m.commit_seq LIMIT $3",
            &[&agent_id, &after, &limit],
        )
    }

    fn search_scan(&self, agent_id: &str, after: i64, scope: SearchScope, limit: i64) -> StoreResult<Vec<Message>> {
        let mine = "(m.recipient_agent_id = $1 OR (m.sender_agent_id = $1 AND m.recipient_agent_id IS NOT NULL)
            OR EXISTS (SELECT 1 FROM memberships x WHERE x.channel_id = m.channel_id AND x.agent_id = $1))";
        let (filter, arg) = match scope {
            SearchScope::All => ("$4::text IS NULL", None),
            SearchScope::Channel(name) => ("c.name_lower = $4", Some(name)),
            SearchScope::Partner(id) => {
                ("m.channel_id IS NULL AND (m.sender_agent_id = $4 OR m.recipient_agent_id = $4)", Some(id))
            }
        };
        self.msgs(
            &format!("m.commit_seq > $2 AND {mine} AND {filter} ORDER BY m.commit_seq LIMIT $3"),
            &[&agent_id, &after, &limit, &arg],
        )
    }

    fn attachment_list(&self, message_id: &str) -> StoreResult<Vec<AttachmentMeta>> {
        Ok(self
            .query(
                "SELECT attachment_id, filename, size FROM attachments WHERE message_id = $1 ORDER BY attachment_id",
                &[&message_id],
            )?
            .iter()
            .map(|r| AttachmentMeta { id: r.get(0), filename: r.get(1), size: r.get(2) })
            .collect())
    }

    fn attachment(&self, id: &str) -> StoreResult<Option<Attachment>> {
        Ok(self
            .query_opt("SELECT message_id, filename, bytes FROM attachments WHERE attachment_id = $1", &[&id])?
            .map(|r| Attachment { message_id: r.get(0), filename: r.get(1), bytes: r.get(2) }))
    }

    fn reactions(&self, message_id: &str) -> StoreResult<Vec<(String, String)>> {
        self.pairs(
            "SELECT x.emoji, u.name || '/' || a.name_lower FROM reactions x
             JOIN agents a ON a.agent_id = x.actor_id JOIN users u ON u.user_id = a.owner_id
             WHERE x.message_id = $1 AND x.present ORDER BY x.emoji, 2",
            &[&message_id],
        )
    }

    fn set_reaction(&self, message_id: &str, actor_id: &str, emoji: &str, present: bool) -> StoreResult<()> {
        self.execute(
            "INSERT INTO reactions(message_id, actor_id, emoji, present) VALUES ($1, $2, $3, $4)
             ON CONFLICT (message_id, actor_id, emoji) DO UPDATE SET present = excluded.present",
            &[&message_id, &actor_id, &emoji, &present],
        )?;
        Ok(())
    }

    fn mark_dm_read(&self, message_id: &str, agent_id: &str, at: &str) -> StoreResult<bool> {
        Ok(self.execute(
            "INSERT INTO dm_read(message_id, agent_id, read_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            &[&message_id, &agent_id, &at],
        )? > 0)
    }

    fn dm_read_at(&self, message_id: &str, agent_id: &str) -> StoreResult<Option<String>> {
        Ok(self
            .query_opt(
                "SELECT read_at FROM dm_read WHERE message_id = $1 AND agent_id = $2",
                &[&message_id, &agent_id],
            )?
            .map(|r| r.get(0)))
    }

    fn channel_read_count(&self, channel_id: &str, except_agent: &str, seq: i64) -> StoreResult<(i64, i64)> {
        let r = self.query_one(
            "SELECT COUNT(*) FILTER (WHERE read_seq >= $3), COUNT(*) FROM memberships
             WHERE channel_id = $1 AND agent_id <> $2",
            &[&channel_id, &except_agent, &seq],
        )?;
        Ok((r.get(0), r.get(1)))
    }

    fn message_count(&self) -> StoreResult<i64> {
        Ok(self.query_one("SELECT COUNT(*) FROM messages", &[])?.get(0))
    }

    // notifications and attempts

    fn insert_notification(&self, n: &NewNotification) -> StoreResult<()> {
        self.execute(
            "INSERT INTO notifications(notification_id, message_id, recipient_agent_id, kind, binding_version,
             status, next_attempt_at) VALUES ($1, $2, $3, $4, $5, 'pending', $6) ON CONFLICT DO NOTHING",
            &[&n.id, &n.message_id, &n.recipient_id, &n.kind, &n.binding_version, &n.next_attempt_at],
        )?;
        self.wakes.borrow_mut().notifications.insert(n.id.to_string());
        Ok(())
    }

    fn due_notifications(&self, now: i64, host: &str, limit: i64) -> StoreResult<Vec<DueNotification>> {
        Ok(self
            .query(
                "SELECT n.notification_id, n.message_id, n.recipient_agent_id, n.kind
                 FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
                 WHERE n.status = 'pending' AND n.next_attempt_at <= $1 AND a.host_id = $2
                 ORDER BY n.next_attempt_at, n.notification_seq LIMIT $3",
                &[&now, &host, &limit],
            )?
            .iter()
            .map(|r| DueNotification { id: r.get(0), message_id: r.get(1), recipient_id: r.get(2), kind: r.get(3) })
            .collect())
    }

    fn set_notification_status(&self, id: &str, status: &str) -> StoreResult<()> {
        self.settle_notification(id, status, None)
    }

    fn settle_notification(&self, id: &str, status: &str, next_attempt_at: Option<i64>) -> StoreResult<()> {
        self.execute(
            "UPDATE notifications SET status = $2, next_attempt_at = COALESCE($3, next_attempt_at)
             WHERE notification_id = $1",
            &[&id, &status, &next_attempt_at],
        )?;
        if status == "pending" {
            self.wakes.borrow_mut().notifications.insert(id.to_string());
        }
        Ok(())
    }

    fn unread_badges(&self, agent_id: &str) -> StoreResult<Vec<(String, i64)>> {
        self.pairs(
            "SELECT c.name_lower, COUNT(m.commit_seq) FROM memberships x
             JOIN channels c ON c.channel_id = x.channel_id
             JOIN messages m ON m.channel_id = x.channel_id AND m.commit_seq > x.read_seq
                AND m.sender_agent_id <> x.agent_id
             WHERE x.agent_id = $1 GROUP BY c.name_lower ORDER BY c.name_lower",
            &[&agent_id],
        )
    }

    fn insert_attempt(
        &self,
        id: &str,
        notification_id: &str,
        bridge_instance: &str,
        target_binding_version: i64,
        host: &str,
        text: &str,
    ) -> StoreResult<()> {
        self.execute(
            "INSERT INTO attempts(attempt_id, notification_id, bridge_instance, target_binding_version, status,
                                  host_id, text)
             VALUES ($1, $2, $3, $4, 'dispatching', $5, $6)",
            &[&id, &notification_id, &bridge_instance, &target_binding_version, &host, &text],
        )?;
        Ok(())
    }

    fn held_attempts(&self, bridge_instance: &str) -> StoreResult<Vec<(String, String)>> {
        self.pairs(
            "SELECT t.attempt_id, a.address_kind || ':' || a.address_target || '@' || a.host_id
             FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id
             WHERE t.status = 'held' AND t.bridge_instance = $1 AND a.address_kind IS NOT NULL
             ORDER BY t.attempt_seq",
            &[&bridge_instance],
        )
    }

    fn stale_attempts(&self, bridge_instance: &str, host: &str) -> StoreResult<Vec<(String, String)>> {
        self.pairs(
            "SELECT t.attempt_id, CASE WHEN t.bridge_instance <> $1 THEN 'bridge instance gone'
                                       ELSE 'recipient rebound' END
             FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id
             WHERE (t.status IN ('held', 'dispatching') OR (t.status = 'accepted' AND n.status = 'notified'))
               AND (a.host_id = $2 OR a.host_id IS NULL)
               AND (t.bridge_instance <> $1 OR t.target_binding_version <> a.binding_version)
             ORDER BY t.attempt_seq",
            &[&bridge_instance, &host],
        )
    }

    fn attempt(&self, id: &str) -> StoreResult<Option<(String, String)>> {
        Ok(self
            .query_opt("SELECT notification_id, status FROM attempts WHERE attempt_id = $1", &[&id])?
            .map(|r| (r.get(0), r.get(1))))
    }

    fn attempt_owner(&self, id: &str) -> StoreResult<Option<(String, Option<String>, i64)>> {
        Ok(self
            .query_opt("SELECT bridge_instance, host_id, last_event_seq FROM attempts WHERE attempt_id = $1", &[&id])?
            .map(|r| (r.get(0), r.get(1), r.get(2))))
    }

    fn update_attempt(&self, id: &str, status: &str, reason: Option<&str>, event_seq: Option<i64>) -> StoreResult<()> {
        self.execute(
            "UPDATE attempts SET status = $2, reason = $3, last_event_seq = COALESCE($4, last_event_seq)
             WHERE attempt_id = $1",
            &[&id, &status, &reason, &event_seq],
        )?;
        Ok(())
    }

    fn latest_attempt(&self, notification_id: &str) -> StoreResult<Option<String>> {
        Ok(self
            .query_opt(
                "SELECT attempt_id FROM attempts WHERE notification_id = $1 ORDER BY attempt_seq DESC LIMIT 1",
                &[&notification_id],
            )?
            .map(|r| r.get(0)))
    }

    fn notifications(&self) -> StoreResult<Vec<NotificationRow>> {
        Ok(self
            .query(
                "SELECT n.notification_id, n.message_id, n.recipient_agent_id, u.name || '/' || a.name_lower, n.kind,
                    n.status, n.next_attempt_at
                 FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
                 JOIN users u ON u.user_id = a.owner_id ORDER BY n.notification_id",
                &[],
            )?
            .iter()
            .map(|r| NotificationRow {
                id: r.get(0),
                message_id: r.get(1),
                recipient_id: r.get(2),
                recipient_name: r.get(3),
                kind: r.get(4),
                status: r.get(5),
                next_attempt_at: r.get(6),
            })
            .collect())
    }

    fn attempts(&self) -> StoreResult<Vec<AttemptRow>> {
        Ok(self
            .query(
                "SELECT attempt_id, notification_id, bridge_instance, target_binding_version, last_event_seq, status,
                    reason FROM attempts ORDER BY attempt_seq",
                &[],
            )?
            .iter()
            .map(|r| AttemptRow {
                id: r.get(0),
                notification_id: r.get(1),
                bridge_instance: r.get(2),
                target_binding_version: r.get(3),
                last_event_seq: r.get(4),
                status: r.get(5),
                reason: r.get(6),
            })
            .collect())
    }

    fn open_attempts(&self, bridge_instance: &str) -> StoreResult<Vec<OpenAttempt>> {
        Ok(self
            .query(
                "SELECT t.attempt_id, t.notification_id,
                        a.address_kind || ':' || a.address_target || '@' || a.host_id, t.text,
                        u.name || '/' || a.name_lower, n.message_id, t.target_binding_version
                 FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
                 JOIN agents a ON a.agent_id = n.recipient_agent_id JOIN users u ON u.user_id = a.owner_id
                 WHERE t.bridge_instance = $1 AND t.status IN ('dispatching', 'held')
                   AND t.text IS NOT NULL AND a.address_kind IS NOT NULL
                 ORDER BY t.attempt_seq",
                &[&bridge_instance],
            )?
            .iter()
            .map(|r| OpenAttempt {
                attempt_id: r.get(0),
                notification_id: r.get(1),
                address: r.get(2),
                text: r.get(3),
                recipient: r.get(4),
                message_id: r.get(5),
                binding_version: r.get(6),
            })
            .collect())
    }

    fn gone_claude_holds(&self, bridge_instance: &str, host: &str) -> StoreResult<Vec<String>> {
        Ok(self
            .query(
                "SELECT t.attempt_id FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
                 JOIN agents a ON a.agent_id = n.recipient_agent_id
                 WHERE t.bridge_instance = $1 AND t.status = 'held' AND a.address_kind = 'claude' AND a.host_id = $2
                   AND NOT EXISTS (SELECT 1 FROM endpoints e
                                   WHERE e.host_id = a.host_id AND e.kind = 'claude' AND e.target = a.address_target)
                 ORDER BY t.attempt_seq",
                &[&bridge_instance, &host],
            )?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }

    fn dm_delivery(&self, message_id: &str, recipient: &str) -> StoreResult<Option<DmDelivery>> {
        Ok(self
            .query_opt(
                "SELECT n.status, t.status, t.reason FROM notifications n
                 LEFT JOIN attempts t ON t.attempt_seq =
                     (SELECT MAX(attempt_seq) FROM attempts WHERE notification_id = n.notification_id)
                 WHERE n.message_id = $1 AND n.recipient_agent_id = $2 ORDER BY n.notification_id LIMIT 1",
                &[&message_id, &recipient],
            )?
            .map(|r| DmDelivery { status: r.get(0), last_attempt: r.get(1), reason: r.get(2) }))
    }

    // bridge instances and endpoints

    fn bridge_instance(&self, instance: &str) -> StoreResult<Option<(String, String)>> {
        Ok(self
            .query_opt("SELECT host_id, state FROM bridge_instances WHERE instance = $1", &[&instance])?
            .map(|r| (r.get(0), r.get(1))))
    }

    fn active_bridges(&self, host: &str) -> StoreResult<Vec<String>> {
        Ok(self
            .query(
                "SELECT instance FROM bridge_instances WHERE host_id = $1 AND state = 'active' ORDER BY instance",
                &[&host],
            )?
            .iter()
            .map(|r| r.get(0))
            .collect())
    }

    fn fence(&self, host: &str, instance: &str, at: &str) -> StoreResult<Vec<String>> {
        let fenced: Vec<String> = self
            .query(
                "UPDATE bridge_instances SET state = 'fenced', updated_at = $3
                 WHERE host_id = $1 AND state = 'active' AND instance <> $2 RETURNING instance",
                &[&host, &instance, &at],
            )?
            .iter()
            .map(|r| r.get(0))
            .collect();
        self.execute(
            "INSERT INTO bridge_instances(instance, host_id, state, updated_at) VALUES ($1, $2, 'active', $3)
             ON CONFLICT (instance) DO UPDATE SET state = 'active', updated_at = $3",
            &[&instance, &host, &at],
        )?;
        let mut fenced = fenced;
        fenced.sort();
        self.wakes.borrow_mut().fenced.extend(fenced.iter().cloned());
        Ok(fenced)
    }

    fn replace_endpoints(&self, host: &str, instance: &str, endpoints: &[(&str, &str)], at: &str) -> StoreResult<()> {
        self.execute("DELETE FROM endpoints WHERE host_id = $1", &[&host])?;
        for (kind, target) in endpoints {
            self.execute(
                "INSERT INTO endpoints(host_id, kind, target, bridge_instance, seen_at) VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (host_id, kind, target)
                 DO UPDATE SET bridge_instance = excluded.bridge_instance, seen_at = excluded.seen_at",
                &[&host, kind, target, &instance, &at],
            )?;
        }
        Ok(())
    }

    fn count_endpoints(&self) -> StoreResult<i64> {
        Ok(self.query_one("SELECT COUNT(*) FROM endpoints", &[])?.get(0))
    }

    // import

    fn import_state(&self, owner: &str) -> StoreResult<ImportState> {
        let user_id = self.user_id(owner)?;
        let uid = user_id.unwrap_or(-1);
        let agents = self
            .query(
                "SELECT name_lower, address_kind, address_target, host_id, pinned_address FROM agents
                 WHERE owner_id = $1",
                &[&uid],
            )?
            .iter()
            .map(|r| {
                let address = address_of(r.get(1), r.get(2), r.get(3)).map(|a| a.to_string());
                (r.get::<_, String>(0), (address, r.get::<_, Option<String>>(4)))
            })
            .collect();
        Ok(ImportState {
            user_id,
            agents,
            channels: self.set("SELECT name_lower FROM channels", &[], |r| r.get(0))?,
            messages: self.set("SELECT message_id FROM messages", &[], |r| r.get(0))?,
            attachments: self.set("SELECT message_id, filename FROM attachments", &[], |r| (r.get(0), r.get(1)))?,
            memberships: self.set(
                "SELECT c.name_lower, a.name_lower FROM memberships x JOIN channels c ON c.channel_id = x.channel_id
                 JOIN agents a ON a.agent_id = x.agent_id WHERE a.owner_id = $1",
                &[&uid],
                |r| (r.get(0), r.get(1)),
            )?,
            dm_read: self.set(
                "SELECT d.message_id, a.name_lower FROM dm_read d JOIN agents a ON a.agent_id = d.agent_id
                 WHERE a.owner_id = $1",
                &[&uid],
                |r| (r.get(0), r.get(1)),
            )?,
            reactions: self.set(
                "SELECT x.message_id, a.name_lower, x.emoji FROM reactions x JOIN agents a ON a.agent_id = x.actor_id
                 WHERE a.owner_id = $1",
                &[&uid],
                |r| (r.get(0), r.get(1), r.get(2)),
            )?,
        })
    }

    fn insert_imported_agent(&self, a: &ImportedAgent) -> StoreResult<()> {
        self.execute(
            "INSERT INTO agents(agent_id, owner_id, name_lower, display, address_kind, address_target,
             host_id, binding_version, pinned_address) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            &[
                &a.id,
                &a.owner_id,
                &a.name,
                &a.display,
                &a.address.map(|x| x.kind.as_str()),
                &a.address.map(|x| x.target.as_str()),
                &a.address.map(|x| x.host.as_str()),
                &a.binding_version,
                &a.pinned,
            ],
        )?;
        Ok(())
    }

    // diagnostics

    /// "ok" when the schema is at the version this binary carries.
    fn integrity_check(&self) -> StoreResult<String> {
        let at: i32 = self.query_one("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", &[])?.get(0);
        let known = MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0);
        Ok(if at == known { "ok".into() } else { format!("schema version {at}, this binary expects {known}") })
    }

    fn count_bound_agents(&self) -> StoreResult<i64> {
        Ok(self.query_one("SELECT COUNT(*) FROM agents WHERE address_kind IS NOT NULL", &[])?.get(0))
    }

    fn unbound_waiting(&self) -> StoreResult<Vec<(String, i64)>> {
        self.pairs(
            "SELECT u.name || '/' || a.name_lower, COUNT(*) FROM notifications n
             JOIN agents a ON a.agent_id = n.recipient_agent_id JOIN users u ON u.user_id = a.owner_id
             WHERE n.status = 'pending' AND a.address_kind IS NULL GROUP BY 1 ORDER BY 1",
            &[],
        )
    }

    fn count_notifications(&self, statuses: &[&str]) -> StoreResult<i64> {
        Ok(self.query_one("SELECT COUNT(*) FROM notifications WHERE status = ANY($1)", &[&statuses])?.get(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_full_maps_to_verified_require() {
        let c = parse_url("postgres://u:p@db.example.com:5432/conde?sslmode=verify-full").unwrap().config;
        assert_eq!(c.get_ssl_mode(), SslMode::Require);
        let c = parse_url("postgresql://u@db.example.com/conde?connect_timeout=3&sslmode=verify-ca").unwrap().config;
        assert_eq!(c.get_ssl_mode(), SslMode::Require);
        assert_eq!(c.get_connect_timeout(), Some(&Duration::from_secs(3)));
        let c = parse_url("host=db.example.com sslmode=verify-full dbname=conde").unwrap().config;
        assert_eq!(c.get_ssl_mode(), SslMode::Require);
        assert_eq!(c.get_application_name(), Some("conde"));
        assert_eq!(c.get_options(), Some("-c idle_in_transaction_session_timeout=15s -c statement_timeout=60s"));
        let c = parse_url("host=/s dbname=c options='-c statement_timeout=5s'").unwrap().config;
        assert_eq!(c.get_options(), Some("-c statement_timeout=5s -c idle_in_transaction_session_timeout=15s"));
    }

    #[test]
    fn plaintext_only_to_loopback_or_socket() {
        let err = parse_url("postgres://u@db.example.com/conde").err().expect("refused");
        assert!(err.0.contains("sslmode=verify-full"), "{err}");
        assert!(parse_url("postgres://u@db.example.com/conde?sslmode=disable").is_err());
        assert!(parse_url("postgres://u@localhost/conde?sslmode=disable").is_ok());
        assert!(parse_url("host=/tmp/pg user=u dbname=conde sslmode=disable").is_ok());
        // every effective destination counts: hostaddr is dialed in preference to host
        let err = parse_url("hostaddr=192.0.2.1 user=u dbname=conde sslmode=disable").err().expect("refused");
        assert!(err.0.contains("192.0.2.1"), "{}", err.0);
        let err = parse_url("host=localhost hostaddr=192.0.2.1 dbname=conde sslmode=disable").err().expect("refused");
        assert!(err.0.contains("192.0.2.1"), "{}", err.0);
        let err = parse_url("postgres://u@localhost,db.example.com/conde?sslmode=disable").err().expect("refused");
        assert!(err.0.contains("db.example.com"), "{}", err.0);
        assert!(parse_url("host=localhost hostaddr=127.0.0.1 dbname=conde sslmode=disable").is_ok());
        assert!(parse_url("hostaddr=192.0.2.1 dbname=conde sslmode=verify-full").is_ok());
    }

    const AZURE: &str = "postgresql://conde:Q7w9e2R4t6Y8u1I3o5P7a9S2d4F6g8H0@psql-conde.postgres.database.azure.com:5432/conde?sslmode=verify-full&sslrootcert=/etc/ssl/certs/ca-certificates.crt";
    const PASSWORD: &str = "Q7w9e2R4t6Y8u1I3o5P7a9S2d4F6g8H0";

    #[test]
    fn the_azure_libpq_url_parses() {
        let u = parse_url(AZURE).unwrap();
        let c = &u.config;
        assert_eq!(c.get_hosts(), &[Host::Tcp("psql-conde.postgres.database.azure.com".into())]);
        assert_eq!(c.get_ports(), &[5432]);
        assert_eq!((c.get_user(), c.get_dbname()), (Some("conde"), Some("conde")));
        assert_eq!(c.get_password(), Some(PASSWORD.as_bytes()));
        assert_eq!(c.get_ssl_mode(), SslMode::Require);
        assert_eq!(u.root_cert.as_deref(), Some(Path::new("/etc/ssl/certs/ca-certificates.crt")));
        let pinned = tls(c, u.root_cert.as_deref());
        assert!(pinned.is_ok(), "the named bundle loads: {:?}", pinned.err().map(|e| e.0));
    }

    #[test]
    fn sslrootcert_absent_or_system_uses_the_system_bundle() {
        for url in ["postgres://u@db.example.com/conde?sslmode=require", "postgres://u@db.example.com/conde?sslmode=verify-full&sslrootcert=system"] {
            let u = parse_url(url).unwrap();
            assert_eq!(u.root_cert, None, "{url}");
            assert!(tls(&u.config, None).is_ok());
        }
        let allow = parse_url("postgres://u@localhost/conde?sslmode=allow").unwrap();
        assert_eq!(allow.config.get_ssl_mode(), SslMode::Prefer);
    }

    #[test]
    fn a_missing_root_bundle_is_a_named_startup_error() {
        let u = parse_url("postgres://u@db.example.com/conde?sslmode=verify-full&sslrootcert=/nonexistent/ca.pem").unwrap();
        let err = tls(&u.config, u.root_cert.as_deref()).err().expect("refused");
        assert!(err.0.contains("sslrootcert /nonexistent/ca.pem: cannot read it"), "{}", err.0);
    }

    #[test]
    fn unknown_and_client_cert_keys_are_named_errors_without_the_password() {
        let unknown = format!("{AZURE}&sslcrl=/x");
        let err = parse_url(&unknown).err().expect("refused");
        assert!(err.0.contains("unknown parameter 'sslcrl'"), "{}", err.0);
        let err = parse_url(&format!("{AZURE}&sslcert=/c.pem")).err().expect("refused");
        assert!(err.0.contains("sslcert (client certificates) is not supported"), "{}", err.0);
        let err = parse_url("host=db.example.com password=s3cr3t-pw sslmode=require sslkey=/k").err().expect("refused");
        assert!(err.0.contains("sslkey") && !err.0.contains("s3cr3t-pw"), "{}", err.0);
        // whatever the postgres crate says about the rest, the password is masked
        let bad_port = AZURE.replace(":5432", ":notaport");
        let err = parse_url(&bad_port).err().expect("refused");
        assert!(!err.0.contains(PASSWORD), "{}", err.0);
        let err = parse_url(&format!("postgresql://conde:{PASSWORD}@db.example.com/conde")).err().expect("refused");
        assert!(err.0.contains("sslmode=verify-full") && !err.0.contains(PASSWORD), "{}", err.0);
        let encoded = parse_url("postgresql://conde:p%40ss%3Aword@db.example.com:bad/conde?sslmode=require").err().expect("refused");
        assert!(!encoded.0.contains("p%40ss%3Aword") && !encoded.0.contains("p@ss:word"), "{}", encoded.0);
    }

    #[test]
    fn keyword_values_may_be_quoted() {
        let u = parse_url(r"host=/tmp/pg user=u dbname=conde sslmode=disable sslrootcert='/a b/ca.pem' options='-c x=1'").unwrap();
        assert_eq!(u.root_cert.as_deref(), Some(Path::new("/a b/ca.pem")));
        assert!(u.config.get_options().unwrap().starts_with("-c x=1"));
        assert!(parse_url("host=/tmp/pg dbname='unterminated").is_err());
    }

    #[test]
    fn migrations_are_ordered_and_unique() {
        let versions: Vec<i32> = MIGRATIONS.iter().map(|m| m.0).collect();
        assert!(versions.windows(2).all(|w| w[0] < w[1]), "{versions:?}");
        assert_eq!(versions.first(), Some(&1));
    }
}
