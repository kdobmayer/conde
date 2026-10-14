//! The web surface over the board's real stores: Entra sign-in, browser sessions, the keys
//! page, the `conde login` leg, the admin API and disabled users, each case on SQLite and,
//! when CONDE_TEST_PG is set, on a fresh Postgres database. The fake provider and browser are
//! conde-remote's web test harness.

#![allow(dead_code)]

#[path = "pg/mod.rs"]
mod pg;

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
use conde::dispatch::Dispatcher;
use conde::store::Wake;
use conde::web_store::WebBoard;
use conde_remote::web::{self, BridgeRegistry, WebConfig, WebState, WebStore};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, sha2::Sha256};
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;

include!("../../conde-remote/tests/support/web_harness.rs");

/// Runs `f` off the async workers (the Postgres client blocks).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

/// A fresh board: Postgres when `pg` and CONDE_TEST_PG is set, else SQLite; None to skip.
async fn board(on_pg: bool) -> Option<Arc<Dispatcher>> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let url = if on_pg {
        match blocking(pg::fresh_database).await {
            Some(url) => Some(url),
            None => {
                eprintln!("skipped: set CONDE_TEST_PG to run the web suite on Postgres");
                return None;
            }
        }
    } else {
        None
    };
    Some(Arc::new(
        blocking(move || match url {
            Some(url) => Dispatcher::connect(&url, conde::store::postgres::PgOpen::SERVER).unwrap(),
            None => Dispatcher::open_in_memory().unwrap(),
        })
        .await,
    ))
}

/// Mints a key the way `conde admin key create` does.
async fn admin_cli_key(d: &Arc<Dispatcher>, user: &str, host: &str) -> String {
    let (d, user, host) = (d.clone(), user.to_string(), host.to_string());
    blocking(move || {
        let key = format!("conde_cli_{}", conde::util::new_ulid());
        let digest = conde_remote::token_digest(&key);
        d.with_tx(|c| conde::ops::hosts::insert_key(c, &user, &host, &digest, "cli", "2026-10-09T00:00:00.000Z"))
            .unwrap();
        key
    })
    .await
}

async fn authenticates(d: &Arc<Dispatcher>, key: &str) -> bool {
    let (d, digest) = (d.clone(), conde_remote::token_digest(key));
    blocking(move || d.with_conn(|c| c.authenticate(&digest)).unwrap().is_some()).await
}

/// The next Revoked wake within a few seconds, skipping other wakes.
async fn next_revoked(events: &mut tokio::sync::broadcast::Receiver<Wake>) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Wake::Revoked(host) = events.recv().await.unwrap() {
                return host;
            }
        }
    })
    .await
    .expect("a revocation wake")
}

/// Runs a case on its own runtime and drops the board after the runtime, off any async worker
/// (the Postgres client must not be torn down on one).
fn run<F: std::future::Future<Output = ()>>(on_pg: bool, case: impl FnOnce(Arc<Dispatcher>) -> F) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let Some(d) = rt.block_on(board(on_pg)) else { return };
    rt.block_on(case(d.clone()));
    drop(rt);
    drop(d);
}

macro_rules! on_both {
    ($($case:ident),* $(,)?) => {
        mod sqlite {
            $(#[test]
            fn $case() {
                super::run(false, super::$case);
            })*
        }
        mod postgres {
            $(#[test]
            fn $case() {
                super::run(true, super::$case);
            })*
        }
    };
}

on_both!(
    an_idle_session_does_not_survive_disable_and_enable,
    a_mint_racing_a_disable_is_refused,
    a_key_display_is_taken_once_under_concurrency,
    a_stale_host_revocation_leaves_a_valid_bridge_connected,
    sign_in_binds_the_identity_and_reuses_it,
    sessions_expire_and_disabled_users_are_signed_out,
    keys_page_mints_shows_once_and_revokes_with_a_wake,
    conde_login_codes_are_single_use_and_expire,
    a_re_enrolled_host_keeps_one_live_key,
    pasted_login_codes_count_tries_and_redeem_once,
    a_login_record_is_issued_and_restored_in_one_transaction,
    a_last_try_ends_the_login_record_unless_its_account_reissued,
    a_restore_takes_the_login_back_from_another_account,
    a_logins_first_confirm_dates_it_past_its_records_purge,
    a_late_confirm_is_told_the_login_expired_whatever_the_session,
    admin_binds_existing_users_and_disables_them,
);

