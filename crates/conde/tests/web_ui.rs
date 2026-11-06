//! The web app's server side over the board's real stores: the app shell at `/`, the browser
//! session on `/v1/rpc` (CSRF, origin, a read-only op allow-list), the owner view (what the
//! signed-in user's own agents can read, nothing else), the fleet, attachment downloads and
//! the event socket. Each case runs on SQLite and, with CONDE_TEST_PG, on Postgres (where the
//! board events travel through LISTEN/NOTIFY). Sign-in goes through conde-remote's fake
//! OpenID provider.

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
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use conde::dispatch::{Dispatcher, Principal};
use conde::protocol::Request;
use conde::web_store::WebBoard;
use conde_remote::web::{self, BridgeRegistry, WebConfig, WebSetup, WebState, WebStore};
use conde_remote::{NotificationSource, ServerConfig, ServerHandle};
use futures_util::{SinkExt, StreamExt};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, sha2::Sha256};
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

include!("../../conde-remote/tests/support/web_harness.rs");

/// The user the fake provider signs in (from its preferred_username).
const USER: &str = "krisztian-dobmayer";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

/// The Postgres database of each board on Postgres, for the reads no op answers.
fn pg_urls() -> &'static Mutex<HashMap<usize, String>> {
    static URLS: OnceLock<Mutex<HashMap<usize, String>>> = OnceLock::new();
    URLS.get_or_init(Mutex::default)
}

async fn board(on_pg: bool) -> Option<Arc<Dispatcher>> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let url = if on_pg {
        match blocking(pg::fresh_database).await {
            Some(url) => Some(url),
            None => {
                pg::skip("the web app suite on Postgres");
                return None;
            }
        }
    } else {
        None
    };
    let d = Arc::new(
        blocking({
            let url = url.clone();
            move || match url {
                Some(url) => {
                    Dispatcher::connect(&url, conde::store::postgres::PgOpen::SERVER).unwrap()
                }
                None => Dispatcher::open_in_memory().unwrap(),
            }
        })
        .await,
    );
    if let Some(url) = url {
        pg_urls()
            .lock()
            .unwrap()
            .insert(Arc::as_ptr(&d) as usize, url);
    }
    Some(d)
}

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
    // keyed by address: gone before the address can be reused
    pg_urls()
        .lock()
        .unwrap()
        .remove(&(Arc::as_ptr(&d) as usize));
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
    the_root_serves_the_app_to_a_signed_in_browser_only,
    browser_rpc_needs_the_session_its_csrf_token_and_this_origin,
    the_browser_is_read_only_and_names_nobody,
    the_owner_sees_what_their_agents_can_read_and_nothing_else,
    another_owner_sees_only_their_own_agents,
    a_retired_name_leaves_the_lists_and_keeps_its_history,
    the_browser_searches_what_its_agents_can_read,
    the_feed_is_what_the_owners_agents_can_read,
    the_feed_pages_between_exclusive_bounds,
    the_feed_lists_delivery_problems,
    a_retry_to_an_unbound_recipient_is_a_problem,
    a_thread_never_steps_through_a_message_the_owner_cannot_read,
    a_long_thread_starts_near_the_message_and_continues_without_holes,
    delivery_state_shows_the_retry_clock_the_hold_and_the_last_error,
    a_read_message_never_shows_the_error_of_a_stale_attempt,
    history_pages_back_in_chronological_order,
    attachments_download_only_where_an_own_agent_can_read,
    channel_members_shows_read_positions_to_members_only,
    the_fleet_shows_online_discovery_and_own_delivery_health,
    the_event_socket_says_when_the_board_changed,
    the_browser_host_is_never_minted_as_a_real_host,
    owner_reads_end_when_the_last_own_member_leaves_a_channel,
    a_credential_on_the_browser_host_never_reaches_the_owner_view,
    a_connected_bridge_keeps_its_host_online,
    the_slack_instance_is_never_a_host,
    the_built_app_reads_through_the_real_ops,
    the_owner_sets_notify_for_their_own_agents_only,
    the_first_sign_in_of_an_invited_person_requests_a_machine_and_lands_on_the_page,
    a_returning_member_gets_a_machine_only_from_the_setup_step_with_its_stored_subject,
    a_refused_put_is_never_issued_and_the_setup_step_issues_it,
    a_failed_deployment_is_read_on_demand_throttled_and_retried_by_setup,
    a_hand_joined_host_is_adopted_without_a_deploy,
    setup_is_refused_while_deleting,
    machine_status_says_requested_failed_not_connected_up_connected_disconnected_and_ready,
    an_adopted_running_machine_is_probed_at_setup_and_its_reply_marks_it_ready,
    enrol_mints_once_per_oid_and_answers_the_contract,
    enrol_refuses_a_bad_signature_issuer_tenant_audience_or_expiry_with_401,
    enrol_answers_404_until_the_vm_resource_has_its_principal,
    enrol_answers_403_for_a_disabled_owner_or_a_deleting_record,
    enrol_is_rate_limited_with_retry_after,
    enrol_answers_503_while_the_tenant_keys_cannot_be_read_and_mints_once_they_can,
    a_relay_start_reaches_the_bridge_and_its_events_reach_the_page,
    a_relay_code_is_sent_once_and_cleared,
    a_start_committed_between_read_and_send_survives_the_clear,
    a_new_start_replaces_the_live_relay_and_the_old_ids_events_are_dropped,
    a_lost_bridge_connection_expires_the_live_relay,
    a_relay_stuck_in_starting_reads_as_expired_after_60_s,
    a_relay_op_is_refused_for_another_owner_or_a_dead_relay_id,
    a_cancel_reaches_the_bridge_and_ends_the_relay,
    disabling_a_user_sets_deleting_cancels_a_running_deployment_deletes_the_one_login_assignment_and_the_vm_and_waits_for_404,
    an_absent_vm_counts_as_deleted,
    a_failed_step_is_recorded_and_the_next_disable_call_resumes_from_it,
);

/// The whole server (bridge API, web sign-in, the app) over `d`, signed in through the fake
/// provider.
struct Ui {
    env: Env<WebBoard>,
    server: ServerHandle,
    d: Arc<Dispatcher>,
}

async fn ui(d: &Arc<Dispatcher>) -> Ui {
    ui_with(d, ServerConfig::default()).await
}

async fn ui_with(d: &Arc<Dispatcher>, config: ServerConfig) -> Ui {
    ui_full(d, config, None, |_| {}).await
}

async fn ui_full(
    d: &Arc<Dispatcher>,
    config: ServerConfig,
    machines: Option<web::MachinesConfig>,
    tweak: impl FnOnce(&mut WebConfig),
) -> Ui {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let idp = start_idp().await;
    // Bound here and handed over: the port is known before the server starts, and no other
    // test can take it in between.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut entra = WebConfig::entra(
        TENANT,
        CLIENT_ID,
        CLIENT_SECRET,
        &format!("{base}/auth/callback"),
    )
    .unwrap();
    entra.issuer = idp.issuer.clone();
    tweak(&mut entra);
    // the tenant's v1 signing keys are the fake provider's
    let login = idp
        .issuer
        .trim_end_matches(&format!("/{TENANT}/v2.0"))
        .to_string();
    let machines = machines.map(|m| web::MachinesConfig {
        login_url: login,
        ..m
    });
    let store = Arc::new(WebBoard::new(d.clone()));
    let web = WebSetup {
        entra: Some(entra),
        admin_key: Some(ADMIN_KEY.into()),
        machines,
        store: store.clone(),
    };
    let server = conde::cloud::start_on(
        d.clone(),
        listener,
        ServerConfig {
            web: Some(web),
            ..config
        },
    )
    .await
    .unwrap();
    let env = Env {
        base,
        idp,
        store,
        registry: Arc::new(FakeRegistry::default()),
        clock: Arc::new(AtomicI64::new(START)),
    };
    env.invite(INVITED.0, INVITED.1).await;
    Ui {
        env,
        server,
        d: d.clone(),
    }
}

impl Ui {
    /// A browser signed in as the provider's current person, with its CSRF token.
    async fn browser(&self) -> (Browser, String) {
        let b = Browser::new();
        b.sign_in(&self.env).await;
        let s = self.session(&b).await;
        (b, s["csrf"].as_str().unwrap().to_string())
    }

