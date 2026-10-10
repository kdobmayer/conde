//! The web surface (Entra sign-in, keys page, `conde login` browser leg) against an
//! in-process fake OpenID provider, an in-memory store and a fake bridge registry.

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
    AdminKey, BindUser, BridgeRegistry, CliCode, CreateUser, HostKey, Identity, KeyDisplay, Limits,
    LoginTxn, Minted, SessionRecord, WebConfig, WebResult, WebState, WebStore, WebUser,
};

include!("support/web_harness.rs");

// --- in-memory store ---------------------------------------------------------------------

struct User {
    id: i64,
    name: String,
    identity: Option<(String, String)>,
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
    displays: HashMap<String, KeyDisplay>,
    hosts: HashMap<String, i64>,
    creds: Vec<Cred>,
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
            identity: None,
            display: None,
            disabled: false,
        });
        id
    }
    /// (user id, bound identity, display name) of the user named `name`.
    fn user(&self, name: &str) -> Option<UserRow> {
        self.m()
            .users
            .iter()
            .find(|u| u.name == name)
            .map(|u| (u.id, u.identity.clone(), u.display.clone()))
    }
    fn live_host_of(&self, key: &str) -> Option<String> {
        let d = token_digest(key);
        self.m()
            .creds
            .iter()
            .find(|c| c.digest == d && c.revoked_at.is_none())
            .map(|c| c.host.clone())
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
    fn user_by_identity(&self, tid: &str, oid: &str) -> WebResult<Option<WebUser>> {
        let id = Some((tid.to_string(), oid.to_string()));
        Ok(self
            .m()
            .users
            .iter()
            .find(|u| u.identity == id)
            .map(User::web))
    }
    fn create_user(&self, identity: &Identity, name: &str) -> WebResult<CreateUser> {
        let mut m = self.m();
        if let Some(u) = m.users.iter().find(|u| u.name == name) {
            return Ok(CreateUser::NameTaken {
                bound: u.identity.is_some(),
            });
        }
        let id = m.users.len() as i64 + 1;
        m.users.push(User {
            id,
            name: name.into(),
            identity: Some((identity.tid.clone(), identity.oid.clone())),
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
        self.m().codes.insert(code_digest.into(), code.clone());
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
        Ok(self.m().codes.remove(code_digest))
    }
    fn purge_expired(&self, now: i64, l: &Limits) -> WebResult<()> {
        let mut m = self.m();
        m.sessions.retain(|_, s| {
            now - s.last_seen <= l.session_idle && now - s.created_at <= l.session_max
        });
        m.logins.retain(|_, t| now - t.created_at <= l.login_ttl);
        m.codes.retain(|_, c| now - c.created_at <= l.cli_code_ttl);
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
        Ok(Minted::Stored { replaced })
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
            Some(u) if u.identity.is_some() => Ok(BindUser::NameBound),
            Some(u) => {
                u.identity = Some((identity.tid.clone(), identity.oid.clone()));
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
async fn sign_in_creates_the_user_on_first_login_and_reuses_it() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    let page = Browser::new().sign_in(&env).await;
    assert!(
        page.contains("Host keys") && page.contains("Krisztián Dobmayer"),
        "{page}"
    );
    assert!(page.contains("No host keys yet"));
    let (id, identity, display) = env.store.user("krisztian-dobmayer").expect("user created");
    assert_eq!(
        identity,
        Some((TENANT.to_string(), "oid-krisztian".to_string()))
    );
    assert_eq!(display.as_deref(), Some("Krisztián Dobmayer"));

    // Same (tid, oid), new display name and upn: same user, profile updated, no new user.
    {
        let mut p = env.idp.person.lock().unwrap();
        p.name = "K. Dobmayer".into();
        p.upn = "renamed@dakai.example".into();
    }
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("K. Dobmayer"));
    assert_eq!(env.store.user("krisztian-dobmayer").unwrap().0, id);
    assert!(env.store.user("renamed").is_none());
    assert_eq!(
        env.idp.discoveries.load(Ordering::SeqCst),
        1,
        "metadata is cached"
    );
}

#[tokio::test]
async fn another_tenant_is_refused_and_no_user_is_created() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    env.idp.person.lock().unwrap().tid = "00000000-0000-0000-0000-000000000000".into();
    let b = Browser::new();
    let (status, _, body) = b.get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("another directory"), "{body}");
    assert!(env.store.m().users.is_empty());
    assert!(env.store.m().sessions.is_empty());
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(loc.starts_with("/auth/login"), "{loc}");
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
    assert!(env.store.m().users.is_empty());
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
    // Login CSRF: the victim's browser lands on the attacker's callback URL.
    let (status, _, body) = victim.get(&callback).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(env.store.m().sessions.is_empty());
    // The attacker's own browser may still finish it once; a replay is refused.
    let (status, _, _) = attacker.get(&callback).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = attacker.get(&callback).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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
async fn an_unlinked_admin_user_with_the_same_name_is_not_adopted() {
    let env = Env::with(Arc::new(MemStore::default())).await;
    env.store.add_plain_user("krisztian-dobmayer");
    let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body.contains("not linked") && body.contains("oid-krisztian"),
        "{body}"
    );
    assert_eq!(env.store.m().users.len(), 1);
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

    // the right code from another run (another verifier) counts as a miss too
    let thief = CliRun { state: run.state.clone(), ..cli_run() };
    let (status, got) = paste_redeem(&env, &thief, &shown).await;
    assert_eq!((status, got["error"].as_str(), got["tries_left"].as_u64()), (StatusCode::BAD_REQUEST, Some("invalid_code"), Some(4)));
    for left in [3, 2, 1] {
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
    let env = Env::with(Arc::new(MemStore::default())).await;
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
