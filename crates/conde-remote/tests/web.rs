//! The web surface (OIDC sign-in and the invite gate, keys page, `conde login` browser leg)
//! against an in-process fake OpenID provider, an in-memory store and a fake bridge registry.

use conde_remote::token_digest;
use conde_remote::web;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, sha2::Sha256};
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;

use web::{
    AdminKey, AdminUser, BindUser, BridgeRegistry, CliCode, CliIssued, CreateUser, HostKey, Identity, Invite, Invited,
    KeyDisplay, Limits, LoginTxn, Minted, SessionRecord, WebConfig, WebResult, WebState, WebStore, WebStoreError,
    WebUser,
};

include!("support/web_harness.rs");

// --- in-memory store ---------------------------------------------------------------------

struct User {
    id: i64,
    name: String,
    /// (issuer, subject), at most one per issuer.
    identities: Vec<(String, String)>,
    email: Option<String>,
    display: Option<String>,
    disabled: bool,
}

impl User {
    fn web(&self) -> WebUser {
        WebUser {
            user_id: self.id,
            name: self.name.clone(),
            disabled: self.disabled,
        }
    }
}

struct Cred {
    digest: String,
    user_id: i64,
    host: String,
    label: String,
    created_at: i64,
    revoked_at: Option<i64>,
}

#[derive(Default)]
struct Mem {
    users: Vec<User>,
    sessions: HashMap<String, SessionRecord>,
    logins: HashMap<String, LoginTxn>,
    codes: HashMap<String, CliCode>,
    /// When each pasted-code login was first confirmed (kept past its record, as on the stores).
    cli_logins: HashMap<String, i64>,
    displays: HashMap<String, KeyDisplay>,
    hosts: HashMap<String, i64>,
    creds: Vec<Cred>,
    /// A pause for a `conde login` record: in `take_cli_code` after the take (outside
    /// the lock), in `issue_cli_code` inside its one transaction (holding the lock).
    cli_gate: Option<TakeGate>,
}

/// A pause after a code is taken: signals `taken`, then waits for `release` (or gives up).
struct TakeGate {
    taken: Arc<tokio::sync::Notify>,
    release: std::sync::mpsc::Receiver<()>,
}

impl TakeGate {
    fn hold(self) {
        self.taken.notify_one();
        let _ = self.release.recv_timeout(Duration::from_secs(5));
    }
}

type UserRow = (i64, Option<(String, String)>, Option<String>);

#[derive(Default)]
struct MemStore(Mutex<Mem>);

impl MemStore {
    fn m(&self) -> std::sync::MutexGuard<'_, Mem> {
        self.0.lock().unwrap()
    }
    /// An admin-created user (no Entra identity), as `conde admin key create` makes them.
    fn add_plain_user(&self, name: &str) -> i64 {
        let mut m = self.m();
        let id = m.users.len() as i64 + 1;
        m.users.push(User {
            id,
            name: name.into(),
            identities: Vec::new(),
            email: None,
            display: None,
            disabled: false,
        });
        id
    }
    /// (user id, first bound identity, display name) of the user named `name`.
    fn user(&self, name: &str) -> Option<UserRow> {
        self.m()
            .users
            .iter()
            .find(|u| u.name == name)
            .map(|u| (u.id, u.identities.first().cloned(), u.display.clone()))
    }
    fn live_host_of(&self, key: &str) -> Option<String> {
        let d = token_digest(key);
        self.m()
            .creds
            .iter()
            .find(|c| c.digest == d && c.revoked_at.is_none())
            .map(|c| c.host.clone())
    }
    /// Sets a user's email directly (a store may hold one address on two users).
    fn set_email(&self, name: &str, email: &str) {
        self.m().users.iter_mut().find(|u| u.name == name).unwrap().email = Some(email.into());
    }
    fn creds_of(&self, host: &str) -> Vec<(String, Option<i64>)> {
        self.m()
            .creds
            .iter()
            .filter(|c| c.host == host)
            .map(|c| (c.label.clone(), c.revoked_at))
            .collect()
    }
}

impl WebStore for MemStore {
    fn user_by_identity(&self, issuer: &str, subject: &str) -> WebResult<Option<WebUser>> {
        let id = (issuer.to_string(), subject.to_string());
        Ok(self
            .m()
            .users
            .iter()
            .find(|u| u.identities.contains(&id))
            .map(User::web))
    }
    fn claim_invite(&self, identity: &Identity, email: &str) -> WebResult<Invite> {
        let key = (identity.issuer.clone(), identity.subject.clone());
        let mut m = self.m();
        if let Some(u) = m.users.iter().find(|u| u.identities.contains(&key)) {
            return Ok(Invite::Bound(u.web()));
        }
        let hits: Vec<usize> = (0..m.users.len())
            .filter(|&i| m.users[i].email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email)))
            .collect();
        let [i] = hits[..] else {
            return Ok(if hits.is_empty() { Invite::NoUser } else { Invite::Ambiguous });
        };
        let user = &mut m.users[i];
        if user.identities.iter().any(|(i, _)| *i == identity.issuer) {
            return Ok(Invite::IssuerBound);
        }
        user.identities.push(key);
        user.display = identity.display_name.clone();
        Ok(Invite::Bound(user.web()))
    }
    fn create_user(&self, identity: &Identity, name: &str) -> WebResult<CreateUser> {
        let mut m = self.m();
        if let Some(u) = m.users.iter().find(|u| u.name == name) {
            return Ok(CreateUser::NameTaken {
                bound: !u.identities.is_empty(),
            });
        }
        let id = m.users.len() as i64 + 1;
        m.users.push(User {
            id,
            name: name.into(),
            identities: vec![(identity.issuer.clone(), identity.subject.clone())],
            email: identity.email.clone(),
            display: identity.display_name.clone(),
            disabled: false,
        });
        Ok(CreateUser::Created(m.users.last().unwrap().web()))
    }
    fn user_disabled(&self, user_id: i64) -> WebResult<bool> {
        Ok(self
            .m()
            .users
            .iter()
            .find(|u| u.id == user_id)
            .is_none_or(|u| u.disabled))
    }
    fn set_profile(&self, user_id: i64, identity: &Identity) -> WebResult<()> {
        if let Some(u) = self.m().users.iter_mut().find(|u| u.id == user_id) {
            u.display = identity.display_name.clone();
        }
        Ok(())
    }
    fn insert_session(&self, id_digest: &str, session: &SessionRecord) -> WebResult<bool> {
        if self.user_disabled(session.user_id)? {
            return Ok(false);
        }
        self.m().sessions.insert(id_digest.into(), session.clone());
        Ok(true)
    }
    fn session(&self, id_digest: &str) -> WebResult<Option<SessionRecord>> {
        Ok(self.m().sessions.get(id_digest).cloned())
    }
    fn touch_session(&self, id_digest: &str, last_seen: i64) -> WebResult<()> {
        if let Some(s) = self.m().sessions.get_mut(id_digest) {
            s.last_seen = last_seen;
        }
        Ok(())
    }
    fn delete_session(&self, id_digest: &str) -> WebResult<()> {
        self.m().sessions.remove(id_digest);
        Ok(())
    }
    fn insert_login(&self, state_digest: &str, txn: &LoginTxn) -> WebResult<()> {
        self.m().logins.insert(state_digest.into(), txn.clone());
        Ok(())
    }
    fn take_login(&self, state_digest: &str) -> WebResult<Option<LoginTxn>> {
        Ok(self.m().logins.remove(state_digest))
    }
    fn insert_cli_code(&self, code_digest: &str, code: &CliCode) -> WebResult<()> {
        // A primary key, as on the real stores.
        match self.m().codes.entry(code_digest.into()) {
            std::collections::hash_map::Entry::Occupied(_) => Err(WebStoreError("duplicate key".into())),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(code.clone());
                Ok(())
            }
        }
    }
    fn issue_cli_code(&self, code_digest: &str, code: &CliCode, ttl: i64) -> WebResult<CliIssued> {
        let mut m = self.m();
        if let Some(gate) = m.cli_gate.take() {
            gate.hold();
        }
        let held = m.codes.get(code_digest).map(|o| (o.user_id, o.created_at));
        let first = *m
            .cli_logins
            .entry(code_digest.into())
            .or_insert(held.map_or(code.created_at, |(_, at)| at.min(code.created_at)));
        if code.created_at - first > ttl {
            return Ok(CliIssued::Expired);
        }
        if held.is_some_and(|(user_id, _)| user_id != code.user_id) {
            return Ok(CliIssued::Theirs);
        }
        m.codes.insert(code_digest.into(), code.clone());
        Ok(CliIssued::Issued)
    }
    fn restore_cli_code(&self, code_digest: &str, code: &CliCode) -> WebResult<()> {
        let mut m = self.m();
        if m.codes.get(code_digest).is_none_or(|newer| newer.user_id != code.user_id) {
            m.codes.insert(code_digest.into(), code.clone());
        }
        Ok(())
    }
    fn end_cli_code(&self, code_digest: &str, user_id: i64) -> WebResult<()> {
        let mut m = self.m();
        if m.codes.get(code_digest).is_some_and(|held| held.user_id != user_id) {
            m.codes.remove(code_digest);
        }
        Ok(())
    }
    fn insert_key_display(&self, token_digest: &str, display: &KeyDisplay) -> WebResult<()> {
        self.m()
            .displays
            .insert(token_digest.into(), display.clone());
        Ok(())
    }
    fn take_key_display(&self, token_digest: &str) -> WebResult<Option<KeyDisplay>> {
        Ok(self.m().displays.remove(token_digest))
    }
    fn take_cli_code(&self, code_digest: &str) -> WebResult<Option<CliCode>> {
        let (code, gate) = {
            let mut m = self.m();
            let code = m.codes.remove(code_digest);
            let gate = if code.is_some() { m.cli_gate.take() } else { None };
            (code, gate)
        };
        if let Some(gate) = gate {
            gate.hold();
        }
        Ok(code)
    }
    fn purge_expired(&self, now: i64, l: &Limits) -> WebResult<()> {
        let mut m = self.m();
        m.sessions.retain(|_, s| {
            now - s.last_seen <= l.session_idle && now - s.created_at <= l.session_max
        });
        m.logins.retain(|_, t| now - t.created_at <= l.login_ttl);
        m.codes.retain(|_, c| now - c.created_at <= l.cli_code_ttl);
        m.cli_logins.retain(|_, at| now - *at <= 86_400);
        m.displays
            .retain(|_, d| now - d.created_at <= l.key_display_ttl);
        Ok(())
    }
    fn host_keys(&self, user_id: i64) -> WebResult<Vec<HostKey>> {
        let m = self.m();
        let mut keys: Vec<HostKey> = m
            .creds
            .iter()
            .filter(|c| c.user_id == user_id)
            .map(|c| HostKey {
                key_id: c.digest[..12].into(),
                host_id: c.host.clone(),
                label: Some(c.label.clone()),
                created_at: Some(c.created_at.to_string()),
                last_seen: None,
                revoked_at: c.revoked_at.map(|t| t.to_string()),
            })
            .collect();
        keys.reverse();
        Ok(keys)
    }
    fn host_owner(&self, host_id: &str) -> WebResult<Option<i64>> {
        Ok(self.m().hosts.get(host_id).copied())
    }
    fn mint_host_key(
        &self,
        user_id: i64,
        host_id: &str,
        label: &str,
        token_digest: &str,
        now: i64,
    ) -> WebResult<Minted> {
        if self.user_disabled(user_id)? {
            return Ok(Minted::Disabled);
        }
        let mut m = self.m();
        let new_host = !m.hosts.contains_key(host_id);
        if *m.hosts.entry(host_id.into()).or_insert(user_id) != user_id {
            return Ok(Minted::HostTaken);
        }
        let mut replaced = 0;
        for c in m
            .creds
            .iter_mut()
            .filter(|c| c.host == host_id && c.revoked_at.is_none())
        {
            c.revoked_at = Some(now);
            replaced += 1;
        }
        m.creds.push(Cred {
            digest: token_digest.into(),
            user_id,
            host: host_id.into(),
            label: label.into(),
            created_at: now,
            revoked_at: None,
        });
        Ok(Minted::Stored { replaced, new_host })
    }
    fn revoke_host(&self, user_id: i64, host_id: &str, now: i64) -> WebResult<u64> {
        let mut m = self.m();
        if m.hosts.get(host_id) != Some(&user_id) {
            return Ok(0);
        }
        let mut n = 0;
        for c in m
            .creds
            .iter_mut()
            .filter(|c| c.host == host_id && c.revoked_at.is_none())
        {
            c.revoked_at = Some(now);
            n += 1;
        }
        Ok(n)
    }
    fn ensure_user(&self, name: &str) -> WebResult<WebUser> {
        if let Some(u) = self.m().users.iter().find(|u| u.name == name) {
            return Ok(u.web());
        }
        let id = self.add_plain_user(name);
        Ok(self.m().users.iter().find(|u| u.id == id).unwrap().web())
    }
    fn bind_identity(&self, name: &str, identity: &Identity) -> WebResult<BindUser> {
        let mut m = self.m();
        match m.users.iter_mut().find(|u| u.name == name) {
            Some(u) if u.identities.iter().any(|(i, _)| *i == identity.issuer) => Ok(BindUser::NameBound),
            Some(u) => {
                u.identities.push((identity.issuer.clone(), identity.subject.clone()));
                Ok(BindUser::Bound(u.web()))
            }
            None => {
                drop(m);
                match self.create_user(identity, name)? {
                    CreateUser::Created(u) => Ok(BindUser::Created(u)),
                    CreateUser::NameTaken { .. } => Ok(BindUser::NameBound),
                }
            }
        }
    }
    fn invite_user(&self, name: &str, email: &str) -> WebResult<Invited> {
        let mut m = self.m();
        let same = |e: &Option<String>| e.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(email));
        if let Some(other) = m.users.iter().find(|u| u.name != name && same(&u.email)) {
            return Ok(Invited::EmailTaken(other.name.clone()));
        }
        if let Some(u) = m.users.iter_mut().find(|u| u.name == name) {
            return Ok(match u.email.clone() {
                Some(_) if same(&u.email) => Invited::Exists(u.web()),
                Some(have) => Invited::OtherEmail(have),
                None => {
                    u.email = Some(email.into());
                    Invited::EmailSet(u.web())
                }
            });
        }
        drop(m);
        let id = self.add_plain_user(name);
        let mut m = self.m();
        let u = m.users.iter_mut().find(|u| u.id == id).unwrap();
        u.email = Some(email.into());
        Ok(Invited::Created(u.web()))
    }
    fn list_users(&self) -> WebResult<Vec<AdminUser>> {
        let mut users: Vec<AdminUser> = self
            .m()
            .users
            .iter()
            .map(|u| AdminUser {
                name: u.name.clone(),
                email: u.email.clone(),
                issuers: u.identities.iter().map(|(i, _)| i.clone()).collect(),
                disabled: u.disabled,
            })
            .collect();
        users.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(users)
    }
    fn all_keys(&self) -> WebResult<Vec<AdminKey>> {
        let ids: Vec<(i64, String)> = self
            .m()
            .users
            .iter()
            .map(|u| (u.id, u.name.clone()))
            .collect();
        let mut out = Vec::new();
        for (id, name) in ids {
            for key in self.host_keys(id)? {
                out.push(AdminKey {
                    user: name.clone(),
                    key,
                });
            }
        }
        Ok(out)
    }
    fn revoke_key(&self, key_id: &str, now: i64) -> WebResult<Option<String>> {
        let mut m = self.m();
        let live = m
            .creds
            .iter_mut()
            .find(|c| c.digest.starts_with(key_id) && c.revoked_at.is_none());
        Ok(live.map(|c| {
            c.revoked_at = Some(now);
            c.host.clone()
        }))
    }
    fn set_disabled(&self, name: &str, disabled: bool) -> WebResult<Option<Vec<String>>> {
        let mut m = self.m();
        let Some(u) = m.users.iter_mut().find(|u| u.name == name) else {
            return Ok(None);
        };
        u.disabled = disabled;
        let id = u.id;
        if disabled {
            m.sessions.retain(|_, s| s.user_id != id);
        }
        let mut hosts: Vec<String> = m
            .hosts
            .iter()
            .filter(|(_, o)| **o == id)
            .map(|(h, _)| h.clone())
            .collect();
        hosts.sort();
        Ok(Some(hosts))
    }
}

