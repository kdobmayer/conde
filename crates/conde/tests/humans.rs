//! Each owner's one human name on a shared board (docs/DESIGN-human-paging.md), through the
//! dispatcher: the ensure (new rows, existing ones rebound to the web host, extras retired, a
//! taken name, racing ensures, at user creation and at the server's start), the protection of the
//! human row, humans never sending (`--from` included), sends and @mentions to humans, and the
//! page ops. On SQLite, and on Postgres with CONDE_TEST_PG.

mod common;

use std::sync::{Arc, Barrier};

use common::*;
use conde::address::{Address, Kind};
use conde::dispatch::{Dispatcher, Principal};
use conde::humans::{self, Ensured};
use conde::protocol::ErrorCode;
use conde::store::Agent;
use serde_json::{Value, json};

const ZBOT: &str = "codex:t-zbot";

/// A shared board `dakai` that sends Slack DMs: krisz (alice) and zed (zbot) on their hosts,
/// each with their human name; krisz's member id U0KRISZ.
fn shared() -> (Board, Principal) {
    let b = Board::new();
    b.d.share("dakai");
    b.d.set_slack(true);
    b.register(ALICE, "alice");
    let zed = b.other("zed", "zbox");
    b.call_as(&zed, ZBOT, "agent.register", json!({"name": "zbot"})).unwrap();
    b.d.with_tx(|tx| Ok(humans::ensure_all(tx)?)).unwrap();
    let uid = b.p.user_id;
    b.d.with_tx(|tx| Ok(tx.set_user_slack(uid, "U0KRISZ", "env")?)).unwrap();
    (b, zed)
}

fn agent(d: &Dispatcher, owner: &str, name: &str) -> Option<Agent> {
    d.with_conn(|tx| {
        let Some(id) = tx.user_id(owner)? else { return Ok(None) };
        tx.agent_by_name(id, name)
    })
    .unwrap()
}

fn humans_of(d: &Dispatcher, owner: &str) -> Vec<Agent> {
    d.with_conn(|tx| {
        let id = tx.user_id(owner)?.unwrap();
        Ok(tx.owner_agents(id)?.into_iter().filter(Agent::is_human).collect())
    })
    .unwrap()
}

fn refused(r: Reply) -> (ErrorCode, String) {
    r.expect_err("refused")
}

#[test]
fn every_owner_gets_one_human_name_bound_to_the_web_host() {
    let (b, _) = shared();
    let krisz = agent(&b.d, "krisz", "human").unwrap();
    assert_eq!(krisz.address, Some(Address::new(Kind::Human, "krisz", "web")));
    assert_eq!(humans_of(&b.d, "zed").len(), 1);
    // an ensure on a row that is right changes nothing
    let again = b.d.with_tx(|tx| Ok(humans::ensure(tx, tx.user_id("krisz")?.unwrap())?)).unwrap();
    assert_eq!(again, Some(Ensured::Kept("krisz/human".into())));
    assert_eq!(agent(&b.d, "krisz", "human").unwrap().binding_version, krisz.binding_version);
    // the directory lists it as a human on the web host, never a target
    let list = b.ok(ALICE, "agent.list", json!({}));
    let row = list["agents"].as_array().unwrap().iter().find(|a| a["name"] == "krisz/human").cloned().unwrap();
    assert_eq!((row["kind"].as_str(), row["host"].as_str()), (Some("human"), Some("web")));
}

#[test]
fn an_existing_human_name_is_kept_and_rebound_to_the_web_host() {
    let b = Board::new();
    // before the ensure: zed's human name published from a host, pinned there
    let zed = b.other("zed", "zbox");
    b.call_as(&zed, "human:zed", "agent.register", json!({"name": "zed"})).unwrap();
    b.call_as(&zed, "human:zed", "agent.pin", json!({"name": "zed", "address": "human:zed"})).unwrap();
    let before = agent(&b.d, "zed", "zed").unwrap();
    b.d.share("dakai");
    let done = b.d.with_tx(|tx| Ok(humans::ensure_all(tx)?)).unwrap();
    assert!(done.contains(&Ensured::Rebound("zed/zed".into())), "{done:?}");
    assert!(done.contains(&Ensured::Created("krisz/human".into())), "{done:?}");
    let after = agent(&b.d, "zed", "zed").unwrap();
    assert_eq!(after.id, before.id, "the same row: history and links stay valid");
    assert_eq!(after.address, Some(Address::new(Kind::Human, "zed", "web")));
    assert_eq!((after.binding_version, after.pinned), (before.binding_version + 1, None));
    assert!(agent(&b.d, "zed", "human").is_none(), "no second human name");
    // the old host routes nothing of it any more: zed's host session no longer acts as the name
    let e = refused(b.call_as(&zed, "human:zed", "inbox.list", json!({})));
    assert_eq!(e.0, ErrorCode::Unauthenticated, "{e:?}");
}

