//! The Slack worker against a local mock of chat.postMessage and users.lookupByEmail: which pages
//! become a Slack DM, what it says, how Slack's answers are recorded, where the member id comes
//! from (the env import, a lookup by email at startup, sign-in and page time, the admin), the
//! one re-lookup after a changed id, and what the page's sender reads. On SQLite, and on Postgres
//! with CONDE_TEST_PG.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::*;
use conde::dispatch::Principal;
use conde::protocol::Request;
use conde::slack::{self, SlackConfig, Token};
use conde::store::{AttemptRow, UserSlack};
use serde_json::{Value, json};

type Answer = (u16, Option<&'static str>, Value);

/// This test binary's log (every case writes to it).
fn log() -> String {
    static LOG: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = LOG.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = buf.clone();
        let _ = tracing_subscriber::fmt().with_ansi(false).with_writer(move || Sink(w.clone())).try_init();
        buf
    });
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

#[derive(Default)]
struct Mock {
    script: VecDeque<Answer>,
    /// (Authorization header, JSON body) per call.
    calls: Vec<(String, Value)>,
    /// users.lookupByEmail answers, by email (default users_not_found), and the emails asked.
    directory: std::collections::BTreeMap<String, String>,
    lookups: Vec<String>,
}

async fn lookup_by_email(
    State(mock): State<Arc<Mutex<Mock>>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or_default();
    assert_eq!(auth, "Bearer xoxb-test");
    let mut mock = mock.lock().unwrap();
    let email = q.get("email").cloned().unwrap_or_default();
    mock.lookups.push(email.clone());
    Json(match mock.directory.get(&email) {
        Some(id) => json!({"ok": true, "user": {"id": id}}),
        None => json!({"ok": false, "error": "users_not_found"}),
    })
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
    worker: Mutex<slack::Worker>,
    b: Board,
}

impl Fixture {
    /// alice (an agent of krisz) on the shared board dakai; krisz's human name krisz/human, owner
    /// krisz's Slack member id U0KRISZ; Slack answers `script`, then ok.
    fn new(script: Vec<Answer>) -> Fixture {
        Fixture::with(script, |b| {
            let uid = b.p.user_id;
            b.d.with_tx(|tx| Ok(tx.set_user_slack(uid, "U0KRISZ", "env")?)).unwrap();
        })
    }

    /// As [`Fixture::new`], `setup` run on the board before the worker starts.
    fn with(script: Vec<Answer>, setup: impl FnOnce(&Board)) -> Fixture {
        log();
        let rt =tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let mock = Arc::new(Mutex::new(Mock { script: script.into(), ..Mock::default() }));
        let app = Router::new()
            .route("/api/chat.postMessage", post(post_message))
            .route("/api/users.lookupByEmail", get(lookup_by_email))
            .with_state(mock.clone());
        let addr = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            addr
        });
        let b = Board::new();
        b.d.share("dakai");
        b.d.set_slack(true);
        b.register(ALICE, "alice");
        b.d.with_tx(|tx| Ok(conde::humans::ensure_all(tx)?)).unwrap();
        setup(&b);
        let config = Arc::new(SlackConfig {
            token: Token::new("xoxb-test"),
            import: Default::default(),
            api_base: format!("http://{addr}/api"),
            board: "dakai".into(),
            public_url: Some("https://conde.example".into()),
        });
        let http = slack::client().unwrap();
        let worker = Mutex::new(slack::Worker::new(b.d.clone(), config.clone(), http.clone()));
        Fixture { rt, mock, config, http, worker, b }
    }

    /// A page from alice to krisz's human; its id.
    fn page(&self, text: &str) -> String {
        let r = self.b.ok(ALICE, "human.page", json!({"text": text}));
        r["message_id"].as_str().unwrap().to_string()
    }

    /// A read of the page, written to the store: nothing marks a web-bound human's DM read today.
    fn read(&self, mid: &str) {
        let human = self.b.agent_id("human");
        let (mid, at) = (mid.to_string(), conde::util::iso_from_ms(self.b.d.now_ms()));
        self.b.d.with_tx(|tx| Ok(tx.mark_dm_read(&mid, &human, &at)?)).unwrap();
    }

    fn user(&self, name: &str) -> UserSlack {
        let users = self.b.d.with_conn(|tx| tx.users_slack()).unwrap();
        users.into_iter().find(|u| u.name == name).expect(name)
    }

    fn lookups(&self) -> Vec<String> {
        self.mock.lock().unwrap().lookups.clone()
    }

    fn pass(&self) -> usize {
        let mut w = self.worker.lock().unwrap();
        self.rt.block_on(w.pass())
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
        let mut sent = self.b.ok(ALICE, "receipt.list", json!({}));
        for r in sent["receipts"].as_array_mut().unwrap() {
            r.as_object_mut().unwrap().remove("slack");
        }
        json!({
            "sent": sent,
            "feed": self.b.ok(ALICE, "message.feed", json!({})),
            "history": owner("owner.history", json!({"agent": "alice", "with": "krisz/human"})),
            "fleet": fleet,
        })
    }
}

