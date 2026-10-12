//! Browser surface of the cloud server: Entra ID sign-in (OIDC authorization code + PKCE),
//! the keys page (list, create shown once, revoke) and the `conde login` browser leg.
//!
//! The server stays stateless: login transactions, browser sessions and one-time CLI codes
//! live in the board's store behind [`WebStore`]; secrets are kept only as SHA-256 digests.
//! Routes: `GET /auth/login`, `GET /auth/callback`, `POST /auth/logout`, `GET /auth/cli`
//! (loopback, or `mode=code` for the pasted code's confirm page), `POST /auth/cli` (that
//! page's button: the code), `POST /auth/cli/redeem`, `GET /keys`, `POST /keys`, `GET /keys/created`,
//! `POST /keys/{host}/revoke`, `POST /keys/apps/{grant_id}/revoke`, the remote MCP's OAuth
//! authorization server ([`oauth`]: metadata, `/oauth/authorize`, `/oauth/token`); with
//! `CONDE_ADMIN_KEY` also the admin API under `/v1/admin`. The server adds the web app
//! ([`app`]): `/`, `/app/*` and the browser API under `/v1`, and the MCP endpoint ([`mcp`]).

mod admin;
pub(crate) mod app;
mod auth;
pub mod cimd;
mod keys;
pub(crate) mod mcp;
pub(crate) mod oauth;
mod pages;
mod ratelimit;
pub(crate) use pages::document;
mod session;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use admin::{AdminState, router as admin_router};
pub use app::embedded as app_embedded;

/// The host of a human the browser names (`human:<user>@web`): never minted as a real host, so
/// no bridge claims those humans' notifications.
pub const BROWSER_HOST: &str = "web";
pub use auth::{Oidc, RedeemResponse};
pub use keys::new_host_key;
pub use ratelimit::McpLimits;

/// Everything the web surface needs to know about the deployment.
#[derive(Clone)]
pub struct WebConfig {
    /// `https://login.microsoftonline.com/<tenant>/v2.0`; discovery is read from here.
    pub issuer: String,
    /// The only tenant whose users may sign in (the `tid` claim must equal it).
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    /// The registered redirect URI, `https://<host>/auth/callback`.
    pub redirect_uri: String,
    /// The server's public origin (`https://<host>`), derived from the redirect URI.
    pub public_url: String,
    /// Where Entra sends the browser after sign-out.
    pub post_logout_redirect_uri: String,
    /// A browser session ends after this long without a request.
    pub session_idle: Duration,
    /// A browser session ends this long after sign-in, whatever its activity.
    pub session_max: Duration,
    /// How long a started sign-in may take before its callback is refused.
    pub login_ttl: Duration,
    /// Lifetime of a one-time `conde login` code.
    pub cli_code_ttl: Duration,
    /// The remote MCP endpoint's authorization server.
    pub mcp: McpConfig,
}

/// The remote MCP's OAuth settings (docs/REMOTE-MCP.md §2, §3).
#[derive(Clone)]
pub struct McpConfig {
    /// Hosts a client's metadata URL (its client_id) may be on, matched exactly
    /// (`CONDE_MCP_CLIENT_HOSTS`, default `claude.ai,claude.com`).
    pub client_hosts: Vec<String>,
    /// Reads client metadata documents.
    pub fetcher: Arc<dyn cimd::ClientMetadataFetcher>,
    /// How long a fetched client metadata document is reused.
    pub client_cache: Duration,
    /// Lifetime of an authorization code.
    pub code_ttl: Duration,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    /// A grant ends this long after consent, whatever its refreshes.
    pub grant_ttl: Duration,
    /// Live grants a user may hold per client_id; a new grant beyond it revokes the oldest
    /// (`CONDE_MCP_GRANTS_PER_CLIENT`, default 10).
    pub grants_per_client: u32,
    /// Period of the server's sweep of expired codes, tokens and grants.
    pub sweep_every: Duration,
    pub limits: McpLimits,
}

