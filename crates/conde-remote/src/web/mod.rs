//! Browser surface of the cloud server: OIDC sign-in (authorization code + PKCE; Keycloak or Entra),
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
pub mod invite;
mod keys;
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

pub use admin::{AdminState, is_format, router as admin_router};
pub use app::embedded as app_embedded;

/// The host of a human the browser names (`human:<user>@web`): never minted as a real host, so
/// no bridge claims those humans' notifications.
pub const BROWSER_HOST: &str = "web";
pub use auth::{Oidc, RedeemResponse};
pub use invite::{InviteConfig, KeycloakAdmin, Resend, SignIn};
pub use keys::new_host_key;
pub use ratelimit::RateLimits;

/// Everything the web surface needs to know about the deployment.
#[derive(Clone)]
pub struct WebConfig {
    /// The OIDC issuer (a Keycloak realm URL, or Entra's `https://login.microsoftonline.com/<tenant>/v2.0`);
    /// discovery is read from here and every id token's `iss` must equal it.
    pub issuer: String,
    /// Set when the issuer is an Entra tenant: the subject is then the `oid` claim and
    /// `preferred_username` (the UPN) counts as a verified email.
    pub entra_tenant: Option<String>,
    pub client_id: String,
    pub client_secret: String,
    /// The registered redirect URI, `https://<host>/auth/callback`.
    pub redirect_uri: String,
    /// The server's public origin (`https://<host>`), derived from the redirect URI.
    pub public_url: String,
    /// Where the provider sends the browser after sign-out.
    pub post_logout_redirect_uri: String,
    /// A browser session ends after this long without a request.
    pub session_idle: Duration,
    /// A browser session ends this long after sign-in, whatever its activity.
    pub session_max: Duration,
    /// How long a started sign-in may take before its callback is refused.
    pub login_ttl: Duration,
    /// Lifetime of a one-time `conde login` code.
    pub cli_code_ttl: Duration,
    /// The rate limits of the `conde login` redeem.
    pub limits: RateLimits,
    /// How the admin API's invites reach the invited person.
    pub invite: InviteConfig,
}

impl std::fmt::Debug for WebConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("invite", &self.invite)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("CONDE_ADMIN_KEY must be at least {0} characters")]
    AdminKey(usize),
    #[error("OIDC_REDIRECT_URI '{0}' must be an https URL ending in /auth/callback")]
    RedirectUri(String),
    #[error("OIDC_ISSUER '{0}' must be an https URL")]
    Issuer(String),
    #[error("CONDE_JOIN_SIGN_IN '{0}' must be google, microsoft or password")]
    JoinSignIn(String),
    #[error("KEYCLOAK_ADMIN_URL '{0}' must be an http or https URL")]
    KeycloakUrl(String),
    #[error(
        "OIDC_ISSUER '{0}' names no Keycloak realm (https://<host>/realms/<realm>), which CONDE_JOIN_SIGN_IN=password needs"
    )]
    Realm(String),
    #[error(transparent)]
    Secret(#[from] crate::SecretError),
}

/// Entra's v2.0 issuer for `tenant`.
pub fn entra_issuer(tenant: &str) -> String {
    format!("https://login.microsoftonline.com/{tenant}/v2.0")
}

/// The tenant of an Entra v2.0 issuer URL; None for any other issuer.
pub fn entra_tenant_of(issuer: &str) -> Option<String> {
    let tenant = issuer
        .strip_prefix("https://login.microsoftonline.com/")?
        .strip_suffix("/v2.0")?;
    (!tenant.is_empty() && !tenant.contains('/')).then(|| tenant.to_string())
}

