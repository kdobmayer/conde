//! `conde login` against a fake board: the browser (reqwest following redirects) lands on the
//! CLI's loopback listener; the CLI redeems the code and stores the key as `conde enroll` does,
//! as a board file pinned to its server and account; `board.info` names the board. The pasted
//! code: the page shows a short code, the CLI redeems it with the run it kept on disk.
#![cfg(unix)] // checks unix file modes

use conde::{boards, credentials, login, util};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const KEY: &str = "conde_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct Board {
    /// code -> (state, challenge) handed out by /auth/cli.
    codes: Mutex<HashMap<String, (String, String)>>,
    /// state -> (shown code, challenge, failed tries) of pasted-code logins.
    pasted: Mutex<HashMap<String, (String, String, u32)>>,
    redeemed: Mutex<Vec<Value>>,
    refuse: Mutex<Option<String>>,
    /// Answer every redeem with this `invalid_request` message.
    bad_request: Mutex<Option<String>>,
    /// Answer every redeem with a 429 `rate_limited` and this message.
    limited: Mutex<Option<String>>,
    /// That 429 says the waiting login expires before the next allowed attempt.
    limited_expires_first: Mutex<bool>,
    /// Answer the next `n` redeems with the per-address 429 and this `retry_after`: (n, wait).
    per_address: Mutex<(u32, u64)>,
    /// Hosts a key was minted for: a second mint is a re-enrollment.
    hosts: Mutex<std::collections::HashSet<String>>,
}

impl Board {
    fn minted(&self, host: &Value) -> Response {
        let reenrolled = !self.hosts.lock().unwrap().insert(host.as_str().unwrap_or_default().to_string());
        Json(json!({"host_id": host, "key": KEY, "user": "krisztian", "reenrolled": reenrolled})).into_response()
    }
}

async fn cli(State(b): State<Arc<Board>>, Query(q): Query<HashMap<String, String>>) -> Response {
    if q.get("mode").map(String::as_str) == Some("code") {
        assert!(!q.contains_key("port"), "a pasted-code link has no loopback port");
        let n = rand_u128();
        let code: String = (0..8).map(|i| b"23456789ABCDEFGHJKMNPQRSTVWXYZ"[((n >> (i * 8)) % 30) as usize] as char).collect();
        b.pasted.lock().unwrap().insert(q["state"].clone(), (code.clone(), q["challenge"].clone(), 0));
        return axum::response::Html(format!("<pre id=\"key\">{}-{}</pre>", &code[..4], &code[4..])).into_response();
    }
    let code = format!("{:064x}", rand_u128());
    b.codes.lock().unwrap().insert(code.clone(), (q["state"].clone(), q["challenge"].clone()));
    Redirect::to(&format!("http://127.0.0.1:{}/done?code={code}&state={}", q["port"], q["state"])).into_response()
}

async fn redeem(State(b): State<Arc<Board>>, Json(req): Json<Value>) -> Response {
    b.redeemed.lock().unwrap().push(req.clone());
    {
        let mut limit = b.per_address.lock().unwrap();
        if limit.0 > 0 {
            limit.0 -= 1;
            let wait = limit.1;
            let message = format!("too many sign-in attempts from this address; retry in {wait} s");
            let body = json!({"error": "rate_limited", "message": message, "retry_after": wait});
            return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", wait.to_string())], Json(body)).into_response();
        }
    }
    if let Some(m) = b.bad_request.lock().unwrap().clone() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_request", "message": m}))).into_response();
    }
    if let Some(m) = b.limited.lock().unwrap().clone() {
        let mut body = json!({"error": "rate_limited", "message": m, "retry_after": 30});
        if *b.limited_expires_first.lock().unwrap() {
            body["login_expires_first"] = json!(true);
        }
        return (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    }
    if let Some(m) = b.refuse.lock().unwrap().clone() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": m}))).into_response();
    }
    let verifier = req["code_verifier"].as_str().unwrap_or_default();
    let code = req["code"].as_str().unwrap_or_default();
    if code.len() != 64 {
        let state = req["state"].as_str().unwrap_or_default();
        let mut pasted = b.pasted.lock().unwrap();
        let Some((shown, challenge, tries)) = pasted.remove(state) else {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "authorization_pending", "message": "no signed-in login waits for this run"}))).into_response();
        };
        if code == shown && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge {
            return b.minted(&req["host"]);
        }
        if tries + 1 >= 5 {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": "wrong code, and that was the last try"}))).into_response();
        }
        pasted.insert(state.to_string(), (shown, challenge, tries + 1));
        let left = 4 - tries;
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_code", "message": format!("wrong code; {left} tries left"), "tries_left": left}))).into_response();
    }
    let Some((state, challenge)) = b.codes.lock().unwrap().remove(code) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": "unknown code"}))).into_response();
    };
    if req["state"] != state.as_str() || URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != challenge {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": "mismatch"}))).into_response();
    }
    b.minted(&req["host"])
}

