//! `conde mcp` driven by a fake MCP client over pipes, against a real
//! `conde daemon` on a temp socket and board.

mod os;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use conde::identity::{Harness, Markers};
use conde::{mcp, util};

const BIN: &str = env!("CARGO_BIN_EXE_conde");

struct Daemon {
    root: PathBuf,
    child: Child,
}

impl Daemon {
    fn start() -> Daemon {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-{}", util::new_ulid()));
        std::fs::create_dir_all(&root).unwrap();
        let log = std::fs::File::create(root.join("daemon.log")).unwrap();
        let mut cmd = Command::new(BIN);
        os::clear_env(&mut cmd).env("PATH", std::env::var("PATH").unwrap_or_default());
        let child = os::user_home(&mut cmd, "tester", &root)
            .env("CONDE_DATA", root.join("data"))
            .env("CONDE_SOCK", os::socket_in(&root))
            .env("CONDE_HOST", "testhost")
            .args(["daemon", "--interval", "0.1"])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let d = Daemon { root, child };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !os::answers(&d.sock()) {
            assert!(Instant::now() < deadline, "daemon did not come up");
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    fn sock(&self) -> PathBuf {
        os::socket_in(&self.root)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A fake MCP client: one server instance on a thread, wired with OS pipes.
struct Peer {
    to_server: std::io::PipeWriter,
    from_server: BufReader<std::io::PipeReader>,
    thread: Option<std::thread::JoinHandle<()>>,
    next_id: i64,
}

impl Peer {
    fn spawn(sock: PathBuf, markers: Markers, harness: Option<Harness>) -> Peer {
        let (in_r, in_w) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let thread = std::thread::spawn(move || {
            let mut server = mcp::Server::new(sock, mcp::Identity::new(markers, harness));
            server.serve(BufReader::new(in_r), out_w).unwrap();
        });
        Peer {
            to_server: in_w,
            from_server: BufReader::new(out_r),
            thread: Some(thread),
            next_id: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.to_server, "{msg}").unwrap();
        let mut line = String::new();
        self.from_server.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    fn notify(&mut self, method: &str) {
        writeln!(
            self.to_server,
            "{}",
            json!({"jsonrpc": "2.0", "method": method})
        )
        .unwrap();
    }

    /// A modern tools/call with optional harness `_meta` extras.
    fn tool(&mut self, name: &str, args: Value, meta: Value) -> (bool, String) {
        let mut m = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        m.as_object_mut()
            .unwrap()
            .extend(meta.as_object().cloned().unwrap_or_default());
        let r = self.rpc(
            "tools/call",
            json!({"_meta": m, "name": name, "arguments": args}),
        );
        let res = &r["result"];
        assert_eq!(res["resultType"], "complete", "{r}");
        (
            res["isError"] == true,
            res["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    fn ok(&mut self, name: &str, args: Value, meta: Value) -> String {
        let (err, text) = self.tool(name, args, meta);
        assert!(!err, "{name} failed: {text}");
        text
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // closing stdin is the stdio shutdown signal; the server must exit on EOF
        let (_, w) = std::io::pipe().unwrap();
        drop(std::mem::replace(&mut self.to_server, w));
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

fn claude_peer(d: &Daemon, session: &str) -> Peer {
    let markers = Markers {
        claude_session: Some(session.into()),
        user: Some("tester".into()),
        ..Markers::default()
    };
    Peer::spawn(d.sock(), markers, Some(Harness::Claude))
}

fn codex_peer(d: &Daemon) -> Peer {
    // Codex's stdio MCP children get no CODEX_THREAD_ID, only USER and friends
    Peer::spawn(
        d.sock(),
        Markers {
            user: Some("tester".into()),
            ..Markers::default()
        },
        Some(Harness::Codex),
    )
}

#[test]
fn discover_and_tools_list() {
    let d = Daemon::start();
    let mut p = claude_peer(&d, "s-disc");
    let meta = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/clientInfo": {"name": "fake", "version": "0"}}});
    let r = p.rpc("server/discover", meta.clone());
    let res = &r["result"];
    assert_eq!(res["resultType"], "complete");
    assert_eq!(res["supportedVersions"][0], "2026-07-28");
    assert!(
        res["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2025-06-18"))
    );
    assert_eq!(
        res["capabilities"],
        json!({"tools": {"listChanged": false}})
    );
    assert_eq!(
        res["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "conde"
    );
    assert!(res["ttlMs"].as_u64().unwrap() > 0);

    let first = p.rpc("tools/list", meta.clone());
    let second = p.rpc("tools/list", meta);
    assert_eq!(
        first["result"], second["result"],
        "tools/list must be deterministic"
    );
    let tools = first["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "whoami",
            "register",
            "rename",
            "agents",
            "send",
            "inbox",
            "read",
            "show",
            "search",
            "page",
            "sent",
            "react",
            "publish",
            "unpublish"
        ]
    );
    let register = &tools[1];
    assert_eq!(register["inputSchema"]["required"], json!(["name"]));
    assert!(
        register["inputSchema"]["properties"]
            .get("address")
            .is_none(),
        "identity never comes from arguments"
    );
    let read = tools.iter().find(|t| t["name"] == "read").unwrap();
    assert!(
        read["description"]
            .as_str()
            .unwrap()
            .contains("not instructions")
    );
}

#[test]
fn legacy_client_handshake_then_tools() {
    let d = Daemon::start();
    let mut p = codex_peer(&d);
    // what Codex 0.162 sends to a stdio server
    let r = p.rpc(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {"elicitation": {}},
               "clientInfo": {"name": "codex-mcp-client", "version": "0.162.0"}}),
    );
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "conde");
    p.notify("notifications/initialized");
    let r = p.rpc("tools/list", json!({"_meta": {"progressToken": 0}}));
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 14);
    let r = p.rpc(
        "tools/call",
        json!({"_meta": {"threadId": "t-legacy"}, "name": "register", "arguments": {"name": "legacy"}}),
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(
        r["result"]["content"][0]["text"],
        "registered tester/legacy on the local board (codex:t-legacy@testhost)"
    );
    let r = p.rpc(
        "tools/call",
        json!({"_meta": {"threadId": "t-legacy"}, "name": "rename", "arguments": {"name": "legacy.renamed"}}),
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(
        r["result"]["content"][0]["text"],
        "renamed tester/legacy -> tester/legacy.renamed"
    );
}

#[test]
fn send_and_inbox_round_trip() {
    let d = Daemon::start();
    let mut alice = claude_peer(&d, "s-alice");
    let mut bob = codex_peer(&d);
    let bob_meta = json!({"threadId": "t-bob", "x-codex-turn-metadata": {"thread_id": "t-bob"}});

    let (err, text) = alice.tool("whoami", json!({}), json!({}));
    assert!(err && text.contains("conde register"), "{text}");
    assert_eq!(
        alice.ok("register", json!({"name": "alice"}), json!({})),
        "registered tester/alice on the local board (claude:s-alice@testhost)"
    );
    assert_eq!(
        bob.ok("register", json!({"name": "bob"}), bob_meta.clone()),
        "registered tester/bob on the local board (codex:t-bob@testhost)"
    );
    assert_eq!(
        alice.ok("whoami", json!({}), json!({})),
        "tester/alice (local, claude:s-alice@testhost)"
    );
    assert_eq!(
        bob.ok("whoami", json!({}), bob_meta.clone()),
        "tester/bob (local, codex:t-bob@testhost)"
    );

    // a Codex call without the harness thread id never falls back to the human
    let (err, text) = bob.tool("whoami", json!({}), json!({}));
    assert!(
        err && text.contains("refusing to act as the human"),
        "{text}"
    );
    // a Claude-spawned server ignores a Codex thread id in _meta
    assert_eq!(
        alice.ok("whoami", json!({}), bob_meta.clone()),
        "tester/alice (local, claude:s-alice@testhost)"
    );

    let agents = alice.ok("agents", json!({}), json!({}));
    assert!(
        agents.contains("tester/bob\tcodex:t-bob@testhost"),
        "{agents}"
    );

    let note = d.root.join("note.txt");
    std::fs::write(&note, b"attached bytes").unwrap();
    let out = alice.ok(
        "send",
        json!({"to": "bob", "text": "hello bob\nline two", "attach": [note.to_str().unwrap()]}),
        json!({}),
    );
    let id = out
        .strip_prefix("queued ")
        .and_then(|s| s.strip_suffix(" to tester/bob"))
        .expect(&out)
        .to_string();

    let inbox = bob.ok("inbox", json!({}), bob_meta.clone());
    assert!(
        inbox.starts_with(&format!("local\tdm\t{id}\ttester/alice\t20")),
        "{inbox}"
    );
    let shown = bob.ok("show", json!({"id": id}), bob_meta.clone());
    assert!(shown.contains("Attachment: note.txt (14 bytes;"), "{shown}");
    let read = bob.ok("read", json!({"target": id}), bob_meta.clone());
    assert!(
        read.starts_with(&format!("=== {id}\nFrom: tester/alice\nTo: tester/bob\n")),
        "{read}"
    );
    assert!(read.ends_with("\nhello bob\nline two"), "{read}");
    assert_eq!(
        bob.ok("inbox", json!({}), bob_meta.clone()),
        "(no unread messages)"
    );

    let reply = bob.ok(
        "send",
        json!({"to": "alice", "text": "got it", "reply_to": id}),
        bob_meta.clone(),
    );
    assert!(reply.ends_with(" to tester/alice"), "{reply}");
    assert!(
        alice
            .ok("read", json!({}), json!({}))
            .contains(&format!("In-Reply-To: {id}\n"))
    );

    assert_eq!(
        alice.ok("react", json!({"id": id}), json!({})),
        format!("reacted 👍 to {id} in dm")
    );
    let hits = bob.ok("search", json!({"pattern": "LINE TWO"}), bob_meta.clone());
    assert!(hits.contains(&id), "{hits}");
    let sent = alice.ok("sent", json!({}), json!({}));
    assert!(
        sent.starts_with(&format!("local  {id}  tester/bob  read")),
        "{sent}"
    );
    assert_eq!(
        alice.ok("react", json!({"id": id, "remove": true}), json!({})),
        format!("removed 👍 from {id} in dm")
    );

    // bad arguments come back as tool errors the model can read
    let (err, text) = alice.tool("send", json!({"to": "bob"}), json!({}));
    assert!(
        err && text.contains("missing required argument 'text'"),
        "{text}"
    );
    let (err, text) = alice.tool("send", json!({"to": "nobody", "text": "x"}), json!({}));
    assert!(err && text.contains("nobody"), "{text}");
}

// --- v2 surface: MCP against a fake daemon (DESIGN-v2 §3, §7) --------------------------------

#[path = "fake_daemon/mod.rs"]
mod fake_daemon;

/// The tools that take a board, as the CLI commands that take --board.
const BOARD_TOOLS: [&str; 9] = [
    "send", "inbox", "read", "show", "search", "sent", "react", "agents", "page",
];

#[test]
fn board_argument_on_the_board_tools_and_never_an_identity() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |_| Ok(json!({})));
    let mut p = claude_peer_at(d.socket.clone(), "s-tools");
    let meta = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}});
    let r = p.rpc("tools/list", meta);
    for t in r["result"]["tools"].as_array().unwrap() {
        let name = t["name"].as_str().unwrap();
        let props = t["inputSchema"]["properties"].as_object().unwrap();
        let publication = ["publish", "unpublish"].contains(&name);
        assert_eq!(
            props.contains_key("board"),
            BOARD_TOOLS.contains(&name) || publication,
            "{name}: {props:?}"
        );
        for claim in ["address", "from", "actor", "as", "owner", "host", "session"] {
            // publish's `as` is the shared name it publishes under, not who calls
            if publication && claim == "as" {
                continue;
            }
            assert!(
                !props.contains_key(claim),
                "{name} takes an identity claim '{claim}'"
            );
        }
        let required: Vec<&str> = t["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        if publication {
            assert_eq!(
                required,
                ["name", "board"],
                "{name}: a publication always names its shared board"
            );
        } else if let Some(b) = props.get("board") {
            assert!(
                b["description"]
                    .as_str()
                    .unwrap()
                    .contains("a bare id is local"),
                "{name}: {b}"
            );
        }
    }
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn board_rules_through_mcp_match_the_cli() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |req| {
        Ok(match req["op"].as_str().unwrap() {
            "message.send" => {
                json!({"message_id": "01MS", "to": "tester/reviewer", "board": req["board"].as_str().unwrap_or("dakai")})
            }
            "message.get" => {
                json!({"message": {"board": "dakai", "message_id": "01MG", "from": "b/r", "to": "tester/me",
                                                "date": "d", "reply_to": "01MP", "text": "body"}})
            }
            "inbox.list" => json!({"items": [
                {"board": "local", "kind": "dm", "message_id": "01ML", "from": "tester/a", "date": "d", "preview": "p"},
                {"board": "dakai", "kind": "dm", "message_id": "01MD", "from": "b/r", "date": "d", "preview": "q"}],
                "boards": [{"board": "dakai", "available": true}, {"board": "two", "available": false}]}),
            "receipt.list" => {
                json!({"receipts": [{"board": "dakai", "message_id": "01SD", "to": "b/r", "status": "unread", "delivery": "held"}]})
            }
            "search" => json!({"board": "dakai", "next_cursor": 3, "hits": []}),
            "agent.whoami" => json!({"name": "tester/me", "address": "claude:s-me@testhost",
                "publications": [{"board": "dakai", "name": "tester/reviewer", "state": "published"}]}),
            "reaction.set" => json!({"place": "dm"}),
            _ => json!({}),
        })
    });
    let mut p = claude_peer_at(d.socket.clone(), "s-me");

