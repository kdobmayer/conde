//! SQLite backend (local mode): one connection behind a mutex, so a
//! transaction holds the board alone, as the dispatcher always did.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::hash::Hash;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::types::FromSql;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Params, Row, params, params_from_iter};

use super::{
    Agent, AgentHealth, Attachment, AttachmentMeta, AttemptRow, Channel, ChannelSummary,
    ChannelThread, CredentialRow, Delivery, DmDelivery, DmThread, DueNotification, EndpointRow,
    EvidenceRow, FeedArg, FeedRange, HostBridge, ImportState, ImportedAgent, McpCodeRow,
    McpGrantRow, McpRefreshRow, Membership, Message, Mode, NewMessage, NewNotification,
    NotificationRow, OpenAttempt, PendingSignal, SearchScope, SlackDue, StaleAttempt, Store,
    StoreError, StoreResult, Tx, UserRow, UserSlack, Wake, Wakes, WebSessionRow, address_of,
    feed_seqs_sql, problem_seqs_sql, thread_down_sql, thread_up_sql,
};
use crate::address::Address;

const SCHEMA: &str = include_str!("sqlite_schema.sql");

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> StoreError {
        StoreError(e.to_string())
    }
}

/// Columns added after the first schema; CREATE TABLE IF NOT EXISTS keeps old tables as they were.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("attempts", "host_id", "TEXT"),
    ("attempts", "text", "TEXT"),
    ("device_credentials", "label", "TEXT"),
    ("device_credentials", "created_at", "TEXT"),
    ("channels", "topic", "TEXT"),
    ("users", "disabled", "INTEGER NOT NULL DEFAULT 0"),
    ("attempts", "settled_at", "TEXT"),
    ("hosts", "route_revision", "INTEGER NOT NULL DEFAULT 0"),
    ("agents", "retired_at", "TEXT"),
    ("hosts", "mailbox_only", "INTEGER NOT NULL DEFAULT 0"),
    ("mcp_grants", "code_digest", "TEXT"),
    ("attempts", "not_before", "INTEGER"),
    ("users", "email", "TEXT"),
    ("users", "slack_member_id", "TEXT"),
    ("users", "slack_source", "TEXT"),
    ("users", "done_pages", "INTEGER NOT NULL DEFAULT 0"),
    ("messages", "page", "TEXT"),
    ("users", "keycloak_id", "TEXT"),
    ("users", "keycloak_invite", "TEXT"),
    ("users", "admin", "INTEGER NOT NULL DEFAULT 0"),
    ("users", "arrived_at", "TEXT"),
];

pub struct SqliteStore {
    conn: Mutex<Connection>,
    signal: PendingSignal,
}

impl SqliteStore {
    /// Opens (creating) the board file in WAL mode and brings its schema up to date.
    pub fn open(path: &Path) -> StoreResult<SqliteStore> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        init(&conn)?;
        Ok(SqliteStore::with(conn))
    }

    pub fn open_in_memory() -> StoreResult<SqliteStore> {
        let conn = Connection::open_in_memory()?;
        init(&conn)?;
        Ok(SqliteStore::with(conn))
    }

    /// An existing board, read only, schema untouched (dry runs).
    pub fn open_read_only(path: &Path) -> StoreResult<SqliteStore> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(SqliteStore::with(conn))
    }

    /// The raw connection, between transactions (tests inject faults through it).
    pub fn with_connection<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        f(&self.conn.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn with(conn: Connection) -> SqliteStore {
        SqliteStore {
            conn: Mutex::new(conn),
            signal: PendingSignal::default(),
        }
    }

    /// A write transaction that also reaches the local board's own tables
    /// ([`super::local::LocalTx`]); commit it with [`Tx::commit`].
    pub fn begin_local(&self) -> StoreResult<Box<SqliteTx<'_>>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Box::new(SqliteTx {
            conn,
            signal: &self.signal,
            open: Cell::new(true),
            failed: Cell::new(false),
            wakes: RefCell::default(),
        }))
    }
}

fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA)?;
    for (table, column, decl) in ADDED_COLUMNS {
        let mut stmt = conn.prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
        ))?;
        if stmt.exists([column])? {
            continue;
        }
        let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {decl}");
        if *column != "arrived_at" {
            conn.execute_batch(&alter)?;
            continue;
        }
        // as Postgres migration 0019, in the ALTER's transaction: users with a host before the
        // welcome count as arrived
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let mut steps = || -> rusqlite::Result<()> {
            // another process may have added it while this one waited for the lock
            if stmt.exists([column])? {
                return Ok(());
            }
            conn.execute_batch(&alter)?;
            conn.execute_batch(
                "UPDATE users SET arrived_at = COALESCE((SELECT MIN(c.created_at) FROM device_credentials c
                     WHERE c.user_id = users.user_id), '1970-01-01T00:00:00Z')
                 WHERE EXISTS (SELECT 1 FROM hosts h WHERE h.user_id = users.user_id)",
            )
        };
        match steps() {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS attempts_by_instance ON attempts(bridge_instance, status);
         CREATE INDEX IF NOT EXISTS credentials_by_host ON device_credentials(host_id);
         CREATE INDEX IF NOT EXISTS notifications_by_recipient ON notifications(recipient_agent_id);
         CREATE UNIQUE INDEX IF NOT EXISTS mcp_grants_by_code ON mcp_grants(code_digest) WHERE code_digest IS NOT NULL;
         CREATE INDEX IF NOT EXISTS messages_by_day ON messages(substr(created_at, 1, 10), commit_seq);
         CREATE INDEX IF NOT EXISTS messages_dm_by_sender ON messages(sender_agent_id, commit_seq) WHERE channel_id IS NULL;
         CREATE INDEX IF NOT EXISTS messages_by_reply_to ON messages(reply_to, commit_seq);",
    )?;
    rebuild_agents(conn)?;
    rebuild_identities(conn)
}

/// A file from before sign-in by any issuer (identities keyed by Entra's `tid`, `oid`, one per
/// user) gets the table rebuilt in the schema's shape, each row keeping its binding as
/// (the tenant's v2.0 issuer URL, oid), as Postgres migration 0017 does. Runs once per file.
fn rebuild_identities(conn: &Connection) -> rusqlite::Result<()> {
    let old_shape = || {
        conn.prepare("SELECT 1 FROM pragma_table_info('identities') WHERE name = 'tid'")?
            .exists([])
    };
    if !old_shape()? {
        return Ok(());
    }
    const START: &str = "CREATE TABLE IF NOT EXISTS identities(";
    let at = SCHEMA.find(START).expect("the schema defines identities");
    let len = SCHEMA[at..]
        .find(");")
        .expect("the identities definition ends")
        + 2;
    let create = SCHEMA[at..at + len].replacen(START, "CREATE TABLE identities_rebuilt(", 1);
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let steps = || -> rusqlite::Result<()> {
        // another process may have rebuilt it while this one waited for the lock
        if !old_shape()? {
            return Ok(());
        }
        conn.execute_batch(&create)?;
        conn.execute_batch(
            "INSERT INTO identities_rebuilt(issuer, subject, user_id, display_name, preferred_username)
                 SELECT 'https://login.microsoftonline.com/' || tid || '/v2.0', oid, user_id, display_name,
                        preferred_username FROM identities;
             DROP TABLE identities;
             ALTER TABLE identities_rebuilt RENAME TO identities;",
        )
    };
    match steps() {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// A file created before `address_kind` accepted 'mailbox' (DESIGN-v2.md §4) gets the agents
/// table rebuilt with the schema's CHECK: the standard SQLite rebuild, in one transaction with
/// foreign keys off, verified by `foreign_key_check` before it commits. Runs once per file.
fn rebuild_agents(conn: &Connection) -> rusqlite::Result<()> {
    let current: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'agents'",
        [],
        |r| r.get(0),
    )?;
    if current.contains("'mailbox'") {
        return Ok(());
    }
    const START: &str = "CREATE TABLE IF NOT EXISTS agents(";
    let at = SCHEMA.find(START).expect("the schema defines agents");
    let len = SCHEMA[at..].find(");").expect("the agents definition ends") + 2;
    let create = SCHEMA[at..at + len].replacen(START, "CREATE TABLE agents_rebuilt(", 1);
    const COLUMNS: &str = "agent_id, owner_id, name_lower, display, address_kind, address_target, host_id, binding_version, pinned_address";
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let rebuilt = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let steps = || -> rusqlite::Result<()> {
            conn.execute_batch(&create)?;
            conn.execute_batch(&format!(
                "INSERT INTO agents_rebuilt({COLUMNS}) SELECT {COLUMNS} FROM agents;
                 DROP TABLE agents;
                 ALTER TABLE agents_rebuilt RENAME TO agents;
                 CREATE INDEX IF NOT EXISTS agents_by_address ON agents(address_kind, address_target, host_id);"
            ))?;
            let broken = conn.prepare("PRAGMA foreign_key_check")?.exists([])?;
            if broken {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                    Some("the agents rebuild left a dangling reference".into()),
                ));
            }
            Ok(())
        };
        match steps() {
            Ok(()) => conn.execute_batch("COMMIT"),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    })();
    conn.pragma_update(None, "foreign_keys", "ON")?;
    rebuilt
}

impl Store for SqliteStore {
    fn begin(&self, mode: Mode) -> StoreResult<Box<dyn Tx + '_>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute_batch(match mode {
            Mode::Read => "BEGIN DEFERRED",
            Mode::Write => "BEGIN IMMEDIATE",
        })?;
        Ok(Box::new(SqliteTx {
            conn,
            signal: &self.signal,
            open: Cell::new(true),
            failed: Cell::new(false),
            wakes: RefCell::default(),
        }))
    }

    fn pending(&self) -> &PendingSignal {
        &self.signal
    }

    fn backend(&self) -> &'static str {
        "sqlite"
    }

    fn ping(&self) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .is_ok()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct SqliteTx<'a> {
    conn: MutexGuard<'a, Connection>,
    signal: &'a PendingSignal,
    open: Cell<bool>,
    failed: Cell<bool>,
    wakes: RefCell<Wakes>,
}

impl Drop for SqliteTx<'_> {
    fn drop(&mut self) {
        if self.open.get() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

const AGENT_SELECT: &str =
    "SELECT a.agent_id, a.owner_id, u.name, a.name_lower, a.display, a.address_kind,
    a.address_target, a.host_id, a.binding_version, a.pinned_address, a.retired_at IS NOT NULL
    FROM agents a JOIN users u ON u.user_id = a.owner_id";

fn agent_row(r: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: r.get(0)?,
        owner_id: r.get(1)?,
        owner: r.get(2)?,
        name: r.get(3)?,
        display: r.get(4)?,
        address: address_of(r.get(5)?, r.get(6)?, r.get(7)?),
        binding_version: r.get(8)?,
        pinned: r.get(9)?,
        retired: r.get(10)?,
    })
}

const MSG_SELECT: &str = "SELECT m.commit_seq, m.message_id, m.sender_agent_id, su.name || '/' || s.name_lower,
    m.recipient_agent_id, ru.name || '/' || r.name_lower, m.channel_id, c.name_lower, m.text, m.reply_to, m.created_at,
    m.page
    FROM messages m
    JOIN agents s ON s.agent_id = m.sender_agent_id JOIN users su ON su.user_id = s.owner_id
    LEFT JOIN agents r ON r.agent_id = m.recipient_agent_id LEFT JOIN users ru ON ru.user_id = r.owner_id
    LEFT JOIN channels c ON c.channel_id = m.channel_id";

const MCP_GRANT_SELECT: &str =
    "SELECT grant_id, user_id, client_id, client_host, client_name, resource, created_at,
    last_used_at, expires_at, revoked_at FROM mcp_grants";

