//! The Slack worker against a local mock of chat.postMessage: who gets a Slack DM, what it says,
//! how Slack's answers are recorded, and that nothing the board shows changes. On SQLite, and on
//! Postgres with CONDE_TEST_PG.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use conde::dispatch::Principal;
use conde::protocol::Request;
use conde::slack::{self, SlackConfig, Token};
use conde::store::AttemptRow;
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
    /// alice (an agent) and krisz (a human, owner krisz mapped to U0KRISZ) on one board; Slack
    /// answers `script`, then ok.
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

    fn slack_attempts(&self) -> Vec<AttemptRow> {
        let all = self.b.d.with_conn(|tx| tx.attempts()).unwrap();
        all.into_iter().filter(|a| a.bridge_instance == conde::store::SLACK_INSTANCE).collect()
    }

    /// (status, reason) of the Slack attempts, oldest first.
    fn outcomes(&self) -> Vec<(String, Option<String>)> {
        self.slack_attempts().into_iter().map(|a| (a.status, a.reason)).collect()
    }

    /// What the sender and the board's owner read about alice's DMs: `conde sent`, `conde watch`,
    /// the web app's DM history and fleet.
    fn sender_views(&self) -> Value {
        let browser = Principal {
            user_id: self.b.p.user_id,
            user: self.b.p.user.clone(),
            host: "web".into(),
            local: false,
            bridge_instance: None,
            bridge_connection: None,
            browser: true,
        };
        let owner = |op: &str, body: Value| {
            let resp = self.b.d.dispatch(&browser, &Request::new(conde::util::new_ulid(), op, body));
            assert!(resp.error.is_none(), "{op}: {:?}", resp.error);
            resp.result.unwrap()
        };
        let mut fleet = owner("fleet.list", json!({}));
        fleet["now"] = Value::Null;
        json!({
            "sent": self.b.ok(ALICE, "receipt.list", json!({})),
            "feed": self.b.ok(ALICE, "message.feed", json!({})),
            "history": owner("owner.history", json!({"agent": "alice", "with": "krisz/krisz"})),
            "fleet": fleet,
        })
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
    assert_eq!(f.outcomes(), [("delivered".to_string(), None)]);
    assert_eq!(f.b.status_of(&mid, "krisz"), "badge");

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
fn a_final_slack_error_is_recorded_once() {
    let f = Fixture::new(vec![(200, None, json!({"ok": false, "error": "channel_not_found"}))]);
    let mid = f.b.send(ALICE, "krisz", "hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes(), [("refused".to_string(), Some("slack: channel_not_found".to_string()))]);
    assert_eq!(f.b.status_of(&mid, "krisz"), "pending");
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
    f.b.send(ALICE, "krisz", "hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes(), [("failed".to_string(), Some("slack: ratelimited".to_string()))]);
    f.b.d.advance_clock(119_000);
    assert_eq!(f.pass(), 0);
    f.b.d.advance_clock(2_000);
    assert_eq!(f.pass(), 1);
    f.b.d.advance_clock(f.b.d.retry_ms() - 5_000);
    assert_eq!(f.pass(), 0);
    f.b.d.advance_clock(5_000);
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 3);
    assert_eq!(f.outcomes().last().unwrap().0, "delivered");
}

#[test]
fn a_rate_limit_stops_the_rest_of_the_owners_pass() {
    let f = Fixture::new(vec![(429, Some("120"), json!({"ok": false, "error": "ratelimited"}))]);
    f.b.send(ALICE, "krisz", "one");
    f.b.send(ALICE, "krisz", "two");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);
    let failed = ("failed".to_string(), Some("slack: ratelimited".to_string()));
    assert_eq!(f.outcomes(), [failed.clone(), failed]);
    f.b.d.advance_clock(119_000);
    assert_eq!(f.pass(), 0);
    f.b.d.advance_clock(2_000);
    assert_eq!(f.pass(), 2);
    assert_eq!(f.calls().len(), 3);
}

#[test]
fn a_bridge_pass_during_a_send_leaves_it_to_the_worker() {
    let f = Fixture::new(Vec::new());
    f.b.send(ALICE, "krisz", "hello");
    let jobs = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    assert_eq!(jobs.len(), 1);
    // the human's host bridge claims while the send is in flight
    let mut br = f.b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    for job in jobs {
        assert_eq!(f.rt.block_on(slack::deliver(&f.b.d, &f.config, &f.http, job)), Some(slack::Outcome::Delivered));
    }
    assert_eq!(f.outcomes(), [("delivered".to_string(), None)]);
    assert_eq!(f.pass(), 0);
    assert_eq!(f.calls().len(), 1);
}

#[test]
fn a_send_taken_over_after_its_lease_is_not_posted_again() {
    let f = Fixture::new(Vec::new());
    f.b.send(ALICE, "krisz", "hello");
    // replica A claims, then stalls past its lease
    let stalled = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    assert_eq!(stalled.len(), 1);
    f.b.d.advance_clock(slack::LEASE_MS + 1);
    // replica B takes it over and sends
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);
    // A wakes up: its send is no longer its own
    for job in stalled {
        assert_eq!(f.rt.block_on(slack::deliver(&f.b.d, &f.config, &f.http, job)), None);
    }
    assert_eq!(f.calls().len(), 1);
    let statuses: Vec<String> = f.outcomes().into_iter().map(|o| o.0).collect();
    assert_eq!(statuses, ["abandoned", "delivered"]);
}