impl WebConfig {
    /// Builds the configuration for the OIDC `issuer` with the default lifetimes.
    pub fn oidc(
        issuer: &str,
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
            invite: InviteConfig {
                workspace: invite::workspace_of(&public_url),
                ..InviteConfig::default()
            },
            issuer: issuer.to_string(),
            entra_tenant: entra_tenant_of(issuer),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            redirect_uri: redirect_uri.to_string(),
            post_logout_redirect_uri: format!("{public_url}/"),
            public_url,
            session_idle: Duration::from_secs(8 * 3600),
            session_max: Duration::from_secs(24 * 3600),
            login_ttl: Duration::from_secs(600),
            cli_code_ttl: Duration::from_secs(300),
            limits: RateLimits::default(),
        })
    }

    /// Builds the Entra configuration for `tenant_id` with the default lifetimes.
    pub fn entra(
        tenant_id: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
    ) -> Result<WebConfig, ConfigError> {
        WebConfig::oidc(
            &entra_issuer(tenant_id),
            client_id,
            client_secret,
            redirect_uri,
        )
    }

    /// From the process environment; see [`WebConfig::from_vars`].
    pub fn from_env() -> Result<Option<WebConfig>, ConfigError> {
        WebConfig::from_vars(|k| std::env::var(k).ok())
    }

    /// From `OIDC_ISSUER`, `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET` (or `OIDC_CLIENT_SECRET_FILE`)
    /// and `OIDC_REDIRECT_URI`, each falling back to its Entra variable (`ENTRA_TENANT_ID` gives
    /// the tenant's issuer, `ENTRA_CLIENT_ID`, `ENTRA_CLIENT_SECRET`, `ENTRA_REDIRECT_URI`).
    /// `Ok(None)` when none is set (web sign-in disabled); an error when only some are.
    pub fn from_vars(
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<WebConfig>, ConfigError> {
        let var = |name: &str| var(name).filter(|v| !v.is_empty());
        let secret = |name: &str| crate::secret_from(name, var);
        let issuer =
            var("OIDC_ISSUER").or_else(|| var("ENTRA_TENANT_ID").map(|t| entra_issuer(&t)));
        let client_id = var("OIDC_CLIENT_ID").or_else(|| var("ENTRA_CLIENT_ID"));
        // a set OIDC_CLIENT_SECRET_FILE decides alone: an empty one is unset, never the Entra secret
        let client_secret = match secret("OIDC_CLIENT_SECRET")? {
            None if var("OIDC_CLIENT_SECRET_FILE").is_none() => secret("ENTRA_CLIENT_SECRET")?,
            other => other,
        };
        let redirect_uri = var("OIDC_REDIRECT_URI").or_else(|| var("ENTRA_REDIRECT_URI"));
        let (Some(issuer), Some(client_id), Some(client_secret), Some(redirect_uri)) =
            (&issuer, &client_id, &client_secret, &redirect_uri)
        else {
            return match (&issuer, &client_id, &client_secret, &redirect_uri) {
                (None, None, None, None) => Ok(None),
                (None, ..) => Err(ConfigError::Missing("OIDC_ISSUER (or ENTRA_TENANT_ID)")),
                (_, None, ..) => Err(ConfigError::Missing("OIDC_CLIENT_ID (or ENTRA_CLIENT_ID)")),
                (_, _, None, _) => Err(ConfigError::Missing(
                    "OIDC_CLIENT_SECRET (or ENTRA_CLIENT_SECRET)",
                )),
                _ => Err(ConfigError::Missing(
                    "OIDC_REDIRECT_URI (or ENTRA_REDIRECT_URI)",
                )),
            };
        };
        if !issuer.starts_with("https://") {
            return Err(ConfigError::Issuer(issuer.clone()));
        }
        let mut config = WebConfig::oidc(issuer, client_id, client_secret, redirect_uri)?;
        if !config.public_url.starts_with("https://") {
            return Err(ConfigError::RedirectUri(config.redirect_uri));
        }
        config.invite = InviteConfig::from_vars(&var, &config.public_url, &config.issuer)?;
        Ok(Some(config))
    }

    /// Cookies carry `Secure` and the `__Host-` prefix when the server is served over https.
    pub(crate) fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
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

/// A validated sign-in identity. `(issuer, subject)` is the key; `verified_email` is matched
/// once, by the invite gate; the rest is shown, never matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub issuer: String,
    /// The `sub` claim; for Entra the `oid` (the directory object, stable across apps).
    pub subject: String,
    pub display_name: Option<String>,
    pub preferred_username: Option<String>,
    /// What the Slack worker looks the person up by: `preferred_username` (the UPN), else the
    /// `email` claim when the UPN is not an address.
    pub email: Option<String>,
    /// The address the provider vouches for: `email` with `email_verified`, or Entra's UPN.
    pub verified_email: Option<String>,
}

