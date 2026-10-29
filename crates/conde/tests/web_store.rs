//! The web surface over the board's real stores: OIDC sign-in and the invite gate, browser
//! sessions, the keys page, the `conde login` leg, the admin API and disabled users, each case on SQLite and,
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
                pg::skip("the web suite on Postgres");
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
        d.with_tx(|c| {
            conde::ops::hosts::insert_key(
                c,
                &user,
                &host,
                &digest,
                "cli",
                "2026-10-09T00:00:00.000Z",
            )
        })
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
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let Some(d) = rt.block_on(board(on_pg)) else {
        return;
    };
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
    sign_in_stores_the_email_and_the_admin_sets_the_slack_id,
    the_invite_gate_binds_exactly_one_verified_email_match,
    a_generic_issuer_needs_a_verified_email,
    an_entra_sign_in_lands_on_its_migrated_binding_by_oid,
    an_invite_sends_one_email_with_the_steps,
    a_password_invite_has_keycloak_send_the_account_setup_email,
    a_lost_create_answer_resumes_and_a_foreign_account_is_refused,
    a_realm_token_failure_answers_502_with_the_user_created,
    two_racing_invites_record_one_realm_account,
    a_set_up_realm_account_gets_no_new_link,
    disabled_and_signed_in_users_get_no_email,
    accounts_this_invite_did_not_create_are_refused,
    realm_failures_answer_502_with_the_user_created,
    a_microsoft_invite_names_the_microsoft_account,
    an_sso_invite_names_the_company_account,
    a_password_workspace_needs_three_character_names,
    the_realm_record_is_claimed_and_recorded_once,
    the_realm_record_is_cleared_and_claimed_anew,
    a_cleared_user_is_invited_again_with_a_new_realm_account,
    a_slow_sign_in_server_is_cut_off_at_the_deadline,
    the_welcome_names_a_published_agent_of_an_admin,
    a_first_enrollment_is_greeted_once_and_the_admin_agent_told,
    a_keys_page_enrollment_is_greeted,
    a_conde_login_enrollment_is_greeted,
    two_concurrent_first_mints_greet_once,
    an_unshared_board_sends_no_welcome,
    an_sso_invite_opens_its_join_page_for_agents_and_browsers,
    a_password_invite_opens_its_join_page_with_the_password_step_first,
    a_join_link_expires_and_an_unknown_one_says_so,
);

async fn env(d: &Arc<Dispatcher>) -> Env<WebBoard> {
    Env::with(Arc::new(WebBoard::new(d.clone()))).await
}

async fn sign_in_binds_the_identity_and_reuses_it(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("Krisztián Dobmayer"), "{page}");
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    let user = blocking(move || s.user_by_identity(&issuer, "oid-krisztian").unwrap())
        .await
        .unwrap();
    assert_eq!(user.name, "krisztian-dobmayer");
    env.idp.person.lock().unwrap().name = "K. D.".into();
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("K. D."));
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    let again = blocking(move || s.user_by_identity(&issuer, "oid-krisztian").unwrap())
        .await
        .unwrap();
    assert_eq!(again.user_id, user.user_id);

    {
        let mut p = env.idp.person.lock().unwrap();
        p.oid = "oid-stranger".into();
        p.upn = "stranger@dakai.example".into();
    }
    let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body.contains("This account is not invited to this board."),
        "{body}"
    );
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    assert!(
        blocking(move || s.user_by_identity(&issuer, "oid-stranger").unwrap())
            .await
            .is_none()
    );
}

/// The invite gate on the real stores: an identity bound to no user is let in only when its
/// verified email is the email of exactly one user who holds no identity of this issuer.
async fn the_invite_gate_binds_exactly_one_verified_email_match(d: Arc<Dispatcher>) {
    let env = Env::uninvited(Arc::new(WebBoard::new(d.clone())), |_| {}).await;
    let refused = || async {
        let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(
            body.contains("This account is not invited to this board. Ask the board admin."),
            "{body}"
        );
    };
    let issuers = |name: &'static str| {
        let d = d.clone();
        blocking(move || {
            d.with_conn(|t| t.identity_issuers(t.user_id(name)?.unwrap()))
                .unwrap()
        })
    };
    // none: nobody has the address
    refused().await;
    // ambiguous: two users share the address (an invite refuses that, a sign-in's email does not)
    let k = Some(ADMIN_KEY);
    let (status, got) = admin(
        &env,
        "/v1/admin/users",
        Some(json!({"name": "kd", "email": "krisztian.dobmayer@dakai.example"})),
        k,
    )
    .await;
    assert_eq!(
        (status, got["status"].as_str()),
        (StatusCode::OK, Some("invited")),
        "{got}"
    );
    let d2 = d.clone();
    blocking(move || {
        d2.with_tx(|t| {
            let other = t.ensure_user("kd-2")?;
            Ok(t.set_user_email(other, Some("Krisztian.Dobmayer@DAKAI.example"))?)
        })
        .unwrap()
    })
    .await;
    refused().await;
    assert!(issuers("kd").await.is_empty() && issuers("kd-2").await.is_empty());
    // exactly one: bound, and the session starts
    let d2 = d.clone();
    blocking(move || {
        d2.with_tx(
            |t| Ok(t.set_user_email(t.user_id("kd-2")?.unwrap(), Some("kd2@dakai.example"))?),
        )
        .unwrap()
    })
    .await;
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("Krisztián Dobmayer"), "{page}");
    assert_eq!(issuers("kd").await, vec![env.idp.issuer.clone()]);
    // the user holds an identity of this issuer: another one with the address is refused
    env.idp.person.lock().unwrap().oid = "oid-recreated".into();
    refused().await;
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    assert!(
        blocking(move || s.user_by_identity(&issuer, "oid-recreated").unwrap())
            .await
            .is_none()
    );
    let (_, list) = admin(&env, "/v1/admin/users", None, k).await;
    assert_eq!(
        list,
        json!({"users": [
            {"name": "kd", "email": "Krisztian.Dobmayer@dakai.example", "issuers": [env.idp.issuer], "disabled": false,
             "admin": false},
            {"name": "kd-2", "email": "kd2@dakai.example", "issuers": [], "disabled": false, "admin": false},
        ]})
    );
}

/// A user bound as migration 0017 leaves an Entra row, (the tenant's issuer, oid), with no email
/// and no invite: an Entra sign-in lands on it by oid, never by the per-app sub.
async fn an_entra_sign_in_lands_on_its_migrated_binding_by_oid(d: Arc<Dispatcher>) {
    let env = Env::uninvited(Arc::new(WebBoard::new(d.clone())), |_| {}).await;
    let (d2, issuer) = (d.clone(), env.idp.issuer.clone());
    let krisz = blocking(move || {
        d2.with_tx(|t| {
            let id = t.ensure_user("krisz")?;
            Ok(t.bind_identity(id, &issuer, "oid-krisztian", None, None)
                .map(|_| id)?)
        })
        .unwrap()
    })
    .await;
    Browser::new().sign_in(&env).await;
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    let user = blocking(move || s.user_by_identity(&issuer, "oid-krisztian").unwrap())
        .await
        .unwrap();
    assert_eq!((user.user_id, user.name.as_str()), (krisz, "krisz"));
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    assert!(
        blocking(move || s
            .user_by_identity(&issuer, "pairwise-oid-krisztian")
            .unwrap())
        .await
        .is_none()
    );
    let (_, list) = admin(&env, "/v1/admin/users", None, Some(ADMIN_KEY)).await;
    assert_eq!(list["users"].as_array().unwrap().len(), 1, "{list}");
}

/// A Keycloak-style issuer on the real stores: the subject is `sub`, and the email must be marked
/// verified; preferred_username is never matched.
async fn a_generic_issuer_needs_a_verified_email(d: Arc<Dispatcher>) {
    let env = Env::with_config(Arc::new(WebBoard::new(d.clone())), |c| {
        c.entra_tenant = None
    })
    .await;
    {
        let mut p = env.idp.person.lock().unwrap();
        p.entra = false;
        p.upn = "krisztian.dobmayer@dakai.example".into();
        p.email = Some("krisztian.dobmayer@dakai.example".into());
        p.email_verified = Some(false);
    }
    let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    env.idp.person.lock().unwrap().email_verified = Some(true);
    Browser::new().sign_in(&env).await;
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    let user = blocking(move || {
        s.user_by_identity(&issuer, "pairwise-oid-krisztian")
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(user.name, "krisztian-dobmayer");
    // bound: a later unverified email claim is never stored, whatever preferred_username says
    {
        let mut p = env.idp.person.lock().unwrap();
        p.email = Some("attacker@evil.example".into());
        p.email_verified = None;
        p.upn = "attacker@evil.example".into();
    }
    Browser::new().sign_in(&env).await;
    let d2 = d.clone();
    let email = blocking(move || {
        d2.with_conn(|t| t.user_by_name("krisztian-dobmayer"))
            .unwrap()
            .unwrap()
            .email
    })
    .await;
    assert_eq!(email.as_deref(), Some("krisztian.dobmayer@dakai.example"));
}

async fn sessions_expire_and_disabled_users_are_signed_out(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    b.sign_in(&env).await;
    env.advance(8 * 3600 + 1);
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert_eq!(
        (status, loc.as_str()),
        (StatusCode::SEE_OTHER, "/auth/login?next=%2Fkeys")
    );

    b.sign_in(&env).await;
    let key = admin_cli_key(&d, "krisztian-dobmayer", "box").await;
    assert!(authenticates(&d, &key).await);
    let mut events = d.store().pending().events();
    let (status, got) = admin(
        &env,
        "/v1/admin/users/krisztian-dobmayer/disable",
        Some(json!({})),
        Some(ADMIN_KEY),
    )
    .await;
    assert_eq!(
        (status, &got["disconnected"]),
        (StatusCode::OK, &json!(["box"]))
    );
    assert_eq!(next_revoked(&mut events).await, "box");
    assert!(
        !authenticates(&d, &key).await,
        "a disabled user's key does not authenticate"
    );
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert!(
        status == StatusCode::SEE_OTHER && loc.starts_with("/auth/login"),
        "{loc}"
    );
    let (status, _, body) = b.get(&env.url("/keys")).await;
    assert!(
        status == StatusCode::FORBIDDEN && body.contains("disabled"),
        "{body}"
    );

    admin(
        &env,
        "/v1/admin/users/krisztian-dobmayer/enable",
        Some(json!({})),
        Some(ADMIN_KEY),
    )
    .await;
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
    assert_eq!(
        (status, loc.as_str()),
        (StatusCode::SEE_OTHER, "/keys"),
        "the display is single-use"
    );
    let (_, _, list) = b.get(&env.url("/keys")).await;
    assert!(list.contains("<code>work-laptop</code>") && !list.contains(&key));

    // A second key for the host replaces the first and announces the host.
    let mut events = d.store().pending().events();
    let second = key_of(&b.create_key(&env, &csrf, "work laptop").await);
    assert_eq!(next_revoked(&mut events).await, "work-laptop");
    assert!(!authenticates(&d, &key).await && authenticates(&d, &second).await);

    let r = b
        .post_form(&env.url("/keys/work-laptop/revoke"), &[("csrf", &csrf)])
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(next_revoked(&mut events).await, "work-laptop");
    assert!(!authenticates(&d, &second).await);
    let (_, _, list) = b.get(&env.url("/keys")).await;
    assert!(list.contains("revoked "), "{list}");

    // Another user's host is suffixed, never shared.
    admin_cli_key(&d, "bob", "box").await;
    let mine = key_of(&b.create_key(&env, &csrf, "box").await);
    let (d2, digest) = (d.clone(), conde_remote::token_digest(&mine));
    let host = blocking(move || {
        d2.with_conn(|c| c.authenticate(&digest))
            .unwrap()
            .unwrap()
            .1
    })
    .await;
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
        let (status, got) = redeem(
            &env,
            json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "NUC"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(
            (got["host_id"].as_str(), got["reenrolled"].as_bool()),
            (Some("nuc"), Some(again)),
            "{got}"
        );
        assert_eq!(
            got["replaced"].as_bool(),
            Some(again),
            "a live key existed exactly when the host did: {got}"
        );
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
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    let stored = blocking(move || {
        let user = s
            .user_by_identity(&issuer, "oid-krisztian")
            .unwrap()
            .unwrap();
        s.host_keys(user.user_id).unwrap()
    })
    .await;
    let live = stored
        .iter()
        .filter(|k| k.host_id == "nuc" && k.revoked_at.is_none())
        .count();
    assert_eq!(
        (stored.len(), live),
        (3, 1),
        "the history stays, one key is live"
    );
    let (_, _, page) = b.get(&env.url("/keys")).await;
    assert_eq!(page.matches("<code>nuc</code>").count(), 1, "{page}");
    assert!(
        !page.contains("revoked "),
        "the row is the live key: {page}"
    );

    // a host id another user owns gets a suffix: a new host, not a re-enrollment
    admin_cli_key(&d, "bob", "lab").await;
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let (_, got) = redeem(
        &env,
        json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "lab"}),
    )
    .await;
    assert_eq!(
        (got["host_id"].as_str(), got["reenrolled"].as_bool()),
        (Some("lab-2"), Some(false)),
        "{got}"
    );
    assert_eq!(got["replaced"].as_bool(), Some(false), "{got}");

    // a host whose keys were all revoked earlier: re-enrolled, but no live key was replaced
    let (s, issuer) = (env.store.clone(), env.idp.issuer.clone());
    blocking(move || {
        let user = s
            .user_by_identity(&issuer, "oid-krisztian")
            .unwrap()
            .unwrap();
        assert_eq!(s.revoke_host(user.user_id, "nuc", START).unwrap(), 1);
    })
    .await;
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let (_, got) = redeem(
        &env,
        json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "NUC"}),
    )
    .await;
    assert_eq!(
        (
            got["host_id"].as_str(),
            got["reenrolled"].as_bool(),
            got["replaced"].as_bool()
        ),
        (Some("nuc"), Some(true), Some(false)),
        "{got}"
    );
}

