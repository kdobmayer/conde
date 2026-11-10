//! Hosts and their device credentials: one revocable bearer key per enrolled
//! host, stored as a sha256 digest only.

use std::collections::BTreeSet;

use anyhow::{Context, bail};

use crate::store::{CredentialRow, StoreResult, Tx, topics};

/// A credential as listed to the admin (never the key itself).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// The digest's first 12 hex characters: enough to name a key for revocation.
    pub key_id: String,
    pub user: String,
    pub host: String,
    pub label: Option<String>,
    pub created_at: Option<String>,
    pub revoked_at: Option<String>,
}

pub fn key_id(digest: &str) -> String {
    digest.chars().take(12).collect()
}

/// (user_id, user name) owning `host`.
pub fn host_owner(db: &dyn Tx, host: &str) -> StoreResult<Option<(i64, String)>> {
    db.host_owner(host)
}

/// Store a new key's digest for `user` on `host`, creating both on first
/// sight. A host belongs to exactly one user.
pub fn insert_key(
    db: &dyn Tx,
    user: &str,
    host: &str,
    digest: &str,
    label: &str,
    now: &str,
) -> anyhow::Result<()> {
    let (user, host) = (user.to_lowercase(), host.to_lowercase());
    if host == conde_remote::web::BROWSER_HOST || host == conde_remote::SLACK_INSTANCE {
        bail!("host id '{host}' is reserved; choose another");
    }
    let user_id = db.ensure_user(&user)?;
    crate::humans::ensure(db, user_id)?;
    db.ensure_host(&host, user_id)?;
    let owner = db.host_owner(&host)?.context("host row missing")?;
    if owner.0 != user_id {
        bail!("host '{host}' belongs to user '{}', not '{user}'", owner.1);
    }
    db.insert_credential(digest, user_id, &host, label, now)?;
    Ok(())
}

/// Revoke the live keys of `host`, or the one key whose id starts `key_id`.
/// Returns the revoked keys.
pub fn revoke(
    db: &dyn Tx,
    host: Option<&str>,
    key_id: Option<&str>,
    now: &str,
) -> anyhow::Result<Vec<KeyInfo>> {
    let matching: Vec<KeyInfo> = list(db)?
        .into_iter()
        .filter(|k| k.revoked_at.is_none())
        .filter(|k| host.is_none_or(|h| k.host == h.to_lowercase()))
        .filter(|k| key_id.is_none_or(|id| k.key_id.starts_with(&id.to_lowercase())))
        .collect();
    if host.is_none() && key_id.is_none() {
        bail!("name the key to revoke: --host or --id");
    }
    if key_id.is_some() && matching.len() > 1 {
        bail!("key id is ambiguous: {} live keys match", matching.len());
    }
    for k in &matching {
        db.revoke_credential(&k.key_id, now)?;
    }
    let hosts: BTreeSet<&str> = matching.iter().map(|k| k.host.as_str()).collect();
    for host in hosts {
        release_names(db, host)?;
    }
    Ok(matching)
}

/// Whether `host` had keys and none of them is live: nothing can act as it any more.
fn keyless(creds: &[CredentialRow], host: &str) -> bool {
    let mut keys = creds.iter().filter(|c| c.host == host).peekable();
    keys.peek().is_some() && keys.all(|c| c.revoked_at.is_some())
}

/// Unbinds every name bound on `host` (never the human name), announced to the browsers on
/// commit; history, inbox and channel seats stay, and retiring stays the owner's choice.
fn unbind_on(db: &dyn Tx, host: &str) -> StoreResult<Vec<String>> {
    let mut released = Vec::new();
    for a in db.agents_on_host(host)? {
        if a.is_human() {
            continue;
        }
        db.set_binding(&a.id, None)?;
        released.push(a.canonical());
    }
    if !released.is_empty() {
        db.announce(topics::FLEET | topics::MESSAGES);
    }
    Ok(released)
}

/// After a revoke in the same transaction: when `host` has no live key left (its machine
/// deleted, its last key revoked), the names bound on it are released, since the host can no
/// longer unpublish them. A host that keeps a live key keeps its names.
pub fn release_names(db: &dyn Tx, host: &str) -> StoreResult<Vec<String>> {
    if !keyless(&db.credentials()?, host) {
        return Ok(Vec::new());
    }
    let released = unbind_on(db, host)?;
    for name in &released {
        tracing::info!(%host, %name, "name released: its host has no live key");
    }
    Ok(released)
}

/// [`release_names`] for every host whose keys were all revoked before releases existed (the
/// server's startup pass; a no-op once nothing is stranded).
pub fn release_stranded(db: &dyn Tx) -> StoreResult<Vec<String>> {
    let creds = db.credentials()?;
    let hosts: BTreeSet<&str> = creds.iter().map(|c| c.host.as_str()).collect();
    let mut released = Vec::new();
    for host in hosts.into_iter().filter(|h| keyless(&creds, h)) {
        for name in unbind_on(db, host)? {
            tracing::warn!(%host, %name, "stranded name released: its host has no live key");
            released.push(name);
        }
    }
    Ok(released)
}

pub fn list(db: &dyn Tx) -> StoreResult<Vec<KeyInfo>> {
    Ok(db
        .credentials()?
        .into_iter()
        .map(|c| KeyInfo {
            key_id: key_id(&c.digest),
            user: c.user,
            host: c.host,
            label: c.label,
            created_at: c.created_at,
            revoked_at: c.revoked_at,
        })
        .collect())
}

/// (user_id, host_id, whether the user is disabled) of an unrevoked key with this digest.
pub fn live_credential(db: &dyn Tx, digest: &str) -> StoreResult<Option<(i64, String, bool)>> {
    db.live_credential(digest)
}
