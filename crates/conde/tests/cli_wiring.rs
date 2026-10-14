//! The installed binary's `conde mcp` and `conde setup`, as a harness and a
//! teammate run them: real processes, temp HOME and socket, no real config.
#![cfg(unix)] // drives the daemon over a unix socket

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_conde");

/// `conde 0.1.0 (1281f26 2026-10-10)`, or `(unknown)` for a build without git.
fn is_build_line(line: &str) -> bool {
    regex::Regex::new(r"^conde \d+\.\d+\.\d+ \(([0-9a-f]{7} \d{4}-\d{2}-\d{2}|unknown)\)$").unwrap().is_match(line)
}

#[test]
fn version_names_the_build() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(is_build_line(text.trim_end_matches('\n')), "{text:?}");
    assert_eq!(text, format!("conde {}\n", conde::VERSION));
    for bad in ["conde 0.1.0", "conde 0.1.0 (1281f2 2026-10-10)", "conde 0.1.0 (1281f26)"] {
        assert!(!is_build_line(bad), "{bad}");
    }
}

struct Root {
    root: PathBuf,
    daemon: Option<Child>,
}

impl Root {
    fn new() -> Root {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("wiring-{}", conde::util::new_ulid()));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", root.join("bin/codex")).unwrap();
        Root { root, daemon: None }
    }

    /// A clean environment: temp HOME and socket, no harness markers, and a
    /// PATH without systemctl, launchctl or claude.
    fn cmd(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.env_clear()
            .env("PATH", self.root.join("emptybin"))
            .env("HOME", self.root.join("home"))
            .env("USER", "tester")
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("CONDE_DATA", self.root.join("data"))
            .env("CONDE_SOCK", self.root.join("run/conde.sock"))
            .env("CONDE_HOST", "testhost")
            .stdin(Stdio::null());
        c
    }

    fn start_daemon(&mut self) {
        let child = self.cmd(Path::new(BIN)).args(["daemon", "--interval", "0.1"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        self.daemon = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(self.root.join("run/conde.sock")).is_err() {
            assert!(Instant::now() < deadline, "daemon did not come up");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn setup(&self, args: &[&str]) -> (bool, String) {
        let out = self.cmd(Path::new(BIN)).arg("setup").args(args).output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap();
        (out.status.success(), text)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        if let Some(mut c) = self.daemon.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn conde_mcp_under_codex_registers_the_calling_thread() {
    let mut r = Root::new();
    r.start_daemon();
    // `codex` (a shell) is the MCP server's parent, as when Codex spawns it
    let mut child = r
        .cmd(&r.root.join("bin/codex"))
        .args(["-c", "\"$0\" mcp; :", BIN])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut rpc = |msg: Value| -> Value {
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = rpc(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "codex-mcp-client", "version": "0"}}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "conde");
    let call = |name: &str, args: Value, meta: Value| {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"_meta": meta, "name": name, "arguments": args}})
    };
    let r1 = rpc(call("register", json!({"name": "wired"}), json!({"threadId": "t-wired"})));
    assert_eq!(r1["result"]["content"][0]["text"], "registered tester/wired on the local board (codex:t-wired@testhost)", "{r1}");
    let r2 = rpc(call("whoami", json!({}), json!({"x-codex-turn-metadata": {"thread_id": "t-wired"}})));
    assert_eq!(r2["result"]["isError"], true, "only the stamped threadId counts: {r2}");
    let r3 = rpc(call("whoami", json!({}), json!({})));
    let text = r3["result"]["content"][0]["text"].as_str().unwrap();
    assert!(r3["result"]["isError"] == true && text.contains("refusing to act as the human"), "{r3}");
    drop(stdin);
    assert!(child.wait().unwrap().success(), "conde mcp exits on EOF");
}

#[test]
fn conde_setup_runs_the_single_daemon_and_enrolls_nothing() {
    let r = Root::new();
    let home = r.root.join("home");

    // not enrolled: the local board, every other part still done, enrollment named as its own step
    let (ok, out) = r.setup(&[]);
    assert!(ok, "{out}");
    assert!(out.contains("service: systemctl not found"), "{out}");
    // neither harness on PATH nor its directory: both skipped as --check skips them
    assert!(out.contains("skip    claude: Claude Code not found on this machine; rerun conde setup after installing it\n"), "{out}");
    assert!(out.contains("skip    codex: Codex not found on this machine; rerun conde setup after installing it\n"), "{out}");
    assert!(!home.join(".claude").exists() && !home.join(".claude.json").exists() && !home.join(".codex").exists());
    assert!(out.contains("boards: no shared board enrolled; this host works on its local board. Joining a team's board is a separate step: conde login --server"), "{out}");
    assert!(out.contains("Enrolling publishes nothing and uploads no history"), "{out}");
    let unit = std::fs::read_to_string(home.join(".config/systemd/user/conde.service")).unwrap();
    assert!(unit.contains(" daemon\n"), "{unit}");
    assert!(!home.join(".config/conde/boards").exists(), "setup enrolls nothing");
    // ~/.codex or ~/.claude without a binary on PATH: installed elsewhere
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let (ok, out) = r.setup(&["--codex"]);
    assert!(ok && out.contains("codex: added MCP server conde to ~/.codex/config.toml"), "{out}");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let (ok, out) = r.setup(&["--claude"]);
    assert!(ok, "{out}");
    assert!(out.contains("claude: claude CLI not found; wrote the user-scope MCP entry conde into ~/.claude.json directly"), "{out}");
    assert!(out.contains("claude: set crossSessionInbound to \"accept\" in ~/.claude/settings.json"), "{out}");

    // a v1 credential whose board does not answer stays where it is; the unit never names a server
    let cred = home.join(".config/conde/credentials");
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    std::fs::write(&cred, r#"{"server":"http://127.0.0.1:9","host_id":"testhost","key":"k"}"#).unwrap();
    std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (ok, out) = r.setup(&["--service"]);
    assert!(ok, "{out}");
    assert!(out.contains("boards: the v1 credential ~/.config/conde/credentials stays where it is: http://127.0.0.1:9 did not answer board.info"), "{out}");
    assert!(out.contains("service: unchanged ~/.config/systemd/user/conde.service"), "{out}");
    assert!(cred.exists());

    // a credential readable by others is refused, nothing is moved
    std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (ok, out) = r.setup(&["--service"]);
    assert!(!ok && out.contains("mode 644"), "{out}");

    // --yes is still accepted (a no-op); the first run already set accept
    let (ok, out) = r.setup(&["--claude", "--yes"]);
    assert!(ok, "{out}");
    assert!(out.contains("claude: crossSessionInbound already \"accept\""), "{out}");
    assert!(!out.contains("service:") && !out.contains("codex:") && !out.contains("boards:"), "{out}");

    // the default board is the human's choice among enrolled boards; --board is not setup's
    let (ok, out) = r.setup(&["--default-board", "dakai"]);
    assert!(!ok && out.contains("no board 'dakai' is enrolled on this host"), "{out}");
    let out = r.cmd(Path::new(BIN)).args(["setup", "--board", "dakai"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("--board does not apply to conde setup; the host's default board is conde setup --default-board <alias>"));
}

/// systemd starts the unit with only HOME and USER from the user manager plus the
/// unit's own Environment= lines: the daemon must still find a relocated config directory.
#[test]
fn the_generated_unit_runs_the_daemon_with_the_relocated_config() {
    let r = Root::new();
    let xdg = r.root.join("elsewhere/config");
    let out = r.cmd(Path::new(BIN)).env("XDG_CONFIG_HOME", &xdg).args(["setup", "--service"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let unit = std::fs::read_to_string(xdg.join("systemd/user/conde.service")).unwrap();
    assert!(unit.contains(&format!("\nEnvironment=\"XDG_CONFIG_HOME={}\"\n", xdg.display())), "{unit}");

    let exec: Vec<&str> = unit.lines().find_map(|l| l.strip_prefix("ExecStart=")).unwrap().split(' ').collect();
    let mut daemon = Command::new(exec[0]);
    daemon.args(&exec[1..]).env_clear().env("HOME", r.root.join("home")).env("USER", "tester");
    for line in unit.lines().filter_map(|l| l.strip_prefix("Environment=\"")) {
        let (k, v) = line.trim_end_matches('"').split_once('=').unwrap();
        daemon.env(k, v);
    }
    assert!(exec.ends_with(&["daemon"]), "one daemon shape, no --server: {exec:?}");
    let log = std::fs::File::create(r.root.join("unit-daemon.log")).unwrap();
    let mut child = daemon.stdout(Stdio::null()).stderr(log).spawn().unwrap();
    let sock = r.root.join("run/conde.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sock.exists() {
        let log = std::fs::read_to_string(r.root.join("unit-daemon.log")).unwrap_or_default();
        assert!(child.try_wait().unwrap().is_none() && Instant::now() < deadline, "daemon under the unit env failed:\n{log}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn setup_check_asks_the_live_daemon() {
    let mut r = Root::new();
    let out = r.cmd(Path::new(BIN)).args(["setup", "--check", "--service"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("ISSUE   service: the bridge does not answer on"), "{text}");
    r.start_daemon();
    let out = r.cmd(Path::new(BIN)).args(["setup", "--check", "--service"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("ok      service: the bridge answers on"), "{text}");
}

#[test]
fn lobby_auto_join_and_channel_topics() {
    let mut r = Root::new();
    r.start_daemon();
    // an agent shell: the binary beneath a process named codex, with its thread marker
    let agent = |thread: &str, args: &[&str]| {
        let out = r
            .cmd(&r.root.join("bin/codex"))
            .env("CODEX_THREAD_ID", thread)
            .args(["-c", "\"$0\" \"$@\"; rc=$?; :; exit $rc", BIN])
            .args(args)
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap())
    };
    // desk's stand-in opens #lobby before anyone else registers
    assert_eq!(agent("t-d", &["register", "desk"]).1, "registered tester/desk on the local board (codex:t-d@testhost)\n");
    assert_eq!(agent("t-d", &["create", "#lobby"]), (true, "created #lobby (you are a member)\n".into()));
    assert_eq!(
        agent("t-d", &["topic", "#lobby", "questions about conde; the welcome from desk"]),
        (true, "topic of #lobby: questions about conde; the welcome from desk\n".into())
    );
    // a first registration joins it, a repeated one does not
    let (ok, out) = agent("t-a", &["register", "alice"]);
    assert!(ok, "{out}");
    assert_eq!(out, "registered tester/alice on the local board (codex:t-a@testhost)\njoined #lobby\n");
    let (_, out) = agent("t-a", &["register", "alice"]);
    assert_eq!(out, "registered tester/alice on the local board (codex:t-a@testhost)\n", "only the first registration joins");

    // topics: set by the creator, shown by channels and once on join, cleared by empty text
    assert!(agent("t-a", &["create", "#dev"]).0);
    assert_eq!(agent("t-a", &["topic", "#dev", "build talk"]), (true, "topic of #dev: build talk\n".into()));
    let (_, out) = agent("t-a", &["channels"]);
    assert!(out.contains("#dev: build talk\t1 members\t"), "{out}");
    assert!(out.contains("#lobby: questions about conde; the welcome from desk\t"), "{out}");
    assert!(agent("t-b", &["register", "bob"]).0);
    let (ok, out) = agent("t-b", &["join", "#dev"]);
    assert!(ok && out.starts_with("joined #dev (join post ") && out.ends_with(")\ntopic: build talk\n"), "{out}");
    assert_eq!(agent("t-b", &["join", "#dev"]).1, "already a member of #dev\n");
    let (ok, out) = agent("t-b", &["topic", "#dev", "mine now"]);
    assert!(!ok && out.contains("only the creator of #dev or a human sets its topic"), "{out}");
    assert_eq!(agent("t-a", &["topic", "#dev", ""]), (true, "cleared the topic of #dev\n".into()));
    let (_, out) = agent("t-a", &["channels"]);
    assert!(out.contains("#dev\t2 members\t"), "{out}");

    // rename keeps the session and its memberships under the new name
    assert_eq!(agent("t-b", &["rename", "bob.renamed"]), (true, "renamed tester/bob -> tester/bob.renamed\n".into()));
    let (_, out) = agent("t-a", &["members", "#dev"]);
    assert!(out.contains("tester/bob.renamed"), "{out}");
    let (ok, out) = agent("t-a", &["rename", "bob", "x"]);
    assert!(!ok && out.contains("no agent named 'bob'"), "{out}");
}

// --- v2 surface: the CLI against a fake daemon (DESIGN-v2 §3, §7) -------------------------

#[path = "fake_daemon/mod.rs"]
mod fake_daemon;

use fake_daemon::{Answer, FakeDaemon};

impl Root {
    /// A shared board enrolled as `alias` (the file the daemon and `conde boards` read).
    fn enroll_file(&self, alias: &str, server: &str) {
        let dir = self.root.join("home/.config/conde/boards");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join(format!("{alias}.json"));
        std::fs::write(&path, json!({"server": server, "host_id": "testhost", "owner": "tester", "key": "k"}).to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn fake(&self, respond: impl Fn(&Value) -> Answer + Send + Sync + 'static) -> FakeDaemon {
        FakeDaemon::start(&self.root.join("run/conde.sock"), respond)
    }

    /// `conde <args>`: (exit ok, stdout, stderr).
    fn conde(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(Path::new(BIN)).args(args).output().unwrap();
        (out.status.success(), String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
    }
}

/// Answers every op with `{}` except the scripted ones.
fn scripted(script: Vec<(&'static str, Value)>) -> impl Fn(&Value) -> Answer + Send + Sync + 'static {
    move |req: &Value| {
        let op = req["op"].as_str().unwrap_or_default();
        Ok(script.iter().find(|(o, _)| *o == op).map(|(_, v)| v.clone()).unwrap_or_else(|| json!({})))
    }
}

#[test]
fn board_flag_travels_in_the_envelope_and_references_verbatim() {
    let r = Root::new();
    r.enroll_file("dakai", "https://conde.test");
    let d = r.fake(|req| {
        let board = req["board"].as_str().map(str::to_string);
        Ok(match req["op"].as_str().unwrap() {
            // a result resolved to a shared board carries its label; ids come back bare
            "message.send" => json!({"message_id": "01MSENT", "to": "tester/reviewer", "board": board.unwrap_or("dakai".into())}),
            _ => json!({}),
        })
    });

    let (ok, out, err) = r.conde(&["send", "reviewer", "hi", "--board", "dakai"]);
    assert!(ok, "{err}");
    assert_eq!(out, "queued dakai:01MSENT to tester/reviewer\n");
    let reqs = d.take();
    assert_eq!(reqs[0]["op"], "board.list", "a named board first checks the daemon knows boards");
    let req = reqs.last().unwrap().clone();
    assert_eq!((req["op"].as_str(), req["board"].as_str()), (Some("message.send"), Some("dakai")), "{req}");
    assert_eq!(req["body"]["to"], json!({"agent": "reviewer"}));

    // a reply inherits its conversation's board through the qualified id, with no flag
    let (ok, out, _) = r.conde(&["send", "#dev", "re", "--reply-to", "dakai:01MA"]);
    assert!(ok);
    assert_eq!(out, "queued dakai:01MSENT to tester/reviewer\n");
    let req = d.take().pop().unwrap();
    assert!(req.get("board").is_none(), "no flag, no envelope board: {req}");
    assert_eq!(req["body"]["reply_to"], "dakai:01MA");

    // every board-aware command carries the flag
    for args in [
        &["inbox", "--board", "local"][..],
        &["sent", "--board", "dakai"],
        &["search", "x", "--board", "dakai"],
        &["channels", "--board", "dakai"],
        &["join", "#dev", "--board", "dakai"],
        &["leave", "#dev", "--board", "dakai"],
        &["agents", "--board", "dakai"],
        &["read", "#dev", "--board", "dakai"],
        &["members", "#dev", "--board", "dakai"],
        &["create", "#dev", "--board", "dakai"],
        &["whoami", "--board", "dakai"],
    ] {
        let (ok, _, err) = r.conde(args);
        assert!(ok, "{args:?}: {err}");
        let reqs: Vec<Value> = d.take().into_iter().filter(|r| r["op"] != "board.list").collect();
        assert!(!reqs.is_empty(), "{args:?} sent nothing");
        for req in reqs {
            assert_eq!(req["board"].as_str(), Some(args[args.len() - 1]), "{args:?}: {req}");
        }
    }

    // qualified references travel verbatim; `local:` is the bare id
    assert!(r.conde(&["read", "dakai:01MA"]).0);
    assert_eq!(d.take().pop().unwrap()["body"], json!({"id": "dakai:01MA"}));
    assert!(r.conde(&["show", "local:01MA"]).0);
    assert_eq!(d.take().pop().unwrap()["body"], json!({"id": "01MA"}));
    assert!(r.conde(&["attachment", "dakai:01AT", "--out", "/nonexistent"]).2.contains("attachment"));
    assert_eq!(d.take().pop().unwrap()["body"], json!({"attachment_id": "dakai:01AT"}));

    // rule 2: a reference and a flag naming different boards is refused before the daemon
    for (args, want) in [
        (&["show", "01MA", "--board", "dakai"][..], "01MA is a local message; drop --board, or use the dakai: id"),
        (&["read", "01MA", "--board", "dakai"], "01MA is a local message; drop --board, or use the dakai: id"),
        (&["react", "dakai:01MA", "--board", "local"], "dakai:01MA is a dakai message; drop --board, or use --board dakai"),
        (&["send", "x", "t", "--reply-to", "01MA", "--board", "dakai"], "01MA is a local message; drop --board, or use the dakai: id"),
        (&["register", "worker", "--board", "dakai"], "register is local; a shared address is conde publish worker --board dakai"),
        (&["rename", "worker", "--board", "dakai"], "rename is local; a shared address is conde publish worker --board dakai"),
        (&["pin", "worker", "claude:x", "--board", "dakai"], "pin is local; a shared address is conde publish worker --board dakai"),
        (&["inbox", "--board", "Not An Alias"], "'Not An Alias' is not a board alias"),
    ] {
        let (ok, _, err) = r.conde(args);
        assert!(!ok && err.contains(want), "{args:?}: {err}");
        assert!(d.take().is_empty(), "{args:?} reached the daemon");
    }
    // `--board local` on a local-only command is the command itself
    assert!(r.conde(&["register", "worker", "--board", "local"]).0);
    assert_eq!(d.take().pop().unwrap()["op"], "agent.register");
    // a daemon refusal is printed as the daemon words it
    let d2 = {
        drop(d);
        r.fake(|_| Err(("forbidden", "worker is not published on dakai. Your human decides what is published; when told: conde publish worker --board dakai".into())))
    };
    let (ok, _, err) = r.conde(&["send", "reviewer", "hi", "--board", "dakai"]);
    assert!(!ok);
    assert_eq!(err, "conde: worker is not published on dakai. Your human decides what is published; when told: conde publish worker --board dakai\n");
    drop(d2);
}

#[test]
fn listings_carry_their_board_and_aggregates_name_unavailable_boards() {
    let r = Root::new();
    let _d = r.fake(scripted(vec![
        ("inbox.list", json!({"items": [
            {"board": "local", "kind": "dm", "message_id": "01ML", "from": "tester/reviewer", "date": "2026-10-09", "preview": "rebase done"},
            {"board": "dakai", "kind": "dm", "message_id": "01MD", "from": "balazs/relay", "date": "2026-10-09", "preview": "setup feedback"}],
            "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true}, {"board": "two", "available": false}]})),
        ("receipt.list", json!({"receipts": [
            {"board": "local", "message_id": "01SL", "to": "tester/reviewer", "status": "read 2026", "delivery": "read"},
            {"board": "dakai", "message_id": "01SD", "to": "balazs/relay", "status": "unread", "delivery": "held"}],
            "boards": [{"board": "dakai", "available": true}]})),
        ("message.read", json!({"messages": [
            {"board": "dakai", "place": "#dev", "message_id": "01RD", "from": "balazs/relay", "to": "#dev", "date": "d",
             "reply_to": "01RA", "text": "hello", "attachments": [{"filename": "a.txt", "size": 3, "attachment_id": "01AT"}]},
            {"board": "local", "place": "dm", "message_id": "01RL", "from": "tester/reviewer", "to": "tester/me", "date": "d", "text": "local one"}],
            "boards": [{"board": "two", "available": false}]})),
        ("agent.list", json!({"agents": [
            {"board": "dakai", "name": "balazs/relay", "kind": "mailbox", "host": "bpc", "available": true},
            {"name": "tester/reviewer", "address": "claude:s1@testhost"}]})),
        ("channel.list", json!({"channels": [{"board": "dakai", "channel": "#dev", "members": 3, "messages": 9}]})),
        ("search", json!({"hits": [{"board": "dakai", "place": "#dev", "message_id": "01H", "from": "b/r", "date": "2026-10-09T10:00:00Z", "line": "x"}]})),
    ]));
    let (ok, out, err) = r.conde(&["inbox"]);
    assert!(ok, "{err}");
    assert_eq!(out, "local\tdm\t01ML\ttester/reviewer\t2026-10-09\trebase done\ndakai\tdm\tdakai:01MD\tbalazs/relay\t2026-10-09\tsetup feedback\n");
    assert_eq!(err, "(local: 1 unread; dakai: 1 unread)\n(two: unavailable)\n");
    let (_, out, err) = r.conde(&["sent"]);
    assert_eq!(out, "local  01SL  tester/reviewer  read 2026  delivery: read\ndakai  dakai:01SD  balazs/relay  unread  delivery: held\n");
    assert_eq!(err, "");
    let (_, out, err) = r.conde(&["read"]);
    assert!(out.starts_with("--- dakai #dev\n=== dakai:01RD\nFrom: balazs/relay\nTo: #dev\nDate: d\nIn-Reply-To: dakai:01RA\n\
                             Attachment: a.txt (3 bytes; conde attachment dakai:01AT)\n\nhello\n=== 01RL\n"), "{out}");
    assert_eq!(err, "(two: unavailable)\n");
    // an unlabelled row is local, whatever board the request named
    let (_, out, _) = r.conde(&["agents", "--board", "dakai"]);
    assert_eq!(out, "dakai\tbalazs/relay\tmailbox on bpc\tavailable\nlocal\ttester/reviewer\tclaude:s1@testhost\n");
    let (_, out, _) = r.conde(&["channels", "--board", "dakai"]);
    assert_eq!(out, "dakai\t#dev\t3 members\t9 messages\n");
    let (_, out, _) = r.conde(&["search", "x"]);
    assert_eq!(out, "dakai\t#dev\tdakai:01H\tb/r\t2026-10-09T10:00\tx\n");
}

#[test]
fn publish_unpublish_boards_whoami_and_doctor() {
    let r = Root::new();
    r.enroll_file("dakai", "https://conde.test");
    std::fs::write(r.root.join("home/.config/conde/default-board"), "dakai\n").unwrap();
    let d = r.fake(|req| {
        let body = &req["body"];
        Ok(match req["op"].as_str().unwrap() {
            "mailbox.publish" if body["agent"] == "slow" => json!({"state": "publishing", "op_id": "01OP"}),
            "mailbox.publish" => json!({"state": "published", "op_id": "01OP", "name": body["name"], "mailbox_id": "01MB", "route_generation": 1}),
            "mailbox.unpublish" => json!({"state": "unpublished", "op_id": "01OQ"}),
            "board.list" => json!({"boards": [
                {"board": "local", "agents": 12},
                {"board": "dakai", "state": "connected", "published": [{"agent": "desk", "name": "tester/desk", "state": "published"},
                                                                       {"agent": "worker", "name": "tester/kw", "state": "publishing"}]}]}),
            "board.forget" => json!({"board": "dakai", "publications_removed": 2, "ops_removed": 0}),
            "agent.whoami" => json!({"name": "tester/worker", "address": "claude:s1@testhost",
                "publications": [{"board": "dakai", "name": "tester/kw", "state": "published"}],
                "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true}]}),
            "agent.register" => json!({"name": "tester/worker", "address": "claude:s1@testhost", "publications": [], "boards": ["dakai"]}),
            "doctor" => json!({"checks": [{"ok": true, "text": "local board: conde.db (integrity ok)"},
                                          {"ok": true, "text": "slots: dakai 256/256 used (queue 32/32, outcomes 0, book 0)"}], "issues": 0,
                "origins": [{"origin": "local", "slots_used": 0, "slots": 256}, {"origin": "dakai", "slots_used": 256, "slots": 256, "queued": 32}],
                "build": conde::VERSION}),
            _ => json!({}),
        })
    });

    let (ok, out, err) = r.conde(&["publish", "worker", "--board", "dakai", "--as", "kw", "--take"]);
    assert!(ok, "{err}");
    assert_eq!(out, "published worker on dakai as tester/kw\n");
    let req = d.take().pop().unwrap();
    assert_eq!((req["op"].as_str(), req["board"].as_str()), (Some("mailbox.publish"), Some("dakai")));
    assert_eq!(req["body"], json!({"agent": "worker", "name": "kw", "take": true}));
    // the shared name defaults to the local name; a pending op is reported as pending
    let (ok, out, _) = r.conde(&["publish", "slow", "--board", "dakai"]);
    assert!(ok);
    assert_eq!(d.take().pop().unwrap()["body"], json!({"agent": "slow", "name": "slow"}));
    assert_eq!(out, "publishing slow on dakai is pending (op 01OP): dakai did not answer; the daemon completes it when it reconnects, and conde doctor shows it until then\n");
    // publication always names its board; the default board does not apply
    let (ok, _, err) = r.conde(&["publish", "worker"]);
    assert!(!ok && err.contains("publish needs --board <alias>"), "{err}");
    let (ok, _, err) = r.conde(&["publish", "worker", "--board", "local"]);
    assert!(!ok && err.contains("the local board needs no publication"), "{err}");
    assert!(d.take().is_empty());

    let (ok, out, _) = r.conde(&["unpublish", "worker", "--board", "dakai"]);
    assert!(ok);
    assert_eq!(out, "unpublished worker from dakai\n");
    let req = d.take().pop().unwrap();
    assert_eq!((req["op"].as_str(), req["board"].as_str(), &req["body"]), (Some("mailbox.unpublish"), Some("dakai"), &json!({"agent": "worker"})));

    let (ok, out, err) = r.conde(&["boards"]);
    assert!(ok, "{err}");
    let data = r.root.join("data/conde.db");
    assert_eq!(
        out,
        format!(
            "local\t{}\t12 agents\ndakai\thttps://conde.test\thost testhost\tconnected\tpublished: desk, worker as tester/kw (publishing)\tdefault\n",
            data.display()
        )
    );
    let (ok, out, _) = r.conde(&["boards", "--forget", "dakai"]);
    assert!(ok);
    assert_eq!(out, "forgot dakai: 2 publications and 0 operation records removed on this host; nothing changed on the server\n");
    let req = d.take().pop().unwrap();
    assert_eq!((req["op"].as_str(), req["board"].as_str()), (Some("board.forget"), Some("dakai")));

    let (_, out, _) = r.conde(&["whoami"]);
    assert_eq!(out, "tester/worker (local, claude:s1@testhost)\npublished: dakai as tester/kw\n");
    let (_, out, _) = r.conde(&["register", "worker"]);
    assert_eq!(
        out,
        "registered tester/worker on the local board (claude:s1@testhost)\n\
         not published; teammates on dakai cannot reach it (your human: conde publish worker --board dakai)\n"
    );
    let (ok, out, _) = r.conde(&["doctor"]);
    assert!(ok, "{out}");
    assert!(is_build_line(out.lines().next().unwrap()), "{out}");
    assert!(out.contains(&format!("ok      bridge: running this build ({})\n", conde::VERSION)), "{out}");
    // the daemon's own checks carry the slots per origin; `origins` is data, not printed twice
    assert!(out.contains("ok      slots: dakai 256/256 used (queue 32/32, outcomes 0, book 0)\n"), "{out}");
    assert!(!out.contains("receipt slots used"), "{out}");
    assert!(out.ends_with("all clear\n"), "{out}");
}

/// A one-shot HTTP board answering `POST /v1/rpc` board.info; returns its URL.
fn board_info_server(owner: &'static str) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(n) = conn.read(&mut chunk) {
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + len || n == 0 {
                        break;
                    }
                }
            }
            let body = json!({"v": 1, "type": "response", "request_id": "r", "ok": true,
                "result": {"name": "dakai", "owner": owner, "host": "box", "mailbox_only": false}})
            .to_string();
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = conn.write_all(reply.as_bytes());
        }
    });
    url
}

#[test]
fn enroll_stores_a_pinned_board_and_says_what_it_does_not_do() {
    let r = Root::new();
    let enroll = |args: &[&str]| {
        let mut child = r.cmd(Path::new(BIN)).arg("enroll").args(args).arg("--key-stdin").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        writeln!(child.stdin.take().unwrap(), "conde_k1").unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.success(), String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap())
    };
    let server = board_info_server("krisz");
    let (ok, out) = enroll(&["--server", &server]);
    assert!(ok, "{out}");
    let path = r.root.join("home/.config/conde/boards/dakai.json");
    assert!(out.starts_with(&format!("enrolled host box with {server} as board dakai (account krisz); credential stored at {} (mode 0600)\n", path.display())), "{out}");
    assert!(out.contains("Enrolling publishes nothing and uploads no history: every agent stays on this machine's local board, and teammates cannot reach one until you publish it: conde publish <name> --board dakai\n"), "{out}");
    assert!(out.contains("next: conde setup"), "{out}");
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!((stored["owner"].as_str(), stored["host_id"].as_str(), stored["key"].as_str()), (Some("krisz"), Some("box"), Some("conde_k1")));
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(!r.root.join("home/.config/conde/credentials").exists(), "no v1 credential is written");
    assert!(!out.contains("conde_k1"), "the key is never echoed");

    // the alias is pinned: another server, or the reserved local alias, is refused
    let elsewhere = board_info_server("krisz");
    let (ok, out) = enroll(&["--server", &elsewhere, "--board", "dakai"]);
    assert!(!ok && out.contains(&format!("dakai is pinned to {server} as krisz; to point the alias elsewhere, first run: conde boards --forget dakai")), "{out}");
    let (ok, out) = enroll(&["--server", &server, "--board", "local"]);
    assert!(!ok && out.contains("'local' names this machine's own board"), "{out}");
    // a second alias for the same server is a separate board file
    let (ok, out) = enroll(&["--server", &elsewhere, "--board", "team2"]);
    assert!(ok, "{out}");
    assert!(r.root.join("home/.config/conde/boards/team2.json").exists());
}

/// A daemon started before the upgrade ignores the envelope's board: the CLI refuses instead
/// of acting on the wrong board.
#[test]
fn a_daemon_that_predates_boards_is_never_handed_a_board() {
    let r = Root::new();
    let d = r.fake(|req| match req["op"].as_str().unwrap() {
        "board.list" => Err(("bad_request", "unknown op 'board.list'".into())),
        _ => Ok(json!({"message_id": "01M", "to": "tester/x"})),
    });
    for board in ["dakai", "local"] {
        let (ok, _, err) = r.conde(&["send", "x", "hi", "--board", board]);
        assert!(!ok && err.contains("the running conde daemon predates boards and would ignore --board; restart it"), "{err}");
        assert!(d.take().iter().all(|q| q["op"] == "board.list"), "nothing else was sent");
    }
    // without a board the request is the v1 one
    let (ok, out, _) = r.conde(&["send", "x", "hi"]);
    assert!(ok && out == "queued 01M to tester/x\n", "{out}");
}

/// A bridge running another build than this binary: `conde doctor` names both builds and how to
/// restart, and counts it as an issue (exit 1, no "all clear"); so does a daemon too old to
/// report its build.
#[test]
fn doctor_flags_a_bridge_running_another_build() {
    let r = Root::new();
    let checks = json!([{"ok": true, "text": "local board: conde.db (integrity ok)"}]);
    let old = "0.1.0 (0000000 2026-01-01)";
    let _d = r.fake(scripted(vec![("doctor", json!({"checks": checks, "issues": 0, "build": old}))]));
    let (ok, out, _) = r.conde(&["doctor"]);
    assert!(!ok, "{out}");
    let line = format!(
        "ISSUE   bridge: running another build ({old}) than this binary ({}); if this binary is the one the service \
         should run, restart it (Linux: ",
        conde::VERSION
    );
    assert!(out.contains(&line), "{out}");
    assert!(out.contains("systemctl --user restart conde") && !out.contains("all clear"), "{out}");
    drop(_d);

    let _d = r.fake(scripted(vec![("doctor", json!({"checks": checks, "issues": 0}))]));
    let (ok, out, _) = r.conde(&["doctor"]);
    assert!(!ok, "{out}");
    assert!(out.contains("ISSUE   bridge: the running daemon does not report its build"), "{out}");
}

#[test]
fn paging_hints_and_unconfirmed_reads_name_their_board() {
    let r = Root::new();
    let _d = r.fake(|req| {
        Ok(match (req["op"].as_str().unwrap(), req["board"].as_str()) {
            ("inbox.list", Some("dakai")) => json!({"board": "dakai", "next_cursor": 7, "items": [
                {"board": "dakai", "kind": "dm", "message_id": "dakai:01MD", "from": "b/r", "date": "d", "preview": "q"}]}),
            ("inbox.list", _) => json!({"next_cursor": 5, "items": [
                {"board": "local", "kind": "dm", "message_id": "01ML", "from": "a", "date": "d", "preview": "p"}],
                "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "next_cursor": 9}]}),
            ("message.read", _) => json!({"more": false, "messages": [
                {"board": "dakai", "place": "dm", "message_id": "dakai:01MU", "from": "b/r", "date": "d", "text": "preview only", "unconfirmed": true}],
                "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "unconfirmed": true}]}),
            _ => json!({}),
        })
    });
    // aggregate: `after` pages the local part; each board pages on its own
    let (_, _, err) = r.conde(&["inbox"]);
    assert!(err.contains("(more on local: conde inbox --board local --after 5)\n"), "{err}");
    assert!(err.contains("(more on dakai: conde inbox --board dakai --after 9)\n"), "{err}");
    let (_, _, err) = r.conde(&["inbox", "--board", "dakai"]);
    assert!(err.contains("(more: conde inbox --board dakai --after 7)\n"), "{err}");
    // a read the board did not confirm in time is shown as such, never as read
    let (_, out, err) = r.conde(&["read"]);
    assert!(out.starts_with("=== dakai:01MU (unconfirmed: preview only)\nFrom: b/r\nTo: \nDate: d\nUnconfirmed: dakai did not confirm this read; below is only a preview. The whole message: conde read dakai:01MU\n"), "{out}");
    assert!(err.contains("(dakai: not confirmed read; each message marked unconfirmed above names how to read it whole)"), "{err}");
}

/// Astra's integrated review, findings 4, 8 and 9, and the fields V2 added after it.
#[test]
fn review_fields_unconfirmed_recovery_shared_resolve_profile_paging_and_forget() {
    let r = Root::new();
    let _d = r.fake(|req| {
        Ok(match (req["op"].as_str().unwrap(), req["board"].as_str()) {
            ("message.read", _) => json!({"more": false, "messages": [
                {"board": "dakai", "place": "dm", "message_id": "dakai:01MU", "from": "b/r", "date": "d",
                 "text": "the first eighty characters only", "unconfirmed": true, "recover": "conde read dakai:01MU"}],
                "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "unconfirmed": true}]}),
            ("inbox.list", _) => json!({"next_cursor": 5, "items": [
                {"board": "local", "kind": "dm", "message_id": "01ML", "from": "a", "date": "d", "preview": "p"}],
                "boards": [{"board": "local", "available": true, "next_cursor": 5},
                           {"board": "dakai", "available": true, "next_cursor": 200, "more": true}]}),
            ("agent.resolve", Some("dakai")) => json!({"board": "dakai", "name": "balazs/relay", "kind": "mailbox", "host": "bpc"}),
            ("agent.resolve", _) => json!({"name": "tester/x", "address": "claude:s1@testhost"}),
            ("profile.get", Some("dakai")) => json!({"board": "dakai", "name": "balazs/relay", "kind": "mailbox", "host": "bpc", "display": "Relay"}),
            ("board.forget", _) => json!({"board": "dakai", "publications_removed": 0, "ops_removed": 1, "default_reset": true, "default": "local"}),
            ("message.send", _) => return Err(("not_found", "the default board 'dakai' is not enrolled on this host; ...".into())),
            _ => json!({}),
        })
    });
    // 4: an unconfirmed row is marked as a preview and names its recovery command
    let (_, out, _) = r.conde(&["read"]);
    assert!(out.contains("=== dakai:01MU (unconfirmed: preview only)\n"), "{out}");
    assert!(out.contains("Unconfirmed: dakai did not confirm this read; below is only a preview. The whole message: conde read dakai:01MU\n"), "{out}");
    // 9: every part's cursor, the local one printed once
    let (_, _, err) = r.conde(&["inbox"]);
    assert_eq!(err.matches("(more on local: conde inbox --board local --after 5)").count(), 1, "{err}");
    assert!(err.contains("(more on dakai: conde inbox --board dakai --after 200)"), "{err}");
    // 8: a shared row has no address: kind and host, never a blank line or "(not registered)"
    let (_, out, _) = r.conde(&["resolve", "balazs/relay", "--board", "dakai"]);
    assert_eq!(out, "balazs/relay\tmailbox on bpc\n");
    let (_, out, _) = r.conde(&["resolve", "x"]);
    assert_eq!(out, "claude:s1@testhost\n", "local rows keep the address");
    let (_, out, _) = r.conde(&["profile", "balazs/relay", "--board", "dakai"]);
    assert_eq!(out, "name: balazs/relay\nboard: dakai\nkind: mailbox\nhost: bpc\nDisplay: Relay\n");
    // forget reports the default board it reset
    let (_, out, _) = r.conde(&["boards", "--forget", "dakai"]);
    assert!(out.ends_with("nothing changed on the server\ndefault board reset to local\n"), "{out}");
    // a default board that is no longer enrolled is the daemon's refusal, passed through
    let (ok, _, err) = r.conde(&["send", "x", "hi"]);
    assert!(!ok && err == "conde: the default board 'dakai' is not enrolled on this host; ...\n", "{err}");
}

/// Astra's second pass, P3: an aggregate `sent` pages each board on its own, as inbox does.
#[test]
fn aggregate_sent_names_each_boards_continuation() {
    let r = Root::new();
    let _d = r.fake(|req| {
        Ok(match (req["op"].as_str().unwrap(), req["board"].as_str()) {
            ("receipt.list", Some("dakai")) => json!({"board": "dakai", "next_cursor": 7, "receipts": []}),
            ("receipt.list", _) => json!({"receipts": [{"board": "dakai", "message_id": "dakai:01M", "to": "alice/worker", "status": "queued"}],
                "boards": [{"board": "local", "available": true}, {"board": "dakai", "available": true, "next_cursor": 200}]}),
            _ => json!({}),
        })
    });
    let (_, out, err) = r.conde(&["sent"]);
    assert_eq!(out, "dakai  dakai:01M  alice/worker  queued\n");
    assert!(err.contains("(more on dakai: conde sent --board dakai --after 200)"), "{err}");
    let (_, _, err) = r.conde(&["sent", "--board", "dakai"]);
    assert!(err.contains("(more: conde sent --board dakai --after 7)"), "{err}");
}

/// `conde boards --migrate <alias> --publish*` reaches the review with its choice and asks
/// nothing; the flags exclude each other and need --migrate.
#[test]
fn boards_migrate_takes_the_publish_flags() {
    let r = Root::new();
    r.enroll_file("dakai", "https://conde.test");
    let d = r.fake(scripted(vec![
        ("board.info", json!({"name": "dakai", "owner": "tester", "host": "testhost", "mailbox_only": false})),
        ("host.migrate_preview", json!({"rows": [
            {"agent_id": "01DESK", "name": "desk", "address": "claude:s1@testhost", "pin": null, "candidate": true, "evidence": []},
            {"agent_id": "01LANE", "name": "lane", "address": "claude:s2@testhost", "pin": null, "candidate": false, "evidence": []}]})),
        ("host.migrate_v2", json!({"bindings": [
            {"agent_id": "01DESK", "name": "desk", "address": "claude:s1@testhost", "pin": null, "published": null},
            {"agent_id": "01LANE", "name": "lane", "address": "claude:s2@testhost", "pin": null, "published": {"mailbox_id": "01MB", "route_generation": 1}}]})),
    ]));
    let (ok, out, err) = r.conde(&["boards", "--migrate", "dakai", "--publish", "lane"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("boards: dakai: --publish lane publishes lane; the other 1 names move to this host's local board only\n"), "{out}");
    assert!(out.contains("boards: dakai: switched to v2; published: lane; on this host's local board only: 1 names\n"), "{out}");
    let migrate = d.take().into_iter().find(|q| q["op"] == "host.migrate_v2").unwrap();
    assert_eq!((migrate["board"].as_str(), &migrate["body"]), (Some("dakai"), &json!({"publish": ["01LANE"]})));

    for args in [
        &["boards", "--migrate", "dakai", "--publish-all", "--publish-none"][..],
        &["setup", "--publish", "desk", "--publish-candidates"],
        &["boards", "--publish-all"],
    ] {
        let (ok, _, err) = r.conde(args);
        assert!(!ok, "{args:?} ran");
        assert!(err.contains("cannot be used with") || err.contains("conde boards --migrate <alias>"), "{args:?}: {err}");
    }
    assert!(d.take().is_empty(), "refused before the daemon");
}

/// A publish whose mailbox was taken over or withdrawn on the board before its answer was
/// applied: a clear outcome, never reported as pending.
#[test]
fn a_publish_withdrawn_on_the_board_is_not_pending() {
    let r = Root::new();
    let _d = r.fake(scripted(vec![("mailbox.publish", json!({"state": "withdrawn", "op_id": "01OP", "name": "worker"}))]));
    let (ok, out, err) = r.conde(&["publish", "worker", "--board", "dakai"]);
    assert!(!ok, "{out}");
    assert_eq!(err, "conde: not published: worker was withdrawn on dakai before the publish completed; run conde publish again or check conde boards\n");
    assert!(!out.contains("pending"), "{out}");
    // the board forgotten before the op completed is terminal too
    drop(_d);
    let _d = r.fake(scripted(vec![("mailbox.publish", json!({"state": "forgotten", "op_id": "01OP"}))]));
    let (ok, out, err) = r.conde(&["publish", "worker", "--board", "dakai"]);
    assert!(!ok && !out.contains("pending"), "{out}");
    assert_eq!(err, "conde: not published: board dakai was forgotten before the publish completed\n");
}

/// `search --after` takes the cursor a page printed or a time, RFC 3339 or conde's own dates.
#[test]
fn search_after_takes_a_cursor_or_a_time() {
    let mut r = Root::new();
    r.start_daemon();
    let agent = |thread: &str, args: &[&str]| {
        let out = r
            .cmd(&r.root.join("bin/codex"))
            .env("CODEX_THREAD_ID", thread)
            .args(["-c", "\"$0\" \"$@\"; rc=$?; :; exit $rc", BIN])
            .args(args)
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap())
    };
    assert!(agent("t-a", &["register", "alice"]).0);
    assert!(agent("t-b", &["register", "bob"]).0);
    let (ok, out) = agent("t-a", &["send", "bob", "needle one"]);
    assert!(ok, "{out}");
    let first = out.strip_prefix("queued ").and_then(|s| s.split(' ').next()).expect(&out).to_string();
    let (_, shown) = agent("t-b", &["show", &first]);
    let date = shown.lines().find_map(|l| l.strip_prefix("Date: ")).expect(&shown).to_string();
    // the second message is stamped at a later millisecond
    std::thread::sleep(Duration::from_millis(5));
    assert!(agent("t-a", &["send", "bob", "needle two"]).0);

    let hits = |after: &str| {
        let (ok, out) = agent("t-b", &["search", "needle", "--after", after]);
        assert!(ok, "--after {after}: {out}");
        out.lines().filter(|l| l.contains("needle")).map(|l| l.rsplit('\t').next().unwrap().to_string()).collect::<Vec<_>>()
    };
    assert_eq!(hits(&date), ["needle two"], "conde's own date ({date})");
    assert_eq!(hits(&date.replace('Z', "+00:00")), ["needle two"], "an RFC 3339 offset");
    assert_eq!(hits("2000-01-01T00:00:00Z"), ["needle one", "needle two"]);
    assert_eq!(hits("0"), ["needle one", "needle two"], "a cursor");
    assert!(hits("1000000").is_empty(), "a cursor past the end");

    let (ok, out) = agent("t-b", &["search", "needle", "--after", "yesterday"]);
    assert!(!ok && out.contains("RFC 3339") && out.contains("cursor"), "{out}");
    let (_, help) = agent("t-b", &["search", "--help"]);
    assert!(help.contains("RFC 3339") && help.contains("2026-10-09T14:30:00Z"), "{help}");
}