#[test]
fn extra_human_names_are_retired_and_a_taken_name_is_left_alone() {
    let b = Board::new();
    let zed = b.other("zed", "zbox");
    let zed2 = b.other("zed", "zbox2");
    b.call_as(&zed, "human:zed", "agent.register", json!({"name": "zed"})).unwrap();
    b.call_as(&zed2, "human:zed", "agent.register", json!({"name": "zed2"})).unwrap();
    b.call_as(&zed2, "human:zed", "agent.unregister", json!({"name": "zed2"})).unwrap();
    b.call_as(&zed2, "human:zed", "agent.register", json!({"name": "zed2"})).unwrap();
    // krisz has an agent named `human` and no human name
    b.register(ALICE, "human");
    b.d.share("dakai");
    let done = b.d.with_tx(|tx| Ok(humans::ensure_all(tx)?)).unwrap();
    assert!(done.contains(&Ensured::Conflict("krisz/human".into())), "{done:?}");
    assert!(done.contains(&Ensured::Rebound("zed/zed2".into())), "the highest binding version stays: {done:?}");
    let kept = humans_of(&b.d, "zed");
    assert_eq!(kept.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["zed2"]);
    let retired = agent(&b.d, "zed", "zed").unwrap();
    assert!(retired.retired && retired.address.is_none(), "{retired:?}");
    let taken = agent(&b.d, "krisz", "human").unwrap();
    assert_eq!(taken.address.unwrap().kind, Kind::Codex, "the agent named human is left alone");
    assert!(humans_of(&b.d, "krisz").is_empty());
}

#[test]
fn racing_ensures_leave_one_human_name() {
    // SQLite: one board, many threads
    let d = Arc::new(Dispatcher::open_in_memory().unwrap());
    let uid = d.principal("krisz", "box", false).unwrap().user_id;
    race(vec![d.clone(); 8], uid);
    assert_eq!(humans_of(&d, "krisz").len(), 1);
    // Postgres: separate connections to one database, as two replicas
    let Some(url) = common::pg::fresh_database() else {
        eprintln!("skipped: set CONDE_TEST_PG to race on Postgres");
        return;
    };
    let open = || Arc::new(Dispatcher::connect(&url, conde::store::postgres::PgOpen::COMMAND).unwrap());
    let first = open();
    let uid = first.principal("krisz", "box", false).unwrap().user_id;
    let mut replicas = vec![first];
    replicas.extend((0..3).map(|_| open()));
    race(replicas.clone(), uid);
    assert_eq!(humans_of(&replicas[0], "krisz").len(), 1);
}

fn race(boards: Vec<Arc<Dispatcher>>, uid: i64) {
    let start = Arc::new(Barrier::new(boards.len()));
    let threads: Vec<_> = boards
        .into_iter()
        .map(|d| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                d.with_tx(|tx| Ok(humans::ensure(tx, uid)?)).unwrap()
            })
        })
        .collect();
    let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let created = outcomes.iter().filter(|o| matches!(o, Some(Ensured::Created(_)))).count();
    assert_eq!(created, 1, "{outcomes:?}");
}

