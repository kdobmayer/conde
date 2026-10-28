//! JSON round trips of the v2 bridge frames and enums (DESIGN-v2.md §4, §5), and the v1
//! frames parsing as before.

use conde_remote::{
    Badge, DEFAULT_WINDOW, Endpoint, Hello, MAX_WINDOW, Notify, NotifyFields, NotifyKind, Outcome,
    OutcomeReason, OutcomeStatus, PROTO_V1, PROTO_V2, Route, Routes,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// `value` serializes to exactly `wire`, and `wire` parses back to `value`.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    value: &T,
    wire: Value,
) {
    assert_eq!(serde_json::to_value(value).unwrap(), wire);
    assert_eq!(&serde_json::from_value::<T>(wire).unwrap(), value);
}

fn mailbox(id: &str) -> Endpoint {
    Endpoint::Mailbox {
        mailbox_id: id.into(),
    }
}

#[test]
fn mailbox_endpoint_json_shape() {
    round_trip(
        &mailbox("01MBX"),
        json!({"kind": "mailbox", "mailbox_id": "01MBX"}),
    );
    round_trip(
        &Endpoint::Claude {
            session_id: "s1".into(),
        },
        json!({"kind": "claude", "session_id": "s1"}),
    );
    assert!(serde_json::from_value::<Endpoint>(json!({"kind": "mailbox"})).is_err());
}

#[test]
fn v1_hello_is_unchanged() {
    let v1 = Hello {
        host_id: "box".into(),
        bridge_instance: "i1".into(),
        endpoints: vec![Endpoint::Codex {
            thread_id: "t1".into(),
        }],
        proto: PROTO_V1,
        window: None,
    };
    let wire = json!({"host_id": "box", "bridge_instance": "i1", "endpoints": [{"kind": "codex", "thread_id": "t1"}]});
    round_trip(&v1, wire);
    // A v1 frame as sent today, version and type included, with and without endpoints.
    let parsed: Hello = serde_json::from_value(
        json!({"v": 1, "type": "hello", "host_id": "box", "bridge_instance": "i1"}),
    )
    .unwrap();
    assert_eq!(
        (parsed.proto, parsed.window, parsed.endpoints.len()),
        (PROTO_V1, None, 0)
    );
    assert_eq!(parsed.check(), Ok(()));
    assert_eq!(v1.check(), Ok(()));
}

#[test]
fn a_hello_as_the_slack_instance_is_refused() {
    for instance in ["slack", "Slack"] {
        let hello: Hello = serde_json::from_value(
            json!({"v": 1, "type": "hello", "host_id": "box", "bridge_instance": instance}),
        )
        .unwrap();
        assert!(
            hello.check().unwrap_err().contains("reserved"),
            "{instance}"
        );
    }
}

#[test]
fn v2_hello_round_trip_and_shape() {
    let v2 = Hello {
        host_id: "box".into(),
        bridge_instance: "i2".into(),
        endpoints: vec![mailbox("01MBX")],
        proto: PROTO_V2,
        window: Some(DEFAULT_WINDOW),
    };
    round_trip(
        &v2,
        json!({"host_id": "box", "bridge_instance": "i2", "endpoints": [{"kind": "mailbox", "mailbox_id": "01MBX"}],
               "proto": 2, "window": 32}),
    );
    assert_eq!(v2.check(), Ok(()));
    assert_eq!(
        Hello {
            endpoints: vec![],
            ..v2.clone()
        }
        .check(),
        Ok(())
    );
    assert_eq!(
        Hello {
            window: Some(MAX_WINDOW),
            ..v2.clone()
        }
        .check(),
        Ok(())
    );

    let refused = [
        Hello {
            window: None,
            ..v2.clone()
        },
        Hello {
            window: Some(0),
            ..v2.clone()
        },
        Hello {
            window: Some(MAX_WINDOW + 1),
            ..v2.clone()
        },
        Hello {
            endpoints: vec![
                mailbox("01MBX"),
                Endpoint::Claude {
                    session_id: "s1".into(),
                },
            ],
            ..v2.clone()
        },
        Hello {
            proto: PROTO_V1,
            ..v2.clone()
        },
        Hello {
            proto: PROTO_V1,
            window: None,
            ..v2.clone()
        },
        Hello {
            proto: 3,
            ..v2.clone()
        },
    ];
    for h in refused {
        assert!(h.check().is_err(), "{h:?}");
    }
}

