//! What the CLI and the MCP server share on top of the daemon socket (DESIGN-v2 §3, §7):
//! board references, the surface-side refusals, the wording of the board errors, and the
//! rendering of board-labelled results, so both surfaces follow one contract.
//!
//! The daemon contract the surfaces rely on:
//! - `--board <alias>` (MCP: `board`) travels in the request envelope as `board`, verbatim.
//! - Message and attachment references travel verbatim: `alias:ID` or a bare (local) ID;
//!   `local:ID` is sent bare.
//! - Every row of a result, and every result of a request resolved to a shared board,
//!   carries `"board": "<alias>"` (`"local"` or absent: the local board). Aggregate results
//!   list each board's part as `"boards": [{"board", "available"}]`.
//! - `board.list` answers on every daemon that resolves boards; a daemon that answers
//!   `unknown op` predates them and would ignore the envelope, so a request naming a board is
//!   refused before it is sent ([`stale_daemon`]).
//! - `agent.whoami` and `agent.register` results list `publications: [{board, name, state}]`
//!   (`name` is the shared `owner/name`) and the shared boards in `boards`.
//! - Aggregate parts may carry `next_cursor` (that board's page) and `unconfirmed` (a read the
//!   board did not confirm in time; its rows are previews flagged `unconfirmed`).
//! - Publication ops on the socket carry the alias in the envelope: `mailbox.publish`
//!   `{agent, name, take?}` and `mailbox.unpublish` `{agent}`, each answering
//!   `{state, op_id, name?}`; `board.list`, `board.forget`, `board.info`,
//!   `host.migrate_preview` and `host.migrate_v2 {publish: [agent_id]}` as named in
//!   [`crate::protocol::v2::op`] plus `board.list`/`board.forget` below.

use std::fmt::Write as _;

use serde_json::Value;

use crate::boards::is_alias;
use crate::protocol::v2::LOCAL_BOARD;

/// Daemon-only op: the boards of this host with their live state.
pub const BOARD_LIST: &str = "board.list";
/// Daemon-only op: forget a board alias (its file, publications and ops; never the server).
pub const BOARD_FORGET: &str = "board.forget";

/// Which surface renders: the hints name its own spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Mcp,
}

impl Surface {
    fn flag(self) -> &'static str {
        match self {
            Surface::Cli => "--board",
            Surface::Mcp => "the board argument",
        }
    }

    fn with_board(self, board: &str) -> String {
        match self {
            Surface::Cli => format!("--board {board}"),
            Surface::Mcp => format!("board=\"{board}\""),
        }
    }
}

/// `alias:ID` split into the alias and the ID; a bare ID has no alias.
pub fn split_ref(reference: &str) -> (Option<&str>, &str) {
    match reference.split_once(':') {
        Some((alias, id)) if is_alias(alias) && !id.is_empty() => (Some(alias), id),
        _ => (None, reference),
    }
}

/// The board a reference belongs to: its qualifier, else local.
pub fn ref_board(reference: &str) -> &str {
    split_ref(reference).0.unwrap_or(LOCAL_BOARD)
}

/// A message or attachment reference as the daemon gets it, after rule 2 of §3: a reference
/// qualified for one board with a board flag naming another, or a bare (local) id with a
/// shared board flag, is refused.
pub fn check_ref(surface: Surface, board: Option<&str>, reference: &str) -> Result<String, String> {
    let (alias, id) = split_ref(reference);
    let own = alias.unwrap_or(LOCAL_BOARD);
    match board {
        Some(flag) if flag != own && own == LOCAL_BOARD => Err(local_message_with_board(surface, id, flag)),
        Some(flag) if flag != own => Err(format!(
            "{reference} is a {own} message; drop {}, or use {}",
            surface.flag(),
            surface.with_board(own)
        )),
        _ if own == LOCAL_BOARD => Ok(id.to_string()),
        _ => Ok(reference.to_string()),
    }
}