/// Bridge authentication over the memory store, as the board's: a live key of an enabled user.
struct MemAuth(Arc<MemStore>);

impl conde_remote::Authenticator for MemAuth {
    fn authenticate(&self, token: &str) -> Option<conde_remote::Principal> {
        let d = token_digest(token);
        let m = self.0.m();
        let c = m
            .creds
            .iter()
            .find(|c| c.digest == d && c.revoked_at.is_none())?;
        let user = m.users.iter().find(|u| u.id == c.user_id)?;
        (!user.disabled).then(|| conde_remote::Principal {
            user_id: user.id.to_string(),
            host_id: c.host.clone(),
        })
    }
}

// --- sign-in ----------------------------------------------------------------------------------

#[tokio::test]
async fn an_invited_user_is_bound_on_first_sign_in_and_reused() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().1, None, "invited, not bound");
    let page = Browser::new().sign_in(&env).await;
    assert!(
        page.contains("Host keys") && page.contains("Krisztián Dobmayer"),
        "{page}"
    );
    assert!(page.contains("No host keys yet"));
    let (id, identity, display) = env.store.user("krisztian-dobmayer").expect("the invited user");
    // Entra: the issuer is the configured one, the subject the oid (not the per-app sub).
    assert_eq!(
        identity,
        Some((env.idp.issuer.clone(), "oid-krisztian".to_string()))
    );
    assert_eq!(display.as_deref(), Some("Krisztián Dobmayer"));

    // Same (issuer, subject), new display name and upn: same user, profile updated, no new user.
    {
        let mut p = env.idp.person.lock().unwrap();
        p.name = "K. Dobmayer".into();
        p.upn = "renamed@dakai.example".into();
    }
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("K. Dobmayer"));
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().0, id);
    assert_eq!(env.store.m().users.len(), 1);
    assert_eq!(
        env.idp.discoveries.load(Ordering::SeqCst),
        1,
        "metadata is cached"
    );
}

/// Signs in through the fake provider and expects the invite gate's refusal.
async fn refused_as_uninvited<S>(env: &Env<S>) {
    let b = Browser::new();
    let (status, _, body) = b.get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("This account is not invited to this board. Ask the board admin."), "{body}");
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "no session");
    assert!(loc.starts_with("/auth/login"), "{loc}");
}

#[tokio::test]
async fn an_entra_token_of_another_tenant_is_refused_and_nothing_is_bound() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    env.idp.person.lock().unwrap().tid = "00000000-0000-0000-0000-000000000000".into();
    let b = Browser::new();
    let (status, _, body) = b.get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("could not be verified"), "{body}");
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().1, None);
    assert!(env.store.m().sessions.is_empty());
}

#[tokio::test]
async fn an_identity_no_user_has_the_email_of_is_refused_and_nothing_is_created() {
    let env = Env::uninvited(Arc::new(MemStore::default()), |_| {}).await;
    refused_as_uninvited(&env).await;
    assert!(env.store.m().users.is_empty());
    assert!(env.store.m().sessions.is_empty());
    // another user's invite admits nobody else
    env.invite("bob", "bob@dakai.example").await;
    refused_as_uninvited(&env).await;
    assert_eq!(env.store.user("bob").unwrap().1, None);
}

#[tokio::test]
async fn an_email_two_users_share_admits_neither() {
    let env = Env::uninvited(Arc::new(MemStore::default()), |_| {}).await;
    env.invite("krisztian-dobmayer", "krisztian.dobmayer@dakai.example").await;
    env.store.add_plain_user("kd");
    env.store.set_email("kd", "KRISZTIAN.DOBMAYER@dakai.example");
    refused_as_uninvited(&env).await;
    assert!(env.store.m().users.iter().all(|u| u.identities.is_empty()));
    assert!(env.store.m().sessions.is_empty());
}

#[tokio::test]
async fn the_invite_matches_the_email_whatever_its_case() {
    let env = Env::uninvited(Arc::new(MemStore::default()), |_| {}).await;
    env.invite("kd", "KRISZTIAN.DOBMAYER@DAKAI.EXAMPLE").await;
    Browser::new().sign_in(&env).await;
    assert_eq!(env.store.user("kd").unwrap().1, Some((env.idp.issuer.clone(), "oid-krisztian".into())));
}

#[tokio::test]
async fn a_user_bound_to_this_issuer_is_not_claimed_by_another_identity() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    Browser::new().sign_in(&env).await;
    // Another account of the same directory with the same address (a re-created account).
    env.idp.person.lock().unwrap().oid = "oid-impostor".into();
    refused_as_uninvited(&env).await;
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().1, Some((env.idp.issuer.clone(), "oid-krisztian".into())));
    assert_eq!(env.store.m().users[0].identities.len(), 1);
}

/// A Keycloak realm: no Entra claims, the subject is `sub`, and only an `email` the provider marks
/// verified is matched (never preferred_username).
#[tokio::test]
async fn a_generic_issuer_matches_only_a_verified_email_and_keys_on_sub() {
    let env = Env::with_config(Arc::new(MemStore::default()), |c| c.entra_tenant = None).await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.entra = false;
        p.upn = "krisztian.dobmayer@dakai.example".into();
        p.email = Some("krisztian.dobmayer@dakai.example".into());
    }
    refused_as_uninvited(&env).await;
    env.idp.person.lock().unwrap().email_verified = Some(false);
    refused_as_uninvited(&env).await;
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().1, None);
    env.idp.person.lock().unwrap().email_verified = Some(true);
    Browser::new().sign_in(&env).await;
    assert_eq!(
        env.store.user("krisztian-dobmayer").unwrap().1,
        Some((env.idp.issuer.clone(), "pairwise-oid-krisztian".into()))
    );
    // Bound: the next sign-in is by (issuer, sub), whatever the email says now.
    env.idp.person.lock().unwrap().email_verified = None;
    Browser::new().sign_in(&env).await;
}