impl Default for McpConfig {
    fn default() -> McpConfig {
        McpConfig {
            client_hosts: vec!["claude.ai".into(), "claude.com".into()],
            fetcher: Arc::new(cimd::HttpFetcher::default()),
            client_cache: cimd::CACHE_FOR,
            code_ttl: Duration::from_secs(60),
            access_ttl: Duration::from_secs(3600),
            refresh_ttl: Duration::from_secs(30 * 86400),
            grant_ttl: Duration::from_secs(90 * 86400),
            grants_per_client: 10,
            sweep_every: Duration::from_secs(3600),
            limits: McpLimits::default(),
        }
    }
}

impl std::fmt::Debug for WebConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("CONDE_ADMIN_KEY must be at least {0} characters")]
    AdminKey(usize),
    #[error("ENTRA_REDIRECT_URI '{0}' must be an https URL ending in /auth/callback")]
    RedirectUri(String),
    #[error("{0} must be a whole number of at least 1")]
    NotPositive(&'static str),
}

/// `CONDE_MCP_GRANTS_PER_CLIENT`: a whole number of at least 1 (surrounding spaces allowed).
fn grants_per_client(raw: &str) -> Result<u32, ConfigError> {
    raw.trim().parse().ok().filter(|n| *n >= 1).ok_or(ConfigError::NotPositive("CONDE_MCP_GRANTS_PER_CLIENT"))
}

impl WebConfig {
    /// Builds the Entra configuration for `tenant_id` with the default lifetimes.
    pub fn entra(
        tenant_id: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
    ) -> Result<WebConfig, ConfigError> {
        let public_url = redirect_uri
            .strip_suffix("/auth/callback")
            .filter(|origin| origin.starts_with("https://") || origin.starts_with("http://"))
            .ok_or_else(|| ConfigError::RedirectUri(redirect_uri.to_string()))?
            .to_string();
        Ok(WebConfig {
            issuer: format!("https://login.microsoftonline.com/{tenant_id}/v2.0"),
            tenant_id: tenant_id.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_uri: redirect_uri.to_string(),
            post_logout_redirect_uri: format!("{public_url}/"),
            public_url,
            session_idle: Duration::from_secs(8 * 3600),
            session_max: Duration::from_secs(24 * 3600),
            login_ttl: Duration::from_secs(600),
            cli_code_ttl: Duration::from_secs(300),
            mcp: McpConfig::default(),
        })
    }

    /// From `ENTRA_TENANT_ID`, `ENTRA_CLIENT_ID`, `ENTRA_CLIENT_SECRET`, `ENTRA_REDIRECT_URI`.
    /// `Ok(None)` when none is set (web sign-in disabled); an error when only some are.
    pub fn from_env() -> Result<Option<WebConfig>, ConfigError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let names = [
            "ENTRA_TENANT_ID",
            "ENTRA_CLIENT_ID",
            "ENTRA_CLIENT_SECRET",
            "ENTRA_REDIRECT_URI",
        ];
        let values: Vec<Option<String>> = names.iter().copied().map(var).collect();
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let get = |i: usize| values[i].as_deref().ok_or(ConfigError::Missing(names[i]));
        let mut config = WebConfig::entra(get(0)?, get(1)?, get(2)?, get(3)?)?;
        if !config.public_url.starts_with("https://") {
            return Err(ConfigError::RedirectUri(config.redirect_uri));
        }
        if let Some(hosts) = var("CONDE_MCP_CLIENT_HOSTS") {
            config.mcp.client_hosts = hosts.split(',').map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
        }
        if let Some(cap) = var("CONDE_MCP_GRANTS_PER_CLIENT") {
            config.mcp.grants_per_client = grants_per_client(&cap)?;
        }
        Ok(Some(config))
    }

    /// Cookies carry `Secure` and the `__Host-` prefix when the server is served over https.
    pub(crate) fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    /// The remote MCP's authorization server identifier: the public origin.
    pub fn mcp_issuer(&self) -> String {
        self.public_url.clone()
    }

    /// The MCP endpoint's URL: the protected resource and its tokens' audience.
    pub fn mcp_resource(&self) -> String {
        format!("{}/mcp", self.public_url)
    }

    /// Where the MCP endpoint's protected resource metadata is.
    pub fn mcp_resource_metadata(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource/mcp", self.public_url)
    }
}

