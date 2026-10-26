//! The Slack worker against a local mock of chat.postMessage: who gets a Slack DM, what it says,
//! and how Slack's answers settle the notification. On SQLite, and on Postgres with CONDE_TEST_PG.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use conde::slack::{self, SlackConfig, Token};
use serde_json::{Value, json};

type Answer = (u16, Option<&'static str>, Value);

#[derive(Default)]
struct Mock {
    script: VecDeque<Answer>,
    /// (Authorization header, JSON body) per call.
    calls: Vec<(String, Value)>,
}

async fn post_message(
    State(mock): State<Arc<Mutex<Mock>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, HeaderMap, Json<Value>) {
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let mut mock = mock.lock().unwrap();
    mock.calls.push((auth, body));
    let (status, retry_after, answer) = mock.script.pop_front().unwrap_or((200, None, json!({"ok": true})));
    let mut h = HeaderMap::new();
    if let Some(r) = retry_after {
        h.insert(header::RETRY_AFTER, r.parse().unwrap());
    }
    (StatusCode::from_u16(status).unwrap(), h, Json(answer))
}

struct Fixture {
    rt: tokio::runtime::Runtime,
    mock: Arc<Mutex<Mock>>,
    config: Arc<SlackConfig>,
    http: reqwest::Client,
    b: Board,
}

impl Fixture {
    /// alice (an agent) and krisz (a human, mapped to U0KRISZ) on one board; Slack answers `script`, then ok.
    fn new(script: Vec<Answer>) -> Fixture {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let mock = Arc::new(Mutex::new(Mock { script: script.into(), calls: Vec::new() }));
        let app = Router::new().route("/api/chat.postMessage", post(post_message)).with_state(mock.clone());
        let addr = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            addr
        });
        let b = Board::new();
        b.register(ALICE, "alice");
        b.register(HUMAN, "krisz");
        let config = Arc::new(SlackConfig {
            token: Token::new("xoxb-test"),
            members: [("krisz".to_string(), "U0KRISZ".to_string())].into(),
            api_base: format!("http://{addr}/api"),
            board: "dakai".into(),
            public_url: Some("https://conde.example".into()),
        });
        Fixture { rt, mock, config, http: slack::client().unwrap(), b }
    }

    fn pass(&self) -> usize {
        self.rt.block_on(slack::run_once(&self.b.d, &self.config, &self.http))
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.mock.lock().unwrap().calls.clone()
    }

    fn last_reason(&self) -> Option<String> {
        self.b.d.with_conn(|tx| tx.attempts()).unwrap().last().and_then(|a| a.reason.clone())
    }
}

#[test]
fn a_dm_to_the_configured_person_sends_one_escaped_slack_message() {
    let f = Fixture::new(Vec::new());
    let mid = f.b.send(ALICE, "krisz", "<!channel> & <@U999> look\nthe rest stays on the board");
    // a bridge on the human's host badges it first; the worker still takes it
    let mut br = f.b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    assert_eq!(f.b.status_of(&mid, "krisz"), "badge");

    assert_eq!(f.pass(), 1);
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    let (auth, body) = &calls[0];
    assert_eq!(auth, "Bearer xoxb-test");
    assert_eq!(body["channel"], "U0KRISZ");
    assert_eq!(
        body["text"],
        "krisz/alice sent you a message on dakai:\n\
         >&lt;!channel&gt; &amp; &lt;@U999&gt; look\n\
         <https://conde.example/#/a/krisz/dm/krisz/alice|Open in conde>"
    );
    assert_eq!(body["unfurl_links"], false);
    assert_eq!(f.b.status_of(&mid, "krisz"), "delivered");

    // nothing more, not even after the lease
    br.pass().unwrap();
    f.b.d.advance_clock(slack::LEASE_MS * 2);
    assert_eq!(f.pass(), 0);
    assert_eq!(f.calls().len(), 1);
}

#[test]
fn unconfigured_people_reactions_plain_posts_and_old_messages_send_nothing() {
    let f = Fixture::new(Vec::new());
    let zed = f.b.other("zed", "zbox");
    f.b.call_as(&zed, "human:zed", "agent.register", json!({"name": "zed"})).unwrap();
    let to_zed = f.b.send(ALICE, "zed/zed", "for a person without Slack");
    assert_eq!(f.b.status_of(&to_zed, "zed"), "pending");

    let mine = f.b.send(HUMAN, "alice", "from krisz");
    f.b.ok(ALICE, "reaction.set", json!({"message_id": mine, "emoji": "👍", "present": true}));

    f.b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    f.b.ok(HUMAN, "channel.join", json!({"channel": "#dev"}));
    let post = f.b.send(ALICE, "#dev", "a plain post badges only");
    assert_eq!(f.b.status_of(&post, "krisz"), "none");

    assert_eq!(f.pass(), 0);
    assert!(f.calls().is_empty());

    let old = f.b.send(ALICE, "krisz", "an hour ago");
    f.b.d.advance_clock(slack::MAX_AGE_MS + 1);
    assert_eq!(f.pass(), 0);
    assert!(f.calls().is_empty());
    assert_eq!(f.b.status_of(&old, "krisz"), "pending");
}

#[test]
fn a_final_slack_error_refuses_the_notification_once() {
    let f = Fixture::new(vec![(200, None, json!({"ok": false, "error": "channel_not_found"}))]);
    let mid = f.b.send(ALICE, "krisz", "hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.b.status_of(&mid, "krisz"), "refused");
    assert_eq!(f.last_reason().as_deref(), Some("slack: channel_not_found"));
    f.b.d.advance_clock(10 * 60_000);
    assert_eq!(f.pass(), 0);
    assert_eq!(f.calls().len(), 1);
}

#[test]
fn a_rate_limit_waits_for_retry_after_and_a_5xx_for_the_backoff() {
    let f = Fixture::new(vec![
        (429, Some("120"), json!({"ok": false, "error": "ratelimited"})),
        (503, None, json!({})),
    ]);
    let mid = f.b.send(ALICE, "krisz", "hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.b.status_of(&mid, "krisz"), "pending");
    assert_eq!(f.last_reason().as_deref(), Some("slack: ratelimited"));
    f.b.d.advance_clock(119_000);
    assert_eq!(f.pass(), 0);
    f.b.d.advance_clock(2_000);
    assert_eq!(f.pass(), 1);
    assert_eq!(f.b.status_of(&mid, "krisz"), "pending");
    f.b.d.advance_clock(f.b.d.retry_ms());
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 3);
    assert_eq!(f.b.status_of(&mid, "krisz"), "delivered");
}

#[test]
fn a_bridge_pass_during_a_send_leaves_it_to_the_worker() {
    let f = Fixture::new(Vec::new());
    let mid = f.b.send(ALICE, "krisz", "hello");
    let jobs = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    assert_eq!(jobs.len(), 1);
    // the human's host bridge claims while the send is in flight
    let mut br = f.b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    for job in jobs {
        f.rt.block_on(slack::deliver(&f.b.d, &f.config, &f.http, job));
    }
    assert_eq!(f.b.status_of(&mid, "krisz"), "delivered");
    assert_eq!(f.pass(), 0);
    assert_eq!(f.calls().len(), 1);
}
