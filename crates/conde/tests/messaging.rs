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

/// Astra's probe: a time search keeps its cutoff through the continuation it prints, though
/// copied history appends older dates after newer ones; a plain number pages as before.
#[test]
fn review_time_filter_survives_cursor_continuation_with_copied_history() {
    let b = Board::sqlite();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let sender = b.agent_id("alice");
    let recipient = b.agent_id("bob");
    b.d.with_tx(|tx| {
        for n in 0..conde::protocol::PAGE + 2 {
            let old = n == conde::protocol::PAGE;
            tx.insert_message(&conde::store::NewMessage {
                id: &conde::util::new_ulid(), sender_id: &sender, recipient_id: Some(&recipient), channel_id: None,
                text: if old { "needle copied older message" } else { "needle qualifying message" }, reply_to: None,
                created_at: if old { "2020-01-01T00:00:00.000Z" } else { "2026-10-09T00:00:00.000Z" },
            })?;
        }
        Ok(())
    }).unwrap();
    let first = b.ok(BOB, "search", json!({"pattern":"needle", "after":"2026-01-01T00:00:00Z"}));
    assert_eq!(first["hits"].as_array().unwrap().len(), conde::protocol::PAGE);
    let cursor = first["next_cursor"].clone();
    let next = b.ok(BOB, "search", json!({"pattern":"needle", "after":cursor}));
    let lines: Vec<_> = next["hits"].as_array().unwrap().iter().map(|m| m["line"].as_str().unwrap()).collect();
    assert_eq!(lines, ["needle qualifying message"], "documented continuation must keep the time predicate");
    // the plain number still pages by order alone
    let seq = first["hits"][conde::protocol::PAGE - 1]["seq"].as_i64().unwrap();
    let plain = b.ok(BOB, "search", json!({"pattern":"needle", "after": seq}));
    assert_eq!(plain["hits"].as_array().unwrap().len(), 2, "{plain}");
    assert!(plain["next_cursor"].is_null(), "{plain}");
}

/// A pattern that matches nothing stops after the scan budget and hands back a cursor; the
/// continuation picks up where it stopped (and keeps a time cutoff). On SQLite and, with
/// CONDE_TEST_PG, on Postgres.
fn search_scan_is_bounded(b: Board) {
    use conde::protocol::SEARCH_SCAN;
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let (sender, recipient) = (b.agent_id("alice"), b.agent_id("bob"));
    let filler = SEARCH_SCAN + 50;
    b.d.with_tx(|tx| {
        for n in 0..=filler {
            let text = if n == filler { "the needle at the end".to_string() } else { format!("filler {n}") };
            tx.insert_message(&conde::store::NewMessage {
                id: &conde::util::new_ulid(), sender_id: &sender, recipient_id: Some(&recipient), channel_id: None,
                text: &text, reply_to: None, created_at: "2026-10-09T00:00:00.000Z",
            })?;
        }
        Ok(())
    })
    .unwrap();
    let first = b.ok(BOB, "search", json!({"pattern": "needle"}));
    assert!(first["hits"].as_array().unwrap().is_empty(), "{first}");
    assert_eq!((first["partial"].as_bool(), first["scanned"].as_i64()), (Some(true), Some(SEARCH_SCAN)), "{first}");
    let cursor = first["next_cursor"].as_i64().expect("a numeric cursor");
    let next = b.ok(BOB, "search", json!({"pattern": "needle", "after": cursor}));
    let lines: Vec<_> = next["hits"].as_array().unwrap().iter().map(|h| h["line"].as_str().unwrap()).collect();
    assert_eq!(lines, ["the needle at the end"], "{next}");
    assert!(next["partial"].is_null() && next["next_cursor"].is_null(), "{next}");
    // A time search's continuation keeps its cutoff.
    let timed = b.ok(BOB, "search", json!({"pattern": "needle", "after": "2026-01-01T00:00:00Z"}));
    let token = timed["next_cursor"].as_str().expect("a <seq>@<time> cursor").to_string();
    assert!(token.starts_with(&format!("{cursor}@")), "{token}");
    let next = b.ok(BOB, "search", json!({"pattern": "needle", "after": token}));
    assert_eq!(next["hits"].as_array().unwrap().len(), 1, "{next}");
}

#[test]
fn search_scan_is_bounded_on_sqlite() {
    search_scan_is_bounded(Board::sqlite());
}

#[test]
fn search_scan_is_bounded_on_the_test_store() {
    // Postgres when CONDE_TEST_PG is set, else SQLite again.
    search_scan_is_bounded(Board::new());
}

/// The reviewer's probe: a time-only search over a long history finds a recent match in one
/// call (it starts where that date begins, not at the oldest message), and a recent message
/// copied in at a low seq is found too. On SQLite and, with CONDE_TEST_PG, on Postgres.
fn a_time_search_starts_at_its_date(b: Board) {
    use conde::protocol::SEARCH_SCAN;
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let (sender, recipient) = (b.agent_id("alice"), b.agent_id("bob"));
    let dm = |tx: &dyn conde::store::Tx, text: &str, at: &str| -> anyhow::Result<()> {
        tx.insert_message(&conde::store::NewMessage {
            id: &conde::util::new_ulid(), sender_id: &sender, recipient_id: Some(&recipient), channel_id: None,
            text, reply_to: None, created_at: at,
        })?;
        Ok(())
    };
    b.d.with_tx(|tx| {
        // copied history: a recent message at a low seq, then a long old history after it
        dm(tx, "needle copied recent", "2026-10-13T09:00:00+02:00")?;
        for n in 0..SEARCH_SCAN + 50 {
            dm(tx, &format!("old {n}"), "2020-01-01T00:00:00.000Z")?;
        }
        dm(tx, "needle recent", "2026-10-13T08:00:00.000Z")?;
        Ok(())
    })
    .unwrap();
    let r = b.ok(BOB, "search", json!({"pattern": "needle", "after": "2026-10-12T00:00:00Z"}));
    let lines: Vec<_> = r["hits"].as_array().unwrap().iter().map(|h| h["line"].as_str().unwrap()).collect();
    assert_eq!(lines, ["needle copied recent", "needle recent"], "{r}");
    assert!(r["partial"].is_null(), "one call, not stopped: {r}");
    // nothing on or after a later date: no scan at all
    let r = b.ok(BOB, "search", json!({"pattern": "needle", "after": "2027-01-01T00:00:00Z"}));
    assert!(r["hits"].as_array().unwrap().is_empty() && r["next_cursor"].is_null(), "{r}");
}

#[test]
fn a_time_search_starts_at_its_date_on_sqlite() {
    a_time_search_starts_at_its_date(Board::sqlite());
}

#[test]
fn a_time_search_starts_at_its_date_on_the_test_store() {
    a_time_search_starts_at_its_date(Board::new());
}