/// A store failure; the request fails with 500 and the message is logged, never shown.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct WebStoreError(pub String);

pub type WebResult<T> = Result<T, WebStoreError>;

/// A board user as the web surface sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebUser {
    pub user_id: i64,
    /// The owner namespace (`name/agent`).
    pub name: String,
    /// A disabled user signs in to nothing and none of their keys authenticate.
    pub disabled: bool,
}

/// A validated Entra identity. `(tid, oid)` is the key; the rest is shown, never matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub tid: String,
    pub oid: String,
    pub display_name: Option<String>,
    pub preferred_username: Option<String>,
}

/// Result of [`WebStore::create_user`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateUser {
    Created(WebUser),
    /// The name exists; `bound` says whether it already belongs to an Entra identity.
    NameTaken {
        bound: bool,
    },
}

/// Result of [`WebStore::bind_identity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindUser {
    /// No user had the name; it was created bound to the identity.
    Created(WebUser),
    /// An existing user without an identity was bound to it.
    Bound(WebUser),
    /// The name belongs to another identity already.
    NameBound,
}

/// A credential as the admin API lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminKey {
    pub user: String,
    #[serde(flatten)]
    pub key: HostKey,
}

/// A browser session row, keyed by the digest of the cookie value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub user_id: i64,
    pub user_name: String,
    /// What the page header shows (display name, else preferred_username, else the user name).
    pub display: String,
    /// The per-session CSRF token every POST form carries.
    pub csrf: String,
    /// Unix seconds.
    pub created_at: i64,
    pub last_seen: i64,
}

/// A started sign-in, keyed by the digest of its `state`; taken once by the callback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginTxn {
    pub pkce_verifier: String,
    pub nonce: String,
    /// A local path to return to after sign-in.
    pub return_to: String,
    pub created_at: i64,
}

/// A signed-in `conde login`, taken once by redeem. The loopback flow keys it by the digest of
/// the code it hands the loopback. The pasted-code flow keys it by the digest of
/// `paste:<state>` and keeps the short code the page showed as a digest in `paste`; a wrong
/// code puts it back with one more try, until [`PASTE_TRIES`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliCode {
    pub user_id: i64,
    pub user_name: String,
    /// The CLI run's state; redeem must present the same value.
    pub state: String,
    /// The CLI run's PKCE S256 challenge; redeem must present its verifier.
    pub challenge: String,
    pub created_at: i64,
    /// The pasted-code flow: the digest of `<state>:<code>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste: Option<String>,
    /// Failed redeems so far (pasted-code flow).
    #[serde(default)]
    pub tries: u32,
}

/// The pasted code's alphabet: no 0/O, 1/I/L or U, so it reads aloud and types back cleanly.
pub const CODE_ALPHABET: &str = "23456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Failed redeems a pasted-code login survives; the last one ends it.
pub const PASTE_TRIES: u32 = 5;

/// A just-created key awaiting its one display after the redirect, keyed by the digest of a
/// one-time display token; taken once. The key itself is not stored, only its digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyDisplay {
    pub user_id: i64,
    pub host_id: String,
    /// Whether the key replaced live keys of its host.
    pub replaced: bool,
    pub key_digest: String,
    pub created_at: i64,
}

/// One host credential of a user, as the keys page lists it (never the key itself).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKey {
    /// The digest's first 12 hex characters (the admin CLI's key id).
    pub key_id: String,
    pub host_id: String,
    pub label: Option<String>,
    pub created_at: Option<String>,
    /// Last bridge activity of the host, when the store knows it.
    pub last_seen: Option<String>,
    pub revoked_at: Option<String>,
}