    assert_eq!(
        p.ok(
            "send",
            json!({"to": "reviewer", "text": "hi", "board": "dakai"}),
            json!({})
        ),
        "queued dakai:01MS to tester/reviewer"
    );
    let req = d.take().pop().unwrap();
    assert_eq!(
        (req["op"].as_str(), req["board"].as_str()),
        (Some("message.send"), Some("dakai"))
    );
    assert_eq!(
        req["actor"], "claude:s-me",
        "the actor is derived, never an argument"
    );
    // a reply inherits the board through its qualified id
    p.ok(
        "send",
        json!({"to": "reviewer", "text": "re", "reply_to": "dakai:01MP"}),
        json!({}),
    );
    let req = d.take().pop().unwrap();
    assert!(
        req.get("board").is_none() && req["body"]["reply_to"] == "dakai:01MP",
        "{req}"
    );

    // a bare id is local in MCP as in the CLI
    let (err, text) = p.tool("show", json!({"id": "01MG", "board": "dakai"}), json!({}));
    assert!(
        err && text
            == "conde: 01MG is a local message; drop the board argument, or use the dakai: id",
        "{text}"
    );
    let (err, text) = p.tool(
        "react",
        json!({"id": "dakai:01MG", "board": "local"}),
        json!({}),
    );
    assert!(
        err && text.contains("dakai:01MG is a dakai message"),
        "{text}"
    );
    let (err, text) = p.tool("inbox", json!({"board": 7}), json!({}));
    assert!(
        err && text.contains("argument 'board' must be a string"),
        "{text}"
    );
    assert!(d.take().is_empty(), "refusals never reach the daemon");