fn mcp_grant_row(r: &Row) -> rusqlite::Result<McpGrantRow> {
    Ok(McpGrantRow {
        grant_id: r.get(0)?,
        user_id: r.get(1)?,
        client_id: r.get(2)?,
        client_host: r.get(3)?,
        client_name: r.get(4)?,
        resource: r.get(5)?,
        created_at: r.get(6)?,
        last_used_at: r.get(7)?,
        expires_at: r.get(8)?,
        revoked_at: r.get(9)?,
    })
}

fn msg_row(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        seq: r.get(0)?,
        id: r.get(1)?,
        sender_id: r.get(2)?,
        sender: r.get(3)?,
        recipient_id: r.get(4)?,
        recipient: r.get(5)?,
        channel_id: r.get(6)?,
        channel: r.get(7)?,
        text: r.get(8)?,
        reply_to: r.get(9)?,
        created_at: r.get(10)?,
        page: r.get(11)?,
    })
}

const USER_SLACK_SELECT: &str =
    "SELECT user_id, name, email, slack_member_id, slack_source, done_pages FROM users";

fn user_slack_row(r: &Row) -> rusqlite::Result<UserSlack> {
    Ok(UserSlack {
        user_id: r.get(0)?,
        name: r.get(1)?,
        email: r.get(2)?,
        slack_member_id: r.get(3)?,
        slack_source: r.get(4)?,
        done_pages: r.get(5)?,
    })
}

const USER_SELECT: &str = "SELECT u.user_id, u.name, u.disabled,
    EXISTS (SELECT 1 FROM identities i WHERE i.user_id = u.user_id), u.email, u.admin FROM users u";

fn user_row(r: &Row) -> rusqlite::Result<UserRow> {
    Ok(UserRow {
        user_id: r.get(0)?,
        name: r.get(1)?,
        disabled: r.get(2)?,
        bound: r.get(3)?,
        email: r.get(4)?,
        admin: r.get(5)?,
    })
}

fn pair<A: FromSql, B: FromSql>(r: &Row) -> rusqlite::Result<(A, B)> {
    Ok((r.get(0)?, r.get(1)?))
}

fn one_col<A: FromSql>(r: &Row) -> rusqlite::Result<A> {
    r.get(0)
}

impl SqliteTx<'_> {
    /// The connection, inside this transaction (the local tables' statements).
    pub(crate) fn raw(&self) -> &Connection {
        &self.conn
    }

    /// A failed statement poisons the transaction, as it does on Postgres.
    pub(crate) fn guard<T>(&self, r: rusqlite::Result<T>) -> StoreResult<T> {
        r.map_err(|e| {
            self.failed.set(true);
            e.into()
        })
    }

    fn exec(&self, sql: &str, args: impl Params) -> StoreResult<usize> {
        self.guard(self.conn.execute(sql, args))
    }

    fn one<T>(
        &self,
        sql: &str,
        args: impl Params,
        f: impl FnOnce(&Row) -> rusqlite::Result<T>,
    ) -> StoreResult<T> {
        self.guard(self.conn.query_row(sql, args, f))
    }

    fn opt<T>(
        &self,
        sql: &str,
        args: impl Params,
        f: impl FnOnce(&Row) -> rusqlite::Result<T>,
    ) -> StoreResult<Option<T>> {
        self.guard(self.conn.query_row(sql, args, f).optional())
    }

    fn rows<T>(
        &self,
        sql: &str,
        args: impl Params,
        f: impl FnMut(&Row) -> rusqlite::Result<T>,
    ) -> StoreResult<Vec<T>> {
        let run = || -> rusqlite::Result<Vec<T>> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(args, f)?;
            rows.collect()
        };
        self.guard(run())
    }

    fn set<T: Eq + Hash>(
        &self,
        sql: &str,
        args: impl Params,
        f: impl FnMut(&Row) -> rusqlite::Result<T>,
    ) -> StoreResult<HashSet<T>> {
        Ok(self.rows(sql, args, f)?.into_iter().collect())
    }

    fn msgs(&self, where_clause: &str, args: impl Params) -> StoreResult<Vec<Message>> {
        self.rows(&format!("{MSG_SELECT} WHERE {where_clause}"), args, msg_row)
    }

    fn hosts_to_wake(&self) -> StoreResult<Vec<String>> {
        let wakes = self.wakes.borrow();
        let mut hosts = wakes.hosts.clone();
        if !wakes.notifications.is_empty() {
            let marks = vec!["?"; wakes.notifications.len()].join(", ");
            let sql = format!(
                "SELECT DISTINCT a.host_id FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
                 WHERE n.status = 'pending' AND a.host_id IS NOT NULL AND n.notification_id IN ({marks})"
            );
            hosts.extend(self.rows(&sql, params_from_iter(wakes.notifications.iter()), one_col)?);
        }
        Ok(hosts.into_iter().collect())
    }
}

