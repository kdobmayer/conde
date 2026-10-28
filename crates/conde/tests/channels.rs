//! Channel topics and the `#lobby` auto-join, through the dispatcher.

mod common;

use common::*;
use conde::protocol::ErrorCode;
use serde_json::{Value, json};

fn topics(b: &Board) -> Vec<(String, Value)> {
    let r = b.ok(ALICE, "channel.list", json!({}));
    r["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["channel"].as_str().unwrap().to_string(),
                c["topic"].clone(),
            )
        })
        .collect()
}

#[test]
fn topics_are_set_by_the_creator_or_a_human() {
    let b = Board::new();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.register(HUMAN, "krisz");
    let r = b.ok(
        ALICE,
        "channel.create",
        json!({"channel": "#dev", "topic": "  release planning  "}),
    );
    assert_eq!(r["topic"], "release planning", "trimmed");
    b.ok(ALICE, "channel.create", json!({"channel": "#ops"}));
    assert_eq!(
        topics(&b),
        vec![
            ("#dev".into(), json!("release planning")),
            ("#ops".into(), Value::Null)
        ]
    );

    let joined = b.ok(BOB, "channel.join", json!({"channel": "#dev"}));
    assert_eq!(
        joined["topic"], "release planning",
        "the join result carries the topic"
    );
    let again = b.ok(BOB, "channel.join", json!({"channel": "#dev"}));
    assert_eq!(
        (again["already_member"].clone(), again["topic"].clone()),
        (json!(true), json!("release planning"))
    );

    // the creator and a human may; another agent, member or not, may not
    let err = b
        .call(
            BOB,
            "channel.topic",
            json!({"channel": "#dev", "topic": "mine now"}),
        )
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::Forbidden, "{err:?}");
    b.ok(
        ALICE,
        "channel.topic",
        json!({"channel": "#dev", "topic": "v2 planning"}),
    );
    b.ok(
        HUMAN,
        "channel.topic",
        json!({"channel": "#ops", "topic": "pager rota"}),
    );
    assert_eq!(
        topics(&b),
        vec![
            ("#dev".into(), json!("v2 planning")),
            ("#ops".into(), json!("pager rota"))
        ]
    );
    let cleared = b.ok(
        HUMAN,
        "channel.topic",
        json!({"channel": "#dev", "topic": " "}),
    );
    assert_eq!(cleared["topic"], Value::Null, "empty clears");

    // limits: 256 characters, one line
    let long = "é".repeat(257);
    let err = b
        .call(
            ALICE,
            "channel.topic",
            json!({"channel": "#dev", "topic": long}),
        )
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::BadRequest);
    b.ok(
        ALICE,
        "channel.topic",
        json!({"channel": "#dev", "topic": "é".repeat(256)}),
    );
    let err = b
        .call(
            ALICE,
            "channel.create",
            json!({"channel": "#x", "topic": "two\nlines"}),
        )
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::BadRequest);
    assert!(
        topics(&b).iter().all(|(c, _)| c != "#x"),
        "a refused create creates nothing"
    );
    let err = b
        .call(
            ALICE,
            "channel.topic",
            json!({"channel": "#nope", "topic": "x"}),
        )
        .unwrap_err();
    assert_eq!(err.0, ErrorCode::NotFound);
}

#[test]
fn a_new_name_joins_the_lobby_once_silently() {
    let b = Board::new();
    // no lobby: registration works and joins nothing
    let r = b.ok(ALICE, "agent.register", json!({"name": "alice"}));
    assert_eq!(r["joined"], Value::Null);
    b.ok(ALICE, "channel.create", json!({"channel": "#lobby"}));
    b.send(ALICE, "#lobby", "history before bob");
    let before = b.notifications().len();

    let r = b.ok(BOB, "agent.register", json!({"name": "bob"}));
    assert_eq!(r["joined"], "#lobby");
    let members = b.ok(ALICE, "channel.members", json!({"channel": "#lobby"}));
    assert_eq!(
        members["members"][1],
        json!({"name": "krisz/bob", "behind": 0}),
        "a member from the end of the log"
    );
    assert_eq!(
        b.ok(BOB, "inbox.list", json!({}))["items"],
        json!([]),
        "history is not unread"
    );
    assert_eq!(b.notifications().len(), before, "no notification");
    assert_eq!(
        b.ok(BOB, "message.read", json!({"channel": "#lobby"}))["messages"],
        json!([]),
        "no join post"
    );

    // re-registering or moving the name never re-joins
    b.ok(BOB, "channel.leave", json!({"channel": "#lobby"}));
    let r = b.ok(BOB, "agent.register", json!({"name": "bob"}));
    assert_eq!(r["joined"], Value::Null, "re-register");
    let r = b.ok(CAROL, "agent.register", json!({"name": "bob"}));
    assert_eq!(r["joined"], Value::Null, "rebinding to another session");
    let members = b.ok(ALICE, "channel.members", json!({"channel": "#lobby"}));
    assert_eq!(members["members"].as_array().unwrap().len(), 1);

    // a name the owner pinned first is still new at its first registration
    b.ok(HUMAN, "agent.register", json!({"name": "krisz"}));
    b.ok(HUMAN, "agent.pin", json!({"name": "reviewer"}));
    let r = b.ok("codex:t-rev", "agent.register", json!({"name": "reviewer"}));
    assert_eq!(r["joined"], "#lobby");
}