    assert!(
        p.ok("show", json!({"id": "dakai:01MG"}), json!({}))
            .starts_with(
                "=== dakai:01MG\nFrom: b/r\nTo: tester/me\nDate: d\nIn-Reply-To: dakai:01MP\n"
            )
    );
    assert_eq!(d.take().pop().unwrap()["body"], json!({"id": "dakai:01MG"}));
    assert_eq!(
        p.ok("react", json!({"id": "dakai:01MG"}), json!({})),
        "reacted 👍 to dakai:01MG in dm"
    );
    assert_eq!(
        p.ok("inbox", json!({}), json!({})),
        "local\tdm\t01ML\ttester/a\td\tp\ndakai\tdm\tdakai:01MD\tb/r\td\tq\n(local: 1 unread; dakai: 1 unread)\n(two: unavailable)"
    );
    assert_eq!(
        p.ok("sent", json!({"board": "dakai"}), json!({})),
        "dakai  dakai:01SD  b/r  unread  delivery: held"
    );
    assert_eq!(
        p.ok(
            "search",
            json!({"pattern": "x", "board": "dakai"}),
            json!({})
        ),
        "(no matches)\n(more: call search with board=\"dakai\" after=3)"
    );
    assert_eq!(d.take().pop().unwrap()["board"], "dakai");
    assert_eq!(
        p.ok("whoami", json!({}), json!({})),
        "tester/me (local, claude:s-me@testhost)\npublished: dakai as tester/reviewer"
    );
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}