async fn env(d: &Arc<Dispatcher>) -> Env<WebBoard> {
    Env::with(Arc::new(WebBoard::new(d.clone()))).await
}

async fn sign_in_binds_the_identity_and_reuses_it(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("Krisztián Dobmayer"), "{page}");
    let s = env.store.clone();
    let user = blocking(move || s.user_by_identity(TENANT, "oid-krisztian").unwrap()).await.unwrap();
    assert_eq!(user.name, "krisztian-dobmayer");
    env.idp.person.lock().unwrap().name = "K. D.".into();
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("K. D."));
    let s = env.store.clone();
    let again = blocking(move || s.user_by_identity(TENANT, "oid-krisztian").unwrap()).await.unwrap();
    assert_eq!(again.user_id, user.user_id);

    env.idp.person.lock().unwrap().tid = "00000000-0000-0000-0000-000000000000".into();
    env.idp.person.lock().unwrap().oid = "oid-stranger".into();
    let (status, _, _) = Browser::new().get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let s = env.store.clone();
    assert!(blocking(move || s.user_by_identity("00000000-0000-0000-0000-000000000000", "oid-stranger").unwrap()).await.is_none());
}

async fn sessions_expire_and_disabled_users_are_signed_out(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    b.sign_in(&env).await;
    env.advance(8 * 3600 + 1);
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/auth/login?next=%2Fkeys"));

    b.sign_in(&env).await;
    let key = admin_cli_key(&d, "krisztian-dobmayer", "box").await;
    assert!(authenticates(&d, &key).await);
    let mut events = d.store().pending().events();
    let (status, got) = admin(&env, "/v1/admin/users/krisztian-dobmayer/disable", Some(json!({})), Some(ADMIN_KEY)).await;
    assert_eq!((status, &got["disconnected"]), (StatusCode::OK, &json!(["box"])));
    assert_eq!(next_revoked(&mut events).await, "box");
    assert!(!authenticates(&d, &key).await, "a disabled user's key does not authenticate");
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert!(status == StatusCode::SEE_OTHER && loc.starts_with("/auth/login"), "{loc}");
    let (status, _, body) = b.get(&env.url("/keys")).await;
    assert!(status == StatusCode::FORBIDDEN && body.contains("disabled"), "{body}");

    admin(&env, "/v1/admin/users/krisztian-dobmayer/enable", Some(json!({})), Some(ADMIN_KEY)).await;
    assert!(authenticates(&d, &key).await);
    b.sign_in(&env).await;
}

async fn keys_page_mints_shows_once_and_revokes_with_a_wake(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    let csrf = csrf_of(&b.sign_in(&env).await);
    let key = key_of(&b.create_key(&env, &csrf, "Work Laptop").await);
    assert!(authenticates(&d, &key).await);
    let (status, loc) = b.peek(&env.url("/keys/created")).await;
    assert_eq!((status, loc.as_str()), (StatusCode::SEE_OTHER, "/keys"), "the display is single-use");
    let (_, _, list) = b.get(&env.url("/keys")).await;
    assert!(list.contains("<code>work-laptop</code>") && !list.contains(&key));

    // A second key for the host replaces the first and announces the host.
    let mut events = d.store().pending().events();
    let second = key_of(&b.create_key(&env, &csrf, "work laptop").await);
    assert_eq!(next_revoked(&mut events).await, "work-laptop");
    assert!(!authenticates(&d, &key).await && authenticates(&d, &second).await);

    let r = b.post_form(&env.url("/keys/work-laptop/revoke"), &[("csrf", &csrf)]).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(next_revoked(&mut events).await, "work-laptop");
    assert!(!authenticates(&d, &second).await);
    let (_, _, list) = b.get(&env.url("/keys")).await;
    assert!(list.contains("revoked "), "{list}");

    // Another user's host is suffixed, never shared.
    admin_cli_key(&d, "bob", "box").await;
    let mine = key_of(&b.create_key(&env, &csrf, "box").await);
    let (d2, digest) = (d.clone(), conde_remote::token_digest(&mine));
    let host = blocking(move || d2.with_conn(|c| c.authenticate(&digest)).unwrap().unwrap().1).await;
    assert_eq!(host, "box-2");
}

