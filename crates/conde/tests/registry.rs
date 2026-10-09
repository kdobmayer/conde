mod common;

use common::*;
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