/// Result of [`WebStore::create_user`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateUser {
    Created(WebUser),
    /// The name exists; `bound` says whether any identity is bound to it.
    NameTaken {
        bound: bool,
    },
}

/// Result of [`WebStore::bind_identity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindUser {
    /// No user had the name; it was created bound to the identity.
    Created(WebUser),
    /// An existing user without an identity of this issuer was bound to it.
    Bound(WebUser),
    /// The name holds another identity of this issuer already.
    NameBound,
}

/// Result of [`WebStore::claim_invite`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invite {
    /// The identity is bound to the user (now, or by a concurrent sign-in).
    Bound(WebUser),
    /// No user has the email.
    NoUser,
    /// More than one user has the email.
    Ambiguous,
    /// The one user with the email holds another identity of this issuer.
    IssuerBound,
}

/// Result of [`WebStore::invite_user`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invited {
    /// A new user with the email.
    Created(WebUser),
    /// An existing user without an email got this one.
    EmailSet(WebUser),
    /// The user exists with this email already.
    Exists(WebUser),
    /// The user exists with another email.
    OtherEmail(String),
    /// Another user has the email.
    EmailTaken(String),
}

/// The realm account of a password workspace's invite: the Keycloak user id once known, and the
/// correlation value its create request carried as the `conde_invite` attribute.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RealmAccount {
    pub id: Option<String>,
    pub invite: Option<String>,
}

/// A user as the admin API lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminUser {
    pub name: String,
    pub email: Option<String>,
    /// The issuers the user has an identity of.
    pub issuers: Vec<String>,
    pub disabled: bool,
    /// The workspace admin role.
    #[serde(default)]
    pub admin: bool,
}

/// The setup guide a workspace's welcome names when its admin set none.
pub const DEFAULT_GUIDE: &str = "https://conde.cloud/docs/setup";
/// The welcome text when the admin wrote none.
pub const DEFAULT_WELCOME_TEXT: &str =
    "Welcome to this conde workspace: your agents can now reach your teammates' agents here.";

/// A workspace's welcome, set by its admin (`POST /v1/admin/welcome`): the admin's agent that
/// newcomers write to with questions, the team channel, the setup guide and a short text. Stored
/// with unset fields; [`Welcome::filled`] gives the guide and text their defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// `owner/name` of a published agent of an admin user.
    #[serde(default)]
    pub admin_agent: Option<String>,
    /// `#channel` on the board.
    #[serde(default)]
    pub lobby: Option<String>,
    #[serde(default)]
    pub guide: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Welcome {
    /// The welcome with the default guide and text where unset.
    pub fn filled(mut self) -> Welcome {
        self.guide.get_or_insert_with(|| DEFAULT_GUIDE.to_string());
        self.text
            .get_or_insert_with(|| DEFAULT_WELCOME_TEXT.to_string());
        self
    }
}

/// An https URL a welcome may name: at most 512 characters, with no whitespace, control or
/// format characters.
pub fn welcome_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty())
        && url.len() <= 512
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_format(c))
}

/// A change to the welcome (`POST /v1/admin/welcome`): a field present replaces the stored one
/// (null clears it), an absent field stays as stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WelcomePatch {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub admin_agent: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub lobby: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub guide: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub text: Option<Option<String>>,
}

/// A field that is present, null or not.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

impl WelcomePatch {
    /// Every field cleared (`conde admin welcome clear`).
    pub fn clear() -> WelcomePatch {
        WelcomePatch {
            admin_agent: Some(None),
            lobby: Some(None),
            guide: Some(None),
            text: Some(None),
        }
    }