async fn pasted_login_codes_count_tries_and_redeem_once(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let b = Browser::new();
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') {
        "3333-3333"
    } else {
        "2222-2222"
    };
    for left in [4, 3] {
        let (status, got) = paste_redeem(&env, &run, wrong).await;
        assert_eq!(
            (status, got["tries_left"].as_u64()),
            (StatusCode::BAD_REQUEST, Some(left)),
            "{got}"
        );
    }
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["host_id"], "head-less");
    assert!(authenticates(&d, got["key"].as_str().unwrap()).await);
    assert_eq!(
        paste_redeem(&env, &run, &shown).await.0,
        StatusCode::BAD_REQUEST
    );

    // the last wrong try ends the login
    let run = cli_run();
    let shown = paste_leg(&env, &b, &run).await;
    let wrong = if shown.starts_with('2') {
        "3333-3333"
    } else {
        "2222-2222"
    };
    for _ in 0..5 {
        assert_eq!(
            paste_redeem(&env, &run, wrong).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (status, got) = paste_redeem(&env, &run, &shown).await;
    assert_eq!(
        (status, got["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("authorization_pending")),
        "{got}"
    );
}

async fn admin_binds_existing_users_and_disables_them(d: Arc<Dispatcher>) {
    let env = Env::uninvited(Arc::new(WebBoard::new(d.clone())), |_| {}).await;
    let k = Some(ADMIN_KEY);
    let key = admin_cli_key(&d, "krisztian-dobmayer", "laptop").await;
    // Sign-in refuses to adopt the user without an email; the admin binds it once (Entra's form).
    let (status, _, body) = Browser::new().get(&env.url("/keys")).await;
    assert!(
        status == StatusCode::FORBIDDEN && body.contains("not invited"),
        "{body}"
    );
    let bind = json!({"tid": TENANT, "oid": "oid-krisztian", "name": "krisztian-dobmayer"});
    let (status, got) = admin(&env, "/v1/admin/users", Some(bind.clone()), k).await;
    assert_eq!(
        (status, got["status"].as_str()),
        (StatusCode::OK, Some("bound"))
    );
    let (_, got) = admin(&env, "/v1/admin/users", Some(bind), k).await;
    assert_eq!(got["status"], "exists");
    let page = Browser::new().sign_in(&env).await;
    assert!(page.contains("<code>laptop</code>"), "{page}");

    let (status, minted) = admin(
        &env,
        "/v1/admin/keys",
        Some(json!({"user": "bob", "host_label": "Build Box"})),
        k,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    let (_, list) = admin(&env, "/v1/admin/keys", None, k).await;
    let ids: HashSet<&str> = list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["key_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(minted["key_id"].as_str().unwrap()) && ids.len() == 2,
        "{list}"
    );
    let mut events = d.store().pending().events();
    let path = format!(
        "/v1/admin/keys/{}/revoke",
        minted["key_id"].as_str().unwrap()
    );
    assert_eq!(
        admin(&env, &path, Some(json!({})), k).await.0,
        StatusCode::OK
    );
    assert_eq!(next_revoked(&mut events).await, "build-box");
    assert!(!authenticates(&d, minted["key"].as_str().unwrap()).await);
    assert!(authenticates(&d, &key).await);
}

/// Sign-in stores the verified email (preferred_username, the UPN; the `email` claim only when the
/// UPN is not an address and the claim is marked verified) and asks the Slack
/// worker to look its member id up; the user gets a human name; the admin key alone sets the
/// member id, which a lookup never replaces.
async fn sign_in_stores_the_email_and_the_admin_sets_the_slack_id(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let user = |d: &Arc<Dispatcher>| {
        let d = d.clone();
        blocking(move || {
            d.with_conn(|t| t.user_slack(t.user_id("krisztian-dobmayer")?.unwrap()))
                .unwrap()
                .unwrap()
        })
    };
    Browser::new().sign_in(&env).await;
    let u = user(&d).await;
    assert_eq!(
        u.email.as_deref(),
        Some("Krisztian.Dobmayer@dakai.example"),
        "no email claim: preferred_username"
    );
    assert!(d.take_slack_lookups().contains(&u.user_id));
    // the email claim is mutable and unverified: the UPN wins while it is an address
    env.idp.person.lock().unwrap().email = Some("someone-else@dakai.io".into());
    Browser::new().sign_in(&env).await;
    assert_eq!(
        user(&d).await.email.as_deref(),
        Some("Krisztian.Dobmayer@dakai.example"),
        "the UPN, not the email claim"
    );
    assert!(
        d.take_slack_lookups().contains(&u.user_id),
        "every sign-in asks again"
    );
    env.idp.person.lock().unwrap().upn = "krisztian".into();
    Browser::new().sign_in(&env).await;
    assert_eq!(
        user(&d).await.email.as_deref(),
        Some("Krisztian.Dobmayer@dakai.example"),
        "a UPN without an @: the unverified email claim is not stored, the address stays"
    );
    env.idp.person.lock().unwrap().email_verified = Some(true);
    Browser::new().sign_in(&env).await;
    assert_eq!(
        user(&d).await.email.as_deref(),
        Some("someone-else@dakai.io"),
        "a verified email claim is stored"
    );
    let d2 = d.clone();
    let humans = blocking(move || d2.with_conn(|t| t.owner_agents(u.user_id)).unwrap()).await;
    assert_eq!(
        humans
            .iter()
            .filter(|a| a.is_human())
            .map(|a| a.canonical())
            .collect::<Vec<_>>(),
        ["krisztian-dobmayer/human"]
    );

    let path = "/v1/admin/users/krisztian-dobmayer/slack";
    assert_eq!(
        admin(&env, path, Some(json!({"member_id": "U0ADMIN"})), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, got) = admin(
        &env,
        path,
        Some(json!({"member_id": "not-an-id"})),
        Some(ADMIN_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    let (status, _) = admin(
        &env,
        "/v1/admin/users/nobody/slack",
        Some(json!({"member_id": "U0ADMIN"})),
        Some(ADMIN_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(user(&d).await.slack_member_id, None);
    let (status, got) = admin(
        &env,
        path,
        Some(json!({"member_id": "U0ADMIN"})),
        Some(ADMIN_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!({"user": "krisztian-dobmayer", "slack_member_id": "U0ADMIN", "slack_source": "admin"})
    );
    let u = user(&d).await;
    assert_eq!(
        (u.slack_member_id.as_deref(), u.slack_source.as_deref()),
        (Some("U0ADMIN"), Some("admin"))
    );
}

/// Two server replicas on one Postgres board: a key revoked (and a user disabled) through one
/// replica's admin API closes the live bridge connected to the other, by LISTEN/NOTIFY.
#[test]
fn revocation_closes_a_bridge_on_another_replica() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let Some(url) = rt.block_on(blocking(pg::fresh_database)) else {
        pg::skip("the two-replica revocation test");
        return;
    };
    let replica = |url: String| {
        rt.block_on(async move {
            let d = Arc::new(
                blocking(move || {
                    Dispatcher::connect(&url, conde::store::postgres::PgOpen::SERVER).unwrap()
                })
                .await,
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
            let handle = conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config)
                .await
                .unwrap();
            (d, handle.local_addr(), handle)
        })
    };
    let (d1, one, h1) = replica(url.clone());
    let (d2, two, h2) = replica(url);
    rt.block_on(async {
        let http = reqwest::Client::new();
        let call = |path: String, body: Value| {
            http.post(format!("http://{one}{path}"))
                .bearer_auth(ADMIN_KEY)
                .json(&body)
                .send()
        };
        let connect = |key: &str| {
            let mut c =
                conde_remote::BridgeConfig::new(format!("ws://{two}/v1/bridge"), key, "box");
            c.backoff_min = Duration::from_millis(50);
            c.backoff_max = Duration::from_millis(100);
            conde_remote::BridgeClient::start(c, |_| {})
        };
        let connected = |c: &conde_remote::BridgeClient| {
            let mut rx = c.subscribe();
            async move {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    rx.wait_for(|s| *s == conde_remote::BridgeState::Connected),
                )
                .await
                .expect("the bridge connects")
                .unwrap();
            }
        };
        let stopped = |c: &conde_remote::BridgeClient| {
            let c = c.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(10), c.stopped())
                    .await
                    .expect("closed by the other replica")
            }
        };

        let minted: Value = call(
            "/v1/admin/keys".into(),
            json!({"user": "bob", "host_label": "box"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let client = connect(minted["key"].as_str().unwrap());
        connected(&client).await;
        assert!(
            client
                .call(json!({"op": "doctor", "request_id": "r"}))
                .await
                .is_ok(),
            "the bridge is live on replica two"
        );
        let path = format!(
            "/v1/admin/keys/{}/revoke",
            minted["key_id"].as_str().unwrap()
        );
        assert!(call(path, json!({})).await.unwrap().status().is_success());
        assert_eq!(
            stopped(&client).await,
            conde_remote::StopReason::Unauthorized
        );

        let minted: Value = call(
            "/v1/admin/keys".into(),
            json!({"user": "bob", "host_label": "box"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let client = connect(minted["key"].as_str().unwrap());
        connected(&client).await;
        assert!(
            call("/v1/admin/users/bob/disable".into(), json!({}))
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert_eq!(
            stopped(&client).await,
            conde_remote::StopReason::Unauthorized
        );
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
    assert_eq!(
        admin(
            &env,
            "/v1/admin/users/krisztian-dobmayer/disable",
            Some(json!({})),
            k
        )
        .await
        .0,
        StatusCode::OK
    );
    // No browser request while disabled: the disable itself ends the session.
    assert_eq!(
        admin(
            &env,
            "/v1/admin/users/krisztian-dobmayer/enable",
            Some(json!({})),
            k
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, loc) = b.peek(&env.url("/keys")).await;
    assert!(
        status == StatusCode::SEE_OTHER && loc.starts_with("/auth/login"),
        "{status} {loc}"
    );
}

async fn a_mint_racing_a_disable_is_refused(d: Arc<Dispatcher>) {
    let store = Arc::new(WebBoard::new(d));
    blocking(move || {
        let user = store.ensure_user("race").unwrap();
        assert!(!store.user_disabled(user.user_id).unwrap()); // an earlier read saw the user enabled
        assert!(store.set_disabled("race", true).unwrap().is_some()); // another request commits
        let digest = conde_remote::token_digest("race-key");
        let result = store
            .mint_host_key(user.user_id, "late-host", "test", &digest, 1)
            .unwrap();
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
        assert!(
            !store.insert_session("session-digest", &session).unwrap(),
            "no session for a disabled user"
        );
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
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .sum::<usize>(),
            1
        );
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
    tokio::time::timeout(
        Duration::from_secs(10),
        rx.wait_for(|s| *s == conde_remote::BridgeState::Connected),
    )
    .await
    .expect("the bridge connects")
    .unwrap();
}

/// A host's revocation event that concerns an older key leaves the valid connection alone; a
/// real revocation still closes it.
async fn a_stale_host_revocation_leaves_a_valid_bridge_connected(d: Arc<Dispatcher>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key = admin_cli_key(&d, "review", "box").await;
    let server = conde::cloud::start(
        d.clone(),
        "127.0.0.1:0".parse().unwrap(),
        Default::default(),
    )
    .await
    .unwrap();
    let client = quick_bridge(server.local_addr(), &key, "box");
    assert!(
        client
            .call(json!({"op": "doctor", "request_id": "r"}))
            .await
            .is_ok(),
        "hello admitted"
    );
    let mut state = client.subscribe();
    d.store().pending().raise(Wake::Revoked("box".into()));
    let left = tokio::time::timeout(
        Duration::from_secs(2),
        state.wait_for(|s| *s != conde_remote::BridgeState::Connected),
    )
    .await;
    assert!(
        left.is_err(),
        "a valid bridge was disconnected: {:?}",
        client.state()
    );
    assert!(
        client
            .call(json!({"op": "doctor", "request_id": "r"}))
            .await
            .is_ok()
    );

    let (d2, k) = (d.clone(), key.clone());
    blocking(move || {
        let digest = conde_remote::token_digest(&k);
        d2.with_tx(|c| {
            conde::ops::hosts::revoke(c, None, Some(&digest[..12]), "2026-10-09T00:00:00.000Z")
        })
        .unwrap()
    })
    .await;
    let stopped = tokio::time::timeout(Duration::from_secs(5), client.stopped())
        .await
        .expect("revoked bridge closed");
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
        let config = conde_remote::ServerConfig {
            poll_interval: Duration::from_secs(30),
            ..Default::default()
        };
        let server = conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        let client = quick_bridge(server.local_addr(), &key, "box");
        // A completed call proves the hello was admitted (Connected is set at the upgrade).
        assert!(
            client
                .call(json!({"op": "doctor", "request_id": "r"}))
                .await
                .is_ok()
        );

        let fault = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (d2, trigger) = (d.clone(), fault.clone());
        blocking(move || {
            d2.sqlite().unwrap().with_connection(|c| {
                c.progress_handler(1, Some(move || trigger.load(Ordering::SeqCst)))
            })
        })
        .await
        .unwrap();
        d.store().pending().raise(Wake::All); // a listener gap: every live credential is rechecked
        let late = quick_bridge(server.local_addr(), &other, "box2");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            client.state(),
            conde_remote::BridgeState::Connected,
            "the outage stopped a valid bridge"
        );
        assert!(
            !matches!(late.state(), conde_remote::BridgeState::Stopped(_)),
            "a handshake during the outage stopped for good: {:?}",
            late.state()
        );

        fault.store(false, Ordering::SeqCst);
        wait_connected(&late).await;
        assert!(
            client
                .call(json!({"op": "doctor", "request_id": "r"}))
                .await
                .is_ok()
        );
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
        assert_eq!(
            issue(&record(2, 0)),
            web::CliIssued::Theirs,
            "another account's live record stays"
        );
        assert_eq!(
            issue(&web::CliCode {
                created_at: 1_301,
                ..record(2, 0)
            }),
            web::CliIssued::Expired,
            "and says when it expired"
        );
        assert_eq!(
            issue(&record(1, 3)),
            web::CliIssued::Issued,
            "the same account's is replaced"
        );
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
        let limits = web::Limits {
            session_idle: 3600,
            session_max: 8 * 3600,
            login_ttl: 600,
            cli_code_ttl: 300,
            key_display_ttl: 300,
        };
        let key = "dated-login";
        let issue = |c: &web::CliCode| store.issue_cli_code(key, c, 300).unwrap();
        assert_eq!(issue(&record(1, 1_000)), web::CliIssued::Issued);
        assert_eq!(
            issue(&record(1, 1_200)),
            web::CliIssued::Issued,
            "the same account confirms again"
        );
        store.purge_expired(1_301, &limits).unwrap();
        assert_eq!(
            store.take_cli_code(key).unwrap().map(|c| c.created_at),
            Some(1_200),
            "the newer record is live"
        );
        store.purge_expired(1_600, &limits).unwrap();
        assert_eq!(
            issue(&record(2, 1_600)),
            web::CliIssued::Expired,
            "another account, after the purge"
        );
        assert_eq!(
            issue(&record(1, 1_600)),
            web::CliIssued::Expired,
            "and the first account"
        );
        assert!(
            store.take_cli_code(key).unwrap().is_none(),
            "nothing stored"
        );
        store.purge_expired(1_000 + 86_401, &limits).unwrap();
        assert_eq!(
            issue(&record(2, 1_000 + 86_401)),
            web::CliIssued::Issued,
            "a day later the key is new again"
        );
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
    env.invite("bob", "bob@dakai.example").await;
    let (status, bob_page) = paste_confirm_page(&env, &bob, &run).await;
    assert_eq!(status, StatusCode::OK, "{bob_page}");
    env.advance(301);
    let expired = |(status, body): (StatusCode, String), who: &str| {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{who}: {body}");
        assert!(
            body.contains("This conde login expired. Run conde login again."),
            "{who}: {body}"
        );
    };
    expired(
        paste_confirm(&env, &bob, &run, &csrf_of(&bob_page)).await,
        "existing session",
    );
    let fresh = Browser::new();
    let (status, page) = paste_confirm_page(&env, &fresh, &run).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    expired(
        paste_confirm(&env, &fresh, &run, &csrf_of(&page)).await,
        "fresh sign-in",
    );
    expired(
        paste_confirm(&env, &alice, &run, &csrf_of(&alice_page)).await,
        "the confirming account",
    );
    assert_eq!(
        paste_redeem(&env, &run, "2345-6789").await.1["error"],
        "authorization_pending",
        "nothing was issued"
    );
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
        assert_eq!(
            store.issue_cli_code(key, &record(1, 0), 300).unwrap(),
            web::CliIssued::Issued
        );
        let taken = store
            .take_cli_code(key)
            .unwrap()
            .expect("the guess takes the record");
        assert_eq!(
            store.issue_cli_code(key, &record(2, 0), 300).unwrap(),
            web::CliIssued::Issued,
            "the empty slot takes the other account's"
        );
        store
            .restore_cli_code(key, &web::CliCode { tries: 1, ..taken })
            .unwrap();
        let held = store
            .take_cli_code(key)
            .unwrap()
            .map(|c| (c.user_id, c.tries, c.paste));
        assert_eq!(
            held,
            Some((1, 1, Some("paste-1".into()))),
            "the first account's record is back"
        );
        assert_eq!(
            store.take_cli_code(key).unwrap().map(|c| c.user_id),
            None,
            "one record"
        );
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
        assert!(
            store.take_cli_code(key).unwrap().is_none(),
            "another account's record is removed"
        );
        store.issue_cli_code(key, &record(1), 300).unwrap();
        store.end_cli_code(key, 1).unwrap();
        assert_eq!(
            store.take_cli_code(key).unwrap().map(|c| c.user_id),
            Some(1),
            "the same account's stays"
        );
        store.end_cli_code(key, 1).unwrap();
        assert!(
            store.take_cli_code(key).unwrap().is_none(),
            "an empty slot stays empty"
        );
    })
    .await;
}

// --- the invite email ----------------------------------------------------------------------

use conde_remote::web::invite::{DEFAULT_FROM, INVITE_ATTRIBUTE};
use conde_remote::web::{InviteConfig, KeycloakAdmin, Resend, SignIn};

/// A Resend stand-in: records each request (Authorization, body) and answers `status`.
#[derive(Default)]
struct ResendStub {
    seen: Mutex<Vec<(String, Value)>>,
    /// The Idempotency-Key of each request.
    keys: Mutex<Vec<Option<String>>>,
    status: AtomicUsize,
}

async fn resend_emails(
    State(stub): State<Arc<ResendStub>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let auth = headers[header::AUTHORIZATION].to_str().unwrap().to_string();
    let key = headers
        .get("idempotency-key")
        .map(|v| v.to_str().unwrap().to_string());
    stub.keys.lock().unwrap().push(key);
    stub.seen.lock().unwrap().push((auth, body));
    let status = StatusCode::from_u16(stub.status.load(Ordering::SeqCst) as u16).unwrap();
    (
        status,
        axum::Json(json!({"id": "re-1", "message": "stub answer"})),
    )
        .into_response()
}

async fn start_resend() -> (String, Arc<ResendStub>) {
    let stub = Arc::new(ResendStub::default());
    stub.status.store(200, Ordering::SeqCst);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/emails", post(resend_emails))
        .with_state(stub.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, stub)
}

/// A Keycloak admin API stand-in for realm `demo`.
#[derive(Default)]
struct KcStub {
    accounts: Mutex<Vec<Value>>,
    /// The next create stores the account and answers 502, as if its answer was lost.
    lose_next_create: std::sync::atomic::AtomicBool,
    token_fails: std::sync::atomic::AtomicBool,
    tokens: Mutex<Vec<HashMap<String, String>>>,
    /// The (client_id, redirect_uri) of each account-setup email.
    redirects: Mutex<Vec<(String, String)>>,
    creates: Mutex<Vec<Value>>,
    /// (account id, lifespan, body) of each execute-actions-email.
    actions: Mutex<Vec<(String, String, Value)>>,
    /// The token answer waits this long (milliseconds), so concurrent invites overlap.
    token_delay_ms: AtomicUsize,
    /// When set, the create answers this status instead.
    create_status: AtomicUsize,
    /// When set, execute-actions-email answers this status instead of 204.
    actions_status: AtomicUsize,
    /// The search returns every account, whatever its email.
    search_ignores_email: std::sync::atomic::AtomicBool,
}

const KC_TOKEN: &str = "kc-admin-token";
const KC_SECRET: &str = "kc-client-secret";

fn kc_authorized(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .is_some_and(|v| v == format!("Bearer {KC_TOKEN}").as_str())
}

async fn kc_token(
    State(kc): State<Arc<KcStub>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    kc.tokens.lock().unwrap().push(form.clone());
    let delay = kc.token_delay_ms.load(Ordering::SeqCst) as u64;
    tokio::time::sleep(Duration::from_millis(delay)).await;
    if kc.token_fails.load(Ordering::SeqCst)
        || form.get("client_secret").map(String::as_str) != Some(KC_SECRET)
    {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "unauthorized_client"})),
        )
            .into_response();
    }
    axum::Json(json!({"access_token": KC_TOKEN, "token_type": "Bearer"})).into_response()
}

async fn kc_create(
    State(kc): State<Arc<KcStub>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !kc_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    kc.creates.lock().unwrap().push(body.clone());
    if let status @ 1.. = kc.create_status.load(Ordering::SeqCst) {
        let status = StatusCode::from_u16(status as u16).unwrap();
        return (
            status,
            axum::Json(json!({"errorMessage": "invalidEmailMessage"})),
        )
            .into_response();
    }
    let mut accounts = kc.accounts.lock().unwrap();
    let taken = accounts.iter().any(|a| {
        a["username"] == body["username"]
            || a["email"]
                .as_str()
                .unwrap()
                .eq_ignore_ascii_case(body["email"].as_str().unwrap())
    });
    if taken {
        return (
            StatusCode::CONFLICT,
            axum::Json(json!({"errorMessage": "User exists with same email"})),
        )
            .into_response();
    }
    let id = format!("kc-{}", accounts.len() + 1);
    let mut account = body.clone();
    account["id"] = json!(id);
    account["email"] = json!(body["email"].as_str().unwrap().to_lowercase());
    accounts.push(account);
    if kc.lose_next_create.swap(false, Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    (
        StatusCode::CREATED,
        [(
            header::LOCATION,
            format!("http://kc/admin/realms/demo/users/{id}"),
        )],
    )
        .into_response()
}

async fn kc_search(
    State(kc): State<Arc<KcStub>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !kc_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    assert_eq!(q.get("exact").map(String::as_str), Some("true"));
    let email = q["email"].to_lowercase();
    let found: Vec<Value> = kc
        .accounts
        .lock()
        .unwrap()
        .iter()
        .filter(|a| kc.search_ignores_email.load(Ordering::SeqCst) || a["email"] == email.as_str())
        .cloned()
        .collect();
    axum::Json(found).into_response()
}

async fn kc_user(
    State(kc): State<Arc<KcStub>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if !kc_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match kc
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id.as_str())
    {
        Some(a) => axum::Json(a.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "User not found"})),
        )
            .into_response(),
    }
}

async fn kc_actions(
    State(kc): State<Arc<KcStub>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(q): Query<HashMap<String, String>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !kc_authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let status @ 1.. = kc.actions_status.load(Ordering::SeqCst) {
        return (
            StatusCode::from_u16(status as u16).unwrap(),
            axum::Json(json!({"errorMessage": "smtp down"})),
        )
            .into_response();
    }
    let param = |k: &str| q.get(k).cloned().unwrap_or_default();
    kc.redirects
        .lock()
        .unwrap()
        .push((param("client_id"), param("redirect_uri")));
    kc.actions
        .lock()
        .unwrap()
        .push((id, param("lifespan"), body));
    StatusCode::NO_CONTENT.into_response()
}

async fn start_keycloak() -> (String, Arc<KcStub>) {
    let kc = Arc::new(KcStub::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/realms/demo/protocol/openid-connect/token", post(kc_token))
        .route("/admin/realms/demo/users", post(kc_create).get(kc_search))
        .route("/admin/realms/demo/users/{id}", get(kc_user))
        .route(
            "/admin/realms/demo/users/{id}/execute-actions-email",
            axum::routing::put(kc_actions),
        )
        .with_state(kc.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, kc)
}

async fn mail_env(d: &Arc<Dispatcher>, invite: InviteConfig) -> Env<WebBoard> {
    Env::uninvited(Arc::new(WebBoard::new(d.clone())), move |c| {
        c.invite = invite
    })
    .await
}

async fn invite_req(env: &Env<WebBoard>, body: Value) -> (StatusCode, Value) {
    admin(env, "/v1/admin/users", Some(body), Some(ADMIN_KEY)).await
}

/// The stored (realm account id, invite correlation value) of `name`.
async fn realm_record(d: &Arc<Dispatcher>, name: &str) -> (Option<String>, Option<String>) {
    let (d, name) = (d.clone(), name.to_string());
    blocking(move || {
        d.with_conn(|t| t.user_keycloak(t.user_id(&name)?.unwrap()))
            .unwrap()
    })
    .await
}

/// A Google workspace: the invite sends one email through Resend with the line for the person's
/// agent, the invite's join page (the `join` of the answer); a failed
/// send answers 502 with the user created; a user who signed in already gets nothing; without
/// mail the invite says so.
async fn an_invite_sends_one_email_with_the_steps(d: Arc<Dispatcher>) {
    let (resend_url, resend) = start_resend().await;
    let env = mail_env(
        &d,
        InviteConfig {
            sign_in: Some(SignIn::Google),
            workspace: "clearscale".into(),
            account: String::new(),
            resend: Some(Resend {
                api_key: "re_test_key".into(),
                from: DEFAULT_FROM.into(),
                base_url: resend_url,
            }),
            keycloak: None,
            deadline: None,
        },
    )
    .await;
    let (status, got) = invite_req(
        &env,
        json!({"name": "ann", "email": "ann@clearscale.example", "inviter": "Krisz"}),
    )
    .await;
    assert_eq!(
        (status, got["mail"].as_str(), got["status"].as_str()),
        (StatusCode::OK, Some("sent"), Some("invited")),
        "{got}"
    );
    let join = got["join"].as_str().unwrap().to_string();
    assert!(join.starts_with(&env.url("/join/")), "{join}");
    {
        let seen = resend.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (auth, mail) = &seen[0];
        assert_eq!(auth, "Bearer re_test_key");
        assert_eq!(mail["from"], DEFAULT_FROM);
        assert_eq!(mail["to"], json!(["ann@clearscale.example"]));
        assert_eq!(mail["subject"], "Krisz invited you to clearscale on conde");
        let text = mail["text"].as_str().unwrap();
        let line = format!("Read {join} and follow it.");
        for part in [
            "with your Google account ann@clearscale.example",
            &line,
            "https://conde.cloud/docs/setup",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        let html = mail["html"].as_str().unwrap();
        assert!(html.contains(&format!("Read <a href=\"{join}\"")), "{html}");
        assert!(html.contains("with your Google account"), "{html}");
    }

    resend.status.store(500, Ordering::SeqCst);
    let (status, got) = invite_req(
        &env,
        json!({"name": "bob", "email": "bob@clearscale.example"}),
    )
    .await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("failed")),
        "{got}"
    );
    let message = got["message"].as_str().unwrap();
    assert!(
        message.contains("user bob exists on the board") && message.contains("500"),
        "{message}"
    );
    assert!(!message.contains("re_test_key"), "{message}");
    let users = {
        let s = env.store.clone();
        blocking(move || s.list_users().unwrap()).await
    };
    assert!(
        users.iter().any(|u| u.name == "bob"),
        "the user was created"
    );

    // the provider's person signs in; a re-invite then sends nothing
    resend.status.store(200, Ordering::SeqCst);
    let (status, _) = invite_req(&env, json!({"name": INVITED.0, "email": INVITED.1})).await;
    assert_eq!(status, StatusCode::OK);
    Browser::new().sign_in(&env).await;
    let before = resend.seen.lock().unwrap().len();
    let (status, got) = invite_req(&env, json!({"name": INVITED.0, "email": INVITED.1})).await;
    assert_eq!(
        (status, got["status"].as_str(), got["mail"].as_str()),
        (StatusCode::OK, Some("exists"), Some("none")),
        "{got}"
    );
    assert_eq!(resend.seen.lock().unwrap().len(), before);

    let bare = mail_env(
        &d,
        InviteConfig {
            sign_in: Some(SignIn::Google),
            ..InviteConfig::default()
        },
    )
    .await;
    let (status, got) = invite_req(
        &bare,
        json!({"name": "cy", "email": "cy@clearscale.example"}),
    )
    .await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("unconfigured")),
        "{got}"
    );
}

fn demo_invite(kc_url: &str) -> InviteConfig {
    InviteConfig {
        sign_in: Some(SignIn::Password),
        workspace: "demo".into(),
        account: String::new(),
        resend: None,
        keycloak: Some(KeycloakAdmin {
            url: kc_url.to_string(),
            realm: "demo".into(),
            client_id: "conde-admin".into(),
            client_secret: KC_SECRET.into(),
        }),
        deadline: None,
    }
}

/// A password workspace: the realm account is created (unverified, with the two required actions
/// and the invite's correlation value) and Keycloak sends its account-setup email; the id is
/// recorded, a re-invite uses it alone, and one whose email changed is refused.
async fn a_password_invite_has_keycloak_send_the_account_setup_email(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "Ann@Demo.example"})).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    let token = kc.tokens.lock().unwrap()[0].clone();
    assert_eq!(
        (token["grant_type"].as_str(), token["client_id"].as_str()),
        ("client_credentials", "conde-admin")
    );
    let create = kc.creates.lock().unwrap()[0].clone();
    assert_eq!(create["username"], "ann");
    assert_eq!(create["email"], "Ann@Demo.example");
    assert_eq!(
        (
            create["enabled"].as_bool(),
            create["emailVerified"].as_bool()
        ),
        (Some(true), Some(false))
    );
    assert_eq!(
        create["requiredActions"],
        json!(["VERIFY_EMAIL", "UPDATE_PASSWORD"])
    );
    assert!(
        create.get("credentials").is_none(),
        "conde never sets a password"
    );
    let mark = create["attributes"][INVITE_ATTRIBUTE][0]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        mark.len() == 32 && mark.chars().all(|c| c.is_ascii_hexdigit()),
        "{mark}"
    );
    assert_eq!(
        *kc.actions.lock().unwrap(),
        [(
            "kc-1".to_string(),
            "604800".to_string(),
            json!(["VERIFY_EMAIL", "UPDATE_PASSWORD"])
        )]
    );
    assert_eq!(
        realm_record(&d, "ann").await,
        (Some("kc-1".into()), Some(mark))
    );

    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "Ann@Demo.example"})).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    assert_eq!(
        kc.creates.lock().unwrap().len(),
        1,
        "the recorded account is reused"
    );
    assert_eq!(kc.actions.lock().unwrap().len(), 2);

    kc.accounts.lock().unwrap()[0]["email"] = json!("someone@else.example");
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "Ann@Demo.example"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{got}");
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("the recorded sign-in account no longer matches"),
        "{got}"
    );
    assert_eq!(kc.actions.lock().unwrap().len(), 2);
}

