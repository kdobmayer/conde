//! `conde import`: a synthetic kond board covering every record type, the
//! idempotent re-run, name conflicts, parse errors, and a dry run against a
//! copy of the real board when one exists.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use conde::dispatch::Dispatcher;
use conde::import::{Counts, Options, Report, run};
use conde::protocol::Request;
use conde::util::new_ulid;
use rusqlite::Connection;
use serde_json::json;

#[path = "pg/mod.rs"]
mod pg;

const LIVE: &str = "22222222-2222-2222-2222-222222222222";
const DEAD: &str = "33333333-3333-3333-3333-333333333333";
/// Where the board lived when kond wrote the Attachment headers.
const ORIGIN: &str = "/home/someone/.local/share/kond";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("conde-import-{tag}-{}-{}", std::process::id(), new_ulid()));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(n: u32) -> String {
    format!("01K{n:023}")
}

fn put(path: &Path, content: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// kond's render_message: headers, blank line, body ending in a newline.
fn mail(headers: &[(&str, &str)], body: &str) -> String {
    let mut out: String = headers.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
    out.push('\n');
    out.push_str(body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

const BLOB: &[u8] = &[0, 159, 146, 150, 255, 10, 13, 0];

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn board(&self) -> PathBuf {
        self.root.join("board")
    }

    fn agent(&self, name: &str, sub: &str, file: &str, content: &str) {
        put(&self.board().join("agents").join(name).join(sub).join(file), content);
    }

    /// A delivered DM: the sender's sent copy plus the recipient's inbox copy.
    fn dm(&self, n: u32, from: &str, to: &str, extra: &[(&str, &str)], body: &str, inbox: &str) -> String {
        let id = id(n);
        let date = format!("2025-01-01T00:00:{n:02}.000Z");
        let mut headers = vec![("From", from), ("To", to), ("Date", date.as_str())];
        headers.extend_from_slice(extra);
        let raw = mail(&headers, body);
        self.agent(from, "outbox/sent", &id, &raw);
        self.agent(&to.to_lowercase(), inbox, &id, &raw);
        id
    }

    /// A channel post: the sender's sent copy plus the channel log.
    fn post(&self, n: u32, from: &str, channel: &str, extra: &[(&str, &str)], body: &str) -> String {
        let id = id(n);
        let date = format!("2025-01-01T00:01:{n:02}.000Z");
        let to = format!("#{channel}");
        let mut headers = vec![("From", from), ("To", to.as_str()), ("Date", date.as_str())];
        headers.extend_from_slice(extra);
        let raw = mail(&headers, body);
        self.agent(from, "outbox/sent", &id, &raw);
        put(&self.board().join("channels").join(channel).join("messages").join(&id), &raw);
        id
    }
}

/// Every kond record type plus the edge cases: missing attachment, invalid
/// and colliding names, pin over registry, profile, reply-to, reactions
/// (add, add+remove, channel, orphan), read and unread DMs, channel cursors
/// (empty, on a post, on a reaction event), a bounced message, runtime state.
fn build_fixture(root: &Path) -> Fixture {
    let f = Fixture { root: root.to_path_buf() };
    put(&root.join("registry/krisz"), "human:Tester\n");
    put(&root.join("registry/lead/claude"), "claude:11111111-1111-1111-1111-111111111111\n");
    put(&root.join("state/registry-pin/lead.claude"), format!("claude:{LIVE}\n"));
    put(&root.join("registry/old/worker"), format!("claude:{DEAD}\n"));
    put(&root.join("registry/cx/worker"), "codex:thread-1\n");
    put(&root.join("registry/weird+name"), "codex:thread-2\n");
    put(&root.join("registry/weird name"), "codex:thread-3\n");
    put(&root.join("profiles/lead/claude"), "Display: Lead role\nTeam: core\n");
    put(&root.join("state/notified").join(id(1)), "");
    put(&root.join("state/held/x"), "m s 1\n");
    put(&root.join("state/registry-deny/lead.claude"), "claude:*\n");
    put(&root.join("state/starred/krisz"), "#team\n");

    let att = format!("{ORIGIN}/board/attachments/{}/blob.bin", id(1));
    let gone = format!("{ORIGIN}/board/attachments/{}/gone.txt", id(1));
    put(&f.board().join("attachments").join(id(1)).join("blob.bin"), BLOB);
    let m1 = f.dm(1, "krisz", "lead.claude", &[("Attachment", &att), ("Attachment", &gone)], "hello\n\nwith a blank line", "inbox/cur");
    f.agent("krisz", "outbox/sent", &format!("{m1}.read"), "2025-01-01T00:05:00Z\n");
    let m2 = f.dm(2, "lead.claude", "krisz", &[("In-Reply-To", &m1)], "re: hello", "inbox/new");
    f.dm(3, "weird+name", "cx.worker", &[], "from an odd name", "inbox/cur");
    f.dm(4, "krisz", "lead.claude", &[("In-Reply-To", &m2), ("Reaction", "\u{1F44D}")], "\u{1F44D}", "inbox/new");
    f.dm(5, "krisz", "lead.claude", &[("In-Reply-To", &m2), ("Reaction", "\u{1F389}")], "\u{1F389}", "inbox/cur");
    f.dm(6, "krisz", "lead.claude", &[("In-Reply-To", &m2), ("Reaction-Remove", "\u{1F389}")], "-\u{1F389}", "inbox/new");

    let p1 = f.post(7, "krisz", "team", &[], "kickoff");
    f.post(8, "lead.claude", "team", &[("In-Reply-To", &p1)], "@old.worker please look");
    f.post(9, "old.worker", "team", &[("In-Reply-To", &p1), ("Reaction", "\u{1F440}")], "\u{1F440}");
    let cursors = f.board().join("channels/team/cursors");
    put(&cursors.join("krisz"), id(7));
    put(&cursors.join("lead.claude"), "");
    put(&cursors.join("old.worker"), id(9));
    fs::create_dir_all(f.board().join("channels/team/tmp")).unwrap();

    let bounced = mail(&[("From", "cx.worker"), ("To", "nobody"), ("Date", "2025-01-01T00:00:10.000Z")], "lost");
    f.agent("cx.worker", "outbox/failed", &id(10), &bounced);
    f.agent("cx.worker", "outbox/failed", &format!("{}.reason", id(10)), "unknown recipient 'nobody' for 24h\n");
    f.dm(11, "cx.worker", "weird+name", &[("In-Reply-To", &id(99)), ("Reaction", "\u{2705}")], "\u{2705}", "inbox/new");
    f
}

fn options(from: &Path, db: &Path, dry_run: bool) -> Options {
    Options {
        from: from.to_path_buf(),
        db: db.to_path_buf(),
        database_url: None,
        owner: "tester".into(),
        host: "testhost".into(),
        dry_run,
        live_claude: Some(HashSet::from([LIVE.to_string()])),
        progress: false,
        expect_files: None,
    }
}

const EXPECTED: Counts =
    Counts { agents: 6, channels: 1, messages: 5, attachments: 1, memberships: 3, dm_read: 2, reactions: 3 };

fn table_counts(db: &Path) -> Vec<(String, i64)> {
    let conn = Connection::open(db).unwrap();
    ["users", "hosts", "agents", "channels", "messages", "attachments", "memberships", "dm_read", "reactions", "notifications"]
        .iter()
        .map(|t| (t.to_string(), conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()))
        .collect()
}

fn q<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> T {
    conn.query_row(sql, args, |r| r.get(0)).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn assert_clean(report: &Report) {
    assert!(report.ok(), "report not ok:\n{report}");
    let v = report.validation.as_ref().expect("validation ran");
    assert!(v.problems.is_empty(), "{:?}", v.problems);
}

#[test]
fn imports_every_record_type_and_reruns_as_a_no_op() {
    let tmp = TempDir::new("full");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde/conde.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();

    // Dry run: full report, nothing written.
    let dry = run(&options(&kond, &db, true)).unwrap();
    assert!(!db.exists(), "a dry run must not create the store");
    assert_eq!(dry.source, EXPECTED, "{dry}");
    assert_eq!(dry.present, Counts::default());
    assert_eq!(dry.added, EXPECTED);
    assert!(dry.validation.is_none());
    assert!(dry.parse_errors.is_empty(), "{:?}", dry.parse_errors);
    assert_eq!(dry.missing_attachments.len(), 1, "{:?}", dry.missing_attachments);
    assert!(dry.missing_attachments[0].contains("gone.txt"));
    assert_eq!(dry.stale.as_deref(), Some(&[format!("tester/old.worker -> claude:{DEAD}@testhost")][..]));
    assert_eq!(dry.codex_bindings, 3);
    assert_eq!(dry.pins, vec![format!("tester/lead.claude -> claude:{LIVE}@testhost")]);
    assert_eq!(
        dry.renames,
        vec![("weird name".to_string(), "weird-name".to_string()), ("weird+name".to_string(), "weird-name-2".to_string())]
    );
    assert!(dry.conflicts.is_empty());
    let notes = dry.notes.join("\n");
    assert!(notes.contains("1 bounced message(s)"), "{notes}");
    assert!(notes.contains("1 reaction(s) on a message not on the board"), "{notes}");
    assert!(notes.contains("5 reaction event message(s) folded into 3 reaction row(s)"), "{notes}");
    assert!(notes.contains("state/notified (1)") && notes.contains("state/held (1)"), "{notes}");
    assert!(notes.contains("1 DM(s) read without a .read stamp"), "{notes}");
    assert!(notes.contains("1 profile field(s) other than Display"), "{notes}");

    // Import.
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert_eq!(report.added, EXPECTED, "{report}");
    let text = report.to_string();
    assert!(text.contains("bodies: 5 of 5 sha256-equal to the source file"), "{text}");
    assert!(text.contains("attachments: 1 of 1 sha256-equal to the source file"), "{text}");
    assert!(text.contains("cursors: 3 checked, 3 at or past"), "{text}");

    let conn = Connection::open(&db).unwrap();
    let addr = |name: &str| -> Option<String> {
        q(&conn, "SELECT address_kind || ':' || address_target || '@' || host_id FROM agents WHERE name_lower = ?1", [name])
    };
    assert_eq!(addr("krisz").as_deref(), Some("human:tester@testhost"));
    assert_eq!(addr("lead.claude").as_deref(), Some(format!("claude:{LIVE}@testhost").as_str()));
    assert_eq!(addr("old.worker").as_deref(), Some(format!("claude:{DEAD}@testhost").as_str()));
    assert_eq!(addr("weird-name").as_deref(), Some("codex:thread-3@testhost"));
    assert_eq!(addr("weird-name-2").as_deref(), Some("codex:thread-2@testhost"));
    let pinned: Option<String> = q(&conn, "SELECT pinned_address FROM agents WHERE name_lower = 'lead.claude'", []);
    assert_eq!(pinned, Some(format!("claude:{LIVE}@testhost")));
    let display: Option<String> = q(&conn, "SELECT display FROM agents WHERE name_lower = 'lead.claude'", []);
    assert_eq!(display.as_deref(), Some("Lead role"));
    let owners: i64 = q(&conn, "SELECT COUNT(DISTINCT owner_id) FROM agents", []);
    assert_eq!(owners, 1);

    // Historical ids, verbatim bodies, sender, recipient, date, reply-to.
    let row = |mid: &str| -> (String, Option<String>, Option<String>, String, Option<String>, String) {
        conn.query_row(
            "SELECT s.name_lower, r.name_lower, c.name_lower, m.text, m.reply_to, m.created_at FROM messages m
             JOIN agents s ON s.agent_id = m.sender_agent_id LEFT JOIN agents r ON r.agent_id = m.recipient_agent_id
             LEFT JOIN channels c ON c.channel_id = m.channel_id WHERE m.message_id = ?1",
            [mid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap()
    };
    let m1 = row(&id(1));
    assert_eq!((m1.0.as_str(), m1.1.as_deref()), ("krisz", Some("lead.claude")));
    assert_eq!(m1.3, "hello\n\nwith a blank line\n");
    assert_eq!(m1.5, "2025-01-01T00:00:01.000Z");
    let m2 = row(&id(2));
    assert_eq!(m2.4, Some(id(1)));
    assert_eq!(row(&id(3)).0, "weird-name-2");
    let p2 = row(&id(8));
    assert_eq!((p2.0.as_str(), p2.2.as_deref(), p2.4.clone()), ("lead.claude", Some("team"), Some(id(7))));
    let reaction_messages: i64 =
        q(&conn, "SELECT COUNT(*) FROM messages WHERE message_id IN (?1, ?2, ?3, ?4, ?5)", [id(4), id(5), id(6), id(9), id(11)]);
    assert_eq!(reaction_messages, 0, "reaction events are reactions, not messages");
    let seqs: Vec<String> = {
        let mut stmt = conn.prepare("SELECT message_id FROM messages ORDER BY commit_seq").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(seqs, vec![id(1), id(2), id(3), id(7), id(8)], "commit order follows kond id order");

    // Attachment bytes, transactional with its message; the missing one is skipped.
    let (filename, bytes, size): (String, Vec<u8>, i64) = conn
        .query_row("SELECT filename, bytes, size FROM attachments WHERE message_id = ?1", [id(1)], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((filename.as_str(), bytes.as_slice(), size), ("blob.bin", BLOB, BLOB.len() as i64));

    // Read state: M1 read at its stamp, M3 read (no stamp), M2 unread.
    let read_at: String = q(&conn, "SELECT read_at FROM dm_read WHERE message_id = ?1", [id(1)]);
    assert_eq!(read_at, "2025-01-01T00:05:00.000Z");
    let read: i64 = q(&conn, "SELECT COUNT(*) FROM dm_read WHERE message_id IN (?1, ?2)", [id(2), id(3)]);
    assert_eq!(read, 1);
    let m2_read: i64 = q(&conn, "SELECT COUNT(*) FROM dm_read WHERE message_id = ?1", [id(2)]);
    assert_eq!(m2_read, 0);

    // Reactions: last event wins, removal kept as present = 0.
    let reaction = |mid: &str, who: &str, emoji: &str| -> Option<bool> {
        conn.query_row(
            "SELECT x.present FROM reactions x JOIN agents a ON a.agent_id = x.actor_id
             WHERE x.message_id = ?1 AND a.name_lower = ?2 AND x.emoji = ?3",
            [mid, who, emoji],
            |r| r.get(0),
        )
        .ok()
    };
    assert_eq!(reaction(&id(2), "krisz", "\u{1F44D}"), Some(true));
    assert_eq!(reaction(&id(2), "krisz", "\u{1F389}"), Some(false));
    assert_eq!(reaction(&id(7), "old.worker", "\u{1F440}"), Some(true));

    // Membership and cursors: "" -> 0, on a post -> that post, on a reaction event -> last post before it.
    let seq_of = |mid: &str| -> i64 { q(&conn, "SELECT commit_seq FROM messages WHERE message_id = ?1", [mid]) };
    let cursor = |who: &str| -> i64 {
        q(&conn, "SELECT x.read_seq FROM memberships x JOIN agents a ON a.agent_id = x.agent_id WHERE a.name_lower = ?1", [who])
    };
    assert_eq!(cursor("krisz"), seq_of(&id(7)));
    assert_eq!(cursor("lead.claude"), 0);
    assert_eq!(cursor("old.worker"), seq_of(&id(8)));

    // No notification for imported messages; runtime state not imported.
    let notifications: i64 = q(&conn, "SELECT COUNT(*) FROM notifications", []);
    assert_eq!(notifications, 0);
    drop(conn);

    // Re-run: nothing added, nothing changed.
    let before = table_counts(&db);
    let again = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&again);
    assert_eq!(again.added, Counts::default(), "{again}");
    assert_eq!(again.present, EXPECTED);
    assert_eq!(table_counts(&db), before);
    let dry_again = run(&options(&kond, &db, true)).unwrap();
    assert_eq!(dry_again.added, Counts::default());
    assert!(dry_again.conflicts.is_empty(), "{:?}", dry_again.conflicts);
}

#[test]
fn imported_board_reads_through_the_dispatcher() {
    let tmp = TempDir::new("ops");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    assert_clean(&run(&options(&kond, &db, false)).unwrap());

    let d = Dispatcher::open(&db).unwrap();
    let p = d.principal("tester", "testhost", true).unwrap();
    let call = |actor: &str, op: &str, body: serde_json::Value| {
        let mut req = Request::new(new_ulid(), op, body);
        req.actor = Some(actor.to_string());
        req.idempotency_key = Some(new_ulid());
        let resp = d.dispatch(&p, &req);
        assert!(resp.error.is_none(), "{op} as {actor}: {:?}", resp.error);
        resp.result.unwrap()
    };
    let human = "human:tester";
    assert_eq!(call(human, "agent.whoami", json!({}))["name"], "tester/krisz");
    // Unread for the human: M2 only (DM), nothing in #team past its cursor but lead's post.
    let inbox = call(human, "inbox.list", json!({}));
    let ids: Vec<&str> = inbox["items"].as_array().unwrap().iter().map(|i| i["message_id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![id(2).as_str(), id(8).as_str()]);
    let shown = call(&format!("claude:{LIVE}"), "message.get", json!({"id": id(2)}));
    assert_eq!(shown["message"]["reactions"], json!([{"emoji": "\u{1F44D}", "by": ["tester/krisz"]}]));
    let m1 = call(human, "message.get", json!({"id": id(1)}));
    let att = &m1["message"]["attachments"][0];
    assert_eq!(att["filename"], "blob.bin");
    let receipts = call(human, "receipt.list", json!({}));
    assert_eq!(receipts["receipts"][0]["status"], "read 2025-01-01T00:05:00.000Z");
}

#[test]
fn existing_conde_agent_is_reported_and_kept() {
    let tmp = TempDir::new("conflict");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    {
        let d = Dispatcher::open(&db).unwrap();
        let p = d.principal("tester", "testhost", true).unwrap();
        let mut req = Request::new(new_ulid(), "agent.register", json!({"name": "cx.worker"}));
        req.actor = Some("codex:other-thread".into());
        req.idempotency_key = Some(new_ulid());
        assert!(d.dispatch(&p, &req).error.is_none());
    }
    let dry = run(&options(&kond, &db, true)).unwrap();
    assert_eq!(dry.present.agents, 1);
    assert_eq!(dry.conflicts.len(), 1, "{:?}", dry.conflicts);
    assert!(dry.conflicts[0].starts_with("tester/cx.worker: conde has codex:other-thread@testhost"), "{:?}", dry.conflicts);

    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert_eq!(report.added.agents, EXPECTED.agents - 1);
    let conn = Connection::open(&db).unwrap();
    let target: String = q(&conn, "SELECT address_target FROM agents WHERE name_lower = 'cx.worker'", []);
    assert_eq!(target, "other-thread", "conde's binding is kept");
    let to_cx: i64 = q(
        &conn,
        "SELECT COUNT(*) FROM messages m JOIN agents a ON a.agent_id = m.recipient_agent_id WHERE a.name_lower = 'cx.worker'",
        [],
    );
    assert_eq!(to_cx, 1, "history lands on the existing agent");
}

#[test]
fn host_of_another_user_is_refused() {
    let tmp = TempDir::new("host");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    Dispatcher::open(&db).unwrap().principal("someone", "testhost", true).unwrap();
    for dry in [true, false] {
        let err = run(&options(&kond, &db, dry)).expect_err("refused").to_string();
        assert!(err.contains("belongs to user 'someone'"), "{err}");
    }
}

#[test]
fn unreadable_files_are_parse_errors_and_the_rest_imports() {
    let tmp = TempDir::new("broken");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    f.agent("krisz", "inbox/cur", &id(20), "\u{0}");
    put(&f.board().join("agents/krisz/inbox/cur").join(id(21)), [0xff, 0xfe, b'\n', b'\n', b'x']);
    f.agent("krisz", "inbox/cur", "notes.txt", "stray");
    let db = tmp.0.join("conde.db");
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_eq!(report.parse_errors.len(), 3, "{:?}", report.parse_errors);
    assert!(!report.ok());
    assert_eq!(report.source, EXPECTED);
    assert!(report.validation.as_ref().unwrap().problems.is_empty());
}

#[test]
fn cli_dry_run_writes_nothing() {
    let tmp = TempDir::new("cli");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let data = tmp.0.join("data");
    let run_cli = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_conde"))
            .env_clear()
            .env("HOME", &tmp.0)
            .env("USER", "tester")
            .env("CONDE_DATA", &data)
            .env("CONDE_HOST", "testhost")
            .args(["import", "--from"])
            .arg(&kond)
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run_cli(&["--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dry run, nothing written") && stdout.contains("(owner tester, host testhost)"), "{stdout}");
    assert!(!data.exists(), "dry run created {}", data.display());
    let out = run_cli(&[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("result: OK"), "{stdout}");
    assert!(data.join("conde.db").exists());
}

/// A dry run over a copy of the owner's real kond board: it must parse.
#[test]
fn real_board_copy_dry_run_has_no_parse_errors() {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let live = std::env::var_os("KOND_DATA").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share/kond"));
    if !live.join("board").is_dir() {
        eprintln!("skipped: no kond board at {}", live.display());
        return;
    }
    let tmp = TempDir::new("real");
    let copy = tmp.0.join("kond");
    // kond may move a file mid-copy; a vanished file is simply absent from the copy.
    let _ = Command::new("cp").arg("-a").arg("--reflink=auto").arg(&live).arg(&copy).status();
    assert!(copy.join("board").is_dir(), "copy of {} failed", live.display());
    let db = tmp.0.join("conde.db");
    let mut opts = options(&copy, &db, true);
    opts.live_claude = None;
    let report = run(&opts).unwrap();
    assert!(report.parse_errors.is_empty(), "{report}");
    assert!(report.source.messages > 0);
    assert!(!db.exists());
}

fn native(db: &Path) -> (Dispatcher, conde::dispatch::Principal) {
    let d = Dispatcher::open(db).unwrap();
    let p = d.principal("tester", "testhost", true).unwrap();
    (d, p)
}

fn call(d: &Dispatcher, p: &conde::dispatch::Principal, actor: &str, op: &str, body: serde_json::Value) -> serde_json::Value {
    let mut req = Request::new(new_ulid(), op, body);
    req.actor = Some(actor.into());
    req.idempotency_key = Some(new_ulid());
    let r = d.dispatch(p, &req);
    assert!(r.error.is_none(), "{op}: {:?}", r.error);
    r.result.unwrap()
}

#[test]
fn symlinked_attachment_file_outside_the_snapshot_is_rejected() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new("symlink-file");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    let external = tmp.0.join("outside-snapshot.txt");
    put(&external, b"outside bytes");
    let link = f.board().join("attachments").join(id(1)).join("blob.bin");
    fs::remove_file(&link).unwrap();
    symlink(&external, &link).unwrap();
    let db = tmp.0.join("conde.db");
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert_eq!(report.added.attachments, 0);
    assert!(
        report.missing_attachments.iter().any(|m| m.contains("blob.bin") && m.contains("symlink, not followed")),
        "{:?}",
        report.missing_attachments
    );
    let rows: i64 = q(&Connection::open(&db).unwrap(), "SELECT COUNT(*) FROM attachments", []);
    assert_eq!(rows, 0, "no bytes from outside the snapshot");
}

#[test]
fn symlinked_attachment_directory_and_walked_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new("symlink-dir");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    let outside = tmp.0.join("outside");
    put(&outside.join("blob.bin"), b"outside bytes");
    put(&outside.join("msg"), mail(&[("From", "krisz"), ("To", "lead.claude"), ("Date", "2025-01-01T00:00:30.000Z")], "x"));
    let dir = f.board().join("attachments").join(id(1));
    fs::remove_dir_all(&dir).unwrap();
    symlink(&outside, &dir).unwrap();
    symlink(outside.join("msg"), f.board().join("agents/lead.claude/inbox/cur").join(id(30))).unwrap();
    let db = tmp.0.join("conde.db");
    let report = run(&options(&kond, &db, false)).unwrap();
    assert!(
        report.missing_attachments.iter().any(|m| m.contains("blob.bin") && m.contains("symlink, not followed")),
        "{:?}",
        report.missing_attachments
    );
    assert_eq!(report.parse_errors.len(), 1, "{:?}", report.parse_errors);
    assert!(report.parse_errors[0].contains("symlink, not followed"));
    let conn = Connection::open(&db).unwrap();
    let rows: i64 = q(&conn, "SELECT COUNT(*) FROM attachments", []);
    assert_eq!(rows, 0);
    let followed: i64 = q(&conn, "SELECT COUNT(*) FROM messages WHERE message_id = ?1", [id(30)]);
    assert_eq!(followed, 0);
}

#[test]
fn restored_attachment_is_backfilled_on_rerun() {
    let tmp = TempDir::new("backfill");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    let first = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&first);
    assert_eq!(first.added.attachments, 1);
    put(&f.board().join("attachments").join(id(1)).join("gone.txt"), b"restored attachment");
    let dry = run(&options(&kond, &db, true)).unwrap();
    assert_eq!((dry.source.attachments, dry.present.attachments, dry.added.attachments), (2, 1, 1));
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert_eq!(report.added, Counts { attachments: 1, ..Counts::default() }, "{report}");
    let conn = Connection::open(&db).unwrap();
    let mut stmt = conn.prepare("SELECT filename, bytes FROM attachments WHERE message_id = ?1 ORDER BY filename").unwrap();
    let rows: Vec<(String, Vec<u8>)> =
        stmt.query_map([id(1)], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(rows, vec![("blob.bin".into(), BLOB.to_vec()), ("gone.txt".into(), b"restored attachment".to_vec())]);
    let again = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&again);
    assert_eq!(again.added, Counts::default());
}

/// Disclosed limitation (in `conde import --help`): a re-run re-adds a
/// membership that was removed in conde after the import.
#[test]
fn rerun_readds_a_membership_left_in_conde() {
    let tmp = TempDir::new("rejoin");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    assert_clean(&run(&options(&kond, &db, false)).unwrap());
    {
        let (d, p) = native(&db);
        call(&d, &p, &format!("claude:{DEAD}"), "channel.leave", json!({"channel": "#team"}));
    }
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert_eq!(report.added.memberships, 1);
}

#[test]
fn channel_with_native_posts_is_refused_before_any_write() {
    let tmp = TempDir::new("native-channel");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    let (d, p) = native(&db);
    call(&d, &p, "human:tester", "agent.register", json!({"name": "krisz"}));
    call(&d, &p, &format!("claude:{LIVE}"), "agent.register", json!({"name": "lead.claude"}));
    call(&d, &p, "human:tester", "channel.create", json!({"channel": "#team"}));
    call(&d, &p, "human:tester", "channel.invite", json!({"channel": "#team", "name": "lead.claude"}));
    let posted = call(&d, &p, &format!("claude:{LIVE}"), "message.send", json!({"to": {"channel": "#team"}, "text": "native unread"}));
    let native_id = posted["message_id"].clone();
    let unread = |d: &Dispatcher| {
        call(d, &p, "human:tester", "inbox.list", json!({}))["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["message_id"] == native_id)
    };
    assert!(unread(&d));
    drop(d);
    let before = table_counts(&db);

    for dry in [true, false] {
        let report = run(&options(&kond, &db, dry)).unwrap();
        assert!(!report.ok(), "{report}");
        assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
        assert!(report.refused[0].starts_with("#team: holds 2 conde message(s)"), "{:?}", report.refused);
        assert!(report.validation.is_none());
        assert_eq!(table_counts(&db), before, "nothing written (dry run: {dry})");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_conde"))
        .env_clear()
        .env("HOME", &tmp.0)
        .env("USER", "tester")
        .env("CONDE_DATA", db.parent().unwrap())
        .env("CONDE_HOST", "testhost")
        .args(["import", "--dry-run", "--from"])
        .arg(&kond)
        .output()
        .unwrap();
    assert!(!out.status.success(), "a refused import exits non-zero");
    let (d, _) = native(&db);
    assert!(unread(&d), "the native post stays unread");
}

#[test]
fn existing_empty_channel_is_merged() {
    let tmp = TempDir::new("empty-channel");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    {
        let (d, p) = native(&db);
        call(&d, &p, "human:tester", "agent.register", json!({"name": "krisz"}));
        call(&d, &p, "human:tester", "channel.create", json!({"channel": "#team"}));
    }
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert!(report.refused.is_empty());
    assert_eq!((report.present.channels, report.added.channels), (1, 0));
    let posts: i64 = q(&Connection::open(&db).unwrap(), "SELECT COUNT(*) FROM messages WHERE channel_id IS NOT NULL", []);
    assert_eq!(posts, 2);
}

fn external_message(n: u32) -> String {
    mail(&[("From", "krisz"), ("To", "lead.claude"), ("Date", "2025-01-01T00:00:30.000Z")], &format!("external {n}"))
}

#[test]
fn symlinked_fixed_directory_on_the_way_is_not_followed() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new("symlink-inbox");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    let outside = tmp.0.join("outside-inbox");
    put(&outside.join("new").join(id(30)), external_message(30));
    let inbox = f.board().join("agents/lead.claude/inbox");
    fs::remove_dir_all(&inbox).unwrap();
    symlink(&outside, &inbox).unwrap();
    let db = tmp.0.join("conde.db");
    let report = run(&options(&kond, &db, false)).unwrap();
    assert!(!report.ok());
    assert_eq!(report.parse_errors, vec![format!("{}: symlink, not followed", inbox.display())], "reported once");
    let imported: i64 = q(&Connection::open(&db).unwrap(), "SELECT COUNT(*) FROM messages WHERE message_id = ?1", [id(30)]);
    assert_eq!(imported, 0, "nothing from outside the snapshot");
}

#[test]
fn symlinked_channel_and_registry_directories_are_not_followed() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new("symlink-channel-registry");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    let outside = tmp.0.join("outside");
    let ext_channel = outside.join("channel");
    put(&ext_channel.join("messages").join(id(31)), external_message(31));
    put(&ext_channel.join("cursors/krisz"), "");
    let channel_link = f.board().join("channels/ext");
    symlink(&ext_channel, &channel_link).unwrap();
    put(&outside.join("registry-sub/worker"), "codex:ext-thread\n");
    let registry_link = kond.join("registry/ext");
    symlink(outside.join("registry-sub"), &registry_link).unwrap();

    let db = tmp.0.join("conde.db");
    let report = run(&options(&kond, &db, true)).unwrap();
    let mut errors = report.parse_errors.clone();
    errors.sort();
    assert_eq!(
        errors,
        vec![
            format!("{}: symlink, not followed", channel_link.display()),
            format!("{}: symlink, not followed", registry_link.display()),
        ]
    );
    assert_eq!(report.source, EXPECTED, "only the snapshot's own records");

    // The fixed registry root itself.
    let registry = kond.join("registry");
    fs::rename(&registry, outside.join("registry-root")).unwrap();
    symlink(outside.join("registry-root"), &registry).unwrap();
    let report = run(&options(&kond, &db, true)).unwrap();
    assert!(report.parse_errors.contains(&format!("{}: symlink, not followed", registry.display())), "{:?}", report.parse_errors);
    assert!(!report.to_string().contains("codex:thread-1"), "no registry address read through the link");
}

/// The same import into a Postgres board (CONDE_TEST_PG): dry run on an empty
/// database writes nothing, the import validates clean, the re-run adds nothing.
#[test]
fn imports_into_postgres_when_a_database_url_is_given() {
    let Some(url) = pg::fresh_database() else {
        eprintln!("skipped: set CONDE_TEST_PG to import into Postgres");
        return;
    };
    let tmp = TempDir::new("pg");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let opts = |dry_run| Options { database_url: Some(url.clone()), ..options(&kond, &tmp.0.join("unused.db"), dry_run) };

    let dry = run(&opts(true)).unwrap();
    assert_eq!((dry.source, dry.present, dry.added), (EXPECTED, Counts::default(), EXPECTED), "{dry}");
    assert!(dry.to_string().contains("the Postgres board (DATABASE_URL)"), "{dry}");
    let d = Dispatcher::connect(&url, conde::store::postgres::PgOpen::INSPECT).unwrap();
    assert!(d.sqlite().is_none(), "a Postgres board");
    assert!(d.with_conn(|tx| tx.message_count()).is_err(), "a dry run did not create the schema");

    let report = run(&opts(false)).unwrap();
    assert_clean(&report);
    assert_eq!(report.added, EXPECTED, "{report}");
    let again = run(&opts(false)).unwrap();
    assert_clean(&again);
    assert_eq!(again.added, Counts::default(), "{again}");
    assert_eq!(d.with_conn(|tx| tx.message_count()).unwrap(), EXPECTED.messages as i64);
    assert!(!tmp.0.join("unused.db").exists(), "the SQLite path is not touched");
}

/// A board from before channel topics: the dry run reads it without migrating
/// it, and the import migrates it and lands clean.
#[test]
fn a_pre_topic_board_imports_in_both_modes() {
    let tmp = TempDir::new("pre-topic");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("old.db");
    drop(Dispatcher::open(&db).unwrap());
    let topic_column = |db: &Path| -> i64 {
        q(&Connection::open(db).unwrap(), "SELECT COUNT(*) FROM pragma_table_info('channels') WHERE name = 'topic'", [])
    };
    Connection::open(&db).unwrap().execute_batch("ALTER TABLE channels DROP COLUMN topic").unwrap();
    assert_eq!(topic_column(&db), 0);
    let dry = run(&options(&kond, &db, true)).unwrap_or_else(|e| panic!("dry run: {e:#}"));
    assert_eq!(dry.added, EXPECTED, "{dry}");
    assert_eq!(topic_column(&db), 0, "a dry run does not migrate the target");
    let report = run(&options(&kond, &db, false)).unwrap_or_else(|e| panic!("import: {e:#}"));
    assert_clean(&report);
    assert_eq!(report.added, EXPECTED, "{report}");
    assert_eq!(topic_column(&db), 1, "the import migrated it");
}

/// Every file and directory under `root` with its size and mtime: a snapshot
/// that was only read has the same fingerprint afterwards.
fn fingerprint(root: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let m = fs::symlink_metadata(&p).unwrap();
        out.push((p.clone(), m.len(), m.modified().unwrap()));
        if m.is_dir() {
            stack.extend(fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        }
    }
    out.sort();
    out
}

/// Makes a tree read-only (as a read-only mount) and writable again on drop.
struct ReadOnly(PathBuf);

impl ReadOnly {
    fn set(root: &Path, read_only: bool) {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![root.to_path_buf()];
        let mut dirs = Vec::new();
        while let Some(p) = stack.pop() {
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                stack.extend(fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
                dirs.push(p);
            } else {
                fs::set_permissions(&p, fs::Permissions::from_mode(if read_only { 0o444 } else { 0o644 })).unwrap();
            }
        }
        for d in dirs {
            fs::set_permissions(&d, fs::Permissions::from_mode(if read_only { 0o555 } else { 0o755 })).unwrap();
        }
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        ReadOnly::set(&self.0, false);
    }
}

/// The cutover job: distroless, no USER, HOME or harness markers, the
/// snapshot mounted read-only, Postgres from DATABASE_URL (CONDE_TEST_PG; the
/// SQLite file otherwise). Explicit --owner/--host derive no identity, the
/// dry run writes nothing anywhere, the import lands and validates.
#[test]
fn container_job_imports_a_read_only_snapshot_without_a_login_user() {
    let tmp = TempDir::new("job");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let _restore = ReadOnly(kond.clone());
    ReadOnly::set(&kond, true);
    let before = fingerprint(&kond);
    let cwd = tmp.0.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let data = tmp.0.join("data");
    let url = pg::fresh_database();
    let job = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_conde"));
        cmd.env_clear().current_dir(&cwd).args(["import", "--from"]).arg(&kond).args(args);
        match &url {
            Some(url) => cmd.env("DATABASE_URL", url),
            None => cmd.env("CONDE_DATA", &data),
        };
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let explicit = ["--owner", "kdobmayer", "--host", "homepc"];

    let (ok, _, err) = job(&["--dry-run", "--owner", "kdobmayer"]);
    assert!(!ok && err.contains("USER is not set") && err.contains("--host"), "{err}");

    let (ok, out, err) = job(&[&["--dry-run"][..], &explicit].concat());
    assert!(ok, "{out}{err}");
    assert!(out.contains("(owner kdobmayer, host homepc)") && out.contains("result: OK"), "{out}");
    assert!(out.contains("stale bindings: not checked"), "no sessions registry in the job: {out}");
    assert!(!data.exists(), "the dry run created {}", data.display());
    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0, "the dry run wrote into the working directory");

    let (ok, out, err) = job(&explicit);
    assert!(ok, "{out}{err}");
    assert!(out.contains("result: OK") && out.contains("bodies: 5 of 5 sha256-equal"), "{out}");
    assert!(out.contains("import: committed; validating"), "{out}");
    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0, "the import wrote into the working directory");
    assert_eq!(fingerprint(&kond), before, "the snapshot is only read");
    let (ok, out, _) = job(&explicit);
    assert!(ok && out.contains("result: OK"), "{out}");
}

/// Liveness on stdout for the job log: a line per 1000 message files scanned
/// and per 1000 messages written.
#[test]
fn import_prints_progress_every_1000_messages() {
    let tmp = TempDir::new("progress");
    let kond = tmp.0.join("kond");
    let f = build_fixture(&kond);
    for n in 100..2_500 {
        f.dm(n, "krisz", "lead.claude", &[], "bulk", "inbox/cur");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_conde"))
        .env_clear()
        .env("CONDE_DATA", tmp.0.join("data"))
        .args(["import", "--owner", "tester", "--host", "testhost", "--from"])
        .arg(&kond)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    for line in [
        "import: scanned 1000 message files",
        "import: scanned 4000 message files",
        "import: wrote 1000 of 2405 messages",
        "import: wrote 2000 of 2405 messages",
        "import: committed; validating",
    ] {
        assert!(stdout.contains(line), "missing '{line}' in:\n{stdout}");
    }
    assert!(stdout.find("import: committed").unwrap() < stdout.find("result: OK").unwrap());
}

/// H2 validation is by sha256 of every body and attachment: a stored byte changed after the
/// import, at the same size, is found by the re-run's validation.
#[test]
fn validation_compares_every_body_and_attachment_by_sha256() {
    let tmp = TempDir::new("sha");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    let first = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&first);
    let digest = first.to_string().lines().find(|l| l.contains("bodies digest: ")).map(str::to_string);
    assert!(digest.is_some(), "{first}");
    let conn = Connection::open(&db).unwrap();
    conn.execute("UPDATE messages SET text = 'hellO' || substr(text, 6) WHERE message_id = ?1", [id(1)]).unwrap();
    let mut flipped = BLOB.to_vec();
    flipped[0] ^= 1;
    conn.execute("UPDATE attachments SET bytes = ?1 WHERE message_id = ?2", rusqlite::params![flipped, id(1)]).unwrap();
    drop(conn);
    let again = run(&options(&kond, &db, false)).unwrap();
    assert!(!again.ok(), "{again}");
    let problems = &again.validation.as_ref().unwrap().problems;
    assert!(problems.iter().any(|p| p.starts_with(&format!("body {}: sha256 differs", id(1)))), "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with(&format!("attachment {}/blob.bin: sha256 differs", id(1)))), "{problems:?}");
    assert!(again.to_string().contains("bodies: 4 of 5 sha256-equal"), "{again}");
}

fn file_count(root: &Path) -> usize {
    fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| if e.file_type().unwrap().is_dir() { file_count(&e.path()) } else { 1 })
        .sum()
}

#[test]
fn a_directory_with_another_file_count_than_the_snapshot_is_refused() {
    let tmp = TempDir::new("count");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    let n = file_count(&kond);
    let err = run(&Options { expect_files: Some(n + 1), ..options(&kond, &db, false) }).unwrap_err().to_string();
    assert!(err.contains(&format!("holds {n} files, the snapshot {}", n + 1)), "{err}");
    assert!(!db.exists(), "nothing read, nothing written");
    assert_clean(&run(&Options { expect_files: Some(n), ..options(&kond, &db, false) }).unwrap());
}

/// A channel whose kond posts are all on the board already is not refused for later posts:
/// the re-run adds no post there, so no cursor can pass the later ones.
#[test]
fn a_rerun_after_later_posts_in_an_imported_channel_is_not_refused() {
    let tmp = TempDir::new("later-posts");
    let kond = tmp.0.join("kond");
    build_fixture(&kond);
    let db = tmp.0.join("conde.db");
    assert_clean(&run(&options(&kond, &db, false)).unwrap());
    let (d, p) = native(&db);
    let posted = call(&d, &p, &format!("claude:{LIVE}"), "message.send", json!({"to": {"channel": "#team"}, "text": "later"}));
    let unread = |d: &Dispatcher| {
        call(d, &p, "human:tester", "inbox.list", json!({}))["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["message_id"] == posted["message_id"])
    };
    assert!(unread(&d));
    drop(d);
    let before = table_counts(&db);
    let report = run(&options(&kond, &db, false)).unwrap();
    assert_clean(&report);
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(report.added, Counts::default());
    assert_eq!(table_counts(&db), before);
    let (d, _) = native(&db);
    assert!(unread(&d), "the later post stays unread");
}