/// publish and unpublish mirror the CLI: the same request (alias in the envelope, the
/// derived actor) and the same answers; a publication always names a shared board.
#[test]
fn publish_and_unpublish_mirror_the_cli() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |req| {
        let body = &req["body"];
        Ok(match req["op"].as_str().unwrap() {
            "mailbox.publish" if body["agent"] == "slow" => {
                json!({"state": "publishing", "op_id": "01OP"})
            }
            "mailbox.publish" => {
                json!({"state": "published", "op_id": "01OP", "name": body["name"], "owner": "tester"})
            }
            "mailbox.unpublish" => json!({"state": "unpublished", "op_id": "01OQ"}),
            _ => json!({}),
        })
    });
    let mut p = claude_peer_at(d.socket.clone(), "s-me");
    let publishes = |d: &fake_daemon::FakeDaemon| {
        d.take()
            .into_iter()
            .filter(|q| q["op"] != "board.list")
            .collect::<Vec<_>>()
    };

    assert_eq!(
        p.ok(
            "publish",
            json!({"name": "worker", "board": "dakai", "as": "kw", "take": true}),
            json!({})
        ),
        "published worker on dakai as tester/kw"
    );
    let req = publishes(&d).pop().unwrap();
    assert_eq!(
        (
            req["op"].as_str(),
            req["board"].as_str(),
            req["actor"].as_str()
        ),
        (Some("mailbox.publish"), Some("dakai"), Some("claude:s-me"))
    );
    assert_eq!(
        req["body"],
        json!({"agent": "worker", "name": "kw", "take": true})
    );
    assert_eq!(
        p.ok(
            "publish",
            json!({"name": "slow", "board": "dakai"}),
            json!({})
        ),
        "publishing slow on dakai is pending (op 01OP): dakai did not answer; the daemon completes it when it reconnects, and conde doctor shows it until then"
    );
    assert_eq!(
        publishes(&d).pop().unwrap()["body"],
        json!({"agent": "slow", "name": "slow"})
    );
    assert_eq!(
        p.ok(
            "unpublish",
            json!({"name": "worker", "board": "dakai"}),
            json!({})
        ),
        "unpublished worker from dakai"
    );
    let req = publishes(&d).pop().unwrap();
    assert_eq!(
        (req["op"].as_str(), req["board"].as_str(), &req["body"]),
        (
            Some("mailbox.unpublish"),
            Some("dakai"),
            &json!({"agent": "worker"})
        )
    );

    let (err, text) = p.tool("publish", json!({"name": "worker"}), json!({}));
    assert!(
        err && text
            == "conde: publish needs board=\"<alias>\" (conde boards lists this host's boards)",
        "{text}"
    );
    let (err, text) = p.tool(
        "unpublish",
        json!({"name": "worker", "board": "local"}),
        json!({}),
    );
    assert!(
        err && text.contains("the local board needs no publication"),
        "{text}"
    );
    let (err, text) = p.tool(
        "publish",
        json!({"name": "worker", "board": "dakai", "take": "yes"}),
        json!({}),
    );
    assert!(
        err && text.contains("argument 'take' must be a boolean"),
        "{text}"
    );
    assert!(publishes(&d).is_empty(), "refusals never reach the daemon");
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}