fn rand_u128() -> u128 {
    let mut b = [0u8; 16];
    use std::io::Read;
    std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut b).unwrap();
    u128::from_le_bytes(b)
}

/// `POST /v1/rpc` as the server answers `board.info` for a host key.
async fn rpc(headers: axum::http::HeaderMap, Json(req): Json<Value>) -> Response {
    let authorized = headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {KEY}"));
    if !authorized {
        return (StatusCode::UNAUTHORIZED, Json(json!({"v": 1, "type": "response", "request_id": req["request_id"], "ok": false,
            "error": {"code": "unauthenticated", "message": "unknown host key"}}))).into_response();
    }
    assert_eq!(req["op"], "board.info");
    Json(json!({"v": 1, "type": "response", "request_id": req["request_id"], "ok": true,
        "result": {"name": "dakai", "owner": "krisztian", "host": "box-1", "mailbox_only": false}}))
    .into_response()
}

async fn board() -> (String, Arc<Board>) {
    let board = Arc::new(Board::default());
    let app = Router::new()
        .route("/auth/cli", get(cli))
        .route("/auth/cli/redeem", post(redeem))
        .route("/v1/rpc", post(rpc))
        .with_state(board.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, board)
}

fn browser() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
}

/// Opens the URL the way a browser would, after `strays` requests to the loopback.
fn open_with(strays: Vec<&'static str>, seen: Arc<Mutex<Vec<(u16, String)>>>) -> impl FnOnce(&str) {
    move |url: &str| {
        let url = url.to_string();
        tokio::spawn(async move {
            let b = browser();
            let port: u16 = url.split("port=").nth(1).unwrap().split('&').next().unwrap().parse().unwrap();
            for stray in strays {
                let r = b.get(format!("http://127.0.0.1:{port}{stray}")).send().await.unwrap();
                let got = (r.status().as_u16(), r.text().await.unwrap());
                seen.lock().unwrap().push(got);
            }
            let r = b.get(&url).send().await.unwrap();
            let got = (r.status().as_u16(), r.text().await.unwrap());
            seen.lock().unwrap().push(got);
        });
    }
}

#[tokio::test]
async fn login_redeems_the_code_and_stores_the_key_like_enroll() {
    let (server, board) = board().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let strays = vec!["/favicon.ico", "/done?code=x&state=someone-else"];
    let enrolled = login::sign_in(&server, "box-1", open_with(strays, seen.clone()), Duration::from_secs(20)).await.unwrap();
    assert_eq!((enrolled.host_id.as_str(), enrolled.key.as_str(), enrolled.user.as_str()), ("box-1", KEY, "krisztian"));
    assert!(!format!("{enrolled:?}").contains(KEY));

    for _ in 0..50 {
        if seen.lock().unwrap().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].0, 404);
    assert_eq!(seen[1].0, 400, "a foreign state is refused, the run keeps waiting");
    assert_eq!(seen[2].0, 200);
    assert!(seen[2].1.contains("Signed in"));
    let redeemed = board.redeemed.lock().unwrap().clone();
    assert_eq!(redeemed.len(), 1);
    assert_eq!(redeemed[0]["host"], "box-1");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde/boards/dakai.json");
    login::save(&path, "dakai", &server, &enrolled).unwrap();
    assert_eq!(credentials::mode(&path), Some(0o600));
    let stored = boards::load(&path).unwrap();
    assert_eq!(
        (stored.server.as_str(), stored.host_id.as_str(), stored.owner.as_str(), stored.key.as_str()),
        (server.as_str(), "box-1", "krisztian", KEY)
    );
    assert!(!util::local_host().is_empty());

    // the alias stays pinned: another account under the same alias is refused, the file kept
    let other = login::Enrolled { user: "someone".into(), ..enrolled.clone() };
    let err = login::save(&path, "dakai", &server, &other).unwrap_err().to_string();
    assert!(err.starts_with(&format!("dakai is pinned to {server} as krisztian")), "{err}");
    assert_eq!(boards::load(&path).unwrap().owner, "krisztian");
}