#[test]
fn a_user_made_by_the_admin_key_or_a_sign_in_gets_a_human_name() {
    let b = Board::new();
    b.d.share("dakai");
    // `conde admin key create` and the admin API's key mint
    let key = |user: &str, host: &str| {
        let (u, h) = (user.to_string(), host.to_string());
        b.d.with_tx(move |tx| conde::ops::hosts::insert_key(tx, &u, &h, &conde::util::new_ulid(), "l", "2026-10-26T00:00:00.000Z"))
            .unwrap()
    };
    key("anna", "abox");
    assert_eq!(humans_of(&b.d, "anna").iter().map(Agent::canonical).collect::<Vec<_>>(), ["anna/human"]);
    use conde_remote::web::{Identity, WebStore};
    let web = conde::web_store::WebBoard::new(b.d.clone());
    web.ensure_user("bela").unwrap();
    assert_eq!(humans_of(&b.d, "bela").len(), 1);
    let person = |oid: &str| Identity {
        tid: "t".into(),
        oid: oid.into(),
        display_name: None,
        preferred_username: Some(format!("{oid}@dakai.io")),
        email: Some(format!("{oid}@dakai.io")),
    };
    web.create_user(&person("cili"), "cili").unwrap();
    web.bind_identity("dora", &person("dora")).unwrap();
    for owner in ["cili", "dora"] {
        assert_eq!(humans_of(&b.d, owner).len(), 1, "{owner}");
    }
    // the sign-in stored the email and asked for a lookup
    let cili = b.d.with_conn(|tx| tx.user_slack(tx.user_id("cili")?.unwrap())).unwrap().unwrap();
    assert_eq!(cili.email.as_deref(), Some("cili@dakai.io"));
    assert!(b.d.take_slack_lookups().contains(&cili.user_id));
}

#[test]
fn the_server_start_gives_every_owner_a_human_name() {
    let b = Board::new();
    b.register(ALICE, "alice");
    b.other("zed", "zbox");
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let handle = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        conde::cloud::start_on(b.d.clone(), listener, conde_remote::ServerConfig::default()).await.unwrap()
    });
    assert_eq!(humans_of(&b.d, "krisz").len(), 1);
    assert_eq!(humans_of(&b.d, "zed").len(), 1);
    rt.block_on(handle.shutdown()).unwrap();
}

#[test]
fn nothing_binds_moves_renames_or_seats_the_human_name() {
    let (b, zed) = shared();
    let before = agent(&b.d, "krisz", "human").unwrap();
    let protected = |r: Reply| {
        let (code, message) = refused(r);
        assert_eq!(code, ErrorCode::Forbidden, "{message}");
        assert!(message.contains("krisz/human is its owner's human name"), "{message}");
    };
    // register (a session taking the name), unregister, pin, unpin, rename (by the owner)
    protected(b.call(CAROL, "agent.register", json!({"name": "human"})));
    protected(b.call(HUMAN, "agent.unregister", json!({"name": "human"})));
    protected(b.call(HUMAN, "agent.pin", json!({"name": "human", "address": CAROL})));
    protected(b.call(HUMAN, "agent.pin", json!({"name": "human", "address": null})));
    protected(b.call(HUMAN, "agent.rename", json!({"old": "human", "new": "boss"})));
    // a human address is never pinned, a human never registers, `human` is never a new name
    let (code, _) = refused(b.call(HUMAN, "agent.pin", json!({"name": "alice", "address": "human:krisz"})));
    assert_eq!(code, ErrorCode::Forbidden);
    let (code, message) = refused(b.call(HUMAN, "agent.register", json!({"name": "krisz"})));
    assert_eq!(code, ErrorCode::Forbidden, "{message}");
    let (code, message) = refused(b.call(ALICE, "agent.rename", json!({"new": "human"})));
    assert_eq!((code, message.as_str()), (ErrorCode::BadRequest, "'human' is reserved on this board for each owner's human name"));
    // publish and unpublish from a migrated host
    let mbox = b.other("krisz", "mbox");
    b.d.with_tx(|tx| Ok(tx.set_host_mailbox_only("mbox")?)).unwrap();
    let publish = |body: Value| b.d.dispatch(&mbox, &request(None, "mailbox.publish", body));
    let r = publish(json!({"name": "human"}));
    assert!(r.error.unwrap().message.contains("krisz/human is its owner's human name"));
    let r = publish(json!({"name": "krisz", "human": true}));
    assert!(r.error.unwrap().message.starts_with("a human is not published"));
    let r = b.d.dispatch(&mbox, &request(None, "mailbox.unpublish", json!({"mailbox_id": before.id, "expected_generation": before.binding_version})));
    assert!(r.error.unwrap().message.contains("krisz/human is its owner's human name"));
    // channel invite of a human (own or another owner's), and a human joining
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    protected(b.call(ALICE, "channel.invite", json!({"channel": "#dev", "name": "human"})));
    let (code, message) = refused(b.call(ALICE, "channel.invite", json!({"channel": "#dev", "name": "zed/human"})));
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(message.contains("zed/human is its owner's human name"), "{message}");
    let (code, _) = refused(b.call(HUMAN, "channel.join", json!({"channel": "#dev"})));
    assert_eq!(code, ErrorCode::Forbidden);
    let _ = zed;
    // nothing changed
    let after = agent(&b.d, "krisz", "human").unwrap();
    assert_eq!((after.address, after.binding_version, after.pinned), (before.address, before.binding_version, None));
    let members = b.ok(ALICE, "channel.members", json!({"channel": "#dev"}));
    assert_eq!(members["members"].as_array().unwrap().len(), 1, "{members}");
}