impl Tx for SqliteTx<'_> {
    fn commit(self: Box<Self>) -> StoreResult<()> {
        if self.failed.get() {
            return Err(StoreError(
                "transaction aborted by an earlier failed statement".into(),
            ));
        }
        let hosts = if self.wakes.borrow().is_empty() {
            Vec::new()
        } else {
            self.hosts_to_wake()?
        };
        self.conn.execute_batch("COMMIT")?;
        self.open.set(false);
        for host in hosts {
            self.signal.raise(Wake::Host(host));
        }
        for instance in &self.wakes.borrow().fenced {
            self.signal.raise(Wake::Fenced(instance.clone()));
        }
        for host in &self.wakes.borrow().revoked {
            self.signal.raise(Wake::Revoked(host.clone()));
        }
        let board = self.wakes.borrow().board;
        if board != 0 {
            self.signal.raise(Wake::Board(board));
        }
        Ok(())
    }

    // principals and credentials

    fn ensure_user(&self, name: &str) -> StoreResult<i64> {
        self.exec("INSERT OR IGNORE INTO users(name) VALUES (?1)", [name])?;
        self.one("SELECT user_id FROM users WHERE name = ?1", [name], one_col)
    }

    fn ensure_host(&self, host: &str, user_id: i64) -> StoreResult<i64> {
        self.exec(
            "INSERT OR IGNORE INTO hosts(host_id, user_id) VALUES (?1, ?2)",
            params![host, user_id],
        )?;
        self.one(
            "SELECT user_id FROM hosts WHERE host_id = ?1",
            [host],
            one_col,
        )
    }

    fn user_id(&self, name: &str) -> StoreResult<Option<i64>> {
        self.opt("SELECT user_id FROM users WHERE name = ?1", [name], one_col)
    }

    fn host_owner(&self, host: &str) -> StoreResult<Option<(i64, String)>> {
        self.opt(
            "SELECT u.user_id, u.name FROM hosts h JOIN users u ON u.user_id = h.user_id WHERE h.host_id = ?1",
            [host],
            pair,
        )
    }

    fn insert_credential(
        &self,
        digest: &str,
        user_id: i64,
        host: &str,
        label: &str,
        created_at: &str,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO device_credentials(token_digest, user_id, host_id, label, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![digest, user_id, host, label, created_at],
        )?;
        Ok(())
    }

    fn credentials(&self) -> StoreResult<Vec<CredentialRow>> {
        self.rows(
            "SELECT c.token_digest, u.name, c.host_id, c.label, c.created_at, c.revoked_at
             FROM device_credentials c JOIN users u ON u.user_id = c.user_id
             ORDER BY COALESCE(c.created_at, ''), c.host_id",
            [],
            |r| {
                Ok(CredentialRow {
                    digest: r.get(0)?,
                    user: r.get(1)?,
                    host: r.get(2)?,
                    label: r.get(3)?,
                    created_at: r.get(4)?,
                    revoked_at: r.get(5)?,
                })
            },
        )
    }

    fn revoke_credential(&self, key_id: &str, at: &str) -> StoreResult<u64> {
        let hosts: Vec<String> = self.rows(
            "UPDATE device_credentials SET revoked_at = ?2
             WHERE substr(token_digest, 1, length(?1)) = ?1 AND revoked_at IS NULL RETURNING host_id",
            params![key_id, at],
            one_col,
        )?;
        let n = hosts.len() as u64;
        self.wakes.borrow_mut().revoked.extend(hosts);
        Ok(n)
    }

    fn revoke_host_credentials(&self, host: &str, at: &str) -> StoreResult<u64> {
        let n = self.exec(
            "UPDATE device_credentials SET revoked_at = ?2 WHERE host_id = ?1 AND revoked_at IS NULL",
            params![host, at],
        )? as u64;
        if n > 0 {
            self.wakes.borrow_mut().revoked.insert(host.to_string());
        }
        Ok(n)
    }

    fn host_mailbox_only(&self, host: &str) -> StoreResult<bool> {
        Ok(self
            .opt(
                "SELECT mailbox_only FROM hosts WHERE host_id = ?1",
                [host],
                one_col::<bool>,
            )?
            .unwrap_or(false))
    }

    fn set_host_mailbox_only(&self, host: &str) -> StoreResult<()> {
        self.exec(
            "UPDATE hosts SET mailbox_only = 1 WHERE host_id = ?1",
            [host],
        )?;
        Ok(())
    }

    fn host_evidence(&self, owner_id: i64, host: &str) -> StoreResult<Vec<EvidenceRow>> {
        self.rows(super::HOST_EVIDENCE, params![owner_id, host], |r| {
            Ok(EvidenceRow {
                agent_id: r.get(0)?,
                via: r.get(1)?,
                with: r.get(2)?,
                created_at: r.get(3)?,
                imported: r.get::<_, String>(4)? == "imported",
            })
        })
    }

    fn host_last_seen(&self, host: &str) -> StoreResult<Option<String>> {
        self.one(
            "SELECT MAX(updated_at) FROM bridge_instances WHERE host_id = ?1",
            [host],
            one_col,
        )
    }

    fn authenticate(&self, digest: &str) -> StoreResult<Option<(i64, String)>> {
        self.opt(
            "SELECT c.user_id, c.host_id FROM device_credentials c JOIN users u ON u.user_id = c.user_id
             WHERE c.token_digest = ?1 AND c.revoked_at IS NULL AND u.disabled = 0",
            [digest],
            pair,
        )
    }

    // users, sign-in identities and browser sign-in

    fn user_by_identity(&self, issuer: &str, subject: &str) -> StoreResult<Option<UserRow>> {
        self.opt(
            &format!(
                "{USER_SELECT} WHERE u.user_id = (SELECT user_id FROM identities WHERE issuer = ?1 AND subject = ?2)"
            ),
            params![issuer, subject],
            user_row,
        )
    }

    fn user_by_name(&self, name: &str) -> StoreResult<Option<UserRow>> {
        self.opt(
            &format!("{USER_SELECT} WHERE u.name = ?1"),
            [name],
            user_row,
        )
    }

    fn user_by_id(&self, user_id: i64) -> StoreResult<Option<UserRow>> {
        self.opt(
            &format!("{USER_SELECT} WHERE u.user_id = ?1"),
            [user_id],
            user_row,
        )
    }

    fn users_by_email(&self, email: &str) -> StoreResult<Vec<UserRow>> {
        self.rows(
            &format!("{USER_SELECT} WHERE lower(u.email) = lower(?1) ORDER BY u.user_id"),
            [email],
            user_row,
        )
    }

    fn users(&self) -> StoreResult<Vec<UserRow>> {
        self.rows(&format!("{USER_SELECT} ORDER BY u.name"), [], user_row)
    }

    fn identity_issuers(&self, user_id: i64) -> StoreResult<Vec<String>> {
        self.rows(
            "SELECT issuer FROM identities WHERE user_id = ?1 ORDER BY issuer",
            [user_id],
            |r| r.get(0),
        )
    }

    fn bind_identity(
        &self,
        user_id: i64,
        issuer: &str,
        subject: &str,
        display: Option<&str>,
        upn: Option<&str>,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO identities(issuer, subject, user_id, display_name, preferred_username) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![issuer, subject, user_id, display, upn],
        )?;
        Ok(())
    }

    fn set_identity_profile(
        &self,
        issuer: &str,
        subject: &str,
        display: Option<&str>,
        upn: Option<&str>,
    ) -> StoreResult<()> {
        self.exec(
            "UPDATE identities SET display_name = ?3, preferred_username = ?4 WHERE issuer = ?1 AND subject = ?2",
            params![issuer, subject, display, upn],
        )?;
        Ok(())
    }

    fn users_slack(&self) -> StoreResult<Vec<UserSlack>> {
        self.rows(
            &format!("{USER_SLACK_SELECT} ORDER BY user_id"),
            [],
            user_slack_row,
        )
    }

    fn user_slack(&self, user_id: i64) -> StoreResult<Option<UserSlack>> {
        self.opt(
            &format!("{USER_SLACK_SELECT} WHERE user_id = ?1"),
            [user_id],
            user_slack_row,
        )
    }

    fn set_user_email(&self, user_id: i64, email: Option<&str>) -> StoreResult<()> {
        self.exec(
            "UPDATE users SET email = ?2 WHERE user_id = ?1",
            params![user_id, email],
        )?;
        Ok(())
    }

    fn user_keycloak(&self, user_id: i64) -> StoreResult<(Option<String>, Option<String>)> {
        Ok(self
            .opt(
                "SELECT keycloak_id, keycloak_invite FROM users WHERE user_id = ?1",
                [user_id],
                pair,
            )?
            .unwrap_or_default())
    }

    fn claim_user_keycloak_invite(&self, user_id: i64, candidate: &str) -> StoreResult<bool> {
        Ok(self.exec(
            "UPDATE users SET keycloak_invite = ?2
                 WHERE user_id = ?1 AND keycloak_id IS NULL AND keycloak_invite IS NULL",
            params![user_id, candidate],
        )? > 0)
    }

    fn record_user_keycloak_id(&self, user_id: i64, invite: &str, id: &str) -> StoreResult<bool> {
        Ok(self.exec(
            "UPDATE users SET keycloak_id = ?2
                 WHERE user_id = ?1 AND keycloak_invite = ?3 AND keycloak_id IS NULL",
            params![user_id, id, invite],
        )? > 0)
    }

    fn clear_user_keycloak(&self, user_id: i64) -> StoreResult<bool> {
        Ok(self.exec(
            "UPDATE users SET keycloak_id = NULL, keycloak_invite = NULL
                 WHERE user_id = ?1 AND (keycloak_id IS NOT NULL OR keycloak_invite IS NOT NULL)",
            [user_id],
        )? > 0)
    }

    fn backfill_emails(&self) -> StoreResult<u64> {
        Ok(self.exec(
            "UPDATE users SET email = (SELECT MIN(i.preferred_username) FROM identities i
                 WHERE i.user_id = users.user_id AND i.preferred_username LIKE '%@%'
                     AND i.issuer LIKE 'https://login.microsoftonline.com/%')
             WHERE email IS NULL AND EXISTS (SELECT 1 FROM identities i
                 WHERE i.user_id = users.user_id AND i.preferred_username LIKE '%@%'
                     AND i.issuer LIKE 'https://login.microsoftonline.com/%')",
            [],
        )? as u64)
    }

    fn set_user_slack(&self, user_id: i64, member_id: &str, source: &str) -> StoreResult<()> {
        self.exec(
            "UPDATE users SET slack_member_id = ?2, slack_source = ?3 WHERE user_id = ?1",
            params![user_id, member_id, source],
        )?;
        Ok(())
    }

    fn set_done_pages(&self, user_id: i64, on: bool) -> StoreResult<()> {
        self.exec(
            "UPDATE users SET done_pages = ?2 WHERE user_id = ?1",
            params![user_id, on],
        )?;
        Ok(())
    }

    fn set_user_admin(&self, user_id: i64, admin: bool) -> StoreResult<()> {
        self.exec(
            "UPDATE users SET admin = ?2 WHERE user_id = ?1",
            params![user_id, admin],
        )?;
        Ok(())
    }

    fn mark_user_arrived(&self, user_id: i64, at: &str) -> StoreResult<bool> {
        Ok(self.exec(
            "UPDATE users SET arrived_at = ?2 WHERE user_id = ?1 AND arrived_at IS NULL",
            params![user_id, at],
        )? > 0)
    }

    fn setting(&self, key: &str) -> StoreResult<Option<String>> {
        self.opt("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
    }

    fn set_setting(&self, key: &str, value: &str) -> StoreResult<()> {
        self.exec(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn set_user_disabled(&self, user_id: i64, disabled: bool) -> StoreResult<Vec<String>> {
        self.exec(
            "UPDATE users SET disabled = ?2 WHERE user_id = ?1",
            params![user_id, disabled],
        )?;
        if disabled {
            self.exec("DELETE FROM web_sessions WHERE user_id = ?1", [user_id])?;
        }
        let hosts: Vec<String> = self.rows(
            "SELECT host_id FROM hosts WHERE user_id = ?1 ORDER BY host_id",
            [user_id],
            one_col,
        )?;
        if disabled {
            self.wakes
                .borrow_mut()
                .revoked
                .extend(hosts.iter().cloned());
        }
        Ok(hosts)
    }

    fn insert_web_session(&self, id_digest: &str, w: &WebSessionRow) -> StoreResult<()> {
        self.exec(
            "INSERT INTO web_sessions(id_digest, user_id, display, csrf, created_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id_digest,
                w.user_id,
                w.display,
                w.csrf,
                w.created_at,
                w.last_seen
            ],
        )?;
        Ok(())
    }

    fn web_session(&self, id_digest: &str) -> StoreResult<Option<WebSessionRow>> {
        self.opt(
            "SELECT s.user_id, u.name, s.display, s.csrf, s.created_at, s.last_seen
             FROM web_sessions s JOIN users u ON u.user_id = s.user_id WHERE s.id_digest = ?1",
            [id_digest],
            |r| {
                Ok(WebSessionRow {
                    user_id: r.get(0)?,
                    user_name: r.get(1)?,
                    display: r.get(2)?,
                    csrf: r.get(3)?,
                    created_at: r.get(4)?,
                    last_seen: r.get(5)?,
                })
            },
        )
    }

    fn touch_web_session(&self, id_digest: &str, at: i64) -> StoreResult<()> {
        self.exec(
            "UPDATE web_sessions SET last_seen = ?2 WHERE id_digest = ?1",
            params![id_digest, at],
        )?;
        Ok(())
    }

    fn delete_web_session(&self, id_digest: &str) -> StoreResult<()> {
        self.exec("DELETE FROM web_sessions WHERE id_digest = ?1", [id_digest])?;
        Ok(())
    }

    fn put_web_token(
        &self,
        kind: &str,
        digest: &str,
        payload: &str,
        created_at: i64,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO web_tokens(kind, digest, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![kind, digest, payload, created_at],
        )?;
        Ok(())
    }

    fn take_web_token(&self, kind: &str, digest: &str) -> StoreResult<Option<(String, i64)>> {
        self.opt(
            "DELETE FROM web_tokens WHERE kind = ?1 AND digest = ?2 RETURNING payload, created_at",
            params![kind, digest],
            pair,
        )
    }

    fn purge_web(
        &self,
        now: i64,
        session_idle: i64,
        session_max: i64,
        ttls: &[(&str, i64)],
    ) -> StoreResult<()> {
        self.exec(
            "DELETE FROM web_sessions WHERE last_seen < ?1 - ?2 OR created_at < ?1 - ?3",
            params![now, session_idle, session_max],
        )?;
        for (kind, ttl) in ttls {
            self.exec(
                "DELETE FROM web_tokens WHERE kind = ?1 AND created_at < ?2 - ?3",
                params![kind, now, ttl],
            )?;
        }
        Ok(())
    }

    // idempotency

    fn idempotency_get(
        &self,
        actor_id: &str,
        op: &str,
        key: &str,
    ) -> StoreResult<Option<(String, String)>> {
        self.opt(
            "SELECT request_hash, result_json FROM idempotency WHERE actor_id = ?1 AND op = ?2 AND key = ?3",
            params![actor_id, op, key],
            pair,
        )
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
        self.exec(
            "INSERT INTO idempotency(actor_id, op, key, request_hash, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![actor_id, op, key, request_hash, result_json, created_at],
        )?;
        Ok(())
    }

    // agents

    fn agent_by_id(&self, id: &str) -> StoreResult<Option<Agent>> {
        self.opt(
            &format!("{AGENT_SELECT} WHERE a.agent_id = ?1"),
            [id],
            agent_row,
        )
    }

    fn agent_by_name(&self, owner_id: i64, name: &str) -> StoreResult<Option<Agent>> {
        self.opt(
            &format!("{AGENT_SELECT} WHERE a.owner_id = ?1 AND a.name_lower = ?2"),
            params![owner_id, name],
            agent_row,
        )
    }

    fn agents_at_address(&self, owner_id: i64, address: &Address) -> StoreResult<Vec<Agent>> {
        self.rows(
            &format!(
                "{AGENT_SELECT} WHERE a.owner_id = ?1 AND a.address_kind = ?2 AND a.address_target = ?3
                 AND a.host_id = ?4 ORDER BY a.name_lower"
            ),
            params![owner_id, address.kind.as_str(), address.target, address.host],
            agent_row,
        )
    }

    fn retire_agent(&self, agent_id: &str, at: &str) -> StoreResult<bool> {
        let n = self.exec(
            "UPDATE agents SET retired_at = ?2 WHERE agent_id = ?1 AND address_kind IS NULL",
            params![agent_id, at],
        )?;
        Ok(n == 1)
    }

    fn bound_agents(&self) -> StoreResult<Vec<Agent>> {
        self.rows(
            &format!(
                "{AGENT_SELECT} WHERE a.address_kind IS NOT NULL ORDER BY u.name, a.name_lower"
            ),
            [],
            agent_row,
        )
    }

    fn agents_on_host(&self, host: &str) -> StoreResult<Vec<Agent>> {
        self.rows(
            &format!("{AGENT_SELECT} WHERE a.host_id = ?1 AND a.address_kind IS NOT NULL ORDER BY u.name, a.name_lower"),
            [host],
            agent_row,
        )
    }

    fn insert_agent(&self, id: &str, owner_id: i64, name: &str) -> StoreResult<()> {
        self.exec(
            "INSERT INTO agents(agent_id, owner_id, name_lower) VALUES (?1, ?2, ?3)",
            params![id, owner_id, name],
        )?;
        Ok(())
    }

    fn set_binding(&self, agent_id: &str, address: Option<&Address>) -> StoreResult<()> {
        let old: Option<String> = self
            .opt(
                "SELECT host_id FROM agents WHERE agent_id = ?1",
                [agent_id],
                one_col,
            )?
            .flatten();
        self.wakes.borrow_mut().hosts.extend(old.clone());
        self.exec(
            "UPDATE agents SET address_kind = ?2, address_target = ?3, host_id = ?4,
             binding_version = binding_version + 1,
             retired_at = CASE WHEN ?2 IS NULL THEN retired_at END WHERE agent_id = ?1",
            params![
                agent_id,
                address.map(|a| a.kind.as_str()),
                address.map(|a| a.target.as_str()),
                address.map(|a| a.host.as_str())
            ],
        )?;
        self.exec(
            "UPDATE hosts SET route_revision = route_revision + 1 WHERE host_id IN (?1, ?2)",
            params![old, address.map(|a| a.host.as_str())],
        )?;
        if let Some(a) = address {
            self.wakes.borrow_mut().hosts.insert(a.host.clone());
        }
        Ok(())
    }

    fn route_revision(&self, host: &str) -> StoreResult<u64> {
        let r: Option<i64> = self.opt(
            "SELECT route_revision FROM hosts WHERE host_id = ?1",
            [host],
            one_col,
        )?;
        Ok(r.unwrap_or_default() as u64)
    }

    fn set_pin(&self, agent_id: &str, pinned: Option<&str>) -> StoreResult<()> {
        self.exec(
            "UPDATE agents SET pinned_address = ?2 WHERE agent_id = ?1",
            params![agent_id, pinned],
        )?;
        Ok(())
    }

    fn set_display(&self, agent_id: &str, display: Option<&str>) -> StoreResult<()> {
        self.exec(
            "UPDATE agents SET display = ?2 WHERE agent_id = ?1",
            params![agent_id, display],
        )?;
        Ok(())
    }

    fn rename_agent(&self, agent_id: &str, name: &str) -> StoreResult<()> {
        self.exec(
            "UPDATE agents SET name_lower = ?2 WHERE agent_id = ?1",
            params![agent_id, name],
        )?;
        Ok(())
    }

    // channels

    fn channel_by_name(&self, name: &str) -> StoreResult<Option<Channel>> {
        self.opt(
            "SELECT channel_id, name_lower, created_by, topic FROM channels WHERE name_lower = ?1",
            [name],
            |r| {
                Ok(Channel {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_by: r.get(2)?,
                    topic: r.get(3)?,
                })
            },
        )
    }

    fn channel_id_by_name(&self, name: &str) -> StoreResult<Option<String>> {
        self.opt(
            "SELECT channel_id FROM channels WHERE name_lower = ?1",
            [name],
            one_col,
        )
    }

    fn insert_channel(&self, channel: &Channel) -> StoreResult<()> {
        self.exec(
            "INSERT INTO channels(channel_id, name_lower, created_by, topic) VALUES (?1, ?2, ?3, ?4)",
            params![channel.id, channel.name, channel.created_by, channel.topic],
        )?;
        Ok(())
    }

    fn set_channel_topic(&self, channel_id: &str, topic: Option<&str>) -> StoreResult<()> {
        self.exec(
            "UPDATE channels SET topic = ?2 WHERE channel_id = ?1",
            params![channel_id, topic],
        )?;
        Ok(())
    }

    fn channel_member_ids(&self, channel_id: &str) -> StoreResult<Vec<String>> {
        self.rows(
            "SELECT agent_id FROM memberships WHERE channel_id = ?1 ORDER BY agent_id",
            [channel_id],
            one_col,
        )
    }

    fn read_seq(&self, channel_id: &str, agent_id: &str) -> StoreResult<Option<i64>> {
        self.opt(
            "SELECT read_seq FROM memberships WHERE channel_id = ?1 AND agent_id = ?2",
            params![channel_id, agent_id],
            one_col,
        )
    }

    fn advance_cursor(&self, channel_id: &str, agent_id: &str, seq: i64) -> StoreResult<()> {
        self.exec(
            "UPDATE memberships SET read_seq = MAX(read_seq, ?3) WHERE channel_id = ?1 AND agent_id = ?2",
            params![channel_id, agent_id, seq],
        )?;
        Ok(())
    }

    fn channel_last_seq(&self, channel_id: &str) -> StoreResult<i64> {
        self.one(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM messages WHERE channel_id = ?1",
            [channel_id],
            one_col,
        )
    }

    fn insert_membership(
        &self,
        channel_id: &str,
        agent_id: &str,
        read_seq: i64,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO memberships(channel_id, agent_id, read_seq) VALUES (?1, ?2, ?3)",
            params![channel_id, agent_id, read_seq],
        )?;
        Ok(())
    }

    fn delete_membership(&self, channel_id: &str, agent_id: &str) -> StoreResult<()> {
        self.exec(
            "DELETE FROM memberships WHERE channel_id = ?1 AND agent_id = ?2",
            params![channel_id, agent_id],
        )?;
        Ok(())
    }

    fn channel_summaries(&self) -> StoreResult<Vec<ChannelSummary>> {
        self.rows(
            "SELECT c.name_lower, c.topic,
                (SELECT COUNT(*) FROM memberships x WHERE x.channel_id = c.channel_id),
                (SELECT COUNT(*) FROM messages m WHERE m.channel_id = c.channel_id)
             FROM channels c ORDER BY c.name_lower",
            [],
            |r| {
                Ok(ChannelSummary {
                    name: r.get(0)?,
                    topic: r.get(1)?,
                    members: r.get(2)?,
                    messages: r.get(3)?,
                })
            },
        )
    }

    fn channel_member_status(&self, channel_id: &str) -> StoreResult<Vec<(String, i64)>> {
        self.rows(
            "SELECT u.name || '/' || a.name_lower,
                (SELECT COUNT(*) FROM messages m WHERE m.channel_id = x.channel_id AND m.commit_seq > x.read_seq)
             FROM memberships x JOIN agents a ON a.agent_id = x.agent_id JOIN users u ON u.user_id = a.owner_id
             WHERE x.channel_id = ?1 ORDER BY 1",
            [channel_id],
            pair,
        )
    }

    fn channel_message_ids(&self, channel_id: &str) -> StoreResult<Vec<String>> {
        self.rows(
            "SELECT message_id FROM messages WHERE channel_id = ?1 ORDER BY commit_seq",
            [channel_id],
            one_col,
        )
    }

    fn channel_seq_at(&self, channel_id: &str, message_id: &str) -> StoreResult<i64> {
        self.one(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM messages WHERE channel_id = ?1 AND message_id <= ?2",
            params![channel_id, message_id],
            one_col,
        )
    }

    // messages

    fn insert_message(&self, m: &NewMessage) -> StoreResult<i64> {
        self.exec(
            "INSERT INTO messages(message_id, sender_agent_id, recipient_agent_id, channel_id, text, reply_to, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![m.id, m.sender_id, m.recipient_id, m.channel_id, m.text, m.reply_to, m.created_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn set_message_page(&self, message_id: &str, kind: &str) -> StoreResult<()> {
        self.exec(
            "UPDATE messages SET page = ?2 WHERE message_id = ?1",
            params![message_id, kind],
        )?;
        Ok(())
    }

    fn pages_since(&self, sender_id: &str, since: &str) -> StoreResult<i64> {
        self.one(
            "SELECT COUNT(*) FROM messages WHERE sender_agent_id = ?1 AND page IS NOT NULL AND created_at >= ?2",
            params![sender_id, since],
            one_col,
        )
    }

    fn insert_message_if_absent(&self, m: &NewMessage) -> StoreResult<bool> {
        Ok(self.exec(
            "INSERT OR IGNORE INTO messages(message_id, sender_agent_id, recipient_agent_id, channel_id, text,
             reply_to, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![m.id, m.sender_id, m.recipient_id, m.channel_id, m.text, m.reply_to, m.created_at],
        )? > 0)
    }

    fn insert_attachment(
        &self,
        id: &str,
        message_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO attachments(attachment_id, message_id, filename, bytes, size) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, message_id, filename, bytes, bytes.len() as i64],
        )?;
        Ok(())
    }

    fn message_by_id(&self, id: &str) -> StoreResult<Option<Message>> {
        self.opt(
            &format!("{MSG_SELECT} WHERE m.message_id = ?1"),
            [id],
            msg_row,
        )
    }

    fn inbox(&self, agent_id: &str, after: i64, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.commit_seq > ?2 AND (
                (m.recipient_agent_id = ?1 AND NOT EXISTS
                    (SELECT 1 FROM dm_read d WHERE d.message_id = m.message_id AND d.agent_id = ?1))
                OR (m.sender_agent_id <> ?1 AND EXISTS (SELECT 1 FROM memberships x
                    WHERE x.channel_id = m.channel_id AND x.agent_id = ?1 AND m.commit_seq > x.read_seq)))
             ORDER BY m.commit_seq LIMIT ?3",
            params![agent_id, after, limit],
        )
    }

    fn unread_in_channel(
        &self,
        channel_id: &str,
        agent_id: &str,
        limit: i64,
    ) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.channel_id = ?1 AND m.commit_seq > (SELECT read_seq FROM memberships
                WHERE channel_id = ?1 AND agent_id = ?2) ORDER BY m.commit_seq LIMIT ?3",
            params![channel_id, agent_id, limit],
        )
    }

    fn unread_all(&self, agent_id: &str, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "(m.recipient_agent_id = ?1 AND NOT EXISTS
                (SELECT 1 FROM dm_read d WHERE d.message_id = m.message_id AND d.agent_id = ?1))
             OR EXISTS (SELECT 1 FROM memberships x
                WHERE x.channel_id = m.channel_id AND x.agent_id = ?1 AND m.commit_seq > x.read_seq)
             ORDER BY m.commit_seq LIMIT ?2",
            params![agent_id, limit],
        )
    }

    fn sent_by(&self, agent_id: &str, after: i64, limit: i64) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.sender_agent_id = ?1 AND m.commit_seq > ?2 ORDER BY m.commit_seq LIMIT ?3",
            params![agent_id, after, limit],
        )
    }

    fn first_seq_from_day(&self, day: &str) -> StoreResult<Option<i64>> {
        // each day's lowest seq heads its run in messages_by_day: one probe per day, not a walk
        Ok(self
            .opt(
                "WITH RECURSIVE days(day) AS (
                     SELECT MIN(substr(created_at, 1, 10)) FROM messages WHERE substr(created_at, 1, 10) >= ?1
                     UNION ALL
                     SELECT (SELECT MIN(substr(m.created_at, 1, 10)) FROM messages m WHERE substr(m.created_at, 1, 10) > d.day)
                     FROM days d WHERE d.day IS NOT NULL)
                 SELECT MIN((SELECT MIN(m.commit_seq) FROM messages m WHERE substr(m.created_at, 1, 10) = d.day))
                 FROM days d WHERE d.day IS NOT NULL",
                [day],
                |r| r.get::<_, Option<i64>>(0),
            )?
            .flatten())
    }

    fn search_scan(
        &self,
        agent_id: &str,
        after: i64,
        scope: SearchScope,
        from_day: Option<&str>,
        limit: i64,
    ) -> StoreResult<Vec<Message>> {
        let mine = "(m.recipient_agent_id = ?1 OR (m.sender_agent_id = ?1 AND m.recipient_agent_id IS NOT NULL)
            OR EXISTS (SELECT 1 FROM memberships x WHERE x.channel_id = m.channel_id AND x.agent_id = ?1))";
        let (filter, arg) = match scope {
            SearchScope::All => ("?4 IS NULL", None),
            SearchScope::Channel(name) => ("c.name_lower = ?4", Some(name)),
            SearchScope::Partner(id) => (
                "m.channel_id IS NULL AND (m.sender_agent_id = ?4 OR m.recipient_agent_id = ?4)",
                Some(id),
            ),
        };
        self.msgs(
            &format!(
                "m.commit_seq > ?2 AND {mine} AND {filter} AND (?5 IS NULL OR substr(m.created_at, 1, 10) >= ?5)
                 ORDER BY m.commit_seq LIMIT ?3"
            ),
            params![agent_id, after, limit, arg, from_day],
        )
    }

    fn last_seq(&self) -> StoreResult<i64> {
        self.one(
            "SELECT COALESCE(MAX(commit_seq), 0) FROM messages",
            [],
            one_col,
        )
    }

    fn feed_scan(
        &self,
        viewer: Option<&str>,
        after: i64,
        upto: i64,
        agent: Option<&str>,
        channel: Option<&str>,
        newest: bool,
        limit: i64,
    ) -> StoreResult<Vec<Message>> {
        // The page is picked from messages alone, so the walk is the primary key's; the joins
        // only dress the rows it picked.
        let order = if newest { "DESC" } else { "ASC" };
        self.msgs(
            &format!(
                "m.commit_seq IN (SELECT p.commit_seq FROM messages p
                     WHERE p.commit_seq > ?2 AND p.commit_seq <= ?3
                       AND (?1 IS NULL OR p.recipient_agent_id = ?1 OR (p.sender_agent_id = ?1 AND p.recipient_agent_id IS NOT NULL)
                            OR EXISTS (SELECT 1 FROM memberships x WHERE x.channel_id = p.channel_id AND x.agent_id = ?1))
                       AND (?4 IS NULL OR p.sender_agent_id = ?4 OR p.recipient_agent_id = ?4)
                       AND (?5 IS NULL OR p.channel_id = (SELECT channel_id FROM channels WHERE name_lower = ?5))
                     ORDER BY p.commit_seq {order} LIMIT ?6)
                 ORDER BY m.commit_seq {order}"
            ),
            params![viewer, after, upto, agent, channel, limit],
        )
    }

    fn attachment_list(&self, message_id: &str) -> StoreResult<Vec<AttachmentMeta>> {
        self.rows(
            "SELECT attachment_id, filename, size FROM attachments WHERE message_id = ?1 ORDER BY attachment_id",
            [message_id],
            |r| Ok(AttachmentMeta { id: r.get(0)?, filename: r.get(1)?, size: r.get(2)? }),
        )
    }

    fn attachment(&self, id: &str) -> StoreResult<Option<Attachment>> {
        self.opt(
            "SELECT message_id, filename, bytes FROM attachments WHERE attachment_id = ?1",
            [id],
            |r| {
                Ok(Attachment {
                    message_id: r.get(0)?,
                    filename: r.get(1)?,
                    bytes: r.get(2)?,
                })
            },
        )
    }

    fn attachment_sizes(
        &self,
        message_id: &str,
        filename: &str,
    ) -> StoreResult<Option<(i64, i64)>> {
        self.opt(
            "SELECT size, length(bytes) FROM attachments WHERE message_id = ?1 AND filename = ?2
             ORDER BY attachment_id LIMIT 1",
            params![message_id, filename],
            pair,
        )
    }

    fn reactions(&self, message_id: &str) -> StoreResult<Vec<(String, String)>> {
        self.rows(
            "SELECT x.emoji, u.name || '/' || a.name_lower FROM reactions x
             JOIN agents a ON a.agent_id = x.actor_id JOIN users u ON u.user_id = a.owner_id
             WHERE x.message_id = ?1 AND x.present = 1 ORDER BY x.emoji, 2",
            [message_id],
            pair,
        )
    }

    fn set_reaction(
        &self,
        message_id: &str,
        actor_id: &str,
        emoji: &str,
        present: bool,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO reactions(message_id, actor_id, emoji, present) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(message_id, actor_id, emoji) DO UPDATE SET present = excluded.present",
            params![message_id, actor_id, emoji, present],
        )?;
        Ok(())
    }

    fn insert_reaction_if_absent(
        &self,
        message_id: &str,
        actor_id: &str,
        emoji: &str,
        present: bool,
    ) -> StoreResult<bool> {
        Ok(self.exec(
            "INSERT OR IGNORE INTO reactions(message_id, actor_id, emoji, present) VALUES (?1, ?2, ?3, ?4)",
            params![message_id, actor_id, emoji, present],
        )? > 0)
    }

    fn mark_dm_read(&self, message_id: &str, agent_id: &str, at: &str) -> StoreResult<bool> {
        Ok(self.exec(
            "INSERT OR IGNORE INTO dm_read(message_id, agent_id, read_at) VALUES (?1, ?2, ?3)",
            params![message_id, agent_id, at],
        )? > 0)
    }

    fn dm_read_at(&self, message_id: &str, agent_id: &str) -> StoreResult<Option<String>> {
        self.opt(
            "SELECT read_at FROM dm_read WHERE message_id = ?1 AND agent_id = ?2",
            params![message_id, agent_id],
            one_col,
        )
    }

    fn channel_read_count(
        &self,
        channel_id: &str,
        except_agent: &str,
        seq: i64,
    ) -> StoreResult<(i64, i64)> {
        self.one(
            "SELECT COALESCE(SUM(read_seq >= ?3), 0), COUNT(*) FROM memberships
             WHERE channel_id = ?1 AND agent_id <> ?2",
            params![channel_id, except_agent, seq],
            pair,
        )
    }

    fn message_count(&self) -> StoreResult<i64> {
        self.one("SELECT COUNT(*) FROM messages", [], one_col)
    }

    // notifications and attempts

    fn insert_notification(&self, n: &NewNotification) -> StoreResult<()> {
        self.exec(
            "INSERT OR IGNORE INTO notifications(notification_id, message_id, recipient_agent_id, kind,
             binding_version, status, next_attempt_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![n.id, n.message_id, n.recipient_id, n.kind, n.binding_version, n.next_attempt_at],
        )?;
        self.wakes
            .borrow_mut()
            .notifications
            .insert(n.id.to_string());
        Ok(())
    }

    fn due_notifications(
        &self,
        now: i64,
        host: &str,
        limit: i64,
    ) -> StoreResult<Vec<DueNotification>> {
        self.rows(
            "SELECT n.notification_id, n.message_id, n.recipient_agent_id, n.kind
             FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
             WHERE n.status = 'pending' AND n.next_attempt_at <= ?1 AND a.host_id = ?2
             ORDER BY n.next_attempt_at, n.rowid LIMIT ?3",
            params![now, host, limit],
            |r| {
                Ok(DueNotification {
                    id: r.get(0)?,
                    message_id: r.get(1)?,
                    recipient_id: r.get(2)?,
                    kind: r.get(3)?,
                })
            },
        )
    }

    fn slack_due(&self, now: i64, since: i64, limit: i64) -> StoreResult<Vec<SlackDue>> {
        // A human's notification keeps its creation time in next_attempt_at: no bridge attempts it.
        self.rows(
            "SELECT n.notification_id, n.message_id, n.recipient_agent_id, a.owner_id, LOWER(u.name)
             FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
             JOIN users u ON u.user_id = a.owner_id JOIN messages m ON m.message_id = n.message_id
             WHERE n.status IN ('pending', 'badge') AND n.next_attempt_at BETWEEN ?2 AND ?1
               AND n.kind = 'dm' AND m.page IS NOT NULL AND u.slack_member_id IS NOT NULL
               AND a.address_kind = 'human' AND m.created_at >= ?3
               AND NOT EXISTS (SELECT 1 FROM dm_read r WHERE r.message_id = m.message_id AND r.agent_id = a.agent_id)
               AND NOT EXISTS (SELECT 1 FROM agents a2
                               JOIN notifications n2 ON n2.recipient_agent_id = a2.agent_id AND n2.message_id = n.message_id
                               JOIN attempts t ON t.notification_id = n2.notification_id
                               WHERE a2.owner_id = a.owner_id AND t.bridge_instance = ?5
                                 AND (t.status IN ('delivered', 'refused', 'dropped') OR t.not_before > ?1))
             ORDER BY n.rowid LIMIT ?4",
            params![now, since, crate::util::iso_from_ms(since), limit, super::SLACK_INSTANCE],
            |r| {
                Ok(SlackDue {
                    id: r.get(0)?,
                    message_id: r.get(1)?,
                    recipient_id: r.get(2)?,
                    owner_id: r.get(3)?,
                    owner: r.get(4)?,
                })
            },
        )
    }

    fn slack_waiting(&self, now: i64, since: i64) -> StoreResult<Vec<(i64, String)>> {
        self.rows(
            "SELECT a.owner_id, MAX(n.message_id)
             FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
             JOIN users u ON u.user_id = a.owner_id JOIN messages m ON m.message_id = n.message_id
             WHERE n.status IN ('pending', 'badge') AND n.next_attempt_at BETWEEN ?2 AND ?1
                   AND n.kind = 'dm' AND m.page IS NOT NULL AND a.address_kind = 'human' AND m.created_at >= ?3
                   AND u.slack_member_id IS NULL
                   AND NOT EXISTS (SELECT 1 FROM dm_read r WHERE r.message_id = m.message_id AND r.agent_id = a.agent_id)
             GROUP BY a.owner_id ORDER BY a.owner_id",
            params![now, since, crate::util::iso_from_ms(since)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    fn slack_attempt(&self, message_id: &str) -> StoreResult<Option<(String, Option<String>)>> {
        self.opt(
            "SELECT t.status, t.reason FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             WHERE n.message_id = ?1 AND t.bridge_instance = ?2 ORDER BY t.rowid DESC LIMIT 1",
            params![message_id, super::SLACK_INSTANCE],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    fn slack_abandon(&self, message_id: &str, owner: &str, at: &str) -> StoreResult<u64> {
        Ok(self.exec(
            "UPDATE attempts SET status = 'abandoned', reason = 'lease expired', settled_at = ?3
             WHERE bridge_instance = ?4 AND status = 'dispatching' AND notification_id IN (
                 SELECT n.notification_id FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
                 JOIN users u ON u.user_id = a.owner_id WHERE n.message_id = ?1 AND LOWER(u.name) = ?2)",
            params![message_id, owner, at, super::SLACK_INSTANCE],
        )? as u64)
    }

    fn set_attempt_not_before(&self, id: &str, not_before: i64) -> StoreResult<()> {
        self.exec(
            "UPDATE attempts SET not_before = ?2 WHERE attempt_id = ?1",
            params![id, not_before],
        )?;
        Ok(())
    }

    fn attempt_not_before(&self, id: &str) -> StoreResult<Option<i64>> {
        Ok(self
            .opt(
                "SELECT not_before FROM attempts WHERE attempt_id = ?1",
                [id],
                |r| r.get::<_, Option<i64>>(0),
            )?
            .flatten())
    }

    fn set_notification_status(&self, id: &str, status: &str) -> StoreResult<()> {
        self.settle_notification(id, status, None)
    }

    fn settle_read_in_channel(
        &self,
        channel_id: &str,
        recipient_id: &str,
        cursor: i64,
    ) -> StoreResult<u64> {
        Ok(self.exec(
            "UPDATE notifications SET status = 'read'
             WHERE recipient_agent_id = ?2 AND status IN ('pending', 'dispatching', 'notified', 'held')
               AND message_id IN (SELECT message_id FROM messages WHERE channel_id = ?1 AND commit_seq <= ?3)",
            params![channel_id, recipient_id, cursor],
        )? as u64)
    }

    fn settle_read(&self, message_id: &str, recipient_id: &str) -> StoreResult<bool> {
        Ok(self.exec(
            "UPDATE notifications SET status = 'read'
             WHERE message_id = ?1 AND recipient_agent_id = ?2 AND status IN ('pending', 'dispatching', 'notified', 'held')",
            params![message_id, recipient_id],
        )? > 0)
    }

    fn settle_notification(
        &self,
        id: &str,
        status: &str,
        next_attempt_at: Option<i64>,
    ) -> StoreResult<()> {
        self.exec(
            "UPDATE notifications SET status = ?2, next_attempt_at = COALESCE(?3, next_attempt_at)
             WHERE notification_id = ?1",
            params![id, status, next_attempt_at],
        )?;
        if status == "pending" {
            self.wakes.borrow_mut().notifications.insert(id.to_string());
        }
        Ok(())
    }

    fn dm_delivery(&self, message_id: &str, recipient: &str) -> StoreResult<Option<DmDelivery>> {
        self.opt(
            "SELECT n.status, t.status, t.reason FROM notifications n
             LEFT JOIN attempts t ON t.rowid = (SELECT MAX(rowid) FROM attempts WHERE notification_id = n.notification_id AND bridge_instance <> 'slack')
             WHERE n.message_id = ?1 AND n.recipient_agent_id = ?2 ORDER BY n.notification_id LIMIT 1",
            params![message_id, recipient],
            |r| Ok(DmDelivery { status: r.get(0)?, last_attempt: r.get(1)?, reason: r.get(2)? }),
        )
    }

    fn unread_badges(&self, agent_id: &str) -> StoreResult<Vec<(String, i64)>> {
        self.rows(
            "SELECT c.name_lower, COUNT(m.commit_seq) FROM memberships x
             JOIN channels c ON c.channel_id = x.channel_id
             JOIN messages m ON m.channel_id = x.channel_id AND m.commit_seq > x.read_seq
                AND m.sender_agent_id <> x.agent_id
             WHERE x.agent_id = ?1 GROUP BY c.name_lower ORDER BY c.name_lower",
            [agent_id],
            pair,
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
        self.exec(
            "INSERT INTO attempts(attempt_id, notification_id, bridge_instance, target_binding_version, status,
                                  host_id, text)
             VALUES (?1, ?2, ?3, ?4, 'dispatching', ?5, ?6)",
            params![id, notification_id, bridge_instance, target_binding_version, host, text],
        )?;
        Ok(())
    }

    fn held_attempts(&self, bridge_instance: &str) -> StoreResult<Vec<(String, String)>> {
        self.rows(
            "SELECT t.attempt_id, a.address_kind || ':' || a.address_target || '@' || a.host_id
             FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id
             WHERE t.status = 'held' AND t.bridge_instance = ?1 AND a.address_kind IS NOT NULL
               AND n.status IN ('pending', 'dispatching', 'notified', 'held')
             ORDER BY t.rowid",
            [bridge_instance],
            pair,
        )
    }

    fn open_attempts(&self, bridge_instance: &str) -> StoreResult<Vec<OpenAttempt>> {
        self.rows(
            "SELECT t.attempt_id, t.notification_id,
                    a.address_kind || ':' || a.address_target || '@' || a.host_id, t.text,
                    u.name || '/' || a.name_lower, n.message_id, t.target_binding_version, n.kind,
                    n.recipient_agent_id
             FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id JOIN users u ON u.user_id = a.owner_id
             WHERE t.bridge_instance = ?1 AND t.status IN ('dispatching', 'held')
               AND t.text IS NOT NULL AND a.address_kind IS NOT NULL AND n.status IN ('pending', 'dispatching', 'notified', 'held')
             ORDER BY t.rowid",
            [bridge_instance],
            |r| {
                Ok(OpenAttempt {
                    attempt_id: r.get(0)?,
                    notification_id: r.get(1)?,
                    address: r.get(2)?,
                    text: r.get(3)?,
                    recipient: r.get(4)?,
                    message_id: r.get(5)?,
                    binding_version: r.get(6)?,
                    kind: r.get(7)?,
                    recipient_id: r.get(8)?,
                })
            },
        )
    }

    fn stale_attempts(&self, bridge_instance: &str, host: &str) -> StoreResult<Vec<StaleAttempt>> {
        self.rows(
            "SELECT t.attempt_id, CASE WHEN t.bridge_instance <> ?1 THEN 'bridge instance gone'
                                       ELSE 'recipient rebound' END,
                    n.notification_id, n.message_id, n.recipient_agent_id
             FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id
             WHERE (t.status IN ('held', 'dispatching') OR (t.status = 'accepted' AND n.status = 'notified'))
               AND n.status IN ('pending', 'dispatching', 'notified', 'held')
               AND (a.host_id = ?2 OR a.host_id IS NULL)
               AND (t.bridge_instance <> ?1 OR t.target_binding_version <> a.binding_version)
               AND t.bridge_instance <> ?3
             ORDER BY t.rowid",
            params![bridge_instance, host, super::SLACK_INSTANCE],
            |r| {
                Ok(StaleAttempt {
                    attempt_id: r.get(0)?,
                    reason: r.get(1)?,
                    notification_id: r.get(2)?,
                    message_id: r.get(3)?,
                    recipient_id: r.get(4)?,
                })
            },
        )
    }

    fn gone_holds(&self, bridge_instance: &str, host: &str) -> StoreResult<Vec<String>> {
        self.rows(
            "SELECT t.attempt_id FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id
             WHERE t.bridge_instance = ?1 AND t.status = 'held' AND a.address_kind IN ('claude', 'mailbox')
               AND a.host_id = ?2 AND n.status IN ('pending', 'dispatching', 'notified', 'held')
               AND NOT EXISTS (SELECT 1 FROM endpoints e
                               WHERE e.host_id = a.host_id AND e.kind = a.address_kind AND e.target = a.address_target)
             ORDER BY t.rowid",
            params![bridge_instance, host],
            one_col,
        )
    }

    fn count_dispatching(&self, bridge_instance: &str) -> StoreResult<i64> {
        self.one(
            "SELECT COUNT(*) FROM attempts t JOIN notifications n ON n.notification_id = t.notification_id
             WHERE t.bridge_instance = ?1 AND t.status = 'dispatching' AND n.status IN ('pending', 'dispatching', 'notified', 'held')",
            [bridge_instance],
            one_col,
        )
    }

    fn attempt(&self, id: &str) -> StoreResult<Option<(String, String)>> {
        self.opt(
            "SELECT notification_id, status FROM attempts WHERE attempt_id = ?1",
            [id],
            pair,
        )
    }

    fn attempt_route(&self, id: &str) -> StoreResult<Option<(i64, i64)>> {
        self.opt(
            "SELECT t.target_binding_version, a.binding_version FROM attempts t
             JOIN notifications n ON n.notification_id = t.notification_id
             JOIN agents a ON a.agent_id = n.recipient_agent_id WHERE t.attempt_id = ?1",
            [id],
            pair,
        )
    }

    fn attempt_target(&self, id: &str) -> StoreResult<Option<(String, String, String)>> {
        self.opt(
            "SELECT n.notification_id, n.message_id, n.recipient_agent_id FROM attempts t
             JOIN notifications n ON n.notification_id = t.notification_id WHERE t.attempt_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    fn attempt_owner(&self, id: &str) -> StoreResult<Option<(String, Option<String>, i64)>> {
        self.opt(
            "SELECT bridge_instance, host_id, last_event_seq FROM attempts WHERE attempt_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    fn update_attempt(
        &self,
        id: &str,
        status: &str,
        reason: Option<&str>,
        event_seq: Option<i64>,
        at: &str,
    ) -> StoreResult<()> {
        self.exec(
            "UPDATE attempts SET status = ?2, reason = ?3, last_event_seq = COALESCE(?4, last_event_seq),
             settled_at = ?5 WHERE attempt_id = ?1",
            params![id, status, reason, event_seq, at],
        )?;
        Ok(())
    }

    fn latest_attempt(&self, notification_id: &str) -> StoreResult<Option<String>> {
        self.opt(
            "SELECT attempt_id FROM attempts WHERE notification_id = ?1 AND bridge_instance <> 'slack'
             ORDER BY rowid DESC LIMIT 1",
            [notification_id],
            one_col,
        )
    }

    fn notifications(&self) -> StoreResult<Vec<NotificationRow>> {
        self.rows(
            "SELECT n.notification_id, n.message_id, n.recipient_agent_id, u.name || '/' || a.name_lower, n.kind,
                n.status, n.next_attempt_at
             FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
             JOIN users u ON u.user_id = a.owner_id ORDER BY n.notification_id",
            [],
            |r| {
                Ok(NotificationRow {
                    id: r.get(0)?,
                    message_id: r.get(1)?,
                    recipient_id: r.get(2)?,
                    recipient_name: r.get(3)?,
                    kind: r.get(4)?,
                    status: r.get(5)?,
                    next_attempt_at: r.get(6)?,
                })
            },
        )
    }

    fn attempts(&self) -> StoreResult<Vec<AttemptRow>> {
        self.rows(
            "SELECT attempt_id, notification_id, bridge_instance, target_binding_version, last_event_seq, status,
                reason FROM attempts ORDER BY rowid",
            [],
            |r| {
                Ok(AttemptRow {
                    id: r.get(0)?,
                    notification_id: r.get(1)?,
                    bridge_instance: r.get(2)?,
                    target_binding_version: r.get(3)?,
                    last_event_seq: r.get(4)?,
                    status: r.get(5)?,
                    reason: r.get(6)?,
                })
            },
        )
    }

    // bridge instances and endpoints

    fn bridge_instance(&self, instance: &str) -> StoreResult<Option<(String, String)>> {
        self.opt(
            "SELECT host_id, state FROM bridge_instances WHERE instance = ?1",
            [instance],
            pair,
        )
    }

    fn active_bridges(&self, host: &str) -> StoreResult<Vec<String>> {
        self.rows(
            "SELECT instance FROM bridge_instances WHERE host_id = ?1 AND state = 'active' ORDER BY instance",
            [host],
            one_col,
        )
    }

    fn fence(&self, host: &str, instance: &str, at: &str) -> StoreResult<Vec<String>> {
        let fenced: Vec<String> = self.rows(
            "SELECT instance FROM bridge_instances WHERE host_id = ?1 AND state = 'active' AND instance <> ?2
             ORDER BY instance",
            params![host, instance],
            one_col,
        )?;
        self.exec(
            "UPDATE bridge_instances SET state = 'fenced', updated_at = ?3
             WHERE host_id = ?1 AND state = 'active' AND instance <> ?2",
            params![host, instance, at],
        )?;
        self.exec(
            "INSERT INTO bridge_instances(instance, host_id, state, updated_at) VALUES (?1, ?2, 'active', ?3)
             ON CONFLICT(instance) DO UPDATE SET state = 'active', updated_at = ?3",
            params![instance, host, at],
        )?;
        self.wakes
            .borrow_mut()
            .fenced
            .extend(fenced.iter().cloned());
        Ok(fenced)
    }

    fn fence_host(&self, host: &str, at: &str) -> StoreResult<Vec<String>> {
        let fenced: Vec<String> = self.rows(
            "SELECT instance FROM bridge_instances WHERE host_id = ?1 AND state = 'active' ORDER BY instance",
            [host],
            one_col,
        )?;
        self.exec(
            "UPDATE bridge_instances SET state = 'fenced', updated_at = ?2 WHERE host_id = ?1 AND state = 'active'",
            params![host, at],
        )?;
        self.wakes
            .borrow_mut()
            .fenced
            .extend(fenced.iter().cloned());
        Ok(fenced)
    }

    fn replace_endpoints(
        &self,
        host: &str,
        instance: &str,
        endpoints: &[(&str, &str)],
        at: &str,
    ) -> StoreResult<()> {
        self.exec("DELETE FROM endpoints WHERE host_id = ?1", [host])?;
        for (kind, target) in endpoints {
            self.exec(
                "INSERT OR REPLACE INTO endpoints(host_id, kind, target, bridge_instance, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![host, kind, target, instance, at],
            )?;
        }
        Ok(())
    }

    fn count_endpoints(&self) -> StoreResult<i64> {
        self.one("SELECT COUNT(*) FROM endpoints", [], one_col)
    }

    // web UI: owner-scoped reads

    fn owner_agents(&self, owner_id: i64) -> StoreResult<Vec<Agent>> {
        self.rows(
            &format!("{AGENT_SELECT} WHERE a.owner_id = ?1 ORDER BY a.name_lower"),
            [owner_id],
            agent_row,
        )
    }

    fn owner_dm_threads(&self, owner_id: i64) -> StoreResult<Vec<DmThread>> {
        self.rows(
            "SELECT p.me, au.name || '/' || a.name_lower, p.partner, pu.name || '/' || pa.name_lower,
                    p.last_seq, lm.created_at, p.unread
             FROM (SELECT x.me, x.partner, MAX(x.seq) AS last_seq, SUM(x.unread) AS unread FROM (
                       SELECT m.sender_agent_id AS me, m.recipient_agent_id AS partner, m.commit_seq AS seq, 0 AS unread
                       FROM messages m JOIN agents o ON o.agent_id = m.sender_agent_id
                       WHERE m.channel_id IS NULL AND o.owner_id = ?1
                       UNION ALL
                       SELECT m.recipient_agent_id, m.sender_agent_id, m.commit_seq,
                              CASE WHEN EXISTS (SELECT 1 FROM dm_read d WHERE d.message_id = m.message_id
                                  AND d.agent_id = m.recipient_agent_id) THEN 0 ELSE 1 END
                       FROM messages m JOIN agents o ON o.agent_id = m.recipient_agent_id
                       WHERE m.channel_id IS NULL AND o.owner_id = ?1) x
                   GROUP BY x.me, x.partner) p
             JOIN agents a ON a.agent_id = p.me JOIN users au ON au.user_id = a.owner_id
             JOIN agents pa ON pa.agent_id = p.partner JOIN users pu ON pu.user_id = pa.owner_id
             JOIN messages lm ON lm.commit_seq = p.last_seq
             WHERE a.owner_id = ?1
             ORDER BY p.last_seq DESC, p.me",
            [owner_id],
            |r| {
                Ok(DmThread {
                    agent_id: r.get(0)?,
                    agent: r.get(1)?,
                    partner_id: r.get(2)?,
                    partner: r.get(3)?,
                    last_seq: r.get(4)?,
                    last_at: r.get(5)?,
                    unread: r.get(6)?,
                })
            },
        )
    }

    fn owner_channel_threads(&self, owner_id: i64) -> StoreResult<Vec<ChannelThread>> {
        self.rows(
            "SELECT a.agent_id, u.name || '/' || a.name_lower, c.name_lower, c.topic,
                    COALESCE((SELECT MAX(commit_seq) FROM messages WHERE channel_id = c.channel_id), 0),
                    (SELECT created_at FROM messages WHERE channel_id = c.channel_id
                     ORDER BY commit_seq DESC LIMIT 1),
                    (SELECT COUNT(*) FROM messages m WHERE m.channel_id = c.channel_id
                     AND m.commit_seq > x.read_seq AND m.sender_agent_id <> x.agent_id)
             FROM memberships x JOIN channels c ON c.channel_id = x.channel_id
             JOIN agents a ON a.agent_id = x.agent_id JOIN users u ON u.user_id = a.owner_id
             WHERE a.owner_id = ?1 ORDER BY a.name_lower, c.name_lower",
            [owner_id],
            |r| {
                Ok(ChannelThread {
                    agent_id: r.get(0)?,
                    agent: r.get(1)?,
                    name: r.get(2)?,
                    topic: r.get(3)?,
                    last_seq: r.get(4)?,
                    last_at: r.get(5)?,
                    unread: r.get(6)?,
                })
            },
        )
    }

    fn owner_dm_history(
        &self,
        owner_id: i64,
        agent_id: &str,
        partner_id: &str,
        before: i64,
        limit: i64,
    ) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.channel_id IS NULL AND ((m.sender_agent_id = ?2 AND m.recipient_agent_id = ?3)
                OR (m.sender_agent_id = ?3 AND m.recipient_agent_id = ?2))
             AND EXISTS (SELECT 1 FROM agents o WHERE o.agent_id = ?2 AND o.owner_id = ?1)
             AND m.commit_seq < ?4 ORDER BY m.commit_seq DESC LIMIT ?5",
            params![owner_id, agent_id, partner_id, before, limit],
        )
    }

    fn owner_channel_history(
        &self,
        owner_id: i64,
        channel_id: &str,
        before: i64,
        limit: i64,
    ) -> StoreResult<Vec<Message>> {
        self.msgs(
            "m.channel_id = ?2 AND EXISTS (SELECT 1 FROM memberships x JOIN agents o ON o.agent_id = x.agent_id
                WHERE x.channel_id = ?2 AND o.owner_id = ?1)
             AND m.commit_seq < ?3 ORDER BY m.commit_seq DESC LIMIT ?4",
            params![owner_id, channel_id, before, limit],
        )
    }

    fn owner_message(&self, owner_id: i64, message_id: &str) -> StoreResult<Option<Message>> {
        self.opt(
            &format!(
                "{MSG_SELECT} WHERE m.message_id = ?2 AND (
                    (m.channel_id IS NULL AND (s.owner_id = ?1 OR r.owner_id = ?1))
                    OR (m.channel_id IS NOT NULL AND EXISTS (SELECT 1 FROM memberships x
                        JOIN agents o ON o.agent_id = x.agent_id
                        WHERE x.channel_id = m.channel_id AND o.owner_id = ?1)))"
            ),
            params![owner_id, message_id],
            msg_row,
        )
    }

    fn owner_memberships(&self, owner_id: i64) -> StoreResult<Vec<Membership>> {
        self.rows(
            "SELECT a.agent_id, u.name || '/' || a.name_lower, c.channel_id, c.name_lower
             FROM memberships x JOIN agents a ON a.agent_id = x.agent_id JOIN users u ON u.user_id = a.owner_id
             JOIN channels c ON c.channel_id = x.channel_id
             WHERE a.owner_id = ?1 ORDER BY a.name_lower, c.name_lower",
            [owner_id],
            |r| Ok(Membership { agent_id: r.get(0)?, agent: r.get(1)?, channel_id: r.get(2)?, channel: r.get(3)? }),
        )
    }

    fn owner_feed(
        &self,
        owner_id: i64,
        ranges: &[FeedRange],
        after: i64,
        before: i64,
        limit: i64,
        oldest_first: bool,
    ) -> StoreResult<Vec<Message>> {
        let Some((seqs, args)) =
            feed_seqs_sql(owner_id, ranges, after, before, limit, oldest_first, |n| {
                format!("?{n}")
            })
        else {
            return Ok(Vec::new());
        };
        let args = args.into_iter().map(|a| match a {
            FeedArg::Int(n) => rusqlite::types::Value::Integer(n),
            FeedArg::Text(t) => rusqlite::types::Value::Text(t),
        });
        let order = if oldest_first { "ASC" } else { "DESC" };
        self.msgs(
            &format!("m.commit_seq IN ({seqs}) ORDER BY m.commit_seq {order}"),
            params_from_iter(args),
        )
    }

    fn owner_problem_seqs(&self, owner_id: i64, after: i64, before: i64) -> StoreResult<Vec<i64>> {
        self.rows(
            &problem_seqs_sql(|n| format!("?{n}")),
            params![owner_id, after, before],
            one_col,
        )
    }

    fn owner_thread_up(
        &self,
        owner_id: i64,
        id: &str,
    ) -> StoreResult<Vec<(i64, String, Option<String>)>> {
        self.rows(
            &thread_up_sql(|n| format!("?{n}")),
            params![owner_id, id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    fn owner_thread_down(
        &self,
        owner_id: i64,
        top: &str,
        after: i64,
        limit: i64,
    ) -> StoreResult<Vec<i64>> {
        self.rows(
            &thread_down_sql(|n| format!("?{n}")),
            params![owner_id, top, after, limit],
            one_col,
        )
    }

    fn messages_at(&self, seqs: &[i64]) -> StoreResult<Vec<Message>> {
        if seqs.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; seqs.len()].join(", ");
        self.msgs(
            &format!("m.commit_seq IN ({marks}) ORDER BY m.commit_seq DESC"),
            params_from_iter(seqs),
        )
    }

    fn owner_search_scan(
        &self,
        owner_id: i64,
        after: i64,
        scope: SearchScope,
        from_day: Option<&str>,
        limit: i64,
    ) -> StoreResult<Vec<Message>> {
        let owned = "((m.channel_id IS NULL AND (s.owner_id = ?1 OR r.owner_id = ?1))
            OR (m.channel_id IS NOT NULL AND EXISTS (SELECT 1 FROM memberships x
                JOIN agents o ON o.agent_id = x.agent_id WHERE x.channel_id = m.channel_id AND o.owner_id = ?1)))";
        let (filter, arg) = match scope {
            SearchScope::All => ("?4 IS NULL", None),
            SearchScope::Channel(name) => ("c.name_lower = ?4", Some(name)),
            SearchScope::Partner(id) => (
                "m.channel_id IS NULL AND (m.sender_agent_id = ?4 OR m.recipient_agent_id = ?4)",
                Some(id),
            ),
        };
        self.msgs(
            &format!(
                "m.commit_seq > ?2 AND {owned} AND {filter} AND (?5 IS NULL OR substr(m.created_at, 1, 10) >= ?5)
                 ORDER BY m.commit_seq LIMIT ?3"
            ),
            params![owner_id, after, limit, arg, from_day],
        )
    }

    fn insert_mcp_code(&self, code_digest: &str, c: &McpCodeRow) -> StoreResult<()> {
        self.exec(
            "INSERT INTO mcp_codes(code_digest, user_id, client_id, client_name, redirect_uri, challenge, resource, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![code_digest, c.user_id, c.client_id, c.client_name, c.redirect_uri, c.challenge, c.resource, c.created_at],
        )?;
        Ok(())
    }

    fn take_mcp_code(&self, code_digest: &str) -> StoreResult<Option<McpCodeRow>> {
        self.opt(
            "DELETE FROM mcp_codes WHERE code_digest = ?1
             RETURNING user_id, client_id, client_name, redirect_uri, challenge, resource, created_at",
            [code_digest],
            |r| {
                Ok(McpCodeRow {
                    user_id: r.get(0)?,
                    client_id: r.get(1)?,
                    client_name: r.get(2)?,
                    redirect_uri: r.get(3)?,
                    challenge: r.get(4)?,
                    resource: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        )
    }

    fn insert_mcp_grant(&self, g: &McpGrantRow) -> StoreResult<()> {
        self.exec(
            "INSERT INTO mcp_grants(grant_id, user_id, client_id, client_host, client_name, resource, created_at,
                                    last_used_at, expires_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                g.grant_id, g.user_id, g.client_id, g.client_host, g.client_name, g.resource, g.created_at,
                g.last_used_at, g.expires_at, g.revoked_at
            ],
        )?;
        Ok(())
    }

    fn insert_mcp_token(
        &self,
        token_digest: &str,
        grant_id: &str,
        audience: &str,
        expires_at: i64,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO mcp_tokens(token_digest, grant_id, audience, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![token_digest, grant_id, audience, expires_at],
        )?;
        Ok(())
    }

    fn insert_mcp_refresh(
        &self,
        digest: &str,
        grant_id: &str,
        issued_at: i64,
        expires_at: i64,
    ) -> StoreResult<()> {
        self.exec(
            "INSERT INTO mcp_refresh(digest, grant_id, issued_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![digest, grant_id, issued_at, expires_at],
        )?;
        Ok(())
    }

    fn mcp_grant(&self, grant_id: &str) -> StoreResult<Option<McpGrantRow>> {
        self.opt(
            &format!("{MCP_GRANT_SELECT} WHERE grant_id = ?1"),
            [grant_id],
            mcp_grant_row,
        )
    }

    fn mcp_token(&self, token_digest: &str) -> StoreResult<Option<(String, String, i64)>> {
        self.opt(
            "SELECT grant_id, audience, expires_at FROM mcp_tokens WHERE token_digest = ?1",
            [token_digest],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    fn mcp_refresh(&self, digest: &str) -> StoreResult<Option<McpRefreshRow>> {
        self.opt(
            "SELECT grant_id, issued_at, expires_at, consumed_at FROM mcp_refresh WHERE digest = ?1",
            [digest],
            |r| Ok(McpRefreshRow { grant_id: r.get(0)?, issued_at: r.get(1)?, expires_at: r.get(2)?, consumed_at: r.get(3)? }),
        )
    }

    fn consume_mcp_refresh(&self, digest: &str, at: i64) -> StoreResult<bool> {
        let n = self.exec(
            "UPDATE mcp_refresh SET consumed_at = ?2 WHERE digest = ?1 AND consumed_at IS NULL",
            params![digest, at],
        )?;
        Ok(n == 1)
    }

    fn touch_mcp_grant(&self, grant_id: &str, at: i64) -> StoreResult<()> {
        self.exec(
            "UPDATE mcp_grants SET last_used_at = ?2 WHERE grant_id = ?1",
            params![grant_id, at],
        )?;
        Ok(())
    }

    fn revoke_mcp_grant(&self, grant_id: &str, at: i64) -> StoreResult<bool> {
        let n = self.exec(
            "UPDATE mcp_grants SET revoked_at = ?2 WHERE grant_id = ?1 AND revoked_at IS NULL",
            params![grant_id, at],
        )?;
        self.exec("DELETE FROM mcp_tokens WHERE grant_id = ?1", [grant_id])?;
        self.exec("DELETE FROM mcp_refresh WHERE grant_id = ?1", [grant_id])?;
        Ok(n == 1)
    }

    fn mcp_code(&self, code_digest: &str) -> StoreResult<Option<McpCodeRow>> {
        self.opt(
            "SELECT user_id, client_id, client_name, redirect_uri, challenge, resource, created_at
             FROM mcp_codes WHERE code_digest = ?1",
            [code_digest],
            |r| {
                Ok(McpCodeRow {
                    user_id: r.get(0)?,
                    client_id: r.get(1)?,
                    client_name: r.get(2)?,
                    redirect_uri: r.get(3)?,
                    challenge: r.get(4)?,
                    resource: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        )
    }

    fn set_mcp_grant_code(&self, grant_id: &str, code_digest: &str) -> StoreResult<()> {
        self.exec(
            "UPDATE mcp_grants SET code_digest = ?2 WHERE grant_id = ?1",
            params![grant_id, code_digest],
        )?;
        Ok(())
    }

    fn mcp_grant_of_code(&self, code_digest: &str) -> StoreResult<Option<String>> {
        self.opt(
            "SELECT grant_id FROM mcp_grants WHERE code_digest = ?1",
            [code_digest],
            |r| r.get(0),
        )
    }

    fn mcp_grants_of(&self, user_id: i64) -> StoreResult<Vec<McpGrantRow>> {
        self.rows(
            &format!("{MCP_GRANT_SELECT} WHERE user_id = ?1 ORDER BY created_at DESC, grant_id"),
            [user_id],
            mcp_grant_row,
        )
    }

    fn purge_mcp(&self, now: i64, code_ttl: i64) -> StoreResult<()> {
        self.exec(
            "DELETE FROM mcp_codes WHERE created_at < ?1 - ?2",
            params![now, code_ttl],
        )?;
        self.exec("DELETE FROM mcp_tokens WHERE expires_at < ?1", [now])?;
        // A consumed refresh token stays while its grant lives, so a replay is still recognised.
        self.exec(
            "DELETE FROM mcp_refresh WHERE consumed_at IS NULL AND expires_at < ?1",
            [now],
        )?;
        self.exec(
            "DELETE FROM mcp_refresh WHERE grant_id IN
                 (SELECT grant_id FROM mcp_grants WHERE expires_at < ?1 OR revoked_at IS NOT NULL)",
            [now],
        )?;
        self.exec(
            "DELETE FROM mcp_grants WHERE expires_at < ?1 OR revoked_at IS NOT NULL",
            [now],
        )?;
        Ok(())
    }

    fn deliveries(&self, message_id: &str) -> StoreResult<Vec<Delivery>> {
        // Notification ids are `<message id>.<recipient id>`: the primary key serves the lookup.
        self.rows(
            "SELECT n.recipient_agent_id, ru.name || '/' || r.name_lower, n.kind, n.status, n.next_attempt_at,
                    (SELECT COUNT(*) FROM attempts WHERE notification_id = n.notification_id AND bridge_instance <> 'slack'), t.status, t.reason
             FROM notifications n
             JOIN agents r ON r.agent_id = n.recipient_agent_id JOIN users ru ON ru.user_id = r.owner_id
             LEFT JOIN attempts t ON t.rowid = (SELECT MAX(rowid) FROM attempts WHERE notification_id = n.notification_id AND bridge_instance <> 'slack')
             WHERE n.notification_id > ?1 || '.' AND n.notification_id < ?1 || '/' AND n.message_id = ?1
             ORDER BY 2",
            [message_id],
            |r| {
                Ok(Delivery {
                    recipient_id: r.get(0)?,
                    recipient: r.get(1)?,
                    kind: r.get(2)?,
                    status: r.get(3)?,
                    next_attempt_at: r.get(4)?,
                    attempts: r.get(5)?,
                    last_status: r.get(6)?,
                    last_reason: r.get(7)?,
                })
            },
        )
    }

    fn host_bridges(&self) -> StoreResult<Vec<HostBridge>> {
        self.rows(
            "SELECT h.host_id, u.name,
                    (SELECT MAX(updated_at) FROM bridge_instances b WHERE b.host_id = h.host_id AND b.state = 'active'),
                    (SELECT MAX(updated_at) FROM bridge_instances b WHERE b.host_id = h.host_id)
             FROM hosts h JOIN users u ON u.user_id = h.user_id ORDER BY u.name, h.host_id",
            [],
            |r| Ok(HostBridge { host: r.get(0)?, owner: r.get(1)?, heartbeat: r.get(2)?, last_seen: r.get(3)? }),
        )
    }

    fn endpoint_rows(&self) -> StoreResult<Vec<EndpointRow>> {
        self.rows(
            "SELECT host_id, kind, target, seen_at FROM endpoints ORDER BY host_id, kind, target",
            [],
            |r| {
                Ok(EndpointRow {
                    host: r.get(0)?,
                    kind: r.get(1)?,
                    target: r.get(2)?,
                    seen_at: r.get(3)?,
                })
            },
        )
    }

    fn agent_health(&self, owner_id: i64, stuck_before: &str) -> StoreResult<Vec<AgentHealth>> {
        self.rows(
            "SELECT n.recipient_agent_id,
                    COUNT(DISTINCT CASE WHEN n.status = 'held' THEN n.notification_id END),
                    COUNT(DISTINCT CASE WHEN n.status IN ('pending', 'dispatching') AND m.created_at < ?2
                          AND a.address_kind <> 'human'
                          THEN n.notification_id END),
                    MAX(CASE WHEN t.status = 'delivered' OR (t.status = 'accepted' AND a.address_kind = 'codex')
                        THEN t.settled_at END),
                    SUM(CASE WHEN t.status = 'delivered' OR (t.status = 'accepted' AND a.address_kind = 'codex')
                        THEN 1 ELSE 0 END) > 0
             FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
             JOIN messages m ON m.message_id = n.message_id
             LEFT JOIN attempts t ON t.notification_id = n.notification_id AND t.bridge_instance <> 'slack'
             WHERE a.owner_id = ?1 GROUP BY n.recipient_agent_id ORDER BY n.recipient_agent_id",
            params![owner_id, stuck_before],
            |r| {
                Ok(AgentHealth {
                    agent_id: r.get(0)?,
                    held: r.get(1)?,
                    stuck: r.get(2)?,
                    last_reached: r.get(3)?,
                    reached: r.get(4)?,
                })
            },
        )
    }

    fn touch_bridge(&self, instance: &str, at: &str) -> StoreResult<bool> {
        Ok(self.exec(
            "UPDATE bridge_instances SET updated_at = ?2 WHERE instance = ?1 AND state = 'active'",
            params![instance, at],
        )? > 0)
    }

    fn announce(&self, topics: u8) {
        self.wakes.borrow_mut().board |= topics;
    }

    // import

    fn import_state(&self, owner: &str) -> StoreResult<ImportState> {
        let user_id = self.user_id(owner)?;
        let uid = user_id.unwrap_or(-1);
        let agents = self
            .rows(
                "SELECT name_lower, address_kind, address_target, host_id, pinned_address FROM agents
                 WHERE owner_id = ?1",
                [uid],
                |r| {
                    let address = address_of(r.get(1)?, r.get(2)?, r.get(3)?).map(|a| a.to_string());
                    Ok((r.get::<_, String>(0)?, (address, r.get::<_, Option<String>>(4)?)))
                },
            )?
            .into_iter()
            .collect();
        Ok(ImportState {
            user_id,
            agents,
            channels: self.set("SELECT name_lower FROM channels", [], one_col)?,
            messages: self.set("SELECT message_id FROM messages", [], one_col)?,
            attachments: self.set("SELECT message_id, filename FROM attachments", [], pair)?,
            memberships: self.set(
                "SELECT c.name_lower, a.name_lower FROM memberships x JOIN channels c ON c.channel_id = x.channel_id
                 JOIN agents a ON a.agent_id = x.agent_id WHERE a.owner_id = ?1",
                [uid],
                pair,
            )?,
            dm_read: self.set(
                "SELECT d.message_id, a.name_lower FROM dm_read d JOIN agents a ON a.agent_id = d.agent_id
                 WHERE a.owner_id = ?1",
                [uid],
                pair,
            )?,
            reactions: self.set(
                "SELECT x.message_id, a.name_lower, x.emoji FROM reactions x JOIN agents a ON a.agent_id = x.actor_id
                 WHERE a.owner_id = ?1",
                [uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?,
        })
    }

    fn insert_imported_agent(&self, a: &ImportedAgent) -> StoreResult<()> {
        self.exec(
            "INSERT INTO agents(agent_id, owner_id, name_lower, display, address_kind, address_target,
             host_id, binding_version, pinned_address) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                a.id,
                a.owner_id,
                a.name,
                a.display,
                a.address.map(|x| x.kind.as_str()),
                a.address.map(|x| x.target.as_str()),
                a.address.map(|x| x.host.as_str()),
                a.binding_version,
                a.pinned,
            ],
        )?;
        Ok(())
    }

    // diagnostics

    fn integrity_check(&self) -> StoreResult<String> {
        self.one("PRAGMA quick_check", [], one_col)
    }

    fn count_bound_agents(&self) -> StoreResult<i64> {
        self.one(
            "SELECT COUNT(*) FROM agents WHERE address_kind IS NOT NULL",
            [],
            one_col,
        )
    }

    fn unbound_waiting(&self, owner_id: i64) -> StoreResult<Vec<(String, i64)>> {
        self.rows(
            "SELECT u.name || '/' || a.name_lower, COUNT(*) FROM notifications n
             JOIN agents a ON a.agent_id = n.recipient_agent_id JOIN users u ON u.user_id = a.owner_id
             WHERE n.status = 'pending' AND a.address_kind IS NULL AND a.owner_id = ?1 GROUP BY 1 ORDER BY 1",
            [owner_id],
            pair,
        )
    }

    fn count_notifications(&self, owner_id: i64, statuses: &[&str]) -> StoreResult<i64> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let marks = vec!["?"; statuses.len()].join(", ");
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&owner_id];
        args.extend(statuses.iter().map(|s| s as &dyn rusqlite::ToSql));
        self.one(
            &format!(
                "SELECT COUNT(*) FROM notifications n JOIN agents a ON a.agent_id = n.recipient_agent_id
                 WHERE a.owner_id = ? AND n.status IN ({marks})"
            ),
            args.as_slice(),
            one_col,
        )
    }
}