#[tokio::test]
async fn a_refused_redeem_is_reported() {
    let (server, board) = board().await;
    *board.refuse.lock().unwrap() = Some("the code expired; run conde login again".into());
    let err = login::sign_in(&server, "h", open_with(vec![], Default::default()), Duration::from_secs(20)).await.unwrap_err();
    assert!(err.to_string().contains("the code expired"), "{err}");
}

#[tokio::test]
async fn an_unfinished_browser_sign_in_times_out() {
    let (server, board) = board().await;
    let err = login::sign_in(&server, "h", |_| {}, Duration::from_millis(300)).await.unwrap_err();
    assert!(err.to_string().contains("did not finish"), "{err}");
    assert!(board.redeemed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn plain_http_is_refused_for_remote_servers() {
    let err = login::sign_in("http://conde.example", "h", |_| panic!("no browser"), Duration::from_secs(1)).await.unwrap_err();
    assert!(err.to_string().contains("not TLS"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn board_info_names_the_board_with_the_host_key() {
    let (server, _) = board().await;
    let s = server.clone();
    let info = tokio::task::spawn_blocking(move || boards::board_info(&s, KEY)).await.unwrap().unwrap();
    assert_eq!((info.name.as_str(), info.owner.as_str(), info.host.as_str(), info.mailbox_only), ("dakai", "krisztian", "box-1", false));
    let err = tokio::task::spawn_blocking(move || boards::board_info(&server, "conde_wrong")).await.unwrap().unwrap_err();
    assert!(err.to_string().contains("refused board.info: unknown host key"), "{err}");
    let err = boards::board_info("http://conde.example", KEY).unwrap_err();
    assert!(err.to_string().contains("not TLS"), "the key never goes over plain http to a remote host: {err}");
}

/// What the human sees on the page a pasted-code URL opens: the code.
async fn shown_code(url: &str) -> String {
    let page = browser().get(url).send().await.unwrap().text().await.unwrap();
    page.split("<pre id=\"key\">").nth(1).unwrap().split('<').next().unwrap().to_string()
}

#[tokio::test]
async fn a_pasted_code_finishes_the_login_started_on_this_host() {
    let (server, board) = board().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde/login/x.json");
    let url = login::start_code(&server, &path).unwrap();
    assert!(url.starts_with(&format!("{server}/auth/cli?mode=code&state=")), "{url}");
    assert_eq!(credentials::mode(&path), Some(0o600), "the waiting run holds the PKCE verifier");
    assert!(!url.contains(&std::fs::read_to_string(&path).unwrap().split("\"verifier\":\"").nth(1).unwrap()[..43]));
    // a code before the sign-in (from an older page) leaves the login waiting
    match login::finish_code(&server, &path, "2345-6789", "box-1").await.unwrap() {
        login::CodeOutcome::Retry(why) => assert!(why.contains("no signed-in login waits"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(path.exists());
    let shown = shown_code(&url).await;

    let wrong = if shown.starts_with('Z') { "YYYY-YYYY" } else { "ZZZZ-ZZZZ" };
    match login::finish_code(&server, &path, wrong, "box-1").await.unwrap() {
        login::CodeOutcome::Retry(why) => assert!(why.contains("4 tries left"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(path.exists(), "a wrong code keeps the login waiting");
    // a mistyped character outside the alphabet is caught here: not sent, the login still waits
    for (typo, says) in [("K7QM-3XR0", "'0' is not in a sign-in code"), ("k7qm-3xri", "'I' is not in"), ("K7QM-3XR", "that is 7 characters")] {
        match login::finish_code(&server, &path, typo, "box-1").await.unwrap() {
            login::CodeOutcome::Retry(why) => {
                assert!(why.contains(says) && why.contains("23456789ABCDEFGHJKMNPQRSTVWXYZ (there is no 0/O, 1/I/L or U)"), "{why}")
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(board.redeemed.lock().unwrap().len(), 2, "a mistyped code is not sent");
    assert!(path.exists());

    // a request the server cannot take (here: a host name) is not final: the login still waits
    *board.bad_request.lock().unwrap() = Some("bad host name".into());
    let err = login::finish_code(&server, &path, &shown, "box-1").await.unwrap_err().to_string();
    assert!(err.contains("bad host name"), "{err}");
    assert!(path.exists(), "invalid_request keeps the waiting login");
    *board.bad_request.lock().unwrap() = None;

    // typed as read aloud
    let typed = shown.to_lowercase().replace('-', " ");
    let enrolled = match login::finish_code(&server, &path, &typed, "box-1").await.unwrap() {
        login::CodeOutcome::Enrolled(e) => e,
        other => panic!("{other:?}"),
    };
    assert_eq!((enrolled.host_id.as_str(), enrolled.key.as_str(), enrolled.user.as_str()), ("box-1", KEY, "krisztian"));
    assert!(!path.exists(), "the login is done");
    let err = login::finish_code(&server, &path, &shown, "box-1").await.unwrap_err().to_string();
    assert!(err.contains(&format!("start one with: conde login --server {server} --code")), "{err}");
}

#[tokio::test]
async fn a_waiting_login_ends_on_a_final_refusal_on_expiry_and_stays_with_its_server() {
    let (server, _board) = board().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login/x.json");

    // out of tries: the server's final refusal removes the waiting login
    let url = login::start_code(&server, &path).unwrap();
    let shown = shown_code(&url).await;
    let wrong = if shown.starts_with('Z') { "YYYY-YYYY" } else { "ZZZZ-ZZZZ" };
    for _ in 0..4 {
        assert!(matches!(login::finish_code(&server, &path, wrong, "h").await.unwrap(), login::CodeOutcome::Retry(_)));
    }
    let err = login::finish_code(&server, &path, wrong, "h").await.unwrap_err().to_string();
    assert!(err.contains("last try"), "{err}");
    assert!(!path.exists());

    login::start_code(&server, &path).unwrap();
    let err = login::finish_code("https://conde.example", &path, "2345-6789", "h").await.unwrap_err().to_string();
    assert!(err.contains(&format!("belongs to {server}")), "{err}");

    let mut pending: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    pending["started_at"] = json!(pending["started_at"].as_u64().unwrap() - login::PENDING_FOR.as_secs() - 1);
    std::fs::write(&path, pending.to_string()).unwrap();
    let err = login::finish_code(&server, &path, "2345-6789", "h").await.unwrap_err().to_string();
    assert!(err.contains("expired"), "{err}");
    assert!(!path.exists());
}

#[test]
fn a_machine_without_a_display_is_headless() {
    assert!(login::headless(true, None, None));
    assert!(login::headless(true, Some(""), None));
    assert!(!login::headless(true, Some(":0"), None));
    assert!(!login::headless(true, None, Some("wayland-0")));
    assert!(!login::headless(false, None, None), "macOS opens the browser without DISPLAY");
}

/// `conde login` as an agent runs it: no terminal, so the first call prints the URL and how to
/// finish; the second call carries the code the human read out and stores the key.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_runs_the_pasted_code_login_in_two_calls() {
    let (server, _board) = board().await;
    let config = tempfile::tempdir().unwrap();
    let conde = |args: Vec<String>| {
        let config = config.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_conde"))
                .args(&args)
                .env("XDG_CONFIG_HOME", &config)
                // no bridge runs on this socket
                .env("CONDE_SOCK", config.join("conde.sock"))
                .env("DISPLAY", ":0")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
        })
    };
    let start = vec!["login".into(), "--server".into(), server.clone(), "--host".into(), "box-9".into(), "--no-browser".into()];
    // Without a terminal the run waits for its code: exit 3, not the 0 of an enrolled host.
    let (status, out, err) = conde(start.clone()).await.unwrap();
    assert_eq!(status, Some(3), "{err}");
    assert!(out.is_empty(), "nothing enrolled: {out}");
    assert!(!err.contains("still waiting"), "{err}");
    // starting again replaces the waiting run, and says so
    let (status, _, err) = conde(start).await.unwrap();
    assert!(status == Some(3) && err.contains("was still waiting for its code; this one replaces it"), "{err}");
    let finish = format!("conde login --server {server} --host box-9 --code <CODE>");
    assert!(err.contains(&finish), "{err}");
    let url = err.lines().map(str::trim).find(|l| l.starts_with(&format!("{server}/auth/cli?mode=code"))).unwrap().to_string();

    let shown = shown_code(&url).await;
    let finish = vec!["login", "--server", &server, "--host", "box-9", "--code", &shown, "--board", "dakai"];
    let (status, out, err) = conde(finish.into_iter().map(String::from).collect()).await.unwrap();
    assert_eq!(status, Some(0), "{out}{err}");
    assert!(out.contains("signed in as krisztian"), "{out}");
    assert!(out.contains(&format!("\nenrolled host box-9 with {server} as board dakai (account krisztian)")), "{out}");
    assert!(!out.contains("re-enrolled"), "{out}");
    assert!(out.contains("\nbridge: not running; it reads the new key at start\nnext: conde setup"), "{out}");
    let path = config.path().join("conde/boards/dakai.json");
    assert_eq!(credentials::mode(&path), Some(0o600));
    let stored = boards::load(&path).unwrap();
    assert_eq!((stored.host_id.as_str(), stored.key.as_str()), ("box-9", KEY));
    assert_eq!(std::fs::read_dir(config.path().join("conde/login")).unwrap().count(), 0, "nothing waits any more");

    // the same host again: the server revoked its previous key, and the CLI says so
    let start = vec!["login".into(), "--server".into(), server.clone(), "--host".into(), "box-9".into(), "--no-browser".into()];
    let (status, _, err) = conde(start).await.unwrap();
    assert_eq!(status, Some(3), "{err}");
    let url = err.lines().map(str::trim).find(|l| l.starts_with(&format!("{server}/auth/cli?mode=code"))).unwrap().to_string();
    let shown = shown_code(&url).await;
    let finish = vec!["login", "--server", &server, "--host", "box-9", "--code", &shown, "--board", "dakai"];
    let (status, out, err) = conde(finish.into_iter().map(String::from).collect()).await.unwrap();
    assert_eq!(status, Some(0), "{out}{err}");
    assert!(out.contains("\nre-enrolled host box-9: the previous key for this host is revoked\n"), "{out}");
    assert!(out.contains(&format!("\nboard dakai at {server} (account krisztian); credential stored at")), "{out}");
    assert!(!out.contains("\nenrolled host"), "{out}");
}

/// A rate-limited redeem is a retry, like a wrong code: the CLI shows the server's wait and the
/// login keeps waiting (on a terminal the prompt comes back).
#[tokio::test]
async fn a_rate_limited_redeem_keeps_the_login_waiting() {
    let (server, board) = board().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login/x.json");
    let url = login::start_code(&server, &path).unwrap();
    let shown = shown_code(&url).await;
    *board.limited.lock().unwrap() = Some("too many code attempts for this account; retry in 30 s".into());
    match login::finish_code(&server, &path, &shown, "h").await.unwrap() {
        login::CodeOutcome::Retry(why) => assert!(why.contains("retry in 30 s"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(path.exists(), "the login still waits");
    *board.limited.lock().unwrap() = None;
    assert!(matches!(login::finish_code(&server, &path, &shown, "h").await.unwrap(), login::CodeOutcome::Enrolled(_)));
}

/// A 429 whose next allowed attempt comes after the login expires ends the run: the server's
/// message, no "run again with the code" hint, and the waiting login removed.
#[tokio::test]
async fn a_rate_limit_outlasting_the_login_ends_it() {
    let (server, board) = board().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login/x.json");
    let url = login::start_code(&server, &path).unwrap();
    let shown = shown_code(&url).await;
    let message = "too many code attempts for this account; the next is allowed in 400 s, after this login expires (in 100 s): run conde login --code again then";
    *board.limited.lock().unwrap() = Some(message.into());
    *board.limited_expires_first.lock().unwrap() = true;
    let err = login::finish_code(&server, &path, &shown, "h").await.unwrap_err().to_string();
    assert_eq!(err, format!("the server refused the sign-in: {message}"));
    assert!(!path.exists(), "nothing waits any more");
}

/// The loopback redeem waits out the per-address 429 (Retry-After) and retries; a wait past
/// the bound fails at once with the server's message.
#[tokio::test]
async fn the_loopback_redeem_waits_out_a_short_rate_limit() {
    let (server, board) = board().await;
    *board.per_address.lock().unwrap() = (2, 1);
    let started = std::time::Instant::now();
    let enrolled = login::sign_in(&server, "box-1", open_with(vec![], Default::default()), Duration::from_secs(20)).await.unwrap();
    assert_eq!(enrolled.host_id, "box-1");
    assert_eq!(board.redeemed.lock().unwrap().len(), 3, "two refusals, then the code redeemed");
    assert!(started.elapsed() >= Duration::from_secs(2), "each retry waited its Retry-After");

    *board.per_address.lock().unwrap() = (1, login::LIMITED_WAIT.as_secs() + 1);
    let started = std::time::Instant::now();
    let err = login::sign_in(&server, "box-1", open_with(vec![], Default::default()), Duration::from_secs(20)).await.unwrap_err();
    assert!(err.to_string().contains("too many sign-in attempts from this address"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10), "no wait past the bound");
}