#[tokio::test]
async fn spoiled_id_tokens_are_refused() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    for spoil in [
        Spoil::Audience,
        Spoil::Nonce,
        Spoil::Expired,
        Spoil::Signature,
        Spoil::Issuer,
    ] {
        *env.idp.spoil.lock().unwrap() = spoil;
        let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{spoil:?}: {body}");
        assert!(body.contains("could not be verified"), "{spoil:?}: {body}");
    }
    assert_eq!(env.store.m().users.len(), 1, "the invited user only");
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().1, None, "nothing bound");
    assert!(env.store.m().sessions.is_empty());
    *env.idp.spoil.lock().unwrap() = Spoil::None;
    Browser::new().sign_in(&env).await;
}

#[tokio::test]
async fn rotated_signing_keys_are_fetched_again() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    Browser::new().sign_in(&env).await;
    env.idp.rotate();
    Browser::new().sign_in(&env).await;
    assert_eq!(env.idp.jwks_reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_callback_from_another_browser_is_refused() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let victim = Browser::new();
    let attacker = Browser::new();
    // The attacker starts a sign-in and gets the provider's redirect back to the board.
    let (_, authorize) = attacker.peek(&env.url("/auth/login")).await;
    let (status, callback) = attacker.peek(&authorize).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(callback.contains("/auth/callback?code="), "{callback}");
    // Login CSRF: the victim's browser lands on the attacker's callback URL and is sent to its
    // own sign-in, never signed in with the attacker's code.
    let (status, loc) = victim.peek(&callback).await;
    assert_eq!(
        (status, loc.as_str()),
        (StatusCode::SEE_OTHER, SIGN_IN_AGAIN)
    );
    assert!(env.store.m().sessions.is_empty());
    // The attacker's own browser may still finish it once; a replay does not sign in again.
    let (status, _, _) = attacker.get(&callback).await;
    assert_eq!(status, StatusCode::OK);
    let (status, loc) = attacker.peek(&callback).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/"));
    assert_eq!(env.store.m().sessions.len(), 1);
}

/// Starts a sign-in in `b` (returning to /keys) and returns the provider's redirect back to the
/// board: the callback URL with its code and state.
async fn started_callback<S>(env: &Env<S>, b: &Browser) -> String {
    let (status, authorize) = b.peek(&env.url("/auth/login?next=%2Fkeys")).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{authorize}");
    let (status, callback) = b.peek(&authorize).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{callback}");
    assert!(callback.contains("/auth/callback?code="), "{callback}");
    callback
}

/// Whether `b` holds the cookie that marks one automatic sign-in retry after a stale callback.
fn retry_marked<S>(env: &Env<S>, b: &Browser) -> bool {
    use reqwest::cookie::CookieStore;
    b.jar
        .cookies(&env.url("/").parse().unwrap())
        .is_some_and(|v| v.to_str().unwrap().contains("conde_login_retry="))
}

const SIGN_IN_AGAIN: &str = "/auth/login?next=%2F&retry=1";

/// The callback URL's `state`.
fn state_of(callback: &str) -> String {
    let url: reqwest::Url = callback.parse().unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1;
    state.into_owned()
}

/// The callback URL with its `state` replaced.
fn with_state(callback: &str, state: &str) -> String {
    let mut url: reqwest::Url = callback.parse().unwrap();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if k == "state" {
                state.to_string()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
    url.to_string()
}

/// The Set-Cookie headers of a response.
fn set_cookies(r: &reqwest::Response) -> Vec<String> {
    r.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

/// Two sign-ins started in one browser (two tabs, or one page's concurrent 401s) both finish:
/// the second does not void the first one's state.
#[tokio::test]
async fn two_sign_ins_started_in_one_browser_both_finish() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let first = started_callback(&env, &b).await;
    let second = started_callback(&env, &b).await;
    assert_eq!(
        b.peek(&first).await,
        (StatusCode::SEE_OTHER, "/keys".to_string()),
        "first"
    );
    assert_eq!(
        b.peek(&second).await,
        (StatusCode::SEE_OTHER, "/keys".to_string()),
        "second"
    );
    assert_eq!(env.store.m().sessions.len(), 2);
    assert!(env.store.m().logins.is_empty());
}

/// Login CSRF stays closed: a callback whose state this browser did not start is never finished
/// here, even beside its own pending sign-in; it starts the browser's own sign-in instead.
#[tokio::test]
async fn a_state_this_browser_did_not_start_is_never_accepted() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let (victim, attacker) = (Browser::new(), Browser::new());
    let own = started_callback(&env, &victim).await;
    let forged = started_callback(&env, &attacker).await;
    assert_eq!(
        victim.peek(&forged).await,
        (StatusCode::SEE_OTHER, SIGN_IN_AGAIN.to_string())
    );
    assert!(
        env.store.m().sessions.is_empty(),
        "the attacker's code was not redeemed"
    );
    assert_eq!(env.store.m().logins.len(), 2, "nor its sign-in consumed");
    assert_eq!(
        victim.peek(&own).await,
        (StatusCode::SEE_OTHER, "/keys".to_string())
    );
    assert_eq!(
        attacker.peek(&forged).await,
        (StatusCode::SEE_OTHER, "/keys".to_string())
    );
}

/// A finished callback opened again (tab restore, back button) while signed in goes to the board.
#[tokio::test]
async fn a_replayed_callback_with_a_live_session_goes_to_the_board() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let callback = started_callback(&env, &b).await;
    assert_eq!(
        b.peek(&callback).await,
        (StatusCode::SEE_OTHER, "/keys".to_string())
    );
    assert_eq!(
        b.peek(&callback).await,
        (StatusCode::SEE_OTHER, "/".to_string())
    );
    assert_eq!(env.store.m().sessions.len(), 1, "no second session");
    assert!(!retry_marked(&env, &b));
}

/// A stale callback without a session (the session ended, then the tab came back on the old
/// callback URL; or the sign-in expired) starts a fresh sign-in once. A second stale callback in
/// a row shows the error page, which links home, and drops the marker.
#[tokio::test]
async fn a_stale_callback_without_a_session_signs_in_again_once() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let callback = started_callback(&env, &b).await;
    assert_eq!(
        b.peek(&callback).await,
        (StatusCode::SEE_OTHER, "/keys".to_string())
    );
    env.advance(8 * 3600 + 1);
    let r = b.step.get(&callback).send().await.unwrap();
    assert_eq!(
        (r.status(), &r.headers()[header::LOCATION]),
        (StatusCode::SEE_OTHER, &SIGN_IN_AGAIN.parse().unwrap())
    );
    let set: Vec<&str> = r
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert!(
        set.iter().any(|c| c.starts_with("conde_login_retry=1;")
            && ["Max-Age=300", "HttpOnly", "SameSite=Lax", "Path=/"]
                .iter()
                .all(|a| c.contains(a))),
        "{set:?}"
    );
    assert!(retry_marked(&env, &b));
    let (_, authorize) = b.peek(&env.url(SIGN_IN_AGAIN)).await;
    let (_, fresh) = b.peek(&authorize).await;
    assert_eq!(
        b.peek(&fresh).await,
        (StatusCode::SEE_OTHER, "/".to_string())
    );
    assert!(
        !retry_marked(&env, &b),
        "a finished sign-in drops the marker"
    );

    let b = Browser::new();
    let late = started_callback(&env, &b).await;
    env.advance(601);
    assert_eq!(
        b.peek(&late).await,
        (
            StatusCode::SEE_OTHER,
            "/auth/login?next=%2Fkeys&retry=1".to_string()
        )
    );
    assert!(retry_marked(&env, &b));
    let late = started_callback(&env, &b).await;
    env.advance(601);
    let (status, _, page) = b.get(&late).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{page}");
    assert!(page.contains("expired"), "{page}");
    assert!(page.contains(r#"<a class="brand" href="/">"#), "{page}");
    assert!(
        page.contains(r#"<a href="/">Go to the board</a>"#),
        "{page}"
    );
    assert!(!retry_marked(&env, &b), "no loop: the marker is dropped");
    assert!(env.store.m().sessions.len() == 1);
}

/// The login cookie keeps the four newest pending sign-ins: a fifth drops the oldest.
#[tokio::test]
async fn a_fifth_pending_sign_in_drops_the_oldest() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let mut callbacks = Vec::new();
    for _ in 0..5 {
        callbacks.push(started_callback(&env, &b).await);
    }
    assert_eq!(
        b.peek(&callbacks[0]).await,
        (StatusCode::SEE_OTHER, SIGN_IN_AGAIN.to_string())
    );
    assert!(
        env.store.m().sessions.is_empty(),
        "the oldest state was dropped"
    );
    for (i, callback) in callbacks[1..].iter().enumerate() {
        assert_eq!(
            b.peek(callback).await,
            (StatusCode::SEE_OTHER, "/keys".to_string()),
            "{}",
            i + 2
        );
    }
}

/// A return path of 2048 characters survives the sign-in; a longer one is refused with a reason,
/// never dropped for /keys.
#[tokio::test]
async fn a_return_path_up_to_2048_characters_is_carried_and_a_longer_one_refused() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let next = |len: usize| format!("/keys?pad={}", "a".repeat(len - 10));
    let login = |path: &str| {
        let next: String = openidconnect::url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
        env.url(&format!("/auth/login?next={next}"))
    };
    let b = Browser::new();
    let longest = next(2048);
    let (status, authorize) = b.peek(&login(&longest)).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{authorize}");
    let (_, callback) = b.peek(&authorize).await;
    let (status, back) = b.peek(&callback).await;
    assert_eq!((status, back.as_str()), (StatusCode::SEE_OTHER, longest.as_str()), "carried back whole");

    let (status, _, body) = Browser::new().get(&login(&next(2049))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("longer than 2048 characters"), "{body}");
}

#[tokio::test]
async fn provider_errors_are_shown() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let (_, login) = b.peek(&env.url("/auth/login")).await;
    assert!(login.contains("/oauth2/v2.0/authorize?"));
    let (status, _, body) = b
        .get(&env.url(
            "/auth/callback?error=access_denied&error_description=AADSTS50105%3A+not+assigned",
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("AADSTS50105: not assigned"), "{body}");
}

#[tokio::test]
async fn an_admin_user_without_the_email_is_not_adopted_by_its_name() {
    let env = Env::uninvited(Arc::new(MemStore::default()), |_| {}).await;
    env.store.add_plain_user("krisztian-dobmayer");
    refused_as_uninvited(&env).await;
    assert_eq!(env.store.m().users.len(), 1);
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().1, None);
}

