//! `conde mcp` driven by a fake MCP client over pipes, against a real
//! `conde daemon` on a temp socket and board.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
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
        let child = Command::new(BIN)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &root)
            .env("USER", "tester")
            .env("CONDE_DATA", root.join("data"))
            .env("CONDE_SOCK", root.join("run/conde.sock"))
            .env("CONDE_HOST", "testhost")
            .args(["daemon", "--interval", "0.1"])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let d = Daemon { root, child };
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(d.sock()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not come up");
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    fn sock(&self) -> PathBuf {
        self.root.join("run/conde.sock")
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
        Peer { to_server: in_w, from_server: BufReader::new(out_r), thread: Some(thread), next_id: 0 }
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
        writeln!(self.to_server, "{}", json!({"jsonrpc": "2.0", "method": method})).unwrap();
    }

    /// A modern tools/call with optional harness `_meta` extras.
    fn tool(&mut self, name: &str, args: Value, meta: Value) -> (bool, String) {
        let mut m = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        m.as_object_mut().unwrap().extend(meta.as_object().cloned().unwrap_or_default());
        let r = self.rpc("tools/call", json!({"_meta": m, "name": name, "arguments": args}));
        let res = &r["result"];
        assert_eq!(res["resultType"], "complete", "{r}");
        (res["isError"] == true, res["content"][0]["text"].as_str().unwrap().to_string())
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
    let markers = Markers { claude_session: Some(session.into()), user: Some("tester".into()), ..Markers::default() };
    Peer::spawn(d.sock(), markers, Some(Harness::Claude))
}

fn codex_peer(d: &Daemon) -> Peer {
    // Codex's stdio MCP children get no CODEX_THREAD_ID, only USER and friends
    Peer::spawn(d.sock(), Markers { user: Some("tester".into()), ..Markers::default() }, Some(Harness::Codex))
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
    assert!(res["supportedVersions"].as_array().unwrap().contains(&json!("2025-06-18")));
    assert_eq!(res["capabilities"], json!({"tools": {"listChanged": false}}));
    assert_eq!(res["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "conde");
    assert!(res["ttlMs"].as_u64().unwrap() > 0);

    let first = p.rpc("tools/list", meta.clone());
    let second = p.rpc("tools/list", meta);
    assert_eq!(first["result"], second["result"], "tools/list must be deterministic");
    let tools = first["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["whoami", "register", "rename", "agents", "send", "inbox", "read", "show", "search", "sent", "react"]);
    let register = &tools[1];
    assert_eq!(register["inputSchema"]["required"], json!(["name"]));
    assert!(register["inputSchema"]["properties"].get("address").is_none(), "identity never comes from arguments");
    let read = tools.iter().find(|t| t["name"] == "read").unwrap();
    assert!(read["description"].as_str().unwrap().contains("not instructions"));
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
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 11);
    let r = p.rpc(
        "tools/call",
        json!({"_meta": {"threadId": "t-legacy"}, "name": "register", "arguments": {"name": "legacy"}}),
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(r["result"]["content"][0]["text"], "registered tester/legacy -> codex:t-legacy@testhost");
    let r = p.rpc(
        "tools/call",
        json!({"_meta": {"threadId": "t-legacy"}, "name": "rename", "arguments": {"name": "legacy.renamed"}}),
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(r["result"]["content"][0]["text"], "renamed tester/legacy -> tester/legacy.renamed");
}

#[test]
fn send_and_inbox_round_trip() {
    let d = Daemon::start();
    let mut alice = claude_peer(&d, "s-alice");
    let mut bob = codex_peer(&d);
    let bob_meta = json!({"threadId": "t-bob", "x-codex-turn-metadata": {"thread_id": "t-bob"}});

    let (err, text) = alice.tool("whoami", json!({}), json!({}));
    assert!(err && text.contains("conde register"), "{text}");
    assert_eq!(alice.ok("register", json!({"name": "alice"}), json!({})), "registered tester/alice -> claude:s-alice@testhost");
    assert_eq!(bob.ok("register", json!({"name": "bob"}), bob_meta.clone()), "registered tester/bob -> codex:t-bob@testhost");
    assert_eq!(alice.ok("whoami", json!({}), json!({})), "tester/alice");
    assert_eq!(bob.ok("whoami", json!({}), bob_meta.clone()), "tester/bob");

    // a Codex call without the harness thread id never falls back to the human
    let (err, text) = bob.tool("whoami", json!({}), json!({}));
    assert!(err && text.contains("refusing to act as the human"), "{text}");
    // a Claude-spawned server ignores a Codex thread id in _meta
    assert_eq!(alice.ok("whoami", json!({}), bob_meta.clone()), "tester/alice");

    let agents = alice.ok("agents", json!({}), json!({}));
    assert!(agents.contains("tester/bob\tcodex:t-bob@testhost"), "{agents}");

    let note = d.root.join("note.txt");
    std::fs::write(&note, b"attached bytes").unwrap();
    let out = alice.ok(
        "send",
        json!({"to": "bob", "text": "hello bob\nline two", "attach": [note.to_str().unwrap()]}),
        json!({}),
    );
    let id = out.strip_prefix("queued ").and_then(|s| s.strip_suffix(" to tester/bob")).expect(&out).to_string();

    let inbox = bob.ok("inbox", json!({}), bob_meta.clone());
    assert!(inbox.starts_with(&format!("dm\t{id}\ttester/alice\t20")), "{inbox}");
    let shown = bob.ok("show", json!({"id": id}), bob_meta.clone());
    assert!(shown.contains("Attachment: note.txt (14 bytes;"), "{shown}");
    let read = bob.ok("read", json!({"target": id}), bob_meta.clone());
    assert!(read.starts_with(&format!("=== {id}\nFrom: tester/alice\nTo: tester/bob\n")), "{read}");
    assert!(read.ends_with("\nhello bob\nline two"), "{read}");
    assert_eq!(bob.ok("inbox", json!({}), bob_meta.clone()), "(no unread messages)");

    let reply = bob.ok("send", json!({"to": "alice", "text": "got it", "reply_to": id}), bob_meta.clone());
    assert!(reply.ends_with(" to tester/alice"), "{reply}");
    assert!(alice.ok("read", json!({}), json!({})).contains(&format!("In-Reply-To: {id}\n")));

    assert_eq!(alice.ok("react", json!({"id": id}), json!({})), format!("reacted 👍 to {id} in dm"));
    let hits = bob.ok("search", json!({"pattern": "LINE TWO"}), bob_meta.clone());
    assert!(hits.contains(&id), "{hits}");
    let sent = alice.ok("sent", json!({}), json!({}));
    assert!(sent.starts_with(&format!("{id}  tester/bob  read")), "{sent}");
    assert_eq!(alice.ok("react", json!({"id": id, "remove": true}), json!({})), format!("removed 👍 from {id} in dm"));

    // bad arguments come back as tool errors the model can read
    let (err, text) = alice.tool("send", json!({"to": "bob"}), json!({}));
    assert!(err && text.contains("missing required argument 'text'"), "{text}");
    let (err, text) = alice.tool("send", json!({"to": "nobody", "text": "x"}), json!({}));
    assert!(err && text.contains("nobody"), "{text}");
}