/// A one-time remote MCP authorization code, keyed by its digest; taken once by the token
/// endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCode {
    pub user_id: i64,
    pub client_id: String,
    /// The client's `client_name` at consent (shown on the keys page, quoted).
    pub client_name: Option<String>,
    pub redirect_uri: String,
    /// The PKCE S256 challenge.
    pub challenge: String,
    /// The resource the code is for (the MCP endpoint).
    pub resource: String,
    pub created_at: i64,
}

/// A remote MCP grant: one client's consented access for one user (unix seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpGrant {
    pub grant_id: String,
    pub user_id: i64,
    pub client_id: String,
    pub client_host: String,
    pub client_name: Option<String>,
    /// The resource consented to: every token of the grant carries it as its audience.
    pub resource: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

/// Fresh tokens of a grant as digests, with their lifetimes in seconds (each capped at the
/// grant's end by the store).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpTokens {
    pub access_digest: String,
    pub refresh_digest: String,
    pub access_ttl: i64,
    pub refresh_ttl: i64,
}

/// An access token as the MCP endpoint checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpAccess {
    pub grant: McpGrant,
    /// The grant user's name (owner namespace).
    pub user: String,
    pub disabled: bool,
    /// The token's audience.
    pub audience: String,
    pub expires_at: i64,
}

/// Result of [`WebStore::create_mcp_grant`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCreated {
    /// The first access token's expiry.
    pub access_expires_at: i64,
    /// The grants revoked to stay within the cap, oldest first.
    pub revoked: Vec<String>,
}

/// Result of [`WebStore::refresh_mcp_grant`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpRefreshed {
    /// The token was consumed and the new pair stored; the new access token's expiry.
    Rotated { grant: McpGrant, access_expires_at: i64 },
    /// An already consumed token was presented again: the grant was revoked in the same
    /// transaction.
    Reused { grant: McpGrant },
    /// Unknown, expired, of another client, of an ended grant or a disabled user.
    Invalid,
}

/// The user's published human name on the board, which the remote MCP writes as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpHuman {
    /// `owner/name`.
    One(String),
    /// None or several: why writes are refused, with what to do (the tools' wording).
    Refused(String),
}

/// Result of [`WebStore::mint_host_key`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Minted {
    /// Stored; `replaced` live keys of the same host were revoked in the same transaction;
    /// `new_host`: the host did not exist before this key.
    Stored { replaced: u64, new_host: bool },
    /// The host id belongs to another user.
    HostTaken,
    /// The user is disabled (checked in the same transaction as the mint).
    Disabled,
}

/// The board's storage as the web surface needs it. Implemented over the board's store
/// (SQLite locally, Postgres in the cloud); every method is one transaction and is called
/// from a blocking thread. Times are unix seconds.
pub trait WebStore: Send + Sync {
    fn user_by_identity(&self, tid: &str, oid: &str) -> WebResult<Option<WebUser>>;
    /// Creates user `name` bound to the identity, or reports the name as taken.
    fn create_user(&self, identity: &Identity, name: &str) -> WebResult<CreateUser>;
    /// Records the display-only profile fields seen at sign-in.
    fn set_profile(&self, user_id: i64, identity: &Identity) -> WebResult<()>;
    /// Whether the user is disabled (an unknown user counts as disabled).
    fn user_disabled(&self, user_id: i64) -> WebResult<bool>;

    /// Stores the session unless its user is disabled (or gone), checked in the same
    /// transaction; false when refused.
    fn insert_session(&self, id_digest: &str, session: &SessionRecord) -> WebResult<bool>;
    fn session(&self, id_digest: &str) -> WebResult<Option<SessionRecord>>;
    fn touch_session(&self, id_digest: &str, last_seen: i64) -> WebResult<()>;
    fn delete_session(&self, id_digest: &str) -> WebResult<()>;