async fn conde_login_codes_are_single_use_and_expire(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let body = json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "Test-Box"});
    let (status, got) = redeem(&env, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["host_id"], "test-box");
    assert!(authenticates(&d, got["key"].as_str().unwrap()).await);
    assert_eq!(redeem(&env, body).await.0, StatusCode::BAD_REQUEST);

    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    env.advance(301);
    let (status, got) =
        redeem(&env, json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "h"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(got["message"].as_str().unwrap().contains("expired"), "{got}");
}

/// `conde login` again on an enrolled host: the server says so, revokes the previous key in
/// the same transaction, and the keys page shows the host once.
async fn a_re_enrolled_host_keeps_one_live_key(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    let mut keys = Vec::new();
    let mut events = d.store().pending().events();
    for again in [false, true, true] {
        let run = cli_run();
        let code = browser_leg(&env, &b, &run).await;
        let (status, got) = redeem(&env, json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "NUC"})).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!((got["host_id"].as_str(), got["reenrolled"].as_bool()), (Some("nuc"), Some(again)), "{got}");
        if again {
            assert_eq!(next_revoked(&mut events).await, "nuc");
        }
        keys.push(got["key"].as_str().unwrap().to_string());
    }
    let (newest, older) = keys.split_last().unwrap();
    assert!(authenticates(&d, newest).await);
    for old in older {
        assert!(!authenticates(&d, old).await, "a previous key is revoked");
    }
    let s = env.store.clone();
    let stored = blocking(move || {
        let user = s.user_by_identity(TENANT, "oid-krisztian").unwrap().unwrap();
        s.host_keys(user.user_id).unwrap()
    })
    .await;
    let live = stored.iter().filter(|k| k.host_id == "nuc" && k.revoked_at.is_none()).count();
    assert_eq!((stored.len(), live), (3, 1), "the history stays, one key is live");
    let (_, _, page) = b.get(&env.url("/keys")).await;
    assert_eq!(page.matches("<code>nuc</code>").count(), 1, "{page}");
    assert!(!page.contains("revoked "), "the row is the live key: {page}");

    // a host id another user owns gets a suffix: a new host, not a re-enrollment
    admin_cli_key(&d, "bob", "lab").await;
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let (_, got) = redeem(&env, json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "lab"})).await;
    assert_eq!((got["host_id"].as_str(), got["reenrolled"].as_bool()), (Some("lab-2"), Some(false)), "{got}");
}

async fn pasted_login_codes_count_tries_and_redeem_once(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    for left in [4, 3] {
        let (status, got) = paste_redeem(&env, &run, wrong).await;
        assert_eq!((status, got["tries_left"].as_u64()), (StatusCode::BAD_REQUEST, Some(left)), "{got}");
    }
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["host_id"], "head-less");
    assert!(authenticates(&d, got["key"].as_str().unwrap()).await);
    assert_eq!(paste_redeem(&env, &run, &shown).await.0, StatusCode::BAD_REQUEST);

    // the last wrong try ends the login
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') { "3333-3333" } else { "2222-2222" };
    for _ in 0..5 {
        assert_eq!(paste_redeem(&env, &run, wrong).await.0, StatusCode::BAD_REQUEST);
    }
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!((status, got["error"].as_str()), (StatusCode::BAD_REQUEST, Some("authorization_pending")), "{got}");
}

