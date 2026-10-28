//! The web surface's storage (conde-remote's `WebStore`) over the board's store: sign-in
//! identities and invites, browser sessions, one-time records (login transactions, `conde login` codes,
//! key displays), host keys and disabled users. Every method is one transaction; writes that
//! revoke keys or disable a user announce the hosts on commit, so every replica closes them.

use std::sync::Arc;

use conde_remote::web::{
    AdminKey, AdminUser, BindUser, CliCode, CliIssued, CreateUser, HostKey, Identity, Invite, Invited, KeyDisplay, Limits,
    LoginTxn, Minted, SessionRecord, WebResult, WebStore, WebStoreError, WebUser,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dispatch::Dispatcher;
use crate::ops::hosts::key_id;
use crate::store::{CredentialRow, StoreError, Tx, UserRow, WebSessionRow};
use crate::util::iso_from_ms;

const LOGIN: &str = "login";
const CLI_CODE: &str = "cli_code";
const KEY_DISPLAY: &str = "key_display";
/// When a pasted-code login was first confirmed, by the login's key (no payload).
const CLI_LOGIN: &str = "cli_login";
/// How long that time is kept: past the code's own purge and any CLI wait, so a late confirm is
/// answered by the login's age whether or not its record was purged.
const CLI_LOGIN_KEEP: i64 = 86_400;

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

/// Binds the identity to the user and records its verified email (never an unverified claim,
/// never clearing a stored one: the email is what an invite matches).
fn bind(t: &dyn Tx, user_id: i64, identity: &Identity) -> Result<(), StoreError> {
    t.bind_identity(
        user_id,
        &identity.issuer,
        &identity.subject,
        identity.display_name.as_deref(),
        identity.preferred_username.as_deref(),
    )?;
    match identity.verified_email.as_deref() {
        Some(email) => t.set_user_email(user_id, Some(email)),
        None => Ok(()),
    }
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
    fn user_by_identity(&self, issuer: &str, subject: &str) -> WebResult<Option<WebUser>> {
        Ok(self.read(|t| t.user_by_identity(issuer, subject))?.map(web_user))
    }

    fn claim_invite(&self, identity: &Identity, email: &str) -> WebResult<Invite> {
        self.write(|t| {
            if let Some(u) = t.user_by_identity(&identity.issuer, &identity.subject)? {
                return Ok(Invite::Bound(web_user(u)));
            }
            let users = t.users_by_email(email)?;
            let [user] = &users[..] else {
                return Ok(if users.is_empty() { Invite::NoUser } else { Invite::Ambiguous });
            };
            if t.identity_issuers(user.user_id)?.contains(&identity.issuer) {
                return Ok(Invite::IssuerBound);
            }
            bind(t, user.user_id, identity)?;
            Ok(Invite::Bound(web_user(user.clone())))
        })
        .inspect(|i| {
            if let Invite::Bound(u) = i {
                self.d.request_slack_lookup(u.user_id);
            }
        })
    }

    fn create_user(&self, identity: &Identity, name: &str) -> WebResult<CreateUser> {
        self.write(|t| {
            if let Some(u) = t.user_by_name(name)? {
                return Ok(CreateUser::NameTaken { bound: u.bound });
            }
            let user_id = t.ensure_user(name)?;
            bind(t, user_id, identity)?;
            crate::humans::ensure(t, user_id)?;
            Ok(CreateUser::Created(WebUser { user_id, name: name.to_string(), disabled: false }))
        })
        .inspect(|c| {
            if let CreateUser::Created(u) = c {
                self.d.request_slack_lookup(u.user_id);
            }
        })
    }

    fn set_profile(&self, user_id: i64, identity: &Identity) -> WebResult<()> {
        self.write(|t| {
            if let Some(email) = identity.verified_email.as_deref() {
                t.set_user_email(user_id, Some(email))?;
            }
            Ok(t.set_identity_profile(
                &identity.issuer,
                &identity.subject,
                identity.display_name.as_deref(),
                identity.preferred_username.as_deref(),
            )?)
        })?;
        self.d.request_slack_lookup(user_id);
        Ok(())
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

    fn issue_cli_code(&self, code_digest: &str, code: &CliCode, ttl: i64) -> WebResult<CliIssued> {
        let payload = serde_json::to_string(code).map_err(|e| WebStoreError(e.to_string()))?;
        self.write(|t| {
            let held = t.take_web_token(CLI_CODE, code_digest)?;
            let old = held.as_ref().and_then(|(p, _)| serde_json::from_str::<CliCode>(p).ok());
            let first = match t.take_web_token(CLI_LOGIN, code_digest)? {
                Some((_, at)) => at,
                // a record from before the first confirm's time was kept dates the login too
                None => old.as_ref().map_or(code.created_at, |o| o.created_at.min(code.created_at)),
            };
            t.put_web_token(CLI_LOGIN, code_digest, "", first)?;
            let answer = if code.created_at - first > ttl {
                CliIssued::Expired
            } else if old.as_ref().is_some_and(|o| o.user_id != code.user_id) {
                CliIssued::Theirs
            } else {
                t.put_web_token(CLI_CODE, code_digest, &payload, code.created_at)?;
                return Ok(CliIssued::Issued);
            };
            if let Some((held, created_at)) = held {
                t.put_web_token(CLI_CODE, code_digest, &held, created_at)?;
            }
            Ok(answer)
        })
    }

    fn restore_cli_code(&self, code_digest: &str, code: &CliCode) -> WebResult<()> {
        let payload = serde_json::to_string(code).map_err(|e| WebStoreError(e.to_string()))?;
        self.write(|t| {
            match t.take_web_token(CLI_CODE, code_digest)? {
                Some((newer, created_at))
                    if serde_json::from_str::<CliCode>(&newer).is_ok_and(|n| n.user_id == code.user_id) =>
                {
                    t.put_web_token(CLI_CODE, code_digest, &newer, created_at)?
                }
                _ => t.put_web_token(CLI_CODE, code_digest, &payload, code.created_at)?,
            }
            Ok(())
        })
    }

    fn end_cli_code(&self, code_digest: &str, user_id: i64) -> WebResult<()> {
        self.write(|t| {
            if let Some((held, created_at)) = t.take_web_token(CLI_CODE, code_digest)?
                && serde_json::from_str::<CliCode>(&held).is_ok_and(|h| h.user_id == user_id)
            {
                t.put_web_token(CLI_CODE, code_digest, &held, created_at)?;
            }
            Ok(())
        })
    }

    fn insert_key_display(&self, token_digest: &str, display: &KeyDisplay) -> WebResult<()> {
        self.put(KEY_DISPLAY, token_digest, display, display.created_at)
    }

    fn take_key_display(&self, token_digest: &str) -> WebResult<Option<KeyDisplay>> {
        self.take(KEY_DISPLAY, token_digest)
    }

    fn purge_expired(&self, now: i64, l: &Limits) -> WebResult<()> {
        let ttls = [
            (LOGIN, l.login_ttl),
            (CLI_CODE, l.cli_code_ttl),
            (CLI_LOGIN, CLI_LOGIN_KEEP.max(l.cli_code_ttl)),
            (KEY_DISPLAY, l.key_display_ttl),
        ];
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
            crate::humans::ensure(t, user_id)?;
            let user = t.user_by_id(user_id)?.ok_or_else(|| anyhow::anyhow!("user {name} vanished"))?;
            Ok(web_user(user))
        })
    }

    fn bind_identity(&self, name: &str, identity: &Identity) -> WebResult<BindUser> {
        self.write(|t| {
            Ok(match t.user_by_name(name)? {
                Some(u) if t.identity_issuers(u.user_id)?.contains(&identity.issuer) => BindUser::NameBound,
                Some(u) => {
                    bind(t, u.user_id, identity)?;
                    BindUser::Bound(web_user(u))
                }
                None => {
                    let user_id = t.ensure_user(name)?;
                    crate::humans::ensure(t, user_id)?;
                    bind(t, user_id, identity)?;
                    BindUser::Created(WebUser { user_id, name: name.to_string(), disabled: false })
                }
            })
        })
    }

    fn invite_user(&self, name: &str, email: &str) -> WebResult<Invited> {
        self.write(|t| {
            let holder = t.users_by_email(email)?.into_iter().find(|u| u.name != name);
            if let Some(other) = holder {
                return Ok(Invited::EmailTaken(other.name));
            }
            Ok(match t.user_by_name(name)? {
                Some(u) => match u.email.clone() {
                    Some(have) if have.eq_ignore_ascii_case(email) => Invited::Exists(web_user(u)),
                    Some(have) => Invited::OtherEmail(have),
                    None => {
                        t.set_user_email(u.user_id, Some(email))?;
                        Invited::EmailSet(web_user(u))
                    }
                },
                None => {
                    let user_id = t.ensure_user(name)?;
                    crate::humans::ensure(t, user_id)?;
                    t.set_user_email(user_id, Some(email))?;
                    Invited::Created(WebUser { user_id, name: name.to_string(), disabled: false })
                }
            })
        })
        .inspect(|i| {
            if let Invited::Created(u) | Invited::EmailSet(u) = i {
                self.d.request_slack_lookup(u.user_id);
            }
        })
    }

    fn list_users(&self) -> WebResult<Vec<AdminUser>> {
        self.read(|t| {
            let mut out = Vec::new();
            for u in t.users()? {
                out.push(AdminUser {
                    issuers: t.identity_issuers(u.user_id)?,
                    name: u.name,
                    email: u.email,
                    disabled: u.disabled,
                });
            }
            Ok(out)
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

    fn set_slack_member(&self, name: &str, member_id: &str) -> WebResult<Option<WebUser>> {
        self.write(|t| match t.user_by_name(name)? {
            Some(u) => {
                t.set_user_slack(u.user_id, member_id, "admin")?;
                Ok(Some(web_user(u)))
            }
            None => Ok(None),
        })
    }

    fn set_disabled(&self, name: &str, disabled: bool) -> WebResult<Option<Vec<String>>> {
        self.write(|t| match t.user_by_name(name)? {
            Some(u) => Ok(Some(t.set_user_disabled(u.user_id, disabled)?)),
            None => Ok(None),
        })
    }
}
