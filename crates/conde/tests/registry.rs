mod common;

use common::*;
use conde::notify::{NotifyOutcome, ReceiptStatus};
use conde::protocol::ErrorCode;
use serde_json::json;

#[test]
fn register_is_self_only() {
    let b = Board::new();
    let err = b
        .call(ALICE, "agent.register", json!({"name": "overseer", "address": "claude:4c06-0000"}))
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    assert!(err.1.contains("self-only"), "{}", err.1);
    assert!(b.call(ALICE, "agent.resolve", json!({"name": "overseer"})).is_err());

    let r = b.ok(ALICE, "agent.register", json!({"name": "Vegrehajtas.Codex", "address": "codex:t-alice"}));
    assert_eq!(r["name"], "krisz/vegrehajtas.codex");
    assert_eq!(r["address"], "codex:t-alice@box");
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/vegrehajtas.codex");
}

#[test]
fn a_mailbox_address_is_neither_a_v1_actor_nor_a_binding() {
    let b = Board::new();
    let err = b.call("mailbox:01MBX", "agent.register", json!({"name": "worker"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::BadRequest, "{}", err.1);
    b.register(HUMAN, "krisz");
    let err = b.call(HUMAN, "agent.pin", json!({"name": "role", "address": "mailbox:01MBX"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::BadRequest, "{}", err.1);
}

#[test]
fn reregistering_moves_the_name_to_the_caller() {
    let b = Board::new();
    b.register(CAROL, "role");
    b.register(ALICE, "role");
    assert_eq!(b.ok(BOB, "agent.resolve", json!({"name": "role"}))["address"], "codex:t-alice@box");
    let err = b.call(CAROL, "agent.whoami", json!({})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Unauthenticated);
    assert!(err.1.contains("claude:s-carol"), "{}", err.1);
}

#[test]
fn everybody_is_reserved_and_names_are_validated() {
    let b = Board::new();
    assert_eq!(b.call(ALICE, "agent.register", json!({"name": "everybody"})).unwrap_err().0, ErrorCode::BadRequest);
    assert_eq!(b.call(ALICE, "agent.register", json!({"name": "../x"})).unwrap_err().0, ErrorCode::BadRequest);
}

#[test]
fn whoami_with_two_names_on_one_session_is_ambiguous() {
    let b = Board::new();
    b.register(ALICE, "a1");
    b.register(ALICE, "a2");
    let err = b.call(ALICE, "agent.whoami", json!({})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Conflict);
    assert!(err.1.contains("krisz/a1, krisz/a2"), "{}", err.1);
}

#[test]
fn pins_hold_roles() {
    let b = Board::new();
    b.register(HUMAN, "krisz");
    // agents cannot pin
    assert_eq!(
        b.call(ALICE, "agent.pin", json!({"name": "role", "address": ALICE})).unwrap_err().0,
        ErrorCode::Forbidden
    );
    b.ok(HUMAN, "agent.pin", json!({"name": "role", "address": "codex:t-alice"}));
    // the pin binds the role at once
    assert_eq!(b.ok(BOB, "agent.resolve", json!({"name": "role"}))["address"], "codex:t-alice@box");
    let err = b.call(BOB, "agent.register", json!({"name": "role"})).unwrap_err();
    assert!(err.1.contains("owner-pinned"), "{}", err.1);
    b.register(ALICE, "role"); // the pinned address itself may register
    let err = b.call(ALICE, "agent.unregister", json!({"name": "ROLE"})).unwrap_err();
    assert!(err.1.contains("refusing unregister"), "{}", err.1);
    b.ok(HUMAN, "agent.pin", json!({"name": "role", "address": null}));
    b.register(BOB, "role");
    assert_eq!(b.ok(BOB, "agent.whoami", json!({}))["name"], "krisz/role");
}

#[test]
fn unregister_compare_and_delete() {
    let b = Board::new();
    b.register(ALICE, "a");
    let err = b.call(HUMAN, "agent.unregister", json!({"name": "a", "if_address": BOB})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Conflict);
    let r = b.ok(HUMAN, "agent.unregister", json!({"name": "a", "if_address": ALICE}));
    assert_eq!(r["was_registered"], true);
    let r = b.ok(HUMAN, "agent.unregister", json!({"name": "a"}));
    assert_eq!(r["was_registered"], false);
    assert_eq!(b.call(ALICE, "agent.whoami", json!({})).unwrap_err().0, ErrorCode::Unauthenticated);
}

#[test]
fn from_override_is_for_the_local_human_only() {
    let b = Board::new();
    b.register(HUMAN, "krisz");
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let as_alice = b.request(&b.p, HUMAN, Some("alice"), "agent.whoami", json!({}), None).unwrap();
    assert_eq!(as_alice["name"], "krisz/alice");
    let err = b.request(&b.p, BOB, Some("alice"), "agent.whoami", json!({}), None).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    let remote = b.other("krisz", "box");
    let err = b.request(&remote, HUMAN, Some("alice"), "agent.whoami", json!({}), None).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
}

#[test]
fn owners_cannot_touch_each_others_names_or_dms() {
    let b = Board::new();
    let bob_owner = b.other("bob", "bobhost");
    b.register(ALICE, "dev");
    b.register(CAROL, "peer");
    let mid = b.send(CAROL, "dev", "private to krisz/dev");

    // another owner cannot register into krisz's namespace
    let err = b.call_as(&bob_owner, BOB, "agent.register", json!({"name": "krisz/dev"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    // nor claim an address on krisz's host
    let err = b.call_as(&bob_owner, "codex:t-alice@box", "agent.register", json!({"name": "dev"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    // their own "dev" is a different name
    let r = b.call_as(&bob_owner, BOB, "agent.register", json!({"name": "dev"})).unwrap();
    assert_eq!(r["name"], "bob/dev");
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/dev");
    // bob/dev can neither read nor fetch krisz's DM
    for op in ["message.get", "message.read"] {
        let err = b.call_as(&bob_owner, BOB, op, json!({"id": mid})).unwrap_err();
        assert_eq!(err.0, ErrorCode::NotFound, "{op}");
    }
    assert!(b.call_as(&bob_owner, BOB, "inbox.list", json!({})).unwrap()["items"].as_array().unwrap().is_empty());
    // cross-owner DMs work when addressed explicitly
    let r = b.call_as(&bob_owner, BOB, "message.send", json!({"to": {"agent": "krisz/dev"}, "text": "hi from bob"})).unwrap();
    assert_eq!(r["to"], "krisz/dev");
    let inbox = b.ok(ALICE, "inbox.list", json!({}));
    let froms: Vec<_> = inbox["items"].as_array().unwrap().iter().map(|i| i["from"].as_str().unwrap()).collect();
    assert_eq!(froms, ["krisz/peer", "bob/dev"]);
    // unqualified names resolve inside the caller's namespace
    let r = b.call_as(&bob_owner, BOB, "agent.resolve", json!({"name": "dev"})).unwrap();
    assert_eq!(r["address"], "codex:t-bob@bobhost");
}

#[test]
fn idempotent_mutations() {
    let b = Board::new();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let body = json!({"to": {"agent": "bob"}, "text": "once"});
    let first = b.request(&b.p, ALICE, None, "message.send", body.clone(), Some("k1")).unwrap();
    // response lost; the client retries with the same key
    let again = b.request(&b.p, ALICE, None, "message.send", body, Some("k1")).unwrap();
    assert_eq!(first, again);
    assert_eq!(b.message_count(), 1);
    let err = b
        .request(&b.p, ALICE, None, "message.send", json!({"to": {"agent": "bob"}, "text": "different"}), Some("k1"))
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::Conflict);
    let err = b.request(&b.p, ALICE, None, "message.send", json!({"to": {"agent": "bob"}, "text": "x"}), None).unwrap_err();
    assert_eq!(err.0, ErrorCode::BadRequest);
    // queries need no key
    assert!(b.request(&b.p, ALICE, None, "inbox.list", json!({}), None).is_ok());
}

#[test]
fn profiles_are_human_edited() {
    let b = Board::new();
    b.register(ALICE, "alice");
    let err = b.call(ALICE, "profile.set", json!({"name": "alice", "key": "Display", "value": "Al"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    b.ok(HUMAN, "profile.set", json!({"name": "alice", "key": "display", "value": "Alpha"}));
    assert_eq!(b.ok(BOB, "profile.get", json!({"name": "alice"}))["display"], "Alpha");
    assert_eq!(b.ok(BOB, "agent.list", json!({}))["agents"][0]["display"], "Alpha");
}

#[test]
fn agents_unregister_only_their_own_binding() {
    let b = Board::new();
    b.register(ALICE, "a");
    b.register(BOB, "b");
    let err = b.call(BOB, "agent.unregister", json!({"name": "a"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    assert!(err.1.contains("self-only"), "{}", err.1);
    let err = b.call(BOB, "agent.unregister", json!({"name": "a", "if_address": ALICE})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/a");
    // its own binding: allowed
    assert_eq!(b.ok(BOB, "agent.unregister", json!({"name": "b"}))["was_registered"], true);
    // the human owner may remove any unpinned name
    assert_eq!(b.ok(HUMAN, "agent.unregister", json!({"name": "a"}))["was_registered"], true);
    // a name in another owner's namespace is never reachable
    let other = b.other("bob", "otherhost");
    let err = b.call_as(&other, "human:bob", "agent.unregister", json!({"name": "krisz/a"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
}

#[test]
fn doctor_warns_about_the_fallback_host_id() {
    let b = Board::new();
    let fallback = b.other("solo", conde::util::FALLBACK_HOST);
    let r = b.call_as(&fallback, "codex:t-solo", "doctor", json!({})).unwrap();
    let text = r["checks"].to_string();
    assert!(text.contains("Set CONDE_HOST"), "{text}");
    assert!(r["issues"].as_i64().unwrap() >= 1);
    let r = b.ok(ALICE, "doctor", json!({}));
    assert!(!r["checks"].to_string().contains("CONDE_HOST"));
}

#[test]
fn host_id_keeps_existing_pinned_bindings_resolvable() {
    let d = std::sync::Arc::new(conde::dispatch::Dispatcher::open_in_memory().unwrap());
    let old = d.principal("krisz", "worker.example.com", true).unwrap();
    let b = Board { d: d.clone(), p: old };
    b.register(ALICE, "worker");
    b.ok(HUMAN, "agent.pin", json!({"name": "worker", "address": ALICE}));
    let host = conde::util::host_id("worker.example.com").unwrap();
    assert_eq!(host, "worker.example.com");
    let again = d.principal("krisz", &host, true).unwrap();
    assert_eq!(b.call_as(&again, ALICE, "agent.whoami", json!({})).unwrap()["name"], "krisz/worker");
    b.call_as(&again, ALICE, "agent.register", json!({"name": "worker"})).unwrap();
}

fn rows(v: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    v[key].as_array().cloned().unwrap_or_default()
}

#[test]
fn rename_keeps_the_agent_its_inbox_and_its_delivery() {
    let b = Board::new();
    b.register(ALICE, "old");
    b.register(BOB, "bob");
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(BOB, "channel.join", json!({"channel": "#dev"}));
    let read_dm = b.send(BOB, "old", "already read");
    b.ok(ALICE, "message.read", json!({"id": read_dm}));
    let sent_dm = b.send(ALICE, "bob", "sent before the rename");
    let pending = b.send(BOB, "old", "waiting for delivery");
    let mention = b.send(BOB, "#dev", "@old please look");
    let id = b.agent_id("old");

    let r = b.ok(ALICE, "agent.rename", json!({"new": "New"}));
    assert_eq!(r, json!({"name": "krisz/new", "old": "krisz/old", "address": "codex:t-alice@box"}));
    assert_eq!(b.agent_id("new"), id, "same row, same id");
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/new");
    assert_eq!(b.ok(BOB, "agent.resolve", json!({"name": "new"}))["address"], "codex:t-alice@box");

    // notifications queued for the old name reach the same session
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    let texts: Vec<String> = br.notifier().sent_to("codex:t-alice").iter().map(|s| s.1.clone()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.iter().any(|t| t.contains(&pending)), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains(&mention)), "{texts:?}");
    assert_eq!(b.status_of(&pending, "new"), "notified");
    assert_eq!(b.status_of(&mention, "new"), "notified");

    // read state, inbox, history and memberships stay
    let inbox: Vec<_> = rows(&b.ok(ALICE, "inbox.list", json!({})), "items").iter().map(|i| i["message_id"].clone()).collect();
    assert!(inbox.contains(&json!(pending)) && inbox.contains(&json!(mention)), "{inbox:?}");
    assert!(!inbox.contains(&json!(read_dm)), "{inbox:?}");
    assert_eq!(b.ok(ALICE, "message.read", json!({"id": read_dm}))["messages"][0]["text"], "already read");
    let receipts = rows(&b.ok(ALICE, "receipt.list", json!({})), "receipts");
    assert_eq!(receipts[0]["message_id"], json!(sent_dm));
    let members = rows(&b.ok(BOB, "channel.members", json!({"channel": "#dev"})), "members");
    assert!(members.iter().any(|m| m["name"] == "krisz/new"), "{members:?}");
    let to = b.ok(BOB, "message.send", json!({"to": {"agent": "new"}, "text": "to the new name"}));
    assert_eq!(to["to"], "krisz/new");
    assert!(!b.ok(BOB, "doctor", json!({}))["checks"].to_string().contains("unbound"));

    // the old name is free: unknown to senders, and a fresh row when registered again
    let err = b.call(BOB, "message.send", json!({"to": {"agent": "old"}, "text": "x"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::NotFound);
    let r = b.ok(CAROL, "agent.register", json!({"name": "old"}));
    assert_eq!(r["name"], "krisz/old");
    assert_ne!(b.agent_id("old"), id);
    assert!(rows(&b.ok(CAROL, "inbox.list", json!({})), "items").is_empty());
}

#[test]
fn rename_refuses_taken_pinned_invalid_and_foreign_names() {
    let b = Board::new();
    b.register(HUMAN, "krisz");
    b.register(ALICE, "a");
    b.register(BOB, "b");
    let refused = |actor: &str, body: serde_json::Value, code: ErrorCode, needle: &str| {
        let err = b.call(actor, "agent.rename", body.clone()).unwrap_err();
        assert_eq!(err.0, code, "{body}: {}", err.1);
        assert!(err.1.contains(needle), "{body}: {}", err.1);
    };
    // taken: bound to another session, or a human's
    refused(ALICE, json!({"new": "b"}), ErrorCode::Conflict, "registered to codex:t-bob@box");
    refused(ALICE, json!({"new": "krisz"}), ErrorCode::Conflict, "registered to human:krisz@box");
    // taken: pinned
    b.ok(HUMAN, "agent.pin", json!({"name": "role", "address": "claude:s-carol"}));
    refused(ALICE, json!({"new": "role"}), ErrorCode::Conflict, "owner-pinned");
    // taken: an earlier, unbound name keeps its own messages
    b.send(ALICE, "b", "for the earlier b");
    b.ok(BOB, "agent.unregister", json!({"name": "b"}));
    refused(ALICE, json!({"new": "b"}), ErrorCode::Conflict, "conde register b");
    // invalid or reserved
    refused(ALICE, json!({"new": "../x"}), ErrorCode::BadRequest, "invalid name");
    refused(ALICE, json!({"new": "everybody"}), ErrorCode::BadRequest, "reserved");
    // the caller's own name pinned
    refused(CAROL, json!({"new": "free"}), ErrorCode::Forbidden, "owner-pinned");
    refused(HUMAN, json!({"old": "role", "new": "free"}), ErrorCode::Forbidden, "owner-pinned");
    // another session's name, and another owner's namespace
    b.register(BOB, "b2");
    refused(BOB, json!({"old": "a", "new": "free"}), ErrorCode::Forbidden, "self-only");
    let other = b.other("bob", "otherhost");
    let err = b.call_as(&other, "human:bob", "agent.rename", json!({"old": "krisz/a", "new": "free"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden);
    let err = b.call_as(&other, "human:bob", "agent.rename", json!({"old": "a", "new": "free"})).unwrap_err();
    assert_eq!(err.0, ErrorCode::NotFound);
    // nothing moved
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/a");
    assert_eq!(b.ok(BOB, "agent.whoami", json!({}))["name"], "krisz/b2");
    assert_eq!(b.ok(CAROL, "agent.whoami", json!({}))["name"], "krisz/role");
}

#[test]
fn the_human_renames_any_unpinned_name() {
    let b = Board::new();
    b.register(HUMAN, "krisz");
    b.register(ALICE, "a");
    let r = b.ok(HUMAN, "agent.rename", json!({"old": "a", "new": "proj.impl"}));
    assert_eq!(r["name"], "krisz/proj.impl");
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/proj.impl");
    // an unbound name too, and the human's own name with one argument
    b.ok(ALICE, "agent.unregister", json!({"name": "proj.impl"}));
    let r = b.ok(HUMAN, "agent.rename", json!({"old": "proj.impl", "new": "retired"}));
    assert_eq!(r["address"], json!(null));
    assert_eq!(b.ok(HUMAN, "agent.rename", json!({"new": "boss"}))["name"], "krisz/boss");
    assert_eq!(b.ok(HUMAN, "agent.whoami", json!({}))["name"], "krisz/boss");
    // renaming to the current name changes nothing
    assert_eq!(b.ok(HUMAN, "agent.rename", json!({"new": "BOSS"}))["name"], "krisz/boss");
}

#[test]
fn rename_is_idempotent_by_key() {
    let b = Board::new();
    b.register(ALICE, "a");
    let first = b.request(&b.p, ALICE, None, "agent.rename", json!({"new": "b"}), Some("k1")).unwrap();
    let again = b.request(&b.p, ALICE, None, "agent.rename", json!({"new": "b"}), Some("k1")).unwrap();
    assert_eq!(first, again);
    assert_eq!(first["old"], "krisz/a");
    let err = b.request(&b.p, ALICE, None, "agent.rename", json!({"new": "c"}), Some("k1")).unwrap_err();
    assert_eq!(err.0, ErrorCode::Conflict);
    let err = b.request(&b.p, ALICE, None, "agent.rename", json!({"new": "c"}), None).unwrap_err();
    assert_eq!(err.0, ErrorCode::BadRequest);
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/b");
}

#[test]
fn unregister_then_register_still_leaves_the_old_name_waiting() {
    let b = Board::new();
    b.register(ALICE, "old");
    b.register(BOB, "bob");
    b.ok(ALICE, "agent.unregister", json!({"name": "old"}));
    b.register(ALICE, "new");
    assert_eq!(b.ok(ALICE, "agent.whoami", json!({}))["name"], "krisz/new");
    // a send to the old name is stored and waits for a session to hold it
    let mid = b.send(BOB, "old", "stranded");
    assert_eq!(b.status_of(&mid, "old"), "pending");
    let checks = b.ok(BOB, "doctor", json!({}))["checks"].to_string();
    assert!(checks.contains("unbound: krisz/old has 1 notification(s) waiting"), "{checks}");
    assert!(checks.contains("use conde rename"), "{checks}");
}

#[test]
fn doctor_reports_only_the_callers_own_namespace() {
    let b = Board::new();
    let ben = b.other("ben", "benhost");
    b.register(ALICE, "alice");
    // ben: a live name whose recipient holds one message and refuses another, and an unbound name with two waiting
    for (actor, name) in [("codex:t-live", "live"), ("codex:t-gone", "gone")] {
        b.call_as(&ben, actor, "agent.register", json!({"name": name})).unwrap();
    }
    b.call_as(&ben, "codex:t-gone", "agent.unregister", json!({"name": "gone"})).unwrap();
    b.send(ALICE, "ben/gone", "stranded 1");
    b.send(ALICE, "ben/gone", "stranded 2");
    let mut br = conde::bridge::Bridge::new(
        b.d.clone(),
        b.d.bridge_principal(&ben),
        FakeNotifier { outcomes: [NotifyOutcome::Accepted, NotifyOutcome::Held].into(), ..Default::default() },
    );
    let refused = b.send(ALICE, "ben/live", "refused");
    br.pass().unwrap();
    let attempt = br.notifier().last_attempt();
    br.notifier().receipt(&attempt, ReceiptStatus::Refused);
    br.pass().unwrap();
    assert_eq!(b.status_of(&refused, "live"), "refused");
    let held = b.send(ALICE, "ben/live", "held");
    br.pass().unwrap();
    assert_eq!(b.status_of(&held, "live"), "held");
    // krisz: an unbound name of its own with one waiting
    b.register(BOB, "mine");
    b.ok(BOB, "agent.unregister", json!({"name": "mine"}));
    b.send(ALICE, "mine", "stranded at home");

    let r = b.ok(ALICE, "doctor", json!({}));
    let checks = r["checks"].to_string();
    assert!(checks.contains("unbound: krisz/mine has 1 notification(s) waiting"), "{checks}");
    assert!(!checks.contains("ben/"), "{checks}");
    assert!(checks.contains("held: 0 notification(s)"), "{checks}");
    assert!(checks.contains("refused: 0 notification(s)"), "{checks}");
    assert_eq!(r["issues"], 1, "{checks}");

    let r = b.call_as(&ben, "codex:t-live", "doctor", json!({})).unwrap();
    let checks = r["checks"].to_string();
    assert!(checks.contains("unbound: ben/gone has 2 notification(s) waiting"), "{checks}");
    assert!(!checks.contains("krisz/"), "{checks}");
    assert!(checks.contains("held: 1 notification(s)"), "{checks}");
    assert!(checks.contains("refused: 1 notification(s)"), "{checks}");
    assert_eq!(r["issues"], 2, "{checks}");
}