async fn admin_binds_existing_users_and_disables_them(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let k = Some(ADMIN_KEY);
    let key = admin_cli_key(&d, "krisztian-dobmayer", "laptop").await;
    // Sign-in refuses to adopt the unbound user; the admin binds it once.
    let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
    assert!(status == StatusCode::FORBIDDEN && body.contains("not linked"), "{body}");
    let bind = json!({"tid": TENANT, "oid": "oid-krisztian", "name": "krisztian-dobmayer"});
    let (status, got) = admin(&env, "/v1/admin/users", Some(bind.clone()), k).await;
    assert_eq!((status, got["status"].as_str()), (StatusCode::OK, Some("bound")));
    let (_, got) = admin(&env, "/v1/admin/users", Some(bind), k).await;
    assert_eq!(got["status"], "exists");
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("<code>laptop</code>"), "{page}");

    let (status, minted) = admin(&env, "/v1/admin/keys", Some(json!({"user": "bob", "host_label": "Build Box"})), k).await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    let (_, list) = admin(&env, "/v1/admin/keys", None, k).await;
    let ids: HashSet<&str> = list["keys"].as_array().unwrap().iter().map(|k| k["key_id"].as_str().unwrap()).collect();
    assert!(ids.contains(minted["key_id"].as_str().unwrap()) && ids.len() == 2, "{list}");
    let mut events = d.store().pending().events();
    let path = format!("/v1/admin/keys/{}/revoke", minted["key_id"].as_str().unwrap());
    assert_eq!(admin(&env, &path, Some(json!({})), k).await.0, StatusCode::OK);
    assert_eq!(next_revoked(&mut events).await, "build-box");
    assert!(!authenticates(&d, minted["key"].as_str().unwrap()).await);
    assert!(authenticates(&d, &key).await);
}

/// Two server replicas on one Postgres board: a key revoked (and a user disabled) through one
/// replica's admin API closes the live bridge connected to the other, by LISTEN/NOTIFY.
#[test]
fn revocation_closes_a_bridge_on_another_replica() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let Some(url) = rt.block_on(blocking(pg::fresh_database)) else {
        eprintln!("skipped: set CONDE_TEST_PG to run the two-replica revocation test");
        return;
    };
    let replica = |url: String| {
        rt.block_on(async move {
            let d = Arc::new(
                blocking(move || Dispatcher::connect(&url, conde::store::postgres::PgOpen::SERVER).unwrap()).await,
            );
            let config = conde_remote::ServerConfig {
                poll_interval: Duration::from_secs(30),
                web: Some(web::WebSetup {
                    entra: None,
                    admin_key: Some(ADMIN_KEY.into()),
                    store: Arc::new(WebBoard::new(d.clone())),
                }),
                ..conde_remote::ServerConfig::default()
            };
            let handle = conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config).await.unwrap();
            (d, handle.local_addr(), handle)
        })
    };
    let (d1, one, h1) = replica(url.clone());
    let (d2, two, h2) = replica(url);
    rt.block_on(async {
        let http = reqwest::Client::new();
        let call = |path: String, body: Value| http.post(format!("http://{one}{path}")).bearer_auth(ADMIN_KEY).json(&body).send();
        let connect = |key: &str| {
            let mut c = conde_remote::BridgeConfig::new(format!("ws://{two}/v1/bridge"), key, "box");
            c.backoff_min = Duration::from_millis(50);
            c.backoff_max = Duration::from_millis(100);
            conde_remote::BridgeClient::start(c, |_| {})
        };
        let connected = |c: &conde_remote::BridgeClient| {
            let mut rx = c.subscribe();
            async move {
                tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|s| *s == conde_remote::BridgeState::Connected))
                    .await
                    .expect("the bridge connects")
                    .unwrap();
            }
        };
        let stopped = |c: &conde_remote::BridgeClient| {
            let c = c.clone();
            async move { tokio::time::timeout(Duration::from_secs(10), c.stopped()).await.expect("closed by the other replica") }
        };

        let minted: Value = call("/v1/admin/keys".into(), json!({"user": "bob", "host_label": "box"}))
            .await.unwrap().json().await.unwrap();
        let client = connect(minted["key"].as_str().unwrap());
        connected(&client).await;
        assert!(client.call(json!({"op": "doctor", "request_id": "r"})).await.is_ok(), "the bridge is live on replica two");
        let path = format!("/v1/admin/keys/{}/revoke", minted["key_id"].as_str().unwrap());
        assert!(call(path, json!({})).await.unwrap().status().is_success());
        assert_eq!(stopped(&client).await, conde_remote::StopReason::Unauthorized);

        let minted: Value = call("/v1/admin/keys".into(), json!({"user": "bob", "host_label": "box"}))
            .await.unwrap().json().await.unwrap();
        let client = connect(minted["key"].as_str().unwrap());
        connected(&client).await;
        assert!(call("/v1/admin/users/bob/disable".into(), json!({})).await.unwrap().status().is_success());
        assert_eq!(stopped(&client).await, conde_remote::StopReason::Unauthorized);
        drop((h1, h2));
    });
    drop(rt);
    drop((d1, d2));
}