fn request(actor: Option<&str>, op: &str, body: Value) -> conde::protocol::Request {
    let mut req = conde::protocol::Request::new(conde::util::new_ulid(), op, body);
    req.actor = actor.map(Into::into);
    req.idempotency_key = Some(conde::util::new_ulid());
    req
}

#[test]
fn a_human_never_sends_not_even_as_an_agent() {
    let (b, _) = shared();
    let mid = b.send(ALICE, "zed/zbot", "hi");
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    let no = "humans do not send on dakai: your agents page you on Slack (conde page), and you read the board on its \
              web page; ask one of your agents to send";
    let count = b.message_count();
    for (op, body) in [
        ("message.send", json!({"to": {"agent": "zed/zbot"}, "text": "from a human"})),
        ("message.send", json!({"to": {"channel": "#dev"}, "text": "a human's post"})),
        ("reaction.set", json!({"message_id": mid, "emoji": "👍", "present": true})),
        ("channel.invite", json!({"channel": "#dev", "name": "zed/zbot"})),
        ("channel.join", json!({"channel": "#dev"})),
        ("channel.create", json!({"channel": "#mine"})),
        ("human.page", json!({"text": "paging myself"})),
    ] {
        // as the human, and as the human acting for alice (`--from`, a local socket's override)
        assert_eq!(refused(b.call(HUMAN, op, body.clone())), (ErrorCode::Forbidden, no.to_string()), "{op}");
        let from = b.request(&b.p, HUMAN, Some("alice"), op, body, Some(&conde::util::new_ulid()));
        assert_eq!(refused(from), (ErrorCode::Forbidden, no.to_string()), "{op} --from alice");
    }
    assert_eq!(b.message_count(), count, "nothing stored");
    // the local board is unchanged: there the human sends, as themselves or for an agent
    let local = Board::new();
    local.register(ALICE, "alice");
    local.register(BOB, "bob");
    local.register(HUMAN, "krisz");
    local.send(HUMAN, "bob", "from the human");
    local.request(&local.p, HUMAN, Some("alice"), "message.send", json!({"to": {"agent": "bob"}, "text": "as alice"}), Some("k")).unwrap();
}

#[test]
fn a_send_to_a_human_is_refused_and_names_the_way() {
    let (b, zed) = shared();
    let count = b.message_count();
    let own = refused(b.call(ALICE, "message.send", json!({"to": {"agent": "human"}, "text": "hi"})));
    assert_eq!(own, (ErrorCode::Forbidden, "use `conde page` to reach your human".to_string()));
    let other = refused(b.call(ALICE, "message.send", json!({"to": {"agent": "zed/human"}, "text": "hi"})));
    assert_eq!(
        other,
        (ErrorCode::Forbidden, "humans are reached through their own agents; message one of `zed`'s agents instead".to_string())
    );
    let mine = refused(b.call_as(&zed, ZBOT, "message.send", json!({"to": {"agent": "krisz/human"}, "text": "hi"})));
    assert!(mine.1.contains("one of `krisz`'s agents"), "{mine:?}");
    assert_eq!(b.message_count(), count);
}

