//! The resolver (DESIGN-v2.md §3): picks exactly one board per request, or the boards of an
//! aggregate view, from `--board`, a qualified message reference (`dakai:01M...`), and the
//! host's default board. A bare reference is always local.

use serde_json::Value;

use crate::boards::is_alias;

fn valid_alias(a: &str) -> bool {
    is_alias(a) && a != LOCAL_BOARD
}
use crate::protocol::v2::LOCAL_BOARD;
use crate::protocol::{ErrorCode, Request};

/// Where a request goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    Board(String),
    /// The local board and each connected board where the caller is published, merged.
    Aggregate,
}

pub type Refusal = (ErrorCode, String);

/// Ops of the local registry: the default board never applies, a shared `--board` is refused.
const LOCAL_ONLY: &[(&str, &str)] = &[
    ("agent.register", "register"),
    ("agent.unregister", "unregister"),
    ("agent.rename", "rename"),
    ("agent.pin", "pin"),
];

/// Ops that never leave the machine (the web's owner view, the bridge's own ops).
fn never_shared(op: &str) -> bool {
    op.starts_with("owner.") || op.starts_with("bridge.") || op == "fleet.list"
}

/// The body field holding a message reference, for the ops that take one.
fn reference_field(op: &str, body: &Value) -> Option<&'static str> {
    match op {
        "message.get" => Some("id"),
        "message.read" if body.get("id").is_some_and(|v| !v.is_null()) => Some("id"),
        "reaction.set" => Some("message_id"),
        "attachment.get" => Some("attachment_id"),
        "message.send" if body.get("reply_to").is_some_and(|v| !v.is_null()) => Some("reply_to"),
        _ => None,
    }
}

fn aggregate(op: &str, body: &Value) -> bool {
    match op {
        "inbox.list" | "receipt.list" | "agent.whoami" | "doctor" => true,
        "message.read" => ["id", "channel"]
            .iter()
            .all(|k| body.get(*k).is_none_or(Value::is_null)),
        _ => false,
    }
}

/// `dakai:01M...` -> (Some("dakai"), "01M..."); a bare id -> (None, id). `local:` is local.
pub fn split_ref(reference: &str) -> (Option<&str>, &str) {
    match reference.split_once(':') {
        Some((board, id)) if !id.is_empty() && (board == LOCAL_BOARD || valid_alias(board)) => (
            if board == LOCAL_BOARD {
                None
            } else {
                Some(board)
            },
            id,
        ),
        _ => (None, reference),
    }
}

fn unknown_board(alias: &str, enrolled: &[String]) -> Refusal {
    (
        ErrorCode::NotFound,
        crate::surface::unknown_board(alias, enrolled),
    )
}

