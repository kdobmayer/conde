mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use common::*;
use conde::protocol::{ErrorCode, MAX_ATTACHMENT, MAX_TEXT};
use serde_json::{Value, json};

fn setup() -> Board {
    let b = Board::new();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.register(CAROL, "carol");
    b
}

fn items(v: &Value, key: &str) -> Vec<Value> {
    v[key].as_array().cloned().unwrap_or_default()
}

#[test]
fn dm_inbox_read_and_receipt() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "hello bob\nsecond line");
    let inbox = b.ok(BOB, "inbox.list", json!({}));
    let rows = items(&inbox, "items");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "dm");
    assert_eq!(rows[0]["from"], "krisz/alice");
    assert_eq!(rows[0]["preview"], "hello bob");
    assert_eq!(b.ok(ALICE, "receipt.list", json!({}))["receipts"][0]["status"], "unread");

    let read = b.ok(BOB, "message.read", json!({"id": mid}));
    assert_eq!(read["messages"][0]["text"], "hello bob\nsecond line");
    assert!(items(&b.ok(BOB, "inbox.list", json!({})), "items").is_empty());
    let status = b.ok(ALICE, "receipt.list", json!({}))["receipts"][0]["status"].as_str().unwrap().to_string();
    assert!(status.starts_with("read 20"), "{status}");
    // the sender sees its own DM but reading it marks nothing
    b.ok(ALICE, "message.read", json!({"id": mid}));
    // unknown recipients are refused at once
    let err = b.call(ALICE, "message.send", json!({"to": {"agent": "nobody"}, "text": "x"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::NotFound);
}

#[test]
fn channels_unread_excludes_own_posts_and_markers_only_advance() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#Dev"}));
    let old = b.send(ALICE, "#dev", "history before bob");
    b.ok(ALICE, "channel.invite", json!({"name": "bob", "channel": "#dev"}));
    // history is readable, never unread; the invite post is
    let inbox = items(&b.ok(BOB, "inbox.list", json!({})), "items");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["kind"], "#dev");
    assert!(inbox[0]["preview"].as_str().unwrap().contains("@krisz/bob was added to #dev"));
    let m1 = b.send(BOB, "#dev", "bob's own post");
    let m2 = b.send(ALICE, "#dev", "second");
    assert_eq!(items(&b.ok(BOB, "inbox.list", json!({})), "items").len(), 2);

    // read the newest by id, then an older one: the marker never goes back
    b.ok(BOB, "message.read", json!({"id": m2}));
    b.ok(BOB, "message.read", json!({"id": old}));
    let members = b.ok(BOB, "channel.members", json!({"channel": "#dev"}));
    let bob = items(&members, "members").into_iter().find(|m| m["name"] == "krisz/bob").unwrap();
    assert_eq!(bob["behind"], 0);
    assert!(items(&b.ok(BOB, "inbox.list", json!({})), "items").is_empty());
    let _ = m1;

    // non-members can neither post nor read
    let err = b.call(CAROL, "message.send", json!({"to": {"channel": "#dev"}, "text": "x"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    assert_eq!(b.call(CAROL, "message.get", json!({"id": m2})).unwrap_err().0, ErrorCode::NotFound);
    let joined = b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
    assert_eq!(joined["already_member"], false);
    assert_eq!(b.ok(CAROL, "channel.join", json!({"channel": "#dev"}))["already_member"], true);
    assert_eq!(b.ok(CAROL, "message.get", json!({"id": m2}))["message"]["text"], "second");
    let list = b.ok(CAROL, "channel.list", json!({}));
    assert_eq!(list["channels"][0]["members"], 3);
    b.ok(CAROL, "channel.leave", json!({"channel": "#dev"}));
    assert_eq!(b.call(CAROL, "channel.leave", json!({"channel": "#dev"})).unwrap_err().0, ErrorCode::Forbidden);
}

#[test]
fn read_channel_marks_all_and_dm_read_keeps_first_time() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#c"}));
    b.ok(ALICE, "channel.invite", json!({"name": "bob", "channel": "#c"}));
    b.send(ALICE, "#c", "one");
    b.send(BOB, "#c", "two");
    let r = b.ok(BOB, "message.read", json!({"channel": "#c"}));
    // channel read shows own posts too, like kond
    assert_eq!(items(&r, "messages").len(), 3);
    assert!(items(&b.ok(BOB, "message.read", json!({"channel": "#c"})), "messages").is_empty());

    let mid = b.send(ALICE, "bob", "dm");
    let all = b.ok(BOB, "message.read", json!({}));
    assert_eq!(items(&all, "messages").len(), 1);
    let at = |b: &Board| -> String {
        let bob = b.agent_id("bob");
        b.d.with_conn(|tx| tx.dm_read_at(&mid, &bob)).unwrap().unwrap()
    };
    let first = at(&b);
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.ok(BOB, "message.read", json!({"id": mid}));
    assert_eq!(at(&b), first);
}

#[test]
fn reactions_set_and_clear() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "react to me");
    let r = b.ok(BOB, "reaction.set", json!({"message_id": mid, "emoji": "👍", "present": true}));
    assert_eq!(r["place"], "dm");
    b.ok(ALICE, "reaction.set", json!({"message_id": mid, "emoji": "👍", "present": true}));
    let view = b.ok(ALICE, "message.get", json!({"id": mid}))["message"].clone();
    assert_eq!(view["reactions"], json!([{"emoji": "👍", "by": ["krisz/alice", "krisz/bob"]}]));
    // last event wins; setting twice is idempotent
    b.ok(BOB, "reaction.set", json!({"message_id": mid, "emoji": "👍", "present": false}));
    b.ok(BOB, "reaction.set", json!({"message_id": mid, "emoji": "👍", "present": false}));
    let view = b.ok(ALICE, "message.get", json!({"id": mid}))["message"].clone();
    assert_eq!(view["reactions"], json!([{"emoji": "👍", "by": ["krisz/alice"]}]));
    // invisible to carol
    let err = b.call(CAROL, "reaction.set", json!({"message_id": mid, "emoji": "👍", "present": true})).unwrap_err();
    assert_eq!(err.0, ErrorCode::NotFound);
}

