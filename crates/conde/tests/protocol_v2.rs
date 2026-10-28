//! JSON round trips of the v2 op bodies (DESIGN-v2.md §6, §9), the board labels of daemon
//! responses (§3), and the local operation records' request frames (§4, §5).

use conde::protocol::v2::{
    BOARD_FIELD, BoardInfo, BoardPart, Evidence, EvidenceSource, LOCAL_BOARD, Manifest,
    ManifestRow, MigrateBody, MigratePreview, Mine, MineMailbox, PreviewRow, PublishBody,
    Published, UnpublishBody, Unpublished, Via, label, op,
};
use conde::store::local::{BoardOp, BoardOpKind, BoardOpState, PublicationState};
use conde_remote::Route;
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

#[test]
fn op_names() {
    assert_eq!(
        [
            op::BOARD_INFO,
            op::MAILBOX_PUBLISH,
            op::MAILBOX_UNPUBLISH,
            op::MAILBOX_MINE,
            op::HOST_MIGRATE_PREVIEW,
            op::HOST_MIGRATE_V2
        ],
        [
            "board.info",
            "mailbox.publish",
            "mailbox.unpublish",
            "mailbox.mine",
            "host.migrate_preview",
            "host.migrate_v2"
        ]
    );
}

#[test]
fn board_info_round_trip() {
    let info = BoardInfo {
        name: "dakai".into(),
        owner: "kdobmayer".into(),
        host: "homepc".into(),
        mailbox_only: true,
    };
    round_trip(
        &info,
        json!({"name": "dakai", "owner": "kdobmayer", "host": "homepc", "mailbox_only": true}),
    );
}

#[test]
fn publish_round_trip_with_optional_flags() {
    round_trip(
        &PublishBody {
            name: "worker".into(),
            human: false,
            take: false,
        },
        json!({"name": "worker"}),
    );
    round_trip(
        &PublishBody {
            name: "krisz".into(),
            human: true,
            take: true,
        },
        json!({"name": "krisz", "human": true, "take": true}),
    );
    round_trip(
        &Published {
            name: "worker".into(),
            mailbox_id: "01MBX".into(),
            route_generation: 2,
            revision: None,
        },
        json!({"name": "worker", "mailbox_id": "01MBX", "route_generation": 2}),
    );
    assert!(
        serde_json::from_value::<PublishBody>(json!({"take": true})).is_err(),
        "name is required"
    );
}

#[test]
fn unpublish_carries_the_expected_generation() {
    round_trip(
        &UnpublishBody {
            mailbox_id: "01MBX".into(),
            expected_generation: 2,
        },
        json!({"mailbox_id": "01MBX", "expected_generation": 2}),
    );
    assert!(serde_json::from_value::<UnpublishBody>(json!({"mailbox_id": "01MBX"})).is_err());
    round_trip(
        &Unpublished {
            mailbox_id: "01MBX".into(),
            route_generation: 3,
            revision: None,
        },
        json!({"mailbox_id": "01MBX", "route_generation": 3}),
    );
}

#[test]
fn mine_round_trip() {
    let mine = Mine {
        mailboxes: vec![MineMailbox {
            name: "desk".into(),
            mailbox_id: "01MBX".into(),
            route_generation: 5,
        }],
        revision: None,
    };
    round_trip(
        &mine,
        json!({"mailboxes": [{"name": "desk", "mailbox_id": "01MBX", "route_generation": 5}]}),
    );
    let revised = Mine {
        revision: Some(7),
        ..mine
    };
    round_trip(
        &revised,
        json!({"mailboxes": [{"name": "desk", "mailbox_id": "01MBX", "route_generation": 5}], "revision": 7}),
    );
}

#[test]
fn migrate_preview_round_trip() {
    let preview = MigratePreview {
        rows: vec![PreviewRow {
            agent_id: "01AG".into(),
            name: "desk".into(),
            address: "claude:5f2c@homepc".into(),
            pin: Some("claude:5f2c@homepc".into()),
            candidate: true,
            evidence: vec![
                Evidence {
                    via: Via::Dm,
                    with: "balazs/relay".into(),
                    source: EvidenceSource::Later,
                    messages: 4,
                    last_at: "2026-10-09T10:00:00Z".into(),
                },
                Evidence {
                    via: Via::Channel,
                    with: "#dev".into(),
                    source: EvidenceSource::Imported,
                    messages: 1,
                    last_at: "2026-10-01T08:00:00Z".into(),
                },
            ],
        }],
    };
    round_trip(
        &preview,
        json!({"rows": [{"agent_id": "01AG", "name": "desk", "address": "claude:5f2c@homepc",
                         "pin": "claude:5f2c@homepc", "candidate": true, "evidence": [
            {"via": "dm", "with": "balazs/relay", "source": "later", "messages": 4, "last_at": "2026-10-09T10:00:00Z"},
            {"via": "channel", "with": "#dev", "source": "imported", "messages": 1, "last_at": "2026-10-01T08:00:00Z"}]}]}),
    );
}