#[test]
fn a_channel_mention_of_a_human_is_listed_not_delivered() {
    let (b, zed) = shared();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.call_as(&zed, ZBOT, "channel.join", json!({"channel": "#dev"})).unwrap();
    let r = b.ok(ALICE, "message.send", json!({"to": {"channel": "#dev"}, "text": "@human @zed/human @zed/zbot `conde page @human`"}));
    assert_eq!(r["not_delivered"], json!(["not delivered: @human is a human", "not delivered: @zed/human is a human"]));
    let mid = r["message_id"].as_str().unwrap();
    assert_eq!(b.status_of(mid, "zbot"), "pending", "the agent's mention is delivered");
    assert_eq!(b.status_of(mid, "human"), "none");
    // @everybody reaches the members, never a human, even one seated in the channel before the
    // board kept human names (a legacy membership)
    let (dev, human) = (b.d.with_conn(|tx| tx.channel_by_name("dev")).unwrap().unwrap().id, b.agent_id("human"));
    b.d.with_tx(|tx| Ok(tx.insert_membership(&dev, &human, 0)?)).unwrap();
    let all = b.ok(ALICE, "message.send", json!({"to": {"channel": "#dev"}, "text": "@everybody standup"}));
    assert!(all.get("not_delivered").is_none());
    assert_eq!(b.status_of(all["message_id"].as_str().unwrap(), "zbot"), "pending");
    let humans: Vec<_> = b.notifications().into_iter().filter(|(k, _, _)| k.ends_with(".human")).collect();
    assert!(humans.is_empty(), "{humans:?}");
}

#[test]
fn a_page_is_stored_and_paged_and_a_done_page_follows_the_setting() {
    let (b, zed) = shared();
    let r = b.ok(ALICE, "human.page", json!({"text": "need a go for the deploy"}));
    assert_eq!((r["status"].as_str(), r["to"].as_str(), r["kind"].as_str()), (Some("paged"), Some("krisz/human"), Some("blocked")));
    let mid = r["message_id"].as_str().unwrap();
    let m = b.d.with_conn(|tx| tx.message_by_id(mid)).unwrap().unwrap();
    assert_eq!((m.sender.as_str(), m.recipient.as_deref(), m.page.as_deref()), ("krisz/alice", Some("krisz/human"), Some("blocked")));
    assert_eq!(b.notifications().iter().find(|(k, _, _)| k == &format!("{mid}.human")).map(|(_, kind, _)| kind.as_str()), Some("dm"));

    // done pages are off by default: not stored
    let count = b.message_count();
    let r = b.ok(ALICE, "human.page", json!({"text": "deploy finished", "kind": "done"}));
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("not paged"), Some("done pages are off")));
    assert!(r.get("message_id").is_none());
    assert_eq!(b.message_count(), count);
    // on: stored and paged
    assert_eq!(b.ok(ALICE, "human.done_pages", json!({"on": true})), json!({"human": "krisz/human", "done_pages": true}));
    let r = b.ok(ALICE, "human.page", json!({"text": "deploy finished", "kind": "done"}));
    assert_eq!(r["status"], "paged");
    let m = b.d.with_conn(|tx| tx.message_by_id(r["message_id"].as_str().unwrap())).unwrap().unwrap();
    assert_eq!(m.page.as_deref(), Some("done"));
    b.ok(ALICE, "human.done_pages", json!({"on": false}));
    assert_eq!(b.ok(ALICE, "human.page", json!({"text": "x", "kind": "done"}))["status"], "not paged");

    // zed has no member id and no email: stored, not paged, with the fix
    let r = b.call_as(&zed, ZBOT, "human.page", json!({"text": "blocked"})).unwrap();
    assert_eq!(r["status"], "stored, not paged");
    assert_eq!(
        r["reason"],
        "no Slack member id and no sign-in email for zed; the board admin sets it: conde admin slack zed <member-id>"
    );
    // a board without Slack stores it and says so
    b.d.set_slack(false);
    let r = b.ok(ALICE, "human.page", json!({"text": "blocked again"}));
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("stored, not paged"), Some("dakai sends no Slack DMs (no bot token is set); your human reads it on the web board")));
    // a page is the caller's own human's: no recipient is taken from the body
    let r = b.ok(ALICE, "human.page", json!({"text": "to whom?", "to": {"agent": "zed/human"}}));
    assert_eq!(r["to"], "krisz/human");
    // the kind is blocked or done; the local board pages nobody
    assert_eq!(refused(b.call(ALICE, "human.page", json!({"text": "x", "kind": "fyi"}))).0, ErrorCode::BadRequest);
    let local = Board::new();
    local.register(ALICE, "alice");
    let e = refused(local.call(ALICE, "human.page", json!({"text": "x"})));
    assert_eq!(e, (ErrorCode::Forbidden, "paging reaches your human on a shared board: name it with --board <alias>".into()));
}