    async fn session(&self, b: &Browser) -> Value {
        b.follow
            .get(self.env.url("/v1/session"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// One browser RPC with the CSRF header and this origin.
    async fn rpc(&self, b: &Browser, csrf: &str, op: &str, body: Value) -> (StatusCode, Value) {
        let request =
            json!({"v": 1, "type": "request", "request_id": "r1", "op": op, "body": body});
        let r = b
            .step
            .post(self.env.url("/v1/rpc"))
            .header("x-csrf-token", csrf)
            .header(header::ORIGIN, &self.env.base)
            .json(&request)
            .send()
            .await
            .unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }

    /// One browser RPC that writes: the request carries `key` as its idempotency key.
    async fn mutate(
        &self,
        b: &Browser,
        csrf: &str,
        op: &str,
        body: Value,
        key: &str,
    ) -> (StatusCode, Value) {
        let request = json!({"v": 1, "type": "request", "request_id": "r1", "op": op, "body": body,
                             "idempotency_key": key});
        let r = b
            .step
            .post(self.env.url("/v1/rpc"))
            .header("x-csrf-token", csrf)
            .header(header::ORIGIN, &self.env.base)
            .json(&request)
            .send()
            .await
            .unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }

    /// The result of a browser RPC that must succeed.
    async fn ok(&self, b: &Browser, csrf: &str, op: &str, body: Value) -> Value {
        let (status, v) = self.rpc(b, csrf, op, body).await;
        assert_eq!(status, StatusCode::OK, "{op}: {v}");
        assert_eq!(v["ok"], true, "{op}: {v}");
        v["result"].clone()
    }

    /// A principal of `user` on `host`, created on first sight.
    async fn principal(&self, user: &str, host: &str) -> Principal {
        let (d, user, host) = (self.d.clone(), user.to_string(), host.to_string());
        blocking(move || d.principal(&user, &host, false).unwrap()).await
    }

    /// One op through the dispatcher as `actor` (a CLI or bridge request).
    async fn op(&self, p: &Principal, actor: &str, op: &str, body: Value) -> Value {
        let (d, p) = (self.d.clone(), p.clone());
        let mut req = Request::new(conde::util::new_ulid(), op, body);
        req.actor = Some(actor.to_string());
        req.idempotency_key = Some(conde::util::new_ulid());
        let resp = blocking(move || d.dispatch(&p, &req)).await;
        assert!(resp.ok, "{op}: {:?}", resp.error);
        resp.result.unwrap()
    }
}

// --- the app shell ------------------------------------------------------------------------------

async fn the_root_serves_the_app_to_a_signed_in_browser_only(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let b = Browser::new();
    let r = b.step.get(ui.env.url("/")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/auth/login?next=%2F");
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    // Sign-in returns to the app.
    let (status, at, body) = b.get(&ui.env.url("/")).await;
    assert_eq!(
        (status, at.as_str()),
        (StatusCode::OK, ui.env.url("/").as_str()),
        "{body}"
    );
    assert!(body.starts_with("<!doctype html>"), "{body}");
    let r = b.step.get(ui.env.url("/")).send().await.unwrap();
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(r.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(
        b.step
            .get(ui.env.url("/app/nope.js"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    if conde_remote::web::app_embedded() {
        assert!(body.contains("/app/immutable/"), "{body}");
        let r = b
            .step
            .get(ui.env.url("/app/version.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-cache");
    }
    // The keys page stays a server page, in the app's shell with the app's icons: its masthead
    // lists the user's own agents from the board, Keys current, the user menu at the foot.
    let k = ui.principal(USER, "k-host").await;
    ui.op(
        &k,
        "claude:sess-worker",
        "agent.register",
        json!({"name": "worker"}),
    )
    .await;
    let dv = ui.principal("david", "d-host").await;
    ui.op(
        &dv,
        "codex:thread-scout",
        "agent.register",
        json!({"name": "scout"}),
    )
    .await;
    let (status, _, keys) = b.get(&ui.env.url("/keys")).await;
    assert_eq!(status, StatusCode::OK);
    for part in [
        r#"<a href="/keys" class="on" aria-current="page">Keys</a>"#,
        r#"<span class="name">worker</span>"#,
        "You see what your agent can read.",
        r#"<form method="post" action="/auth/logout">"#,
    ] {
        assert!(keys.contains(part), "{part} missing: {keys}");
    }
    assert!(
        !keys.contains("scout") && !keys.contains("<header>"),
        "{keys}"
    );
    assert!(
        keys.contains(r#"<link rel="icon" href="/app/favicon.svg" type="image/svg+xml">"#),
        "{keys}"
    );
    let icon = b.step.get(ui.env.url("/favicon.ico")).send().await.unwrap();
    if icons_built() {
        assert!(
            keys.contains(r#"<a class="word" href="/#/"><svg "#),
            "{keys}"
        );
        assert_eq!(icon.status(), StatusCode::OK);
        assert_eq!(icon.headers()[header::CONTENT_TYPE], "image/png");
        let r = b
            .step
            .get(ui.env.url("/app/favicon.svg"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()[header::CONTENT_TYPE], "image/svg+xml");
    } else {
        assert_eq!(icon.status(), StatusCode::NOT_FOUND);
    }
    ui.server.shutdown().await.unwrap();
}

/// The built app's own calls against the real ops, with the fields the client reads: the shell
/// and its files, the shell's reads, the Feed's first page, a thread opened on one of its rows,
/// the problems filter and search.
async fn the_built_app_reads_through_the_real_ops(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;

    let (status, _, html) = b.get(&ui.env.url("/")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    if conde_remote::web::app_embedded() {
        let files: HashSet<&str> = html
            .match_indices("\"/app/")
            .map(|(at, _)| {
                let rest = &html[at + 1..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert!(files.iter().any(|f| f.ends_with(".js")), "{html}");
        for f in files {
            let r = b.step.get(ui.env.url(f)).send().await.unwrap();
            assert_eq!(r.status(), StatusCode::OK, "{f}");
        }
    } else {
        eprintln!("no web build embedded: the shell's files are not checked");
    }

    let rail = ui.ok(&b, &csrf, "owner.list", json!({})).await;
    assert_eq!(rail["owner"], USER);
    let worker = rail["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == format!("{USER}/worker"))
        .expect("worker in the rail");
    assert!(
        worker["dms"].is_array() && worker["channels"].is_array(),
        "{worker}"
    );
    assert!(worker["channels"][0]["channel"].is_string(), "{worker}");
    let (dd, user) = (ui.d.clone(), c.k.user_id);
    blocking(move || dd.with_tx(|t| Ok(t.set_user_admin(user, true)?)).unwrap()).await;
    let fleet = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert!(
        fleet["now"].is_string() && fleet["online_ttl_s"].is_number(),
        "{fleet}"
    );
    assert_eq!(fleet["admins"], json!([USER]), "the admin mark's source");
    for h in fleet["hosts"].as_array().unwrap() {
        assert!(
            h["host"].is_string() && h["owner"].is_string() && h["online"].is_boolean(),
            "{h}"
        );
    }
    for a in fleet["agents"].as_array().unwrap() {
        assert!(
            a["name"].is_string() && a["owner"].is_string() && a["host"].is_string(),
            "{a}"
        );
    }

    // a held DM, for the problems filter
    let mut agent_id = HashMap::new();
    for p in [&c.k, &c.dv] {
        let (d, user) = (ui.d.clone(), p.user_id);
        for a in blocking(move || d.with_conn(|t| t.owner_agents(user)).unwrap()).await {
            agent_id.insert(a.canonical(), a.id);
        }
    }
    let held = ui
        .op(
            &c.k,
            &c.worker,
            "message.send",
            json!({"to": {"agent": "david/scout"}, "text": "held for the smoke"}),
        )
        .await["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    let notification = format!("{held}.{}", agent_id["david/scout"]);
    let dd = ui.d.clone();
    blocking(move || {
        dd.with_tx(|t| Ok(t.set_notification_status(&notification, "held")?))
            .unwrap()
    })
    .await;

    // the Feed's first page, as the client asks for it with no filter
    let feed = ui.ok(&b, &csrf, "owner.feed", json!({})).await;
    assert!(feed["more"].is_boolean(), "{feed}");
    let rows = feed["messages"].as_array().unwrap();
    assert!(!rows.is_empty(), "{feed}");
    for m in rows {
        for k in ["message_id", "place", "from", "to", "date", "text"] {
            assert!(m[k].is_string(), "{k}: {m}");
        }
        assert!(m["seq"].is_i64(), "{m}");
        assert!(m["reply_to"].is_null() || m["reply_to"].is_string(), "{m}");
        assert!(
            m["attachments"].is_array() && m["reactions"].is_array(),
            "{m}"
        );
        assert!(
            ["in", "out", "own"].contains(&m["direction"].as_str().unwrap()),
            "{m}"
        );
        assert!(
            m["via"]
                .as_array()
                .is_some_and(|v| !v.is_empty() && v.iter().all(Value::is_string)),
            "{m}"
        );
        if let Some(r) = m["delivery"]["recipients"].as_array() {
            assert!(
                r.iter()
                    .all(|r| r["recipient"].is_string() && r["state"].is_string()),
                "{m}"
            );
        }
    }
    let ids: Vec<&str> = rows
        .iter()
        .map(|m| m["message_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&c.worker_dm["message_id"].as_str().unwrap()),
        "{ids:?}"
    );
    assert!(
        !ids.contains(&c.private_dm["message_id"].as_str().unwrap()),
        "{ids:?}"
    );

    // a thread opened on one of its rows
    let id = c.worker_dm["message_id"].as_str().unwrap();
    let t = ui.ok(&b, &csrf, "owner.thread", json!({"id": id})).await;
    assert!(
        t["top"].is_string() && t["hidden_parent"].is_boolean(),
        "{t}"
    );
    assert!(t["start"].is_null() || t["start"].is_string(), "{t}");
    assert!(t["next"].is_null() || t["next"].is_i64(), "{t}");
    assert!(
        t["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["message_id"] == id),
        "{t}"
    );

    // needs attention
    let p = ui
        .ok(&b, &csrf, "owner.feed", json!({"problems": true}))
        .await;
    assert_eq!(p["more"], false, "{p}");
    let problems = p["messages"].as_array().unwrap();
    assert!(
        problems.iter().any(|m| m["message_id"] == held.as_str()),
        "{p}"
    );
    for m in problems {
        let states: Vec<&str> = m["delivery"]["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["state"].as_str().unwrap())
            .collect();
        assert!(
            states
                .iter()
                .any(|s| ["retrying", "held", "denied", "refused", "gone"].contains(s)),
            "{m}"
        );
    }

    // search
    let s = ui
        .ok(
            &b,
            &csrf,
            "owner.search",
            json!({"pattern": "report|private"}),
        )
        .await;
    assert!(s.get("next_cursor").is_some(), "{s}");
    let hits = s["hits"].as_array().unwrap();
    for h in hits {
        for k in ["place", "message_id", "from", "date", "line"] {
            assert!(h[k].is_string(), "{k}: {h}");
        }
        assert!(h["seq"].is_i64(), "{h}");
    }
    assert!(
        hits.iter()
            .any(|h| h["message_id"] == c.worker_dm["message_id"]),
        "{s}"
    );
    assert!(
        hits.iter()
            .all(|h| h["message_id"] != c.private_dm["message_id"]),
        "{s}"
    );
    ui.server.shutdown().await.unwrap();
}

// --- the board as the tests set it up ------------------------------------------------------------

/// The signed-in user's agents `worker` (Claude, k-host) and `helper` (Codex, k-host); david's
/// `scout` (Codex) and `ranger` (Claude) on d-host. DMs: scout<->worker (with an attachment),
/// helper->worker, and scout->ranger (between david's agents, with an attachment). Channels:
/// #team (worker and scout) and #secret (scout and ranger).
struct Cast {
    k: Principal,
    worker: String,
    helper: String,
    dv: Principal,
    scout: String,
    ranger: String,
    /// worker -> scout, with an attachment.
    worker_dm: Value,
    /// scout -> ranger, with an attachment.
    private_dm: Value,
    team_post: Value,
    secret_post: Value,
}

async fn cast(ui: &Ui) -> Cast {
    let k = ui.principal(USER, "k-host").await;
    let (worker, helper) = (
        "claude:sess-worker".to_string(),
        "codex:thread-helper".to_string(),
    );
    ui.op(&k, &worker, "agent.register", json!({"name": "worker"}))
        .await;
    ui.op(&k, &helper, "agent.register", json!({"name": "helper"}))
        .await;
    let dv = ui.principal("david", "d-host").await;
    let (scout, ranger) = (
        "codex:thread-scout".to_string(),
        "claude:sess-ranger".to_string(),
    );
    ui.op(&dv, &scout, "agent.register", json!({"name": "scout"}))
        .await;
    ui.op(&dv, &ranger, "agent.register", json!({"name": "ranger"}))
        .await;
    let file = |name: &str| json!([{"filename": name, "data": STANDARD.encode(format!("bytes of {name}"))}]);

    ui.op(
        &dv,
        &scout,
        "message.send",
        json!({"to": {"agent": format!("{USER}/worker")}, "text": "status?"}),
    )
    .await;
    let worker_dm = ui
        .op(&k, &worker, "message.send",
            json!({"to": {"agent": "david/scout"}, "text": "green, report attached", "attachments": file("report.txt")}))
        .await;
    ui.op(
        &k,
        &helper,
        "message.send",
        json!({"to": {"agent": "worker"}, "text": "handoff to worker"}),
    )
    .await;
    let private_dm = ui
        .op(&dv, &scout, "message.send",
            json!({"to": {"agent": "ranger"}, "text": "private between david's agents", "attachments": file("secret.txt")}))
        .await;
    ui.op(
        &k,
        &worker,
        "channel.create",
        json!({"channel": "#team", "topic": "daily status"}),
    )
    .await;
    ui.op(&dv, &scout, "channel.join", json!({"channel": "#team"}))
        .await;
    let team_post = ui
        .op(
            &dv,
            &scout,
            "message.send",
            json!({"to": {"channel": "#team"}, "text": "scout here"}),
        )
        .await;
    ui.op(&dv, &scout, "channel.create", json!({"channel": "#secret"}))
        .await;
    ui.op(&dv, &ranger, "channel.join", json!({"channel": "#secret"}))
        .await;
    let secret_post = ui
        .op(
            &dv,
            &ranger,
            "message.send",
            json!({"to": {"channel": "#secret"}, "text": "members only"}),
        )
        .await;
    Cast {
        k,
        worker,
        helper,
        dv,
        scout,
        ranger,
        worker_dm,
        private_dm,
        team_post,
        secret_post,
    }
}

fn texts(h: &Value) -> Vec<String> {
    h["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect()
}

// --- the browser principal ----------------------------------------------------------------------

async fn browser_rpc_needs_the_session_its_csrf_token_and_this_origin(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let request =
        json!({"v": 1, "type": "request", "request_id": "r", "op": "agent.list", "body": {}});
    let rpc = ui.env.url("/v1/rpc");

    let anon = reqwest::Client::new();
    let r = anon.post(&rpc).json(&request).send().await.unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "no bearer and no session"
    );
    let r = anon.get(ui.env.url("/v1/session")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let (b, csrf) = ui.browser().await;
    let s = ui.session(&b).await;
    assert_eq!(s["user"], USER);
    assert_eq!(s["csrf_header"], "x-csrf-token");

    let r = b.step.post(&rpc).json(&request).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "no CSRF token");
    let r = b
        .step
        .post(&rpc)
        .header("x-csrf-token", "0".repeat(64))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "a wrong CSRF token");
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["error"]["code"], "stale_session",
        "the app reloads its session on this code: {v}"
    );
    let r = b
        .step
        .post(&rpc)
        .header("x-csrf-token", &csrf)
        .header(header::ORIGIN, "https://evil.example")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "another origin");
    let v: Value = b
        .step
        .post(&rpc)
        .header("x-csrf-token", &csrf)
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (v["ok"].as_bool(), v["request_id"].as_str()),
        (Some(true), Some("r")),
        "{v}"
    );

    // A signed-out session is refused at once.
    let r = b
        .post_form(&ui.env.url("/auth/logout"), &[("csrf", csrf.as_str())])
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let (status, _) = ui.rpc(&b, &csrf, "agent.list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    ui.server.shutdown().await.unwrap();
}

async fn the_browser_is_read_only_and_names_nobody(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let bound = |d: Arc<Dispatcher>| async move {
        blocking(move || d.with_conn(|t| t.bound_agents()).unwrap().len()).await
    };
    let before = bound(ui.d.clone()).await;
    for (op, body) in [
        (
            "message.send",
            json!({"to": {"agent": "david/scout"}, "text": "x"}),
        ),
        ("message.read", json!({})),
        (
            "reaction.set",
            json!({"message_id": c.worker_dm["message_id"], "emoji": "+1", "present": true}),
        ),
        ("channel.join", json!({"channel": "#secret"})),
        ("channel.create", json!({"channel": "#new"})),
        ("agent.register", json!({"name": "x"})),
        ("agent.whoami", json!({})),
        (
            "profile.set",
            json!({"name": "worker", "key": "display", "value": "W"}),
        ),
        ("message.get", json!({"id": c.private_dm["message_id"]})),
        ("attachment.get", json!({"attachment_id": "x"})),
        ("search", json!({"pattern": "."})),
        ("inbox.list", json!({})),
        ("bridge.claim", json!({})),
    ] {
        let (status, v) = ui.rpc(&b, &csrf, op, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{op}: {v}");
    }
    ui.ok(&b, &csrf, "owner.list", json!({})).await;
    ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert!(
        ui.session(&b).await.get("name").is_none(),
        "no browser identity"
    );
    assert_eq!(
        bound(ui.d.clone()).await,
        before,
        "no request names or binds anything"
    );
    let d2 = ui.d.clone();
    let web = blocking(move || d2.with_conn(|t| t.bound_agents()).unwrap()).await;
    // only each owner's human name is bound on the web host, never by a browser request
    let on_web = |a: &&conde::store::Agent| a.address.as_ref().is_some_and(|x| x.host == "web");
    assert!(
        web.iter()
            .filter(on_web)
            .all(|a| a.is_human() && a.name == "human"),
        "{web:?}"
    );
    ui.server.shutdown().await.unwrap();
}

// --- the owner view -----------------------------------------------------------------------------

async fn the_owner_sees_what_their_agents_can_read_and_nothing_else(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;

    let rail = ui.ok(&b, &csrf, "owner.list", json!({})).await;
    assert_eq!(rail["owner"], USER);
    let agents = rail["agents"].as_array().unwrap();
    let names: Vec<&str> = agents.iter().map(|a| a["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            format!("{USER}/helper"),
            format!("{USER}/human"),
            format!("{USER}/worker")
        ]
    );
    assert_eq!(
        agents[1]["address"],
        format!("human:{USER}@web"),
        "the owner's human name is in the rail"
    );
    let worker = &agents[2];
    let partners: Vec<&str> = worker["dms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["with"].as_str().unwrap())
        .collect();
    assert_eq!(
        partners,
        vec![format!("{USER}/helper").as_str(), "david/scout"],
        "newest first: {worker}"
    );
    let scout_thread = &worker["dms"][1];
    assert_eq!(
        scout_thread["unread"], 1,
        "scout's question is unread by worker: {worker}"
    );
    assert_eq!(
        worker["channels"],
        json!([{"channel": "#team", "topic": "daily status", "last_seq": c.team_post["seq"],
        "last_at": worker["channels"][0]["last_at"], "unread": 2, "notify": "mentions"}]),
        "scout's join notice and post"
    );
    let helper = &agents[0];
    assert_eq!(helper["dms"][0]["with"], format!("{USER}/worker"));
    assert_eq!(helper["channels"], json!([]));
    let all = rail.to_string();
    for hidden in ["ranger", "#secret", "private between", "members only"] {
        assert!(!all.contains(hidden), "{hidden} leaked: {all}");
    }

    // A thread of an own agent with another owner's agent, in full and in order.
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/scout"}),
        )
        .await;
    assert_eq!(texts(&h), vec!["status?", "green, report attached"]);
    assert_eq!(h["agent"], format!("{USER}/worker"));
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "helper", "with": "worker"}),
        )
        .await;
    assert_eq!(texts(&h), vec!["handoff to worker"]);
    // Never a DM between two other owners' agents, whichever way it is asked for.
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/ranger"}),
        )
        .await;
    assert!(texts(&h).is_empty(), "{h}");
    for body in [
        json!({"agent": "david/scout", "with": "david/ranger"}),
        json!({"agent": "scout", "with": "david/ranger"}),
    ] {
        let (_, v) = ui.rpc(&b, &csrf, "owner.history", body.clone()).await;
        assert!(
            ["forbidden", "not_found"].contains(&v["error"]["code"].as_str().unwrap()),
            "{body}: {v}"
        );
    }
    // Channels by an own agent's membership only.
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "channel": "#team"}),
        )
        .await;
    assert_eq!(texts(&h).last().map(String::as_str), Some("scout here"));
    for body in [
        json!({"agent": "worker", "channel": "#secret"}),
        json!({"agent": "helper", "channel": "#team"}),
    ] {
        let (_, v) = ui.rpc(&b, &csrf, "owner.history", body.clone()).await;
        assert_eq!(v["error"]["code"], "forbidden", "{body}: {v}");
    }
    // One message (reply quotes) under the same rule.
    let m = ui
        .ok(
            &b,
            &csrf,
            "owner.message",
            json!({"id": c.team_post["message_id"]}),
        )
        .await;
    assert_eq!(m["message"]["text"], "scout here");
    for id in [&c.private_dm["message_id"], &c.secret_post["message_id"]] {
        let (_, v) = ui.rpc(&b, &csrf, "owner.message", json!({"id": id})).await;
        assert_eq!(v["error"]["code"], "not_found", "{v}");
    }
    // The owner view is the owner's: an agent of the same owner reads only its own conversations.
    let (d2, mut req) = (
        ui.d.clone(),
        Request::new(
            "r",
            "owner.history",
            json!({"agent": "worker", "with": "david/scout"}),
        ),
    );
    req.actor = Some(c.helper.clone());
    let k = c.k.clone();
    let resp = blocking(move || d2.dispatch(&k, &req)).await;
    assert_eq!(
        resp.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden)
    );
    let _ = &c.ranger;
    ui.server.shutdown().await.unwrap();
}

/// `owner.search` from the browser: hits from the owner's agents' DMs and channels, never a DM
/// between two other owners' agents or a channel none of them is in. The other names the
/// allow-list adds pass its gate; an op left off the list is refused before the dispatcher.
async fn the_browser_searches_what_its_agents_can_read(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let _ = cast(&ui).await;
    let r = ui
        .ok(&b, &csrf, "owner.search", json!({"pattern": "."}))
        .await;
    let lines: Vec<&str> = r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["line"].as_str().unwrap())
        .collect();
    for seen in [
        "status?",
        "green, report attached",
        "handoff to worker",
        "scout here",
    ] {
        assert!(lines.contains(&seen), "{seen} missing: {r}");
    }
    let all = r.to_string();
    for hidden in ["private between", "members only"] {
        assert!(!all.contains(hidden), "{hidden} leaked: {all}");
    }
    ui.ok(&b, &csrf, "owner.feed", json!({})).await;
    let (status, v) = ui.rpc(&b, &csrf, "owner.thread", json!({})).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::OK, Some("bad_request")),
        "owner.thread answers: {v}"
    );
    for op in ["message.feed", "search", "owner.attachment"] {
        let (status, v) = ui.rpc(&b, &csrf, op, json!({"pattern": "."})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{op}: {v}");
        assert_eq!(
            v["error"]["message"],
            format!("'{op}' is not available in the browser"),
            "{v}"
        );
    }
    ui.server.shutdown().await.unwrap();
}

/// The texts of a feed page, newest first.
fn feed_texts(f: &Value) -> Vec<String> {
    texts(f)
}

/// The feed of the signed-in owner, drained page by page (`limit` per page) with the lower bound
/// fixed and the upper one moving down: every matching row above `after`.
async fn drain(
    ui: &Ui,
    b: &Browser,
    csrf: &str,
    mut body: Value,
    limit: i64,
) -> (Vec<Value>, usize) {
    body["limit"] = json!(limit);
    let (mut rows, mut pages) = (Vec::new(), 0);
    loop {
        let page = ui.ok(b, csrf, "owner.feed", body.clone()).await;
        pages += 1;
        let got = page["messages"].as_array().unwrap().clone();
        assert!(got.len() as i64 <= limit, "{page}");
        if let Some(last) = got.last() {
            body["before"] = last["seq"].clone();
        }
        rows.extend(got);
        if page["more"] == false {
            return (rows, pages);
        }
    }
}

/// `owner.feed` holds exactly what the owner's agents can read (`owner.search`'s rule), never a
/// DM between two other owners' agents, each row once with its direction and the own agents it
/// is read through; every predicate narrows it, alone and combined.
async fn the_feed_is_what_the_owners_agents_can_read(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    ui.op(
        &c.k,
        &c.worker,
        "message.send",
        json!({"to": {"channel": "#team"}, "text": "worker on team"}),
    )
    .await;
    // helper posts in #team and leaves: its post stays readable through worker
    ui.op(&c.k, &c.helper, "channel.join", json!({"channel": "#team"}))
        .await;
    ui.op(
        &c.k,
        &c.helper,
        "message.send",
        json!({"to": {"channel": "#team"}, "text": "helper was here"}),
    )
    .await;
    ui.op(
        &c.k,
        &c.helper,
        "channel.leave",
        json!({"channel": "#team"}),
    )
    .await;
    ui.op(
        &c.k,
        &c.worker,
        "message.send",
        json!({"to": {"agent": "helper"}, "text": "handoff back"}),
    )
    .await;
    let feed = |body: Value| ui.ok(&b, &csrf, "owner.feed", body);

    // A DM between two own agents sits in a sender and a recipient range: one row each, and
    // `more` exact after the merge (two such DMs: a page of one has more; merged without
    // dedupe, the newest one would fill the page twice and `more` would read false).
    let own = feed(json!({"direction": "own", "limit": 1})).await;
    assert_eq!(
        (feed_texts(&own), own["more"].as_bool()),
        (vec!["handoff back".to_string()], Some(true)),
        "{own}"
    );
    let own = feed(json!({"direction": "own", "limit": 2})).await;
    assert_eq!(
        (feed_texts(&own), own["more"].as_bool()),
        (
            vec!["handoff back".to_string(), "handoff to worker".to_string()],
            Some(false)
        ),
        "{own}"
    );

    // The whole feed, page by page: the owned set exactly, each message once, newest first.
    let (all, pages) = drain(&ui, &b, &csrf, json!({}), 2).await;
    assert!(pages > 2, "{pages} pages");
    let ids: Vec<&str> = all
        .iter()
        .map(|m| m["message_id"].as_str().unwrap())
        .collect();
    let seqs: Vec<i64> = all.iter().map(|m| m["seq"].as_i64().unwrap()).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] > w[1]),
        "newest first, no repeats: {seqs:?}"
    );
    let search = ui
        .ok(&b, &csrf, "owner.search", json!({"pattern": "."}))
        .await;
    let mut owned: Vec<&str> = search["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["message_id"].as_str().unwrap())
        .collect();
    let mut got = ids.clone();
    owned.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, owned, "the feed is the owned set");
    let text_of = |m: &Value| m["text"].as_str().unwrap().to_string();
    let all_texts: Vec<String> = all.iter().map(text_of).collect();
    for hidden in ["private between david's agents", "members only"] {
        assert!(!all_texts.iter().any(|t| t == hidden), "{hidden} leaked");
    }
    let row = |text: &str| {
        all.iter()
            .find(|m| m["text"] == text)
            .unwrap_or_else(|| panic!("{text} missing"))
            .clone()
    };
    let (worker, helper) = (format!("{USER}/worker"), format!("{USER}/helper"));
    for (text, direction, via) in [
        ("status?", "in", vec![worker.clone()]),
        ("green, report attached", "out", vec![worker.clone()]),
        (
            "handoff to worker",
            "own",
            vec![helper.clone(), worker.clone()],
        ),
        ("handoff back", "own", vec![worker.clone(), helper.clone()]),
        ("scout here", "in", vec![worker.clone()]),
        ("worker on team", "out", vec![worker.clone()]),
        ("helper was here", "out", vec![worker.clone()]),
    ] {
        let r = row(text);
        assert_eq!(
            (r["direction"].as_str(), r["via"].clone()),
            (Some(direction), json!(via)),
            "{r}"
        );
        assert!(
            r["delivery"].is_object() && r["place"].is_string(),
            "the history view: {r}"
        );
    }

    // Each predicate alone.
    let set = |f: &Value| -> Vec<String> {
        let mut t = feed_texts(f);
        t.sort();
        t
    };
    let sorted = |v: &[&str]| -> Vec<String> {
        let mut t: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        t.sort();
        t
    };
    let dm = feed(json!({"place": "dm"})).await;
    assert_eq!(
        set(&dm),
        sorted(&[
            "status?",
            "green, report attached",
            "handoff to worker",
            "handoff back"
        ])
    );
    let team = feed(json!({"place": "#team", "limit": 100})).await;
    assert!(
        team["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["place"] == "#team"),
        "{team}"
    );
    assert!(
        set(&team).contains(&"scout here".to_string())
            && set(&team).contains(&"helper was here".to_string())
    );
    let (_, v) = ui
        .rpc(&b, &csrf, "owner.feed", json!({"place": "#secret"}))
        .await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    let out = feed(json!({"direction": "out", "limit": 100})).await;
    assert!(
        out["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["direction"] == "out"),
        "{out}"
    );
    for t in [
        "green, report attached",
        "worker on team",
        "helper was here",
    ] {
        assert!(set(&out).contains(&t.to_string()), "{t}: {out}");
    }
    let inbound = feed(json!({"direction": "in", "limit": 100})).await;
    assert!(
        inbound["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["direction"] == "in" && !m["from"].as_str().unwrap().starts_with(USER)),
        "{inbound}"
    );
    for t in ["status?", "scout here"] {
        assert!(set(&inbound).contains(&t.to_string()), "{t}: {inbound}");
    }
    // agent: sender, DM recipient or current member; helper left #team, so only its own post.
    let h = feed(json!({"agent": "helper", "limit": 100})).await;
    assert_eq!(
        set(&h),
        sorted(&[
            "@krisztian-dobmayer/helper joined #team.",
            "handoff to worker",
            "handoff back",
            "helper was here"
        ]),
        "{h}"
    );
    let w = feed(json!({"agent": "worker", "limit": 100})).await;
    assert_eq!(
        set(&w).len(),
        set(&feed(json!({"limit": 100})).await).len(),
        "worker reads all of it: {w}"
    );
    let (_, v) = ui
        .rpc(&b, &csrf, "owner.feed", json!({"agent": "david/scout"}))
        .await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    // Combined: another own agent's post in worker's channel; helper's own post there; worker's DMs out.
    let combined = feed(json!({"agent": "worker", "direction": "out", "place": "#team"})).await;
    assert_eq!(
        set(&combined)
            .iter()
            .filter(|t| !t.starts_with('@'))
            .cloned()
            .collect::<Vec<_>>(),
        sorted(&["helper was here", "worker on team"]),
        "{combined}"
    );
    let combined = feed(json!({"agent": "helper", "direction": "out", "place": "#team"})).await;
    assert_eq!(
        feed_texts(&combined),
        vec![
            "helper was here",
            "@krisztian-dobmayer/helper joined #team."
        ],
        "{combined}"
    );
    let combined = feed(json!({"agent": "worker", "direction": "out", "place": "dm"})).await;
    assert_eq!(
        feed_texts(&combined),
        vec!["green, report attached"],
        "{combined}"
    );
    let combined = feed(json!({"agent": "worker", "direction": "own", "place": "dm"})).await;
    assert_eq!(
        feed_texts(&combined),
        vec!["handoff back", "handoff to worker"],
        "{combined}"
    );

    // A teammate signed in: an owner with no agents reads nothing; with one, never a DM between
    // two other owners' agents (G9).
    ui.env.invite("dpacsuta", "dpacsuta@dakai.example").await;
    ui.env.idp.person.lock().unwrap().oid = "oid-dp".into();
    ui.env.idp.person.lock().unwrap().upn = "dpacsuta@dakai.example".into();
    let (b2, csrf2) = ui.browser().await;
    let empty = ui.ok(&b2, &csrf2, "owner.feed", json!({})).await;
    assert_eq!(empty, json!({"messages": [], "more": false}));
    let p = ui.principal("dpacsuta", "p-host").await;
    ui.op(
        &p,
        "codex:thread-probe",
        "agent.register",
        json!({"name": "probe"}),
    )
    .await;
    ui.op(
        &p,
        "codex:thread-probe",
        "message.send",
        json!({"to": {"agent": format!("{USER}/worker")}, "text": "hello worker"}),
    )
    .await;
    ui.op(
        &p,
        "codex:thread-probe",
        "channel.join",
        json!({"channel": "#team"}),
    )
    .await;
    let theirs = ui
        .ok(&b2, &csrf2, "owner.feed", json!({"limit": 100}))
        .await;
    let theirs_texts = feed_texts(&theirs);
    assert!(
        theirs_texts.contains(&"hello worker".to_string())
            && theirs_texts.contains(&"scout here".to_string()),
        "{theirs}"
    );
    for hidden in [
        "private between david's agents",
        "members only",
        "handoff to worker",
        "status?",
        "green, report attached",
    ] {
        assert!(
            !theirs_texts.iter().any(|t| t == hidden),
            "{hidden} leaked: {theirs}"
        );
    }
    ui.server.shutdown().await.unwrap();
}

/// Both bounds exclusive and `more` exact; a drain with the lower bound fixed returns exactly
/// the rows above it; a row committed between two passes is in the next pass from the same bound.
async fn the_feed_pages_between_exclusive_bounds(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let k = ui.principal(USER, "k-host").await;
    let dv = ui.principal("david", "d-host").await;
    ui.op(&k, "codex:thread-w", "agent.register", json!({"name": "w"}))
        .await;
    ui.op(
        &dv,
        "codex:thread-x",
        "agent.register",
        json!({"name": "x"}),
    )
    .await;
    let send = |text: String| {
        ui.op(
            &dv,
            "codex:thread-x",
            "message.send",
            json!({"to": {"agent": format!("{USER}/w")}, "text": text}),
        )
    };
    let mut seq = vec![0];
    for n in 1..=7 {
        seq.push(send(format!("m{n}")).await["seq"].as_i64().unwrap());
    }
    let feed = |body: Value| ui.ok(&b, &csrf, "owner.feed", body);
    let names = |v: &[usize]| v.iter().map(|n| format!("m{n}")).collect::<Vec<_>>();
    let page = feed(json!({"limit": 3})).await;
    assert_eq!(
        (feed_texts(&page), page["more"].as_bool()),
        (names(&[7, 6, 5]), Some(true))
    );
    let page = feed(json!({"after": seq[2], "before": seq[6]})).await;
    assert_eq!(
        (feed_texts(&page), page["more"].as_bool()),
        (names(&[5, 4, 3]), Some(false)),
        "both bounds exclusive"
    );
    let page = feed(json!({"after": seq[2], "before": seq[6], "limit": 3})).await;
    assert_eq!(page["more"], false, "exactly the page: {page}");
    let page = feed(json!({"after": seq[2], "before": seq[6], "limit": 2})).await;
    assert_eq!(
        (feed_texts(&page), page["more"].as_bool()),
        (names(&[5, 4]), Some(true))
    );
    let page = feed(json!({"after": seq[3], "before": seq[4]})).await;
    assert_eq!(
        page,
        json!({"messages": [], "more": false}),
        "nothing strictly between neighbours"
    );

    let (rows, pages) = drain(&ui, &b, &csrf, json!({"after": seq[1]}), 2).await;
    let got: Vec<String> = rows
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        (got, pages),
        (names(&[7, 6, 5, 4, 3, 2]), 3),
        "the rows above the bound, in three pages"
    );

    let first = feed(json!({"after": seq[5]})).await;
    assert_eq!(feed_texts(&first), names(&[7, 6]));
    send("m8".into()).await;
    let next = feed(json!({"after": seq[5]})).await;
    assert_eq!(
        (feed_texts(&next), next["more"].as_bool()),
        (names(&[8, 7, 6]), Some(false)),
        "the new row is in the next pass"
    );
    ui.server.shutdown().await.unwrap();
}

/// `owner.feed {problems}`: every problem state a recipient the owner may judge can be in, never a
/// read message or another owner's recipient's problem; two problems far apart on one page each,
/// never an empty page with `more`; no problems an empty page without `more`.
async fn the_feed_lists_delivery_problems(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let ids = |p: &Principal| {
        let (d, user) = (ui.d.clone(), p.user_id);
        async move { blocking(move || d.with_conn(|t| t.owner_agents(user)).unwrap()).await }
    };
    let mut agent_id = std::collections::HashMap::new();
    for a in ids(&c.k).await.into_iter().chain(ids(&c.dv).await) {
        agent_id.insert(a.canonical(), a.id);
    }
    let (u, agent_id) = (&ui, &agent_id);
    let send = |p: &Principal, actor: &str, to: Value, text: &str| {
        let body = json!({"to": to, "text": text});
        let (p, actor) = (p.clone(), actor.to_string());
        async move {
            u.op(&p, &actor, "message.send", body).await["message_id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let set = |message: &str, recipient: &str, status: &str| {
        let (d, id, status) = (
            u.d.clone(),
            format!("{message}.{}", agent_id[recipient]),
            status.to_string(),
        );
        async move {
            blocking(move || {
                d.with_tx(|t| Ok(t.set_notification_status(&id, &status)?))
                    .unwrap()
            })
            .await
        }
    };
    let feed = |body: Value| ui.ok(&b, &csrf, "owner.feed", body);
    let scout = json!({"agent": "david/scout"});

    // retrying: scout's bridge claims it and the hold expires
    let retry = send(&c.k, &c.worker, scout.clone(), "retrying one").await;
    let bridge = ui.d.bridge_principal(&c.dv);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let job = claim["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["message_id"] == retry.as_str())
        .unwrap()
        .clone();
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": job["attempt_id"], "outcome": "expired", "reason": "no answer"}),
    )
    .await;
    let first = send(&c.k, &c.worker, scout.clone(), "held one").await;
    set(&first, "david/scout", "held").await;
    for n in 0..30 {
        send(&c.k, &c.worker, scout.clone(), &format!("fine {n}")).await;
    }
    let denied = send(&c.k, &c.worker, scout.clone(), "denied one").await;
    set(&denied, "david/scout", "denied").await;
    let refused = send(&c.k, &c.helper, json!({"agent": "worker"}), "refused one").await;
    set(&refused, &format!("{USER}/worker"), "refused").await;
    let gone = send(
        &c.k,
        &c.worker,
        json!({"agent": "david/ranger"}),
        "gone one",
    )
    .await;
    set(&gone, "david/ranger", "gone").await;
    // not problems: a hold the recipient read; another owner's recipient of another owner's post
    let read = send(&c.k, &c.worker, scout.clone(), "held then read").await;
    set(&read, "david/scout", "held").await;
    ui.op(&c.dv, &c.scout, "message.read", json!({"id": read}))
        .await;
    let theirs = send(
        &c.dv,
        &c.scout,
        json!({"channel": "#team"}),
        "@ranger see this",
    )
    .await;
    set(&theirs, "david/ranger", "denied").await;
    // a hold still on the notification, beside the recipient's read marker: read wins
    let marked = send(&c.k, &c.worker, scout.clone(), "held beside a read marker").await;
    set(&marked, "david/scout", "held").await;
    let (d, m, r) = (u.d.clone(), marked.clone(), agent_id["david/scout"].clone());
    blocking(move || {
        d.with_tx(|t| Ok(t.mark_dm_read(&m, &r, "2026-10-14T12:00:00.000Z")?))
            .unwrap()
    })
    .await;

    let all = feed(json!({"problems": true})).await;
    let states: Vec<(String, String)> = all["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let worst = m["delivery"]["recipients"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["state"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join(",");
            (m["text"].as_str().unwrap().to_string(), worst)
        })
        .collect();
    let want: Vec<(String, String)> = [
        ("gone one", "gone"),
        ("refused one", "refused"),
        ("denied one", "denied"),
        ("held one", "held"),
        ("retrying one", "retrying"),
    ]
    .iter()
    .map(|(t, s)| (t.to_string(), s.to_string()))
    .collect();
    assert_eq!(
        (states, all["more"].as_bool()),
        (want, Some(false)),
        "{all}"
    );

    // Thirty fine messages between two problems: a page each, never an empty one with `more`.
    let one = feed(json!({"problems": true, "place": "dm", "agent": "worker", "direction": "out", "after": 0, "before": i64::MAX, "limit": 1})).await;
    assert_eq!(
        (feed_texts(&one), one["more"].as_bool()),
        (vec!["gone one".to_string()], Some(true)),
        "{one}"
    );
    let mut before = one["messages"][0]["seq"].as_i64().unwrap();
    let mut seen = Vec::new();
    loop {
        let page =
            feed(json!({"problems": true, "direction": "out", "before": before, "limit": 1})).await;
        let got = feed_texts(&page);
        assert!(
            !got.is_empty() || page["more"] == false,
            "an empty page with more: {page}"
        );
        seen.extend(got);
        match page["messages"].as_array().unwrap().last() {
            Some(m) if page["more"] == true => before = m["seq"].as_i64().unwrap(),
            _ => break,
        }
    }
    assert_eq!(
        seen,
        vec!["denied one", "held one", "retrying one"],
        "out, page by page"
    );
    let helper = feed(json!({"problems": true, "agent": "helper"})).await;
    assert_eq!(feed_texts(&helper), vec!["refused one"], "{helper}");
    let team = feed(json!({"problems": true, "place": "#team"})).await;
    assert_eq!(
        team,
        json!({"messages": [], "more": false}),
        "scout's post: only another owner's recipient has a problem"
    );

    // An owner without problems: an empty page, no `more`.
    ui.env.invite("dpacsuta", "dpacsuta@dakai.example").await;
    ui.env.idp.person.lock().unwrap().oid = "oid-dp".into();
    ui.env.idp.person.lock().unwrap().upn = "dpacsuta@dakai.example".into();
    let (b2, csrf2) = ui.browser().await;
    let p = ui.principal("dpacsuta", "p-host").await;
    ui.op(
        &p,
        "codex:thread-probe",
        "agent.register",
        json!({"name": "probe"}),
    )
    .await;
    ui.op(
        &p,
        "codex:thread-probe",
        "message.send",
        json!({"to": {"agent": format!("{USER}/worker")}, "text": "hello"}),
    )
    .await;
    let none = ui
        .ok(&b2, &csrf2, "owner.feed", json!({"problems": true}))
        .await;
    assert_eq!(none, json!({"messages": [], "more": false}));
    ui.server.shutdown().await.unwrap();
}

/// A retry pending to a recipient that has since been unregistered (no address any more) is a
/// problem in the feed, as the delivery view shows it.
async fn a_retry_to_an_unbound_recipient_is_a_problem(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    ui.op(
        &c.dv,
        "codex:thread-lost",
        "agent.register",
        json!({"name": "lost"}),
    )
    .await;
    let sent = ui
        .op(
            &c.k,
            &c.worker,
            "message.send",
            json!({"to": {"agent": "david/lost"}, "text": "to an agent that leaves"}),
        )
        .await;
    let bridge = ui.d.bridge_principal(&c.dv);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let job = claim["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["message_id"] == sent["message_id"])
        .unwrap()
        .clone();
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": job["attempt_id"], "outcome": "expired", "reason": "no answer"}),
    )
    .await;
    ui.op(
        &c.dv,
        "codex:thread-lost",
        "agent.unregister",
        json!({"name": "lost"}),
    )
    .await;

    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/lost"}),
        )
        .await;
    let shown =
        h["messages"].as_array().unwrap().last().unwrap()["delivery"]["recipients"][0]["state"]
            .clone();
    assert_eq!(shown, "retrying", "{h}");
    let f = ui
        .ok(&b, &csrf, "owner.feed", json!({"problems": true}))
        .await;
    let row = f["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["message_id"] == sent["message_id"]);
    assert!(
        row.is_some_and(|m| m["delivery"]["recipients"][0]["state"] == shown),
        "the feed agrees with the delivery view: {f}"
    );
    ui.server.shutdown().await.unwrap();
}

/// The ids of an `owner.thread` answer, in order.
fn thread_ids(t: &Value) -> Vec<String> {
    t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message_id"].as_str().unwrap().to_string())
        .collect()
}

/// A ← B ← C ← D with B a DM between david's agents: the thread of C is C and D under a hidden
/// parent, the thread of A is A alone (B cuts it), and B itself is not found.
async fn a_thread_never_steps_through_a_message_the_owner_cannot_read(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let send = |p: &Principal, actor: &str, to: &str, text: &str, reply: Option<&str>| {
        let body = json!({"to": {"agent": to}, "text": text, "reply_to": reply});
        let (p, actor) = (p.clone(), actor.to_string());
        let u = &ui;
        async move {
            u.op(&p, &actor, "message.send", body).await["message_id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let a = send(&c.k, &c.worker, "david/scout", "A", None).await;
    let bb = send(&c.dv, &c.scout, "ranger", "B", Some(&a)).await;
    let cc = send(&c.dv, &c.scout, &format!("{USER}/worker"), "C", Some(&bb)).await;
    let dd = send(&c.k, &c.worker, "david/scout", "D", Some(&cc)).await;
    let thread = |id: &str| ui.ok(&b, &csrf, "owner.thread", json!({"id": id}));
    for id in [&cc, &dd] {
        let t = thread(id).await;
        assert_eq!(
            (
                thread_ids(&t),
                t["top"].as_str(),
                t["hidden_parent"].as_bool()
            ),
            (vec![cc.clone(), dd.clone()], Some(cc.as_str()), Some(true)),
            "{t}"
        );
        assert!(t["start"].is_null() && t["next"].is_null(), "{t}");
    }
    let t = thread(&a).await;
    assert_eq!(
        (thread_ids(&t), t["hidden_parent"].as_bool()),
        (vec![a.clone()], Some(false)),
        "B cuts the chain: {t}"
    );
    assert!(!t.to_string().contains("\"B\""), "{t}");
    let (_, v) = ui.rpc(&b, &csrf, "owner.thread", json!({"id": bb})).await;
    assert_eq!(v["error"]["code"], "not_found", "{v}");
    ui.server.shutdown().await.unwrap();
}

/// A chain of 250 opened at its 230th message starts at its 131st with `start` naming the first;
/// a path of 150 with 300 replies on a side branch, opened deep in the path, has the message in
/// the first answer, and after continuing and merging by id every subtree message is there once
/// with its parent present or being the top.
async fn a_long_thread_starts_near_the_message_and_continues_without_holes(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let mut n = 0;
    let mut send = |reply: Option<String>| {
        n += 1;
        let (actor, to) = if n % 2 == 0 {
            (c.worker.clone(), "helper")
        } else {
            (c.helper.clone(), "worker")
        };
        let body = json!({"to": {"agent": to}, "text": format!("t{n}"), "reply_to": reply});
        let (k, u) = (c.k.clone(), &ui);
        async move {
            u.op(&k, &actor, "message.send", body).await["message_id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let thread = |body: Value| ui.ok(&b, &csrf, "owner.thread", body);

    let mut chain: Vec<String> = Vec::new();
    for _ in 0..250 {
        let id = send(chain.last().cloned()).await;
        chain.push(id);
    }
    let t = thread(json!({"id": chain[229]})).await;
    assert_eq!(
        (t["top"].as_str(), t["start"].as_str()),
        (Some(chain[130].as_str()), Some(chain[0].as_str())),
        "{}",
        t["top"]
    );
    assert_eq!(thread_ids(&t), chain[130..].to_vec());
    assert!(t["next"].is_null() && t["hidden_parent"] == false);

    // P1..P60, then S1..S300 replying down from P60, then P61..P150
    let mut path: Vec<String> = Vec::new();
    for _ in 0..60 {
        let id = send(path.last().cloned()).await;
        path.push(id);
    }
    let mut side = vec![path[59].clone()];
    for _ in 0..300 {
        let id = send(side.last().cloned()).await;
        side.push(id);
    }
    for _ in 60..150 {
        let id = send(path.last().cloned()).await;
        path.push(id);
    }
    let first = thread(json!({"id": path[139]})).await;
    let top = first["top"].as_str().unwrap().to_string();
    assert_eq!(
        (top.as_str(), first["start"].as_str()),
        (path[40].as_str(), Some(path[0].as_str()))
    );
    let ids = thread_ids(&first);
    assert_eq!(ids.len(), 200);
    assert!(
        path[40..140].iter().all(|p| ids.contains(p)),
        "the whole path from the top to the message"
    );
    let mut seen: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut page = first;
    loop {
        for m in page["messages"].as_array().unwrap() {
            seen.insert(m["message_id"].as_str().unwrap().to_string(), m.clone());
        }
        match page["next"].as_i64() {
            Some(next) => page = thread(json!({"id": path[139], "after": next})).await,
            None => break,
        }
    }
    let subtree: std::collections::HashSet<&String> =
        path[40..].iter().chain(side[1..].iter()).collect();
    assert_eq!(seen.len(), subtree.len(), "every subtree message once");
    assert!(seen.keys().all(|id| subtree.contains(id)));
    for (id, m) in &seen {
        let parent = m["reply_to"].as_str().unwrap_or_default();
        assert!(
            *id == top || seen.contains_key(parent),
            "{id}'s parent {parent} is missing"
        );
    }
    ui.server.shutdown().await.unwrap();
}

/// B26: the human owner retires a name nothing holds: it leaves owner.list, fleet.list and the
/// directory, its history stays in the feed, search and thread, and registering it revives it.
/// A bound name, another owner's name and an agent's request are refused.
async fn a_retired_name_leaves_the_lists_and_keeps_its_history(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let human = "human:kd";
    let call = |actor: &str, op: &str, body: Value| {
        let (d, k, actor, op) = (ui.d.clone(), c.k.clone(), actor.to_string(), op.to_string());
        async move {
            let mut req = Request::new(conde::util::new_ulid(), &op, body);
            req.actor = Some(actor);
            req.idempotency_key = Some(conde::util::new_ulid());
            blocking(move || d.dispatch(&k, &req)).await
        }
    };
    let code = |r: conde::protocol::Response| r.error.map(|e| e.code);
    use conde::protocol::ErrorCode::{Forbidden, NotFound};
    assert_eq!(
        code(call(human, "agent.retire", json!({"name": "helper"})).await),
        Some(Forbidden),
        "bound"
    );
    assert_eq!(
        code(call(human, "agent.retire", json!({"name": "david/scout"})).await),
        Some(Forbidden),
        "not yours"
    );
    assert_eq!(
        code(call(human, "agent.retire", json!({"name": "nobody"})).await),
        Some(NotFound),
        "unknown"
    );
    // a message only helper, of this owner's agents, can read: no other live agent keeps it visible
    let note = ui
        .op(
            &c.k,
            &c.helper,
            "message.send",
            json!({"to": {"agent": "david/scout"}, "text": "helper only note"}),
        )
        .await;
    ui.op(&c.k, human, "agent.unregister", json!({"name": "helper"}))
        .await;
    assert_eq!(
        code(call(&c.worker, "agent.retire", json!({"name": "helper"})).await),
        Some(Forbidden),
        "an agent"
    );
    ui.op(&c.k, human, "agent.retire", json!({"name": "helper"}))
        .await;

    let helper = format!("{USER}/helper");
    let names = |v: &Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect()
    };
    let rail = ui.ok(&b, &csrf, "owner.list", json!({})).await;
    assert!(!names(&rail, "agents").contains(&helper), "{rail}");
    let fleet = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert!(!names(&fleet, "agents").contains(&helper), "{fleet}");
    let dir = ui.op(&c.k, &c.worker, "agent.list", json!({})).await;
    assert!(!names(&dir, "agents").contains(&helper), "{dir}");
    // its history stays readable
    let feed = ui
        .ok(&b, &csrf, "owner.feed", json!({"agent": "helper"}))
        .await;
    let handoff = feed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["text"] == "handoff to worker")
        .cloned();
    assert!(handoff.is_some(), "{feed}");
    let found = ui
        .ok(
            &b,
            &csrf,
            "owner.search",
            json!({"pattern": "handoff to worker"}),
        )
        .await;
    assert_eq!(found["hits"].as_array().unwrap().len(), 1, "{found}");
    let t = ui
        .ok(
            &b,
            &csrf,
            "owner.thread",
            json!({"id": handoff.unwrap()["message_id"]}),
        )
        .await;
    assert_eq!(texts(&t), vec!["handoff to worker"]);
    let found = ui
        .ok(
            &b,
            &csrf,
            "owner.search",
            json!({"pattern": "helper only note"}),
        )
        .await;
    assert_eq!(found["hits"].as_array().unwrap().len(), 1, "{found}");
    let t = ui
        .ok(&b, &csrf, "owner.thread", json!({"id": note["message_id"]}))
        .await;
    assert_eq!(texts(&t), vec!["helper only note"]);

    // registering it again locally brings it back
    ui.op(&c.k, &c.helper, "agent.register", json!({"name": "helper"}))
        .await;
    let rail = ui.ok(&b, &csrf, "owner.list", json!({})).await;
    assert!(names(&rail, "agents").contains(&helper), "{rail}");
    ui.server.shutdown().await.unwrap();
}

async fn another_owner_sees_only_their_own_agents(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    ui.browser().await;
    let c = cast(&ui).await;
    // A second owner signs in; their agent `probe` writes to worker and joins #team.
    ui.env.invite("dpacsuta", "dpacsuta@dakai.example").await;
    ui.env.idp.person.lock().unwrap().oid = "oid-dp".into();
    ui.env.idp.person.lock().unwrap().upn = "dpacsuta@dakai.example".into();
    let (b, csrf) = ui.browser().await;
    let p = ui.principal("dpacsuta", "p-host").await;
    let probe = "codex:thread-probe";
    ui.op(&p, probe, "agent.register", json!({"name": "probe"}))
        .await;
    ui.op(
        &p,
        probe,
        "message.send",
        json!({"to": {"agent": format!("{USER}/worker")}, "text": "hello worker"}),
    )
    .await;
    ui.op(&p, probe, "channel.join", json!({"channel": "#team"}))
        .await;

    let rail = ui.ok(&b, &csrf, "owner.list", json!({})).await;
    let names: Vec<&str> = rail["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["dpacsuta/human", "dpacsuta/probe"], "{rail}");
    let a = &rail["agents"][1];
    assert_eq!(a["name"], "dpacsuta/probe");
    assert_eq!(
        a["dms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["with"].clone())
            .collect::<Vec<_>>(),
        vec![json!(format!("{USER}/worker"))]
    );
    assert_eq!(a["channels"][0]["channel"], "#team");
    let all = rail.to_string();
    for hidden in ["helper", "ranger", "#secret", "scout", "handoff", "status?"] {
        assert!(!all.contains(hidden), "{hidden} leaked: {all}");
    }
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "probe", "with": format!("{USER}/worker")}),
        )
        .await;
    assert_eq!(texts(&h), vec!["hello worker"]);
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "probe", "with": format!("{USER}/helper")}),
        )
        .await;
    assert!(
        texts(&h).is_empty(),
        "worker and helper's DMs stay theirs: {h}"
    );
    let (_, v) = ui
        .rpc(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": format!("{USER}/worker"), "with": "david/scout"}),
        )
        .await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    let (_, v) = ui
        .rpc(
            &b,
            &csrf,
            "owner.message",
            json!({"id": c.worker_dm["message_id"]}),
        )
        .await;
    assert_eq!(v["error"]["code"], "not_found", "{v}");
    ui.server.shutdown().await.unwrap();
}

async fn delivery_state_shows_the_retry_clock_the_hold_and_the_last_error(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let sent = ui
        .op(
            &c.k,
            &c.worker,
            "message.send",
            json!({"to": {"agent": "david/scout"}, "text": "ping"}),
        )
        .await;
    let thread = json!({"agent": "worker", "with": "david/scout"});
    let state = |h: &Value| {
        h["messages"].as_array().unwrap().last().unwrap()["delivery"]["recipients"][0].clone()
    };

    let h = ui.ok(&b, &csrf, "owner.history", thread.clone()).await;
    let r = state(&h);
    assert_eq!(
        (r["recipient"].as_str(), r["state"].as_str()),
        (Some("david/scout"), Some("queued")),
        "{r}"
    );
    assert!(
        r["retry_at"].is_null(),
        "no retry clock before a failure: {r}"
    );

    // Scout's bridge claims it and the thread holds it for approval.
    let bridge = ui.d.bridge_principal(&c.dv);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let job = claim["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["message_id"] == sent["message_id"])
        .unwrap()
        .clone();
    let attempt = job["attempt_id"].as_str().unwrap().to_string();
    assert_eq!(
        state(&ui.ok(&b, &csrf, "owner.history", thread.clone()).await)["state"],
        "sending"
    );
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": attempt, "outcome": "held"}),
    )
    .await;
    assert_eq!(
        state(&ui.ok(&b, &csrf, "owner.history", thread.clone()).await)["state"],
        "held"
    );

    // The hold expires: a retry is due after the backoff, with the error shown.
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": attempt, "outcome": "expired", "reason": "no answer in 10 min"}),
    )
    .await;
    let h = ui.ok(&b, &csrf, "owner.history", thread.clone()).await;
    let r = state(&h);
    assert_eq!(r["state"], "retrying", "{r}");
    assert_eq!(r["attempts"], 1);
    assert_eq!(
        r["last_error"],
        json!({"status": "expired", "reason": "no answer in 10 min"})
    );
    assert!(
        r["retry_at"].as_str().unwrap() > h["messages"][0]["date"].as_str().unwrap(),
        "{r}"
    );

    // Read wins over the delivery state.
    ui.op(
        &c.dv,
        &c.scout,
        "message.read",
        json!({"id": sent["message_id"]}),
    )
    .await;
    let r = state(&ui.ok(&b, &csrf, "owner.history", thread.clone()).await);
    assert_eq!(r["state"], "read", "{r}");
    assert!(r["read_at"].is_string());

    // Scout's question to worker: only worker's own notification, never scout's other recipients.
    let h = ui.ok(&b, &csrf, "owner.history", thread).await;
    let first = &h["messages"][0];
    assert_eq!(first["from"], "david/scout");
    let rs = first["delivery"]["recipients"].as_array().unwrap();
    assert_eq!(rs.len(), 1, "{first}");
    assert_eq!(rs[0]["recipient"], format!("{USER}/worker"));
    ui.server.shutdown().await.unwrap();
}

/// The state a host takeover left on the live board: the recipient read the message, its
/// notification is read, and the old instance's attempt under it was abandoned afterwards. The
/// message shows as read with its time, never with that attempt's error.
async fn a_read_message_never_shows_the_error_of_a_stale_attempt(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let sent = ui
        .op(
            &c.k,
            &c.worker,
            "message.send",
            json!({"to": {"agent": "david/scout"}, "text": "ping"}),
        )
        .await;
    let bridge = ui.d.bridge_principal(&c.dv);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let job = claim["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["message_id"] == sent["message_id"])
        .unwrap()
        .clone();
    let (attempt, notification) = (
        job["attempt_id"].as_str().unwrap().to_string(),
        job["notification_id"].as_str().unwrap().to_string(),
    );
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": attempt, "outcome": "accepted"}),
    )
    .await;
    ui.op(
        &c.dv,
        &c.scout,
        "message.read",
        json!({"id": sent["message_id"]}),
    )
    .await;
    let d2 = d.clone();
    blocking(move || {
        d2.with_tx(|tx| {
            tx.update_attempt(
                &attempt,
                "abandoned",
                Some("bridge instance gone"),
                None,
                "2026-10-09T13:31:22.000Z",
            )?;
            tx.set_notification_status(&notification, "read")?;
            Ok(())
        })
        .unwrap()
    })
    .await;

    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/scout"}),
        )
        .await;
    let r = h["messages"].as_array().unwrap().last().unwrap()["delivery"]["recipients"][0].clone();
    assert_eq!(
        (r["state"].as_str(), r["status"].as_str()),
        (Some("read"), Some("read")),
        "{r}"
    );
    assert!(r["read_at"].is_string(), "{r}");
    assert_eq!(
        r["last_error"],
        Value::Null,
        "a read message carries no delivery error: {r}"
    );
    ui.server.shutdown().await.unwrap();
}

async fn history_pages_back_in_chronological_order(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    for i in 0..3 {
        ui.op(
            &c.k,
            &c.worker,
            "message.send",
            json!({"to": {"agent": "david/scout"}, "text": format!("n{i}")}),
        )
        .await;
    }
    let page = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/scout", "limit": 2}),
        )
        .await;
    assert_eq!(page["more"], true);
    assert_eq!(texts(&page), vec!["n1", "n2"]);
    let before = page["messages"][0]["seq"].clone();
    let older = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/scout", "limit": 2, "before": before}),
        )
        .await;
    assert_eq!(texts(&older), vec!["green, report attached", "n0"]);
    let oldest = ui
        .ok(&b, &csrf, "owner.history",
            json!({"agent": "worker", "with": "david/scout", "limit": 2, "before": older["messages"][0]["seq"]}))
        .await;
    assert_eq!(
        (texts(&oldest), oldest["more"].as_bool()),
        (vec!["status?".to_string()], Some(false))
    );
    ui.server.shutdown().await.unwrap();
}

// --- attachments --------------------------------------------------------------------------------

async fn attachments_download_only_where_an_own_agent_can_read(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let h = ui
        .ok(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "with": "david/scout"}),
        )
        .await;
    let att = h["messages"][1]["attachments"][0].clone();
    assert_eq!(att["filename"], "report.txt");
    let url = ui.env.url(&format!(
        "/v1/attachments/{}",
        att["attachment_id"].as_str().unwrap()
    ));
    let r = b.step.get(&url).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(r.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(
        r.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"report.txt\"; filename*=UTF-8''report.txt"
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"bytes of report.txt");
    assert_eq!(
        reqwest::get(&url).await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
        "signed out"
    );

    // The attachment of a DM between two of david's agents: not found for this owner.
    let d2 = ui.d.clone();
    let mid = c.private_dm["message_id"].as_str().unwrap().to_string();
    let secret = blocking(move || d2.with_conn(|t| t.attachment_list(&mid)).unwrap()).await;
    let url = ui.env.url(&format!("/v1/attachments/{}", secret[0].id));
    assert_eq!(
        b.step.get(&url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        b.step
            .get(ui.env.url("/v1/attachments/not-an-id"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    ui.server.shutdown().await.unwrap();
}

// --- channel members ------------------------------------------------------------------------------

async fn channel_members_shows_read_positions_to_members_only(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    // Who is in a channel is public; how far each member has read is for members only.
    let m = ui
        .op(
            &c.k,
            &c.worker,
            "channel.members",
            json!({"channel": "#secret"}),
        )
        .await;
    assert_eq!(
        m["members"],
        json!([{"name": "david/ranger"}, {"name": "david/scout"}]),
        "{m}"
    );
    let m = ui
        .op(
            &c.dv,
            &c.ranger,
            "channel.members",
            json!({"channel": "#secret"}),
        )
        .await;
    assert!(
        m["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["behind"].is_i64()),
        "{m}"
    );
    // The browser is no member of anything.
    let m = ui
        .ok(&b, &csrf, "channel.members", json!({"channel": "#team"}))
        .await;
    assert!(
        m["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x.get("behind").is_none()),
        "{m}"
    );
    ui.server.shutdown().await.unwrap();
}

// --- fleet ----------------------------------------------------------------------------------------

async fn the_fleet_shows_online_discovery_and_own_delivery_health(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let agent = |f: &Value, name: &str| {
        f["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let host = |f: &Value, name: &str| {
        f["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["host"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let worker = format!("{USER}/worker");

    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(f["online_ttl_s"], 90);
    assert_eq!(host(&f, "k-host")["online"], false, "no bridge yet: {f}");
    let w = agent(&f, &worker);
    assert_eq!(
        (
            w["kind"].as_str(),
            w["discovered"].is_null(),
            w["own"].as_bool()
        ),
        (Some("claude"), true, Some(true))
    );
    let s = agent(&f, "david/scout");
    assert!(
        s["held"].is_null() && s["stuck"].is_null() && s["last_reached"].is_null(),
        "another owner's health: {s}"
    );

    // k-host's bridge connects, reports worker's session live, then delivers helper's DM to it.
    let bridge = ui.d.bridge_principal(&c.k);
    ui.op(&bridge, "", "bridge.fence", json!({})).await;
    ui.op(
        &bridge,
        "",
        "bridge.endpoints",
        json!({"endpoints": [{"kind": "claude", "target": "sess-worker"}]}),
    )
    .await;
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let attempts: Vec<String> = claim["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["attempt_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        attempts.len(),
        2,
        "scout's and helper's DMs to worker: {claim}"
    );
    // The delivery receipt comes 30 s after the claim: "reached" is the receipt's time.
    let claimed_at = ui.ok(&b, &csrf, "fleet.list", json!({})).await["now"]
        .as_str()
        .unwrap()
        .to_string();
    ui.d.advance_clock(30_000);
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": attempts[0], "outcome": "delivered"}),
    )
    .await;
    let reported_at = ui.ok(&b, &csrf, "fleet.list", json!({})).await["now"]
        .as_str()
        .unwrap()
        .to_string();
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": attempts[1], "outcome": "held"}),
    )
    .await;

    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    let h = host(&f, "k-host");
    assert_eq!(
        (h["online"].as_bool(), h["owner"].as_str()),
        (Some(true), Some(USER)),
        "{h}"
    );
    let w = agent(&f, &worker);
    assert!(w["discovered"].is_string(), "{w}");
    assert_eq!(w["held"], 1, "{w}");
    assert_eq!(w["ever_reached"], true, "{w}");
    assert_eq!(
        agent(&f, &format!("{USER}/helper"))["ever_reached"],
        false,
        "never reached: {f}"
    );
    let reached = w["last_reached"].as_str().unwrap().to_string();
    assert!(
        reached > claimed_at && reached <= reported_at,
        "{reached} is the receipt's time, after {claimed_at}"
    );
    // A delivery recorded without a time (before migration 5, or by an older server during a
    // roll) is reached at an unknown time, never "never reached" and never an invented time.
    if let Some(sqlite) = ui.d.sqlite() {
        sqlite
            .with_connection(|c| c.execute_batch("UPDATE attempts SET settled_at = NULL"))
            .unwrap();
        let w = agent(&ui.ok(&b, &csrf, "fleet.list", json!({})).await, &worker);
        assert_eq!(
            (w["ever_reached"].as_bool(), w["last_reached"].is_null()),
            (Some(true), true),
            "{w}"
        );
        let d2 = ui.d.clone();
        blocking(move || {
            d2.sqlite().unwrap().with_connection(|c| {
                c.execute(
                    "UPDATE attempts SET settled_at = ?1 WHERE attempt_id = ?2",
                    (&reported_at, &attempts[0]),
                )
            })
        })
        .await
        .unwrap();
    }

    // Without heartbeats the host drops offline after the TTL; one heartbeat brings it back.
    ui.d.advance_clock(91_000);
    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(host(&f, "k-host")["online"], false, "{f}");
    let board = conde::cloud::Board::new(ui.d.clone());
    let instance = bridge.bridge_instance.clone().unwrap();
    blocking(move || board.heartbeat("k-host", &instance)).await;
    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(host(&f, "k-host")["online"], true, "{f}");

    // An undelivered DM older than a minute is stuck (helper is never claimed: Codex, no endpoint
    // matters; the claim above happened before this DM).
    ui.op(
        &c.dv,
        &c.scout,
        "message.send",
        json!({"to": {"agent": format!("{USER}/helper")}, "text": "to helper"}),
    )
    .await;
    ui.d.advance_clock(61_000);
    let f = ui.ok(&b, &csrf, "fleet.list", json!({})).await;
    assert_eq!(agent(&f, &format!("{USER}/helper"))["stuck"], 1, "{f}");
    ui.server.shutdown().await.unwrap();
}

// --- events -------------------------------------------------------------------------------------

async fn the_event_socket_says_when_the_board_changed(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let url = ui.env.url("/v1/events").replace("http://", "ws://");
    let cookie = b.jar_cookie(&ui.env.base);
    let handshake = |origin: Option<&str>, cookie: Option<&str>| {
        let mut req =
            tungstenite::client::IntoClientRequest::into_client_request(url.as_str()).unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert(header::ORIGIN, o.parse().unwrap());
        }
        if let Some(c) = cookie {
            req.headers_mut().insert(header::COOKIE, c.parse().unwrap());
        }
        req
    };
    let refused = |r: Result<_, tungstenite::Error>| match r {
        Err(tungstenite::Error::Http(resp)) => resp.status(),
        Ok(_) => panic!("upgraded"),
        Err(e) => panic!("{e}"),
    };
    let base = ui.env.base.clone();
    assert_eq!(
        refused(
            tokio_tungstenite::connect_async(handshake(Some(&base), None))
                .await
                .map(|_| ())
        ),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refused(
            tokio_tungstenite::connect_async(handshake(None, Some(&cookie)))
                .await
                .map(|_| ())
        ),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        refused(
            tokio_tungstenite::connect_async(handshake(
                Some("https://evil.example"),
                Some(&cookie)
            ))
            .await
            .map(|_| ())
        ),
        StatusCode::FORBIDDEN
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(handshake(Some(&base), Some(&cookie)))
        .await
        .unwrap();
    let hello = next_frame(&mut ws).await.expect("a hello");
    assert_eq!(hello["type"], "hello");

    let c = cast(&ui).await;
    ui.op(
        &c.dv,
        &c.scout,
        "message.send",
        json!({"to": {"agent": format!("{USER}/worker")}, "text": "note to worker"}),
    )
    .await;
    let changed = loop {
        let f = next_frame(&mut ws).await.expect("a change");
        if f["type"] == "changed" && f["topics"].as_array().unwrap().contains(&json!("messages")) {
            break f;
        }
    };
    assert!(
        !changed.to_string().contains("note to worker"),
        "events carry no content: {changed}"
    );
    // The browser itself writes nothing, so it announces nothing.
    ui.ok(&b, &csrf, "owner.list", json!({})).await;

    // A bridge outcome on another connection reaches the browser as a delivery change.
    let bridge = ui.d.bridge_principal(&c.k);
    let claim = ui.op(&bridge, "", "bridge.claim", json!({})).await;
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    // Everything up to the claim is announced and consumed first, so the next frame can only
    // be the report's.
    while quiet_frame(&mut ws).await.is_some() {}
    ui.op(
        &bridge,
        "",
        "bridge.report",
        json!({"attempt_id": attempt, "outcome": "delivered"}),
    )
    .await;
    let f = next_frame(&mut ws).await.expect("the report's change");
    assert_eq!(f["type"], "changed", "{f}");
    assert_eq!(f["topics"], json!(["delivery", "fleet"]), "{f}");
    ws.send(tungstenite::Message::Close(None)).await.unwrap();
    ui.server.shutdown().await.unwrap();
}

/// The next text frame unless the socket stays quiet for a moment.
async fn quiet_frame(ws: &mut Ws) -> Option<Value> {
    let read = async {
        loop {
            match ws.next().await?.ok()? {
                tungstenite::Message::Text(t) => return serde_json::from_str::<Value>(&t).ok(),
                _ => continue,
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(800), read)
        .await
        .ok()
        .flatten()
}

async fn the_browser_host_is_never_minted_as_a_real_host(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let b = Browser::new();
    let page = b.sign_in(&ui.env).await;
    let created = b.create_key(&ui.env, &csrf_of(&page), "web").await;
    assert!(created.contains("web-2"), "{created}");
    let d2 = ui.d.clone();
    let owner = blocking(move || d2.with_conn(|c| c.host_owner("web")).unwrap()).await;
    assert!(owner.is_none(), "{owner:?}");
    let d2 = ui.d.clone();
    let admin = blocking(move || {
        d2.with_tx(|c| {
            conde::ops::hosts::insert_key(
                c,
                USER,
                "web",
                "digest",
                "admin",
                "2026-10-09T00:00:00.000Z",
            )
        })
    })
    .await;
    assert!(admin.unwrap_err().to_string().contains("reserved"));
    ui.server.shutdown().await.unwrap();
}

async fn a_connected_bridge_keeps_its_host_online(d: Arc<Dispatcher>) {
    let config = ServerConfig {
        ping_interval: Duration::from_millis(150),
        ..ServerConfig::default()
    };
    let ui = ui_with(&d, config).await;
    let (b, csrf) = ui.browser().await;
    let key = format!("conde_test_{}", conde::util::new_ulid());
    let digest = conde_remote::token_digest(&key);
    let d2 = ui.d.clone();
    blocking(move || {
        d2.with_tx(|c| {
            conde::ops::hosts::insert_key(
                c,
                USER,
                "k-host",
                &digest,
                "test",
                "2026-10-09T00:00:00.000Z",
            )
        })
        .unwrap()
    })
    .await;
    let mut cfg = conde_remote::BridgeConfig::new(
        format!("{}/v1/bridge", ui.env.base.replace("http", "ws")),
        &key,
        "k-host",
    );
    cfg.backoff_min = Duration::from_millis(50);
    let bridge = conde_remote::BridgeClient::start(cfg, |_| {});
    let mut state = bridge.subscribe();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| *s == conde_remote::BridgeState::Connected),
    )
    .await
    .expect("the bridge connects")
    .unwrap();
    let online = |f: &Value| {
        f["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["host"] == "k-host")
            .unwrap()["online"]
            .clone()
    };
    // The client reports Connected once its hello is out; the server fences right after.
    let mut up = false;
    for _ in 0..50 {
        up = online(&ui.ok(&b, &csrf, "fleet.list", json!({})).await) == true;
        if up {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up, "the host comes online when its bridge connects");
    // Past the TTL of the connect-time stamp, answered pings alone keep the host online.
    ui.d.advance_clock(91_000);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        online(&ui.ok(&b, &csrf, "fleet.list", json!({})).await),
        true
    );
    bridge.shutdown();
    ui.server.shutdown().await.unwrap();
}

async fn owner_reads_end_when_the_last_own_member_leaves_a_channel(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let posted = ui
        .op(
            &c.k,
            &c.worker,
            "message.send",
            json!({"to": {"channel": "#team"}, "text": "my old channel post",
                   "attachments": [{"filename": "mine.txt", "data": STANDARD.encode("mine")}]}),
        )
        .await;
    let before = ui
        .ok(
            &b,
            &csrf,
            "owner.message",
            json!({"id": posted["message_id"]}),
        )
        .await;
    let attachment = before["message"]["attachments"][0]["attachment_id"]
        .as_str()
        .unwrap()
        .to_string();
    let url = ui.env.url(&format!("/v1/attachments/{attachment}"));
    assert_eq!(
        b.step.get(&url).send().await.unwrap().status(),
        StatusCode::OK
    );
    // Worker, the owner's only member of #team, leaves: the channel, its posts (worker's own
    // included) and their attachments are no longer the owner's to read.
    ui.op(
        &c.k,
        &c.worker,
        "channel.leave",
        json!({"channel": "#team"}),
    )
    .await;
    let (_, h) = ui
        .rpc(
            &b,
            &csrf,
            "owner.history",
            json!({"agent": "worker", "channel": "#team"}),
        )
        .await;
    assert_eq!(h["error"]["code"], "forbidden", "{h}");
    for id in [&posted["message_id"], &c.team_post["message_id"]] {
        let (_, m) = ui.rpc(&b, &csrf, "owner.message", json!({"id": id})).await;
        assert_eq!(m["error"]["code"], "not_found", "{m}");
    }
    assert_eq!(
        b.step.get(&url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    // DMs are unaffected.
    ui.ok(
        &b,
        &csrf,
        "owner.message",
        json!({"id": c.worker_dm["message_id"]}),
    )
    .await;
    ui.server.shutdown().await.unwrap();
}

async fn a_credential_on_the_browser_host_never_reaches_the_owner_view(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    ui.browser().await;
    let c = cast(&ui).await;
    // A credential on host `web`, as the admin path could mint one before the name was reserved.
    let token = conde::util::new_ulid();
    let digest = conde_remote::token_digest(&token);
    let d2 = ui.d.clone();
    blocking(move || {
        d2.with_tx(|tx| {
            let uid = tx.ensure_user(USER)?;
            tx.ensure_host("web", uid)?;
            tx.insert_credential(&digest, uid, "web", "legacy", "2026-10-01T00:00:00.000Z")?;
            Ok(())
        })
        .unwrap()
    })
    .await;
    let r = reqwest::Client::new()
        .post(ui.env.url("/v1/rpc"))
        .bearer_auth(&token)
        .json(
            &json!({"v": 1, "type": "request", "request_id": "legacy", "op": "owner.history",
                      "body": {"agent": "worker", "with": "david/scout"}}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "the reserved host never authenticates"
    );
    // And a principal on that host is still no browser: the owner view is refused.
    let mut p = c.k.clone();
    p.host = "web".into();
    let d2 = ui.d.clone();
    let req = Request::new(
        "r",
        "owner.history",
        json!({"agent": "worker", "with": "david/scout"}),
    );
    let resp = blocking(move || d2.dispatch(&p, &req)).await;
    assert_eq!(
        resp.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden)
    );
    ui.server.shutdown().await.unwrap();
}

/// `slack` names the server's own Slack sends: never minted from the keys page or the admin path,
/// and a credential on it never authenticates.
async fn the_slack_instance_is_never_a_host(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let b = Browser::new();
    let page = b.sign_in(&ui.env).await;
    let created = b.create_key(&ui.env, &csrf_of(&page), "slack").await;
    assert!(created.contains("slack-2"), "{created}");
    let d2 = ui.d.clone();
    let owner = blocking(move || d2.with_conn(|c| c.host_owner("slack")).unwrap()).await;
    assert!(owner.is_none(), "{owner:?}");
    let d2 = ui.d.clone();
    let admin = blocking(move || {
        d2.with_tx(|c| {
            conde::ops::hosts::insert_key(
                c,
                USER,
                "slack",
                "digest",
                "admin",
                "2026-10-09T00:00:00.000Z",
            )
        })
    })
    .await;
    assert!(admin.unwrap_err().to_string().contains("reserved"));
    let token = conde::util::new_ulid();
    let digest = conde_remote::token_digest(&token);
    let d2 = ui.d.clone();
    blocking(move || {
        d2.with_tx(|tx| {
            let uid = tx.ensure_user(USER)?;
            tx.ensure_host("slack", uid)?;
            tx.insert_credential(&digest, uid, "slack", "legacy", "2026-10-01T00:00:00.000Z")?;
            Ok(())
        })
        .unwrap()
    })
    .await;
    let r = reqwest::Client::new()
        .post(ui.env.url("/v1/rpc"))
        .bearer_auth(&token)
        .json(
            &json!({"v": 1, "type": "request", "request_id": "r", "op": "agent.list", "body": {}}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    ui.server.shutdown().await.unwrap();
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The next text frame within a few seconds.
async fn next_frame(ws: &mut Ws) -> Option<Value> {
    let read = async {
        loop {
            match ws.next().await?.ok()? {
                tungstenite::Message::Text(t) => return serde_json::from_str::<Value>(&t).ok(),
                _ => continue,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .ok()
        .flatten()
}

impl Browser {
    /// The cookies this browser sends to `base`, as one Cookie header value.
    fn jar_cookie(&self, base: &str) -> String {
        use reqwest::cookie::CookieStore;
        let url: reqwest::Url = base.parse().unwrap();
        self.jar
            .cookies(&url)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }
}

// --- static files without sign-in ----------------------------------------------------------------

/// Whether the web build the tests run with carries the icons (none, or one made before the
/// mark, does not); the icon checks are skipped otherwise.
fn icons_built() -> bool {
    let dist =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../conde-remote/web-dist/app");
    let built = [
        "favicon.svg",
        "mark.svg",
        "favicon-light-32.png",
        "favicon-dark-32.png",
    ]
    .iter()
    .all(|f| dist.join(f).is_file());
    if !built {
        eprintln!(
            "the web build carries no icons (run npm run build in web/): icon checks skipped"
        );
    }
    built
}

/// A server without sign-in serves the app's static files and the icons, and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn without_sign_in_only_the_static_files_are_served() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let d = Arc::new(Dispatcher::open_in_memory().unwrap());
    let server = conde::cloud::start(d, "127.0.0.1:0".parse().unwrap(), ServerConfig::default())
        .await
        .unwrap();
    let base = format!("http://{}", server.local_addr());
    let client = reqwest::Client::new();
    if icons_built() {
        for path in [
            "/app/favicon.svg",
            "/app/mark.svg",
            "/app/favicon-light-32.png",
            "/app/favicon-dark-32.png",
            "/favicon.ico",
        ] {
            let response = client.get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(!response.headers().contains_key("set-cookie"), "{path}");
        }
    }
    for path in [
        "/v1/session",
        "/v1/attachments/example",
        "/v1/events",
        "/app/v1/session",
        "/app/v1/rpc",
        "/app/%2e%2e%2f%2e%2e%2fCargo.toml",
        "/app/%2e%2e%2f%2e%2e%2fsrc%2fserver.rs",
        "/app/immutable%2f..%2f..%2findex.html",
        "/app/%2fetc%2fpasswd",
    ] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let response = client
        .post(format!("{base}/v1/rpc"))
        .json(&json!({"op": "owner.list", "body": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let readme = client.get(format!("{base}/readme")).send().await.unwrap();
    assert_eq!(readme.status(), StatusCode::OK);
    let html = readme.text().await.unwrap();
    assert!(html.contains("/app/favicon.svg"), "{html}");
    if icons_built() {
        assert!(html.contains("<svg "), "{html}");
    }
    server.shutdown().await.unwrap();
}

/// An encoded parent traversal never reaches a file outside the build, not even through a symlink.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_encoded_traversal_never_leaves_the_build() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("outside.txt");
    std::fs::write(&target, "OUTSIDE THE WEB BUILD").unwrap();
    let link = dir.path().join("outside-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let d = Arc::new(Dispatcher::open_in_memory().unwrap());
    let server = conde::cloud::start(d, "127.0.0.1:0".parse().unwrap(), ServerConfig::default())
        .await
        .unwrap();
    let traversal = format!(
        "{}{}",
        "../".repeat(20),
        link.to_str().unwrap().trim_start_matches('/')
    )
    .replace('.', "%2e")
    .replace('/', "%2f");
    let response = reqwest::get(format!("http://{}/app/{traversal}", server.local_addr()))
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    server.shutdown().await.unwrap();
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a file outside the build was served: {body}"
    );
}

// --- the owner's one write: an own agent's notify setting ----------------------------------------

async fn the_owner_sets_notify_for_their_own_agents_only(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf) = ui.browser().await;
    let c = cast(&ui).await;
    let body = |agent: &str, channel: &str, notify: &str| json!({"agent": agent, "channel": channel, "notify": notify});
    let (status, v) = ui
        .mutate(
            &b,
            &csrf,
            "owner.notify",
            body("worker", "#team", "all"),
            "k1",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["result"],
        json!({"agent": format!("{USER}/worker"), "channel": "#team", "notify": "all"}),
        "{v}"
    );
    // the same key again is the same answer
    let (_, again) = ui
        .mutate(
            &b,
            &csrf,
            "owner.notify",
            body("worker", "#team", "all"),
            "k1",
        )
        .await;
    assert_eq!(again["result"], v["result"]);
    let rail = ui.ok(&b, &csrf, "owner.list", json!({})).await;
    let worker = rail["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == format!("{USER}/worker"))
        .unwrap();
    assert_eq!(worker["channels"][0]["notify"], "all", "{worker}");
    // from now on an agent's plain post reaches worker
    let post = ui
        .op(
            &c.dv,
            &c.scout,
            "message.send",
            json!({"to": {"channel": "#team"}, "text": "plain post"}),
        )
        .await;
    let (d2, mid) = (
        ui.d.clone(),
        post["message_id"].as_str().unwrap().to_string(),
    );
    let pushed: Vec<(String, String)> =
        blocking(move || d2.with_conn(|t| t.notifications()).unwrap())
            .await
            .into_iter()
            .filter(|n| n.message_id == mid)
            .map(|n| (n.recipient_name, n.kind))
            .collect();
    assert_eq!(pushed, [(format!("{USER}/worker"), "post".to_string())]);

    // never another owner's agent, nor a channel the own agent is not in, nor a bad value
    for (b_, code) in [
        (body("david/scout", "#team", "all"), "forbidden"),
        (body("scout", "#team", "all"), "not_found"),
        (body("worker", "#secret", "all"), "forbidden"),
        (body("helper", "#team", "all"), "forbidden"),
        (body("worker", "#team", "loud"), "bad_request"),
    ] {
        let (_, v) = ui
            .mutate(
                &b,
                &csrf,
                "owner.notify",
                b_.clone(),
                &conde::util::new_ulid(),
            )
            .await;
        assert_eq!(v["error"]["code"], code, "{b_}: {v}");
    }
    let members = ui
        .op(
            &c.dv,
            &c.scout,
            "channel.members",
            json!({"channel": "#team"}),
        )
        .await;
    assert!(
        members["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] == format!("{USER}/worker") || m["notify"] == "mentions"),
        "{members}"
    );
    // a write needs its idempotency key, like every write
    let (_, v) = ui
        .rpc(
            &b,
            &csrf,
            "owner.notify",
            body("worker", "#team", "mentions"),
        )
        .await;
    assert_eq!(v["error"]["code"], "bad_request", "{v}");
    // an agent never sets it through the owner op, not even for itself
    let (d2, mut req) = (
        ui.d.clone(),
        Request::new("r", "owner.notify", body("worker", "#team", "mentions")),
    );
    req.actor = Some(c.worker.clone());
    req.idempotency_key = Some("k2".into());
    let k = c.k.clone();
    let resp = blocking(move || d2.dispatch(&k, &req)).await;
    assert_eq!(
        resp.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden)
    );
    // the browser's key answers no bearer request of the same user, which is refused, never replayed
    let (d2, mut req) = (
        ui.d.clone(),
        Request::new("r", "owner.notify", body("worker", "#team", "all")),
    );
    req.idempotency_key = Some("k1".into());
    let k = c.k.clone();
    let resp = blocking(move || d2.dispatch(&k, &req)).await;
    assert_eq!(
        resp.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden)
    );
    // nor one whose raw actor spells the browser's scope
    let (d2, mut req) = (
        ui.d.clone(),
        Request::new("r", "owner.notify", body("worker", "#team", "all")),
    );
    req.actor = Some("browser:".into());
    req.idempotency_key = Some("k1".into());
    let k = c.k.clone();
    let resp = blocking(move || d2.dispatch(&k, &req)).await;
    assert_eq!(
        resp.error.map(|e| e.code),
        Some(conde::protocol::ErrorCode::Forbidden),
        "{:?}",
        resp.result
    );
    ui.server.shutdown().await.unwrap();
}

// --- agent machines -----------------------------------------------------------------------------

const AUDIENCE: &str = "52f027a8-e3d6-4fb8-996c-d2221d38b6d6";
const TEMPLATE_SPEC: &str = "/subscriptions/sub-1/resourceGroups/conde-vm/providers/Microsoft.Resources/templateSpecs/agent-vm/versions/2";
const DEPLOYMENTS: &str =
    "/subscriptions/sub-1/resourceGroups/conde-vm/providers/Microsoft.Resources/deployments/";
const VMS: &str = "/providers/Microsoft.Compute/virtualMachines/";

/// Scripted answers, each used once and the last one kept.
type Answers = std::collections::VecDeque<(u16, Value)>;

/// One request the fake Resource Manager received.
#[derive(Clone, Debug)]
struct ArmCall {
    method: String,
    path: String,
    query: String,
    auth: String,
    body: Value,
}

/// A fake Resource Manager: records every request, refuses one without the identity's bearer
/// token (401), and answers deployment PUTs and GETs from scripts, each answer used once and the
/// last one kept.
struct Arm {
    url: String,
    calls: Mutex<Vec<ArmCall>>,
    puts: Mutex<std::collections::VecDeque<(u16, Value)>>,
    gets: Mutex<std::collections::VecDeque<(u16, Value)>>,
    /// VM reads by VM name; a VM without a script does not exist (404).
    vms: Mutex<HashMap<String, std::collections::VecDeque<(u16, Value)>>>,
    /// Answers by method and exact path, before every other script.
    routes: Mutex<HashMap<(String, String), Answers>>,
}

fn running() -> Value {
    json!({"name": "d", "properties": {"provisioningState": "Running"}})
}

fn next_answer(q: &Mutex<std::collections::VecDeque<(u16, Value)>>) -> (u16, Value) {
    let mut q = q.lock().unwrap();
    if q.len() > 1 {
        q.pop_front().unwrap()
    } else {
        q[0].clone()
    }
}

async fn fake_arm() -> Arc<Arm> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let arm = Arc::new(Arm {
        url: format!("http://{}", listener.local_addr().unwrap()),
        calls: Mutex::new(Vec::new()),
        puts: Mutex::new([(201, json!({"name": "d"}))].into()),
        gets: Mutex::new([(200, running())].into()),
        vms: Mutex::new(HashMap::new()),
        routes: Mutex::new(HashMap::new()),
    });
    let a = arm.clone();
    let app = Router::new().fallback(
        move |method: axum::http::Method,
              uri: axum::http::Uri,
              headers: HeaderMap,
              body: axum::body::Bytes| {
            let a = a.clone();
            async move {
                a.calls.lock().unwrap().push(ArmCall {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query: uri.query().unwrap_or("").to_string(),
                    auth: headers
                        .get(header::AUTHORIZATION)
                        .map(|v| v.to_str().unwrap().to_string())
                        .unwrap_or_default(),
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                });
                let bearer = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.starts_with("Bearer arm-token-"));
                let routed = a
                    .routes
                    .lock()
                    .unwrap()
                    .get_mut(&(method.to_string(), uri.path().to_string()))
                    .map(|q| if q.len() > 1 { q.pop_front().unwrap() } else { q[0].clone() });
                let (status, body) = match method.as_str() {
                    _ if !bearer => (
                        401,
                        json!({"error": {"code": "AuthenticationFailed", "message": "no token"}}),
                    ),
                    _ if routed.is_some() => routed.unwrap(),
                    "PUT" => next_answer(&a.puts),
                    _ if uri.path().contains(VMS) => {
                        let name = uri.path().rsplit('/').next().unwrap().to_string();
                        match a.vms.lock().unwrap().get_mut(&name) {
                            Some(q) if q.len() > 1 => q.pop_front().unwrap(),
                            Some(q) => q[0].clone(),
                            None => (
                                404,
                                json!({"error": {"code": "ResourceNotFound", "message": "no such VM"}}),
                            ),
                        }
                    }
                    _ => next_answer(&a.gets),
                };
                (StatusCode::from_u16(status).unwrap(), axum::Json(body))
            }
        },
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    arm
}

impl Arm {
    fn script(&self, method: &str, answers: Vec<(u16, Value)>) {
        let q = if method == "PUT" {
            &self.puts
        } else {
            &self.gets
        };
        *q.lock().unwrap() = answers.into();
    }

    /// The answers of the VM `name`'s reads, each used once and the last one kept.
    fn vm(&self, name: &str, answers: Vec<(u16, Value)>) {
        self.vms
            .lock()
            .unwrap()
            .insert(name.to_string(), answers.into());
    }

    /// The answers of `method` on exactly `path`, each used once and the last one kept.
    fn route(&self, method: &str, path: &str, answers: Vec<(u16, Value)>) {
        self.routes
            .lock()
            .unwrap()
            .insert((method.to_string(), path.to_string()), answers.into());
    }

    /// The `method` calls on exactly `path` so far.
    fn on(&self, method: &str, path: &str) -> Vec<ArmCall> {
        self.calls(method)
            .into_iter()
            .filter(|c| c.path == path)
            .collect()
    }

    /// The VM reads so far.
    fn vm_reads(&self) -> Vec<ArmCall> {
        self.calls("GET")
            .into_iter()
            .filter(|c| c.path.contains(VMS))
            .collect()
    }

    fn calls(&self, method: &str) -> Vec<ArmCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.method == method)
            .cloned()
            .collect()
    }

    /// The `method` calls once there are `n` of them.
    async fn wait(&self, method: &str, n: usize) -> Vec<ArmCall> {
        for _ in 0..100 {
            let calls = self.calls(method);
            if calls.len() >= n {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{method}: {:?}", self.calls(method));
    }
}

/// The server with agent machines on, against a fake Resource Manager and identity endpoint.
async fn ui_machines(d: &Arc<Dispatcher>) -> (Ui, Arc<Arm>) {
    ui_machines_with(d, |_| {}).await
}

async fn ui_machines_with(
    d: &Arc<Dispatcher>,
    tweak: impl FnOnce(&mut WebConfig),
) -> (Ui, Arc<Arm>) {
    let arm = fake_arm().await;
    let (identity, _) = identity_endpoint(200, 3600).await;
    let machines = web::MachinesConfig {
        subscription: "sub-1".into(),
        resource_group: "conde-vm".into(),
        template_spec: TEMPLATE_SPEC.into(),
        audience: AUDIENCE.into(),
        vm_size: web::DEFAULT_VM_SIZE.into(),
        identity: web::ManagedIdentity {
            endpoint: identity,
            header: "identity-header".into(),
            client_id: "id-conde".into(),
        },
        arm_url: arm.url.clone(),
        login_url: web::LOGIN_URL.into(),
        poll_every: Duration::from_millis(20),
    };
    (
        ui_full(d, ServerConfig::default(), Some(machines), tweak).await,
        arm,
    )
}

/// Signs in from `/keys`, stepping through every redirect: where the callback sends the browser.
async fn sign_in_landing(b: &Browser, env: &Env<WebBoard>) -> String {
    let mut url = env.url("/keys");
    loop {
        let r = b.step.get(&url).send().await.unwrap();
        let status = r.status();
        let location = r
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("{url}: {status}"));
        if url.contains("/auth/callback") {
            return location;
        }
        url = reqwest::Url::parse(&url)
            .unwrap()
            .join(&location)
            .unwrap()
            .to_string();
    }
}

impl Ui {
    /// Binds the provider's current person to `user` before any sign-in, so the next sign-in is a
    /// returning member's.
    async fn bound(&self, user: &str, oid: &str, email: &str) {
        let identity = web::Identity {
            issuer: self.env.idp.issuer.clone(),
            subject: oid.into(),
            display_name: None,
            preferred_username: Some(email.into()),
            email: None,
            verified_email: Some(email.into()),
        };
        let (store, user) = (self.env.store.clone(), user.to_string());
        blocking(move || store.bind_identity(&user, &identity).unwrap()).await;
    }

    /// The provider's person from now on.
    fn person(&self, oid: &str, upn: &str, name: &str) {
        let mut p = self.env.idp.person.lock().unwrap();
        p.oid = oid.into();
        p.upn = upn.into();
        p.name = name.into();
    }

    async fn setup(&self, b: &Browser, csrf: &str) -> Value {
        let (status, v) = self
            .mutate(
                b,
                csrf,
                "machine.setup",
                json!({}),
                &conde::util::new_ulid(),
            )
            .await;
        assert_eq!(
            (status, v["ok"].as_bool()),
            (StatusCode::OK, Some(true)),
            "{v}"
        );
        v["result"].clone()
    }

    async fn user_id(&self, user: &str) -> i64 {
        let (d, user) = (self.d.clone(), user.to_string());
        blocking(move || d.with_conn(|t| t.user_id(&user)).unwrap().unwrap()).await
    }

    async fn record(&self, user: &str) -> Option<conde::store::MachineRow> {
        let (d, uid) = (self.d.clone(), self.user_id(user).await);
        blocking(move || d.with_conn(|t| t.machine_of_user(uid)).unwrap()).await
    }

    /// The record once `ready` says it is.
    async fn record_when(
        &self,
        user: &str,
        ready: impl Fn(&conde::store::MachineRow) -> bool,
    ) -> conde::store::MachineRow {
        for _ in 0..100 {
            if let Some(m) = self.record(user).await.filter(|m| ready(m)) {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{user}: {:?}", self.record(user).await);
    }

    async fn write(
        &self,
        f: impl FnOnce(&dyn conde::store::Tx) -> anyhow::Result<()> + Send + 'static,
    ) {
        let d = self.d.clone();
        blocking(move || d.with_tx(f).unwrap()).await;
    }
}

fn deployment_body(owner: &str, oid: &str, server: &str) -> Value {
    json!({"properties": {
        "mode": "Incremental",
        "templateLink": {"id": TEMPLATE_SPEC},
        "parameters": {
            "owner": {"value": owner},
            "ownerObjectId": {"value": oid},
            "condeResource": {"value": AUDIENCE},
            "vmSize": {"value": "Standard_B2as_v2"},
            "condeServer": {"value": server},
        },
    }})
}

/// An invited person's first sign-in records the machine, PUTs the named deployment of the
/// template spec with the token's oid and lands on the page; a later sign-in returns where it
/// started and PUTs nothing; a first sign-in through `conde login` keeps the login's return.
async fn the_first_sign_in_of_an_invited_person_requests_a_machine_and_lands_on_the_page(
    d: Arc<Dispatcher>,
) {
    let (ui, arm) = ui_machines(&d).await;
    let b = Browser::new();
    assert_eq!(sign_in_landing(&b, &ui.env).await, "/#/machine");
    let put = arm.wait("PUT", 1).await.remove(0);
    assert_eq!(put.path, format!("{DEPLOYMENTS}vm-agent-{USER}"));
    assert_eq!(put.query, "api-version=2021-04-01");
    assert_eq!(put.auth, "Bearer arm-token-1");
    assert_eq!(
        put.body,
        deployment_body(USER, "oid-krisztian", &ui.env.base)
    );
    let m = ui.record_when(USER, |m| m.requested_at.is_some()).await;
    assert_eq!(
        (
            m.host_id.as_str(),
            m.owner_object_id.as_str(),
            m.deploy_error
        ),
        (&*format!("vm-agent-{USER}"), "oid-krisztian", None)
    );
    let session = ui.session(&b).await;
    assert_eq!(session["machines"], true, "{session}");
    let status = ui
        .ok(
            &b,
            session["csrf"].as_str().unwrap(),
            "machine.status",
            json!({}),
        )
        .await;
    assert_eq!(status["state"], "requested", "{status}");
    assert_eq!(status["host_id"], format!("vm-agent-{USER}"));
    // a returning member's sign-in: back where it started, no second PUT
    let again = Browser::new();
    again.sign_in(&ui.env).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(arm.calls("PUT").len(), 1);
    // a first sign-in through conde login keeps its return, and still asks for the machine
    ui.env
        .invite("levente-szabo", "levente.szabo@dakai.example")
        .await;
    ui.person(
        "oid-levente",
        "Levente.Szabo@dakai.example",
        "Levente Szabó",
    );
    let cli = Browser::new();
    browser_leg(&ui.env, &cli, &cli_run()).await;
    let put = arm.wait("PUT", 2).await.remove(1);
    assert_eq!(put.path, format!("{DEPLOYMENTS}vm-agent-levente-szabo"));
    ui.server.shutdown().await.unwrap();
}

/// A member bound before machines existed gets no machine at sign-in; "Set up my agent" records
/// it with the stored identity's subject and PUTs; a second click while it is pending PUTs nothing.
async fn a_returning_member_gets_a_machine_only_from_the_setup_step_with_its_stored_subject(
    d: Arc<Dispatcher>,
) {
    let (ui, arm) = ui_machines(&d).await;
    ui.bound(USER, "oid-krisztian", INVITED.1).await;
    let (b, csrf) = ui.browser().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(arm.calls("PUT").is_empty());
    let (_, v) = ui.rpc(&b, &csrf, "machine.status", json!({})).await;
    assert_eq!(v["error"]["code"], "not_found", "{v}");
    let r = ui.setup(&b, &csrf).await;
    assert_eq!(r["state"], "requested", "{r}");
    let put = arm.wait("PUT", 1).await.remove(0);
    assert_eq!(
        put.body,
        deployment_body(USER, "oid-krisztian", &ui.env.base)
    );
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let r = ui.setup(&b, &csrf).await;
    assert_eq!(r["deploy"], Value::Null, "{r}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(arm.calls("PUT").len(), 1);
    ui.server.shutdown().await.unwrap();
}

/// A PUT Azure refuses leaves the record never issued with the refusal as its error, shown as
/// `failed`; the setup step issues it again and the accepted PUT clears the error.
async fn a_refused_put_is_never_issued_and_the_setup_step_issues_it(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    arm.script(
        "PUT",
        vec![
            (
                403,
                json!({"error": {"code": "AuthorizationFailed", "message": "no write on conde-vm"}}),
            ),
            (201, json!({"name": "d"})),
        ],
    );
    let (b, csrf) = ui.browser_landing().await;
    let m = ui.record_when(USER, |m| m.deploy_error.is_some()).await;
    assert_eq!(m.requested_at, None);
    let status = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    assert_eq!(status["state"], "failed", "{status}");
    assert_eq!(
        status["deploy_error"],
        "the deployment PUT answered 403 Forbidden: AuthorizationFailed: no write on conde-vm"
    );
    assert!(
        arm.calls("GET").is_empty(),
        "a refused PUT has no deployment to read"
    );
    let r = ui.setup(&b, &csrf).await;
    assert_eq!(
        (r["state"].as_str(), r["deploy_error"].clone()),
        (Some("requested"), Value::Null),
        "{r}"
    );
    arm.wait("PUT", 2).await;
    let m = ui.record_when(USER, |m| m.requested_at.is_some()).await;
    assert_eq!(m.deploy_error, None);
    ui.server.shutdown().await.unwrap();
}

/// A pending deployment is read when the page asks, at most once in 30 s, with the identity's
/// token: `Running`; then a read Azure fails (503), which changes nothing stored; then, 30 s
/// later, `Failed` with its error, which the page shows; setup clears it and PUTs again.
async fn a_failed_deployment_is_read_on_demand_throttled_and_retried_by_setup(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    arm.script(
        "GET",
        vec![
            (200, running()),
            (
                503,
                json!({"error": {"code": "ServiceUnavailable", "message": "try later"}}),
            ),
            (
                200,
                json!({"properties": {"provisioningState": "Failed", "error": {
                    "code": "DeploymentFailed",
                    "message": "At least one resource deployment operation failed.",
                    "details": [{"code": "SkuNotAvailable", "message": "Standard_B2as_v2 is not available."}],
                }}}),
            ),
        ],
    );
    let (b, csrf) = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let first = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    let second = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    for s in [&first, &second] {
        assert_eq!(
            (s["state"].as_str(), s["deploy_state"].as_str()),
            (Some("requested"), Some("Running")),
            "{s}"
        );
    }
    let gets = arm.calls("GET");
    assert_eq!(gets.len(), 1, "one read in 30 s");
    assert_eq!(gets[0].path, format!("{DEPLOYMENTS}vm-agent-{USER}"));
    assert_eq!(gets[0].query, "api-version=2021-04-01");
    assert_eq!(gets[0].auth, "Bearer arm-token-1");
    // a failed read changes nothing stored; the next one, after the interval, reads again
    d.advance_clock(31_000);
    let unread = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    assert_eq!(
        (
            unread["state"].as_str(),
            unread["deploy_state"].as_str(),
            unread["deploy_error"].clone()
        ),
        (Some("requested"), Some("Running"), Value::Null),
        "{unread}"
    );
    assert_eq!(arm.calls("GET").len(), 2);
    d.advance_clock(31_000);
    let failed = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    assert_eq!(
        (failed["state"].as_str(), failed["deploy_state"].as_str()),
        (Some("failed"), Some("Failed")),
        "{failed}"
    );
    assert_eq!(
        failed["deploy_error"],
        "At least one resource deployment operation failed. Standard_B2as_v2 is not available."
    );
    let r = ui.setup(&b, &csrf).await;
    assert_eq!(
        (
            r["state"].as_str(),
            r["deploy_error"].clone(),
            r["deploy_state"].clone()
        ),
        (Some("requested"), Value::Null, Value::Null),
        "{r}"
    );
    arm.wait("PUT", 2).await;
    ui.server.shutdown().await.unwrap();
}

/// A member who joined a VM by hand (a live key of `vm-agent-<owner>`, or the host of their
/// published vm.agent) gets the record as joined, with no deployment.
async fn a_hand_joined_host_is_adopted_without_a_deploy(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    ui.bound(USER, "oid-krisztian", INVITED.1).await;
    let (b, csrf) = ui.browser().await;
    let uid = ui.user_id(USER).await;
    let store = ui.env.store.clone();
    blocking(move || {
        store
            .mint_host_key(uid, &format!("vm-agent-{USER}"), "vm", "digest-k", START)
            .unwrap()
    })
    .await;
    let r = ui.setup(&b, &csrf).await;
    assert_eq!(
        (
            r["state"].as_str(),
            r["host_id"].clone(),
            r["deploy"].clone()
        ),
        (Some("up"), json!(format!("vm-agent-{USER}")), Value::Null),
        "{r}"
    );
    assert!(r["minted_at"].is_string(), "{r}");
    // a pilot VM under a short name: the published vm.agent's host
    let lev = "levente-szabo";
    ui.env.invite(lev, "levente.szabo@dakai.example").await;
    ui.bound(lev, "oid-levente", "levente.szabo@dakai.example")
        .await;
    ui.person(
        "oid-levente",
        "Levente.Szabo@dakai.example",
        "Levente Szabó",
    );
    let (b2, csrf2) = ui.browser().await;
    let lid = ui.user_id(lev).await;
    let store = ui.env.store.clone();
    blocking(move || {
        store
            .mint_host_key(lid, "vm-agent-levente", "vm", "digest-l", START)
            .unwrap()
    })
    .await;
    ui.write(move |t| {
        t.insert_agent("agent-lev", lid, "vm.agent")?;
        t.set_binding(
            "agent-lev",
            Some(&conde::address::Address::parse("mailbox:mbx-lev@vm-agent-levente", "-").unwrap()),
        )?;
        Ok(())
    })
    .await;
    let r = ui.setup(&b2, &csrf2).await;
    assert_eq!(
        (r["state"].as_str(), r["host_id"].as_str()),
        (Some("up"), Some("vm-agent-levente")),
        "{r}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(arm.calls("PUT").is_empty());
    ui.server.shutdown().await.unwrap();
}

/// While the disable route removes the machine, setup is refused and the status says so.
async fn setup_is_refused_while_deleting(d: Arc<Dispatcher>) {
    let (ui, _arm) = ui_machines(&d).await;
    let (b, csrf) = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let uid = ui.user_id(USER).await;
    ui.write(move |t| Ok(t.machine_set_deleting(uid, "2026-11-05T23:00:00.000Z")?))
        .await;
    let (_, v) = ui
        .mutate(&b, &csrf, "machine.setup", json!({}), "k-del")
        .await;
    assert_eq!(v["error"]["code"], "conflict", "{v}");
    let s = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    assert_eq!(s["state"], "deleting", "{s}");
    ui.server.shutdown().await.unwrap();
}

/// Every state the page shows, from what the store holds: accepted (`requested`), a terminal
/// error (`failed`), 30 minutes without joining (`not_connected`), joined (`up`), a heartbeat and
/// the published vm.agent (`connected`), a live endpoint with the startup exchange answered
/// (`ready`), the heartbeat past the TTL (`disconnected`). The browser reads only its own.
async fn machine_status_says_requested_failed_not_connected_up_connected_disconnected_and_ready(
    d: Arc<Dispatcher>,
) {
    let (ui, _arm) = ui_machines(&d).await;
    ui.bound(USER, "oid-krisztian", INVITED.1).await;
    let (b, csrf) = ui.browser().await;
    let uid = ui.user_id(USER).await;
    let host = format!("vm-agent-{USER}");
    let iso = |ms: i64| conde::util::iso_from_ms(ms);
    let now = d.now_ms();
    let h = host.clone();
    ui.write(move |t| {
        t.machine_request(&conde::store::NewMachine {
            user_id: uid,
            host_id: &h,
            owner_object_id: "oid-krisztian",
            vm_size: "Standard_B2as_v2",
            minted_at: None,
        })?;
        t.machine_set_deploy(
            uid,
            &conde::store::MachineDeploy {
                requested_at: Some(iso(now)),
                ..Default::default()
            },
        )?;
        Ok(())
    })
    .await;
    let state = || async { ui.ok(&b, &csrf, "machine.status", json!({})).await };
    let s = state().await;
    assert_eq!(s["state"], "requested", "{s}");
    assert!(
        s["now"].is_string() && s["admins"].is_array() && s["relay"].is_null(),
        "{s}"
    );
    ui.write(move |t| {
        t.machine_set_deploy(
            uid,
            &conde::store::MachineDeploy {
                requested_at: Some(iso(now)),
                deploy_error: Some("quota".into()),
                ..Default::default()
            },
        )?;
        Ok(())
    })
    .await;
    assert_eq!(state().await["state"], "failed");
    let late = now - conde::ops::machines::JOIN_DEADLINE_MS - 1_000;
    ui.write(move |t| {
        t.machine_set_deploy(
            uid,
            &conde::store::MachineDeploy {
                requested_at: Some(iso(late)),
                deploy_checked_at: Some(iso(now)),
                ..Default::default()
            },
        )?;
        Ok(())
    })
    .await;
    assert_eq!(state().await["state"], "not_connected");
    // joined by hand: a live key of the host
    let (store, h) = (ui.env.store.clone(), host.clone());
    blocking(move || {
        store
            .mint_host_key(uid, &h, "vm", "digest-k", START)
            .unwrap()
    })
    .await;
    assert_eq!(state().await["state"], "up");
    let h = host.clone();
    ui.write(move |t| {
        t.fence(&h, "inst-1", &iso(now))?;
        Ok(())
    })
    .await;
    assert_eq!(
        state().await["state"],
        "up",
        "a heartbeat without the published vm.agent"
    );
    let h = host.clone();
    ui.write(move |t| {
        t.insert_agent("agent-k", uid, "vm.agent")?;
        let address = format!("mailbox:mbx-k@{h}");
        t.set_binding(
            "agent-k",
            Some(&conde::address::Address::parse(&address, "-").unwrap()),
        )?;
        Ok(())
    })
    .await;
    assert_eq!(state().await["state"], "connected");
    let h = host.clone();
    ui.write(move |t| {
        t.replace_endpoints(&h, "inst-1", &[("mailbox", "mbx-k")], &iso(now))?;
        Ok(())
    })
    .await;
    assert_eq!(
        state().await["state"],
        "connected",
        "reachable, not answered"
    );
    ui.write(move |t| {
        t.machine_set_ready(uid, &iso(now))?;
        Ok(())
    })
    .await;
    assert_eq!(state().await["state"], "ready");
    d.advance_clock(conde::ops::web::ONLINE_TTL_MS + 1_000);
    assert_eq!(state().await["state"], "disconnected");
    // the browser reads its own machine only
    let (_, v) = ui
        .rpc(&b, &csrf, "machine.status", json!({"owner": "david"}))
        .await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    ui.server.shutdown().await.unwrap();
}

impl Ui {
    /// A first sign-in that lands on the machine page, with the CSRF token of its session.
    async fn browser_landing(&self) -> (Browser, String) {
        let b = Browser::new();
        assert_eq!(sign_in_landing(&b, &self.env).await, "/#/machine");
        let s = self.session(&b).await;
        (b, s["csrf"].as_str().unwrap().to_string())
    }
}

/// A pilot VM already running when its owner adopts it: its bridge announced the published
/// vm.agent's live endpoint before the record existed and announces nothing new after. Setup
/// sends the probe at adoption, once, from the endpoints on record, and the running bridge gets it
/// without a reconnect; the agent's reply marks the machine ready; a repeated setup and a repeated
/// endpoints frame send no second probe.
async fn an_adopted_running_machine_is_probed_at_setup_and_its_reply_marks_it_ready(
    d: Arc<Dispatcher>,
) {
    let (ui, _arm) = ui_machines(&d).await;
    ui.bound(USER, "oid-krisztian", INVITED.1).await;
    let (b, csrf) = ui.browser().await;
    let host = format!("vm-agent-{USER}");
    let vm = ui.principal(USER, &host).await;
    let uid = vm.user_id;
    let key = format!("conde_test_{}", conde::util::new_ulid());
    let (store, h, digest) = (
        ui.env.store.clone(),
        host.clone(),
        conde_remote::token_digest(&key),
    );
    blocking(move || store.mint_host_key(uid, &h, "vm", &digest, START).unwrap()).await;
    // published: vm.agent bound to its mailbox on the migrated host
    let h = host.clone();
    ui.write(move |t| {
        t.set_host_mailbox_only(&h)?;
        t.insert_agent("agent-k", uid, "vm.agent")?;
        let address = format!("mailbox:mbx-k@{h}");
        t.set_binding(
            "agent-k",
            Some(&conde::address::Address::parse(&address, "-").unwrap()),
        )?;
        Ok(())
    })
    .await;
    // announced: the running bridge's hello lists the live mailbox
    let mut req = format!("{}/v1/bridge", ui.env.base.replace("http", "ws"))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mailbox = json!([{"kind": "mailbox", "mailbox_id": "mbx-k"}]);
    let hello = json!({"v": 1, "type": "hello", "host_id": host, "bridge_instance": "inst-k",
                       "endpoints": mailbox, "proto": 2, "window": 4});
    ws.send(tungstenite::Message::text(hello.to_string()))
        .await
        .unwrap();
    for _ in 0..100 {
        let d2 = d.clone();
        let rows = blocking(move || d2.with_conn(|t| t.endpoint_rows()).unwrap()).await;
        if rows.iter().any(|e| e.target == "mbx-k") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let messages = || {
        let d2 = d.clone();
        async move { blocking(move || d2.with_conn(|t| t.message_count()).unwrap()).await }
    };
    assert_eq!(messages().await, 0, "no record yet, no probe");
    let r = ui.setup(&b, &csrf).await;
    assert_eq!(r["state"], "connected", "{r}");
    let probe = ui
        .record(USER)
        .await
        .unwrap()
        .probe_id
        .expect("the probe at adoption");
    let (d2, id) = (d.clone(), probe.clone());
    let m = blocking(move || d2.with_conn(|t| t.message_by_id(&id)).unwrap().unwrap()).await;
    assert_eq!(
        (m.sender.as_str(), m.text.as_str()),
        (&*format!("{USER}/human"), conde::ops::machines::PROBE_TEXT)
    );
    // the running bridge gets it, no reconnect and no endpoint change
    let notify = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = ws.next().await.unwrap().unwrap();
            let Ok(text) = frame.to_text() else { continue };
            let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            if v["type"] == "notify" && v["message_id"] == probe.as_str() {
                return v;
            }
        }
    })
    .await
    .expect("the probe reaches the running bridge");
    assert_eq!(
        notify["endpoint"],
        json!({"kind": "mailbox", "mailbox_id": "mbx-k"})
    );
    // a repeated setup and a repeated endpoints frame send no second probe
    ui.setup(&b, &csrf).await;
    let again = json!({"v": 1, "type": "endpoints", "added": mailbox, "removed": []});
    ws.send(tungstenite::Message::text(again.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(ui.record(USER).await.unwrap().probe_id, Some(probe.clone()));
    assert_eq!(messages().await, 1, "one probe");
    ui.op(
        &vm,
        "mailbox:mbx-k",
        "message.send",
        json!({"to": {"agent": format!("{USER}/human")}, "text": "here", "reply_to": probe}),
    )
    .await;
    let s = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    assert_eq!(s["state"], "ready", "{s}");
    drop(ws);
    ui.server.shutdown().await.unwrap();
}

// --- the enrol route --------------------------------------------------------------------------

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// The claims of a v1 token of the VM identity `oid` for conde-machines, shaped as IMDS issues it.
fn vm_claims(oid: &str) -> Value {
    let now = unix_now();
    let sts = format!("https://sts.windows.net/{TENANT}/");
    json!({"aud": AUDIENCE, "iss": sts, "iat": now - 60, "nbf": now - 60, "exp": now + 3600,
           "aio": "k2BgYJjVfL", "appid": "1f3c9a2e-0000-4000-8000-000000000001", "appidacr": "2",
           "idp": sts, "idtyp": "app", "oid": oid, "sub": oid, "tid": TENANT, "uti": "u1",
           "ver": "1.0", "xms_mirid": format!("{VMS}vm-agent-{USER}")})
}

/// `claims` signed by the test key `i` under `kid`, with `alg` in the header.
fn signed(claims: &Value, alg: &str, kid: &str, i: usize) -> String {
    let header = json!({"alg": alg, "typ": "JWT", "kid": kid});
    let input = format!(
        "{}.{}",
        b64(header.to_string().as_bytes()),
        b64(claims.to_string().as_bytes())
    );
    let sig = SigningKey::<Sha256>::new(rsa_key(i))
        .sign(input.as_bytes())
        .to_vec();
    format!("{input}.{}", b64(&sig))
}

/// A VM's identity as Azure answers the VM read once the VM exists.
fn vm_with_identity(principal: &str) -> Value {
    json!({"name": "vm", "identity": {"type": "SystemAssigned", "principalId": principal,
           "tenantId": TENANT}})
}

impl Ui {
    fn vm_token(&self, oid: &str) -> String {
        self.env.idp.sign(&vm_claims(oid))
    }

    /// `POST /v1/machines/enrol` as the VM sends it, with `bearer` when given.
    async fn enrol(&self, bearer: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut r = reqwest::Client::new()
            .post(self.env.url("/v1/machines/enrol"))
            .json(&json!({"v": 1}));
        if let Some(token) = bearer {
            r = r.bearer_auth(token);
        }
        let r = r.send().await.unwrap();
        let (status, headers) = (r.status(), r.headers().clone());
        (status, headers, r.json().await.unwrap_or(Value::Null))
    }

    /// Whether `key` opens a bridge connection.
    async fn bridge_opens(&self, key: &str) -> bool {
        let mut req = format!("{}/v1/bridge", self.env.base.replace("http", "ws"))
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {key}").parse().unwrap());
        tokio_tungstenite::connect_async(req).await.is_ok()
    }
}

/// The VM's token gets the host key once: 200 with exactly the contract's nine fields and
/// `no-store`; the key opens the bridge. The hand-joined key the host had is revoked and its
/// bridge disconnected. Two concurrent calls mint once; every later call is 409 and the minted key
/// keeps working.
async fn enrol_mints_once_per_oid_and_answers_the_contract(d: Arc<Dispatcher>) {
    d.share("dakai");
    let (ui, arm) = ui_machines(&d).await;
    let host = format!("vm-agent-{USER}");
    arm.vm(&host, vec![(200, vm_with_identity("oid-vm-k"))]);
    let _browser = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    // joined by hand before the route existed: a live key and its connected bridge
    let uid = ui.user_id(USER).await;
    let old = format!("conde_test_{}", conde::util::new_ulid());
    let (store, h, digest) = (
        ui.env.store.clone(),
        host.clone(),
        conde_remote::token_digest(&old),
    );
    blocking(move || store.mint_host_key(uid, &h, "vm", &digest, START).unwrap()).await;
    let mut req = format!("{}/v1/bridge", ui.env.base.replace("http", "ws"))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {old}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let hello = json!({"v": 1, "type": "hello", "host_id": host, "bridge_instance": "inst-old",
                       "endpoints": [], "proto": 2, "window": 4});
    ws.send(tungstenite::Message::text(hello.to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let token = ui.vm_token("oid-vm-k");
    let (a, b) = tokio::join!(ui.enrol(Some(&token)), ui.enrol(Some(&token)));
    let (minted, other) = if a.0 == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(minted.0, StatusCode::OK, "{}", minted.2);
    assert_eq!(other.0, StatusCode::CONFLICT, "{}", other.2);
    assert_eq!(other.2["error"], "already_minted");
    assert_eq!(minted.1[header::CACHE_CONTROL], "no-store");
    let body = minted.2;
    let mut fields: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "board", "host_id", "key", "key_id", "label", "name", "server", "user", "v"
        ]
    );
    let key = body["key"].as_str().unwrap().to_string();
    assert!(key.len() == 70 && key.starts_with("conde_"), "a host key");
    assert_eq!(
        body,
        json!({"v": 1, "server": ui.env.base, "board": "dakai", "user": USER, "host_id": host,
               "label": host, "key_id": &conde_remote::token_digest(&key)[..12], "key": key,
               "name": format!("{USER}/vm.agent")})
    );
    // the hand-joined key is revoked and its bridge closed
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the replaced key's bridge is disconnected");
    assert!(!ui.bridge_opens(&old).await, "the replaced key is revoked");
    assert!(
        ui.bridge_opens(&key).await,
        "the minted key opens the bridge"
    );
    let m = ui.record(USER).await.unwrap();
    assert_eq!(m.principal_id.as_deref(), Some("oid-vm-k"));
    assert!(m.minted_at.is_some());
    // later calls, from anyone holding a token of the identity: 409, nothing changes
    let (status, _, v) = ui.enrol(Some(&ui.vm_token("oid-vm-k"))).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("already_minted"))
    );
    assert!(
        ui.bridge_opens(&key).await,
        "the first key still opens the bridge"
    );
    assert_eq!(ui.record(USER).await.unwrap().minted_at, m.minted_at);
    ui.server.shutdown().await.unwrap();
}

/// Every token that is not the tenant's v1 token for conde-machines, signed with a tenant key and
/// in its lifetime, is 401 `invalid_token` and mints nothing; the keys come from the v1 keys URL,
/// cached, and are read again once for an unknown `kid` (a rotated key then verifies).
async fn enrol_refuses_a_bad_signature_issuer_tenant_audience_or_expiry_with_401(
    d: Arc<Dispatcher>,
) {
    let (ui, arm) = ui_machines(&d).await;
    arm.vm(
        &format!("vm-agent-{USER}"),
        vec![(200, vm_with_identity("oid-vm-k"))],
    );
    let _browser = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let idp = &ui.env.idp;
    let with = |change: &dyn Fn(&mut Value)| {
        let mut c = vm_claims("oid-vm-k");
        change(&mut c);
        idp.sign(&c)
    };
    let now = unix_now();
    let spoiled = {
        *idp.spoil.lock().unwrap() = Spoil::Signature;
        let t = ui.vm_token("oid-vm-k");
        *idp.spoil.lock().unwrap() = Spoil::None;
        t
    };
    let cases: Vec<(&str, Option<String>)> = vec![
        ("no bearer", None),
        ("not a JWS", Some("not-a-token".into())),
        ("a bad signature", Some(spoiled)),
        (
            "the v2 issuer",
            Some(with(&|c| {
                c["iss"] = json!(format!("https://login.microsoftonline.com/{TENANT}/v2.0"))
            })),
        ),
        (
            "another tenant",
            Some(with(&|c| {
                c["tid"] = json!("72f988bf-86f1-41af-91ab-2d7cd011db47")
            })),
        ),
        (
            "the SSO app's audience",
            Some(with(&|c| c["aud"] = json!(CLIENT_ID))),
        ),
        (
            "an api:// audience",
            Some(with(&|c| c["aud"] = json!(format!("api://{AUDIENCE}")))),
        ),
        ("expired", Some(with(&|c| c["exp"] = json!(now - 1)))),
        (
            "not valid yet",
            Some(with(&|c| c["nbf"] = json!(now + 600))),
        ),
        (
            "no exp",
            Some(with(&|c| {
                c.as_object_mut().unwrap().remove("exp");
            })),
        ),
        (
            "no oid",
            Some(with(&|c| {
                c.as_object_mut().unwrap().remove("oid");
            })),
        ),
        (
            "HS256",
            Some(signed(&vm_claims("oid-vm-k"), "HS256", "kid-1", 0)),
        ),
        (
            "an unknown kid",
            Some(signed(&vm_claims("oid-vm-k"), "RS256", "kid-x", 1)),
        ),
    ];
    for (case, token) in &cases {
        let (status, headers, v) = ui.enrol(token.as_deref()).await;
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("invalid_token")),
            "{case}: {v}"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{case}");
    }
    assert_eq!(
        ui.record(USER).await.unwrap().minted_at,
        None,
        "nothing minted"
    );
    assert!(arm.vm_reads().is_empty(), "a refused token reads no VM");
    // one read for the first key lookup, one more for the unknown kid
    assert_eq!(idp.v1_keys_reads.load(Ordering::SeqCst), 2);
    // the tenant rotates its key: the new kid is read once and verifies
    idp.rotate();
    let (status, _, v) = ui.enrol(Some(&ui.vm_token("oid-vm-k"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(idp.v1_keys_reads.load(Ordering::SeqCst), 3);
    ui.server.shutdown().await.unwrap();
}

/// While the VM resource does not exist, or exists without its identity, the enrol is 404
/// `unknown_machine` and stores nothing; once Azure answers the identity it mints. A record that
/// has its identity is never read again.
async fn enrol_answers_404_until_the_vm_resource_has_its_principal(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    let host = format!("vm-agent-{USER}");
    arm.vm(
        &host,
        vec![
            (
                404,
                json!({"error": {"code": "ResourceNotFound", "message": "no such VM"}}),
            ),
            (200, json!({"name": "vm"})),
            (200, vm_with_identity("oid-vm-k")),
        ],
    );
    let _browser = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let token = ui.vm_token("oid-vm-k");
    for round in ["no VM yet", "no identity yet"] {
        let (status, _, v) = ui.enrol(Some(&token)).await;
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_machine")),
            "{round}: {v}"
        );
        assert_eq!(ui.record(USER).await.unwrap().principal_id, None, "{round}");
    }
    let (status, _, v) = ui.enrol(Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let reads = arm.vm_reads();
    assert_eq!(reads.len(), 3);
    for r in &reads {
        assert_eq!(
            r.path,
            format!("/subscriptions/sub-1/resourceGroups/conde-vm{VMS}{host}")
        );
        assert_eq!(r.query, "api-version=2024-07-01");
        assert_eq!(r.auth, "Bearer arm-token-1");
    }
    // an identity no record has: 404, and the minted record is not read again
    let (status, _, v) = ui.enrol(Some(&ui.vm_token("oid-stranger"))).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_machine")),
        "{v}"
    );
    assert_eq!(arm.vm_reads().len(), 3);
    ui.server.shutdown().await.unwrap();
}

/// A record being deleted, or one whose owner is disabled, is 403 `disabled` and mints nothing.
async fn enrol_answers_403_for_a_disabled_owner_or_a_deleting_record(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    arm.vm(
        &format!("vm-agent-{USER}"),
        vec![(200, vm_with_identity("oid-vm-k"))],
    );
    arm.vm(
        "vm-agent-levente-szabo",
        vec![(200, vm_with_identity("oid-vm-l"))],
    );
    let _k = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let lev = "levente-szabo";
    ui.env.invite(lev, "levente.szabo@dakai.example").await;
    ui.person(
        "oid-levente",
        "Levente.Szabo@dakai.example",
        "Levente Szabó",
    );
    let _l = ui.browser_landing().await;
    ui.record_when(lev, |m| m.requested_at.is_some()).await;
    // the VM identity was read at an earlier enrol, then the disable route started
    let uid = ui.user_id(USER).await;
    ui.write(move |t| {
        t.machine_set_principal(uid, "oid-vm-k")?;
        Ok(t.machine_set_deleting(uid, "2026-11-06T09:00:00.000Z")?)
    })
    .await;
    let store = ui.env.store.clone();
    blocking(move || store.set_disabled(lev, true).unwrap()).await;
    for (user, oid) in [(USER, "oid-vm-k"), (lev, "oid-vm-l")] {
        let (status, _, v) = ui.enrol(Some(&ui.vm_token(oid))).await;
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("disabled")),
            "{user}: {v}"
        );
        let m = ui.record(user).await.unwrap();
        assert_eq!(
            (m.principal_id.as_deref(), m.minted_at),
            (Some(oid), None),
            "{user}"
        );
    }
    let d2 = d.clone();
    let keys = blocking(move || d2.with_conn(|t| t.credentials()).unwrap()).await;
    assert!(
        keys.iter().all(|k| !k.host.starts_with("vm-agent-")),
        "no key minted: {keys:?}"
    );
    ui.server.shutdown().await.unwrap();
}

/// Past its per-address budget the enrol is 429 `rate_limited` with `Retry-After` in seconds,
/// before the token is looked at.
async fn enrol_is_rate_limited_with_retry_after(d: Arc<Dispatcher>) {
    let (ui, _arm) = ui_machines_with(&d, |c| c.limits.machine_enrol_per_min = 2).await;
    for _ in 0..2 {
        assert_eq!(ui.enrol(None).await.0, StatusCode::UNAUTHORIZED);
    }
    let (status, headers, v) = ui.enrol(Some(&ui.vm_token("oid-vm-k"))).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")),
        "{v}"
    );
    let wait: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert_eq!(v["retry_after"], wait);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    ui.server.shutdown().await.unwrap();
}

/// While the tenant's signing keys cannot be read the enrol is 503 `unavailable`, which the VM
/// retries (a 401 would be final), and nothing is minted; once the keys can be read the same
/// token mints.
async fn enrol_answers_503_while_the_tenant_keys_cannot_be_read_and_mints_once_they_can(
    d: Arc<Dispatcher>,
) {
    let (ui, arm) = ui_machines(&d).await;
    let host = format!("vm-agent-{USER}");
    arm.vm(&host, vec![(200, vm_with_identity("oid-vm-k"))]);
    let _browser = ui.browser_landing().await;
    ui.record_when(USER, |m| m.requested_at.is_some()).await;
    let idp = &ui.env.idp;
    idp.v1_keys_status.store(503, Ordering::SeqCst);
    let token = ui.vm_token("oid-vm-k");
    let (status, headers, v) = ui.enrol(Some(&token)).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("unavailable")),
        "{v}"
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        ui.record(USER).await.unwrap().minted_at,
        None,
        "nothing minted"
    );
    let d2 = d.clone();
    let keys = blocking(move || d2.with_conn(|t| t.credentials()).unwrap()).await;
    assert!(keys.iter().all(|k| k.host != host), "no key: {keys:?}");
    assert!(arm.vm_reads().is_empty(), "an unchecked token reads no VM");
    idp.v1_keys_status.store(0, Ordering::SeqCst);
    let (status, _, v) = ui.enrol(Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(ui.record(USER).await.unwrap().minted_at.is_some());
    ui.server.shutdown().await.unwrap();
}

// --- the sign-in relay ------------------------------------------------------------------------

/// A pasted code as a test writes it (a fixture, never a real code); assertions never print it.
const RELAY_CODE: &str = "relay-fixture-7Qx#a1";

/// The signed-in person's minted machine `vm-agent-<USER>` with its host key: (browser, CSRF,
/// host, key).
async fn relay_machine(ui: &Ui) -> (Browser, String, String, String) {
    let (b, csrf) = ui.browser().await;
    let host = format!("vm-agent-{USER}");
    let key = vm_key(ui, USER, &host).await;
    (b, csrf, host, key)
}

/// A minted machine record of `user` on `host` and the host's key.
async fn vm_key(ui: &Ui, user: &str, host: &str) -> String {
    let uid = ui.principal(user, host).await.user_id;
    let key = format!("conde_test_{}", conde::util::new_ulid());
    let (store, h, digest) = (
        ui.env.store.clone(),
        host.to_string(),
        conde_remote::token_digest(&key),
    );
    blocking(move || store.mint_host_key(uid, &h, "vm", &digest, START).unwrap()).await;
    let h = host.to_string();
    ui.write(move |t| {
        t.set_host_mailbox_only(&h)?;
        t.machine_request(&conde::store::NewMachine {
            user_id: uid,
            host_id: &h,
            owner_object_id: "oid-relay",
            vm_size: "Standard_B2as_v2",
            minted_at: Some("2026-11-06T08:00:00.000Z"),
        })?;
        Ok(())
    })
    .await;
    key
}

/// The machine's bridge: a v2 hello without endpoints.
async fn vm_bridge(ui: &Ui, host: &str, key: &str, instance: &str) -> Ws {
    let mut req = format!("{}/v1/bridge", ui.env.base.replace("http", "ws"))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let hello = json!({"v": 1, "type": "hello", "host_id": host, "bridge_instance": instance,
                       "endpoints": [], "proto": 2, "window": 4});
    ws.send(tungstenite::Message::text(hello.to_string()))
        .await
        .unwrap();
    ws
}

/// The next `relay` frame the bridge gets within `ms`, skipping every other frame.
async fn relay_within(ws: &mut Ws, ms: u64) -> Option<Value> {
    let read = async {
        loop {
            match ws.next().await {
                Some(Ok(tungstenite::Message::Text(t))) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "relay" {
                        return Some(v);
                    }
                }
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(ms), read)
        .await
        .ok()
        .flatten()
}

async fn relay_frame(ws: &mut Ws) -> Value {
    relay_within(ws, 10_000).await.expect("a relay frame")
}

/// The bridge reports `event` of a relay.
async fn relay_event(ws: &mut Ws, event: Value) {
    let mut f = event;
    f["v"] = json!(1);
    f["type"] = json!("relay_event");
    ws.send(tungstenite::Message::text(f.to_string()))
        .await
        .unwrap();
}

/// The browser's event socket.
async fn events_socket(ui: &Ui, b: &Browser) -> Ws {
    let url = ui.env.url("/v1/events").replace("http://", "ws://");
    let mut req = url.as_str().into_client_request().unwrap();
    req.headers_mut()
        .insert(header::ORIGIN, ui.env.base.parse().unwrap());
    req.headers_mut()
        .insert(header::COOKIE, b.jar_cookie(&ui.env.base).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(next_frame(&mut ws).await.expect("a hello")["type"], "hello");
    ws
}

impl Ui {
    /// One relay op from the browser, with its own idempotency key: the whole response.
    async fn relay(&self, b: &Browser, csrf: &str, op: &str, body: Value) -> Value {
        let (status, v) = self
            .mutate(b, csrf, op, body, &conde::util::new_ulid())
            .await;
        assert_eq!(status, StatusCode::OK, "{op}");
        v
    }

    /// Starts `flow`: the new relay's id.
    async fn relay_start(&self, b: &Browser, csrf: &str, flow: &str) -> String {
        let r = self
            .relay(b, csrf, "machine.relay.start", json!({"flow": flow}))
            .await;
        assert_eq!(r["ok"], true, "{r}");
        r["result"]["relay_id"].as_str().unwrap().to_string()
    }

    /// `machine.status`'s relay once `ready` holds for it.
    async fn relay_when(&self, b: &Browser, csrf: &str, ready: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        for _ in 0..100 {
            last = self.ok(b, csrf, "machine.status", json!({})).await["relay"].clone();
            if ready(&last) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("relay: {last}");
    }

    /// The record's `pending_code` column itself, read with SQL on either store.
    async fn pending_code_column(&self, host: &str) -> Option<String> {
        let (d, h) = (self.d.clone(), host.to_string());
        let url = match self.d.sqlite() {
            Some(_) => None,
            None => pg_urls()
                .lock()
                .unwrap()
                .get(&(Arc::as_ptr(&self.d) as usize))
                .cloned(),
        };
        blocking(move || match url {
            Some(url) => ::postgres::Client::connect(&url, ::postgres::NoTls)
                .unwrap()
                .query_one(
                    "SELECT pending_code FROM machines WHERE host_id = $1",
                    &[&h],
                )
                .unwrap()
                .get(0),
            None => d.sqlite().unwrap().with_connection(|c| {
                c.query_row(
                    "SELECT pending_code FROM machines WHERE host_id = ?1",
                    [&h],
                    |r| r.get(0),
                )
                .unwrap()
            }),
        })
        .await
    }

    /// The host's unsent relay command, read as the bridge's poll reads it.
    async fn pending(&self, host: &str) -> Option<conde::store::RelayCommand> {
        let (d, h) = (self.d.clone(), host.to_string());
        blocking(move || d.with_conn(|t| t.machine_commands(&h)).unwrap()).await
    }
}

fn state(relay: &Value) -> (&str, &str) {
    (
        relay["state"].as_str().unwrap_or(""),
        relay["reason"].as_str().unwrap_or(""),
    )
}

/// The browser's start reaches the machine's bridge as the contract's `relay` frame; each
/// `relay_event` the bridge sends tells the page's event socket `machine`, and `machine.status`
/// shows it: the Claude URL, then waiting and done; GitHub's device code with its expiry.
async fn a_relay_start_reaches_the_bridge_and_its_events_reach_the_page(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let mut events = events_socket(&ui, &b).await;
    let id = ui.relay_start(&b, &csrf, "claude").await;
    assert_eq!(
        relay_frame(&mut ws).await,
        json!({"v": 1, "type": "relay", "relay_id": id, "flow": "claude", "action": "start"})
    );
    let r = ui
        .relay_when(&b, &csrf, |r| r["relay_id"] == id.as_str())
        .await;
    assert_eq!(
        (r["flow"].as_str(), state(&r)),
        (Some("claude"), ("starting", ""))
    );
    while quiet_frame(&mut events).await.is_some() {}
    let url = "https://claude.com/cai/oauth/authorize?code=true&state=s1";
    relay_event(&mut ws, json!({"relay_id": id, "event": "url", "url": url})).await;
    let changed = next_frame(&mut events).await.expect("the event's change");
    assert_eq!(changed["type"], "changed", "{changed}");
    assert!(
        changed["topics"]
            .as_array()
            .unwrap()
            .contains(&json!("machine")),
        "{changed}"
    );
    let r = ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    assert_eq!(r["url"], url, "{r}");
    relay_event(&mut ws, json!({"relay_id": id, "event": "waiting"})).await;
    relay_event(&mut ws, json!({"relay_id": id, "event": "done"})).await;
    let r = ui.relay_when(&b, &csrf, |r| r["state"] == "done").await;
    assert_eq!(r["relay_id"], id.as_str());

    let gh = ui.relay_start(&b, &csrf, "github").await;
    assert_eq!(
        relay_frame(&mut ws).await,
        json!({"v": 1, "type": "relay", "relay_id": gh, "flow": "github", "action": "start"})
    );
    relay_event(
        &mut ws,
        json!({"relay_id": gh, "event": "url", "url": "https://github.com/login/device",
               "user_code": "WDJB-MJHT", "expires_at": "2026-11-06T10:15:00Z"}),
    )
    .await;
    let r = ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    assert_eq!(
        (&r["flow"], &r["url"], &r["user_code"], &r["expires_at"]),
        (
            &json!("github"),
            &json!("https://github.com/login/device"),
            &json!("WDJB-MJHT"),
            &json!("2026-11-06T10:15:00Z")
        ),
        "{r}"
    );
    ui.server.shutdown().await.unwrap();
}

/// The pasted code goes to the bridge once, in the contract's `code` frame, and is cleared from
/// the record when the connection sent it; no answer, status or later poll carries it. A code is
/// taken only while the Claude relay shows its URL.
async fn a_relay_code_is_sent_once_and_cleared(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let id = ui.relay_start(&b, &csrf, "claude").await;
    relay_frame(&mut ws).await;
    relay_event(
        &mut ws,
        json!({"relay_id": id, "event": "url", "url": "https://claude.com/cai/oauth/authorize"}),
    )
    .await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    let r = ui
        .relay(
            &b,
            &csrf,
            "machine.relay.code",
            json!({"relay_id": id, "code": RELAY_CODE}),
        )
        .await;
    assert!(
        !r.to_string().contains(RELAY_CODE),
        "the answer has no code"
    );
    assert_eq!(r["ok"], true, "the code is taken");
    let f = relay_frame(&mut ws).await;
    let mut keys: Vec<&String> = f.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["action", "code", "relay_id", "type", "v"]);
    assert!(
        f["action"] == "code" && f["relay_id"] == id.as_str() && f["code"] == RELAY_CODE,
        "the code frame carries the pasted code for the live relay"
    );
    // cleared by the send: no later poll sends it again
    assert!(
        relay_within(&mut ws, 2_500).await.is_none(),
        "the code goes once"
    );
    assert!(ui.pending(&host).await.is_none(), "nothing pending");
    assert!(
        ui.pending_code_column(&host).await.is_none(),
        "the record's pending_code is NULL after the send"
    );
    let s = ui.ok(&b, &csrf, "machine.status", json!({})).await;
    assert!(
        !s.to_string().contains(RELAY_CODE),
        "the status has no code"
    );
    // after the helper took it, no second code
    relay_event(&mut ws, json!({"relay_id": id, "event": "waiting"})).await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "waiting").await;
    let again = ui
        .relay(
            &b,
            &csrf,
            "machine.relay.code",
            json!({"relay_id": id, "code": RELAY_CODE}),
        )
        .await;
    assert!(
        !again.to_string().contains(RELAY_CODE),
        "the refusal has no code"
    );
    assert_eq!(
        again["error"]["code"], "conflict",
        "{}",
        again["error"]["message"]
    );
    ui.server.shutdown().await.unwrap();
}

/// The connection clears exactly the command it sent: a start, or a code, committed between its
/// read and its clear stays pending, and the next poll sends it.
async fn a_start_committed_between_read_and_send_survives_the_clear(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    // no bridge yet: the commands wait in the record
    let first = ui.relay_start(&b, &csrf, "claude").await;
    let read = ui.pending(&host).await.expect("the first start");
    assert_eq!(read.relay.relay_id, first);
    let second = ui.relay_start(&b, &csrf, "github").await;
    let (h, sent) = (host.clone(), read.relay.clone());
    ui.write(move |t| {
        Ok(t.machine_relay_command_sent(&h, &sent.relay_id, &sent.action, sent.code.as_deref())?)
    })
    .await;
    let left = ui.pending(&host).await.expect("the newer start stays");
    assert_eq!(
        (left.relay.relay_id.as_str(), left.relay.action.as_str()),
        (second.as_str(), "start")
    );
    // the same for two codes: the second, committed after the read of the first, stays
    let h = host.clone();
    let s2 = second.clone();
    ui.write(move |t| {
        t.machine_relay_command_sent(&h, &s2, "start", None)?;
        let claude = t.machine_relay_start(
            t.user_id(USER)?.unwrap(),
            "relay-codes",
            "claude",
            "2026-11-06T08:00:00.000Z",
        );
        claude?;
        t.machine_relay_command_sent(&h, "relay-codes", "start", None)?;
        let url = conde::store::RelayUpdate {
            relay_id: "relay-codes",
            state: "url",
            url: Some("https://claude.com/cai/oauth/authorize"),
            ..Default::default()
        };
        assert!(t.machine_relay_update(&h, &url, "2026-11-06T08:00:01.000Z")?);
        Ok(())
    })
    .await;
    // the ops' clock is far from the written relay_at, which only the start rule reads
    let code = |c: &str| json!({"relay_id": "relay-codes", "code": c});
    let r = ui
        .relay(&b, &csrf, "machine.relay.code", code("first-code"))
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let read = ui.pending(&host).await.expect("the first code");
    assert_eq!(read.relay.code.as_deref(), Some("first-code"));
    let r = ui
        .relay(&b, &csrf, "machine.relay.code", code("second-code"))
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let (h, sent) = (host.clone(), read.relay.clone());
    ui.write(move |t| {
        Ok(t.machine_relay_command_sent(&h, &sent.relay_id, &sent.action, sent.code.as_deref())?)
    })
    .await;
    // the bridge's next poll sends what stayed, once
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let f = relay_frame(&mut ws).await;
    assert_eq!(
        (&f["relay_id"], &f["action"], &f["code"]),
        (&json!("relay-codes"), &json!("code"), &json!("second-code"))
    );
    assert!(
        relay_within(&mut ws, 2_500).await.is_none(),
        "what stayed goes once"
    );
    ui.server.shutdown().await.unwrap();
}

/// A new start replaces the live relay: the bridge gets the new start, and every event of the old
/// relay is dropped from then on.
async fn a_new_start_replaces_the_live_relay_and_the_old_ids_events_are_dropped(
    d: Arc<Dispatcher>,
) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let old = ui.relay_start(&b, &csrf, "claude").await;
    relay_frame(&mut ws).await;
    relay_event(
        &mut ws,
        json!({"relay_id": old, "event": "url", "url": "https://claude.com/cai/oauth/authorize"}),
    )
    .await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    let new = ui.relay_start(&b, &csrf, "codex").await;
    assert_eq!(
        relay_frame(&mut ws).await,
        json!({"v": 1, "type": "relay", "relay_id": new, "flow": "codex", "action": "start"})
    );
    relay_event(&mut ws, json!({"relay_id": old, "event": "done"})).await;
    relay_event(
        &mut ws,
        json!({"relay_id": old, "event": "failed", "reason": "process_exited"}),
    )
    .await;
    relay_event(
        &mut ws,
        json!({"relay_id": new, "event": "url", "url": "https://auth.openai.com/codex/device",
               "user_code": "ABCD-12345", "expires_at": "2026-11-06T10:15:00Z"}),
    )
    .await;
    let r = ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    assert_eq!(
        (&r["relay_id"], &r["flow"], &r["user_code"], &r["reason"]),
        (
            &json!(new),
            &json!("codex"),
            &json!("ABCD-12345"),
            &Value::Null
        ),
        "{r}"
    );
    ui.server.shutdown().await.unwrap();
}

/// The connection that carried a relay ends (closed by the bridge, or replaced by its reconnect):
/// the relay is `failed` `expired`. A relay the ending connection never carried stays.
async fn a_lost_bridge_connection_expires_the_live_relay(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let id = ui.relay_start(&b, &csrf, "claude").await;
    relay_frame(&mut ws).await;
    relay_event(
        &mut ws,
        json!({"relay_id": id, "event": "url", "url": "https://claude.com/cai/oauth/authorize"}),
    )
    .await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    ws.send(tungstenite::Message::Close(None)).await.unwrap();
    drop(ws);
    let r = ui.relay_when(&b, &csrf, |r| r["state"] == "failed").await;
    assert_eq!(
        (state(&r), &r["relay_id"]),
        (("failed", "expired"), &json!(id))
    );

    // the same instance reconnects: the replaced connection's relay is over too
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let id = ui.relay_start(&b, &csrf, "github").await;
    relay_frame(&mut ws).await;
    relay_event(&mut ws, json!({"relay_id": id, "event": "waiting"})).await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "waiting").await;
    let mut again = vm_bridge(&ui, &host, &key, "inst-r").await;
    let r = ui.relay_when(&b, &csrf, |r| r["state"] == "failed").await;
    assert_eq!(state(&r), ("failed", "expired"));

    // a relay the connection never carried is not its to expire
    let live = ui.relay_start(&b, &csrf, "codex").await;
    relay_frame(&mut again).await;
    let (h, old) = (host.clone(), id.clone());
    let d2 = d.clone();
    let expired = blocking(move || {
        d2.with_tx(|t| Ok(t.machine_relay_expire(&h, &old, "2026-11-06T09:00:00.000Z")?))
            .unwrap()
    })
    .await;
    assert!(!expired);
    let r = ui.ok(&b, &csrf, "machine.status", json!({})).await["relay"].clone();
    assert_eq!(
        (&r["relay_id"], state(&r)),
        (&json!(live), ("starting", ""))
    );
    drop(ws);
    ui.server.shutdown().await.unwrap();
}

/// A start no bridge took within 60 s reads `failed` `expired`, is not sent when a bridge comes,
/// and a new start replaces it.
async fn a_relay_stuck_in_starting_reads_as_expired_after_60_s(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let stuck = ui.relay_start(&b, &csrf, "claude").await;
    let r = ui
        .relay_when(&b, &csrf, |r| r["relay_id"] == stuck.as_str())
        .await;
    assert_eq!(state(&r), ("starting", ""));
    d.advance_clock(59_000);
    let r = ui.ok(&b, &csrf, "machine.status", json!({})).await["relay"].clone();
    assert_eq!(state(&r), ("starting", ""), "inside the 60 s");
    d.advance_clock(2_000);
    let r = ui.ok(&b, &csrf, "machine.status", json!({})).await["relay"].clone();
    assert_eq!(state(&r), ("failed", "expired"));
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    assert!(
        relay_within(&mut ws, 2_500).await.is_none(),
        "the stale start is not sent"
    );
    let fresh = ui.relay_start(&b, &csrf, "claude").await;
    let f = relay_frame(&mut ws).await;
    assert_eq!(
        (&f["relay_id"], &f["action"]),
        (&json!(fresh), &json!("start"))
    );
    ui.server.shutdown().await.unwrap();
}

/// The relay ops are the signed-in owner's, in the browser: a dead or another owner's relay id
/// is refused and changes nothing, a code is taken only for the live Claude relay showing its URL
/// and only in form, and a bearer key (the machine's own included) is refused.
async fn a_relay_op_is_refused_for_another_owner_or_a_dead_relay_id(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let refused = |r: &Value, code: &str| {
        assert!(!r.to_string().contains(RELAY_CODE), "a refusal has no code");
        assert_eq!(r["error"]["code"], code, "{}", r["error"]["message"]);
    };
    let id = ui.relay_start(&b, &csrf, "claude").await;
    relay_frame(&mut ws).await;
    let code = |id: &str, c: &str| json!({"relay_id": id, "code": c});
    // not yet showing its URL
    let r = ui
        .relay(&b, &csrf, "machine.relay.code", code(&id, RELAY_CODE))
        .await;
    refused(&r, "conflict");
    relay_event(
        &mut ws,
        json!({"relay_id": id, "event": "url", "url": "https://claude.com/cai/oauth/authorize"}),
    )
    .await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    let dead = conde::util::new_ulid();
    let r = ui
        .relay(&b, &csrf, "machine.relay.code", code(&dead, RELAY_CODE))
        .await;
    refused(&r, "conflict");
    let r = ui
        .relay(&b, &csrf, "machine.relay.cancel", json!({"relay_id": dead}))
        .await;
    refused(&r, "conflict");
    for bad in [String::new(), "x".repeat(257), format!("{RELAY_CODE}\n")] {
        let r = ui
            .relay(&b, &csrf, "machine.relay.code", code(&id, &bad))
            .await;
        refused(&r, "bad_request");
    }
    let r = ui
        .relay(&b, &csrf, "machine.relay.start", json!({"flow": "gitlab"}))
        .await;
    refused(&r, "bad_request");
    assert!(ui.pending(&host).await.is_none(), "nothing was taken");

    // another owner, with a machine of their own, cannot touch this relay
    ui.env.invite("dpacsuta", "dpacsuta@dakai.example").await;
    ui.person("oid-dp", "dpacsuta@dakai.example", "D P");
    let (b2, csrf2) = ui.browser().await;
    vm_key(&ui, "dpacsuta", "vm-agent-dpacsuta").await;
    let r = ui
        .relay(&b2, &csrf2, "machine.relay.code", code(&id, RELAY_CODE))
        .await;
    refused(&r, "conflict");
    let r = ui
        .relay(&b2, &csrf2, "machine.relay.cancel", json!({"relay_id": id}))
        .await;
    refused(&r, "conflict");
    let theirs = ui.ok(&b2, &csrf2, "machine.status", json!({})).await;
    assert_eq!(theirs["relay"], Value::Null, "{theirs}");
    let mine = ui.ok(&b, &csrf, "machine.status", json!({})).await["relay"].clone();
    assert_eq!((&mine["relay_id"], state(&mine)), (&json!(id), ("url", "")));
    assert!(ui.pending(&host).await.is_none(), "nothing was taken");

    // a code for a flow that takes none
    let gh = ui.relay_start(&b, &csrf, "github").await;
    relay_frame(&mut ws).await;
    relay_event(
        &mut ws,
        json!({"relay_id": gh, "event": "url", "url": "https://github.com/login/device",
               "user_code": "WDJB-MJHT", "expires_at": "2026-11-06T10:15:00Z"}),
    )
    .await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "url").await;
    let r = ui
        .relay(&b, &csrf, "machine.relay.code", code(&gh, RELAY_CODE))
        .await;
    refused(&r, "conflict");

    // a bearer key, the machine's own, is not the browser
    for (op, body) in [
        ("machine.relay.start", json!({"flow": "claude"})),
        ("machine.relay.code", code(&gh, RELAY_CODE)),
        ("machine.relay.cancel", json!({"relay_id": gh})),
    ] {
        let req = json!({"v": 1, "type": "request", "request_id": "r1", "op": op, "body": body,
                         "idempotency_key": conde::util::new_ulid()});
        let r: Value = reqwest::Client::new()
            .post(ui.env.url("/v1/rpc"))
            .bearer_auth(&key)
            .json(&req)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        refused(&r, "forbidden");
    }
    let mine = ui.ok(&b, &csrf, "machine.status", json!({})).await["relay"].clone();
    assert_eq!((&mine["relay_id"], state(&mine)), (&json!(gh), ("url", "")));
    ui.server.shutdown().await.unwrap();
}

/// A cancel goes to the bridge as the contract's `cancel` frame and ends the relay; the helper's
/// later lines change nothing.
async fn a_cancel_reaches_the_bridge_and_ends_the_relay(d: Arc<Dispatcher>) {
    let ui = ui(&d).await;
    let (b, csrf, host, key) = relay_machine(&ui).await;
    let mut ws = vm_bridge(&ui, &host, &key, "inst-r").await;
    let id = ui.relay_start(&b, &csrf, "github").await;
    relay_frame(&mut ws).await;
    relay_event(&mut ws, json!({"relay_id": id, "event": "waiting"})).await;
    ui.relay_when(&b, &csrf, |r| r["state"] == "waiting").await;
    let r = ui
        .relay(&b, &csrf, "machine.relay.cancel", json!({"relay_id": id}))
        .await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        relay_frame(&mut ws).await,
        json!({"v": 1, "type": "relay", "relay_id": id, "action": "cancel"})
    );
    relay_event(&mut ws, json!({"relay_id": id, "event": "done"})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = ui.ok(&b, &csrf, "machine.status", json!({})).await["relay"].clone();
    assert_eq!(state(&r), ("cancelled", ""));
    assert!(ui.pending(&host).await.is_none());
    ui.server.shutdown().await.unwrap();
}

// --- disable removes the machine (unit 9) ---------------------------------------------------

const RG: &str = "/subscriptions/sub-1/resourceGroups/conde-vm";
const LOGIN_ROLE_ID: &str = "/subscriptions/sub-1/providers/Microsoft.Authorization/roleDefinitions/1c0163c0-47e6-4577-8991-ea5c82e286e4";
const MINE: &str = "6f8d1b6e-4c7a-5d2e-9a31-0b7e5c4d3a21";

fn vm_path(host: &str) -> String {
    format!("{RG}/providers/Microsoft.Compute/virtualMachines/{host}")
}

fn roles_path(host: &str) -> String {
    format!(
        "{}/providers/Microsoft.Authorization/roleAssignments",
        vm_path(host)
    )
}

fn assignment(name: &str, scope: &str, principal: &str, role: &str) -> Value {
    json!({"id": format!("{scope}/providers/Microsoft.Authorization/roleAssignments/{name}"),
           "name": name, "type": "Microsoft.Authorization/roleAssignments",
           "properties": {"scope": scope, "principalId": principal, "principalType": "User",
                          "roleDefinitionId": role}})
}

fn deployment(state: &str) -> Value {
    json!({"name": "d", "properties": {"provisioningState": state}})
}

fn vm(state: &str) -> Value {
    json!({"name": "vm", "properties": {"provisioningState": state}})
}

fn not_found() -> Value {
    json!({"error": {"code": "ResourceNotFound", "message": "gone"}})
}

impl Ui {
    /// A minted machine record of `user` (created when new) on `host` for owner `oid`.
    async fn machine(&self, user: &str, host: &str, oid: &str) {
        let (user, host, oid) = (user.to_string(), host.to_string(), oid.to_string());
        self.write(move |t| {
            let uid = t.ensure_user(&user)?;
            t.machine_request(&conde::store::NewMachine {
                user_id: uid,
                host_id: &host,
                owner_object_id: &oid,
                vm_size: web::DEFAULT_VM_SIZE,
                minted_at: Some("2026-11-06T08:00:00.000Z"),
            })?;
            Ok(())
        })
        .await;
    }

    /// The admin's disable (or enable) of `user`.
    async fn admin_set(&self, user: &str, action: &str) -> Value {
        let (status, v) = admin(
            &self.env,
            &format!("/v1/admin/users/{user}/{action}"),
            Some(json!({})),
            Some(ADMIN_KEY),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v
    }

    /// Waits until `user` has no machine record.
    async fn record_gone(&self, user: &str) {
        for _ in 0..200 {
            if self.record(user).await.is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{user}: {:?}", self.record(user).await);
    }
}

/// An admin's disable removes the owner's machine and nothing else: the running deployment is
/// cancelled once and read until it ended; of the four assignments listed at the VM (the
/// owner's login one, a foreign principal's login one at the same scope, the owner's other role
/// there, the owner's login one inherited from the resource group) only the first is deleted;
/// the VM's DELETE answers 202, its reads 200 until 404, then the record is gone. An enable
/// calls nothing, and another person's machine is never named.
async fn disabling_a_user_sets_deleting_cancels_a_running_deployment_deletes_the_one_login_assignment_and_the_vm_and_waits_for_404(
    d: Arc<Dispatcher>,
) {
    let (ui, arm) = ui_machines(&d).await;
    let host = "vm-agent-krisztian-dobmayer";
    ui.machine(USER, host, "oid-krisztian").await;
    ui.machine("levente-szabo", "vm-agent-levente-szabo", "oid-levente")
        .await;
    let deploy = format!("{DEPLOYMENTS}{host}");
    arm.route(
        "GET",
        &deploy,
        vec![
            (200, deployment("Running")),
            (200, deployment("Running")),
            (200, deployment("Canceled")),
        ],
    );
    arm.route(
        "POST",
        &format!("{deploy}/cancel"),
        vec![(204, Value::Null)],
    );
    let vm_id = vm_path(host);
    let reader = "/subscriptions/sub-1/providers/Microsoft.Authorization/roleDefinitions/acdd72a7-3385-48ef-bd42-f606fba81ae7";
    arm.route(
        "GET",
        &roles_path(host),
        vec![(
            200,
            json!({"value": [
                assignment(MINE, &vm_id, "oid-krisztian", LOGIN_ROLE_ID),
                assignment("1d4e7a90-2b3c-4d5e-8f60-718293a4b5c6", &vm_id, "oid-levente", LOGIN_ROLE_ID),
                assignment("2e5f8b01-3c4d-5e6f-9071-8293a4b5c6d7", &vm_id, "oid-krisztian", reader),
                assignment("3f6091c2-4d5e-6f70-a182-93a4b5c6d7e8", RG, "oid-krisztian", LOGIN_ROLE_ID),
            ]}),
        )],
    );
    let mine = format!("{}/{MINE}", roles_path(host));
    arm.route("DELETE", &mine, vec![(200, json!({}))]);
    arm.route("DELETE", &vm_id, vec![(202, Value::Null)]);
    arm.vm(
        host,
        vec![
            (200, vm("Succeeded")),
            (200, vm("Deleting")),
            (404, not_found()),
        ],
    );

    let v = ui.admin_set(USER, "enable").await;
    assert!(v.get("machine").is_none(), "{v}");
    assert!(
        arm.calls.lock().unwrap().is_empty(),
        "an enable calls Azure"
    );

    let v = ui.admin_set(USER, "disable").await;
    assert_eq!(
        (v["disabled"].as_bool(), v["machine"].as_str()),
        (Some(true), Some("deleting")),
        "{v}"
    );
    assert!(v.get("machine_error").is_none(), "{v}");
    ui.record_gone(USER).await;

    let calls = arm.calls.lock().unwrap().clone();
    let at = |method: &str, path: &str| {
        calls
            .iter()
            .position(|c| c.method == method && c.path == path)
            .unwrap_or_else(|| panic!("{method} {path}: {calls:#?}"))
    };
    assert_eq!(
        arm.on("POST", &format!("{deploy}/cancel")).len(),
        1,
        "one cancel"
    );
    assert_eq!(arm.on("GET", &deploy).len(), 3, "read until it ended");
    let list = &arm.on("GET", &roles_path(host))[0];
    assert!(list.query.contains("$filter=atScope()"), "{}", list.query);
    let deletes: Vec<String> = arm.calls("DELETE").into_iter().map(|c| c.path).collect();
    assert_eq!(
        deletes,
        vec![mine.clone(), vm_id.clone()],
        "exactly the owner's login and the VM"
    );
    let ended = calls
        .iter()
        .rposition(|c| c.method == "GET" && c.path == deploy)
        .unwrap();
    assert!(
        ended < at("GET", &roles_path(host)),
        "the deployment ended first"
    );
    assert!(
        at("DELETE", &mine) < at("DELETE", &vm_id),
        "the login before the VM"
    );
    assert_eq!(
        arm.on("GET", &vm_id).len(),
        3,
        "read before the DELETE, then until 404"
    );
    for c in &calls {
        assert!(
            c.path.starts_with(&format!("{RG}/")),
            "outside the resource group: {c:?}"
        );
        assert!(
            !c.path.contains("levente"),
            "another person's machine: {c:?}"
        );
        assert!(c.auth.starts_with("Bearer arm-token-"), "{c:?}");
    }
    assert!(
        ui.record("levente-szabo").await.is_some(),
        "the other machine stays"
    );
    ui.server.shutdown().await.unwrap();
}

/// Nothing left in Azure is a removal done: no deployment, no VM to list assignments at, no VM;
/// and a VM whose DELETE answers 404 is gone without a wait. Neither deletes anything else.
async fn an_absent_vm_counts_as_deleted(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    let (gone, late) = ("vm-agent-krisztian-dobmayer", "vm-agent-levente-szabo");
    ui.machine(USER, gone, "oid-krisztian").await;
    ui.machine("levente-szabo", late, "oid-levente").await;
    for host in [gone, late] {
        arm.route(
            "GET",
            &format!("{DEPLOYMENTS}{host}"),
            vec![(404, not_found())],
        );
        arm.route("GET", &roles_path(host), vec![(404, not_found())]);
    }
    arm.vm(late, vec![(200, vm("Succeeded"))]);
    arm.route("DELETE", &vm_path(late), vec![(404, not_found())]);

    let v = ui.admin_set(USER, "disable").await;
    assert_eq!(v["machine"], "deleting", "{v}");
    ui.record_gone(USER).await;
    assert!(arm.calls("DELETE").is_empty(), "{:?}", arm.calls("DELETE"));
    assert!(arm.calls("POST").is_empty());
    assert_eq!(arm.on("GET", &vm_path(gone)).len(), 1);

    let v = ui.admin_set("levente-szabo", "disable").await;
    assert_eq!(v["machine"], "deleting", "{v}");
    ui.record_gone("levente-szabo").await;
    let deletes: Vec<String> = arm.calls("DELETE").into_iter().map(|c| c.path).collect();
    assert_eq!(deletes, vec![vm_path(late)]);
    assert_eq!(
        arm.on("GET", &vm_path(late)).len(),
        1,
        "no wait after a 404"
    );
    ui.server.shutdown().await.unwrap();
}

/// A refused step is the record's `delete_error` and ends the task, the record still deleting
/// and the VM untouched; the next disable answers that error, keeps the first deleting time and
/// runs from where it stopped: the ended deployment is not cancelled again.
async fn a_failed_step_is_recorded_and_the_next_disable_call_resumes_from_it(d: Arc<Dispatcher>) {
    let (ui, arm) = ui_machines(&d).await;
    let host = "vm-agent-krisztian-dobmayer";
    ui.machine(USER, host, "oid-krisztian").await;
    let deploy = format!("{DEPLOYMENTS}{host}");
    arm.route(
        "GET",
        &deploy,
        vec![(200, deployment("Running")), (200, deployment("Canceled"))],
    );
    arm.route(
        "POST",
        &format!("{deploy}/cancel"),
        vec![(204, Value::Null)],
    );
    arm.route(
        "GET",
        &roles_path(host),
        vec![(
            200,
            json!({"value": [assignment(MINE, &vm_path(host), "oid-krisztian", LOGIN_ROLE_ID)]}),
        )],
    );
    let mine = format!("{}/{MINE}", roles_path(host));
    arm.route(
        "DELETE",
        &mine,
        vec![(
            403,
            json!({"error": {"code": "AuthorizationFailed", "message": "not allowed"}}),
        )],
    );
    arm.vm(host, vec![(200, vm("Succeeded"))]);

    let v = ui.admin_set(USER, "disable").await;
    assert_eq!(v["machine"], "deleting", "{v}");
    let m = ui.record_when(USER, |m| m.delete_error.is_some()).await;
    let error = m.delete_error.clone().unwrap();
    assert!(
        error.contains("403") && error.contains("AuthorizationFailed"),
        "{error}"
    );
    let first = m.deleting_at.clone().expect("deleting");
    assert!(
        arm.on("DELETE", &vm_path(host)).is_empty(),
        "the VM is untouched"
    );
    assert_eq!(arm.on("DELETE", &mine).len(), 1);

    arm.route("DELETE", &mine, vec![(204, Value::Null)]);
    arm.route("DELETE", &vm_path(host), vec![(202, Value::Null)]);
    arm.vm(host, vec![(200, vm("Succeeded")), (404, not_found())]);
    let v = ui.admin_set(USER, "disable").await;
    assert_eq!(v["machine"], "deleting", "{v}");
    assert_eq!(v["machine_error"], error.as_str(), "{v}");
    assert_eq!(
        ui.record(USER).await.and_then(|m| m.deleting_at),
        Some(first),
        "the first deleting time is kept"
    );
    ui.record_gone(USER).await;
    assert_eq!(
        arm.on("POST", &format!("{deploy}/cancel")).len(),
        1,
        "cancelled once"
    );
    assert_eq!(arm.on("DELETE", &mine).len(), 2);
    assert_eq!(arm.on("DELETE", &vm_path(host)).len(), 1);
    ui.server.shutdown().await.unwrap();
}
