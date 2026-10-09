//! The one JSON command dispatcher: authenticated principal + request frame
//! in, response frame out. Every mutation runs in one transaction together
//! with its idempotency record.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;
use thiserror::Error;

use crate::address::{Address, Kind, split_qualified};
use crate::ops::{self, agents::Agent};
use crate::store::postgres::{PgOpen, PgStore};
use crate::store::sqlite::SqliteStore;
use crate::store::{Mode, Store, StoreError, Tx};
use crate::protocol::{ErrorCode, Request, Response, VERSION};
use crate::util::{iso_from_ms, now_ms, sha256_hex};

/// Seconds-scale backoff before a failed / expired / dropped notification
/// gets a fresh attempt.
pub const NOTIFY_RETRY_MS: i64 = 30_000;

/// Who is on the other end of a connection, as established by the transport
/// (never by the request): in local mode the socket's OS user on this host.
#[derive(Clone, Debug)]
pub struct Principal {
    pub user_id: i64,
    pub user: String,
    pub host: String,
    /// Local unix socket (allows the human's `--from` override).
    pub local: bool,
    /// Set for a bridge connection: this bridge process's random instance id.
    pub bridge_instance: Option<String>,
    /// The bridge instance whose connection carried this request (a remote RPC
    /// over a bridge socket): refused once a newer instance fenced it.
    pub bridge_connection: Option<String>,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct OpError {
    pub code: ErrorCode,
    pub message: String,
}

pub type OpResult<T> = Result<T, OpError>;

macro_rules! op_error_ctor {
    ($($fn:ident => $code:ident),* $(,)?) => {
        $(pub fn $fn(message: impl Into<String>) -> OpError {
            OpError { code: ErrorCode::$code, message: message.into() }
        })*
    };
}

op_error_ctor! {
    bad_request => BadRequest,
    unauthenticated => Unauthenticated,
    forbidden => Forbidden,
    not_found => NotFound,
    conflict => Conflict,
    too_large => TooLarge,
}

impl From<StoreError> for OpError {
    fn from(e: StoreError) -> OpError {
        OpError { code: ErrorCode::Internal, message: format!("storage error: {e}") }
    }
}

/// Per-request state handed to an op.
pub struct Ctx<'a> {
    pub db: &'a dyn Tx,
    pub p: &'a Principal,
    pub actor: Option<&'a str>,
    pub from: Option<&'a str>,
    pub now_ms: i64,
    /// Backoff before a failed / expired / dropped notification is retried.
    pub retry_ms: i64,
}