#[test]
fn migrate_body_and_manifest_round_trip() {
    round_trip(
        &MigrateBody {
            publish: vec!["01AG".into()],
        },
        json!({"publish": ["01AG"]}),
    );
    let manifest = Manifest {
        bindings: vec![
            ManifestRow {
                agent_id: "01AG".into(),
                name: "desk".into(),
                address: "claude:5f2c@homepc".into(),
                pin: None,
                published: Some(Route {
                    mailbox_id: "01AG".into(),
                    route_generation: 9,
                }),
            },
            ManifestRow {
                agent_id: "01OT".into(),
                name: "lane.v0".into(),
                address: "codex:th-7@homepc".into(),
                pin: Some("codex:th-7@homepc".into()),
                published: None,
            },
        ],
    };
    round_trip(
        &manifest,
        json!({"bindings": [
            {"agent_id": "01AG", "name": "desk", "address": "claude:5f2c@homepc", "pin": null,
             "published": {"mailbox_id": "01AG", "route_generation": 9}},
            {"agent_id": "01OT", "name": "lane.v0", "address": "codex:th-7@homepc", "pin": "codex:th-7@homepc",
             "published": null}]}),
    );
}

#[test]
fn response_rows_carry_their_board() {
    let mut row = json!({"id": "01M4", "from": "reviewer"});
    label(&mut row, LOCAL_BOARD);
    assert_eq!(
        row,
        json!({"id": "01M4", "from": "reviewer", "board": "local"})
    );
    label(&mut row, "dakai");
    assert_eq!(row[BOARD_FIELD], "dakai");
    round_trip(
        &BoardPart {
            board: "dakai".into(),
            available: false,
        },
        json!({"board": "dakai", "available": false}),
    );
}

#[test]
fn local_record_states_match_the_schema() {
    assert_eq!(
        PublicationState::ALL.map(PublicationState::as_str),
        ["publishing", "published", "unpublishing"]
    );
    assert_eq!(
        BoardOpKind::ALL.map(BoardOpKind::as_str),
        ["publish", "unpublish", "migrate"]
    );
    assert_eq!(
        BoardOpState::ALL.map(BoardOpState::as_str),
        ["pending", "done", "failed"]
    );
    for s in PublicationState::ALL {
        assert_eq!(PublicationState::parse(s.as_str()), Some(s));
    }
    for k in BoardOpKind::ALL {
        assert_eq!(BoardOpKind::parse(k.as_str()), Some(k));
    }
    for s in BoardOpState::ALL {
        assert_eq!(BoardOpState::parse(s.as_str()), Some(s));
    }
    assert_eq!(PublicationState::parse("gone"), None);
}

#[test]
fn a_board_op_is_sent_with_its_op_id_as_idempotency_key() {
    let body = serde_json::to_value(UnpublishBody {
        mailbox_id: "01MBX".into(),
        expected_generation: 2,
    })
    .unwrap();
    let rec = BoardOp {
        op_id: "01OPID".into(),
        board: "dakai".into(),
        kind: BoardOpKind::Unpublish,
        agent_id: Some("01AG".into()),
        request: body.clone(),
        state: BoardOpState::Pending,
        result: None,
        created_at: "t0".into(),
        updated_at: "t0".into(),
    };
    let first = serde_json::to_value(rec.request("r1")).unwrap();
    assert_eq!(
        first,
        json!({"v": 1, "type": "request", "request_id": "r1", "op": "mailbox.unpublish",
               "idempotency_key": "01OPID", "body": body})
    );
    // A resumed op differs in its request id only.
    let mut again = serde_json::to_value(rec.request("r2")).unwrap();
    again["request_id"] = json!("r1");
    assert_eq!(again, first);
    let ops = [
        BoardOpKind::Publish,
        BoardOpKind::Unpublish,
        BoardOpKind::Migrate,
    ]
    .map(BoardOpKind::op_name);
    assert_eq!(
        ops,
        [
            op::MAILBOX_PUBLISH,
            op::MAILBOX_UNPUBLISH,
            op::HOST_MIGRATE_V2
        ]
    );
}
