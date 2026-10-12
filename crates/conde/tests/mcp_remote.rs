//! The remote MCP endpoint over the real server and dispatcher (docs/REMOTE-MCP.md): its OAuth
//! authorization server (metadata, authorize through the fake Entra sign-in, consent, code,
//! token, refresh rotation with reuse revocation), client metadata documents (an in-test
//! fetcher), `POST /mcp` in both eras, the human write path and the owner read path, the keys
//! page's connected apps, and the limits. Each case runs on SQLite and, with CONDE_TEST_PG, on
//! Postgres.

#![allow(dead_code)]

#[path = "pg/mod.rs"]
mod pg;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
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
use conde::dispatch::{Dispatcher, Principal};
use conde::protocol::Request;
use conde::web_store::WebBoard;
use conde_remote::web::cimd::{ClientMetadataFetcher, FetchError};
use conde_remote::web::{
    self, BridgeRegistry, McpGrant, McpLimits, McpTokens, WebConfig, WebSetup, WebState, WebStore,
};
use conde_remote::{ServerConfig, ServerHandle};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, sha2::Sha256};
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;

include!("../../conde-remote/tests/support/web_harness.rs");

/// The user the fake provider signs in (from its preferred_username).
const USER: &str = "krisztian-dobmayer";
/// The test client's metadata URL (its client_id) and its loopback redirect.
const CLIENT: &str = "https://claude.ai/oauth/test-client";
const REDIRECT: &str = "http://localhost/callback";
/// The redirect the client uses: the listed loopback URI with a port.
const REDIRECT_PORTED: &str = "http://localhost:53111/callback";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