#[test]
fn a_page_sends_one_escaped_slack_message_naming_its_kind() {
    let f = Fixture::new(Vec::new());
    let mid = f.page("<!channel> & <@U999> look");
    // no bridge takes a human bound to the web host
    let mut br = f.b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    assert_eq!(f.b.status_of(&mid, "human"), "pending");

    assert_eq!(f.pass(), 1);
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    let (auth, body) = &calls[0];
    assert_eq!(auth, "Bearer xoxb-test");
    assert_eq!(body["channel"], "U0KRISZ");
    assert_eq!(
        body["text"],
        "krisz/alice is blocked on you:\n\
         >&lt;!channel&gt; &amp; &lt;@U999&gt; look\n\
         <https://conde.example/#/a/human/dm/krisz/alice|Open in conde on dakai>"
    );
    assert_eq!(body["unfurl_links"], false);
    assert_eq!(f.outcomes(), [("delivered".to_string(), None)]);
    assert_eq!(f.b.status_of(&mid, "human"), "pending");

    // nothing more, not even after the lease
    br.pass().unwrap();
    f.b.d.advance_clock(slack::LEASE_MS * 2);
    assert_eq!(f.pass(), 0);
    assert_eq!(f.calls().len(), 1);

    // a done page names its kind
    f.b.ok(ALICE, "human.done_pages", json!({"on": true}));
    f.b.ok(ALICE, "human.page", json!({"text": "migration finished", "kind": "done"}));
    assert_eq!(f.pass(), 1);
    let text = f.calls()[1].1["text"].as_str().unwrap().to_string();
    assert!(text.starts_with("krisz/alice finished:\n>migration finished\n"), "{text}");
}

