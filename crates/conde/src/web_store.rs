//! The web surface's storage (conde-remote's `WebStore`) over the board's store: Entra
//! identities, browser sessions, one-time records (login transactions, `conde login` codes,
//! key displays), host keys and disabled users. Every method is one transaction; writes that
//! revoke keys or disable a user announce the hosts on commit, so every replica closes them.

use std::sync::Arc;

use conde_remote::web::{
    AdminKey, BindUser, CliCode, CreateUser, HostKey, Identity, KeyDisplay, Limits, LoginTxn, McpAccess, McpCode,
    McpCodeState, McpCreated, McpGrant, McpHuman, McpRedeemed, McpRefreshed, McpTokens, Minted, SessionRecord, WebResult, WebStore,
    WebStoreError, WebUser,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dispatch::Dispatcher;
use crate::ops::hosts::key_id;
use crate::store::{CredentialRow, McpCodeRow, McpGrantRow, StoreError, Tx, UserRow, WebSessionRow};
use crate::util::iso_from_ms;

const LOGIN: &str = "login";
const CLI_CODE: &str = "cli_code";
const KEY_DISPLAY: &str = "key_display";

pub struct WebBoard {
    d: Arc<Dispatcher>,
}

impl WebBoard {
    pub fn new(d: Arc<Dispatcher>) -> WebBoard {
        WebBoard { d }
    }

    fn read<T>(&self, f: impl FnOnce(&dyn Tx) -> Result<T, StoreError>) -> WebResult<T> {
        self.d.with_conn(f).map_err(|e| WebStoreError(e.to_string()))
    }

    fn write<T>(&self, f: impl FnOnce(&dyn Tx) -> anyhow::Result<T>) -> WebResult<T> {
        self.d.with_tx(f).map_err(|e| WebStoreError(format!("{e:#}")))
    }

    fn put<T: Serialize>(&self, kind: &str, digest: &str, value: &T, created_at: i64) -> WebResult<()> {
        let payload = serde_json::to_string(value).map_err(|e| WebStoreError(e.to_string()))?;
        self.write(|t| Ok(t.put_web_token(kind, digest, &payload, created_at)?))
    }

    fn take<T: DeserializeOwned>(&self, kind: &str, digest: &str) -> WebResult<Option<T>> {
        let Some((payload, _)) = self.write(|t| Ok(t.take_web_token(kind, digest)?))? else {
            return Ok(None);
        };
        serde_json::from_str(&payload).map(Some).map_err(|e| WebStoreError(e.to_string()))
    }
}

fn iso(secs: i64) -> String {
    iso_from_ms(secs * 1000)
}

fn web_user(u: UserRow) -> WebUser {
    WebUser { user_id: u.user_id, name: u.name, disabled: u.disabled }
}

fn grant(g: McpGrantRow) -> McpGrant {
    McpGrant {
        grant_id: g.grant_id,
        user_id: g.user_id,
        client_id: g.client_id,
        client_host: g.client_host,
        client_name: g.client_name,
        resource: g.resource,
        created_at: g.created_at,
        last_used_at: g.last_used_at,
        expires_at: g.expires_at,
        revoked_at: g.revoked_at,
    }
}

/// A grant's last use is written at most this often (seconds).
const TOUCH_EVERY: i64 = 60;

/// Stores a token pair of the grant, each capped at the grant's end; the access token's expiry.
fn grant_row(g: &McpGrant) -> McpGrantRow {
    McpGrantRow {
        grant_id: g.grant_id.clone(),
        user_id: g.user_id,
        client_id: g.client_id.clone(),
        client_host: g.client_host.clone(),
        client_name: g.client_name.clone(),
        resource: g.resource.clone(),
        created_at: g.created_at,
        last_used_at: g.last_used_at,
        expires_at: g.expires_at,
        revoked_at: None,
    }
}

/// Stores the grant with its first token pair unless the user is disabled, first revoking the
/// user's oldest live grants of the same client beyond `max_live`.
fn create_grant(
    t: &dyn Tx,
    row: &McpGrantRow,
    code_digest: &str,
    tokens: &McpTokens,
    max_live: u32,
    now: i64,
) -> Result<Option<McpCreated>, StoreError> {
    if t.user_by_id(row.user_id)?.is_none_or(|u| u.disabled) {
        return Ok(None);
    }
    // Room for the new grant: the user's live grants of this client beyond the cap end,
    // oldest first (newest first from the store; ties broken by grant_id).
    let live: Vec<McpGrantRow> = t
        .mcp_grants_of(row.user_id)?
        .into_iter()
        .filter(|o| o.client_id == row.client_id && o.revoked_at.is_none() && o.expires_at > now)
        .collect();
    let mut revoked = Vec::new();
    for old in live.into_iter().skip(max_live.max(1) as usize - 1).rev() {
        if t.revoke_mcp_grant(&old.grant_id, now)? {
            revoked.push(old.grant_id);
        }
    }
    t.insert_mcp_grant(row)?;
    t.set_mcp_grant_code(&row.grant_id, code_digest)?;
    Ok(Some(McpCreated { access_expires_at: store_tokens(t, row, tokens, now)?, revoked }))
}

fn store_tokens(t: &dyn Tx, g: &McpGrantRow, tokens: &McpTokens, now: i64) -> Result<i64, StoreError> {
    let access_expires_at = (now + tokens.access_ttl).min(g.expires_at);
    t.insert_mcp_token(&tokens.access_digest, &g.grant_id, &g.resource, access_expires_at)?;
    t.insert_mcp_refresh(&tokens.refresh_digest, &g.grant_id, now, (now + tokens.refresh_ttl).min(g.expires_at))?;
    Ok(access_expires_at)
}

fn host_key(t: &dyn Tx, c: CredentialRow) -> Result<HostKey, StoreError> {
    Ok(HostKey {
        key_id: key_id(&c.digest),
        last_seen: t.host_last_seen(&c.host)?,
        host_id: c.host,
        label: c.label,
        created_at: c.created_at,
        revoked_at: c.revoked_at,
    })
}

impl WebStore for WebBoard {
    fn user_by_identity(&self, tid: &str, oid: &str) -> WebResult<Option<WebUser>> {
        Ok(self.read(|t| t.user_by_identity(tid, oid))?.map(web_user))
    }

    fn create_user(&self, identity: &Identity, name: &str) -> WebResult<CreateUser> {
        self.write(|t| {
            if let Some(u) = t.user_by_name(name)? {
                return Ok(CreateUser::NameTaken { bound: u.bound });
            }
            let user_id = t.ensure_user(name)?;
            t.bind_identity(
                user_id,
                &identity.tid,
                &identity.oid,
                identity.display_name.as_deref(),
                identity.preferred_username.as_deref(),
            )?;
            Ok(CreateUser::Created(WebUser { user_id, name: name.to_string(), disabled: false }))
        })
    }

    fn set_profile(&self, user_id: i64, identity: &Identity) -> WebResult<()> {
        self.write(|t| {
            Ok(t.set_identity_profile(
                user_id,
                identity.display_name.as_deref(),
                identity.preferred_username.as_deref(),
            )?)
        })
    }

    fn user_disabled(&self, user_id: i64) -> WebResult<bool> {
        Ok(self.read(|t| t.user_by_id(user_id))?.is_none_or(|u| u.disabled))
    }

    fn insert_session(&self, id_digest: &str, s: &SessionRecord) -> WebResult<bool> {
        let row = WebSessionRow {
            user_id: s.user_id,
            user_name: s.user_name.clone(),
            display: s.display.clone(),
            csrf: s.csrf.clone(),
            created_at: s.created_at,
            last_seen: s.last_seen,
        };
        self.write(|t| {
            if t.user_by_id(row.user_id)?.is_none_or(|u| u.disabled) {
                return Ok(false);
            }
            t.insert_web_session(id_digest, &row)?;
            Ok(true)
        })
    }

    fn session(&self, id_digest: &str) -> WebResult<Option<SessionRecord>> {
        Ok(self.read(|t| t.web_session(id_digest))?.map(|r| SessionRecord {
            user_id: r.user_id,
            user_name: r.user_name,
            display: r.display,
            csrf: r.csrf,
            created_at: r.created_at,
            last_seen: r.last_seen,
        }))
    }

    fn touch_session(&self, id_digest: &str, last_seen: i64) -> WebResult<()> {
        self.write(|t| Ok(t.touch_web_session(id_digest, last_seen)?))
    }

    fn delete_session(&self, id_digest: &str) -> WebResult<()> {
        self.write(|t| Ok(t.delete_web_session(id_digest)?))
    }

    fn insert_login(&self, state_digest: &str, txn: &LoginTxn) -> WebResult<()> {
        self.put(LOGIN, state_digest, txn, txn.created_at)
    }

    fn take_login(&self, state_digest: &str) -> WebResult<Option<LoginTxn>> {
        self.take(LOGIN, state_digest)
    }

    fn insert_cli_code(&self, code_digest: &str, code: &CliCode) -> WebResult<()> {
        self.put(CLI_CODE, code_digest, code, code.created_at)
    }

    fn take_cli_code(&self, code_digest: &str) -> WebResult<Option<CliCode>> {
        self.take(CLI_CODE, code_digest)
    }

    fn insert_key_display(&self, token_digest: &str, display: &KeyDisplay) -> WebResult<()> {
        self.put(KEY_DISPLAY, token_digest, display, display.created_at)
    }

    fn take_key_display(&self, token_digest: &str) -> WebResult<Option<KeyDisplay>> {
        self.take(KEY_DISPLAY, token_digest)
    }

    fn purge_expired(&self, now: i64, l: &Limits) -> WebResult<()> {
        let ttls = [(LOGIN, l.login_ttl), (CLI_CODE, l.cli_code_ttl), (KEY_DISPLAY, l.key_display_ttl)];
        self.write(|t| Ok(t.purge_web(now, l.session_idle, l.session_max, &ttls)?))
    }

    fn host_keys(&self, user_id: i64) -> WebResult<Vec<HostKey>> {
        self.read(|t| {
            let Some(user) = t.user_by_id(user_id)? else { return Ok(Vec::new()) };
            let mut keys = Vec::new();
            for c in t.credentials()?.into_iter().filter(|c| c.user == user.name) {
                keys.push(host_key(t, c)?);
            }
            keys.reverse();
            Ok(keys)
        })
    }

    fn host_owner(&self, host_id: &str) -> WebResult<Option<i64>> {
        Ok(self.read(|t| t.host_owner(host_id))?.map(|(id, _)| id))
    }

    fn mint_host_key(
        &self,
        user_id: i64,
        host_id: &str,
        label: &str,
        token_digest: &str,
        now: i64,
    ) -> WebResult<Minted> {
        self.write(|t| {
            if t.user_by_id(user_id)?.is_none_or(|u| u.disabled) {
                return Ok(Minted::Disabled);
            }
            let new_host = t.host_owner(host_id)?.is_none();
            if t.ensure_host(host_id, user_id)? != user_id {
                return Ok(Minted::HostTaken);
            }
            let at = iso(now);
            let replaced = t.revoke_host_credentials(host_id, &at)?;
            t.insert_credential(token_digest, user_id, host_id, label, &at)?;
            Ok(Minted::Stored { replaced, new_host })
        })
    }

    fn revoke_host(&self, user_id: i64, host_id: &str, now: i64) -> WebResult<u64> {
        self.write(|t| match t.host_owner(host_id)? {
            Some((owner, _)) if owner == user_id => Ok(t.revoke_host_credentials(host_id, &iso(now))?),
            _ => Ok(0),
        })
    }

    fn ensure_user(&self, name: &str) -> WebResult<WebUser> {
        self.write(|t| {
            let user_id = t.ensure_user(name)?;
            let user = t.user_by_id(user_id)?.ok_or_else(|| anyhow::anyhow!("user {name} vanished"))?;
            Ok(web_user(user))
        })
    }

    fn bind_identity(&self, name: &str, identity: &Identity) -> WebResult<BindUser> {
        self.write(|t| {
            let bind = |user_id| {
                t.bind_identity(
                    user_id,
                    &identity.tid,
                    &identity.oid,
                    identity.display_name.as_deref(),
                    identity.preferred_username.as_deref(),
                )
            };
            Ok(match t.user_by_name(name)? {
                Some(u) if u.bound => BindUser::NameBound,
                Some(u) => {
                    bind(u.user_id)?;
                    BindUser::Bound(web_user(u))
                }
                None => {
                    let user_id = t.ensure_user(name)?;
                    bind(user_id)?;
                    BindUser::Created(WebUser { user_id, name: name.to_string(), disabled: false })
                }
            })
        })
    }

    fn all_keys(&self) -> WebResult<Vec<AdminKey>> {
        self.read(|t| {
            let mut keys = Vec::new();
            for c in t.credentials()? {
                let user = c.user.clone();
                keys.push(AdminKey { user, key: host_key(t, c)? });
            }
            keys.reverse();
            Ok(keys)
        })
    }

    fn revoke_key(&self, key: &str, now: i64) -> WebResult<Option<String>> {
        self.write(|t| {
            let live: Vec<CredentialRow> = t
                .credentials()?
                .into_iter()
                .filter(|c| c.revoked_at.is_none() && key_id(&c.digest) == key)
                .collect();
            let [one] = &live[..] else { return Ok(None) };
            t.revoke_credential(key, &iso(now))?;
            Ok(Some(one.host.clone()))
        })
    }

    fn set_disabled(&self, name: &str, disabled: bool) -> WebResult<Option<Vec<String>>> {
        self.write(|t| match t.user_by_name(name)? {
            Some(u) => Ok(Some(t.set_user_disabled(u.user_id, disabled)?)),
            None => Ok(None),
        })
    }

    fn insert_mcp_code(&self, code_digest: &str, c: &McpCode) -> WebResult<()> {
        let row = McpCodeRow {
            user_id: c.user_id,
            client_id: c.client_id.clone(),
            client_name: c.client_name.clone(),
            redirect_uri: c.redirect_uri.clone(),
            challenge: c.challenge.clone(),
            resource: c.resource.clone(),
            created_at: c.created_at,
        };
        self.write(|t| Ok(t.insert_mcp_code(code_digest, &row)?))
    }

    fn take_mcp_code(&self, code_digest: &str) -> WebResult<Option<McpCode>> {
        Ok(self.write(|t| Ok(t.take_mcp_code(code_digest)?))?.map(|c| McpCode {
            user_id: c.user_id,
            client_id: c.client_id,
            client_name: c.client_name,
            redirect_uri: c.redirect_uri,
            challenge: c.challenge,
            resource: c.resource,
            created_at: c.created_at,
        }))
    }

    fn create_mcp_grant(
        &self,
        g: &McpGrant,
        code_digest: &str,
        tokens: &McpTokens,
        max_live: u32,
        now: i64,
    ) -> WebResult<Option<McpCreated>> {
        let row = grant_row(g);
        self.write(|t| Ok(create_grant(t, &row, code_digest, tokens, max_live, now)?))
    }

    fn redeem_mcp_code(
        &self,
        code_digest: &str,
        g: &McpGrant,
        tokens: &McpTokens,
        max_live: u32,
        now: i64,
    ) -> WebResult<McpRedeemed> {
        let row = grant_row(g);
        self.write(|t| {
            if t.take_mcp_code(code_digest)?.is_none() {
                return Ok(McpRedeemed::Taken);
            }
            // Committed either way: a refused redemption still spends the code.
            Ok(match create_grant(t, &row, code_digest, tokens, max_live, now)? {
                Some(created) => McpRedeemed::Created(created),
                None => McpRedeemed::Disabled,
            })
        })
    }

    fn mcp_access(&self, token_digest: &str, now: i64) -> WebResult<Option<McpAccess>> {
        let found = self.read(|t| {
            let Some((grant_id, audience, expires_at)) = t.mcp_token(token_digest)? else { return Ok(None) };
            let Some(g) = t.mcp_grant(&grant_id)? else { return Ok(None) };
            let Some(user) = t.user_by_id(g.user_id)? else { return Ok(None) };
            Ok(Some(McpAccess { grant: grant(g), user: user.name, disabled: user.disabled, audience, expires_at }))
        })?;
        if let Some(a) = &found
            && a.grant.revoked_at.is_none()
            && a.grant.last_used_at.is_none_or(|at| now - at >= TOUCH_EVERY)
        {
            let id = a.grant.grant_id.clone();
            self.write(|t| Ok(t.touch_mcp_grant(&id, now)?))?;
        }
        Ok(found)
    }

    fn refresh_mcp_grant(
        &self,
        refresh_digest: &str,
        client_id: &str,
        tokens: &McpTokens,
        now: i64,
    ) -> WebResult<McpRefreshed> {
        self.write(|t| {
            let Some(r) = t.mcp_refresh(refresh_digest)? else { return Ok(McpRefreshed::Invalid) };
            let Some(g) = t.mcp_grant(&r.grant_id)? else { return Ok(McpRefreshed::Invalid) };
            if g.client_id != client_id {
                return Ok(McpRefreshed::Invalid);
            }
            if r.consumed_at.is_some() {
                t.revoke_mcp_grant(&g.grant_id, now)?;
                return Ok(McpRefreshed::Reused { grant: grant(g) });
            }
            let disabled = t.user_by_id(g.user_id)?.is_none_or(|u| u.disabled);
            if g.revoked_at.is_some() || g.expires_at <= now || r.expires_at <= now || disabled {
                return Ok(McpRefreshed::Invalid);
            }
            // Consumed by a concurrent rotation since the read: the family is spent all the same.
            if !t.consume_mcp_refresh(refresh_digest, now)? {
                t.revoke_mcp_grant(&g.grant_id, now)?;
                return Ok(McpRefreshed::Reused { grant: grant(g) });
            }
            let access_expires_at = store_tokens(t, &g, tokens, now)?;
            Ok(McpRefreshed::Rotated { grant: grant(g), access_expires_at })
        })
    }

    fn mcp_grants(&self, user_id: i64, now: i64) -> WebResult<Vec<McpGrant>> {
        Ok(self
            .read(|t| t.mcp_grants_of(user_id))?
            .into_iter()
            .filter(|g| g.revoked_at.is_none() && g.expires_at > now)
            .map(grant)
            .collect())
    }

    fn revoke_mcp_grant(&self, user_id: i64, grant_id: &str, now: i64) -> WebResult<bool> {
        self.write(|t| match t.mcp_grant(grant_id)? {
            Some(g) if g.user_id == user_id => Ok(t.revoke_mcp_grant(grant_id, now)?),
            _ => Ok(false),
        })
    }

    fn mcp_code_state(&self, code_digest: &str) -> WebResult<McpCodeState> {
        self.read(|t| {
            Ok(if let Some(c) = t.mcp_code(code_digest)? {
                McpCodeState::Issued(McpCode {
                    user_id: c.user_id,
                    client_id: c.client_id,
                    client_name: c.client_name,
                    redirect_uri: c.redirect_uri,
                    challenge: c.challenge,
                    resource: c.resource,
                    created_at: c.created_at,
                })
            } else if t.mcp_grant_of_code(code_digest)?.is_some() {
                McpCodeState::Redeemed
            } else {
                McpCodeState::Unknown
            })
        })
    }

    fn revoke_mcp_token(&self, token_digest: &str, client_id: &str, now: i64) -> WebResult<Option<(McpGrant, bool)>> {
        self.write(|t| {
            let grant_id = match t.mcp_token(token_digest)? {
                Some((grant_id, _, _)) => grant_id,
                None => match t.mcp_refresh(token_digest)? {
                    Some(r) => r.grant_id,
                    None => return Ok(None),
                },
            };
            match t.mcp_grant(&grant_id)? {
                Some(g) if g.client_id == client_id => {
                    let revoked = t.revoke_mcp_grant(&grant_id, now)?;
                    Ok(t.mcp_grant(&grant_id)?.map(|g| (grant(g), revoked)))
                }
                _ => Ok(None),
            }
        })
    }

    fn revoke_mcp_code_grant(
        &self,
        code_digest: &str,
        client_id: &str,
        now: i64,
    ) -> WebResult<Option<(McpGrant, bool)>> {
        self.write(|t| {
            let Some(grant_id) = t.mcp_grant_of_code(code_digest)? else { return Ok(None) };
            match t.mcp_grant(&grant_id)? {
                Some(g) if g.client_id == client_id => {
                    let revoked = t.revoke_mcp_grant(&grant_id, now)?;
                    Ok(t.mcp_grant(&grant_id)?.map(|g| (grant(g), revoked)))
                }
                _ => Ok(None),
            }
        })
    }

    fn mcp_human(&self, user_id: i64) -> WebResult<McpHuman> {
        let board = self.d.shared_name().unwrap_or("this board").to_string();
        let found = self.read(|t| crate::mcp_remote::human_row(t, user_id, &board))?;
        Ok(match found {
            Ok(a) => McpHuman::One(a.canonical()),
            Err(why) => McpHuman::Refused(why),
        })
    }

    fn purge_mcp(&self, now: i64, code_ttl: i64) -> WebResult<()> {
        self.write(|t| Ok(t.purge_mcp(now, code_ttl)?))
    }
}