async fn board(on_pg: bool) -> Option<Arc<Dispatcher>> {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let url = if on_pg {
        match blocking(pg::fresh_database).await {
            Some(url) => Some(url),
            None => {
                eprintln!("skipped: set CONDE_TEST_PG to run the remote MCP suite on Postgres");
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

fn run<F: Future<Output = ()>>(on_pg: bool, case: impl FnOnce(Arc<Dispatcher>) -> F) {
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
    metadata_and_the_challenge_point_at_this_server,
    a_connected_app_reads_and_writes_as_the_human_until_revoked,
    a_replayed_refresh_token_revokes_the_whole_grant,
    client_metadata_refusals_are_pages_never_redirects,
    pkce_code_and_audience_refusals,
    protocol_errors_map_to_their_statuses,
    the_buckets_run_out,
    no_human_name_refuses_writes_with_the_hint,
    a_disabled_user_is_refused_at_the_next_request,
    grants_beyond_the_cap_revoke_the_oldest_of_that_client,
    connecting_past_the_cap_keeps_the_newest_grants_live,
    expired_rows_go_at_every_refresh_and_a_spent_token_stays_with_its_grant,
    the_sweep_purges_expired_rows_without_a_refresh,
);

// --- the in-test client metadata documents --------------------------------------------------------

/// Documents by URL; anything else is unreachable. Counts fetches (the cache is checked).
#[derive(Default)]
struct Docs {
    docs: Mutex<HashMap<String, Result<Vec<u8>, FetchError>>>,
    fetches: AtomicUsize,
}

impl ClientMetadataFetcher for Docs {
    fn fetch<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, FetchError>> + Send + 'a>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        let r = self.docs.lock().unwrap().get(url).cloned().unwrap_or(Err(FetchError::Unreachable));
        Box::pin(async move { r })
    }
}

impl Docs {
    fn put(&self, url: &str, doc: Result<Vec<u8>, FetchError>) {
        self.docs.lock().unwrap().insert(url.into(), doc);
    }
}

fn client_doc(id: &str) -> Value {
    json!({"client_id": id, "client_name": "Test <Client>", "client_uri": "https://claude.ai",
           "redirect_uris": [REDIRECT, "https://claude.ai/api/mcp/auth_callback"],
           "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
           "token_endpoint_auth_method": "none"})
}

fn doc(v: Value) -> Result<Vec<u8>, FetchError> {
    Ok(v.to_string().into_bytes())
}

// --- the server ---------------------------------------------------------------------------------

struct Mcp {
    env: Env<WebBoard>,
    server: ServerHandle,
    d: Arc<Dispatcher>,
    docs: Arc<Docs>,
    http: reqwest::Client,
}

async fn mcp(d: &Arc<Dispatcher>) -> Mcp {
    mcp_with(d, |_| {}).await
}

async fn mcp_with(d: &Arc<Dispatcher>, tweak: impl FnOnce(&mut WebConfig)) -> Mcp {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let idp = start_idp().await;
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let base = format!("http://127.0.0.1:{port}");
    let mut entra = WebConfig::entra(TENANT, CLIENT_ID, CLIENT_SECRET, &format!("{base}/auth/callback")).unwrap();
    entra.issuer = idp.issuer.clone();
    let docs = Arc::new(Docs::default());
    docs.put(CLIENT, doc(client_doc(CLIENT)));
    entra.mcp.fetcher = docs.clone();
    tweak(&mut entra);
    let store = Arc::new(WebBoard::new(d.clone()));
    let web = WebSetup { entra: Some(entra), admin_key: None, store: store.clone() };
    let server = conde::cloud::start(
        d.clone(),
        format!("127.0.0.1:{port}").parse().unwrap(),
        ServerConfig { web: Some(web), ..ServerConfig::default() },
    )
    .await
    .unwrap();
    let env = Env { base, idp, store, registry: Arc::new(FakeRegistry::default()), clock: Arc::new(AtomicI64::new(START)) };
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    Mcp { env, server, d: d.clone(), docs, http }
}

/// One PKCE pair.
struct Pkce {
    verifier: String,
    challenge: String,
}

fn pkce() -> Pkce {
    let verifier = b64(&rand::random::<[u8; 32]>());
    let challenge = b64(&sha2::Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

#[derive(Clone, Debug)]
struct Tokens {
    access: String,
    refresh: String,
}

fn query(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

fn location(r: &reqwest::Response) -> String {
    r.headers().get(header::LOCATION).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
}

impl Mcp {
    fn url(&self, path: &str) -> String {
        self.env.url(path)
    }

    fn resource(&self) -> String {
        self.url("/mcp")
    }

    /// The authorization request of the test client, with `extra` replacing or adding parameters
    /// (an empty value drops one).
    fn authorize_url(&self, p: &Pkce, state: &str, extra: &[(&str, &str)]) -> String {
        let resource = self.resource();
        let mut params: Vec<(String, String)> = [
            ("response_type", "code"),
            ("client_id", CLIENT),
            ("redirect_uri", REDIRECT_PORTED),
            ("code_challenge", p.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("resource", resource.as_str()),
            ("scope", "conde"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in extra {
            params.retain(|(pk, _)| pk != k);
            if !v.is_empty() {
                params.push((k.to_string(), v.to_string()));
            }
        }
        let mut url = reqwest::Url::parse(&self.url("/oauth/authorize")).unwrap();
        url.query_pairs_mut().extend_pairs(params.iter());
        url.to_string()
    }

    /// The consent page of a browser (signing it in through the fake provider when needed).
    async fn consent_page(&self, b: &Browser, url: &str) -> String {
        let (status, at, page) = b.get(url).await;
        assert_eq!(status, StatusCode::OK, "{at}: {page}");
        assert!(at.starts_with(&self.url("/oauth/authorize")), "{at}");
        assert!(page.contains("Allow"), "{page}");
        page
    }

    /// Answers the consent page; returns the redirect's query.
    async fn answer(&self, b: &Browser, url: &str, page: &str, decision: &str) -> HashMap<String, String> {
        let mut form: Vec<(String, String)> = query(url).into_iter().collect();
        form.push(("csrf".into(), csrf_of(page)));
        form.push(("decision".into(), decision.into()));
        let r = b.step.post(self.url("/oauth/authorize")).form(&form).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::SEE_OTHER, "{:?}", r.text().await);
        let to = location(&r);
        assert!(to.starts_with(REDIRECT_PORTED), "{to}");
        query(&to)
    }

    async fn token(&self, form: &[(&str, &str)]) -> (StatusCode, Value) {
        let r = self.http.post(self.url("/oauth/token")).form(form).send().await.unwrap();
        let status = r.status();
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        (status, r.json().await.unwrap())
    }

    async fn redeem(&self, code: &str, p: &Pkce) -> (StatusCode, Value) {
        let resource = self.resource();
        self.token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_PORTED),
            ("client_id", CLIENT),
            ("code_verifier", &p.verifier),
            ("resource", &resource),
        ])
        .await
    }

    /// A one-time code from a signed-in (or signing-in) browser's consent.
    async fn code(&self, b: &Browser, p: &Pkce) -> String {
        let url = self.authorize_url(p, "st-1", &[]);
        let page = self.consent_page(b, &url).await;
        let q = self.answer(b, &url, &page, "allow").await;
        assert_eq!(q["state"], "st-1");
        assert_eq!(q["iss"], self.env.base, "RFC 9207 issuer");
        q["code"].clone()
    }

    /// The whole connect: consent, code, token.
    async fn connect(&self, b: &Browser) -> Tokens {
        let p = pkce();
        let code = self.code(b, &p).await;
        let (status, t) = self.redeem(&code, &p).await;
        assert_eq!(status, StatusCode::OK, "{t}");
        assert_eq!((t["token_type"].as_str(), t["scope"].as_str()), (Some("Bearer"), Some("conde")));
        assert_eq!(t["expires_in"], 3600);
        tokens(&t)
    }

    async fn refresh(&self, refresh: &str) -> (StatusCode, Value) {
        self.token(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", CLIENT)]).await
    }

    /// One raw POST /mcp.
    async fn post(&self, token: Option<&str>, headers: &[(&str, &str)], body: impl Into<reqwest::Body>) -> (StatusCode, HeaderMap, Value) {
        let mut req = self.http.post(self.resource()).header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let r = req.body(body).send().await.unwrap();
        let (status, h) = (r.status(), r.headers().clone());
        let text = r.text().await.unwrap();
        (status, h, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A modern (2026-07-28) request with its mirrored headers.
    async fn modern(&self, token: &str, method: &str, mut params: Value) -> (StatusCode, Value) {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                                 "io.modelcontextprotocol/clientCapabilities": {}});
        let name = params["name"].as_str().map(str::to_string);
        let mut headers = vec![("mcp-protocol-version", "2026-07-28"), ("mcp-method", method)];
        if let Some(n) = name.as_deref() {
            headers.push(("mcp-name", n));
        }
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let (status, _, v) = self.post(Some(token), &headers, body.to_string()).await;
        (status, v)
    }

    /// A tool call: (is_error, text).
    async fn tool(&self, token: &str, name: &str, args: Value) -> (bool, String) {
        let (status, v) = self.modern(token, "tools/call", json!({"name": name, "arguments": args})).await;
        assert_eq!(status, StatusCode::OK, "{name}: {v}");
        let r = &v["result"];
        (r["isError"] == true, r["content"][0]["text"].as_str().unwrap_or_default().to_string())
    }

    async fn ok(&self, token: &str, name: &str, args: Value) -> String {
        let (err, text) = self.tool(token, name, args.clone()).await;
        assert!(!err, "{name} {args}: {text}");
        text
    }

    async fn refused(&self, token: &str, name: &str, args: Value) -> String {
        let (err, text) = self.tool(token, name, args.clone()).await;
        assert!(err, "{name} {args} should fail: {text}");
        text
    }

    async fn principal(&self, user: &str, host: &str) -> Principal {
        let (d, user, host) = (self.d.clone(), user.to_string(), host.to_string());
        blocking(move || d.principal(&user, &host, false).unwrap()).await
    }

    /// One op through the dispatcher (a CLI or bridge request).
    async fn op(&self, p: &Principal, actor: Option<&str>, op: &str, body: Value) -> Value {
        let (d, p) = (self.d.clone(), p.clone());
        let mut req = Request::new(conde::util::new_ulid(), op, body);
        req.actor = actor.map(Into::into);
        req.idempotency_key = Some(conde::util::new_ulid());
        let resp = blocking(move || d.dispatch(&p, &req)).await;
        assert!(resp.ok, "{op}: {:?}", resp.error);
        resp.result.unwrap()
    }
}

fn tokens(t: &Value) -> Tokens {
    let (access, refresh) = (t["access_token"].as_str().unwrap(), t["refresh_token"].as_str().unwrap());
    assert!(access.starts_with("conde_at_") && access.len() == 73, "{access}");
    assert!(refresh.starts_with("conde_rt_") && refresh.len() == 73, "{refresh}");
    Tokens { access: access.into(), refresh: refresh.into() }
}

/// A browser signed in through the fake provider.
async fn signed_in(m: &Mcp) -> Browser {
    let b = Browser::new();
    b.sign_in(&m.env).await;
    b
}

/// The board: the signed-in user's agents worker (Claude) and helper (Codex) on k-host, and
/// the human name `krisztian` published from the migrated host k-desk; david's scout and
/// ranger on d-host. DMs scout<->worker, scout->ranger (between david's agents); #team (worker,
/// scout).
struct Cast {
    dv: Principal,
    scout: &'static str,
    private_dm: Value,
    worker_dm: Value,
}

async fn cast(m: &Mcp, with_human: bool) -> Cast {
    let k = m.principal(USER, "k-host").await;
    m.op(&k, Some("claude:sess-worker"), "agent.register", json!({"name": "worker"})).await;
    m.op(&k, Some("codex:thread-helper"), "agent.register", json!({"name": "helper"})).await;
    if with_human {
        let desk = m.principal(USER, "k-desk").await;
        let d = m.d.clone();
        blocking(move || d.with_tx(|t| Ok(t.set_host_mailbox_only("k-desk")?)).unwrap()).await;
        m.op(&desk, None, "mailbox.publish", json!({"name": "krisztian", "human": true})).await;
    }
    let dv = m.principal("david", "d-host").await;
    let (scout, ranger) = ("codex:thread-scout", "claude:sess-ranger");
    m.op(&dv, Some(scout), "agent.register", json!({"name": "scout"})).await;
    m.op(&dv, Some(ranger), "agent.register", json!({"name": "ranger"})).await;
    m.op(&dv, Some(scout), "message.send", json!({"to": {"agent": format!("{USER}/worker")}, "text": "status?"})).await;
    let worker_dm = m
        .op(&k, Some("claude:sess-worker"), "message.send", json!({"to": {"agent": "david/scout"}, "text": "green across the board"}))
        .await;
    let private_dm = m
        .op(&dv, Some(scout), "message.send", json!({"to": {"agent": "ranger"}, "text": "private between david's agents"}))
        .await;
    m.op(&k, Some("claude:sess-worker"), "channel.create", json!({"channel": "#team"})).await;
    m.op(&dv, Some(scout), "channel.join", json!({"channel": "#team"})).await;
    Cast { dv, scout, private_dm, worker_dm }
}

fn id_of(v: &Value) -> String {
    v["message_id"].as_str().unwrap().to_string()
}

// --- metadata -----------------------------------------------------------------------------------

async fn metadata_and_the_challenge_point_at_this_server(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let base = m.env.base.clone();
    let challenge = format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\", scope=\"conde\"");
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}});
    let (status, h, _) = m.post(None, &[], init.to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "initialize needs the token too");
    assert_eq!(h[header::WWW_AUTHENTICATE], challenge.as_str());
    let (status, h, _) = m.post(Some("conde_at_nope"), &[], init.to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(h[header::WWW_AUTHENTICATE], format!("{challenge}, error=\"invalid_token\"").as_str());

    for path in ["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource"] {
        let v: Value = m.http.get(m.url(path)).send().await.unwrap().json().await.unwrap();
        assert_eq!(
            v,
            json!({"resource": format!("{base}/mcp"), "authorization_servers": [base], "scopes_supported": ["conde"],
                   "bearer_methods_supported": ["header"]}),
            "{path}"
        );
    }
    let v: Value = m.http.get(m.url("/.well-known/oauth-authorization-server")).send().await.unwrap().json().await.unwrap();
    assert_eq!(
        v,
        json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/oauth/authorize"),
            "token_endpoint": format!("{base}/oauth/token"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "client_id_metadata_document_supported": true,
            "scopes_supported": ["conde"],
            "authorization_response_iss_parameter_supported": true,
        })
    );
    assert!(v.get("registration_endpoint").is_none());
    for method in [reqwest::Method::GET, reqwest::Method::DELETE] {
        let r = m.http.request(method.clone(), m.resource()).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
    m.server.shutdown().await.unwrap();
}

// --- the full flow ------------------------------------------------------------------------------

async fn a_connected_app_reads_and_writes_as_the_human_until_revoked(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    // The user exists, bound to the provider's identity (another browser signed in), with a cast.
    signed_in(&m).await;
    let c = cast(&m, true).await;
    // Signed out: authorize goes through the board's sign-in to the consent page.
    let b = Browser::new();
    let p = pkce();
    let url = m.authorize_url(&p, "st-flow", &[]);
    let (status, to) = b.peek(&url).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(to.starts_with("/auth/login?next=%2Foauth%2Fauthorize%3F"), "{to}");
    let page = m.consent_page(&b, &url).await;
    for shown in ["claude.ai", "&#8220;Test &#60;Client&#62;&#8221;", "localhost", &format!("{USER}/krisztian")] {
        assert!(page.contains(shown) || page.contains(&shown.replace("&#60;", "&lt;").replace("&#62;", "&gt;")), "{shown}: {page}");
    }
    let csp = b.step.get(&url).send().await.unwrap().headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().to_string();
    assert!(csp.contains("form-action 'self' http://localhost:53111"), "{csp}");
    let q = m.answer(&b, &url, &page, "allow").await;
    assert_eq!((q["state"].as_str(), q["iss"].as_str()), ("st-flow", m.env.base.as_str()));
    let (status, t) = m.redeem(&q["code"], &p).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let tk = tokens(&t);
    let (status, again) = m.redeem(&q["code"], &p).await;
    assert_eq!((status, again["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "a code is single use");

    // Both eras list the human subset of tools.
    let (status, v) = m.modern(&tk.access, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["agents", "inbox", "read", "search", "send", "react"]);
    assert_eq!(v["result"]["resultType"], "complete");
    for t in v["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{t}");
        let props = t["inputSchema"]["properties"].as_object().unwrap();
        assert!(!props.contains_key("board") && !props.contains_key("actor") && !props.contains_key("from"), "{t}");
    }
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}});
    let (status, h, v) = m.post(Some(&tk.access), &[], init.to_string()).await;
    assert_eq!((status, v["result"]["protocolVersion"].as_str()), (StatusCode::OK, Some("2025-06-18")), "{v}");
    assert!(h.get("mcp-session-id").is_none(), "no sessions");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let (status, _, v) = m.post(Some(&tk.access), &[("mcp-protocol-version", "2025-06-18")], list.to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 6);
    assert!(v["result"].get("resultType").is_none(), "legacy shape: {v}");

    // Send as the human name, get a reply in the inbox, read it (marked read).
    let sent = m.ok(&tk.access, "send", json!({"to": "david/scout", "text": "hello from the phone"})).await;
    assert!(sent.starts_with("queued 01") && sent.ends_with(" to david/scout"), "plain id: {sent}");
    let reply = m
        .op(&c.dv, Some(c.scout), "message.send", json!({"to": {"agent": format!("{USER}/krisztian")}, "text": "got it, thanks"}))
        .await;
    let inbox = m.ok(&tk.access, "inbox", json!({})).await;
    let reply_id = id_of(&reply);
    assert!(inbox.starts_with(&format!("dm\t{reply_id}\tdavid/scout\t")), "no board column, plain id: {inbox}");
    let read = m.ok(&tk.access, "read", json!({})).await;
    assert!(read.contains(&format!("=== {reply_id}\nFrom: david/scout\nTo: {USER}/krisztian")) && read.contains("got it, thanks"), "{read}");
    assert_eq!(m.ok(&tk.access, "inbox", json!({})).await, "(no unread messages)");
    let reacted = m.ok(&tk.access, "react", json!({"id": reply_id})).await;
    assert_eq!(reacted, format!("reacted \u{1F44D} to {reply_id} in dm"));
    // The human's sent message reached scout's mailbox as the human name.
    let scout_inbox = m.op(&c.dv, Some(c.scout), "inbox.list", json!({})).await;
    assert!(scout_inbox.to_string().contains(&format!("{USER}/krisztian")), "{scout_inbox}");

    // An owned agent's conversation reads through the owner view, nothing marked.
    let h = m.ok(&tk.access, "read", json!({"agent": "worker", "target": "david/scout"})).await;
    assert!(h.starts_with(&format!("--- {USER}/worker with david/scout\n")), "{h}");
    let (q1, q2) = (h.find("status?").unwrap(), h.find("green across the board").unwrap());
    assert!(q1 < q2, "oldest first: {h}");
    let by_id = m.ok(&tk.access, "read", json!({"target": id_of(&c.worker_dm)})).await;
    assert!(by_id.contains("green across the board") && by_id.contains("Delivery: david/scout"), "{by_id}");
    let mine = m.ok(&tk.access, "agents", json!({"mine": true})).await;
    assert!(mine.contains(&format!("{USER}/worker (claude)")) && mine.contains("  dm david/scout"), "{mine}");
    assert!(!mine.contains("ranger"), "{mine}");
    let directory = m.ok(&tk.access, "agents", json!({})).await;
    assert!(directory.contains("david/ranger\tclaude on d-host"), "{directory}");
    assert!(m.ok(&tk.access, "search", json!({"pattern": "green"})).await.contains("green across the board"));

    // A DM between another owner's agents stays invisible to read, search and react.
    let private = id_of(&c.private_dm);
    let refused = m.refused(&tk.access, "read", json!({"target": private})).await;
    assert_eq!(refused, format!("conde: no message '{private}' visible to your agents"));
    for agent in ["david/scout", "scout"] {
        let r = m.refused(&tk.access, "read", json!({"agent": agent, "target": "david/ranger"})).await;
        assert!(!r.contains("private between"), "{r}");
    }
    let r = m.ok(&tk.access, "read", json!({"agent": "worker", "target": "david/ranger"})).await;
    assert!(!r.contains("private between"), "{r}");
    assert_eq!(m.ok(&tk.access, "search", json!({"pattern": "private"})).await, "(no matches)");
    m.refused(&tk.access, "react", json!({"id": private})).await;
    // No tool takes an identity or a board.
    for extra in ["actor", "from", "board"] {
        let mut args = json!({"to": "david/scout", "text": "x"});
        args[extra] = json!("claude:sess-worker");
        let text = m.refused(&tk.access, "send", args).await;
        assert_eq!(text, format!("conde: unknown argument '{extra}' for send"));
    }

    // The keys page lists the app; Revoke ends it at once.
    let keys = b.get(&m.url("/keys")).await.2;
    assert!(keys.contains("Connected apps") && keys.contains("<code>claude.ai</code>"), "{keys}");
    let at = keys.find("/keys/apps/").unwrap() + "/keys/apps/".len();
    let grant = keys[at..].split('/').next().unwrap().to_string();
    let r = b.post_form(&m.url(&format!("/keys/apps/{grant}/revoke")), &[("csrf", "0".repeat(64).as_str())]).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "CSRF as the keys page");
    assert_eq!(m.ok(&tk.access, "inbox", json!({})).await, "(no unread messages)", "still live");
    let r = b.post_form(&m.url(&format!("/keys/apps/{grant}/revoke")), &[("csrf", csrf_of(&keys).as_str())]).await;
    assert_eq!((r.status(), location(&r).as_str()), (StatusCode::SEE_OTHER, "/keys"));
    let (status, h, _) = m.post(Some(&tk.access), &[], list.to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(h[header::WWW_AUTHENTICATE].to_str().unwrap().contains("error=\"invalid_token\""));
    let (status, v) = m.refresh(&tk.refresh).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "the refresh token ended with it");
    assert!(!b.get(&m.url("/keys")).await.2.contains("<code>claude.ai</code>"), "no longer listed");
    m.server.shutdown().await.unwrap();
}

async fn a_replayed_refresh_token_revokes_the_whole_grant(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let first = m.connect(&b).await;
    let other = m.connect(&b).await;
    let mut current = first.clone();
    for _ in 0..3 {
        let (status, v) = m.refresh(&current.refresh).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        current = tokens(&v);
        m.ok(&current.access, "inbox", json!({})).await;
    }
    let (status, v) = m.refresh(&first.refresh).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{v}");
    let (status, _, _) = m.post(Some(&current.access), &[], json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the latest access token died with its family");
    let (status, v) = m.refresh(&current.refresh).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "and the latest refresh token");
    // Another grant of the same user and client is untouched.
    m.ok(&other.access, "inbox", json!({})).await;
    // A refresh token of another client is refused without spending it.
    let (status, _) = m.token(&[("grant_type", "refresh_token"), ("refresh_token", &other.refresh),
                                 ("client_id", "https://claude.ai/oauth/other")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v) = m.refresh(&other.refresh).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    m.server.shutdown().await.unwrap();
}

// --- refusals -----------------------------------------------------------------------------------

async fn client_metadata_refusals_are_pages_never_redirects(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    let p = pkce();
    let mut mismatch = client_doc("https://claude.ai/oauth/mismatch");
    mismatch["client_id"] = CLIENT.into();
    let mut incomplete = client_doc("https://claude.ai/oauth/incomplete");
    incomplete.as_object_mut().unwrap().remove("redirect_uris");
    let mut secret = client_doc("https://claude.ai/oauth/secret");
    secret["token_endpoint_auth_method"] = "client_secret_post".into();
    for (path, body) in [
        ("unreachable", Err(FetchError::Unreachable)),
        ("timeout", Err(FetchError::Timeout)),
        ("redirect", Err(FetchError::Redirect)),
        ("status", Err(FetchError::Status(500))),
        ("large", Err(FetchError::TooLarge)),
        ("notjson", Ok(b"<html>".to_vec())),
        ("mismatch", doc(mismatch)),
        ("incomplete", doc(incomplete)),
        ("secret", doc(secret)),
    ] {
        m.docs.put(&format!("https://claude.ai/oauth/{path}"), body);
    }
    let cases: Vec<(String, &str, &str)> = vec![
        ("http://claude.ai/oauth/test-client".into(), REDIRECT_PORTED, "client_id_not_https"),
        ("https://evil.example/oauth/c".into(), REDIRECT_PORTED, "client_host_not_allowed"),
        ("https://sub.claude.ai/oauth/c".into(), REDIRECT_PORTED, "client_host_not_allowed"),
        ("https://claude.ai".into(), REDIRECT_PORTED, "client_id_malformed"),
        ("".into(), REDIRECT_PORTED, "client_id_missing"),
        ("https://claude.ai/oauth/unreachable".into(), REDIRECT_PORTED, "metadata_unreachable"),
        ("https://claude.ai/oauth/timeout".into(), REDIRECT_PORTED, "metadata_timeout"),
        ("https://claude.ai/oauth/redirect".into(), REDIRECT_PORTED, "metadata_redirected"),
        ("https://claude.ai/oauth/status".into(), REDIRECT_PORTED, "metadata_status"),
        ("https://claude.ai/oauth/large".into(), REDIRECT_PORTED, "metadata_too_large"),
        ("https://claude.ai/oauth/notjson".into(), REDIRECT_PORTED, "metadata_not_json"),
        ("https://claude.ai/oauth/mismatch".into(), REDIRECT_PORTED, "client_id_mismatch"),
        ("https://claude.ai/oauth/incomplete".into(), REDIRECT_PORTED, "metadata_incomplete"),
        ("https://claude.ai/oauth/secret".into(), REDIRECT_PORTED, "unsupported_auth_method"),
        (CLIENT.into(), "http://localhost:53111/elsewhere", "redirect_uri_not_registered"),
        (CLIENT.into(), "https://evil.example/callback", "redirect_uri_not_registered"),
        (CLIENT.into(), "", "redirect_uri_missing"),
    ];
    for (client, redirect, code) in &cases {
        let url = m.authorize_url(&p, "st", &[("client_id", client), ("redirect_uri", redirect)]);
        let r = b.step.get(&url).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{code}");
        assert!(r.headers().get(header::LOCATION).is_none(), "{code}: never a redirect");
        let page = r.text().await.unwrap();
        assert!(page.contains(&format!("invalid_client: {code}")), "{code}: {page}");
    }
    // A repeated parameter is refused the same way.
    let url = format!("{}&state=again", m.authorize_url(&p, "st", &[]));
    let r = b.step.get(&url).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(r.text().await.unwrap().contains("duplicate_parameter"));
    // The good document is fetched once and then served from the cache.
    let before = m.docs.fetches.load(Ordering::SeqCst);
    m.consent_page(&b, &m.authorize_url(&p, "st", &[])).await;
    m.consent_page(&b, &m.authorize_url(&p, "st", &[])).await;
    assert!(m.docs.fetches.load(Ordering::SeqCst) - before <= 1);
    m.server.shutdown().await.unwrap();
}

async fn pkce_code_and_audience_refusals(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let p = pkce();
    // After the redirect URI is trusted, errors go back to the client with state and issuer.
    let other_resource = m.url("/other");
    for (extra, error) in [
        (vec![("code_challenge_method", "plain")], "invalid_request"),
        (vec![("code_challenge_method", "")], "invalid_request"),
        (vec![("code_challenge", "")], "invalid_request"),
        (vec![("code_challenge", "short")], "invalid_request"),
        (vec![("response_type", "token")], "unsupported_response_type"),
        (vec![("resource", other_resource.as_str())], "invalid_target"),
    ] {
        let url = m.authorize_url(&p, "st-x", &extra);
        let (status, to) = b.peek(&url).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{extra:?}");
        let q = query(&to);
        assert_eq!((q["error"].as_str(), q["state"].as_str()), (error, "st-x"), "{extra:?}: {to}");
        assert_eq!(q["iss"], m.env.base);
    }
    let (_, to) = b.peek(&m.authorize_url(&p, "", &[])).await;
    assert_eq!(query(&to)["error"], "invalid_request", "state is required: {to}");

    // Consent needs the session's CSRF token; deny goes back with access_denied.
    let url = m.authorize_url(&p, "st-c", &[]);
    let page = m.consent_page(&b, &url).await;
    let mut form: Vec<(String, String)> = query(&url).into_iter().collect();
    form.push(("decision".into(), "allow".into()));
    let r = b.step.post(m.url("/oauth/authorize")).form(&form).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "no CSRF");
    let r = reqwest::Client::new().post(m.url("/oauth/authorize")).form(&form).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "no session");
    let q = m.answer(&b, &url, &page, "deny").await;
    assert_eq!((q["error"].as_str(), q["state"].as_str()), ("access_denied", "st-c"));

    // The code: bound to its verifier, client and redirect URI, single use (whatever the outcome).
    let resource = m.resource();
    let code = m.code(&b, &p).await;
    let wrong = pkce();
    let (status, v) = m.redeem(&code, &wrong).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "wrong verifier");
    let (_, v) = m.redeem(&code, &p).await;
    assert_eq!(v["error"], "invalid_grant", "the failed attempt spent the code");
    for (key, value) in [("redirect_uri", "http://localhost:9/callback"), ("client_id", "https://claude.ai/oauth/x")] {
        let code = m.code(&b, &p).await;
        let mut form = vec![("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", REDIRECT_PORTED),
                            ("client_id", CLIENT), ("code_verifier", p.verifier.as_str()), ("resource", resource.as_str())];
        form.retain(|(k, _)| *k != key);
        form.push((key, value));
        let (status, v) = m.token(&form).await;
        assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{key}");
    }
    let code = m.code(&b, &p).await;
    let (status, v) = m.token(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT_PORTED),
                                ("client_id", CLIENT), ("code_verifier", &p.verifier), ("resource", &m.url("/other"))]).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_target")));
    let (_, v) = m.token(&[("grant_type", "password")]).await;
    assert_eq!(v["error"], "unsupported_grant_type");

    // A token issued for another resource is refused here; so is a host key.
    let (store, now) = (m.env.store.clone(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64);
    let user_id = blocking({ let s = store.clone(); move || s.user_by_identity(TENANT, "oid-krisztian").unwrap().unwrap().user_id }).await;
    let foreign = format!("conde_at_{}", "ab".repeat(32));
    let grant = McpGrant {
        grant_id: "g-foreign".into(), user_id, client_id: CLIENT.into(), client_host: "claude.ai".into(), client_name: None,
        resource: "https://other.example/mcp".into(), created_at: now, last_used_at: None, expires_at: now + 3600, revoked_at: None,
    };
    let tokens = McpTokens { access_digest: conde_remote::token_digest(&foreign), refresh_digest: "r-foreign".into(),
                             access_ttl: 600, refresh_ttl: 600 };
    let s = store.clone();
    assert!(blocking(move || s.create_mcp_grant(&grant, &tokens, 10, now).unwrap()).await.is_some());
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    let (status, h, _) = m.post(Some(&foreign), &[], ping.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong audience");
    assert!(h[header::WWW_AUTHENTICATE].to_str().unwrap().contains("resource_metadata="));
    let (status, _, _) = m.post(Some(&conde_remote::web::new_host_key()), &[], ping).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a host key is not an MCP token");
    m.server.shutdown().await.unwrap();

    // An expired code.
    let m = mcp_with(&d, |c| c.mcp.code_ttl = Duration::from_secs(1)).await;
    let b = signed_in(&m).await;
    let code = m.code(&b, &p).await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let (status, v) = m.redeem(&code, &p).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{v}");
    m.server.shutdown().await.unwrap();
}

async fn protocol_errors_map_to_their_statuses(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let tk = m.connect(&b).await;
    let t = Some(tk.access.as_str());
    let modern = |method: &str, name: Option<&str>| {
        let mut params = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                                           "io.modelcontextprotocol/clientCapabilities": {}}});
        if let Some(n) = name {
            params["name"] = json!(n);
            params["arguments"] = json!({});
        }
        json!({"jsonrpc": "2.0", "id": 9, "method": method, "params": params}).to_string()
    };
    let h_ok = [("mcp-protocol-version", "2026-07-28"), ("mcp-method", "tools/call"), ("mcp-name", "inbox")];
    let (status, _, v) = m.post(t, &h_ok, modern("tools/call", Some("inbox"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for headers in [&h_ok[1..], &[h_ok[0], h_ok[1]][..], &[h_ok[0], ("mcp-method", "tools/list"), h_ok[2]][..],
                    &[h_ok[0], h_ok[1], ("mcp-name", "send")][..], &[("mcp-protocol-version", "2025-06-18"), h_ok[1], h_ok[2]][..]] {
        let (status, _, v) = m.post(t, headers, modern("tools/call", Some("inbox"))).await;
        assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32020)), "{headers:?}: {v}");
    }
    let encoded = [h_ok[0], h_ok[1], ("mcp-name", "=?base64?aW5ib3g=?=")];
    assert_eq!(m.post(t, &encoded, modern("tools/call", Some("inbox"))).await.0, StatusCode::OK, "base64 sentinel decoded");
    let (status, _, v) = m.post(t, &[], "{not json").await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32700)));
    let (status, _, v) = m.post(t, &[], "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]").await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32600)));
    let mut bad = serde_json::from_str::<Value>(&modern("tools/list", None)).unwrap();
    bad["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("1999-01-01");
    let (status, _, v) = m.post(t, &[("mcp-protocol-version", "1999-01-01"), ("mcp-method", "tools/list")], bad.to_string()).await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32022)), "{v}");
    assert_eq!(v["error"]["data"]["supported"][0], "2026-07-28");
    let (status, _, v) = m.post(t, &[("mcp-protocol-version", "2026-07-28"), ("mcp-method", "nope/nope")], modern("nope/nope", None)).await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::NOT_FOUND, Some(-32601)));
    let legacy = json!({"jsonrpc": "2.0", "id": 3, "method": "nope/nope"}).to_string();
    let (status, _, v) = m.post(t, &[("mcp-protocol-version", "2025-06-18")], legacy.clone()).await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::OK, Some(-32601)), "legacy: 200 with the error");
    let (status, _, v) = m.post(t, &[], json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}).to_string()).await;
    assert_eq!((status, &v["result"]), (StatusCode::OK, &json!({})), "no header: 2025-03-26");
    let (status, _, v) = m.post(t, &[("mcp-protocol-version", "2026-07-28")], legacy.clone()).await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32020)), "a modern header without _meta");
    let (status, _, v) = m.post(t, &[("mcp-protocol-version", "2024-01-01")], legacy).await;
    assert_eq!((status, v["error"]["code"].as_i64()), (StatusCode::BAD_REQUEST, Some(-32022)));
    let (status, _, _) = m.post(t, &[], json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    let (status, _, _) = m.post(t, &[("origin", "https://evil.example")], ping.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for origin in ["https://claude.ai", "https://claude.com", m.env.base.as_str()] {
        assert_eq!(m.post(t, &[("origin", origin)], ping.clone()).await.0, StatusCode::OK, "{origin}");
    }
    let (status, _, _) = m.post(t, &[], vec![b' '; 512 * 1024 + 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // Cookies are never read: a signed-in browser without a token is refused.
    let r = b.step.post(m.resource()).body(ping).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    m.server.shutdown().await.unwrap();
}

async fn the_buckets_run_out(d: Arc<Dispatcher>) {
    // Slow refills (one a minute, one per 30 s), so the test's own pace cannot refill them.
    let limits = McpLimits { tool_calls_per_min: 1, tool_calls_burst: 3, writes_per_min: 2, oauth_per_min: 1000 };
    let m = mcp_with(&d, |c| c.mcp.limits = limits).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let tk = m.connect(&b).await;
    m.ok(&tk.access, "send", json!({"to": "david/scout", "text": "one"})).await;
    m.ok(&tk.access, "send", json!({"to": "david/scout", "text": "two"})).await;
    let text = m.refused(&tk.access, "send", json!({"to": "david/scout", "text": "three"})).await;
    assert!(text.starts_with("conde: rate limited; retry in ") && text.ends_with(" s"), "the user's write bucket: {text}");
    let text = m.refused(&tk.access, "inbox", json!({})).await;
    assert!(text.starts_with("conde: rate limited; retry in "), "the grant's call bucket: {text}");
    m.server.shutdown().await.unwrap();

    let limits = McpLimits { oauth_per_min: 2, ..McpLimits::default() };
    let m = mcp_with(&d, |c| c.mcp.limits = limits).await;
    for _ in 0..2 {
        assert_eq!(m.token(&[("grant_type", "password")]).await.0, StatusCode::BAD_REQUEST);
    }
    let r = m.http.post(m.url("/oauth/token")).form(&[("grant_type", "password")]).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers()[header::RETRY_AFTER].to_str().unwrap().parse::<u64>().unwrap() >= 1);
    m.server.shutdown().await.unwrap();
}

async fn no_human_name_refuses_writes_with_the_hint(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    let c = cast(&m, false).await;
    let page = m.consent_page(&b, &m.authorize_url(&pkce(), "st", &[])).await;
    let hint = "you have no human name on shared; publish it from a host: conde publish &lt;your-local-name&gt; --board shared";
    assert!(page.contains(hint) || page.contains(&hint.replace("&lt;", "&#60;").replace("&gt;", "&#62;")), "{page}");
    let tk = m.connect(&b).await;
    for (tool, args) in [("send", json!({"to": "david/scout", "text": "x"})), ("inbox", json!({})), ("read", json!({})),
                         ("react", json!({"id": id_of(&c.worker_dm)}))] {
        let text = m.refused(&tk.access, tool, args).await;
        assert_eq!(text, "conde: you have no human name on shared; publish it from a host: conde publish <your-local-name> --board shared", "{tool}");
    }
    // Reads of the owner view still work.
    assert!(m.ok(&tk.access, "read", json!({"target": id_of(&c.worker_dm)})).await.contains("green across the board"));
    assert!(m.ok(&tk.access, "agents", json!({"mine": true})).await.contains("/worker"));
    // Two human names: refused with their list.
    for host in ["k-desk2", "k-desk3"] {
        let p = m.principal(USER, host).await;
        let (d2, h) = (m.d.clone(), host.to_string());
        blocking(move || d2.with_tx(|t| Ok(t.set_host_mailbox_only(&h)?)).unwrap()).await;
        m.op(&p, None, "mailbox.publish", json!({"name": format!("me-{host}"), "human": true})).await;
    }
    let text = m.refused(&tk.access, "send", json!({"to": "david/scout", "text": "x"})).await;
    assert!(text.contains("several human names") && text.contains(&format!("{USER}/me-k-desk2")) && text.contains(&format!("{USER}/me-k-desk3")), "{text}");
    m.server.shutdown().await.unwrap();
}

async fn a_disabled_user_is_refused_at_the_next_request(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let tk = m.connect(&b).await;
    m.ok(&tk.access, "inbox", json!({})).await;
    let s = m.env.store.clone();
    blocking(move || s.set_disabled(USER, true).unwrap()).await;
    let (status, _, _) = m.post(Some(&tk.access), &[], json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, v) = m.refresh(&tk.refresh).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
    // Kept, like host keys: enabling the user again restores the grant.
    let s = m.env.store.clone();
    blocking(move || s.set_disabled(USER, false).unwrap()).await;
    m.ok(&tk.access, "inbox", json!({})).await;
    m.server.shutdown().await.unwrap();
}

// --- purging expired rows -------------------------------------------------------------------------

/// Unix seconds: the server runs on the system clock.
fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn new_refresh() -> String {
    format!("conde_rt_{}", conde_remote::token_digest(&conde::util::new_ulid()))
}

/// Rows planted behind the server's back, as digests: on the live grant, an expired access
/// token, an expired unspent refresh token and a refresh token spent and past its own expiry
/// (`spent` is its plaintext: presenting it is a replay); and an expired grant with a spent
/// refresh token.
struct Stale {
    access: String,
    refresh: String,
    spent: String,
    dead_grant: String,
    dead_refresh: String,
}

impl Mcp {
    async fn plant(&self, tk: &Tokens) -> Stale {
        let (d, live) = (self.d.clone(), conde_remote::token_digest(&tk.access));
        let spent = new_refresh();
        let stale = Stale {
            access: conde_remote::token_digest("conde_at_stale"),
            refresh: conde_remote::token_digest(&new_refresh()),
            spent: spent.clone(),
            dead_grant: format!("g-dead-{}", conde::util::new_ulid()),
            dead_refresh: conde_remote::token_digest(&new_refresh()),
        };
        let (access, refresh, dead_grant, dead_refresh) =
            (stale.access.clone(), stale.refresh.clone(), stale.dead_grant.clone(), stale.dead_refresh.clone());
        blocking(move || {
            d.with_tx(|t| {
                let now = unix_now();
                let (grant_id, audience, _) = t.mcp_token(&live)?.unwrap();
                let mut g = t.mcp_grant(&grant_id)?.unwrap();
                t.insert_mcp_token(&access, &grant_id, &audience, now - 10)?;
                t.insert_mcp_refresh(&refresh, &grant_id, now - 100, now - 10)?;
                let spent = conde_remote::token_digest(&spent);
                t.insert_mcp_refresh(&spent, &grant_id, now - 100, now - 10)?;
                assert!(t.consume_mcp_refresh(&spent, now - 50)?);
                g.grant_id = dead_grant.clone();
                (g.created_at, g.expires_at) = (now - 100, now - 10);
                t.insert_mcp_grant(&g)?;
                t.insert_mcp_refresh(&dead_refresh, &dead_grant, now - 100, now - 10)?;
                assert!(t.consume_mcp_refresh(&dead_refresh, now - 50)?);
                Ok(())
            })
            .unwrap()
        })
        .await;
        stale
    }

    /// Whether each row is still stored: (access token, refresh token, grant).
    async fn stored(&self, access: Option<&str>, refresh: Option<&str>, grant: Option<&str>) -> (bool, bool, bool) {
        let d = self.d.clone();
        let (a, r, g) = (access.map(str::to_string), refresh.map(str::to_string), grant.map(str::to_string));
        blocking(move || {
            d.with_tx(|t| {
                Ok((
                    a.map_or(Ok(false), |a| t.mcp_token(&a).map(|x| x.is_some()))?,
                    r.map_or(Ok(false), |r| t.mcp_refresh(&r).map(|x| x.is_some()))?,
                    g.map_or(Ok(false), |g| t.mcp_grant(&g).map(|x| x.is_some()))?,
                ))
            })
            .unwrap()
        })
        .await
    }

    async fn assert_purged(&self, s: &Stale) {
        assert_eq!(self.stored(Some(&s.access), Some(&s.refresh), None).await, (false, false, false), "expired tokens");
        assert_eq!(self.stored(None, Some(&s.dead_refresh), Some(&s.dead_grant)).await, (false, false, false), "expired grant");
        let spent = conde_remote::token_digest(&s.spent);
        assert_eq!(self.stored(None, Some(&spent), None).await, (false, true, false), "a spent token stays while its grant lives");
    }
}

async fn expired_rows_go_at_every_refresh_and_a_spent_token_stays_with_its_grant(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let tk = m.connect(&b).await;
    let s = m.plant(&tk).await;
    let (status, v) = m.refresh(&tk.refresh).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let current = tokens(&v);
    m.assert_purged(&s).await;
    let rotated = conde_remote::token_digest(&tk.refresh);
    assert_eq!(m.stored(None, Some(&rotated), None).await, (false, true, false), "nor on its consumption");
    m.ok(&current.access, "inbox", json!({})).await;
    // The kept token is still recognised: its replay revokes the grant.
    let (status, v) = m.refresh(&s.spent).await;
    assert_eq!((status, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")), "{v}");
    assert!(v["error_description"].as_str().unwrap().contains("already used"), "{v}");
    let (status, _, _) = m.post(Some(&current.access), &[], json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    m.server.shutdown().await.unwrap();
}

async fn the_sweep_purges_expired_rows_without_a_refresh(d: Arc<Dispatcher>) {
    let m = mcp_with(&d, |c| c.mcp.sweep_every = Duration::from_millis(100)).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let tk = m.connect(&b).await;
    let s = m.plant(&tk).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while m.stored(Some(&s.access), None, None).await.0 {
        assert!(tokio::time::Instant::now() < deadline, "no sweep within 10 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    m.assert_purged(&s).await;
    m.ok(&tk.access, "inbox", json!({})).await;
    let (status, v) = m.refresh(&tk.refresh).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    m.server.shutdown().await.unwrap();
}

// --- the cap on live grants -----------------------------------------------------------------------

/// A grant of the signed-in user stored directly, with `access` as its access token.
fn planted(id: &str, user_id: i64, client_id: &str, created_at: i64, expires_at: i64) -> (McpGrant, McpTokens, String) {
    let access = format!("conde_at_{}", conde_remote::token_digest(id));
    let grant = McpGrant {
        grant_id: id.into(), user_id, client_id: client_id.into(), client_host: "claude.ai".into(), client_name: None,
        resource: String::new(), created_at, last_used_at: None, expires_at, revoked_at: None,
    };
    let tokens = McpTokens { access_digest: conde_remote::token_digest(&access), refresh_digest: format!("r-{id}"),
                             access_ttl: 600, refresh_ttl: 600 };
    (grant, tokens, access)
}

async fn grants_beyond_the_cap_revoke_the_oldest_of_that_client(d: Arc<Dispatcher>) {
    let m = mcp(&d).await;
    let _signed_in = signed_in(&m).await;
    cast(&m, true).await;
    let store = m.env.store.clone();
    let user_id = blocking({ let s = store.clone(); move || s.user_by_identity(TENANT, "oid-krisztian").unwrap().unwrap().user_id }).await;
    let now = unix_now();
    let resource = m.resource();
    let create = |id: &str, client: &str, at: i64, expires_at: i64, cap: u32| {
        let (mut grant, tokens, access) = planted(id, user_id, client, at, expires_at);
        grant.resource = resource.clone();
        let s = store.clone();
        async move { (blocking(move || s.create_mcp_grant(&grant, &tokens, cap, at).unwrap().unwrap().revoked).await, access) }
    };
    let other = "https://claude.ai/oauth/other";
    let (_, other_access) = create("g-other", other, now - 50, now + 3600, 3).await;
    // An ended grant of the same client does not count.
    let (none, _) = create("g-ended", CLIENT, now - 3700, now - 100, 3).await;
    assert!(none.is_empty());
    let mut access = HashMap::new();
    for (i, id) in ["g-1", "g-2", "g-3"].into_iter().enumerate() {
        let (revoked, a) = create(id, CLIENT, now - 40 + 10 * i as i64, now + 3600, 3).await;
        assert!(revoked.is_empty(), "{id} fits under the cap: {revoked:?}");
        access.insert(id, a);
    }
    let (revoked, a) = create("g-4", CLIENT, now - 5, now + 3600, 3).await;
    assert_eq!(revoked, ["g-1"], "the oldest of the client ends");
    access.insert("g-4", a);
    // A lower cap ends as many as needed, oldest first.
    let (revoked, _) = create("g-5", CLIENT, now, now + 3600, 2).await;
    assert_eq!(revoked, ["g-2", "g-3"]);
    let s = store.clone();
    let mut live: Vec<String> = blocking(move || s.mcp_grants(user_id, now).unwrap()).await.into_iter().map(|g| g.grant_id).collect();
    live.sort();
    assert_eq!(live, ["g-4", "g-5", "g-other"], "another client's grant is untouched");
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    for id in ["g-1", "g-2", "g-3"] {
        let (status, _, _) = m.post(Some(&access[id]), &[], ping.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{id} was revoked with its tokens");
    }
    for token in [&access["g-4"], &other_access] {
        let (status, _, _) = m.post(Some(token), &[], ping.clone()).await;
        assert_eq!(status, StatusCode::OK);
    }
    m.server.shutdown().await.unwrap();
}

async fn connecting_past_the_cap_keeps_the_newest_grants_live(d: Arc<Dispatcher>) {
    let m = mcp_with(&d, |c| c.mcp.grants_per_client = 2).await;
    let b = signed_in(&m).await;
    cast(&m, true).await;
    let first = m.connect(&b).await;
    let second = m.connect(&b).await;
    let third = m.connect(&b).await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    let mut alive = 0;
    for t in [&first, &second] {
        let (status, _, _) = m.post(Some(&t.access), &[], ping.clone()).await;
        alive += usize::from(status == StatusCode::OK);
    }
    // Grants made within the same second tie on age: which of the older two ends is not fixed.
    assert_eq!(alive, 1, "one of the older grants was revoked");
    m.ok(&third.access, "inbox", json!({})).await;
    let page = b.get(&m.url("/keys")).await.2;
    assert_eq!(page.matches("<code>claude.ai</code>").count(), 2, "{page}");
    m.server.shutdown().await.unwrap();
}