    /// `stored` with this change.
    pub fn apply(&self, stored: &Welcome) -> Welcome {
        let pick = |change: &Option<Option<String>>, old: &Option<String>| match change {
            Some(new) => new.clone(),
            None => old.clone(),
        };
        Welcome {
            admin_agent: pick(&self.admin_agent, &stored.admin_agent),
            lobby: pick(&self.lobby, &stored.lobby),
            guide: pick(&self.guide, &stored.guide),
            text: pick(&self.text, &stored.text),
        }
    }
}

/// How a host was enrolled, as an arrival notice names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enrollment {
    /// `conde login`.
    Login,
    /// The keys page.
    KeysPage,
    /// The admin API.
    Admin,
}

impl Enrollment {
    pub fn as_str(self) -> &'static str {
        match self {
            Enrollment::Login => "conde login",
            Enrollment::KeysPage => "keys page",
            Enrollment::Admin => "admin key",
        }
    }
}

/// Result of [`WebStore::set_welcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetWelcome {
    Stored(Welcome),
    /// The welcome names something the board does not have; the reason.
    Refused(String),
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

/// Result of [`WebStore::issue_cli_code`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliIssued {
    /// The record was stored.
    Issued,
    /// Another account's live record holds the login.
    Theirs,
    /// The login was first confirmed more than the TTL ago: it expired, for every account, and
    /// nothing was stored.
    Expired,
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
    fn user_by_identity(&self, issuer: &str, subject: &str) -> WebResult<Option<WebUser>>;
    /// The invite gate, in one transaction: binds the identity to the one user whose email is
    /// `email` (compared ASCII case-insensitively) when that user holds no identity of the
    /// identity's issuer yet.
    fn claim_invite(&self, identity: &Identity, email: &str) -> WebResult<Invite>;
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
    /// Issues a pasted-code login's record in one transaction. A login first confirmed more than
    /// `ttl` seconds before `code.created_at` expired: nothing is stored, whatever account asks
    /// and whether or not its record was purged (the first confirm's time outlives the record).
    /// Otherwise a record of another account stays, and `code` replaces any earlier record of
    /// the same account.
    fn issue_cli_code(&self, code_digest: &str, code: &CliCode, ttl: i64) -> WebResult<CliIssued>;
    /// Puts back a record taken for a wrong guess, in one transaction: a record the same account
    /// issued since (it confirmed again) stays instead; another account's record issued in the
    /// gap is replaced, so the login stays with the account that held it.
    fn restore_cli_code(&self, code_digest: &str, code: &CliCode) -> WebResult<()>;
    /// Ends a login taken for its last wrong try or found expired, in one transaction: a record
    /// the same account (`user_id`) issued since stays; another account's record issued in the
    /// gap is removed, so the dead login is not left to another account.
    fn end_cli_code(&self, code_digest: &str, user_id: i64) -> WebResult<()>;

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
    /// Creates `name` bound to the identity, or binds an existing `name` that holds no identity
    /// of the identity's issuer to it.
    fn bind_identity(&self, name: &str, identity: &Identity) -> WebResult<BindUser>;
    /// The invite: creates user `name` with `email`, or gives an existing `name` without an email
    /// this one; an email another user has is refused.
    fn invite_user(&self, name: &str, email: &str) -> WebResult<Invited>;
    /// Every user, by name.
    fn list_users(&self) -> WebResult<Vec<AdminUser>>;
    /// Every credential, newest first.
    fn all_keys(&self) -> WebResult<Vec<AdminKey>>;
    /// Revokes the live key whose id is `key_id`; returns its host.
    fn revoke_key(&self, key_id: &str, now: i64) -> WebResult<Option<String>>;
    /// Sets the user's disabled flag (disabling also deletes the user's browser sessions);
    /// returns the user's hosts, or None for an unknown user.
    fn set_disabled(&self, name: &str, disabled: bool) -> WebResult<Option<Vec<String>>>;
    /// The issuers the user holds an identity of. Default: refused (a store without invites).
    fn user_issuers(&self, _user_id: i64) -> WebResult<Vec<String>> {
        Err(WebStoreError("this board has no invites".into()))
    }
    /// The realm account an invite recorded for the user (password workspaces).
    fn realm_account(&self, _user_id: i64) -> WebResult<RealmAccount> {
        Err(WebStoreError("this board has no invites".into()))
    }
    /// Stores `candidate` as the user's correlation value unless a value or an account id is
    /// stored already, in one transaction; the stored record, which every concurrent invite uses.
    fn claim_realm_invite(&self, _user_id: i64, _candidate: &str) -> WebResult<RealmAccount> {
        Err(WebStoreError("this board has no invites".into()))
    }
    /// Records the account id when the stored value is `invite` and no id is recorded, in one
    /// transaction; the stored record.
    fn record_realm_id(&self, _user_id: i64, _invite: &str, _id: &str) -> WebResult<RealmAccount> {
        Err(WebStoreError("this board has no invites".into()))
    }
    /// Forgets the realm account recorded for user `name` (its id and correlation value; the
    /// user's identities and email stay), so the next invite creates a new one; whether one was
    /// recorded, None for an unknown user.
    fn clear_realm_account(&self, _name: &str) -> WebResult<Option<bool>> {
        Err(WebStoreError("this board has no invites".into()))
    }
    /// Sets the user's Slack member id as the admin's (a lookup by email never replaces it);
    /// None for an unknown user. Default: refused (a store without human paging).
    fn set_slack_member(&self, _name: &str, _member_id: &str) -> WebResult<Option<WebUser>> {
        Err(WebStoreError("this board has no Slack member ids".into()))
    }
    /// Sets or clears the user's admin role; None for an unknown user. Default: refused.
    fn set_admin(&self, _name: &str, _admin: bool) -> WebResult<Option<WebUser>> {
        Err(WebStoreError("this board has no admin role".into()))
    }
    /// The workspace's welcome as stored (unset fields None). Default: none set.
    fn welcome(&self) -> WebResult<Welcome> {
        Ok(Welcome::default())
    }
    /// Applies the change to the stored welcome, in one transaction, after checking that the
    /// admin agent it sets is a published agent of an admin user and the lobby it sets a channel
    /// on the board. Default: refused.
    fn set_welcome(&self, _change: &WelcomePatch) -> WebResult<SetWelcome> {
        Err(WebStoreError("this board has no welcome".into()))
    }
    /// A key was minted for a host new to the board: on the user's first enrollment, the
    /// welcome's greeting and arrival notice, once per user. Default: nothing.
    fn enrolled(
        &self,
        _user_id: i64,
        _host_id: &str,
        _how: Enrollment,
        _now: i64,
    ) -> WebResult<()> {
        Ok(())
    }
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
        let limits = ratelimit::Limiters::new(config.limits);
        WebState(Arc::new(Inner {
            core: Core {
                store,
                registry,
                clock,
            },
            config,
            oidc,
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
    /// Sign-in from `OIDC_*` (or `ENTRA_*`), the admin API from `CONDE_ADMIN_KEY` (or
    /// `CONDE_ADMIN_KEY_FILE`); `Ok(None)` when neither is configured.
    pub fn from_env(store: Arc<dyn WebStore>) -> Result<Option<WebSetup>, ConfigError> {
        let entra = WebConfig::from_env()?;
        let admin_key = crate::secret_env("CONDE_ADMIN_KEY")?;
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
        let mut state = None;
        if let Some(config) = &self.entra {
            let web = WebState::new(config.clone(), self.store.clone(), registry.clone());
            app = app.merge(router(web.clone()));
            state = Some(web);
        }
        if let Some(key) = self.admin_key {
            let core = Core {
                store: self.store,
                registry,
                clock: system_clock(),
            };
            app = app.merge(admin::router(AdminState::new(
                core,
                &key,
                self.entra.as_ref(),
            )));
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
        .with_state(state)
}