/// Resolves `req` and returns the target with the request as the board must see it: `board`
/// dropped, and its message reference unqualified.
pub fn resolve(
    req: &Request,
    default_board: &str,
    enrolled: &[String],
) -> Result<(Target, Request), Refusal> {
    let known = |b: &str| enrolled.iter().any(|e| e == b);
    let mut out = req.clone();
    out.board = None;
    let explicit = req
        .board
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());
    if let Some(b) = explicit
        && b != LOCAL_BOARD
        && !known(b)
    {
        return Err(unknown_board(b, enrolled));
    }
    if let Some((_, verb)) = LOCAL_ONLY.iter().find(|(op, _)| *op == req.op) {
        return match explicit {
            Some(b) if b != LOCAL_BOARD => Err((
                ErrorCode::BadRequest,
                format!("{verb} is local; a shared address is conde publish <name> --board {b}"),
            )),
            _ => Ok((Target::Local, out)),
        };
    }
    if never_shared(&req.op) {
        return match explicit {
            Some(b) if b != LOCAL_BOARD => {
                Err((ErrorCode::BadRequest, format!("{} is local only", req.op)))
            }
            _ => Ok((Target::Local, out)),
        };
    }
    if let Some(field) = reference_field(&req.op, &req.body) {
        let raw = req
            .body
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (board, id) = split_ref(&raw);
        let target = match (board, explicit) {
            (None, Some(b)) if b != LOCAL_BOARD => {
                return Err((
                    ErrorCode::BadRequest,
                    format!("{id} is a local message; drop --board, or use the {b}: id"),
                ));
            }
            (Some(q), Some(b)) if b != q => {
                return Err((
                    ErrorCode::BadRequest,
                    format!("{raw} is a {q} message; drop --board, or use --board {q}"),
                ));
            }
            (Some(q), _) if !known(q) => return Err(unknown_board(q, enrolled)),
            (Some(q), _) => Target::Board(q.to_string()),
            (None, _) => Target::Local,
        };
        if let Some(obj) = out.body.as_object_mut() {
            obj.insert(field.into(), Value::String(id.to_string()));
        }
        return Ok((target, out));
    }
    if aggregate(&req.op, &req.body) {
        return Ok((
            match explicit {
                None => Target::Aggregate,
                Some(LOCAL_BOARD) => Target::Local,
                Some(b) => Target::Board(b.to_string()),
            },
            out,
        ));
    }
    // Paging reaches a human on a shared board: with a local default, the one enrolled board.
    let board = match (explicit, enrolled) {
        (None, [only]) if default_board == LOCAL_BOARD && req.op.starts_with("human.") => {
            only.as_str()
        }
        _ => explicit.unwrap_or(default_board),
    };
    if board == LOCAL_BOARD {
        return Ok((Target::Local, out));
    }
    if !known(board) {
        // Never another destination than the one configured.
        return Err((
            ErrorCode::NotFound,
            format!(
                "the default board '{board}' is not enrolled on this host; name the board with --board, \
                 or set the default with conde setup ({})",
                crate::surface::unknown_board(board, enrolled)
            ),
        ));
    }
    Ok((Target::Board(board.to_string()), out))
}

/// Keys whose string values are board-local ids, printed qualified for a shared board.
const ID_KEYS: &[&str] = &["message_id", "reply_to", "attachment_id"];

