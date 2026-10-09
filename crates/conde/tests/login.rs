//! `conde login` against a fake board: the browser (reqwest following redirects) lands on the
//! CLI's loopback listener; the CLI redeems the code and stores the key as `conde enroll` does,
//! as a board file pinned to its server and account; `board.info` names the board.

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
    redeemed: Mutex<Vec<Value>>,
    refuse: Mutex<Option<String>>,
}

async fn cli(State(b): State<Arc<Board>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let code = format!("{:064x}", rand_u128());
    b.codes.lock().unwrap().insert(code.clone(), (q["state"].clone(), q["challenge"].clone()));
    Redirect::to(&format!("http://127.0.0.1:{}/done?code={code}&state={}", q["port"], q["state"])).into_response()
}

async fn redeem(State(b): State<Arc<Board>>, Json(req): Json<Value>) -> Response {
    b.redeemed.lock().unwrap().push(req.clone());
    if let Some(m) = b.refuse.lock().unwrap().clone() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": m}))).into_response();
    }
    let Some((state, challenge)) = req["code"].as_str().and_then(|c| b.codes.lock().unwrap().remove(c)) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": "unknown code"}))).into_response();
    };
    let verifier = req["code_verifier"].as_str().unwrap_or_default();
    if req["state"] != state.as_str() || URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != challenge {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "message": "mismatch"}))).into_response();
    }
    Json(json!({"host_id": req["host"], "key": KEY, "user": "krisztian"})).into_response()
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