fn fields() -> NotifyFields {
    NotifyFields {
        kind: NotifyKind::Mention,
        sender: "balazs/relay".into(),
        place: "#dev".into(),
        badges: vec![Badge {
            place: "#ops".into(),
            unread: 3,
        }],
        member: false,
    }
}

#[test]
fn v1_notify_parses_with_no_structured_fields() {
    let wire = json!({"notification_id": "n1", "attempt_id": "a1", "recipient": "u/r", "binding_version": 3,
                      "endpoint": {"kind": "claude", "session_id": "s1"}, "message_id": "m1",
                      "text": "conde: new message m1 from u/x. Read it with: conde read m1"});
    let n: Notify = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(n.structured(), None);
    assert_eq!(serde_json::to_value(&n).unwrap(), wire);
    // An attempt stored without text still sends the key a v1 client requires.
    let empty = Notify {
        text: String::new(),
        ..n
    };
    assert_eq!(serde_json::to_value(&empty).unwrap()["text"], json!(""));
}

#[test]
fn v2_notify_round_trip() {
    let n = Notify {
        notification_id: "n1".into(),
        attempt_id: "a1".into(),
        recipient: "u/r".into(),
        binding_version: 7,
        endpoint: mailbox("01MBX"),
        message_id: "m1".into(),
        text: String::new(),
        fields: Some(fields()),
    };
    round_trip(
        &n,
        json!({"notification_id": "n1", "attempt_id": "a1", "recipient": "u/r", "binding_version": 7,
               "endpoint": {"kind": "mailbox", "mailbox_id": "01MBX"}, "message_id": "m1", "text": "",
               "kind": "mention", "sender": "balazs/relay", "place": "#dev",
               "badges": [{"place": "#ops", "unread": 3}], "member": false}),
    );
    let mut without_text = serde_json::to_value(&n).unwrap();
    without_text.as_object_mut().unwrap().remove("text");
    assert_eq!(serde_json::from_value::<Notify>(without_text).unwrap(), n);
    assert_eq!(n.structured(), Some(&fields()));
    let dm = NotifyFields {
        kind: NotifyKind::Dm,
        place: "dm".into(),
        badges: vec![],
        member: true,
        ..fields()
    };
    round_trip(
        &dm,
        json!({"kind": "dm", "sender": "balazs/relay", "place": "dm", "badges": [], "member": true}),
    );
    for kind in [NotifyKind::Dm, NotifyKind::Mention, NotifyKind::Post] {
        let s = serde_json::to_value(kind).unwrap();
        assert_eq!(serde_json::from_value::<NotifyKind>(s).unwrap(), kind);
    }
    assert_eq!(serde_json::to_value(NotifyKind::Dm).unwrap(), json!("dm"));
}

#[test]
fn a_notify_missing_any_structured_field_has_none() {
    let full = json!({"notification_id": "n1", "attempt_id": "a1", "recipient": "u/r", "binding_version": 7,
                      "endpoint": {"kind": "mailbox", "mailbox_id": "01MBX"}, "message_id": "m1",
                      "kind": "dm", "sender": "u/x", "place": "dm", "badges": [], "member": true});
    assert!(
        serde_json::from_value::<Notify>(full.clone())
            .unwrap()
            .structured()
            .is_some()
    );
    for missing in ["kind", "sender", "place", "badges", "member"] {
        let mut partial = full.clone();
        partial.as_object_mut().unwrap().remove(missing);
        let n: Notify = serde_json::from_value(partial).unwrap();
        assert_eq!(n.structured(), None, "without {missing}");
    }
    let mut not_bool = full.clone();
    not_bool["member"] = json!("yes");
    assert_eq!(
        serde_json::from_value::<Notify>(not_bool)
            .unwrap()
            .structured(),
        None
    );
    let mut unknown_kind = full.clone();
    unknown_kind["kind"] = json!("reaction");
    assert_eq!(
        serde_json::from_value::<Notify>(unknown_kind)
            .unwrap()
            .structured(),
        None
    );
}