/// A create whose answer was lost: the retry's 409 lookup finds the account carrying this
/// invite's value, records it and has the email sent once. An account the invite did not
/// create (no value) is refused with 409 and no email.
async fn a_lost_create_answer_resumes_and_a_foreign_account_is_refused(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    kc.lose_next_create.store(true, Ordering::SeqCst);
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{got}");
    let (id, mark) = realm_record(&d, "ann").await;
    assert_eq!(id, None);
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    assert_eq!(kc.creates.lock().unwrap().len(), 2);
    assert_eq!(
        kc.actions
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.0.as_str())
            .collect::<Vec<_>>(),
        ["kc-1"]
    );
    assert_eq!(realm_record(&d, "ann").await, (Some("kc-1".into()), mark));

    kc.accounts.lock().unwrap().push(
        json!({"id": "kc-eve", "username": "eve", "email": "eve@demo.example", "enabled": true}),
    );
    let (status, got) = invite_req(&env, json!({"name": "eve", "email": "eve@demo.example"})).await;
    assert_eq!(
        (status, got["error"].as_str()),
        (StatusCode::CONFLICT, Some("conflict")),
        "{got}"
    );
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("already has an account with this email that this invitation did not create"),
        "{got}"
    );
    assert_eq!(
        kc.actions.lock().unwrap().len(),
        1,
        "no email for a foreign account"
    );
    assert_eq!(realm_record(&d, "eve").await.0, None);
}