#[tokio::test]
async fn sessions_expire_when_idle_and_at_the_absolute_limit() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    b.sign_in(&env).await;
    env.advance(8 * 3600 - 10);
    assert_eq!(b.peek(&env.url("/keys")).await.0, StatusCode::OK);
    env.advance(8 * 3600 + 1);
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert_eq!(
        (status, loc.as_str()),
        (StatusCode::SEE_OTHER, "/auth/login?next=%2Fkeys")
    );
    assert!(
        env.store.m().sessions.is_empty(),
        "the expired session is deleted"
    );

    // Active every 4 hours, still cut at 24 hours.
    b.sign_in(&env).await;
    for _ in 0..6 {
        env.advance(4 * 3600);
        assert_eq!(b.peek(&env.url("/keys")).await.0, StatusCode::OK);
    }
    env.advance(60);
    assert_eq!(b.peek(&env.url("/keys")).await.0, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn logout_ends_the_session_and_signs_out_of_the_provider() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let page = b.sign_in(&env).await;
    let csrf = csrf_of(&page);
    let r = b
        .post_form(
            &env.url("/auth/logout"),
            &[("csrf", "0".repeat(64).as_str())],
        )
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(env.store.m().sessions.len(), 1);
    let r = b
        .post_form(&env.url("/auth/logout"), &[("csrf", &csrf)])
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let loc = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    let end = format!(
        "{}/oauth2/v2.0/logout?",
        env.idp.issuer.trim_end_matches("/v2.0")
    );
    assert!(loc.starts_with(&end), "{loc}");
    assert!(loc.contains(&format!("client_id={CLIENT_ID}")), "{loc}");
    let back: String =
        openidconnect::url::form_urlencoded::byte_serialize(env.url("/").as_bytes()).collect();
    assert!(
        loc.contains(&format!("post_logout_redirect_uri={back}")),
        "{loc}"
    );
    assert!(env.store.m().sessions.is_empty());
    assert_eq!(b.peek(&env.url("/keys")).await.0, StatusCode::SEE_OTHER);
}

// --- keys page --------------------------------------------------------------------------------

#[tokio::test]
async fn keys_are_created_once_listed_and_revoked_with_csrf() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let page = b.sign_in(&env).await;
    let csrf = csrf_of(&page);

    for bad in ["", "f00", &"0".repeat(64)] {
        let r = b
            .post_form(
                &env.url("/keys"),
                &[("csrf", bad), ("label", "Work Laptop")],
            )
            .await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN, "csrf {bad:?}");
    }
    assert!(env.store.m().creds.is_empty());
    let r = b
        .post_form(&env.url("/keys"), &[("csrf", &csrf), ("label", "  ")])
        .await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);

    let r = b
        .post_form(
            &env.url("/keys"),
            &[("csrf", &csrf), ("label", "Work Laptop")],
        )
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert!(r.headers()[header::LOCATION].to_str().unwrap() == "/keys/created");
    let r = b.follow.get(env.url("/keys/created")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    let csp = r.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    let created = r.text().await.unwrap();
    let key = key_of(&created);
    assert!(key.starts_with("conde_") && key.len() == 70, "{key}");
    assert_eq!(env.store.live_host_of(&key).as_deref(), Some("work-laptop"));
    assert!(
        created.contains("conde enroll --server http://127.0.0.1"),
        "{created}"
    );
    let nonce = csp
        .split("'nonce-")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert!(
        created.contains(&format!("<script nonce=\"{nonce}\">")),
        "{csp}"
    );

    let (_, _, list) = b.get(&env.url("/keys")).await;
    assert!(list.contains("<code>work-laptop</code>") && list.contains("Work Laptop"));
    assert!(!list.contains(&key), "the key is shown once");
    assert!(list.contains("Revoke"));

    // Revocation needs the token, then revokes and closes the live bridge connection.
    env.registry.connect("work-laptop");
    let r = b
        .post_form(&env.url("/keys/work-laptop/revoke"), &[("csrf", "nope")])
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert!(env.store.live_host_of(&key).is_some());
    assert!(env.registry.is_open("work-laptop"));
    let r = b
        .post_form(&env.url("/keys/work-laptop/revoke"), &[("csrf", &csrf)])
        .await;
    assert_eq!(
        (r.status(), r.headers()[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/keys")
    );
    assert!(env.store.live_host_of(&key).is_none());
    assert!(!env.registry.is_open("work-laptop"));
    assert_eq!(env.registry.closed(), vec!["work-laptop".to_string()]);
    let (_, _, list) = b.get(&env.url("/keys")).await;
    assert!(list.contains("revoked "), "{list}");
}

#[tokio::test]
async fn a_new_key_for_a_host_replaces_its_live_key() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let csrf = csrf_of(&b.sign_in(&env).await);
    let first = key_of(&b.create_key(&env, &csrf, "box").await);
    env.registry.connect("box");
    let page = b.create_key(&env, &csrf, "box").await;
    assert!(page.contains("previous key of this host was revoked"));
    let second = key_of(&page);
    assert!(env.store.live_host_of(&first).is_none());
    assert_eq!(env.store.live_host_of(&second).as_deref(), Some("box"));
    assert!(!env.registry.is_open("box"));
}

#[tokio::test]
async fn hosts_of_other_users_are_neither_reused_nor_revocable() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let other = env.store.add_plain_user("bob");
    let foreign = token_digest("bobs-key");
    env.store
        .mint_host_key(other, "box", "box", &foreign, START)
        .unwrap();
    let b = Browser::new();
    let csrf = csrf_of(&b.sign_in(&env).await);
    let key = key_of(&b.create_key(&env, &csrf, "Box").await);
    assert_eq!(env.store.live_host_of(&key).as_deref(), Some("box-2"));
    let r = b
        .post_form(&env.url("/keys/box/revoke"), &[("csrf", &csrf)])
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(env.store.creds_of("box"), vec![("box".to_string(), None)]);
    assert!(env.registry.closed().is_empty());
}

#[tokio::test]
async fn signed_out_posts_are_refused() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let r = b
        .post_form(&env.url("/keys"), &[("csrf", "x"), ("label", "box")])
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = b
        .post_form(&env.url("/keys/box/revoke"), &[("csrf", "x")])
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

// --- conde login, browser leg -----------------------------------------------------------------