async fn an_idle_session_does_not_survive_disable_and_enable(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    b.sign_in(&env).await;
    let k = Some(ADMIN_KEY);
    assert_eq!(admin(&env, "/v1/admin/users/krisztian-dobmayer/disable", Some(json!({})), k).await.0, StatusCode::OK);
    // No browser request while disabled: the disable itself ends the session.
    assert_eq!(admin(&env, "/v1/admin/users/krisztian-dobmayer/enable", Some(json!({})), k).await.0, StatusCode::OK);
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert!(status == StatusCode::SEE_OTHER && loc.starts_with("/auth/login"), "{status} {loc}");
}

async fn a_mint_racing_a_disable_is_refused(d: Arc<Dispatcher>) {
    let store = Arc::new(WebBoard::new(d));
    blocking(move || {
        let user = store.ensure_user("race").unwrap();
        assert!(!store.user_disabled(user.user_id).unwrap()); // an earlier read saw the user enabled
        assert!(store.set_disabled("race", true).unwrap().is_some()); // another request commits
        let digest = conde_remote::token_digest("race-key");
        let result = store.mint_host_key(user.user_id, "late-host", "test", &digest, 1).unwrap();
        assert_eq!(result, web::Minted::Disabled);
        assert!(store.host_keys(user.user_id).unwrap().is_empty());
        let session = web::SessionRecord {
            user_id: user.user_id,
            user_name: "race".into(),
            display: "race".into(),
            csrf: "c".into(),
            created_at: 1,
            last_seen: 1,
        };
        assert!(!store.insert_session("session-digest", &session).unwrap(), "no session for a disabled user");
        assert!(store.session("session-digest").unwrap().is_none());
    })
    .await;
}

async fn a_key_display_is_taken_once_under_concurrency(d: Arc<Dispatcher>) {
    blocking(move || {
        let store = Arc::new(WebBoard::new(d));
        let user = store.ensure_user("take-once").unwrap();
        let display = web::KeyDisplay {
            user_id: user.user_id,
            host_id: "box".into(),
            key_digest: "key-digest".into(),
            created_at: 1,
            replaced: false,
        };
        store.insert_key_display("token-digest", &display).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (store, barrier) = (store.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    usize::from(store.take_key_display("token-digest").unwrap().is_some())
                })
            })
            .collect();
        assert_eq!(handles.into_iter().map(|h| h.join().unwrap()).sum::<usize>(), 1);
    })
    .await;
}