/// A token refusal is a 502 naming the step; the user exists and nothing reached the realm.
async fn a_realm_token_failure_answers_502_with_the_user_created(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    kc.token_fails.store(true, Ordering::SeqCst);
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(
        (status, got["user"].as_str(), got["mail"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("ann"), Some("failed")),
        "{got}"
    );
    let message = got["message"].as_str().unwrap();
    assert!(
        message.contains("the admin token request was refused (401"),
        "{message}"
    );
    assert!(!message.contains(KC_SECRET), "{message}");
    assert!(kc.creates.lock().unwrap().is_empty() && kc.actions.lock().unwrap().is_empty());
}

/// A user invited on the board whose realm invite already holds the correlation `value`.
async fn preclaimed(env: &Env<WebBoard>, name: &str, email: &str, value: &str) -> i64 {
    let (s, name, email, value) = (
        env.store.clone(),
        name.to_string(),
        email.to_string(),
        value.to_string(),
    );
    blocking(move || {
        let web::Invited::Created(u) = s.invite_user(&name, &email).unwrap() else {
            panic!("a new user")
        };
        let stored = s.claim_realm_invite(u.user_id, &value).unwrap();
        assert_eq!(stored.invite.as_deref(), Some(value.as_str()));
        u.user_id
    })
    .await
}

fn google_invite(resend_url: String, sign_in: SignIn) -> InviteConfig {
    InviteConfig {
        sign_in: Some(sign_in),
        workspace: "clearscale".into(),
        resend: Some(Resend {
            api_key: "re_test_key".into(),
            from: DEFAULT_FROM.into(),
            base_url: resend_url,
        }),
        ..InviteConfig::default()
    }
}

/// Two invites of one user at once: one correlation value wins, one realm account is created and
/// recorded, and both answers agree.
async fn two_racing_invites_record_one_realm_account(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    kc.token_delay_ms.store(300, Ordering::SeqCst);
    let body = json!({"name": "ann", "email": "ann@demo.example"});
    let (a, b) = tokio::join!(
        invite_req(&env, body.clone()),
        invite_req(&env, body.clone())
    );
    assert_eq!(
        (a.0, b.0),
        (StatusCode::OK, StatusCode::OK),
        "{} {}",
        a.1,
        b.1
    );
    assert_eq!(kc.accounts.lock().unwrap().len(), 1);
    let (id, mark) = realm_record(&d, "ann").await;
    assert_eq!(id.as_deref(), Some("kc-1"));
    let created = kc.creates.lock().unwrap().clone();
    assert!(
        created
            .iter()
            .all(|c| c["attributes"][INVITE_ATTRIBUTE][0].as_str() == mark.as_deref()),
        "{created:?}"
    );
    kc.token_delay_ms.store(0, Ordering::SeqCst);
    let (status, got) = invite_req(&env, body).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    assert_eq!(realm_record(&d, "ann").await.0.as_deref(), Some("kc-1"));
}

/// A re-invite asks only for the actions the account still has pending; an account that set its
/// password and verified its address gets nothing.
async fn a_set_up_realm_account_gets_no_new_link(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    let body = json!({"name": "ann", "email": "ann@demo.example"});
    assert_eq!(invite_req(&env, body.clone()).await.0, StatusCode::OK);
    kc.accounts.lock().unwrap()[0]["requiredActions"] = json!(["UPDATE_PASSWORD"]);
    let (status, got) = invite_req(&env, body.clone()).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    assert_eq!(kc.actions.lock().unwrap()[1].2, json!(["UPDATE_PASSWORD"]));
    kc.accounts.lock().unwrap()[0]["requiredActions"] = json!([]);
    let (status, got) = invite_req(&env, body).await;
    assert_eq!(
        (status, got["mail"].as_str(), got["reason"].as_str()),
        (StatusCode::OK, Some("none"), Some("set up")),
        "{got}"
    );
    assert_eq!(
        kc.actions.lock().unwrap().len(),
        2,
        "no link to an account that is set up"
    );
}

/// No email for a disabled user (either path) or for a user already bound to the issuer, whose
/// answer carries the real outcome.
async fn disabled_and_signed_in_users_get_no_email(d: Arc<Dispatcher>) {
    let k = Some(ADMIN_KEY);
    let (resend_url, resend) = start_resend().await;
    let google = mail_env(&d, google_invite(resend_url, SignIn::Google)).await;
    let ann = json!({"name": "ann", "email": "ann@clearscale.example"});
    assert_eq!(invite_req(&google, ann.clone()).await.0, StatusCode::OK);
    assert_eq!(
        admin(&google, "/v1/admin/users/ann/disable", Some(json!({})), k)
            .await
            .0,
        StatusCode::OK
    );
    let (status, got) = invite_req(&google, ann).await;
    assert_eq!(
        (status, got["mail"].as_str(), got["reason"].as_str()),
        (StatusCode::OK, Some("none"), Some("disabled")),
        "{got}"
    );
    assert_eq!(resend.seen.lock().unwrap().len(), 1);

    let (kc_url, kc) = start_keycloak().await;
    let demo = mail_env(&d, demo_invite(&kc_url)).await;
    let bob = json!({"name": "bob", "email": "bob@demo.example"});
    assert_eq!(invite_req(&demo, bob.clone()).await.0, StatusCode::OK);
    assert_eq!(
        admin(&demo, "/v1/admin/users/bob/disable", Some(json!({})), k)
            .await
            .0,
        StatusCode::OK
    );
    let tokens = kc.tokens.lock().unwrap().len();
    let (status, got) = invite_req(&demo, bob).await;
    assert_eq!(
        (status, got["reason"].as_str()),
        (StatusCode::OK, Some("disabled")),
        "{got}"
    );
    assert_eq!(
        kc.tokens.lock().unwrap().len(),
        tokens,
        "the realm is not called"
    );

    // bound without an email (the admin's binding), then invited: "email set", nothing sent
    let bind = json!({"issuer": demo.idp.issuer, "subject": "s-carl", "name": "carl"});
    assert_eq!(
        admin(&demo, "/v1/admin/users", Some(bind), k).await.0,
        StatusCode::OK
    );
    let (status, got) =
        invite_req(&demo, json!({"name": "carl", "email": "carl@demo.example"})).await;
    assert_eq!(
        (
            status,
            got["status"].as_str(),
            got["mail"].as_str(),
            got["reason"].as_str()
        ),
        (
            StatusCode::OK,
            Some("email set"),
            Some("none"),
            Some("signed in")
        ),
        "{got}"
    );
    assert_eq!(kc.tokens.lock().unwrap().len(), tokens);
    // the provider's person signs in after the invite; a re-invite answers "exists"
    assert_eq!(
        invite_req(&demo, json!({"name": INVITED.0, "email": INVITED.1}))
            .await
            .0,
        StatusCode::OK
    );
    Browser::new().sign_in(&demo).await;
    let actions = kc.actions.lock().unwrap().len();
    let (status, got) = invite_req(&demo, json!({"name": INVITED.0, "email": INVITED.1})).await;
    assert_eq!(
        (status, got["status"].as_str(), got["mail"].as_str()),
        (StatusCode::OK, Some("exists"), Some("none")),
        "{got}"
    );
    assert_eq!(kc.actions.lock().unwrap().len(), actions);
}

/// After a 409 only the one account carrying this invite's value and its email is adopted:
/// another value, two matches, a matching value under another email, or a name taken by an
/// account with another email are 409 to the admin with no email.
async fn accounts_this_invite_did_not_create_are_refused(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    let account = |id: &str, name: &str, email: &str, value: Option<&str>| {
        let mut a = json!({"id": id, "username": name, "email": email, "enabled": true});
        if let Some(v) = value {
            a["attributes"] = json!({INVITE_ATTRIBUTE: [v]});
        }
        a
    };
    let refused = |got: &Value, wording: &str| {
        assert_eq!(got["error"], "conflict", "{got}");
        let m = got["message"].as_str().unwrap();
        assert!(m.contains(wording), "{m}");
    };
    preclaimed(&env, "ann", "ann@demo.example", "v-ann").await;
    kc.accounts
        .lock()
        .unwrap()
        .push(account("kc-a", "ann", "ann@demo.example", Some("v-other")));
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    refused(
        &got,
        "an account with this email that this invitation did not create",
    );

    preclaimed(&env, "bob", "bob@demo.example", "v-bob").await;
    kc.accounts.lock().unwrap().extend([
        account("kc-b1", "bob", "bob@demo.example", Some("v-bob")),
        account("kc-b2", "bob2", "bob@demo.example", Some("v-bob")),
    ]);
    let (status, got) = invite_req(&env, json!({"name": "bob", "email": "bob@demo.example"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");

    // the name is taken by an account with another email: nothing found by this email
    preclaimed(&env, "cyd", "cyd@demo.example", "v-cyd").await;
    kc.accounts.lock().unwrap().push(account(
        "kc-c",
        "cyd",
        "cyd@elsewhere.example",
        Some("v-cyd"),
    ));
    let (status, got) = invite_req(&env, json!({"name": "cyd", "email": "cyd@demo.example"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    refused(
        &got,
        "the sign-in server already has an account that this invitation did not create",
    );
    assert!(
        !got["message"].as_str().unwrap().contains("with this email"),
        "{got}"
    );
    // a search that also returns it: the value matches, the email does not
    kc.search_ignores_email.store(true, Ordering::SeqCst);
    kc.accounts.lock().unwrap().retain(|a| a["id"] == "kc-c");
    let (status, got) = invite_req(&env, json!({"name": "cyd", "email": "cyd@demo.example"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert!(
        kc.actions.lock().unwrap().is_empty(),
        "no email for a foreign account"
    );
    assert_eq!(realm_record(&d, "cyd").await.0, None);
}

/// A refused create (other than 409) and a refused actions email are 502 with the user created.
async fn realm_failures_answer_502_with_the_user_created(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    kc.create_status.store(400, Ordering::SeqCst);
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(
        (status, got["user"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("ann")),
        "{got}"
    );
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("invalidEmailMessage"),
        "{got}"
    );
    kc.create_status.store(0, Ordering::SeqCst);
    kc.actions_status.store(500, Ordering::SeqCst);
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{got}");
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("sending the account-setup email was refused (500"),
        "{got}"
    );
    assert_eq!(
        realm_record(&d, "ann").await.0.as_deref(),
        Some("kc-1"),
        "a retry resumes with the account"
    );
    let users = {
        let s = env.store.clone();
        blocking(move || s.list_users().unwrap()).await
    };
    assert!(users.iter().any(|u| u.name == "ann"));
}

/// A Microsoft workspace's email names the Microsoft account; the same user and email get the same
/// email again (the same join page) under the same Idempotency-Key.
async fn a_microsoft_invite_names_the_microsoft_account(d: Arc<Dispatcher>) {
    let (resend_url, resend) = start_resend().await;
    let env = mail_env(&d, google_invite(resend_url, SignIn::Microsoft)).await;
    let body = json!({"name": "ann", "email": "ann@dakai.example"});
    let (status, got) = invite_req(&env, body.clone()).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("sent")),
        "{got}"
    );
    let text = resend.seen.lock().unwrap()[0].1["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("with your Microsoft account ann@dakai.example"),
        "{text}"
    );
    assert!(!text.contains("Google"), "{text}");
    invite_req(&env, body).await;
    {
        let seen = resend.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1, seen[1].1, "the same email again");
    }
    let keys = resend.keys.lock().unwrap().clone();
    assert!(
        keys[0].as_deref().is_some_and(|k| !k.contains("ann@")),
        "{keys:?}"
    );
    assert_eq!(keys[0], keys[1]);
}

/// An SSO workspace's email goes through Resend like Google's and names the company account; the
/// realm is not called.
async fn an_sso_invite_names_the_company_account(d: Arc<Dispatcher>) {
    let (resend_url, resend) = start_resend().await;
    let env = mail_env(
        &d,
        InviteConfig {
            workspace: "VVP USA".into(),
            account: "VVP USA".into(),
            ..google_invite(resend_url, SignIn::Sso)
        },
    )
    .await;
    let (status, got) =
        invite_req(&env, json!({"name": "ann", "email": "ann@vvpusa.example"})).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("sent")),
        "{got}"
    );
    let seen = resend.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let mail = &seen[0].1;
    assert_eq!(mail["subject"], "You're invited to VVP USA on conde");
    let text = mail["text"].as_str().unwrap();
    assert!(
        text.contains("with your VVP USA account ann@vvpusa.example"),
        "{text}"
    );
    let join = got["join"].as_str().unwrap();
    assert!(
        text.contains(&format!("Read {join} and follow it.")),
        "{text}"
    );
    assert!(
        !text.contains("Google") && !text.contains("Microsoft"),
        "{text}"
    );
}

/// The realm's username validator needs three characters: a shorter name is refused before
/// anything is created.
async fn a_password_workspace_needs_three_character_names(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    let (status, got) = invite_req(&env, json!({"name": "al", "email": "al@demo.example"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    assert_eq!(
        got["message"],
        "on a password workspace the name needs at least 3 characters"
    );
    assert!(kc.tokens.lock().unwrap().is_empty());
    let users = {
        let s = env.store.clone();
        blocking(move || s.list_users().unwrap()).await
    };
    assert!(!users.iter().any(|u| u.name == "al"));
}

/// The realm record's two conditional writers: a correlation value is claimed once; an id is
/// recorded once, only under the stored value; neither replaces what is stored.
async fn the_realm_record_is_claimed_and_recorded_once(d: Arc<Dispatcher>) {
    let board = Arc::new(WebBoard::new(d.clone()));
    blocking(move || {
        let uid = board.ensure_user("ann").unwrap().user_id;
        let record = |id: Option<&str>, invite: &str| web::RealmAccount {
            id: id.map(String::from),
            invite: Some(invite.to_string()),
        };
        assert_eq!(
            board.realm_account(uid).unwrap(),
            web::RealmAccount::default()
        );
        assert_eq!(
            board.claim_realm_invite(uid, "v1").unwrap(),
            record(None, "v1")
        );
        assert_eq!(
            board.claim_realm_invite(uid, "v2").unwrap(),
            record(None, "v1"),
            "the first value stays"
        );
        assert_eq!(
            board.record_realm_id(uid, "v2", "kc-x").unwrap(),
            record(None, "v1"),
            "only under the stored value"
        );
        assert_eq!(
            board.record_realm_id(uid, "v1", "kc-1").unwrap(),
            record(Some("kc-1"), "v1")
        );
        assert_eq!(
            board.record_realm_id(uid, "v1", "kc-2").unwrap(),
            record(Some("kc-1"), "v1"),
            "the id stays"
        );
        assert_eq!(
            board.claim_realm_invite(uid, "v3").unwrap(),
            record(Some("kc-1"), "v1")
        );
        assert_eq!(
            board.realm_account(uid).unwrap(),
            record(Some("kc-1"), "v1")
        );
    })
    .await;
}

/// The clear forgets one user's realm record (the id and a lone correlation value alike) and
/// answers whether there was one, None for an unknown user; a new value is claimed after it.
async fn the_realm_record_is_cleared_and_claimed_anew(d: Arc<Dispatcher>) {
    let board = Arc::new(WebBoard::new(d.clone()));
    blocking(move || {
        let uid = board.ensure_user("ann").unwrap().user_id;
        let bob = board.ensure_user("bob").unwrap().user_id;
        let claimed = |invite: &str| web::RealmAccount {
            id: None,
            invite: Some(invite.to_string()),
        };
        assert_eq!(board.clear_realm_account("ann").unwrap(), Some(false));
        board.claim_realm_invite(uid, "v1").unwrap();
        board.record_realm_id(uid, "v1", "kc-1").unwrap();
        board.claim_realm_invite(bob, "vb").unwrap();
        assert_eq!(board.clear_realm_account("ann").unwrap(), Some(true));
        assert_eq!(
            board.realm_account(uid).unwrap(),
            web::RealmAccount::default()
        );
        assert_eq!(
            board.realm_account(bob).unwrap(),
            claimed("vb"),
            "another user's record stays"
        );
        assert_eq!(board.clear_realm_account("ann").unwrap(), Some(false));
        assert_eq!(board.claim_realm_invite(uid, "v2").unwrap(), claimed("v2"));
        assert_eq!(board.clear_realm_account("ann").unwrap(), Some(true));
        assert_eq!(
            board.realm_account(uid).unwrap(),
            web::RealmAccount::default()
        );
        assert_eq!(board.clear_realm_account("nobody").unwrap(), None);
    })
    .await;
}

/// A user whose realm account was deleted in Keycloak: the re-invite is refused until the admin
/// clears the record; the next invite then creates a new account (the create answers 201) and
/// records it. The clear keeps the user's email and identities, answers false when nothing is
/// recorded, 404 for no such user, and needs the admin key.
async fn a_cleared_user_is_invited_again_with_a_new_realm_account(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    let k = Some(ADMIN_KEY);
    let invite = json!({"name": "ann", "email": "ann@demo.example"});
    let (status, got) = invite_req(&env, invite.clone()).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    let (id, first_mark) = realm_record(&d, "ann").await;
    assert_eq!(id.as_deref(), Some("kc-1"));
    let s = env.store.clone();
    blocking(move || {
        let other = web::Identity {
            issuer: "https://other.example/realms/x".into(),
            subject: "sub-ann".into(),
            display_name: None,
            preferred_username: None,
            email: None,
            verified_email: None,
        };
        s.bind_identity("ann", &other).unwrap()
    })
    .await;
    let ann = |env: &Env<WebBoard>| {
        let s = env.store.clone();
        async move {
            blocking(move || s.list_users().unwrap())
                .await
                .into_iter()
                .find(|u| u.name == "ann")
                .unwrap()
        }
    };
    let before = ann(&env).await;
    assert_eq!(
        (before.email.as_deref(), before.issuers.len()),
        (Some("ann@demo.example"), 1)
    );

    // deleted in Keycloak; the new account gets the next id
    *kc.accounts.lock().unwrap() =
        vec![json!({"id": "kc-other", "username": "zed", "email": "zed@demo.example"})];
    let (status, got) = invite_req(&env, invite.clone()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{got}");
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("the recorded sign-in account no longer matches"),
        "{got}"
    );

    let clear = "/v1/admin/users/ann/realm-account/clear";
    assert_eq!(
        admin(&env, clear, Some(json!({})), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(realm_record(&d, "ann").await.0.as_deref(), Some("kc-1"));
    let (status, got) = admin(&env, clear, Some(json!({})), k).await;
    assert_eq!(
        (status, got),
        (StatusCode::OK, json!({"user": "ann", "cleared": true}))
    );
    assert_eq!(realm_record(&d, "ann").await, (None, None));
    let (status, got) = admin(&env, clear, Some(json!({})), k).await;
    assert_eq!(
        (status, got),
        (StatusCode::OK, json!({"user": "ann", "cleared": false}))
    );
    let (status, got) = admin(
        &env,
        "/v1/admin/users/nobody/realm-account/clear",
        Some(json!({})),
        k,
    )
    .await;
    assert_eq!(
        (status, got["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    assert_eq!(ann(&env).await, before, "the email and identities stay");

    let (status, got) = invite_req(&env, invite).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    let creates = kc.creates.lock().unwrap().clone();
    assert_eq!(creates.len(), 2);
    let (id, mark) = realm_record(&d, "ann").await;
    let mark = mark.unwrap();
    assert_eq!(id.as_deref(), Some("kc-2"));
    assert_ne!(Some(&mark), first_mark.as_ref(), "a new correlation value");
    assert_eq!(creates[1]["attributes"][INVITE_ATTRIBUTE][0], mark.as_str());
    assert_eq!(
        kc.actions.lock().unwrap().last().unwrap().0,
        "kc-2",
        "the setup email goes to the new account"
    );
}

/// A sign-in server that does not answer is cut off at the invite's deadline: 502, user created.
async fn a_slow_sign_in_server_is_cut_off_at_the_deadline(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let mut config = demo_invite(&kc_url);
    config.deadline = Some(Duration::from_secs(1));
    let env = mail_env(&d, config).await;
    kc.token_delay_ms.store(3000, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(
        (status, got["user"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("ann")),
        "{got}"
    );
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("did not answer within 1 s"),
        "{got}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
}

// --- the workspace's welcome -----------------------------------------------------------------

/// `owner/name` published (mailbox-bound) on `host`, a v2 host, as `conde publish` leaves it; its
/// id.
async fn published(d: &Arc<Dispatcher>, owner: &str, name: &str, host: &str) -> String {
    let (d, owner, name, host) = (
        d.clone(),
        owner.to_string(),
        name.to_string(),
        host.to_string(),
    );
    blocking(move || {
        d.with_tx(|t| {
            let id = conde::util::new_ulid();
            let user = t.ensure_user(&owner)?;
            t.ensure_host(&host, user)?;
            t.set_host_mailbox_only(&host)?;
            t.insert_agent(&id, user, &name)?;
            let at = conde::address::Address::new(conde::address::Kind::Mailbox, &id, &host);
            t.set_binding(&id, Some(&at))?;
            Ok(id)
        })
        .unwrap()
    })
    .await
}

/// The DMs `owner/name` has, as (from, text).
async fn dms(d: &Arc<Dispatcher>, owner: &str, name: &str) -> Vec<(String, String)> {
    let (d, owner, name) = (d.clone(), owner.to_string(), name.to_string());
    blocking(move || {
        d.with_conn(|t| {
            let Some(user) = t.user_id(&owner)? else {
                return Ok(Vec::new());
            };
            let Some(agent) = t.agent_by_name(user, &name)? else {
                return Ok(Vec::new());
            };
            Ok(t.inbox(&agent.id, 0, 100)?
                .into_iter()
                .map(|m| (m.sender, m.text))
                .collect())
        })
        .unwrap()
    })
    .await
}

async fn mint(env: &Env<WebBoard>, user: &str, host: &str) {
    let (status, got) = admin(
        env,
        "/v1/admin/keys",
        Some(json!({"user": user, "host_label": host})),
        Some(ADMIN_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
}

/// The board shared under `name`, as `conde server` serves it: the greeting names it.
fn shared(d: &Arc<Dispatcher>) {
    d.share("clearscale");
}

/// `boss` is an admin with the published agent `boss/helper` on host box, named by the welcome;
/// the agent's id.
async fn welcome_ready(d: &Arc<Dispatcher>, env: &Env<WebBoard>) -> String {
    shared(d);
    let k = Some(ADMIN_KEY);
    mint(env, "boss", "box").await;
    let id = published(d, "boss", "helper", "box").await;
    admin(env, "/v1/admin/users/boss/promote", Some(json!({})), k).await;
    let w = json!({"admin_agent": "boss/helper", "text": "Hi from ClearScale."});
    let (status, got) = admin(env, "/v1/admin/welcome", Some(w), k).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    id
}

/// One op as `p`: the response.
async fn op_as(
    d: &Arc<Dispatcher>,
    p: conde::dispatch::Principal,
    actor: Option<String>,
    op: &str,
    body: Value,
) -> conde::protocol::Response {
    let (d, op) = (d.clone(), op.to_string());
    blocking(move || {
        let mut req = conde::protocol::Request::new(conde::util::new_ulid(), &op, body);
        req.actor = actor;
        req.idempotency_key = Some(conde::util::new_ulid());
        d.dispatch(&p, &req)
    })
    .await
}

/// The owner view of `user` in the browser: its feed as JSON text.
async fn feed_of(d: &Arc<Dispatcher>, user: &str) -> String {
    let d2 = d.clone();
    let name = user.to_string();
    let user_id = blocking(move || d2.with_conn(|t| t.user_id(&name)).unwrap().unwrap()).await;
    let p = conde::dispatch::Principal {
        user_id,
        user: user.to_string(),
        host: web::BROWSER_HOST.to_string(),
        local: false,
        bridge_instance: None,
        bridge_connection: None,
        browser: true,
    };
    let r = op_as(d, p, None, "owner.feed", json!({})).await;
    assert!(r.ok, "{:?}", r.error);
    r.result.unwrap().to_string()
}

/// The welcome's admin agent must be a published agent of an admin user (promote and demote with
/// the admin key, the role in the user list); its lobby a channel; GET shows it with the
/// defaults filled. A change keeps the fields it leaves out; null (or `welcome set` without
/// --agent) clears one; clearing everything resets it.
async fn the_welcome_names_a_published_agent_of_an_admin(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let k = Some(ADMIN_KEY);
    let set = |w: Value| {
        let env = &env;
        async move { admin(env, "/v1/admin/welcome", Some(w), k).await }
    };
    let (status, got) = admin(&env, "/v1/admin/welcome", None, k).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["welcome"],
        json!({"admin_agent": null, "lobby": null, "guide": "https://conde.cloud/docs/setup",
               "text": web::DEFAULT_WELCOME_TEXT})
    );
    assert_eq!(
        admin(&env, "/v1/admin/welcome", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let agent = json!({"admin_agent": "krisztian/networkoptix"});
    let (status, got) = set(agent.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("no user krisztian"),
        "{got}"
    );
    published(&d, "krisztian", "networkoptix", "box").await;
    let (status, got) = set(agent.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert!(
        got["message"].as_str().unwrap().contains("not an admin"),
        "{got}"
    );

    let (status, got) = admin(
        &env,
        "/v1/admin/users/krisztian/promote",
        Some(json!({})),
        k,
    )
    .await;
    assert_eq!(
        (status, got),
        (StatusCode::OK, json!({"user": "krisztian", "admin": true}))
    );
    let (_, users) = admin(&env, "/v1/admin/users", None, k).await;
    assert_eq!(users["users"][0]["admin"], true, "{users}");
    assert_eq!(
        admin(&env, "/v1/admin/users/nobody/promote", Some(json!({})), k)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, got) = set(json!({"admin_agent": "krisztian/desk"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("not a published agent"),
        "{got}"
    );
    let (status, got) = set(json!({"lobby": "#team"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert!(
        got["message"]
            .as_str()
            .unwrap()
            .contains("no channel #team"),
        "{got}"
    );
    let (status, got) = set(json!({"admin_agent": "desk"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");

    let (status, got) =
        set(json!({"admin_agent": "Krisztian/NetworkOptix", "text": "Hi from ClearScale."})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    // a change of the guide alone keeps the agent and the text
    let (status, got) = set(json!({"guide": "https://clearscale.conde.cloud/readme"})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (_, got) = admin(&env, "/v1/admin/welcome", None, k).await;
    assert_eq!(
        got["welcome"],
        json!({"admin_agent": "krisztian/networkoptix", "lobby": null,
               "guide": "https://clearscale.conde.cloud/readme", "text": "Hi from ClearScale."})
    );
    // `welcome set` without --agent: the agent is cleared, the rest kept
    let (_, got) = set(json!({"admin_agent": null})).await;
    assert_eq!(got["stored"]["admin_agent"], Value::Null, "{got}");
    assert_eq!(got["stored"]["text"], "Hi from ClearScale.", "{got}");
    // `welcome clear`
    let (_, got) =
        set(json!({"admin_agent": null, "lobby": null, "guide": null, "text": null})).await;
    assert_eq!(
        got["stored"],
        json!({"admin_agent": null, "lobby": null, "guide": null, "text": null})
    );

    let (status, got) = admin(&env, "/v1/admin/users/krisztian/demote", Some(json!({})), k).await;
    assert_eq!(
        (status, got),
        (StatusCode::OK, json!({"user": "krisztian", "admin": false}))
    );
    let (status, got) = set(agent).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a demoted user's agent is refused: {got}"
    );
}

/// A user's first host enrollment greets the newcomer's human name in the admin agent's name
/// and sends the admin agent an arrival notice (notified), once: not for the user's second host,
/// not for the admin's own enrollment, and nothing while the welcome names no admin agent. The
/// greeting is in the owner view of the newcomer and the admin, nobody else's; the admin agent's
/// reply to the human name is refused.
async fn a_first_enrollment_is_greeted_once_and_the_admin_agent_told(d: Arc<Dispatcher>) {
    shared(&d);
    let env = env(&d).await;
    let k = Some(ADMIN_KEY);
    // the admin's own first enrollment, before any welcome: nothing to send
    mint(&env, "krisztian", "box").await;
    let admin_id = published(&d, "krisztian", "networkoptix", "box").await;
    admin(
        &env,
        "/v1/admin/users/krisztian/promote",
        Some(json!({})),
        k,
    )
    .await;
    // no admin agent named: a newcomer arrives unannounced (and counts as arrived)
    mint(&env, "bob", "bob-box").await;
    assert!(dms(&d, "krisztian", "networkoptix").await.is_empty());
    assert!(dms(&d, "bob", "human").await.is_empty());

    let w = json!({"admin_agent": "krisztian/networkoptix", "text": "Hi from ClearScale."});
    assert_eq!(
        admin(&env, "/v1/admin/welcome", Some(w), k).await.0,
        StatusCode::OK
    );
    mint(&env, "ann", "laptop").await;
    let notices = dms(&d, "krisztian", "networkoptix").await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (from, text) = &notices[0];
    assert_eq!(from, "ann/human");
    assert!(
        text.starts_with("ann enrolled host laptop (admin key) at ")
            && text.ends_with(
                "; say hello (their human name takes no messages; greet one of their published agents)"
            ),
        "{text}"
    );
    let greetings = dms(&d, "ann", "human").await;
    assert_eq!(greetings.len(), 1, "{greetings:?}");
    let (from, text) = &greetings[0];
    assert_eq!(from, "krisztian/networkoptix");
    assert!(
        text.starts_with("Hi from ClearScale.\n\nYou are in, ann"),
        "{text}"
    );
    assert!(
        text.contains("conde send krisztian/networkoptix \"<your question>\" --board clearscale")
            && text.contains("conde publish <name> --board clearscale"),
        "{text}"
    );
    // the notice reaches the admin agent's host as a notification; the greeting pages no one
    let d2 = d.clone();
    let pending = blocking(move || {
        d2.with_conn(|t| t.due_notifications(i64::MAX, "box", 10))
            .unwrap()
    })
    .await;
    assert_eq!(
        pending
            .iter()
            .map(|n| n.recipient_id.as_str())
            .collect::<Vec<_>>(),
        [admin_id.as_str()]
    );
    // the owner views: the newcomer's and the admin's, not a third owner's
    assert!(feed_of(&d, "ann").await.contains("You are in, ann"));
    assert!(feed_of(&d, "krisztian").await.contains("You are in, ann"));
    assert!(!feed_of(&d, "bob").await.contains("You are in, ann"));
    // the admin agent answering the notice is told to greet a published agent instead
    let d2 = d.clone();
    let p = blocking(move || d2.principal("krisztian", "box", false).unwrap()).await;
    let notice_id = blocking({
        let d = d.clone();
        let admin_id = admin_id.clone();
        move || {
            d.with_conn(|t| t.inbox(&admin_id, 0, 10))
                .unwrap()
                .remove(0)
                .id
        }
    })
    .await;
    let r = op_as(
        &d,
        p,
        Some(format!("mailbox:{admin_id}")),
        "message.send",
        json!({"to": {"agent": "ann/human"}, "text": "hello", "reply_to": notice_id}),
    )
    .await;
    let e = r.error.expect("refused");
    assert_eq!(
        e.code,
        conde::protocol::ErrorCode::Forbidden,
        "{}",
        e.message
    );
    assert!(
        e.message.contains("message one of `ann`'s agents"),
        "{}",
        e.message
    );

    // the second host of the same user, and bob's after the welcome was set: no more
    mint(&env, "ann", "desktop").await;
    mint(&env, "bob", "bob-laptop").await;
    assert_eq!(dms(&d, "krisztian", "networkoptix").await.len(), 1);
    assert_eq!(dms(&d, "ann", "human").await.len(), 1);
    assert!(dms(&d, "bob", "human").await.is_empty());
}

/// The keys page is an enrollment: its first key greets the user.
async fn a_keys_page_enrollment_is_greeted(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    welcome_ready(&d, &env).await;
    let b = Browser::new();
    let csrf = csrf_of(&b.sign_in(&env).await);
    key_of(&b.create_key(&env, &csrf, "Work Laptop").await);
    let notices = dms(&d, "boss", "helper").await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0]
            .1
            .starts_with("krisztian-dobmayer enrolled host work-laptop (keys page) at "),
        "{notices:?}"
    );
    assert_eq!(dms(&d, "krisztian-dobmayer", "human").await.len(), 1);
}

/// `conde login` is an enrollment: its redeemed key greets the user.
async fn a_conde_login_enrollment_is_greeted(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    welcome_ready(&d, &env).await;
    let b = Browser::new();
    let run = cli_run();
    let code = browser_leg(&env, &b, &run).await;
    let body = json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": "Test-Box"});
    let (status, got) = redeem(&env, body).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let notices = dms(&d, "boss", "helper").await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0]
            .1
            .starts_with("krisztian-dobmayer enrolled host test-box (conde login) at "),
        "{notices:?}"
    );
    let greetings = dms(&d, "krisztian-dobmayer", "human").await;
    assert_eq!(greetings.len(), 1, "{greetings:?}");
    assert_eq!(greetings[0].0, "boss/helper");
}

/// Two first enrollments of one user at once: one greeting, one notice.
async fn two_concurrent_first_mints_greet_once(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    welcome_ready(&d, &env).await;
    tokio::join!(mint(&env, "ann", "laptop"), mint(&env, "ann", "desktop"));
    assert_eq!(dms(&d, "boss", "helper").await.len(), 1);
    assert_eq!(dms(&d, "ann", "human").await.len(), 1);
}

/// A board served without its shared name writes no greeting (its commands would name no board).
async fn an_unshared_board_sends_no_welcome(d: Arc<Dispatcher>) {
    let env = env(&d).await;
    let k = Some(ADMIN_KEY);
    mint(&env, "boss", "box").await;
    published(&d, "boss", "helper", "box").await;
    admin(&env, "/v1/admin/users/boss/promote", Some(json!({})), k).await;
    let w = json!({"admin_agent": "boss/helper"});
    assert_eq!(
        admin(&env, "/v1/admin/welcome", Some(w), k).await.0,
        StatusCode::OK
    );
    mint(&env, "ann", "laptop").await;
    assert!(dms(&d, "boss", "helper").await.is_empty());
    assert!(dms(&d, "ann", "human").await.is_empty());
}

// --- the join page --------------------------------------------------------------------------

const CHROME: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const BROWSER_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

/// One GET with these headers (none: reqwest's `*/*` and no User-Agent, as a fetch library
/// sends): (status, content type, Vary, body).
async fn fetch(
    url: &str,
    accept: Option<&str>,
    ua: Option<&str>,
) -> (StatusCode, String, String, String) {
    let mut req = reqwest::Client::new().get(url);
    if let Some(a) = accept {
        req = req.header(header::ACCEPT, a);
    }
    if let Some(ua) = ua {
        req = req.header(header::USER_AGENT, ua);
    }
    let r = req.send().await.unwrap();
    let h = |name| {
        r.headers()
            .get(name)
            .map(|v: &header::HeaderValue| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    };
    let (status, ctype, vary) = (r.status(), h(header::CONTENT_TYPE), h(header::VARY));
    assert_eq!(h(header::CACHE_CONTROL), "no-store");
    (status, ctype, vary, r.text().await.unwrap())
}

/// The board shared as `board` with `boss/helper` as the welcome's admin agent and `#general`
/// as its lobby.
async fn join_welcome(d: &Arc<Dispatcher>, env: &Env<WebBoard>, board: &str) {
    d.share(board);
    let k = Some(ADMIN_KEY);
    mint(env, "boss", "box").await;
    let helper = published(d, "boss", "helper", "box").await;
    admin(env, "/v1/admin/users/boss/promote", Some(json!({})), k).await;
    let d2 = d.clone();
    blocking(move || {
        d2.with_tx(|t| {
            Ok(t.insert_channel(&conde::store::Channel {
                id: conde::util::new_ulid(),
                name: "general".into(),
                created_by: helper,
                topic: None,
            })?)
        })
        .unwrap()
    })
    .await;
    let w = json!({"admin_agent": "boss/helper", "lobby": "#general"});
    let (status, got) = admin(env, "/v1/admin/welcome", Some(w), k).await;
    assert_eq!(status, StatusCode::OK, "{got}");
}

fn in_order(text: &str, parts: &[&str]) {
    let mut at = 0;
    for part in parts {
        let found = text[at..]
            .find(part)
            .unwrap_or_else(|| panic!("{part:?} missing after byte {at}: {text}"));
        at += found + part.len();
    }
}

/// An SSO invite issues a join link (the same invite again, the same link). An agent fetching
/// it gets the markdown guide for this workspace, a browser the human page with the line to
/// paste; `?format=md` and an Accept naming markdown decide over a browser's User-Agent.
async fn an_sso_invite_opens_its_join_page_for_agents_and_browsers(d: Arc<Dispatcher>) {
    let (resend_url, _resend) = start_resend().await;
    let env = mail_env(
        &d,
        InviteConfig {
            workspace: "VVP USA".into(),
            account: "VVP USA".into(),
            ..google_invite(resend_url, SignIn::Sso)
        },
    )
    .await;
    join_welcome(&d, &env, "vvpusa").await;
    let ask = json!({"name": "ann", "email": "ann@vvpusa.example", "inviter": "Krisz & Co"});
    let (status, got) = invite_req(&env, ask.clone()).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("sent")),
        "{got}"
    );
    let join = got["join"].as_str().unwrap().to_string();
    assert!(join.starts_with(&env.url("/join/")), "{join}");
    assert_eq!(join.len(), env.url("/join/").len() + 64, "{join}");
    // the same invite again, twice at once: the same link, refreshed
    let ((s1, a1), (s2, a2)) = tokio::join!(invite_req(&env, ask.clone()), invite_req(&env, ask));
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK), "{a1} {a2}");
    for again in [a1, a2] {
        assert_eq!(
            again["join"],
            join.as_str(),
            "the same invite, the same link"
        );
    }

    let (status, ctype, vary, md) = fetch(&join, None, None).await;
    assert_eq!(
        (status, ctype.as_str()),
        (StatusCode::OK, "text/markdown; charset=utf-8")
    );
    assert_eq!(vary, "Accept, User-Agent");
    let server = env.base.clone();
    in_order(
        &md,
        &[
            "# Join the VVP USA workspace on conde",
            "Krisz & Co invited them as `ann`.",
            &format!("The workspace is {server}, and its board name is `vvpusa`."),
            "Your human signs in with their VVP USA account.",
            "1. Run `conde --version`.",
            "curl -fsSL https://conde.cloud/install.sh | bash",
            "2. Sign in.",
            "they sign in there with their VVP USA account. Then run:",
            &format!("conde login --server {server} --board vvpusa\n"),
            "3. Run `conde doctor`.",
            "4. Run `conde whoami`.",
            "conde register <name>",
            "conde publish <name> --board vvpusa",
            "5. Say hello to the workspace admin's agent, once:",
            "conde send boss/helper \"Hello: <name> joined vvpusa for ann, on <this machine's hostname>.\" --board vvpusa",
            "6. Tell your human, in two sentences:",
            "`conde send <owner>/<agent> \"<message>\" --board vvpusa`",
            "questions go to `boss/helper`, and the team channel is #general.",
        ],
    );
    assert!(md.lines().count() <= 60, "{} lines", md.lines().count());
    for absent in ["password", "Google", "Microsoft", "<!doctype", "&amp;"] {
        assert!(!md.contains(absent), "{absent}: {md}");
    }
    println!("--- the join guide of an sso invite ---\n{md}--- end ---");

    let (status, ctype, _, html) = fetch(&join, Some(BROWSER_ACCEPT), Some(CHROME)).await;
    assert_eq!(
        (status, ctype.as_str()),
        (StatusCode::OK, "text/html; charset=utf-8")
    );
    in_order(
        &html,
        &[
            "<title>Join VVP USA · conde</title>",
            "Krisz &#38; Co invited you to the VVP USA workspace on conde, as <code>ann</code>.",
            r#"<a href="https://conde.cloud">conde.cloud</a>"#,
            "<h2>Hand the setup to your agent</h2>",
            "sign in with your VVP USA account.",
            &format!(r#"<pre id="key">Read {join} and follow it.</pre>"#),
            r#"<button type="button" id="copy">"#,
            "<h2>By hand</h2>",
            "curl -fsSL https://conde.cloud/install.sh | bash",
            &format!("conde login --server {server} --board vvpusa"),
            "Paste the line above to your agent.",
            r#"<a href="https://conde.cloud/docs/setup">"#,
            &format!(r#"<a href="{join}?format=md">this page as text</a>"#),
            "<script nonce=",
        ],
    );
    assert!(!html.contains("Set your password"), "{html}");
    assert!(!html.contains("Krisz & Co"), "escaped: {html}");

    // the query and an Accept naming markdown decide over a browser's User-Agent
    for (url, accept) in [
        (format!("{join}?format=md"), BROWSER_ACCEPT),
        (join.clone(), "text/markdown, text/html;q=0.9"),
    ] {
        let (_, ctype, _, body) = fetch(&url, Some(accept), Some(CHROME)).await;
        assert!(
            ctype.starts_with("text/markdown"),
            "{url} {accept}: {ctype}"
        );
        assert_eq!(body, md);
    }
    let (_, ctype, _, _) = fetch(&format!("{join}?format=html"), None, Some("curl/8.11.1")).await;
    assert!(ctype.starts_with("text/html"), "{ctype}");
    let (_, ctype, _, _) = fetch(&join, Some("*/*"), Some("curl/8.11.1")).await;
    assert!(ctype.starts_with("text/markdown"), "curl: {ctype}");
}

/// A password invite's account-setup email links on to its join page, which puts the password
/// step first: the agent asks its human to set the password from that email, the page shows it
/// as step 1; without an admin agent the guide has no hello step.
async fn a_password_invite_opens_its_join_page_with_the_password_step_first(d: Arc<Dispatcher>) {
    let (kc_url, kc) = start_keycloak().await;
    let env = mail_env(&d, demo_invite(&kc_url)).await;
    d.share("demo");
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@demo.example"})).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("keycloak")),
        "{got}"
    );
    let join = got["join"].as_str().unwrap().to_string();
    // the account-setup email's page links on to the join page, a redirect of the sign-in client
    assert_eq!(
        kc.redirects.lock().unwrap().clone(),
        [(CLIENT_ID.to_string(), join.clone())]
    );
    let (status, _, _, md) = fetch(&join, None, Some("curl/8.11.1")).await;
    assert_eq!(status, StatusCode::OK, "{md}");
    in_order(
        &md,
        &[
            "# Join the demo workspace on conde",
            "They are invited as `ann`.",
            "Your human signs in with a password for this workspace.",
            "account-setup email",
            "ask them to do that before step 2.",
            "they sign in there with the password they set. Then run:",
            &format!("conde login --server {} --board demo", env.base),
            "5. Tell your human, in two sentences:",
            "lists them).",
        ],
    );
    assert!(!md.contains("6. ") && !md.contains("Say hello"), "{md}");
    assert!(
        !md.contains("questions go to") && !md.contains("team channel"),
        "{md}"
    );

    let (status, _, _, html) = fetch(&join, Some(BROWSER_ACCEPT), Some(CHROME)).await;
    assert_eq!(status, StatusCode::OK);
    in_order(
        &html,
        &[
            "You are invited to the demo workspace on conde, as <code>ann</code>.",
            "<h2>1. Set your password</h2>",
            "a separate email with a link to set your password",
            "<h2>2. Hand the rest to your agent</h2>",
            &format!("Read {join} and follow it."),
            "<h2>By hand</h2>",
            &format!("conde login --server {} --board demo", env.base),
        ],
    );
}

/// A join link works for JOIN_TTL after the invite, then says it expired (410); an unknown or
/// malformed token, or a disabled user's link, says the link is not valid (404). Both answer
/// in the reader's format and point to the setup guide. A workspace without mail still issues
/// the link.
async fn a_join_link_expires_and_an_unknown_one_says_so(d: Arc<Dispatcher>) {
    let env = mail_env(
        &d,
        InviteConfig {
            sign_in: Some(SignIn::Google),
            workspace: "clearscale".into(),
            ..InviteConfig::default()
        },
    )
    .await;
    shared(&d);
    let (status, got) = invite_req(&env, json!({"name": "ann", "email": "ann@cs.example"})).await;
    assert_eq!(
        (status, got["mail"].as_str()),
        (StatusCode::OK, Some("unconfigured")),
        "{got}"
    );
    let join = got["join"].as_str().unwrap().to_string();
    let (_, bob) = invite_req(&env, json!({"name": "bob", "email": "bob@cs.example"})).await;
    let bob = bob["join"].as_str().unwrap().to_string();
    let k = Some(ADMIN_KEY);
    admin(&env, "/v1/admin/users/bob/disable", Some(json!({})), k).await;

    let token = join.rsplit('/').next().unwrap();
    let flipped = format!(
        "{}{}",
        &token[..63],
        if token.ends_with('0') { '1' } else { '0' }
    );
    for url in [
        env.url(&format!("/join/{flipped}")),
        env.url("/join/abc"),
        env.url(&format!("/join/{}", token.to_uppercase())),
        bob.clone(),
    ] {
        let (status, ctype, _, md) = fetch(&url, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{url}: {md}");
        assert!(ctype.starts_with("text/markdown"), "{ctype}");
        in_order(
            &md,
            &[
                "# This invitation link is not valid",
                "Tell your human to ask whoever invited them for a new invitation.",
                "https://conde.cloud/docs/setup",
            ],
        );
        let (status, _, _, html) = fetch(&url, Some(BROWSER_ACCEPT), Some(CHROME)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            html.contains("<h1>This invitation link is not valid</h1>"),
            "{html}"
        );
    }

    env.advance(web::JOIN_TTL);
    assert_eq!(
        fetch(&join, None, None).await.0,
        StatusCode::OK,
        "valid to the second"
    );
    env.advance(1);
    let (status, _, _, md) = fetch(&join, None, None).await;
    assert_eq!(status, StatusCode::GONE, "{md}");
    in_order(
        &md,
        &[
            "# This invitation has expired",
            "An invitation link works for 7 days after the invite.",
            "Tell your human to ask whoever invited them for a new invitation.",
            "https://conde.cloud/docs/setup",
        ],
    );
    let (status, _, _, html) = fetch(&join, Some(BROWSER_ACCEPT), Some(CHROME)).await;
    assert_eq!(status, StatusCode::GONE);
    in_order(
        &html,
        &[
            "<h1>This invitation has expired</h1>",
            "Ask whoever invited you for a new invitation.",
            r#"<a href="https://conde.cloud/docs/setup">"#,
        ],
    );
    assert!(!html.contains("<script"), "{html}");
}