#[test]
fn only_pages_reach_slack() {
    let f = Fixture::new(Vec::new());
    // another owner's human, with Slack: alice can only page her own
    let zed = f.b.other("zed", "zbox");
    f.b.call_as(&zed, "codex:t-zed", "agent.register", json!({"name": "zbot"})).unwrap();
    f.b.d.with_tx(|tx| {
        let id = tx.user_id("zed")?.unwrap();
        conde::humans::ensure(tx, id)?;
        Ok(tx.set_user_slack(id, "U0ZED", "env")?)
    })
    .unwrap();
    // a DM between agents, a reaction, a plain post and a post mentioning both humans
    let dm = f.b.send(ALICE, "zed/zbot", "agents talk");
    f.b.call_as(&zed, "codex:t-zed", "reaction.set", json!({"message_id": dm, "emoji": "👍", "present": true})).unwrap();
    f.b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    f.b.send(ALICE, "#dev", "a plain post badges only");
    let r = f.b.ok(ALICE, "message.send", json!({"to": {"channel": "#dev"}, "text": "@human @zed/human look"}));
    assert_eq!(r["not_delivered"], json!(["not delivered: @human is a human", "not delivered: @zed/human is a human"]));
    // a DM and a mention to the human stored before the deploy (no page): no Slack DM either
    let human = f.b.agent_id("human");
    let alice = f.b.agent_id("alice");
    f.b.d.with_tx(|tx| {
        for kind in ["dm", "mention"] {
            let id = conde::util::new_ulid();
            let now = f.b.d.now_ms();
            tx.insert_message(&conde::store::NewMessage {
                id: &id,
                sender_id: &alice,
                recipient_id: Some(&human),
                channel_id: None,
                text: "a legacy message",
                reply_to: None,
                created_at: &conde::util::iso_from_ms(now),
            })?;
            tx.insert_notification(&conde::store::NewNotification {
                id: &format!("{id}.{human}"),
                message_id: &id,
                recipient_id: &human,
                kind,
                binding_version: 1,
                next_attempt_at: now,
            })?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(f.pass(), 0);
    assert!(f.calls().is_empty());

    // a page older than the hour is never sent
    f.page("an hour ago");
    f.b.d.advance_clock(slack::MAX_AGE_MS + 1);
    assert_eq!(f.pass(), 0);
    assert!(f.calls().is_empty());
}

#[test]
fn a_final_slack_error_is_recorded_once() {
    let f = Fixture::new(vec![(200, None, json!({"ok": false, "error": "invalid_auth"}))]);
    let mid = f.page("hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes(), [("refused".to_string(), Some("slack: invalid_auth".to_string()))]);
    assert_eq!(f.b.status_of(&mid, "human"), "pending");
    // a configuration fault: an error naming the code and the owner, never the token
    let log = log();
    let line = log.lines().find(|l| l.contains("code=\"invalid_auth\"")).expect(&log);
    assert!(line.contains("ERROR") && line.contains("owner=krisz"), "{line}");
    assert!(!log.contains("xoxb-test"));
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
    f.page("hello");
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
    f.page("one");
    f.page("two");
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
    f.page("hello");
    let jobs = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    assert_eq!(jobs.len(), 1);
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
    f.page("hello");
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
    f.page("hello");
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
fn a_page_read_before_the_post_is_not_sent() {
    let f = Fixture::new(Vec::new());
    let mid = f.page("hello");
    let jobs = f.rt.block_on(slack::claim_due(&f.b.d, &f.config)).unwrap();
    f.read(&mid);
    for job in jobs {
        assert_eq!(f.rt.block_on(slack::deliver(&f.b.d, &f.config, &f.http, job)), None);
    }
    assert!(f.calls().is_empty());
    assert_eq!(f.outcomes(), [("dropped".to_string(), Some("read".to_string()))]);
    assert_eq!(f.b.ok(ALICE, "receipt.list", json!({}))["receipts"][0]["slack"], "not sent (read before the post)");
}

#[test]
fn the_senders_slack_field_follows_the_page_and_nothing_else_changes() {
    let f = Fixture::new(vec![(200, None, json!({"ok": false, "error": "invalid_auth"}))]);
    let slack_of = |f: &Fixture, i: usize| f.b.ok(ALICE, "receipt.list", json!({}))["receipts"][i]["slack"].clone();
    // refused
    f.page("first");
    assert_eq!(slack_of(&f, 0), "pending");
    let before = f.sender_views();
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes()[0].0, "refused");
    assert_eq!(f.sender_views(), before);
    assert_eq!(slack_of(&f, 0), "failed (slack: invalid_auth)");
    // delivered
    f.page("second");
    let before = f.sender_views();
    assert_eq!(f.pass(), 1);
    assert_eq!(f.outcomes()[1].0, "delivered");
    assert_eq!(f.sender_views(), before);
    assert_eq!(slack_of(&f, 1), "sent");
    // a plain DM has no slack field; an unsent page past the hour says so
    f.b.send(ALICE, "alice", "a note to self");
    assert!(f.b.ok(ALICE, "receipt.list", json!({}))["receipts"][2].get("slack").is_none());
    let r = f.b.ok(ALICE, "message.send", json!({"to": {"agent": "alice"}, "text": "x"}));
    assert!(r.get("not_delivered").is_none());
}

#[test]
fn one_dm_per_page() {
    let f = Fixture::new(Vec::new());
    f.page("one");
    f.page("two");
    assert_eq!(f.pass(), 2);
    assert_eq!(f.calls().len(), 2);
    assert_eq!(f.pass(), 0);
}

#[test]
fn the_env_map_is_imported_once_where_the_id_is_empty() {
    let f = Fixture::with(Vec::new(), |_| {});
    let zed = f.b.other("zed", "zbox");
    let _ = zed;
    f.b.d.with_tx(|tx| {
        let id = tx.user_id("zed")?.unwrap();
        Ok(tx.set_user_slack(id, "U0ZEDOWN", "admin")?)
    })
    .unwrap();
    let import = slack::parse_members("krisz=U0ENV,zed=U0ZEDENV,nobody=U0NOBODY").unwrap();
    let config = SlackConfig { import, ..(*f.config).clone() };
    let imported = f.b.d.with_tx(|tx| slack::import_env(tx, &config)).unwrap();
    assert_eq!(imported, ["krisz"]);
    assert_eq!((f.user("krisz").slack_member_id.as_deref(), f.user("krisz").slack_source.as_deref()), (Some("U0ENV"), Some("env")));
    assert_eq!(f.user("zed").slack_member_id.as_deref(), Some("U0ZEDOWN"));
    // a second start imports nothing: the column is no longer empty
    let config = SlackConfig { import: slack::parse_members("krisz=U0OTHER").unwrap(), ..(*f.config).clone() };
    assert!(f.b.d.with_tx(|tx| slack::import_env(tx, &config)).unwrap().is_empty());
    assert_eq!(f.user("krisz").slack_member_id.as_deref(), Some("U0ENV"));
}

#[test]
fn the_member_id_is_looked_up_at_startup_sign_in_and_page_time() {
    // krisz has an email and no id; Slack knows the email
    let f = Fixture::with(Vec::new(), |b| {
        let uid = b.p.user_id;
        b.d.with_tx(|tx| Ok(tx.set_user_email(uid, Some("krisz@dakai.io"))?)).unwrap();
    });
    f.mock.lock().unwrap().directory.insert("krisz@dakai.io".into(), "U0LOOKED".into());
    // startup: the first pass looks it up
    assert_eq!(f.pass(), 0);
    assert_eq!(f.lookups(), ["krisz@dakai.io"]);
    assert_eq!((f.user("krisz").slack_member_id.as_deref(), f.user("krisz").slack_source.as_deref()), (Some("U0LOOKED"), Some("lookup")));
    assert_eq!(f.pass(), 0);
    assert_eq!(f.lookups().len(), 1, "a user with an id is not looked up again");

    // zed signs in later with an email Slack does not know yet: looked up at sign-in only
    let zed = f.b.other("zed", "zbox");
    f.b.call_as(&zed, "codex:t-zed", "agent.register", json!({"name": "zbot"})).unwrap();
    let zid = f.b.d.with_tx(|tx| {
        let id = tx.user_id("zed")?.unwrap();
        conde::humans::ensure(tx, id)?;
        tx.set_user_email(id, Some("zed@dakai.io"))?;
        Ok(id)
    })
    .unwrap();
    assert_eq!(f.pass(), 0);
    assert_eq!(f.lookups().len(), 1, "no sign-in, no lookup");
    f.b.d.request_slack_lookup(zid);
    assert_eq!(f.pass(), 0);
    assert_eq!(f.lookups(), ["krisz@dakai.io", "zed@dakai.io"]);
    assert_eq!(f.user("zed").slack_member_id, None);

    // page time: zed's agent pages; the page is stored first, then the lookup finds the id
    let r = f.b.call_as(&zed, "codex:t-zed", "human.page", json!({"text": "need a go"})).unwrap();
    assert_eq!(r["status"], "stored, not paged");
    assert!(r["reason"].as_str().unwrap().starts_with("no Slack member id for zed yet; the board looks it up by zed@dakai.io"), "{r}");
    f.mock.lock().unwrap().directory.insert("zed@dakai.io".into(), "U0ZED".into());
    assert_eq!(f.pass(), 1);
    assert_eq!(f.lookups().len(), 3);
    assert_eq!(f.calls()[0].1["channel"], "U0ZED");
    // once per page: a page still waiting is not looked up at every pass
    f.b.d.with_tx(|tx| Ok(tx.set_user_email(zid, Some("zed2@dakai.io"))?)).unwrap();
    assert_eq!(f.pass(), 0);
    assert_eq!(f.lookups().len(), 3);
}

#[test]
fn a_send_to_a_changed_id_is_looked_up_again_and_retried_once() {
    let f = Fixture::with(vec![(200, None, json!({"ok": false, "error": "user_not_found"}))], |b| {
        let uid = b.p.user_id;
        b.d.with_tx(|tx| {
            tx.set_user_email(uid, Some("krisz@dakai.io"))?;
            Ok(tx.set_user_slack(uid, "U0OLD", "env")?)
        })
        .unwrap();
    });
    f.mock.lock().unwrap().directory.insert("krisz@dakai.io".into(), "U0NEW".into());
    f.page("hello");
    assert_eq!(f.pass(), 1);
    let channels: Vec<Value> = f.calls().iter().map(|c| c.1["channel"].clone()).collect();
    assert_eq!(channels, ["U0OLD", "U0NEW"]);
    assert_eq!(f.outcomes(), [("delivered".to_string(), None)]);
    assert_eq!((f.user("krisz").slack_member_id.as_deref(), f.user("krisz").slack_source.as_deref()), (Some("U0NEW"), Some("lookup")));
}

#[test]
fn an_unchanged_or_admin_id_is_final_after_one_refusal() {
    // the lookup finds the same id: final, one post
    let f = Fixture::with(vec![(200, None, json!({"ok": false, "error": "channel_not_found"}))], |b| {
        let uid = b.p.user_id;
        b.d.with_tx(|tx| {
            tx.set_user_email(uid, Some("krisz@dakai.io"))?;
            Ok(tx.set_user_slack(uid, "U0SAME", "lookup")?)
        })
        .unwrap();
    });
    f.mock.lock().unwrap().directory.insert("krisz@dakai.io".into(), "U0SAME".into());
    f.page("hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);
    assert_eq!(f.lookups(), ["krisz@dakai.io"]);
    assert_eq!(f.outcomes(), [("refused".to_string(), Some("slack: channel_not_found".to_string()))]);

    // the admin's id is never looked up nor replaced
    let f = Fixture::with(vec![(200, None, json!({"ok": false, "error": "user_not_found"}))], |b| {
        let uid = b.p.user_id;
        b.d.with_tx(|tx| {
            tx.set_user_email(uid, Some("krisz@dakai.io"))?;
            Ok(tx.set_user_slack(uid, "U0ADMIN", "admin")?)
        })
        .unwrap();
    });
    f.mock.lock().unwrap().directory.insert("krisz@dakai.io".into(), "U0OTHER".into());
    f.page("hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);
    assert!(f.lookups().is_empty());
    assert_eq!(f.user("krisz").slack_member_id.as_deref(), Some("U0ADMIN"));
    assert_eq!(f.outcomes(), [("refused".to_string(), Some("slack: user_not_found".to_string()))]);
}

#[test]
fn pages_waiting_for_a_member_id_never_starve_an_owner_with_one() {
    let f = Fixture::new(Vec::new());
    // zed has no member id and no email: his agents' pages wait, more of them than one pass takes
    let zed = f.b.other("zed", "zbox");
    let bots = ["codex:t-zed1", "codex:t-zed2", "codex:t-zed3"];
    for (n, bot) in bots.iter().enumerate() {
        f.b.call_as(&zed, bot, "agent.register", json!({"name": format!("zbot{n}")})).unwrap();
    }
    f.b.d.with_tx(|tx| Ok(conde::humans::ensure(tx, tx.user_id("zed")?.unwrap())?)).unwrap();
    for i in 0..27 {
        f.b.call_as(&zed, bots[i % 3], "human.page", json!({"text": format!("zed blocked {i}")})).unwrap();
    }
    f.page("krisz's own page");
    assert_eq!(f.pass(), 1);
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1["channel"], "U0KRISZ");
}

#[test]
fn account_inactive_is_the_bots_own_and_never_looks_the_member_up() {
    let f = Fixture::with(vec![(200, None, json!({"ok": false, "error": "account_inactive"}))], |b| {
        let uid = b.p.user_id;
        b.d.with_tx(|tx| {
            tx.set_user_email(uid, Some("krisz@dakai.io"))?;
            Ok(tx.set_user_slack(uid, "U0OLD", "env")?)
        })
        .unwrap();
    });
    f.mock.lock().unwrap().directory.insert("krisz@dakai.io".into(), "U0NEW".into());
    f.page("hello");
    assert_eq!(f.pass(), 1);
    assert_eq!(f.calls().len(), 1);
    assert!(f.lookups().is_empty());
    assert_eq!(f.user("krisz").slack_member_id.as_deref(), Some("U0OLD"));
    assert_eq!(f.outcomes(), [("refused".to_string(), Some("slack: account_inactive".to_string()))]);
}

#[test]
fn a_users_email_is_filled_from_the_identity_at_start_and_looked_up() {
    // users from before migration 0016: identities with their UPNs, no email stored
    let f = Fixture::with(Vec::new(), |b| {
        b.other("zed", "zbox");
        b.other("dora", "dbox");
        b.d.with_tx(|tx| {
            let id = |n: &str| tx.user_id(n).map(Option::unwrap);
            // as migration 0017 leaves Entra rows: the tenant's issuer
            let t = "https://login.microsoftonline.com/t/v2.0";
            tx.bind_identity(id("krisz")?, t, "o-krisz", None, Some("krisz@dakai.io"))?;
            tx.bind_identity(id("zed")?, t, "o-zed", None, Some("zed@dakai.io"))?;
            tx.set_user_email(id("zed")?, Some("kept@dakai.io"))?;
            tx.bind_identity(id("dora")?, t, "o-dora", None, Some("dora"))?;
            Ok(())
        })
        .unwrap();
    });
    f.mock.lock().unwrap().directory.insert("krisz@dakai.io".into(), "U0KRISZ2".into());
    let handle = f.rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        conde::cloud::start_on(f.b.d.clone(), listener, conde_remote::ServerConfig::default()).await.unwrap()
    });
    assert_eq!(f.user("krisz").email.as_deref(), Some("krisz@dakai.io"), "the UPN, at start");
    assert_eq!(f.user("zed").email.as_deref(), Some("kept@dakai.io"), "a stored email is never overwritten");
    assert_eq!(f.user("dora").email, None, "a UPN without an @ is no address");
    // the worker's first pass looks the filled-in user up
    assert_eq!(f.pass(), 0);
    assert!(f.lookups().contains(&"krisz@dakai.io".to_string()), "{:?}", f.lookups());
    assert_eq!(f.user("krisz").slack_member_id.as_deref(), Some("U0KRISZ2"));
    f.rt.block_on(handle.shutdown()).unwrap();
}