fn quick_bridge(addr: std::net::SocketAddr, key: &str, host: &str) -> conde_remote::BridgeClient {
    let mut cfg = conde_remote::BridgeConfig::new(format!("ws://{addr}/v1/bridge"), key, host);
    cfg.backoff_min = Duration::from_millis(50);
    cfg.backoff_max = Duration::from_millis(200);
    conde_remote::BridgeClient::start(cfg, |_| {})
}

async fn wait_connected(c: &conde_remote::BridgeClient) {
    let mut rx = c.subscribe();
    tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|s| *s == conde_remote::BridgeState::Connected))
        .await
        .expect("the bridge connects")
        .unwrap();
}

/// A host's revocation event that concerns an older key leaves the valid connection alone; a
/// real revocation still closes it.
async fn a_stale_host_revocation_leaves_a_valid_bridge_connected(d: Arc<Dispatcher>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key = admin_cli_key(&d, "review", "box").await;
    let server = conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), Default::default()).await.unwrap();
    let client = quick_bridge(server.local_addr(), &key, "box");
    assert!(client.call(json!({"op": "doctor", "request_id": "r"})).await.is_ok(), "hello admitted");
    let mut state = client.subscribe();
    d.store().pending().raise(Wake::Revoked("box".into()));
    let left = tokio::time::timeout(Duration::from_secs(2), state.wait_for(|s| *s != conde_remote::BridgeState::Connected)).await;
    assert!(left.is_err(), "a valid bridge was disconnected: {:?}", client.state());
    assert!(client.call(json!({"op": "doctor", "request_id": "r"})).await.is_ok());

    let (d2, k) = (d.clone(), key.clone());
    blocking(move || {
        let digest = conde_remote::token_digest(&k);
        d2.with_tx(|c| conde::ops::hosts::revoke(c, None, Some(&digest[..12]), "2026-10-09T00:00:00.000Z")).unwrap()
    })
    .await;
    let stopped = tokio::time::timeout(Duration::from_secs(5), client.stopped()).await.expect("revoked bridge closed");
    assert_eq!(stopped, conde_remote::StopReason::Unauthorized);
    server.shutdown().await.unwrap();
}

/// A storage outage is not a revocation: an established bridge stays connected through a
/// recheck that cannot read the store, a new one backs off instead of stopping, and both work
/// once the store answers again (SQLite: the fault is injected on its connection).
#[test]
fn a_storage_outage_never_stops_a_valid_bridge() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    run(false, |d| async move {
        let key = admin_cli_key(&d, "review", "box").await;
        let other = admin_cli_key(&d, "review", "box2").await;
        let config = conde_remote::ServerConfig { poll_interval: Duration::from_secs(30), ..Default::default() };
        let server = conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config).await.unwrap();
        let client = quick_bridge(server.local_addr(), &key, "box");
        // A completed call proves the hello was admitted (Connected is set at the upgrade).
        assert!(client.call(json!({"op": "doctor", "request_id": "r"})).await.is_ok());

        let fault = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (d2, trigger) = (d.clone(), fault.clone());
        blocking(move || {
            d2.sqlite().unwrap().with_connection(|c| c.progress_handler(1, Some(move || trigger.load(Ordering::SeqCst))))
        })
        .await
        .unwrap();
        d.store().pending().raise(Wake::All); // a listener gap: every live credential is rechecked
        let late = quick_bridge(server.local_addr(), &other, "box2");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(client.state(), conde_remote::BridgeState::Connected, "the outage stopped a valid bridge");
        assert!(
            !matches!(late.state(), conde_remote::BridgeState::Stopped(_)),
            "a handshake during the outage stopped for good: {:?}",
            late.state()
        );

        fault.store(false, Ordering::SeqCst);
        wait_connected(&late).await;
        assert!(client.call(json!({"op": "doctor", "request_id": "r"})).await.is_ok());
        assert!(authenticates(&d, &key).await);
        server.shutdown().await.unwrap();
    });
}