#[tokio::test]
async fn conde_login_signs_in_mints_on_redeem_and_codes_are_single_use() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    assert!(
        env.store.m().creds.is_empty(),
        "nothing is minted before the redeem"
    );

    let body = json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "Test-Box.local"});
    let (status, got) = redeem(&env, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["host_id"], "test-box.local");
    assert_eq!(got["user"], "krisztian-dobmayer");
    assert_eq!(got["reenrolled"], false, "a new host");
    let key = got["key"].as_str().unwrap();
    assert_eq!(
        env.store.live_host_of(key).as_deref(),
        Some("test-box.local")
    );
    assert_eq!(env.store.creds_of("test-box.local")[0].0, "Test-Box.local");

    let (status, again) = redeem(&env, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(again["error"], "invalid_grant");

    // A second run in the same browser reuses the session (no new sign-in).
    assert_eq!(env.idp.authorizations.load(Ordering::SeqCst), 1);
    browser_leg(&env, &b, &cli_run()).await;
    assert_eq!(env.idp.authorizations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn conde_login_codes_expire_and_are_bound_to_state_and_verifier() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();

    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    env.advance(301);
    let (status, got) = redeem(
        &env,
        json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "h"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        got["message"].as_str().unwrap().contains("expired"),
        "{got}"
    );

    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let (status, _) = redeem(
        &env,
        json!({"code": code, "state": rand_hex(), "code_verifier": run.verifier, "host": "h"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A failed redeem consumes the code.
    let (status, got) = redeem(
        &env,
        json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "h"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        got["message"].as_str().unwrap().contains("unknown"),
        "{got}"
    );

    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let other = cli_run();
    let (status, got) = redeem(
        &env,
        json!({"code": code, "state": run.state, "code_verifier": other.verifier, "host": "h"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        got["message"].as_str().unwrap().contains("challenge"),
        "{got}"
    );
    assert!(env.store.m().creds.is_empty());
}

#[tokio::test]
async fn conde_login_links_are_validated() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    for query in [
        format!("port=80&state={}&challenge={}", run.state, run.challenge),
        format!("port=5000&state=short&challenge={}", run.challenge),
        format!("port=5000&state={}&challenge=abc", run.state),
        format!("port=5000&state={}", run.state),
    ] {
        let (status, _) = b.peek(&env.url(&format!("/auth/cli?{query}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
}

// --- conde login, pasted code ----------------------------------------------------------------

#[tokio::test]
async fn a_pasted_code_redeems_once_with_the_runs_verifier() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    assert!(env.store.m().creds.is_empty(), "nothing is minted before the redeem");
    let stored = format!("{:?}", env.store.m().codes.values().collect::<Vec<_>>());
    assert!(!stored.contains(&shown.replace('-', "")), "the code is stored only as a digest: {stored}");

    // typed as read aloud: lower case, a space instead of the dash
    let typed = shown.to_lowercase().replace('-', " ");
    let (status, got) = paste_redeem(&env, &run, &typed).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!((got["host_id"].as_str(), got["user"].as_str()), (Some("head-less"), Some("krisztian-dobmayer")));
    assert_eq!(env.store.live_host_of(got["key"].as_str().unwrap()).as_deref(), Some("head-less"));

    // used: nothing waits under this run any more (the CLI has dropped it on success)
    let (status, again) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, again["error"].as_str()), (StatusCode::BAD_REQUEST, Some("authorization_pending")));
    assert_eq!(env.store.m().creds.len(), 1);
}

#[tokio::test]
async fn wrong_pasted_codes_use_up_the_tries_then_the_login_dies() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };

    // the right code from another run (the state from the URL, another verifier) is refused and
    // spends no try: only the run that holds the verifier can use the tries up
    let thief = CliRun { state: run.state.clone(), ..cli_run() };
    let (status, got) = paste_redeem(&env, &thief, &shown).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{got}");
    assert!(got["message"].as_str().unwrap().contains("not from the conde login run"), "{got}");
    for left in [4, 3, 2, 1] {
        let (status, got) = paste_redeem(&env, &run, wrong).await;
        assert_eq!((status, got["tries_left"].as_u64()), (StatusCode::BAD_REQUEST, Some(left)), "{got}");
    }
    let (status, got) = paste_redeem(&env, &run, wrong).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{got}");
    assert!(got["message"].as_str().unwrap().contains("last try"), "{got}");
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("authorization_pending")), "{got}");
    assert!(env.store.m().creds.is_empty() && env.store.m().codes.is_empty(), "the login is gone");

    // a malformed code is refused before any lookup and costs no try
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let (status, got) = paste_redeem(&env, &run, "O0O0-I1I1").await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_request")), "{got}");
    // expiry
    env.advance(301);
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(got["message"].as_str().unwrap().contains("expired"), "{got}");
}

/// A redeem from `ip` (the ingress's X-Forwarded-For entry): (status, Retry-After, body).
async fn redeem_from<S>(env: &Env<S>, ip: &str, body: Value) -> (StatusCode, Option<String>, Value) {
    let r = reqwest::Client::new()
        .post(env.url("/auth/cli/redeem"))
        .header("x-forwarded-for", ip)
        .json(&body)
        .send()
        .await
        .unwrap();
    let after = r.headers().get(header::RETRY_AFTER).map(|v| v.to_str().unwrap().to_string());
    (r.status(), after, r.json().await.unwrap())
}

#[tokio::test]
async fn the_redeem_is_rate_limited_per_address() {
    let env = Env::with_config(Arc::new(MemStore::default()), |c| c.limits.cli_redeem_per_min = 3).await;
    let run = cli_run();
    let body = json!({"code": "c".repeat(64), "state": run.state, "code_verifier": run.verifier, "host": "box"});
    for _ in 0..3 {
        let (status, _, got) = redeem_from(&env, "203.0.113.1", body.clone()).await;
        assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{got}");
    }
    let (status, after, got) = redeem_from(&env, "203.0.113.1", body.clone()).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")), "{got}");
    let wait = got["retry_after"].as_u64().unwrap();
    assert!(wait >= 1 && after == Some(wait.to_string()), "{got} {after:?}");
    assert!(got["message"].as_str().unwrap().contains("from this address"), "{got}");
    assert!(got.get("login_expires_first").is_none(), "a wait to retry: {got}");
    // Another address has its own bucket.
    let (status, _, _) = redeem_from(&env, "203.0.113.2", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The per-code budget (5 tries) restarts with every code the account confirms again; the
/// per-account budget does not: once spent, even the right code waits, and the waiting login
/// keeps its record (the refused attempt spends no try).
#[tokio::test]
async fn pasted_code_attempts_are_limited_per_account_across_reissued_codes() {
    let env = Env::with_config(Arc::new(MemStore::default()), |c| c.limits.cli_guesses_per_hour = 7).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = |shown: &str| if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    for left in [4, 3, 2, 1] {
        let (status, got) = paste_redeem(&env, &run, wrong(&shown)).await;
        assert_eq!((status, got["tries_left"].as_u64()), (StatusCode::BAD_REQUEST, Some(left)), "{got}");
    }
    // Confirming again restarts the per-code tries ...
    let shown = paste_leg(&env, &b, &run).await;
    for left in [4, 3, 2] {
        let (status, got) = paste_redeem(&env, &run, wrong(&shown)).await;
        assert_eq!((status, got["tries_left"].as_u64()), (StatusCode::BAD_REQUEST, Some(left)), "{got}");
    }
    // ... but not the account's: the eighth attempt waits, even with the right code.
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")), "{got}");
    assert!(got["message"].as_str().unwrap().contains("for this account"), "{got}");
    assert!(env.store.m().creds.is_empty(), "nothing minted");
    let tries: Vec<u32> = env.store.m().codes.values().map(|c| c.tries).collect();
    assert_eq!(tries, [3], "the login waits with its tries unspent");
}

/// The reviewer's probe: a stranger holding only the login URL (its state) sends many redeems
/// with a made-up verifier, across several confirmed runs. None of them spends the account's
/// budget or a per-code try, so the account's next real run still signs in with its code.
#[tokio::test]
async fn holding_only_the_login_url_spends_no_attempt_budget() {
    let env = Env::with_config(Arc::new(MemStore::default()), |c| c.limits.cli_guesses_per_hour = 3).await;
    let b = Browser::new();
    for round in 0..3 {
        let run = cli_run();
        let shown = paste_leg(&env, &b, &run).await;
        let stranger = CliRun { state: run.state.clone(), ..cli_run() };
        for _ in 0..5 {
            let (status, got) = paste_redeem(&env, &stranger, &shown).await;
            assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "round {round}: {got}");
        }
        let tries: Vec<u32> = env.store.m().codes.values().map(|c| c.tries).collect();
        assert_eq!(tries, [0], "round {round}: no try spent");
        env.store.m().codes.clear();
    }
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, got["user"].as_str()), (StatusCode::OK, Some("krisztian-dobmayer")), "{got}");
}

/// The last wrong try ends the login: if another account confirmed this login's URL while that
/// guess held the record, its record does not stay in the slot.
#[tokio::test(flavor = "multi_thread")]
async fn the_last_wrong_try_leaves_no_other_accounts_record() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    for _ in 0..4 {
        assert_eq!(paste_redeem(&env, &run, wrong).await.0, StatusCode::BAD_REQUEST);
    }
    // bob signs in and opens the confirm page of the same login
    env.invite("bob", "bob@dakai.example").await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-bob".into();
        p.upn = "bob@dakai.example".into();
        p.name = "Bob".into();
    }
    let bob = Browser::new();
    let (status, page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let taken = Arc::new(tokio::sync::Notify::new());
    let (release, held) = std::sync::mpsc::channel();
    env.store.m().cli_gate = Some(TakeGate { taken: taken.clone(), release: held });
    let last = paste_redeem(&env, &run, wrong);
    let confirm = async {
        tokio::time::timeout(Duration::from_secs(5), taken.notified()).await.expect("the last guess takes the record");
        let (status, body) = paste_confirm(&env, &bob, &run, &csrf_of(&page)).await;
        release.send(()).unwrap();
        (status, body)
    };
    let ((status, got), (confirmed, _)) = tokio::join!(last, confirm);
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{got}");
    assert!(got["message"].as_str().unwrap().contains("last try"), "{got}");
    assert_eq!(confirmed, StatusCode::OK, "bob's confirm landed in the gap");
    assert!(env.store.m().codes.is_empty(), "the dead login is not left to bob");
}

/// When the account's next attempt is allowed only after the waiting login expires, the 429 says
/// so instead of a retry that cannot succeed.
#[tokio::test]
async fn a_wait_past_the_logins_expiry_says_to_start_again() {
    let env = Env::with_config(Arc::new(MemStore::default()), |c| c.limits.cli_guesses_per_hour = 1).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    let (status, _) = paste_redeem(&env, &run, wrong).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")), "{got}");
    let message = got["message"].as_str().unwrap();
    assert!(message.contains("after this login expires") && message.contains("run conde login --code again"), "{message}");
    assert_eq!(got["login_expires_first"], true, "the CLI ends the run on this answer: {got}");
}