/// Rule 2: a bare id is local whatever the board flag says.
pub fn local_message_with_board(surface: Surface, id: &str, board: &str) -> String {
    format!("{id} is a local message; drop {}, or use the {board}: id", surface.flag())
}

/// Rule 3: register, unregister, rename, pin and unpin act on the local registry only.
pub fn local_only(command: &str, name: &str, board: &str) -> String {
    format!("{command} is local; a shared address is conde publish {name} --board {board}")
}

/// Rule 3: a name looked up on one board only.
pub fn no_agent(name: &str, board: &str) -> String {
    if board == LOCAL_BOARD {
        format!("no agent '{name}' on the local board; shared boards are addressed with --board <name>")
    } else {
        format!("no agent '{name}' on {board}")
    }
}

/// Rule 4: a local agent without a publication acting on a shared board.
pub fn not_published(agent: &str, board: &str) -> String {
    format!("{agent} is not published on {board}. Publication is set by your human: conde publish {agent} --board {board}")
}

/// Rule 4: a publication in flight is not usable.
pub fn publication_pending(agent: &str, board: &str, state: &str) -> String {
    format!("{agent} is {state} on {board}; it is usable once that completes (conde doctor shows the pending op)")
}

/// A board alias this host has not enrolled.
pub fn unknown_board(alias: &str, enrolled: &[String]) -> String {
    let known = if enrolled.is_empty() { "none is enrolled".to_string() } else { format!("enrolled: {}", enrolled.join(", ")) };
    format!("no board '{alias}' on this host ({known}; conde boards lists them)")
}

/// A daemon started before the upgrade ignores `board` in the envelope: refuse rather than
/// act on the wrong board.
pub fn stale_daemon(surface: Surface) -> String {
    format!(
        "the running conde daemon predates boards and would ignore {}; restart it (Linux: systemctl --user restart \
         conde; macOS: launchctl kickstart -k gui/$(id -u)/space.nativeai.conde), then run this again",
        surface.flag()
    )
}

/// Whether a daemon's answer to `board.list` says it predates boards.
pub fn predates_boards(error: &str) -> bool {
    error.contains("unknown op")
}