/// The pasted-code record's two single-transaction writes on the real stores: an issue keeps
/// another account's pending record and replaces the same account's; a restore yields to a
/// record issued since and fills an empty slot.
async fn a_login_record_is_issued_and_restored_in_one_transaction(d: Arc<Dispatcher>) {
    let store = WebBoard::new(d.clone());
    blocking(move || {
        let record = |user_id: i64, tries: u32| web::CliCode {
            user_id,
            user_name: format!("user-{user_id}"),
            state: "s".repeat(16),
            challenge: "c".repeat(43),
            created_at: 1_000,
            paste: Some(format!("paste-{user_id}-{tries}")),
            tries,
        };
        let key = "login-key";
        let held = |s: &WebBoard| s.take_cli_code(key).unwrap().map(|c| (c.user_id, c.tries));
        let issue = |c: &web::CliCode| store.issue_cli_code(key, c, 300).unwrap();
        assert_eq!(issue(&record(1, 0)), web::CliIssued::Issued);
        assert_eq!(issue(&record(2, 0)), web::CliIssued::Theirs, "another account's live record stays");
        assert_eq!(issue(&web::CliCode { created_at: 1_301, ..record(2, 0) }), web::CliIssued::Expired, "and says when it expired");
        assert_eq!(issue(&record(1, 3)), web::CliIssued::Issued, "the same account's is replaced");
        assert_eq!(held(&store), Some((1, 3)), "one record, the latest");
        // Taken for a guess: a restore fills the empty slot; after a new issue, the new record stays.
        store.restore_cli_code(key, &record(1, 1)).unwrap();
        issue(&record(1, 0));
        store.restore_cli_code(key, &record(1, 2)).unwrap();
        assert_eq!(held(&store), Some((1, 0)), "the record issued since stands");
        store.restore_cli_code(key, &record(1, 4)).unwrap();
        assert_eq!(held(&store), Some((1, 4)), "an empty slot is filled");
    })
    .await;
}

/// A login's first confirm dates it: once that is more than the TTL ago, every issue answers
/// Expired and stores nothing, for either account, also after the purge removed the record; the
/// date itself is purged only a day later.
async fn a_logins_first_confirm_dates_it_past_its_records_purge(d: Arc<Dispatcher>) {
    let store = WebBoard::new(d.clone());
    blocking(move || {
        let record = |user_id: i64, created_at: i64| web::CliCode {
            user_id,
            user_name: format!("user-{user_id}"),
            state: "s".repeat(16),
            challenge: "c".repeat(43),
            created_at,
            paste: Some(format!("paste-{user_id}")),
            tries: 0,
        };
        let limits = web::Limits { session_idle: 3600, session_max: 8 * 3600, login_ttl: 600, cli_code_ttl: 300, key_display_ttl: 300 };
        let key = "dated-login";
        let issue = |c: &web::CliCode| store.issue_cli_code(key, c, 300).unwrap();
        assert_eq!(issue(&record(1, 1_000)), web::CliIssued::Issued);
        assert_eq!(issue(&record(1, 1_200)), web::CliIssued::Issued, "the same account confirms again");
        store.purge_expired(1_301, &limits).unwrap();
        assert_eq!(store.take_cli_code(key).unwrap().map(|c| c.created_at), Some(1_200), "the newer record is live");
        store.purge_expired(1_600, &limits).unwrap();
        assert_eq!(issue(&record(2, 1_600)), web::CliIssued::Expired, "another account, after the purge");
        assert_eq!(issue(&record(1, 1_600)), web::CliIssued::Expired, "and the first account");
        assert!(store.take_cli_code(key).unwrap().is_none(), "nothing stored");
        store.purge_expired(1_000 + 86_401, &limits).unwrap();
        assert_eq!(issue(&record(2, 1_000 + 86_401)), web::CliIssued::Issued, "a day later the key is new again");
    })
    .await;
}