    fn insert_login(&self, state_digest: &str, txn: &LoginTxn) -> WebResult<()>;
    /// Returns and deletes the transaction.
    fn take_login(&self, state_digest: &str) -> WebResult<Option<LoginTxn>>;

    fn insert_cli_code(&self, code_digest: &str, code: &CliCode) -> WebResult<()>;
    /// Returns and deletes the code.
    fn take_cli_code(&self, code_digest: &str) -> WebResult<Option<CliCode>>;

    fn insert_key_display(&self, token_digest: &str, display: &KeyDisplay) -> WebResult<()>;
    /// Returns and deletes the display.
    fn take_key_display(&self, token_digest: &str) -> WebResult<Option<KeyDisplay>>;

    /// Deletes sessions, login transactions, CLI codes and key displays older than their limits.
    fn purge_expired(&self, now: i64, limits: &Limits) -> WebResult<()>;

    /// The user's credentials, newest first.
    fn host_keys(&self, user_id: i64) -> WebResult<Vec<HostKey>>;
    /// The user owning `host_id`, if the host exists.
    fn host_owner(&self, host_id: &str) -> WebResult<Option<i64>>;
    /// Creates the host for the user on first sight, revokes its live keys and stores the new
    /// key's digest (device_credentials row with label and creation time); refuses a disabled
    /// user in the same transaction.
    fn mint_host_key(
        &self,
        user_id: i64,
        host_id: &str,
        label: &str,
        token_digest: &str,
        now: i64,
    ) -> WebResult<Minted>;
    /// Revokes the live keys of `host_id` when the user owns it; returns how many.
    fn revoke_host(&self, user_id: i64, host_id: &str, now: i64) -> WebResult<u64>;

    // admin API
    /// The user named `name`, created without an identity on first sight.
    fn ensure_user(&self, name: &str) -> WebResult<WebUser>;
    /// Creates `name` bound to the identity, or binds an existing unbound `name` to it.
    fn bind_identity(&self, name: &str, identity: &Identity) -> WebResult<BindUser>;
    /// Every credential, newest first.
    fn all_keys(&self) -> WebResult<Vec<AdminKey>>;
    /// Revokes the live key whose id is `key_id`; returns its host.
    fn revoke_key(&self, key_id: &str, now: i64) -> WebResult<Option<String>>;
    /// Sets the user's disabled flag (disabling also deletes the user's browser sessions);
    /// returns the user's hosts, or None for an unknown user.
    fn set_disabled(&self, name: &str, disabled: bool) -> WebResult<Option<Vec<String>>>;

    // remote MCP (a store without them refuses every MCP request)
    fn insert_mcp_code(&self, _code_digest: &str, _code: &McpCode) -> WebResult<()> {
        Err(no_mcp())
    }
    /// Returns and deletes the code.
    fn take_mcp_code(&self, _code_digest: &str) -> WebResult<Option<McpCode>> {
        Err(no_mcp())
    }
    /// Stores the grant with its first token pair unless the user is disabled (checked in the
    /// same transaction), first revoking the user's oldest live grants of the same client so at
    /// most `max_live` stay live with it; None when refused.
    fn create_mcp_grant(
        &self,
        _grant: &McpGrant,
        _tokens: &McpTokens,
        _max_live: u32,
        _now: i64,
    ) -> WebResult<Option<McpCreated>> {
        Err(no_mcp())
    }
    /// The access token with its grant and user, whatever their state; records the grant's use
    /// at most once a minute while it is live.
    fn mcp_access(&self, _token_digest: &str, _now: i64) -> WebResult<Option<McpAccess>> {
        Err(no_mcp())
    }
    /// Rotates a refresh token of `client_id` in one transaction: consumes it and stores the
    /// new pair; a consumed one presented again revokes its grant.
    fn refresh_mcp_grant(
        &self,
        _refresh_digest: &str,
        _client_id: &str,
        _tokens: &McpTokens,
        _now: i64,
    ) -> WebResult<McpRefreshed> {
        Err(no_mcp())
    }
    /// The user's live grants, newest first (none on a store without them).
    fn mcp_grants(&self, _user_id: i64, _now: i64) -> WebResult<Vec<McpGrant>> {
        Ok(Vec::new())
    }
    /// Revokes the user's grant with its tokens; false when it is not the user's or not live.
    fn revoke_mcp_grant(&self, _user_id: i64, _grant_id: &str, _now: i64) -> WebResult<bool> {
        Err(no_mcp())
    }
    fn mcp_human(&self, _user_id: i64) -> WebResult<McpHuman> {
        Err(no_mcp())
    }
    /// Deletes codes older than `code_ttl` and expired or revoked grants and tokens. Without
    /// MCP there is nothing to purge (the hourly sweep calls it on every store).
    fn purge_mcp(&self, _now: i64, _code_ttl: i64) -> WebResult<()> {
        Ok(())
    }
}