/// Check the syntax of a board flag.
pub fn check_board_flag(board: Option<&str>) -> Result<(), String> {
    match board {
        Some(b) if !is_alias(b) => Err(format!("'{b}' is not a board alias (local, or an enrolled one such as dakai)")),
        _ => Ok(()),
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// `alias:id` for a shared board, the bare id for the local one; an id that is already
/// qualified stays as it is.
pub fn qualify(board: &str, id: &str) -> String {
    if board == LOCAL_BOARD || id.is_empty() || split_ref(id).0.is_some() {
        id.to_string()
    } else {
        format!("{board}:{id}")
    }
}

/// The board of a row: its label, else `fallback` (the enclosing result's board, else local;
/// an unlabelled result is never assumed to be on the board a request named).
fn board_of<'a>(row: &'a Value, fallback: &'a str) -> &'a str {
    row["board"].as_str().filter(|b| !b.is_empty()).unwrap_or(fallback)
}

/// The board of a whole result: its label, else local.
pub fn result_board(r: &Value) -> &str {
    board_of(r, LOCAL_BOARD)
}

/// A rendered result: `text` for stdout, `notes` for stderr (MCP shows both).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub text: String,
    pub notes: String,
}

impl Out {
    fn note(&mut self, line: impl AsRef<str>) {
        self.notes.push_str(line.as_ref());
        self.notes.push('\n');
    }

    /// One text for MCP: the output, then the notes.
    pub fn joined(self) -> String {
        let mut t = self.text;
        t.push_str(&self.notes);
        t.trim_end_matches('\n').to_string()
    }
}

/// `(dakai: unavailable)` for each board of an aggregate view that did not answer.
fn unavailable(out: &mut Out, r: &Value) {
    for part in r["boards"].as_array().into_iter().flatten() {
        if part["available"] == false {
            out.note(format!("({}: unavailable)", s(&part["board"])));
        }
    }
}

fn shared_parts(r: &Value) -> bool {
    r["boards"].as_array().is_some_and(|b| b.iter().any(|p| s(&p["board"]) != LOCAL_BOARD))
}

/// The paging hints: `after` pages one board, so an aggregate view names each part's board.
fn more(out: &mut Out, r: &Value, surface: Surface, tool: &str) {
    let hint = |board: Option<&str>, n: i64| match (surface, tool, board) {
        (Surface::Cli, "search", None) => format!("add --after {n}"),
        (Surface::Cli, "search", Some(b)) => format!("add --board {b} --after {n}"),
        (Surface::Cli, _, None) => format!("conde {tool} --after {n}"),
        (Surface::Cli, _, Some(b)) => format!("conde {tool} --board {b} --after {n}"),
        (Surface::Mcp, _, None) => format!("call {tool} with after={n}"),
        (Surface::Mcp, _, Some(b)) => format!("call {tool} with board=\"{b}\" after={n}"),
    };
    if shared_parts(r) {
        let parts = r["boards"].as_array().cloned().unwrap_or_default();
        // a daemon that lists the local part carries its cursor there
        if !parts.iter().any(|p| s(&p["board"]) == LOCAL_BOARD && p.get("next_cursor").is_some())
            && let Some(n) = r["next_cursor"].as_i64()
        {
            out.note(format!("(more on local: {})", hint(Some(LOCAL_BOARD), n)));
        }
        for part in &parts {
            if let Some(n) = part["next_cursor"].as_i64() {
                let b = s(&part["board"]);
                out.note(format!("(more on {b}: {})", hint(Some(b), n)));
            }
        }
    } else if let Some(n) = r["next_cursor"].as_i64() {
        let b = result_board(r);
        out.note(format!("(more: {})", hint((b != LOCAL_BOARD).then_some(b), n)));
    }
}

/// `queued <id> to <recipient>`.
pub fn sent_line(r: &Value) -> String {
    format!("queued {} to {}", qualify(result_board(r), s(&r["message_id"])), s(&r["to"]))
}

pub fn inbox(r: &Value, surface: Surface) -> Out {
    let requested = result_board(r);
    let mut out = Out::default();
    let items = r["items"].as_array().cloned().unwrap_or_default();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for i in &items {
        let board = board_of(i, requested);
        let _ = writeln!(
            out.text,
            "{board}\t{}\t{}\t{}\t{}\t{}",
            s(&i["kind"]),
            qualify(board, s(&i["message_id"])),
            s(&i["from"]),
            s(&i["date"]),
            s(&i["preview"])
        );
        match counts.iter_mut().find(|(b, _)| b == board) {
            Some((_, n)) => *n += 1,
            None => counts.push((board.to_string(), 1)),
        }
    }
    if items.is_empty() {
        out.note("(no unread messages)");
    } else if shared_parts(r) || counts.len() > 1 {
        let parts: Vec<String> = counts.iter().map(|(b, n)| format!("{b}: {n} unread")).collect();
        out.note(format!("({})", parts.join("; ")));
    }
    unavailable(&mut out, r);
    more(&mut out, r, surface, "inbox");
    out
}

/// `read`: messages grouped by board and place.
pub fn read(r: &Value, surface: Surface) -> Out {
    let requested = result_board(r);
    let mut out = Out::default();
    let messages = r["messages"].as_array().cloned().unwrap_or_default();
    let mut current = (String::new(), String::new());
    for m in &messages {
        let board = board_of(m, requested);
        let place = s(&m["place"]);
        if (board, place) != (current.0.as_str(), current.1.as_str()) {
            current = (board.to_string(), place.to_string());
            if place.starts_with('#') {
                let label = if board == LOCAL_BOARD { place.to_string() } else { format!("{board} {place}") };
                let _ = writeln!(out.text, "--- {label}");
            }
        }
        message(&mut out.text, m, board, surface);
    }
    if messages.is_empty() {
        out.note("(no unread messages)");
    }
    unavailable(&mut out, r);
    for part in r["boards"].as_array().into_iter().flatten() {
        if part["unconfirmed"] == true {
            out.note(format!("({}: not confirmed read; each message marked unconfirmed above names how to read it whole)", s(&part["board"])));
        }
    }
    if r["more"].as_bool() == Some(true) {
        out.note(match surface {
            Surface::Cli => "(more unread: run conde read again)",
            Surface::Mcp => "(more unread: call read again)",
        });
    }
    out
}

/// One message, headers then the body verbatim; ids qualified with their board.
pub fn message(out: &mut String, m: &Value, fallback: &str, surface: Surface) {
    let board = board_of(m, fallback);
    let unconfirmed = m["unconfirmed"] == true;
    let _ = write!(
        out,
        "=== {}{}\nFrom: {}\nTo: {}\nDate: {}\n",
        qualify(board, s(&m["message_id"])),
        if unconfirmed { " (unconfirmed: preview only)" } else { "" },
        s(&m["from"]),
        s(&m["to"]),
        s(&m["date"])
    );
    if let Some(r) = m["reply_to"].as_str() {
        let _ = writeln!(out, "In-Reply-To: {}", qualify(board, r));
    }
    if unconfirmed {
        let id = qualify(board, s(&m["message_id"]));
        let recover = match (surface, m["recover"].as_str()) {
            (Surface::Cli, Some(cmd)) => cmd.to_string(),
            (Surface::Cli, None) => format!("conde read {id}"),
            (Surface::Mcp, _) => format!("call read with target=\"{id}\""),
        };
        let _ = writeln!(out, "Unconfirmed: {board} did not confirm this read; below is only a preview. The whole message: {recover}");
    }
    for a in m["attachments"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "Attachment: {} ({} bytes; conde attachment {})",
            s(&a["filename"]),
            a["size"],
            qualify(board, s(&a["attachment_id"]))
        );
    }
    for r in m["reactions"].as_array().into_iter().flatten() {
        let by: Vec<&str> = r["by"].as_array().into_iter().flatten().map(s).collect();
        let _ = writeln!(out, "Reaction: {} {}", s(&r["emoji"]), by.join(", "));
    }
    out.push('\n');
    out.push_str(s(&m["text"]));
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

pub fn search(r: &Value, surface: Surface) -> Out {
    let requested = result_board(r);
    let mut out = Out::default();
    let hits = r["hits"].as_array().cloned().unwrap_or_default();
    for h in &hits {
        let board = board_of(h, requested);
        let date: String = s(&h["date"]).chars().take(16).collect();
        let _ = writeln!(
            out.text,
            "{board}\t{}\t{}\t{}\t{date}\t{}",
            s(&h["place"]),
            qualify(board, s(&h["message_id"])),
            s(&h["from"]),
            s(&h["line"])
        );
    }
    if hits.is_empty() {
        out.note("(no matches)");
    }
    unavailable(&mut out, r);
    more(&mut out, r, surface, "search");
    out
}

/// `sent`: one line per message with its board's receipt.
pub fn sent(r: &Value, surface: Surface) -> Out {
    let requested = result_board(r);
    let mut out = Out::default();
    let rows = r["receipts"].as_array().cloned().unwrap_or_default();
    for x in &rows {
        let board = board_of(x, requested);
        let delivery = x["delivery"].as_str().map(|d| format!("  delivery: {d}")).unwrap_or_default();
        let _ = writeln!(
            out.text,
            "{board}  {}  {}  {}{delivery}",
            qualify(board, s(&x["message_id"])),
            s(&x["to"]),
            s(&x["status"])
        );
    }
    if rows.is_empty() {
        out.note("(nothing sent)");
    }
    unavailable(&mut out, r);
    more(&mut out, r, surface, "sent");
    out
}

/// `agents`: a local row shows its address; a shared directory row shows kind, host and
/// availability, never a target.
pub fn agents(r: &Value) -> Out {
    let requested = result_board(r);
    let mut out = Out::default();
    let rows = r["agents"].as_array().cloned().unwrap_or_default();
    for a in &rows {
        let board = board_of(a, requested);
        let display = a["display"].as_str().map(|d| format!("\t({d})")).unwrap_or_default();
        let place = match a["address"].as_str() {
            Some(addr) => addr.to_string(),
            None => format!("{} on {}", s(&a["kind"]), s(&a["host"])),
        };
        let avail = match a["available"].as_bool() {
            Some(true) => "\tavailable",
            Some(false) => "\tunavailable",
            None => "",
        };
        let _ = writeln!(out.text, "{board}\t{}\t{place}{avail}{display}", s(&a["name"]));
    }
    if rows.is_empty() {
        out.note("(registry empty)");
    }
    out
}

pub fn channels(r: &Value) -> Out {
    let requested = result_board(r);
    let mut out = Out::default();
    let rows = r["channels"].as_array().cloned().unwrap_or_default();
    for c in &rows {
        let board = board_of(c, requested);
        let name = match c["topic"].as_str() {
            Some(topic) => format!("{}: {topic}", s(&c["channel"])),
            None => s(&c["channel"]).to_string(),
        };
        let _ = writeln!(out.text, "{board}\t{name}\t{} members\t{} messages", c["members"], c["messages"]);
    }
    if rows.is_empty() {
        out.note("(no channels)");
    }
    out
}

fn publications(r: &Value) -> Vec<&Value> {
    r["publications"].as_array().map(|p| p.iter().collect()).unwrap_or_default()
}

fn shared_boards(r: &Value) -> Vec<&str> {
    r["boards"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| p.as_str().unwrap_or_else(|| s(&p["board"])))
        .filter(|b| !b.is_empty() && *b != LOCAL_BOARD)
        .collect()
}

fn publication_line(p: &Value) -> String {
    let state = s(&p["state"]);
    let suffix = if state.is_empty() || state == "published" { String::new() } else { format!(" ({state})") };
    format!("published: {} as {}{suffix}", s(&p["board"]), s(&p["name"]))
}

/// `whoami`: the local name and address, and every publication (the shared name may differ).
pub fn whoami(r: &Value) -> String {
    let mut out = match r["address"].as_str() {
        Some(addr) => format!("{} (local, {addr})", s(&r["name"])),
        None => format!("{} (local)", s(&r["name"])),
    };
    let pubs = publications(r);
    for p in &pubs {
        out.push('\n');
        out.push_str(&publication_line(p));
    }
    for b in shared_boards(r) {
        if !pubs.iter().any(|p| s(&p["board"]) == b) {
            let _ = write!(out, "\nnot published on {b}");
        }
    }
    out
}

/// `register`: always the local board; a hint for every shared board the name is not on.
pub fn registered(r: &Value, typed: &str) -> String {
    let mut out = format!("registered {} on the local board ({})", s(&r["name"]), s(&r["address"]));
    if let Some(lobby) = r["joined"].as_str() {
        let _ = write!(out, "\njoined {lobby}");
    }
    let pubs = publications(r);
    for p in &pubs {
        out.push('\n');
        out.push_str(&publication_line(p));
    }
    let short = typed.rsplit('/').next().unwrap_or(typed);
    for b in shared_boards(r) {
        if !pubs.iter().any(|p| s(&p["board"]) == b) {
            let _ = write!(
                out,
                "\nnot published; teammates on {b} cannot reach it (your human: conde publish {short} --board {b})"
            );
        }
    }
    out
}

/// `rename`: a local rename does not rename a mailbox.
pub fn renamed(r: &Value) -> String {
    let mut out = format!("renamed {} -> {}", s(&r["old"]), s(&r["name"]));
    for p in publications(r) {
        let _ = write!(
            out,
            "\nstill published on {} as {} (a local rename does not rename the mailbox)",
            s(&p["board"]),
            s(&p["name"])
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn references_follow_rule_two() {
        assert_eq!(split_ref("dakai:01M4"), (Some("dakai"), "01M4"));
        assert_eq!(split_ref("01M4"), (None, "01M4"));
        assert_eq!(split_ref("Bad:01"), (None, "Bad:01"));
        assert_eq!(check_ref(Surface::Cli, None, "dakai:01M4"), Ok("dakai:01M4".into()));
        assert_eq!(check_ref(Surface::Cli, Some("dakai"), "dakai:01M4"), Ok("dakai:01M4".into()));
        assert_eq!(check_ref(Surface::Cli, None, "local:01M4"), Ok("01M4".into()));
        assert_eq!(check_ref(Surface::Cli, Some("local"), "01M4"), Ok("01M4".into()));
        assert_eq!(
            check_ref(Surface::Cli, Some("dakai"), "01M4"),
            Err("01M4 is a local message; drop --board, or use the dakai: id".into())
        );
        assert_eq!(
            check_ref(Surface::Mcp, Some("dakai"), "01M4"),
            Err("01M4 is a local message; drop the board argument, or use the dakai: id".into())
        );
        assert_eq!(
            check_ref(Surface::Cli, Some("local"), "dakai:01M4"),
            Err("dakai:01M4 is a dakai message; drop --board, or use --board dakai".into())
        );
        assert_eq!(
            check_ref(Surface::Mcp, Some("other"), "dakai:01M4"),
            Err("dakai:01M4 is a dakai message; drop the board argument, or use board=\"dakai\"".into())
        );
    }

    #[test]
    fn ids_are_qualified_by_their_row_board() {
        assert_eq!(qualify("local", "01M"), "01M");
        assert_eq!(qualify("dakai", "01M"), "dakai:01M");
        assert_eq!(qualify("dakai", "dakai:01M"), "dakai:01M");
        let r = json!({"items": [
            {"board": "local", "kind": "dm", "message_id": "01L", "from": "a", "date": "d", "preview": "p"},
            {"board": "dakai", "kind": "#dev", "message_id": "01D", "from": "b/x", "date": "d", "preview": "q"}],
            "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true}, {"board": "two", "available": false}]});
        let out = inbox(&r, Surface::Cli);
        assert_eq!(out.text, "local\tdm\t01L\ta\td\tp\ndakai\t#dev\tdakai:01D\tb/x\td\tq\n");
        assert_eq!(out.notes, "(local: 1 unread; dakai: 1 unread)\n(two: unavailable)\n");
        // an unlabelled row takes its result's board, else local; never the board a request named
        let out = sent(&json!({"board": "dakai", "receipts": [{"message_id": "01X", "to": "b/y", "status": "unread"}]}), Surface::Cli);
        assert_eq!(out.text, "dakai  dakai:01X  b/y  unread\n");
        let out = sent(&json!({"receipts": [{"message_id": "01X", "to": "b/y", "status": "unread"}]}), Surface::Cli);
        assert_eq!(out.text, "local  01X  b/y  unread\n");
    }

    #[test]
    fn a_shared_sent_continuation_is_rendered() {
        // Astra's second-pass probe
        let result = json!({"receipts": [{"board": "dakai", "message_id": "dakai:01M", "to": "alice/worker", "status": "queued"}],
            "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "next_cursor": 200}]});
        assert!(sent(&result, Surface::Cli).joined().contains("conde sent --board dakai --after 200"));
        assert!(sent(&result, Surface::Mcp).joined().contains("call sent with board=\"dakai\" after=200"));
    }

    #[test]
    fn whoami_and_register_show_both_names() {
        let r = json!({"name": "me/worker", "address": "claude:s1@h",
            "publications": [{"board": "dakai", "name": "me/reviewer", "state": "published"}],
            "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true}, {"board": "two", "available": false}]});
        assert_eq!(whoami(&r), "me/worker (local, claude:s1@h)\npublished: dakai as me/reviewer\nnot published on two");
        let r = json!({"name": "me/worker", "address": "claude:s1@h", "publications": [], "boards": ["dakai"]});
        assert_eq!(
            registered(&r, "worker"),
            "registered me/worker on the local board (claude:s1@h)\n\
             not published; teammates on dakai cannot reach it (your human: conde publish worker --board dakai)"
        );
    }
}
