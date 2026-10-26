//! Hosts and their device credentials: one revocable bearer key per enrolled
//! host, stored as a sha256 digest only.

use anyhow::{Context, bail};

use crate::store::{StoreResult, Tx};

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
pub fn insert_key(db: &dyn Tx, user: &str, host: &str, digest: &str, label: &str, now: &str) -> anyhow::Result<()> {
    let (user, host) = (user.to_lowercase(), host.to_lowercase());
    if host == conde_remote::web::BROWSER_HOST || host == conde_remote::SLACK_INSTANCE {
        bail!("host id '{host}' is reserved; choose another");
    }
    let user_id = db.ensure_user(&user)?;
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
pub fn revoke(db: &dyn Tx, host: Option<&str>, key_id: Option<&str>, now: &str) -> anyhow::Result<Vec<KeyInfo>> {
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
    Ok(matching)
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

/// (user_id, host_id) of a live key with this digest.
pub fn authenticate(db: &dyn Tx, digest: &str) -> StoreResult<Option<(i64, String)>> {
    db.authenticate(digest)
}