fn no_mcp() -> WebStoreError {
    WebStoreError("this store keeps no MCP grants".into())
}

/// Lifetimes for [`WebStore::purge_expired`], in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub session_idle: i64,
    pub session_max: i64,
    pub login_ttl: i64,
    pub cli_code_ttl: i64,
    pub key_display_ttl: i64,
}

/// The live bridge connections, as revocation needs them (the server implements it over its
/// connection registry; this replica's connections only).
pub trait BridgeRegistry: Send + Sync {
    /// Closes the host's live bridge connection, if any.
    fn disconnect_host(&self, host_id: &str);
}

/// Unix seconds.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
    })
}

/// What every web and admin route shares: the store, the live connections, the clock.
#[derive(Clone)]
pub(crate) struct Core {
    store: Arc<dyn WebStore>,
    registry: Arc<dyn BridgeRegistry>,
    clock: Clock,
}

impl Core {
    fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Runs a store call on a blocking thread.
    async fn store<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn WebStore) -> WebResult<T> + Send + 'static,
    ) -> WebResult<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(store.as_ref()))
            .await
            .map_err(|e| WebStoreError(format!("store task failed: {e}")))?
    }
}

struct Inner {
    core: Core,
    config: WebConfig,
    oidc: Oidc,
    clients: cimd::Registry,
    limits: ratelimit::Limiters,
}

/// Shared state of the sign-in and keys routes.
#[derive(Clone)]
pub struct WebState(Arc<Inner>);

impl WebState {
    pub fn new(
        config: WebConfig,
        store: Arc<dyn WebStore>,
        registry: Arc<dyn BridgeRegistry>,
    ) -> WebState {
        WebState::with_clock(config, store, registry, system_clock())
    }

    pub fn with_clock(
        config: WebConfig,
        store: Arc<dyn WebStore>,
        registry: Arc<dyn BridgeRegistry>,
        clock: Clock,
    ) -> WebState {
        let oidc = Oidc::new(&config);
        let m = &config.mcp;
        let clients = cimd::Registry::new(m.client_hosts.clone(), m.fetcher.clone(), m.client_cache);
        let limits = ratelimit::Limiters::new(m.limits);
        WebState(Arc::new(Inner {
            core: Core {
                store,
                registry,
                clock,
            },
            config,
            oidc,
            clients,
            limits,
        }))
    }

    fn core(&self) -> &Core {
        &self.0.core
    }

    fn now(&self) -> i64 {
        self.core().now()
    }

    fn config(&self) -> &WebConfig {
        &self.0.config
    }

    fn limits(&self) -> Limits {
        let c = self.config();
        let s = |d: Duration| d.as_secs() as i64;
        Limits {
            session_idle: s(c.session_idle),
            session_max: s(c.session_max),
            login_ttl: s(c.login_ttl),
            cli_code_ttl: s(c.cli_code_ttl),
            key_display_ttl: keys::SHOW_WITHIN,
        }
    }