/// Qualifies every board-local id in a shared board's result with its alias.
pub fn qualify(alias: &str, v: &mut Value) {
    match v {
        Value::Object(obj) => {
            for (k, val) in obj.iter_mut() {
                match val {
                    Value::String(s) if ID_KEYS.contains(&k.as_str()) && !s.contains(':') => {
                        *s = format!("{alias}:{s}");
                    }
                    _ => qualify(alias, val),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| qualify(alias, i)),
        _ => {}
    }
}

/// The time order of a row: the ULID of its message id, qualified or not.
pub fn order_key(row: &Value) -> String {
    let id = row
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    split_ref(id).1.to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn req(op: &str, body: Value, board: Option<&str>) -> Request {
        let mut r = Request::new("r", op, body);
        r.board = board.map(str::to_string);
        r
    }

    fn target(
        op: &str,
        body: Value,
        board: Option<&str>,
        default: &str,
    ) -> Result<Target, Refusal> {
        resolve(&req(op, body, board), default, &["dakai".to_string()]).map(|(t, _)| t)
    }

    #[test]
    fn names_follow_board_then_default_then_local() {
        let send = json!({"to": {"agent": "reviewer"}, "text": "x"});
        assert_eq!(
            target("message.send", send.clone(), None, "local"),
            Ok(Target::Local)
        );
        assert_eq!(
            target("message.send", send.clone(), Some("dakai"), "local"),
            Ok(Target::Board("dakai".into()))
        );
        assert_eq!(
            target("message.send", send.clone(), None, "dakai"),
            Ok(Target::Board("dakai".into()))
        );
        assert_eq!(
            target("message.send", send.clone(), Some("local"), "dakai"),
            Ok(Target::Local)
        );
        assert_eq!(
            target("channel.join", json!({"channel": "#dev"}), None, "local"),
            Ok(Target::Local)
        );
        assert_eq!(
            target("message.send", send, Some("nope"), "local")
                .unwrap_err()
                .0,
            ErrorCode::NotFound
        );
        // a default board this host does not have is refused, never replaced by local
        let e = target(
            "message.send",
            json!({"to": {"agent": "x"}, "text": "t"}),
            None,
            "gone",
        )
        .unwrap_err();
        assert_eq!(e.0, ErrorCode::NotFound);
        assert!(
            e.1.starts_with("the default board 'gone' is not enrolled on this host"),
            "{}",
            e.1
        );
    }

    #[test]
    fn references_carry_their_board_and_bare_ids_are_local() {
        let (t, r) = resolve(
            &req("message.get", json!({"id": "dakai:01MAAA"}), None),
            "local",
            &["dakai".to_string()],
        )
        .unwrap();
        assert_eq!(t, Target::Board("dakai".into()));
        assert_eq!(r.body["id"], "01MAAA");
        // a bare id is local whatever the default board
        assert_eq!(
            target("message.get", json!({"id": "01MAAA"}), None, "dakai"),
            Ok(Target::Local)
        );
        let reply = json!({"to": {"agent": "x"}, "text": "t", "reply_to": "dakai:01MAAA"});
        let (t, r) = resolve(
            &req("message.send", reply, None),
            "local",
            &["dakai".to_string()],
        )
        .unwrap();
        assert_eq!(
            (t, r.body["reply_to"].as_str()),
            (Target::Board("dakai".into()), Some("01MAAA"))
        );
        // paging goes to the one enrolled board even when the default is local
        assert_eq!(
            target("human.page", json!({"text": "t"}), None, "local"),
            Ok(Target::Board("dakai".into()))
        );
        assert_eq!(
            target("human.page", json!({"text": "t"}), Some("local"), "dakai"),
            Ok(Target::Local)
        );
        let bare_with_board = target(
            "message.read",
            json!({"id": "01MAAA"}),
            Some("dakai"),
            "local",
        )
        .unwrap_err();
        assert_eq!(
            bare_with_board.1,
            "01MAAA is a local message; drop --board, or use the dakai: id"
        );
        assert!(
            target(
                "reaction.set",
                json!({"message_id": "other:01M"}),
                None,
                "local"
            )
            .is_err()
        );
        assert_eq!(
            target(
                "attachment.get",
                json!({"attachment_id": "local:01A"}),
                None,
                "dakai"
            ),
            Ok(Target::Local)
        );
    }

    #[test]
    fn the_registry_is_local_and_aggregates_take_board_only_when_named() {
        let refused = target(
            "agent.register",
            json!({"name": "w"}),
            Some("dakai"),
            "local",
        )
        .unwrap_err();
        assert_eq!(
            refused.1,
            "register is local; a shared address is conde publish <name> --board dakai"
        );
        assert_eq!(
            target("agent.rename", json!({}), None, "dakai"),
            Ok(Target::Local)
        );
        assert_eq!(
            target("inbox.list", json!({}), None, "dakai"),
            Ok(Target::Aggregate)
        );
        assert_eq!(
            target("inbox.list", json!({}), Some("local"), "dakai"),
            Ok(Target::Local)
        );
        assert_eq!(
            target("inbox.list", json!({}), Some("dakai"), "local"),
            Ok(Target::Board("dakai".into()))
        );
        assert_eq!(
            target("message.read", json!({}), None, "local"),
            Ok(Target::Aggregate)
        );
        assert_eq!(
            target("message.read", json!({"channel": "#dev"}), None, "dakai"),
            Ok(Target::Board("dakai".into()))
        );
    }

    #[test]
    fn shared_results_print_qualified_ids() {
        let mut v = json!({"message": {"message_id": "01M", "reply_to": null, "attachments": [{"attachment_id": "01A"}]},
                           "items": [{"message_id": "01N", "reply_to": "01M"}]});
        qualify("dakai", &mut v);
        assert_eq!(v["message"]["message_id"], "dakai:01M");
        assert_eq!(v["message"]["reply_to"], Value::Null);
        assert_eq!(v["message"]["attachments"][0]["attachment_id"], "dakai:01A");
        assert_eq!(v["items"][0]["reply_to"], "dakai:01M");
        assert_eq!(split_ref("dakai:01M"), (Some("dakai"), "01M"));
        assert_eq!(split_ref("01M"), (None, "01M"));
    }
}