#[test]
fn a_page_is_one_line_of_at_most_140_characters() {
    let (b, _) = shared();
    let full = "é".repeat(140);
    assert_eq!(b.ok(ALICE, "human.page", json!({"text": full}))["status"], "paged");
    let count = b.message_count();
    let long = refused(b.call(ALICE, "human.page", json!({"text": "é".repeat(141)})));
    assert_eq!(long, (ErrorCode::BadRequest, "a page is at most 140 characters; this one is 141".into()));
    let two = refused(b.call(ALICE, "human.page", json!({"text": "first\nsecond"})));
    assert_eq!(two, (ErrorCode::BadRequest, "a page is one line: say what you need in one line".into()));
    for separator in ['\u{2028}', '\u{2029}'] {
        let two = refused(b.call(ALICE, "human.page", json!({"text": format!("first{separator}second")})));
        assert_eq!(two, (ErrorCode::BadRequest, "a page is one line: say what you need in one line".into()), "{separator:?}");
    }
    let empty = refused(b.call(ALICE, "human.page", json!({"text": "   "})));
    assert_eq!(empty.0, ErrorCode::BadRequest);
    assert_eq!(b.message_count(), count);
}

#[test]
fn whoami_and_human_status_name_the_human() {
    let (b, zed) = shared();
    let want = json!({"human": "krisz/human", "board": "dakai", "slack": true, "done_pages": false});
    assert_eq!(b.ok(ALICE, "human.status", json!({})), want);
    let who = b.ok(ALICE, "agent.whoami", json!({}));
    assert_eq!(who["human"], want);
    assert_eq!(
        conde::surface::whoami(&who).lines().last().unwrap(),
        "human: krisz/human on dakai · slack id: set · done pages: off"
    );
    let zw = b.call_as(&zed, ZBOT, "agent.whoami", json!({})).unwrap();
    assert_eq!((zw["human"]["human"].as_str(), zw["human"]["slack"].as_bool()), (Some("zed/human"), Some(false)));
    // a local board has no human line
    let local = Board::new();
    local.register(ALICE, "alice");
    assert!(local.ok(ALICE, "agent.whoami", json!({})).get("human").is_none());
}

#[test]
fn an_agent_pages_its_human_at_most_ten_times_an_hour() {
    let (b, zed) = shared();
    for i in 0..10 {
        assert_eq!(b.ok(ALICE, "human.page", json!({"text": format!("blocked {i}")}))["status"], "paged", "{i}");
    }
    let count = b.message_count();
    let over = refused(b.call(ALICE, "human.page", json!({"text": "one more"})));
    assert_eq!(
        over,
        (ErrorCode::Forbidden, "not paged: you paged your human 10 times in the last hour, the limit; say what you need in one page when the oldest is an hour old".into())
    );
    assert_eq!(b.message_count(), count, "the refused page is not stored");
    // the cap is per agent: another agent of the owner still pages, and zed's are his own
    b.register(BOB, "bob");
    assert_eq!(b.ok(BOB, "human.page", json!({"text": "bob blocked"}))["status"], "paged");
    assert_eq!(b.call_as(&zed, ZBOT, "human.page", json!({"text": "zed blocked"})).unwrap()["status"], "stored, not paged");
    // an hour after the first, alice pages again
    b.d.advance_clock(3_600_001);
    assert_eq!(b.ok(ALICE, "human.page", json!({"text": "blocked again"}))["status"], "paged");
}

#[test]
fn a_retired_name_human_is_taken_over_by_the_human_name() {
    let b = Board::new();
    // krisz once had an agent named `human`, unbound and retired before the board kept human names
    b.register(ALICE, "human");
    b.ok(ALICE, "agent.unregister", json!({"name": "human"}));
    b.ok(HUMAN, "agent.retire", json!({"name": "human"}));
    let old = agent(&b.d, "krisz", "human").unwrap();
    assert!(old.retired);
    b.d.share("dakai");
    let done = b.d.with_tx(|tx| Ok(humans::ensure(tx, tx.user_id("krisz")?.unwrap())?)).unwrap();
    assert_eq!(done, Some(Ensured::Rebound("krisz/human".into())));
    let now = agent(&b.d, "krisz", "human").unwrap();
    assert_eq!((now.id, now.retired), (old.id, false));
    assert_eq!(now.address, Some(Address::new(Kind::Human, "krisz", "web")));
}