impl Ctx<'_> {
    pub fn now_iso(&self) -> String {
        iso_from_ms(self.now_ms)
    }

    /// The caller's harness address; its host is this connection's host.
    pub fn actor_address(&self) -> OpResult<Address> {
        let raw = self
            .actor
            .ok_or_else(|| unauthenticated("this request needs the caller's identity (actor)"))?;
        let address = Address::parse(raw, &self.p.host).map_err(|e| bad_request(e.to_string()))?;
        if address.host != self.p.host {
            return Err(forbidden(format!(
                "actor host '{}' is not this connection's host '{}'",
                address.host, self.p.host
            )));
        }
        Ok(address)
    }

    /// Resolve a possibly qualified name inside the caller's own namespace;
    /// a qualified name of another owner is refused.
    pub fn own_name(&self, input: &str) -> OpResult<String> {
        let (owner, name) =
            split_qualified(input).ok_or_else(|| bad_request(format!("invalid name '{input}'")))?;
        match owner {
            Some(o) if o != self.p.user => Err(forbidden(format!(
                "names are per owner: '{o}/{name}' is not in your namespace '{}'",
                self.p.user
            ))),
            _ => Ok(name),
        }
    }

    /// The registered agent this request acts as.
    pub fn me(&self) -> OpResult<Agent> {
        let address = self.actor_address()?;
        if let Some(from) = self.from {
            if address.kind != Kind::Human || !self.p.local {
                return Err(forbidden("--from is a local override for the human only"));
            }
            if from.starts_with('#') {
                return Err(bad_request("--from must be an agent name, not a channel"));
            }
            let name = self.own_name(from)?;
            return ops::agents::by_name(self.db, self.p.user_id, &name)?
                .filter(|a| a.address.is_some())
                .ok_or_else(|| not_found(format!("no agent registered as '{name}'")));
        }
        let mut found = ops::agents::at_address(self.db, self.p.user_id, &address)?;
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(unauthenticated(format!(
                "this session ({address}) is not in the registry; run: conde register <your-name>"
            ))),
            _ => Err(conflict(format!(
                "multiple registry names point at this session: {}",
                found.iter().map(Agent::canonical).collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

pub struct OpSpec {
    pub name: &'static str,
    pub mutation: bool,
    pub bridge_only: bool,
    pub run: fn(&Ctx, &Value) -> OpResult<Value>,
}

pub struct Dispatcher {
    store: Arc<dyn Store>,
    clock_offset_ms: AtomicI64,
    retry_ms: AtomicI64,
}

impl Dispatcher {
    /// A SQLite board file (local mode).
    pub fn open(path: &Path) -> anyhow::Result<Dispatcher> {
        Ok(Dispatcher::with(Arc::new(SqliteStore::open(path)?)))
    }

    pub fn open_in_memory() -> anyhow::Result<Dispatcher> {
        Ok(Dispatcher::with(Arc::new(SqliteStore::open_in_memory()?)))
    }

    /// A Postgres board (cloud mode); `how` says whether to migrate and listen.
    pub fn connect(url: &str, how: PgOpen) -> anyhow::Result<Dispatcher> {
        Ok(Dispatcher::with(Arc::new(PgStore::open(url, how)?)))
    }

    pub fn with(store: Arc<dyn Store>) -> Dispatcher {
        Dispatcher { store, clock_offset_ms: AtomicI64::new(0), retry_ms: AtomicI64::new(NOTIFY_RETRY_MS) }
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    /// The SQLite backend, when this board has one.
    pub fn sqlite(&self) -> Option<&SqliteStore> {
        self.store.as_any().downcast_ref::<SqliteStore>()
    }

    pub fn now_ms(&self) -> i64 {
        now_ms() + self.clock_offset_ms.load(Ordering::Relaxed)
    }

    /// Move the board clock forward (tests of retry backoff).
    pub fn advance_clock(&self, ms: i64) {
        self.clock_offset_ms.fetch_add(ms, Ordering::Relaxed);
    }

    /// Change the notification retry backoff (tests and `--retry-backoff`).
    pub fn set_retry_ms(&self, ms: i64) {
        self.retry_ms.store(ms.max(0), Ordering::Relaxed);
    }

    /// Run `f` in one read transaction (tests, diagnostics, lookups).
    pub fn with_conn<T>(&self, f: impl FnOnce(&dyn Tx) -> Result<T, StoreError>) -> Result<T, StoreError> {
        let tx = self.store.begin(Mode::Read)?;
        let out = f(&*tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Run `f` in one write transaction, committed when it returns Ok.
    pub fn with_tx<T>(&self, f: impl FnOnce(&dyn Tx) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let tx = self.store.begin(Mode::Write)?;
        let out = f(&*tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Whether the store answers.
    pub fn healthy(&self) -> bool {
        self.store.ping()
    }

    /// The principal of an authenticated remote connection: `user_id` on `host`,
    /// optionally as bridge instance `bridge_instance`. None when the user is gone
    /// or the host is not theirs.
    pub fn remote_principal(&self, user_id: i64, host: &str, bridge_instance: Option<&str>) -> Option<Principal> {
        let user = self.with_conn(|tx| tx.host_owner(host)).ok().flatten()?;
        (user.0 == user_id).then(|| Principal {
            user_id,
            user: user.1,
            host: host.to_string(),
            local: false,
            bridge_instance: bridge_instance.map(str::to_string),
            bridge_connection: None,
        })
    }

    /// The principal for `user` on `host`, creating both on first sight. A
    /// host belongs to exactly one user.
    pub fn principal(&self, user: &str, host: &str, local: bool) -> anyhow::Result<Principal> {
        let user = user.to_lowercase();
        let host = host.to_lowercase();
        let (user_id, owner) = self.with_tx(|tx| {
            let user_id = tx.ensure_user(&user)?;
            Ok((user_id, tx.ensure_host(&host, user_id)?))
        })?;
        anyhow::ensure!(owner == user_id, "host '{host}' belongs to another user");
        Ok(Principal { user_id, user, host, local, bridge_instance: None, bridge_connection: None })
    }

    /// A bridge principal for the same user and host, with a fresh instance id.
    pub fn bridge_principal(&self, of: &Principal) -> Principal {
        Principal { bridge_instance: Some(crate::util::new_ulid()), ..of.clone() }
    }

    pub fn dispatch(&self, p: &Principal, req: &Request) -> Response {
        match self.try_dispatch(p, req) {
            Ok(result) => Response::ok(&req.request_id, result),
            Err(e) => Response::err(&req.request_id, e.code, e.message),
        }
    }

    fn try_dispatch(&self, p: &Principal, req: &Request) -> OpResult<Value> {
        if req.v != VERSION || req.frame_type != "request" {
            return Err(bad_request(format!("expected a v{VERSION} request frame")));
        }
        let spec = ops::OPS
            .iter()
            .find(|s| s.name == req.op)
            .ok_or_else(|| bad_request(format!("unknown op '{}'", req.op)))?;
        if spec.bridge_only && p.bridge_instance.is_none() {
            return Err(forbidden(format!("'{}' is a bridge operation", spec.name)));
        }
        if req.from.is_some() && !p.local {
            return Err(forbidden("--from is a local override for the human only"));
        }
        let key = match (&req.idempotency_key, spec.mutation && !spec.bridge_only) {
            (Some(k), true) if !k.is_empty() && k.len() <= 200 => Some(k.as_str()),
            (_, true) => {
                return Err(bad_request(format!("'{}' needs an idempotency_key (1-200 chars)", spec.name)));
            }
            (_, false) => None,
        };
        let actor_id = self.actor_id(p, req);
        let request_hash = key.map(|_| {
            sha256_hex(format!("{}\n{}\n{}", req.op, req.from.as_deref().unwrap_or(""), req.body).as_bytes())
        });

        let tx = self.store.begin(if spec.mutation { Mode::Write } else { Mode::Read })?;
        if let Some(instance) = &p.bridge_connection
            && let Some(newer) = fenced_by(&*tx, &p.host, instance)?
        {
            return Err(forbidden(format!(
                "bridge instance {instance} was fenced by {newer} for host {}; this connection takes no more requests",
                p.host
            )));
        }
        if let (Some(key), Some(hash)) = (key, &request_hash)
            && let Some((stored_hash, result)) = tx.idempotency_get(&actor_id, spec.name, key)?
        {
            if &stored_hash != hash {
                return Err(conflict("idempotency key reused with a different request"));
            }
            return serde_json::from_str(&result).map_err(|e| OpError {
                code: ErrorCode::Internal,
                message: format!("stored result unreadable: {e}"),
            });
        }
        let ctx = Ctx {
            db: &*tx,
            p,
            actor: req.actor.as_deref(),
            from: req.from.as_deref(),
            now_ms: self.now_ms(),
            retry_ms: self.retry_ms.load(Ordering::Relaxed),
        };
        let result = (spec.run)(&ctx, &req.body)?;
        if let (Some(key), Some(hash)) = (key, request_hash) {
            tx.idempotency_put(&actor_id, spec.name, key, &hash, &result.to_string(), &ctx.now_iso())?;
        }
        tx.commit()?;
        Ok(result)
    }

    /// Idempotency scope: the authenticated user plus the normalized actor.
    fn actor_id(&self, p: &Principal, req: &Request) -> String {
        let actor = req
            .actor
            .as_deref()
            .map(|a| Address::parse(a, &p.host).map(|x| x.to_string()).unwrap_or_else(|_| a.to_string()))
            .unwrap_or_default();
        format!("{}|{}|{}", p.user_id, actor, req.from.as_deref().unwrap_or("").to_lowercase())
    }
}

/// The instance that superseded `instance` on `host`, when it was fenced (the
/// durable takeover boundary: checked inside the request's own transaction).
pub fn fenced_by(tx: &dyn Tx, host: &str, instance: &str) -> Result<Option<String>, StoreError> {
    let own = tx.bridge_instance(instance)?.map(|(_, state)| state);
    let newer = tx.active_bridges(host)?.into_iter().find(|a| a != instance);
    Ok(match (own.as_deref(), newer) {
        (_, Some(newer)) => Some(newer),
        (Some("fenced"), None) => Some("a newer instance".to_string()),
        _ => None,
    })
}

// --- body accessors ---------------------------------------------------------

pub fn str_param<'v>(body: &'v Value, key: &str) -> OpResult<&'v str> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request(format!("missing string field '{key}'")))
}

pub fn opt_str<'v>(body: &'v Value, key: &str) -> OpResult<Option<&'v str>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad_request(format!("field '{key}' must be a string"))),
    }
}

pub fn opt_i64(body: &Value, key: &str) -> OpResult<Option<i64>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or_else(|| bad_request(format!("field '{key}' must be an integer"))),
    }
}

/// A single-line header-safe value.
pub fn one_line(value: &str, what: &str) -> OpResult<()> {
    if value.chars().any(char::is_control) {
        return Err(bad_request(format!("{what} must be a single line without control characters")));
    }
    Ok(())
}