/// The confirm of a login first confirmed more than the TTL ago says it expired, on the real
/// stores: from a session that existed before (no purge), from a fresh sign-in (which purges the
/// expired record) and from the account that confirmed it.
async fn a_late_confirm_is_told_the_login_expired_whatever_the_session(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let alice = Browser::new();
    let run = cli_run();
    paste_leg(&env, &alice, &run).await;
    let (_, alice_page) = paste_confirm_page(&env, &alice, &run).await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-bob".into();
        p.upn = "bob@dakai.example".into();
        p.name = "Bob".into();
    }
    let bob = Browser::new();
    let (status, bob_page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{bob_page}");
    env.advance(301);
    let expired = |(status, body): (StatusCode, String), who: &str| {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{who}: {body}");
        assert!(body.contains("This conde login expired. Run conde login again."), "{who}: {body}");
    };
    expired(paste_confirm(&env, &bob, &run, &csrf_of(&bob_page)).await, "existing session");
    let fresh = Browser::new();
    let (status, page) = paste_confirm_page(&env, &fresh, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    expired(paste_confirm(&env, &fresh, &run, &csrf_of(&page)).await, "fresh sign-in");
    expired(paste_confirm(&env, &alice, &run, &csrf_of(&alice_page)).await, "the confirming account");
    assert_eq!(paste_redeem(&env, &run, "2345-6789").await.1["error"], "authorization_pending", "nothing was issued");
}

/// A wrong guess takes the record; another account confirms the same login in the gap and its
/// record goes in: the restore puts the guessing run's record back and drops the other one.
async fn a_restore_takes_the_login_back_from_another_account(d: Arc<Dispatcher>) {
    let store = WebBoard::new(d.clone());
    blocking(move || {
        let record = |user_id: i64, tries: u32| web::CliCode {
            user_id,
            user_name: format!("user-{user_id}"),
            state: "s".repeat(16),
            challenge: "c".repeat(43),
            created_at: 1_000,
            paste: Some(format!("paste-{user_id}")),
            tries,
        };
        let key = "login-key";
        assert_eq!(store.issue_cli_code(key, &record(1, 0), 300).unwrap(), web::CliIssued::Issued);
        let taken = store.take_cli_code(key).unwrap().expect("the guess takes the record");
        assert_eq!(store.issue_cli_code(key, &record(2, 0), 300).unwrap(), web::CliIssued::Issued, "the empty slot takes the other account's");
        store.restore_cli_code(key, &web::CliCode { tries: 1, ..taken }).unwrap();
        let held = store.take_cli_code(key).unwrap().map(|c| (c.user_id, c.tries, c.paste));
        assert_eq!(held, Some((1, 1, Some("paste-1".into()))), "the first account's record is back");
        assert_eq!(store.take_cli_code(key).unwrap().map(|c| c.user_id), None, "one record");
    })
    .await;
}

/// `end_cli_code` on the real stores: after a last wrong try, a record another account issued in
/// the gap is removed; a record the same account issued since stays; an empty slot stays empty.
async fn a_last_try_ends_the_login_record_unless_its_account_reissued(d: Arc<Dispatcher>) {
    let store = WebBoard::new(d.clone());
    blocking(move || {
        let record = |user_id: i64| web::CliCode {
            user_id,
            user_name: format!("user-{user_id}"),
            state: "s".repeat(16),
            challenge: "c".repeat(43),
            created_at: 1_000,
            paste: Some(format!("paste-{user_id}")),
            tries: 0,
        };
        let key = "ended-login";
        store.issue_cli_code(key, &record(2), 300).unwrap();
        store.end_cli_code(key, 1).unwrap();
        assert!(store.take_cli_code(key).unwrap().is_none(), "another account's record is removed");
        store.issue_cli_code(key, &record(1), 300).unwrap();
        store.end_cli_code(key, 1).unwrap();
        assert_eq!(store.take_cli_code(key).unwrap().map(|c| c.user_id), Some(1), "the same account's stays");
        store.end_cli_code(key, 1).unwrap();
        assert!(store.take_cli_code(key).unwrap().is_none(), "an empty slot stays empty");
    })
    .await;
}