#[test]
fn a_send_past_its_lease_is_not_posted_by_its_late_claimer() {
    let f = Fixture::new(Vec::new());
    f.b.send(ALICE, "krisz", "hello");
    let stalled = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    f.b.d.advance_clock(slack::LEASE_MS + 1);
    // A wakes up before anyone took it over: past its lease it posts nothing
    for job in stalled {
        assert_eq!(f.rt.block_on(slack::deliver(&f.b.d, &f.config, &f.http, job)), None);
    }
    assert!(f.calls().is_empty());
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);
}

#[test]
fn a_message_read_before_the_post_is_not_sent() {
    let f = Fixture::new(Vec::new());
    let mid = f.b.send(ALICE, "krisz", "hello");
    let jobs = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    f.b.ok(HUMAN, "message.read", json!({"id": mid}));
    for job in jobs {
        assert_eq!(f.rt.block_on(slack::deliver(&f.b.d, &f.config, &f.http, job)), None);
    }
    assert!(f.calls().is_empty());
    assert_eq!(f.outcomes(), [("dropped".to_string(), Some("read".to_string()))]);
}

#[test]
fn the_senders_views_read_as_without_slack() {
    let f = Fixture::new(vec![(200, None, json!({"ok": false, "error": "channel_not_found"}))]);
    // refused, on a host whose bridge has not badged it
    f.b.send(ALICE, "krisz", "first");
    let before = f.sender_views();
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes()[0].0, "refused");
    assert_eq!(f.sender_views(), before);
    // delivered, after a bridge badged it
    f.b.send(ALICE, "krisz", "second");
    f.b.bridge(FakeNotifier::default()).pass().unwrap();
    let before = f.sender_views();
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes()[1].0, "delivered");
    assert_eq!(f.sender_views(), before);
}

#[test]
fn one_dm_per_message_per_owner_and_never_about_his_own_post() {
    let f = Fixture::new(Vec::new());
    // a second human name of the same owner, on another host
    let other_host = f.b.other("krisz", "box2");
    f.b.call_as(&other_host, "human:krisz", "agent.register", json!({"name": "krisz2"})).unwrap();
    f.b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    f.b.ok(HUMAN, "channel.join", json!({"channel": "#dev"}));
    f.b.call_as(&other_host, "human:krisz", "channel.join", json!({"channel": "#dev"})).unwrap();

    let both = f.b.send(ALICE, "#dev", "@krisz and @krisz2, look");
    assert_eq!(f.b.status_of(&both, "krisz"), "pending");
    assert_eq!(f.b.status_of(&both, "krisz2"), "pending");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);

    // his own post as krisz pushes to krisz2, a member: no Slack DM about it
    let own = f.b.send(HUMAN, "#dev", "my own post");
    assert_eq!(f.b.status_of(&own, "krisz2"), "pending");
    assert_eq!(f.pass(), 0);
    assert_eq!(f.calls().len(), 1);
}