fn claude_peer_at(sock: PathBuf, session: &str) -> Peer {
    let markers = Markers {
        claude_session: Some(session.into()),
        user: Some("tester".into()),
        ..Markers::default()
    };
    Peer::spawn(sock, markers, Some(Harness::Claude))
}

#[test]
fn mcp_refuses_a_board_to_a_daemon_that_predates_boards() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |req| {
        match req["op"].as_str().unwrap() {
            "board.list" => Err(("bad_request", "unknown op 'board.list'".into())),
            _ => Ok(json!({"items": []})),
        }
    });
    let mut p = claude_peer_at(d.socket.clone(), "s-old");
    let (err, text) = p.tool("inbox", json!({"board": "local"}), json!({}));
    assert!(
        err && text.contains("predates boards and would ignore the board argument"),
        "{text}"
    );
    assert!(d.take().iter().all(|q| q["op"] == "board.list"));
    assert_eq!(p.ok("inbox", json!({}), json!({})), "(no unread messages)");
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mcp_renders_unconfirmed_reads_and_per_board_paging_with_its_own_arguments() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |req| {
        Ok(match req["op"].as_str().unwrap() {
            "message.read" => json!({"more": false, "messages": [
                {"board": "dakai", "place": "dm", "message_id": "dakai:01MU", "from": "b/r", "date": "d",
                 "text": "preview", "unconfirmed": true, "recover": "conde read dakai:01MU"}],
                "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "unconfirmed": true}]}),
            "inbox.list" => json!({"items": [], "boards": [{"board": "local", "available": true},
                                                          {"board": "dakai", "available": true, "next_cursor": 200}]}),
            _ => json!({}),
        })
    });
    let mut p = claude_peer_at(d.socket.clone(), "s-u");
    let out = p.ok("read", json!({}), json!({}));
    assert!(
        out.contains("=== dakai:01MU (unconfirmed: preview only)\n"),
        "{out}"
    );
    assert!(
        out.contains("The whole message: call read with target=\"dakai:01MU\""),
        "{out}"
    );
    let out = p.ok("inbox", json!({}), json!({}));
    assert!(
        out.contains("(more on dakai: call inbox with board=\"dakai\" after=200)"),
        "{out}"
    );
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mcp_sent_names_each_boards_continuation() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |_| {
        Ok(
            json!({"receipts": [], "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "next_cursor": 200}]}),
        )
    });
    let mut p = claude_peer_at(d.socket.clone(), "s-sent");
    let out = p.ok("sent", json!({}), json!({}));
    assert!(
        out.contains("(more on dakai: call sent with board=\"dakai\" after=200)"),
        "{out}"
    );
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}

/// The search tool's `after` takes the CLI's forms: a cursor or a time.
#[test]
fn search_after_takes_a_cursor_or_a_time() {
    let d = Daemon::start();
    let mut alice = claude_peer(&d, "s-alice");
    let mut bob = claude_peer(&d, "s-bob");
    alice.ok("register", json!({"name": "alice"}), json!({}));
    bob.ok("register", json!({"name": "bob"}), json!({}));
    let out = alice.ok(
        "send",
        json!({"to": "bob", "text": "needle one"}),
        json!({}),
    );
    let id = out
        .strip_prefix("queued ")
        .and_then(|s| s.split(' ').next())
        .expect(&out)
        .to_string();
    let shown = bob.ok("show", json!({"id": id}), json!({}));
    let date = shown
        .lines()
        .find_map(|l| l.strip_prefix("Date: "))
        .expect(&shown)
        .to_string();
    // the second message is stamped at a later millisecond
    std::thread::sleep(Duration::from_millis(5));
    alice.ok(
        "send",
        json!({"to": "bob", "text": "needle two"}),
        json!({}),
    );

    let mut hits = |after: Value| -> Vec<String> {
        let out = bob.ok(
            "search",
            json!({"pattern": "needle", "after": after}),
            json!({}),
        );
        out.lines()
            .filter(|l| l.contains("needle"))
            .map(|l| l.rsplit('\t').next().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        hits(json!(date)),
        ["needle two"],
        "conde's own date ({date})"
    );
    assert_eq!(
        hits(json!(date.replace('Z', "+00:00"))),
        ["needle two"],
        "an RFC 3339 offset"
    );
    assert_eq!(hits(json!(0)), ["needle one", "needle two"], "a cursor");
    assert_eq!(
        hits(json!("0")),
        ["needle one", "needle two"],
        "a cursor as text"
    );
    assert!(hits(json!(1_000_000)).is_empty(), "a cursor past the end");

    let (err, text) = bob.tool(
        "search",
        json!({"pattern": "needle", "after": "yesterday"}),
        json!({}),
    );
    assert!(
        err && text.contains("RFC 3339") && text.contains("cursor"),
        "{text}"
    );
    let meta = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}});
    let tools = bob.rpc("tools/list", meta);
    let search = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "search")
        .unwrap()
        .clone();
    let after = &search["inputSchema"]["properties"]["after"];
    assert!(
        after["description"].as_str().unwrap().contains("RFC 3339"),
        "{after}"
    );
}