    async fn store<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn WebStore) -> WebResult<T> + Send + 'static,
    ) -> WebResult<T> {
        self.core().store(f).await
    }
}

/// The browser surface and the admin API as the server mounts them: each part is present
/// only when configured.
#[derive(Clone)]
pub struct WebSetup {
    pub entra: Option<WebConfig>,
    /// The admin API's bearer key; the admin API is off without it.
    pub admin_key: Option<String>,
    pub store: Arc<dyn WebStore>,
}

impl std::fmt::Debug for WebSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSetup")
            .field("entra", &self.entra)
            .field("admin", &self.admin_key.is_some())
            .finish_non_exhaustive()
    }
}

impl WebSetup {
    /// Entra sign-in from `ENTRA_*`, the admin API from `CONDE_ADMIN_KEY`; `Ok(None)` when
    /// neither is configured.
    pub fn from_env(store: Arc<dyn WebStore>) -> Result<Option<WebSetup>, ConfigError> {
        let entra = WebConfig::from_env()?;
        let admin_key = std::env::var("CONDE_ADMIN_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        if admin_key
            .as_ref()
            .is_some_and(|k| k.len() < admin::MIN_KEY_LEN)
        {
            return Err(ConfigError::AdminKey(admin::MIN_KEY_LEN));
        }
        if entra.is_none() && admin_key.is_none() {
            return Ok(None);
        }
        Ok(Some(WebSetup {
            entra,
            admin_key,
            store,
        }))
    }

    /// The configured routes over the server's connection registry.
    pub fn router(self, registry: Arc<dyn BridgeRegistry>) -> Router {
        self.into_parts(registry).0
    }

    /// The configured routes and, with Entra configured, the sign-in state they share (the
    /// web app's routes authenticate browsers with it).
    pub fn into_parts(self, registry: Arc<dyn BridgeRegistry>) -> (Router, Option<WebState>) {
        let mut app = Router::new();
        let tenant = self.entra.as_ref().map(|c| c.tenant_id.clone());
        let mut state = None;
        if let Some(config) = self.entra {
            let web = WebState::new(config, self.store.clone(), registry.clone());
            app = app.merge(router(web.clone()));
            state = Some(web);
        }
        if let Some(key) = self.admin_key {
            let core = Core {
                store: self.store,
                registry,
                clock: system_clock(),
            };
            app = app.merge(admin::router(AdminState::new(core, &key, tenant)));
        }
        (app, state)
    }
}

/// The sign-in and keys routes, ready to merge into the server's router (which serves `/`).
pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/cli", get(auth::cli_start).post(auth::cli_confirm))
        .route("/auth/cli/redeem", post(auth::cli_redeem))
        .route("/keys", get(keys::page).post(keys::create))
        .route("/keys/created", get(keys::created))
        .route("/keys/{host}/revoke", post(keys::revoke))
        .route("/keys/apps/{grant_id}/revoke", post(keys::revoke_app))
        .route("/.well-known/oauth-protected-resource", get(oauth::origin_resource))
        .route("/.well-known/oauth-protected-resource/mcp", get(oauth::protected_resource))
        .route("/.well-known/oauth-authorization-server", get(oauth::authorization_server))
        .route("/oauth/authorize", get(oauth::authorize).post(oauth::consent))
        .route("/oauth/token", post(oauth::token))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grant_cap_is_a_whole_number_of_at_least_one() {
        assert_eq!(grants_per_client(" 5 ").unwrap(), 5);
        assert_eq!(grants_per_client("1").unwrap(), 1);
        for bad in ["0", "-1", "abc", "4294967296", "", "2.5"] {
            let e = grants_per_client(bad).unwrap_err();
            assert_eq!(e.to_string(), "CONDE_MCP_GRANTS_PER_CLIENT must be a whole number of at least 1", "{bad:?}");
        }
    }
}