#[tokio::test]
async fn the_pasted_code_is_issued_only_on_the_confirming_post_of_the_account_that_signed_in() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let run = cli_run();
    let link = format!("/auth/cli?mode=code&state={}&challenge={}", run.state, run.challenge);

    // signed out: the sign-in comes first and returns to the same link
    let (status, loc) = Browser::new().peek(&env.url(&link)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(loc.starts_with("/auth/login?next=%2Fauth%2Fcli%3Fmode%3Dcode%26state%3D"), "{loc}");

    // signed in, GET only asks: the account is named, nothing is issued
    let b = Browser::new();
    let (status, page) = paste_confirm_page(&env, &b, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("wants a host key for <strong>krisztian-dobmayer</strong>"), "{page}");
    assert!(page.contains("Continue only if you started <code>conde login --code</code> yourself"), "{page}");
    assert!(!page.contains("<pre id=\"key\">"), "{page}");
    paste_confirm_page(&env, &b, &run).await;
    assert!(env.store.m().codes.is_empty(), "GET has no side effect");
    // the POST needs the session's CSRF token
    let (status, body) = paste_confirm(&env, &b, &run, "forged").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = paste_confirm(&env, &Browser::new(), &run, &csrf_of(&page)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(env.store.m().codes.is_empty());

    let first = paste_leg(&env, &b, &run).await;
    // confirming again (a reload re-posts) issues a new code; the old one is dead
    let second = paste_leg(&env, &b, &run).await;
    assert_ne!(first, second);

    // another account may see the question but cannot take the login over
    env.invite("bob", "bob@dakai.example").await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-bob".into();
        p.upn = "bob@dakai.example".into();
        p.name = "Bob".into();
    }
    let bob = Browser::new();
    let (status, page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let (status, body) = paste_confirm(&env, &bob, &run, &csrf_of(&page)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(!body.contains("<pre id=\"key\">"), "{body}");

    let (status, got) = paste_redeem(&env, &run, &first).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_code")), "{got}");
    let (status, got) = paste_redeem(&env, &run, &second).await;
    assert_eq!((status, got["user"].as_str()), (StatusCode::OK, Some("krisztian-dobmayer")), "{got}");
}

/// Another account confirming a login whose record has expired is told it expired (run conde
/// login again), not that someone else holds it; a live record of another account still is. A
/// fresh sign-in (which purges the expired record) and the first account get the same answer.
#[tokio::test]
async fn another_account_confirming_an_expired_login_is_told_it_expired() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let run = cli_run();
    paste_leg(&env, &Browser::new(), &run).await;
    env.invite("bob", "bob@dakai.example").await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-bob".into();
        p.upn = "bob@dakai.example".into();
        p.name = "Bob".into();
    }
    let bob = Browser::new();
    let (status, page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let (status, body) = paste_confirm(&env, &bob, &run, &csrf_of(&page)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "live: {body}");
    assert!(body.contains("Another account already signed in"), "{body}");
    env.advance(301);
    let (status, page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let (status, body) = paste_confirm(&env, &bob, &run, &csrf_of(&page)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired: {body}");
    assert!(body.contains("This conde login expired. Run conde login again."), "{body}");
    assert!(!body.contains("Another account"), "{body}");
    // a fresh sign-in purges the expired record; the login's age still decides
    let fresh = Browser::new();
    let (status, page) = paste_confirm_page(&env, &fresh, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(env.store.m().codes.is_empty(), "the sign-in purged the expired record");
    let (status, body) = paste_confirm(&env, &fresh, &run, &csrf_of(&page)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired after the purge: {body}");
    assert!(body.contains("This conde login expired. Run conde login again."), "{body}");
    assert!(env.store.m().codes.is_empty(), "nothing issued");
}

/// A pasted code redeemed after it expired ends the login like a last wrong try: a record another
/// account put in while the redeem held the taken one does not stay (reachable for a login whose
/// first confirm's time is not stored, as one confirmed before that time was kept).
#[tokio::test(flavor = "multi_thread")]
async fn an_expired_redeem_leaves_no_other_accounts_record() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let run = cli_run();
    let shown = paste_leg(&env, &Browser::new(), &run).await;
    env.invite("bob", "bob@dakai.example").await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-bob".into();
        p.upn = "bob@dakai.example".into();
        p.name = "Bob".into();
    }
    let bob = Browser::new();
    let (status, page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    env.advance(301);
    env.store.m().cli_logins.clear();
    let taken = Arc::new(tokio::sync::Notify::new());
    let (release, held) = std::sync::mpsc::channel();
    env.store.m().cli_gate = Some(TakeGate { taken: taken.clone(), release: held });
    let late = paste_redeem(&env, &run, &shown);
    let confirm = async {
        tokio::time::timeout(Duration::from_secs(5), taken.notified()).await.expect("the late redeem takes the record");
        let (status, body) = paste_confirm(&env, &bob, &run, &csrf_of(&page)).await;
        release.send(()).unwrap();
        (status, body)
    };
    let ((status, got), (confirmed, body)) = tokio::join!(late, confirm);
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{got}");
    assert!(got["message"].as_str().unwrap().contains("expired"), "{got}");
    assert_eq!(confirmed, StatusCode::OK, "bob's confirm landed in the gap: {body}");
    assert!(env.store.m().codes.is_empty(), "the expired login is not left to bob");
}

#[tokio::test]
async fn pasted_code_links_are_validated_and_the_two_flows_do_not_mix() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    for query in [
        format!("mode=code&port=5000&state={}&challenge={}", run.state, run.challenge),
        format!("mode=paste&state={}&challenge={}", run.state, run.challenge),
        format!("mode=code&state=short&challenge={}", run.challenge),
        format!("mode=code&state={}", run.state),
    ] {
        let (status, _) = b.peek(&env.url(&format!("/auth/cli?{query}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
    // a loopback login is not redeemable with a short code, nor a pasted one with a 64-hex code
    let code = browser_leg(&env, &b, &run).await;
    let (status, got) = paste_redeem(&env, &run, "2345-6789").await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("authorization_pending")), "{got}");
    let other = cli_run();
    paste_leg(&env, &b, &other).await;
    let (status, _) = paste_redeem(&env, &other, &format!("{:064x}", 7)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, got) =
        redeem(&env, json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "h"})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(env.store.m().creds.len(), 1);
}

// --- key display, disabled users ------------------------------------------------------------------

#[tokio::test]
async fn a_created_key_is_shown_once_and_a_refresh_mints_nothing() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let csrf = csrf_of(&b.sign_in(&env).await);
    let key = key_of(&b.create_key(&env, &csrf, "box").await);
    assert_eq!(env.store.live_host_of(&key).as_deref(), Some("box"));
    let (status, loc) = b.peek(&env.url("/keys/created")).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/keys"));
    assert_eq!(env.store.m().creds.len(), 1);

    // A display that waited too long is not shown.
    let r = b
        .post_form(&env.url("/keys"), &[("csrf", &csrf), ("label", "late")])
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    env.advance(121);
    let (status, loc) = b.peek(&env.url("/keys/created")).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/keys"));
}

#[tokio::test]
async fn a_disabled_user_loses_the_session_and_cannot_sign_in_or_redeem() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    b.sign_in(&env).await;
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;

    env.store.set_disabled("krisztian-dobmayer", true).unwrap();
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(loc.starts_with("/auth/login"), "{loc}");
    assert!(env.store.m().sessions.is_empty());
    let (status, _, body) = b.get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("disabled"), "{body}");

    let body =
        json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "h"});
    let (status, got) = redeem(&env, body).await;
    assert_eq!(
        (status, got["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("disabled"))
    );
    assert!(env.store.m().creds.is_empty());
}

// --- admin API --------------------------------------------------------------------------------

#[tokio::test]
async fn the_admin_api_requires_the_admin_key() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    for key in [None, Some("wrong"), Some(&ADMIN_KEY[1..])] {
        let (status, _) = admin(&env, "/v1/admin/keys", None, key).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{key:?}");
        let body = json!({"user": "bob", "host_label": "box"});
        let (status, _) = admin(&env, "/v1/admin/keys", Some(body), key).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let r = reqwest::Client::new()
        .get(env.url("/v1/admin/keys"))
        .basic_auth("admin", Some(ADMIN_KEY))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert!(env.store.m().creds.is_empty());
    assert!(
        web::AdminState::with_clock(
            env.store.clone(),
            env.registry.clone(),
            "short",
            None,
            Arc::new(|| 0)
        )
        .is_err()
    );
}

#[tokio::test]
async fn the_admin_mints_lists_and_revokes_keys() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let k = Some(ADMIN_KEY);
    let (status, got) = admin(
        &env,
        "/v1/admin/keys",
        Some(json!({"user": "Bob", "host_label": "Build Box"})),
        k,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        (got["user"].as_str(), got["host_id"].as_str()),
        (Some("bob"), Some("build-box"))
    );
    let key = got["key"].as_str().unwrap().to_string();
    let key_id = got["key_id"].as_str().unwrap().to_string();
    assert_eq!(key_id, token_digest(&key)[..12]);
    assert_eq!(env.store.live_host_of(&key).as_deref(), Some("build-box"));

    let (status, got) = admin(
        &env,
        "/v1/admin/keys",
        Some(json!({"user": "carol", "host_label": "x", "host": "build-box"})),
        k,
    )
    .await;
    assert_eq!(
        (status, got["error"].as_str()),
        (StatusCode::CONFLICT, Some("host_taken"))
    );
    let (status, _) = admin(
        &env,
        "/v1/admin/keys",
        Some(json!({"user": "a/b", "host_label": "x"})),
        k,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, list) = admin(&env, "/v1/admin/keys", None, k).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["keys"][0]["user"], "bob");
    assert_eq!(list["keys"][0]["key_id"], key_id.as_str());
    assert_eq!(list["keys"][0]["host_id"], "build-box");
    assert!(!list.to_string().contains(&key), "keys are never listed");

    env.registry.connect("build-box");
    let path = format!("/v1/admin/keys/{key_id}/revoke");
    let (status, got) = admin(&env, &path, Some(json!({})), k).await;
    assert_eq!(
        (status, got["host_id"].as_str()),
        (StatusCode::OK, Some("build-box"))
    );
    assert!(env.store.live_host_of(&key).is_none());
    assert!(!env.registry.is_open("build-box"));
    let (status, _) = admin(&env, &path, Some(json!({})), k).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = admin(&env, "/v1/admin/keys/xyz/revoke", Some(json!({})), k).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_admin_pre_creates_and_binds_users() {
    let env = Env::uninvited(Arc::new(MemStore::default()), |_| {}).await;
    let k = Some(ADMIN_KEY);
    let x = json!({"tid": TENANT, "oid": "oid-x", "display": "Xavier Ys"});
    let (status, got) = admin(&env, "/v1/admin/users", Some(x.clone()), k).await;
    assert_eq!(
        (status, got["user"].as_str(), got["status"].as_str()),
        (StatusCode::OK, Some("xavier-ys"), Some("created"))
    );
    let (_, got) = admin(&env, "/v1/admin/users", Some(x), k).await;
    assert_eq!(got["status"], "exists");
    let other = json!({"tid": "00000000-0000-0000-0000-000000000000", "oid": "o", "display": "Z"});
    assert_eq!(
        admin(&env, "/v1/admin/users", Some(other), k).await.0,
        StatusCode::BAD_REQUEST
    );

    // An admin-created user is bound to its Entra identity once; sign-in then lands on it.
    let id = env.store.add_plain_user("krisztian-dobmayer");
    let derived = json!({"tid": TENANT, "oid": "oid-krisztian", "preferred_username": "krisztian.dobmayer@dakai.example"});
    let (status, got) = admin(&env, "/v1/admin/users", Some(derived), k).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    let bind = json!({"tid": TENANT, "oid": "oid-krisztian", "name": "krisztian-dobmayer"});
    let (status, got) = admin(&env, "/v1/admin/users", Some(bind), k).await;
    assert_eq!(
        (status, got["status"].as_str()),
        (StatusCode::OK, Some("bound"))
    );
    Browser::new().sign_in(&env).await;
    assert_eq!(env.store.m().users.len(), 2);
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().0, id);

    let steal = json!({"tid": TENANT, "oid": "oid-other", "name": "krisztian-dobmayer"});
    assert_eq!(
        admin(&env, "/v1/admin/users", Some(steal), k).await.0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn the_admin_invites_lists_and_binds_by_issuer_and_subject() {
    let env = Env::uninvited(Arc::new(MemStore::default()), |_| {}).await;
    let k = Some(ADMIN_KEY);
    let post = |body: Value| admin(&env, "/v1/admin/users", Some(body), k);
    let (status, got) = post(json!({"name": "Ann", "email": " ann@dakai.example "})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, json!({"user": "ann", "email": "ann@dakai.example", "status": "invited", "disabled": false}));
    assert_eq!(post(json!({"name": "ann", "email": "ANN@dakai.example"})).await.1["status"], "exists");
    let (status, got) = post(json!({"name": "ann", "email": "ann@elsewhere.example"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    let (status, got) = post(json!({"name": "bob", "email": "Ann@Dakai.example"})).await;
    assert_eq!((status, got["message"].as_str()), (StatusCode::CONFLICT, Some("user 'ann' has this email already")));
    env.store.add_plain_user("carl");
    assert_eq!(post(json!({"name": "carl", "email": "carl@dakai.example"})).await.1["status"], "email set");
    for bad in [
        json!({"name": "dan", "email": "not-an-address"}),
        json!({"name": "a/b", "email": "dan@dakai.example"}),
        json!({"email": "dan@dakai.example"}),
        json!({}),
        json!({"name": "dan", "email": "dan@dakai.example", "tid": TENANT, "oid": "o"}),
        json!({"issuer": env.idp.issuer, "subject": "s", "tid": TENANT, "oid": "o"}),
        json!({"issuer": "https://login.microsoftonline.com/other/v2.0", "subject": "s", "name": "ann"}),
        json!({"issuer": env.idp.issuer, "subject": "has space", "name": "ann"}),
    ] {
        let (status, got) = post(bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {got}");
    }
    assert!(env.store.user("dan").is_none());

    // {issuer, subject} binds once per issuer.
    let bind = json!({"issuer": env.idp.issuer, "subject": "sub-ann", "name": "ann"});
    assert_eq!(post(bind.clone()).await.1["status"], "bound");
    assert_eq!(post(bind).await.1["status"], "exists");
    let again = json!({"issuer": env.idp.issuer, "subject": "sub-ann-2", "name": "ann"});
    assert_eq!(post(again).await.0, StatusCode::CONFLICT);
    // one identity binds one user: ann's identity is not given to carl
    let (status, got) = post(json!({"issuer": env.idp.issuer, "subject": "sub-ann", "name": "carl"})).await;
    assert_eq!((status, got["message"].as_str()), (StatusCode::CONFLICT, Some("the identity is already bound to 'ann'")));

    let (status, list) = admin(&env, "/v1/admin/users", None, k).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list,
        json!({"users": [
            {"name": "ann", "email": "ann@dakai.example", "issuers": [env.idp.issuer], "disabled": false},
            {"name": "carl", "email": "carl@dakai.example", "issuers": [], "disabled": false},
        ]})
    );
    assert_eq!(admin(&env, "/v1/admin/users", None, Some("wrong")).await.0, StatusCode::UNAUTHORIZED);

    // A board signing in with a non-Entra issuer takes no {tid, oid}.
    let realm = Env::uninvited(Arc::new(MemStore::default()), |c| c.entra_tenant = None).await;
    let (status, got) = admin(&realm, "/v1/admin/users", Some(json!({"tid": TENANT, "oid": "o", "name": "x"})), k).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    assert!(got["message"].as_str().unwrap().contains("not Entra; pass issuer and subject"), "{got}");
}

#[tokio::test]
async fn the_admin_disables_and_enables_users() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let k = Some(ADMIN_KEY);
    admin(
        &env,
        "/v1/admin/keys",
        Some(json!({"user": "bob", "host_label": "box"})),
        k,
    )
    .await;
    env.registry.connect("box");
    let (status, got) = admin(&env, "/v1/admin/users/bob/disable", Some(json!({})), k).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["disconnected"], json!(["box"]));
    assert!(!env.registry.is_open("box"));
    let (status, got) = admin(
        &env,
        "/v1/admin/keys",
        Some(json!({"user": "bob", "host_label": "box2"})),
        k,
    )
    .await;
    assert_eq!(
        (status, got["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("disabled"))
    );
    let (status, got) = admin(&env, "/v1/admin/users/bob/enable", Some(json!({})), k).await;
    assert_eq!(
        (status, got["disabled"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    assert_eq!(
        admin(&env, "/v1/admin/users/nobody/disable", Some(json!({})), k)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

// --- the real server: revocation closes a live bridge ------------------------------------------

/// A board with no messages that records which bridge instances fenced their host.
#[derive(Default)]
struct NoBoard {
    fences: Mutex<Vec<String>>,
}

impl conde_remote::Dispatcher for NoBoard {
    fn dispatch(&self, _: &conde_remote::Principal, _: Value) -> Value {
        json!({"v": 1, "type": "response", "ok": true, "result": {}})
    }
}

impl conde_remote::NotificationSource for NoBoard {
    fn pending_for_host(&self, _: &str, _: &str) -> Vec<conde_remote::Notify> {
        Vec::new()
    }
    fn record_outcome(
        &self,
        _: &str,
        _: &str,
        _: &conde_remote::Outcome,
    ) -> conde_remote::Recorded {
        conde_remote::Recorded::Accepted
    }
    fn fence(&self, _: &str, instance: &str) -> conde_remote::FenceResult {
        self.fences.lock().unwrap().push(instance.into());
        conde_remote::FenceResult::Active
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_key_or_disabling_its_user_closes_the_live_bridge() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let store = Arc::new(MemStore::default());
    let config = conde_remote::ServerConfig {
        poll_interval: Duration::from_millis(50),
        web: Some(web::WebSetup {
            entra: None,
            admin_key: Some(ADMIN_KEY.into()),
            store: store.clone(),
        }),
        ..conde_remote::ServerConfig::default()
    };
    let server = conde_remote::start(
        "127.0.0.1:0".parse().unwrap(),
        config,
        Arc::new(MemAuth(store.clone())),
        Arc::new(NoBoard::default()),
        Arc::new(NoBoard::default()),
    )
    .await
    .unwrap();
    let addr = server.local_addr();
    let http = reqwest::Client::new();
    let call = |path: &str, body: Value| {
        http.post(format!("http://{addr}{path}"))
            .bearer_auth(ADMIN_KEY)
            .json(&body)
            .send()
    };
    let connect = |key: &str, host: &str| {
        let mut c = conde_remote::BridgeConfig::new(format!("ws://{addr}/v1/bridge"), key, host);
        c.backoff_min = Duration::from_millis(50);
        c.backoff_max = Duration::from_millis(100);
        conde_remote::BridgeClient::start(c, |_| {})
    };
    let connected = |client: &conde_remote::BridgeClient| {
        let mut rx = client.subscribe();
        async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                rx.wait_for(|s| *s == conde_remote::BridgeState::Connected),
            )
            .await
            .expect("the bridge connects")
            .unwrap();
        }
    };

    let minted: Value = call(
        "/v1/admin/keys",
        json!({"user": "bob", "host_label": "box"}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let client = connect(minted["key"].as_str().unwrap(), "box");
    connected(&client).await;
    let path = format!(
        "/v1/admin/keys/{}/revoke",
        minted["key_id"].as_str().unwrap()
    );
    assert!(call(&path, json!({})).await.unwrap().status().is_success());
    let reason = tokio::time::timeout(Duration::from_secs(5), client.stopped())
        .await
        .expect("closed at once");
    assert_eq!(reason, conde_remote::StopReason::Unauthorized);

    let minted: Value = call(
        "/v1/admin/keys",
        json!({"user": "bob", "host_label": "box"}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let client = connect(minted["key"].as_str().unwrap(), "box");
    connected(&client).await;
    assert!(
        call("/v1/admin/users/bob/disable", json!({}))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let reason = tokio::time::timeout(Duration::from_secs(5), client.stopped())
        .await
        .expect("closed at once");
    assert_eq!(reason, conde_remote::StopReason::Unauthorized);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_key_display_is_accepted_once_and_only_as_issued() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let csrf = csrf_of(&b.sign_in(&env).await);
    let r = b
        .post_form(&env.url("/keys"), &[("csrf", &csrf), ("label", "box")])
        .await;
    let issued = r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    let (status, _, page) = b.get(&env.url("/keys/created")).await;
    assert_eq!(status, StatusCode::OK);
    let key = key_of(&page);
    assert!(
        issued.contains(&key) && !issued.contains("Max-Age"),
        "{issued}"
    );

    // Replaying the issued cookie after the display shows nothing: acceptance is server-side.
    let url: reqwest::Url = env.url("/keys/created").parse().unwrap();
    b.jar.add_cookie_str(&issued, &url);
    let (status, loc) = b.peek(&env.url("/keys/created")).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/keys"));

    // A forged token, or the right token with another key, is refused.
    let r = b
        .post_form(&env.url("/keys"), &[("csrf", &csrf), ("label", "box2")])
        .await;
    let issued = r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    let value = issued.split(';').next().unwrap();
    let (name, token_key) = value.split_once('=').unwrap();
    let token = token_key.split(':').next().unwrap();
    let swapped = format!(
        "{name}={token}:conde_{}; Path=/keys/created",
        "0".repeat(64)
    );
    b.jar.add_cookie_str(&swapped, &url);
    let (status, loc) = b.peek(&env.url("/keys/created")).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/keys"));
    assert!(
        env.store.m().displays.is_empty(),
        "a refused display is consumed too"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_socket_holding_its_hello_cannot_fence_the_valid_replacement() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let store = Arc::new(MemStore::default());
    let board = Arc::new(NoBoard::default());
    let config = conde_remote::ServerConfig {
        web: Some(web::WebSetup {
            entra: None,
            admin_key: Some(ADMIN_KEY.into()),
            store: store.clone(),
        }),
        ..conde_remote::ServerConfig::default()
    };
    let server = conde_remote::start(
        "127.0.0.1:0".parse().unwrap(),
        config,
        Arc::new(MemAuth(store)),
        board.clone(),
        board.clone(),
    )
    .await
    .unwrap();
    let addr = server.local_addr();
    let http = reqwest::Client::new();
    let mint = || {
        http.post(format!("http://{addr}/v1/admin/keys"))
            .bearer_auth(ADMIN_KEY)
            .json(&json!({"user": "bob", "host_label": "box"}))
            .send()
    };
    let first: Value = mint().await.unwrap().json().await.unwrap();
    let mut request = format!("ws://{addr}/v1/bridge")
        .into_client_request()
        .unwrap();
    let bearer = format!("Bearer {}", first["key"].as_str().unwrap());
    request
        .headers_mut()
        .insert("Authorization", HeaderValue::from_str(&bearer).unwrap());
    // Upgraded with the old key; the hello is held back.
    let (mut stale, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let second: Value = mint().await.unwrap().json().await.unwrap();
    assert_eq!(second["replaced"], 1);
    let mut config = conde_remote::BridgeConfig::new(
        format!("ws://{addr}/v1/bridge"),
        second["key"].as_str().unwrap(),
        "box",
    );
    config.bridge_instance = Some("valid".into());
    let valid = conde_remote::BridgeClient::start(config, |_| {});
    let probe = || {
        tokio::time::timeout(
            Duration::from_secs(3),
            valid.call(json!({"op": "probe", "request_id": "p"})),
        )
    };
    assert!(
        probe().await.unwrap().is_ok(),
        "the replacement is registered"
    );

    let hello = json!({"v": 1, "type": "hello", "host_id": "box", "bridge_instance": "revoked-delayed", "endpoints": []});
    stale.send(Message::text(hello.to_string())).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(3), stale.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(frame.to_string().contains("revoked"), "{frame:?}");
    assert_eq!(*board.fences.lock().unwrap(), vec!["valid".to_string()]);
    assert!(
        probe().await.unwrap().is_ok(),
        "the replacement still serves"
    );
    assert_eq!(valid.state(), conde_remote::BridgeState::Connected);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_keys_page_needs_sign_in_and_unknown_paths_are_404() {
    // `/` belongs to the server's web app (tested with the board in conde's web_ui suite).
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    assert_eq!(
        b.peek(&env.url("/keys")).await.1,
        "/auth/login?next=%2Fkeys"
    );
    let r = b.step.get(env.url("/nope")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

/// Two confirming POSTs of one pasted-code login by its account, the second arriving while the
/// first is held between reading the pending record and writing its own: both are answered (no
/// 500 from a duplicate key), and the code shown last is the one that redeems.
#[tokio::test(flavor = "multi_thread")]
async fn confirms_racing_on_one_pasted_code_login_both_succeed() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    let (status, page) = paste_confirm_page(&env, &b, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let csrf = csrf_of(&page);
    paste_leg(&env, &b, &run).await;
    let taken = Arc::new(tokio::sync::Notify::new());
    let (release, held) = std::sync::mpsc::channel();
    env.store.m().cli_gate = Some(TakeGate { taken: taken.clone(), release: held });
    let first = paste_confirm(&env, &b, &run, &csrf);
    let second = async {
        tokio::time::timeout(Duration::from_secs(5), taken.notified()).await.expect("the first confirm reaches the store");
        let second = paste_confirm(&env, &b, &run, &csrf);
        tokio::pin!(second);
        // It completes in the gap (two transactions) or waits for the first (one).
        let early = tokio::time::timeout(Duration::from_millis(500), &mut second).await.ok();
        release.send(()).unwrap();
        match early {
            Some(done) => done,
            None => second.await,
        }
    };
    let ((s1, p1), (s2, p2)) = tokio::join!(first, second);
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK), "{p1}\n{p2}");
    assert_eq!(env.store.m().codes.len(), 1, "one record for the login");
    let (shown1, shown2) = (key_of(&p1), key_of(&p2));
    let mut redeemed = 0;
    for shown in [&shown1, &shown2] {
        let (status, _) = paste_redeem(&env, &run, shown).await;
        redeemed += usize::from(status == StatusCode::OK);
    }
    assert_eq!(redeemed, 1, "one of the two codes is live");
}

/// A wrong pasted code is put back with one more try spent; if the account confirms again while
/// that redeem holds the taken record, the new record stands and the redeem still answers.
#[tokio::test(flavor = "multi_thread")]
async fn a_confirm_during_a_wrong_guess_keeps_the_new_code() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    let taken = Arc::new(tokio::sync::Notify::new());
    let (release, held) = std::sync::mpsc::channel();
    env.store.m().cli_gate = Some(TakeGate { taken: taken.clone(), release: held });
    let guess = paste_redeem(&env, &run, wrong);
    let confirm = async {
        tokio::time::timeout(Duration::from_secs(5), taken.notified()).await.expect("the redeem takes the record");
        let fresh = paste_leg(&env, &b, &run).await;
        release.send(()).unwrap();
        fresh
    };
    let ((status, got), fresh) = tokio::join!(guess, confirm);
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_code")), "{got}");
    let (status, got) = paste_redeem(&env, &run, &fresh).await;
    assert_eq!((status, got["user"].as_str()), (StatusCode::OK, Some("krisztian-dobmayer")), "{got}");
}

/// A wrong guess holds the taken record while another account confirms the same login from its
/// URL: that confirm finds the slot empty and its record goes in, but the guess's restore puts
/// the first account's record back, so the run keeps waiting on the first account's code.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_guess_gets_the_login_back_from_another_accounts_confirm() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let run = cli_run();
    let shown = paste_leg(&env, &Browser::new(), &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    env.invite("bob", "bob@dakai.example").await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-bob".into();
        p.upn = "bob@dakai.example".into();
        p.name = "Bob".into();
    }
    let bob = Browser::new();
    let (status, page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let taken = Arc::new(tokio::sync::Notify::new());
    let (release, held) = std::sync::mpsc::channel();
    env.store.m().cli_gate = Some(TakeGate { taken: taken.clone(), release: held });
    let guess = paste_redeem(&env, &run, wrong);
    let confirm = async {
        tokio::time::timeout(Duration::from_secs(5), taken.notified()).await.expect("the redeem takes the record");
        let (status, body) = paste_confirm(&env, &bob, &run, &csrf_of(&page)).await;
        release.send(()).unwrap();
        assert_eq!(status, StatusCode::OK, "the slot was empty: {body}");
        key_of(&body)
    };
    let ((status, got), bobs) = tokio::join!(guess, confirm);
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_code")), "{got}");
    {
        let m = env.store.m();
        let held: Vec<_> = m.codes.values().map(|c| (c.user_name.as_str(), c.tries)).collect();
        assert_eq!(held, [("krisztian-dobmayer", 1)], "the first account's record is back, one try spent");
    }
    let (status, got) = paste_redeem(&env, &run, &bobs).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_code")), "{got}");
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, got["user"].as_str()), (StatusCode::OK, Some("krisztian-dobmayer")), "{got}");
}

// --- a store without human paging ----------------------------------------------------------------

/// A store with only the required methods (sign-in, keys, admin), none of which this test calls:
/// the Slack member id is the trait's default.
struct Minimal;

impl WebStore for Minimal {
    fn user_by_identity(&self, _: &str, _: &str) -> WebResult<Option<WebUser>> { unreachable!() }
    fn create_user(&self, _: &Identity, _: &str) -> WebResult<CreateUser> { unreachable!() }
    fn set_profile(&self, _: i64, _: &Identity) -> WebResult<()> { unreachable!() }
    fn user_disabled(&self, _: i64) -> WebResult<bool> { unreachable!() }
    fn insert_session(&self, _: &str, _: &SessionRecord) -> WebResult<bool> { unreachable!() }
    fn session(&self, _: &str) -> WebResult<Option<SessionRecord>> { unreachable!() }
    fn touch_session(&self, _: &str, _: i64) -> WebResult<()> { unreachable!() }
    fn delete_session(&self, _: &str) -> WebResult<()> { unreachable!() }
    fn insert_login(&self, _: &str, _: &LoginTxn) -> WebResult<()> { unreachable!() }
    fn take_login(&self, _: &str) -> WebResult<Option<LoginTxn>> { unreachable!() }
    fn insert_cli_code(&self, _: &str, _: &CliCode) -> WebResult<()> { unreachable!() }
    fn take_cli_code(&self, _: &str) -> WebResult<Option<CliCode>> { unreachable!() }
    fn issue_cli_code(&self, _: &str, _: &CliCode, _: i64) -> WebResult<CliIssued> { unreachable!() }
    fn restore_cli_code(&self, _: &str, _: &CliCode) -> WebResult<()> { unreachable!() }
    fn end_cli_code(&self, _: &str, _: i64) -> WebResult<()> { unreachable!() }
    fn insert_key_display(&self, _: &str, _: &KeyDisplay) -> WebResult<()> { unreachable!() }
    fn take_key_display(&self, _: &str) -> WebResult<Option<KeyDisplay>> { unreachable!() }
    fn purge_expired(&self, _: i64, _: &Limits) -> WebResult<()> { unreachable!() }
    fn host_keys(&self, _: i64) -> WebResult<Vec<HostKey>> { unreachable!() }
    fn host_owner(&self, _: &str) -> WebResult<Option<i64>> { unreachable!() }
    fn mint_host_key(&self, _: i64, _: &str, _: &str, _: &str, _: i64) -> WebResult<Minted> { unreachable!() }
    fn revoke_host(&self, _: i64, _: &str, _: i64) -> WebResult<u64> { unreachable!() }
    fn ensure_user(&self, _: &str) -> WebResult<WebUser> { unreachable!() }
    fn bind_identity(&self, _: &str, _: &Identity) -> WebResult<BindUser> { unreachable!() }
    fn claim_invite(&self, _: &Identity, _: &str) -> WebResult<Invite> { unreachable!() }
    fn invite_user(&self, _: &str, _: &str) -> WebResult<Invited> { unreachable!() }
    fn list_users(&self) -> WebResult<Vec<AdminUser>> { unreachable!() }
    fn all_keys(&self) -> WebResult<Vec<AdminKey>> { unreachable!() }
    fn revoke_key(&self, _: &str, _: i64) -> WebResult<Option<String>> { unreachable!() }
    fn set_disabled(&self, _: &str, _: bool) -> WebResult<Option<Vec<String>>> { unreachable!() }
}

/// The trait's default for the Slack member id, called directly: refused.
#[test]
fn a_store_without_human_paging_refuses_a_slack_member_id() {
    let e = Minimal.set_slack_member("krisz", "U0123").unwrap_err();
    assert_eq!(e.to_string(), "this board has no Slack member ids");
}

/// A login cookie longer than four real states is not split or matched (even when it holds this
/// browser's state) and never comes back in a Set-Cookie; an entry longer than a real state is
/// dropped.
#[tokio::test]
async fn an_oversized_login_cookie_is_ignored_and_never_echoed() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let callback = started_callback(&env, &b).await;
    let state = state_of(&callback);
    let raw = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // A real cookie is at most 4 states of 22 characters; every Set-Cookie stays near that.
    let short = |set: &[String]| set.iter().all(|c| c.len() <= 400);
    let huge = format!(
        "conde_login={state}{}",
        ".AAAAAAAAAAAAAAAAAAAAAA".repeat(8000)
    );
    let r = raw
        .get(&callback)
        .header(header::COOKIE, &huge)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], SIGN_IN_AGAIN);
    assert!(short(&set_cookies(&r)), "{:?}", set_cookies(&r));
    assert!(
        env.store.m().sessions.is_empty(),
        "an oversized cookie matches nothing"
    );
    let r = raw
        .get(env.url("/auth/login?next=%2Fkeys"))
        .header(header::COOKIE, &huge)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let set = set_cookies(&r);
    assert!(
        short(&set) && set.iter().any(|c| c.starts_with("conde_login=")),
        "{set:?}"
    );

    let long_entry = format!("conde_login={}.{state}", "x".repeat(100));
    let r = raw
        .get(&callback)
        .header(header::COOKIE, long_entry)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()[header::LOCATION],
        "/keys",
        "the real state still matches"
    );
    let set = set_cookies(&r);
    assert!(
        !set.iter().any(|c| c.contains("xxxx")),
        "the long entry is dropped: {set:?}"
    );
}

/// A browser that keeps no cookies cannot finish a sign-in: the stale callback's one retry finds
/// no marker and shows why, instead of looping through the provider.
#[tokio::test]
async fn a_browser_that_keeps_no_cookies_is_told_instead_of_looping() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let cookieless = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .unwrap();
    let r = cookieless
        .get(env.url("/keys"))
        .send()
        .await
        .expect("no redirect loop");
    assert_eq!(r.url().path(), "/auth/login");
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let page = r.text().await.unwrap();
    assert!(
        page.contains("not keeping conde&#39;s cookies")
            || page.contains("not keeping conde&#x27;s cookies"),
        "{page}"
    );
    assert!(
        page.contains(r#"<a href="/">Go to the board</a>"#),
        "{page}"
    );
    assert_eq!(
        env.idp.authorizations.load(Ordering::SeqCst),
        1,
        "one trip to the provider"
    );

    // retry=1 without the marker: no sign-in row, no redirect to the provider
    let logins = env.store.m().logins.len();
    let (status, at, _) = Browser::new().get(&env.url(SIGN_IN_AGAIN)).await;
    assert_eq!(
        (status, at),
        (StatusCode::BAD_REQUEST, env.url(SIGN_IN_AGAIN))
    );
    assert_eq!(env.store.m().logins.len(), logins);
    assert_eq!(env.idp.authorizations.load(Ordering::SeqCst), 1);
    // with the marker the retry is an ordinary sign-in
    let r = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(env.url(SIGN_IN_AGAIN))
        .header(header::COOKIE, "conde_login_retry=1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let to = r.headers()[header::LOCATION].to_str().unwrap();
    assert!(to.contains("/oauth2/v2.0/authorize?"), "{to}");
    assert_eq!(env.store.m().logins.len(), logins + 1);
}

/// An expired sign-in restarts toward the page it was started for.
#[tokio::test]
async fn an_expired_sign_in_restarts_to_its_own_return_path() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let callback = started_callback(&env, &b).await;
    env.advance(601);
    assert_eq!(
        b.peek(&callback).await,
        (
            StatusCode::SEE_OTHER,
            "/auth/login?next=%2Fkeys&retry=1".to_string()
        )
    );
}

/// A forged state joining two pending states with the cookie's separator, and an empty state,
/// match nothing: each is a single state this browser did not start.
#[tokio::test]
async fn joined_and_empty_states_match_nothing() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let b = Browser::new();
    let first = started_callback(&env, &b).await;
    let second = started_callback(&env, &b).await;
    let joined = format!("{}.{}", state_of(&first), state_of(&second));
    assert_eq!(
        b.peek(&with_state(&first, &joined)).await,
        (StatusCode::SEE_OTHER, SIGN_IN_AGAIN.to_string())
    );
    // the retry marker is set now, so the next stale callback shows the page
    assert_eq!(
        b.peek(&with_state(&first, "")).await,
        (StatusCode::BAD_REQUEST, String::new())
    );
    assert!(env.store.m().sessions.is_empty());
    assert_eq!(
        env.store.m().logins.len(),
        2,
        "neither pending sign-in was consumed"
    );
    assert_eq!(
        b.peek(&first).await,
        (StatusCode::SEE_OTHER, "/keys".to_string())
    );
    assert_eq!(
        b.peek(&second).await,
        (StatusCode::SEE_OTHER, "/keys".to_string())
    );
}