#[test]
fn routes_frame_round_trip() {
    let routes = Routes {
        mailboxes: vec![
            Route {
                mailbox_id: "01A".into(),
                route_generation: 4,
            },
            Route {
                mailbox_id: "01B".into(),
                route_generation: 1,
            },
        ],
        revision: None,
    };
    round_trip(
        &routes,
        json!({"mailboxes": [{"mailbox_id": "01A", "route_generation": 4}, {"mailbox_id": "01B", "route_generation": 1}]}),
    );
    round_trip(&Routes::default(), json!({"mailboxes": []}));
}

#[test]
fn outcome_statuses_are_the_v1_strings() {
    let wire = [
        "accepted",
        "held",
        "delivered",
        "expired",
        "denied",
        "refused",
        "dropped",
        "failed",
        "abandoned",
    ];
    assert_eq!(OutcomeStatus::ALL.map(OutcomeStatus::as_str), wire);
    for s in OutcomeStatus::ALL {
        round_trip(&s, json!(s.as_str()));
        assert_eq!(OutcomeStatus::parse(s.as_str()), Some(s));
    }
    let open: Vec<_> = OutcomeStatus::ALL
        .into_iter()
        .filter(|s| !s.is_terminal())
        .collect();
    assert_eq!(open, [OutcomeStatus::Accepted, OutcomeStatus::Held]);
    assert_eq!(OutcomeStatus::parse("gone"), None);
}

#[test]
fn outcome_reasons_are_a_closed_set() {
    let wire = [
        "recipient unavailable",
        "recipient offline",
        "recipient session replaced",
        "transport error",
        "duplicate",
        "outcome too large",
    ];
    assert_eq!(OutcomeReason::ALL.map(OutcomeReason::as_str), wire);
    for r in OutcomeReason::ALL {
        round_trip(&r, json!(r.as_str()));
        assert_eq!(OutcomeReason::parse(r.as_str()), Some(r));
    }
    assert!(serde_json::from_value::<OutcomeReason>(json!("no live Claude session 5f2c")).is_err());
    assert_eq!(OutcomeReason::parse("Recipient offline"), None);
}

#[test]
fn v2_outcome_check_refuses_unknown_reasons_and_statuses() {
    let outcome = |status: &str, reason: Option<&str>| Outcome {
        attempt_id: "a1".into(),
        event_seq: 1,
        status: status.into(),
        reason: reason.map(Into::into),
    };
    assert_eq!(
        outcome("refused", Some("recipient unavailable")).check_v2(),
        Ok((
            OutcomeStatus::Refused,
            Some(OutcomeReason::RecipientUnavailable)
        ))
    );
    assert_eq!(
        outcome("delivered", None).check_v2(),
        Ok((OutcomeStatus::Delivered, None))
    );
    assert!(
        outcome("failed", Some("cannot deliver to /run/user/1000/cc.sock"))
            .check_v2()
            .is_err()
    );
    assert!(outcome("gone", None).check_v2().is_err());

    // The v1 frame keeps its free-text reason.
    let v1 = json!({"attempt_id": "a1", "event_seq": 2, "status": "failed", "reason": "no live Claude session 5f2c"});
    round_trip(
        &Outcome {
            event_seq: 2,
            ..outcome("failed", Some("no live Claude session 5f2c"))
        },
        v1,
    );
}