/// The `page` tool sends `human.page` as the session, with the kind from `done`, and shows the
/// board's answer; the instructions carry the paging rule.
#[test]
fn the_page_tool_pages_the_sessions_own_human() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mcp-fake-{}", util::new_ulid()));
    let d = fake_daemon::FakeDaemon::start(&os::socket_in(&root), |req| {
        Ok(match req["body"]["kind"].as_str() {
            Some("done") => {
                json!({"to": "tester/human", "status": "not paged", "reason": "done pages are off", "board": "dakai"})
            }
            _ => {
                json!({"message_id": "dakai:01MP", "to": "tester/human", "status": "paged", "board": "dakai"})
            }
        })
    });
    let mut p = claude_peer_at(d.socket.clone(), "s-me");
    let init = p.rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
    let instructions = init["result"]["instructions"].as_str().unwrap();
    for rule in [
        "whoami names your human, whether the board has their Slack member id, and whether done pages are on",
        "Page your human (the page tool) only when you are blocked on them: a decision, a go, a blocker; say what you need in one line",
        "Page with done=true only when done pages are on",
        "Never message a human name; other people are reached through their agents",
    ] {
        assert!(instructions.contains(rule), "{rule}");
    }
    assert_eq!(
        p.ok("page", json!({"text": "need a go"}), json!({})),
        "paged: queued for Slack; conde sent shows whether it went (dakai:01MP to tester/human)"
    );
    assert_eq!(
        p.ok(
            "page",
            json!({"text": "done", "done": true, "board": "dakai"}),
            json!({})
        ),
        "not paged: done pages are off"
    );
    // a board argument first probes the daemon (board.list), as for every board tool
    let reqs: Vec<Value> = d
        .take()
        .into_iter()
        .filter(|r| r["op"] != "board.list")
        .collect();
    let sent: Vec<(Value, Value, Value)> = reqs
        .iter()
        .map(|r| (r["op"].clone(), r["body"].clone(), r["actor"].clone()))
        .collect();
    assert_eq!(
        sent,
        [
            (
                json!("human.page"),
                json!({"text": "need a go", "kind": "blocked"}),
                json!("claude:s-me")
            ),
            (
                json!("human.page"),
                json!({"text": "done", "kind": "done"}),
                json!("claude:s-me")
            ),
        ]
    );
    assert_eq!(reqs[1]["board"], "dakai");
    drop(p);
    let _ = std::fs::remove_dir_all(&root);
}