#[test]
fn search_scope_and_targets() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#a"}));
    b.ok(CAROL, "channel.create", json!({"channel": "#secret"}));
    b.send(ALICE, "#a", "Deploy plan\nthe DEPLOY is friday");
    b.send(CAROL, "#secret", "deploy secret");
    b.send(BOB, "alice", "deploy question from bob");
    b.send(CAROL, "alice", "deploy from carol");
    let hits = items(&b.ok(ALICE, "search", json!({"pattern": "deploy"})), "hits");
    let places: Vec<_> = hits.iter().map(|h| h["place"].as_str().unwrap()).collect();
    assert_eq!(places, ["#a", "dm", "dm"]);
    assert_eq!(hits[0]["line"], "Deploy plan");
    let hits = items(&b.ok(ALICE, "search", json!({"pattern": "deploy", "target": "bob"})), "hits");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["from"], "krisz/bob");
    let hits = items(&b.ok(ALICE, "search", json!({"pattern": "friday", "target": "#a"})), "hits");
    assert_eq!(hits.len(), 1);
    assert_eq!(b.call(ALICE, "search", json!({"pattern": "("})).unwrap_err().0, ErrorCode::BadRequest);
}

#[test]
fn attachments_round_trip_with_caps() {
    let b = setup();
    let att = |name: &str, bytes: &[u8]| json!({"filename": name, "data": B64.encode(bytes)});
    let r = b.ok(
        ALICE,
        "message.send",
        json!({"to": {"agent": "bob"}, "text": "files", "attachments": [att("/tmp/x/note.txt", b"one"), att("note.txt", b"two")]}),
    );
    let mid = r["message_id"].as_str().unwrap();
    let view = b.ok(BOB, "message.get", json!({"id": mid}))["message"].clone();
    let names: Vec<_> = items(&view, "attachments").iter().map(|a| a["filename"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["note.txt", "note-2.txt"]);
    let id = view["attachments"][1]["attachment_id"].as_str().unwrap();
    let got = b.ok(BOB, "attachment.get", json!({"attachment_id": id}));
    assert_eq!(B64.decode(got["data"].as_str().unwrap()).unwrap(), b"two");
    assert_eq!(b.call(CAROL, "attachment.get", json!({"attachment_id": id})).unwrap_err().0, ErrorCode::NotFound);

    let big = vec![0u8; MAX_ATTACHMENT + 1];
    let err = b
        .call(ALICE, "message.send", json!({"to": {"agent": "bob"}, "text": "x", "attachments": [att("big", &big)]}))
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::TooLarge);
    let long = "x".repeat(MAX_TEXT + 1);
    assert_eq!(b.call(ALICE, "message.send", json!({"to": {"agent": "bob"}, "text": long})).unwrap_err().0, ErrorCode::TooLarge);
    // failed sends commit nothing
    assert_eq!(b.message_count(), 1);
}

#[test]
fn replies_reference_visible_messages() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "q");
    let r = b.ok(BOB, "message.send", json!({"to": {"agent": "alice"}, "text": "a", "reply_to": mid}));
    let view = b.ok(ALICE, "message.get", json!({"id": r["message_id"]}))["message"].clone();
    assert_eq!(view["reply_to"], mid.as_str());
    let err = b.call(CAROL, "message.send", json!({"to": {"agent": "alice"}, "text": "a", "reply_to": mid})).unwrap_err();
    assert_eq!(err.0, ErrorCode::NotFound);
}
