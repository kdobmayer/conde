//! The installed binary's `conde mcp` and `conde setup`, as a harness and a
//! teammate run them: real processes, temp HOME and socket, no real config.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_conde");

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
    assert_eq!(r1["result"]["content"][0]["text"], "registered tester/wired -> codex:t-wired@testhost", "{r1}");
    let r2 = rpc(call("whoami", json!({}), json!({"x-codex-turn-metadata": {"thread_id": "t-wired"}})));
    assert_eq!(r2["result"]["isError"], true, "only the stamped threadId counts: {r2}");
    let r3 = rpc(call("whoami", json!({}), json!({})));
    let text = r3["result"]["content"][0]["text"].as_str().unwrap();
    assert!(r3["result"]["isError"] == true && text.contains("refusing to act as the human"), "{r3}");
    drop(stdin);
    assert!(child.wait().unwrap().success(), "conde mcp exits on EOF");
}

#[test]
fn conde_setup_takes_the_server_from_the_enrolled_credential() {
    let r = Root::new();
    let home = r.root.join("home");

    // not enrolled: a local-board service, and every other part still done
    let (ok, out) = r.setup(&[]);
    assert!(ok, "{out}");
    assert!(out.contains("service: this host is not enrolled"), "{out}");
    assert!(out.contains("service: systemctl not found"), "{out}");
    assert!(out.contains("claude: claude CLI not found; wrote the user-scope MCP entry conde into ~/.claude.json directly"), "{out}");
    assert!(out.contains("claude: set crossSessionInbound to \"accept\" in ~/.claude/settings.json"), "{out}");
    assert!(out.contains("codex: added MCP server conde to ~/.codex/config.toml"), "{out}");
    let unit = std::fs::read_to_string(home.join(".config/systemd/user/conde.service")).unwrap();
    assert!(unit.contains(" daemon\n"), "{unit}");

    // enrolled: the unit bridges to that server
    let cred = home.join(".config/conde/credentials");
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    std::fs::write(&cred, r#"{"server":"https://conde.test","host_id":"testhost","key":"k"}"#).unwrap();
    std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (ok, out) = r.setup(&["--service"]);
    assert!(ok, "{out}");
    assert!(out.contains("service: updated ~/.config/systemd/user/conde.service"), "{out}");
    let unit = std::fs::read_to_string(home.join(".config/systemd/user/conde.service")).unwrap();
    assert!(unit.contains(" daemon --server https://conde.test\n"), "{unit}");
    assert!(unit.contains(&format!("Environment=\"CONDE_SOCK={}\"", r.root.join("run/conde.sock").display())), "{unit}");

    // a credential readable by others is refused, nothing is rewritten
    std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (ok, out) = r.setup(&["--service"]);
    assert!(!ok && out.contains("mode 644"), "{out}");

    // --yes is still accepted (a no-op); the first run already set accept
    let (ok, out) = r.setup(&["--claude", "--yes"]);
    assert!(ok, "{out}");
    assert!(out.contains("claude: crossSessionInbound already \"accept\""), "{out}");
    assert!(!out.contains("service:") && !out.contains("codex:"), "{out}");
}

/// systemd starts the unit with only HOME and USER from the user manager plus the
/// unit's own Environment= lines: the daemon must still find a relocated credential.
#[test]
fn the_generated_unit_finds_a_relocated_credential() {
    let r = Root::new();
    let xdg = r.root.join("elsewhere/config");
    let cred = xdg.join("conde/credentials");
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    std::fs::write(&cred, r#"{"server":"https://conde.test","host_id":"testhost","key":"k"}"#).unwrap();
    std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600)).unwrap();
    let out = r.cmd(Path::new(BIN)).env("XDG_CONFIG_HOME", &xdg).args(["setup", "--service"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let unit = std::fs::read_to_string(xdg.join("systemd/user/conde.service")).unwrap();

    let exec: Vec<&str> = unit.lines().find_map(|l| l.strip_prefix("ExecStart=")).unwrap().split(' ').collect();
    let mut daemon = Command::new(exec[0]);
    daemon.args(&exec[1..]).env_clear().env("HOME", r.root.join("home")).env("USER", "tester");
    for line in unit.lines().filter_map(|l| l.strip_prefix("Environment=\"")) {
        let (k, v) = line.trim_end_matches('"').split_once('=').unwrap();
        daemon.env(k, v);
    }
    assert!(exec.ends_with(&["daemon", "--server", "https://conde.test"]), "{exec:?}");
    let log = std::fs::File::create(r.root.join("unit-daemon.log")).unwrap();
    let mut child = daemon.stdout(Stdio::null()).stderr(log).spawn().unwrap();
    // the socket is bound only after the credential loaded and matched the server
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
    assert_eq!(agent("t-d", &["register", "desk"]).1, "registered tester/desk -> codex:t-d@testhost\n");
    assert_eq!(agent("t-d", &["create", "#lobby"]), (true, "created #lobby (you are a member)\n".into()));
    assert_eq!(
        agent("t-d", &["topic", "#lobby", "questions about conde; the welcome from desk"]),
        (true, "topic of #lobby: questions about conde; the welcome from desk\n".into())
    );
    // a first registration joins it, a repeated one does not
    let (ok, out) = agent("t-a", &["register", "alice"]);
    assert!(ok, "{out}");
    assert_eq!(out, "registered tester/alice -> codex:t-a@testhost\njoined #lobby\n");
    let (_, out) = agent("t-a", &["register", "alice"]);
    assert_eq!(out, "registered tester/alice -> codex:t-a@testhost\n", "only the first registration joins");

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
