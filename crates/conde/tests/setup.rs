//! `conde setup` on temp homes, Linux and macOS branches, with a fake host
//! standing in for systemctl, launchctl, the claude CLI and the terminal.
//! Never touches the real ~/.claude, ~/.codex or ~/.config.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use conde::setup::{self, CmdOut, Env, Host, Options, Os};
use conde::{paths, util};

struct Fake {
    on_path: HashSet<&'static str>,
    calls: Vec<String>,
    launchd_loaded: bool,
    claude_json: PathBuf,
    fail: Option<&'static str>,
    alive: bool,
    print_out: String,
    show_out: String,
}

impl Fake {
    fn new(env: &Env, on_path: &[&'static str]) -> Fake {
        Fake {
            on_path: on_path.iter().copied().collect(),
            calls: Vec::new(),
            launchd_loaded: false,
            claude_json: env.claude_json(),
            fail: None,
            alive: false,
            print_out: String::new(),
            show_out: String::new(),
        }
    }

    fn take_calls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.calls)
    }
}

impl Host for Fake {
    fn which(&self, prog: &str) -> Option<PathBuf> {
        self.on_path.contains(prog).then(|| PathBuf::from(format!("/fake/bin/{prog}")))
    }

    fn run(&mut self, prog: &str, args: &[&str]) -> anyhow::Result<CmdOut> {
        let name = Path::new(prog).file_name().unwrap().to_string_lossy().into_owned();
        let line = format!("{name} {}", args.join(" "));
        self.calls.push(line.clone());
        let mut ok = self.fail.is_none_or(|f| !line.starts_with(f));
        let mut stdout = String::new();
        match (name.as_str(), args) {
            ("claude", ["mcp", "add", "--scope", "user", "conde", "--", cmd, "mcp"]) => {
                // what the real CLI does: a user-scope stdio entry in ~/.claude.json
                let text = std::fs::read_to_string(&self.claude_json).unwrap_or_default();
                let entry = json!({"type": "stdio", "command": cmd, "args": ["mcp"], "env": {}});
                std::fs::write(&self.claude_json, setup::json_set(&text, &["mcpServers", "conde"], &entry)?)?;
            }
            ("claude", ["mcp", "remove", "--scope", "user", "conde"]) => {
                let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&self.claude_json)?)?;
                v["mcpServers"].as_object_mut().unwrap().remove("conde");
                std::fs::write(&self.claude_json, v.to_string())?;
            }
            ("launchctl", ["print", _]) => {
                ok = self.launchd_loaded;
                stdout = self.print_out.clone();
            }
            ("systemctl", ["--user", "show", ..]) => stdout = self.show_out.clone(),
            ("launchctl", ["bootstrap", ..]) => self.launchd_loaded = true,
            ("launchctl", ["bootout", _]) => self.launchd_loaded = false,
            _ => {}
        }
        Ok(CmdOut { ok, stdout, stderr: if ok { String::new() } else { "boom".into() } })
    }

    fn reach(&mut self, _socket: &Path) -> Result<(), String> {
        if self.alive { Ok(()) } else { Err("No such file or directory (os error 2)".into()) }
    }
}

fn temp_root() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("setup-{}", util::new_ulid()));
    std::fs::create_dir_all(root.join("home")).unwrap();
    root
}

fn env(root: &Path, os: Os) -> Env {
    Env {
        os,
        home: root.join("home"),
        uid: 501,
        xdg_config_home: None,
        claude_config_dir: None,
        codex_home: None,
        xdg_data_home: None,
        conde_data: None,
        exe: root.join("bin/conde"),
        path: "/usr/local/bin:/usr/bin:/bin".into(),
        socket: root.join("run/conde.sock"),
    }
}

fn run(env: &Env, opts: &Options, host: &mut Fake) -> (bool, String) {
    let mut out = Vec::new();
    let ok = setup::run(env, opts, host, &mut out).unwrap();
    (ok, String::from_utf8(out).unwrap())
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every file setup wrote, relative to the temp root.
fn files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, acc: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, root, acc);
            } else {
                acc.push(p.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut acc = Vec::new();
    walk(root, root, &mut acc);
    acc.sort();
    acc
}

#[test]
fn linux_full_setup_then_idempotent_rerun() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let opts = Options::select(false, false, false, false);
    let mut host = Fake::new(&env, &["systemctl", "claude"]);
    let exe = env.exe.display().to_string();

    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(
        host.take_calls(),
        [
            "systemctl --user daemon-reload".to_string(),
            "systemctl --user enable conde.service".into(),
            "systemctl --user start conde.service".into(),
            format!("claude mcp add --scope user conde -- {exe} mcp"),
        ]
    );
    assert!(out.contains("service: wrote ~/.config/systemd/user/conde.service"), "{out}");
    assert!(out.contains("claude: set crossSessionInbound to \"accept\" in ~/.claude/settings.json"), "{out}");
    assert!(out.contains("codex: added MCP server conde to ~/.codex/config.toml"), "{out}");
    assert!(
        out.ends_with(&format!(
            "\n\n{}\n  {}\n{}\n\n{}\n",
            setup::FEEDBACK,
            setup::FEEDBACK_COMMAND,
            setup::FEEDBACK_KEEP,
            setup::GUIDE
        )),
        "{out}"
    );
    assert!(setup::FEEDBACK_COMMAND.starts_with("conde send kdobmayer/desk "));

    let unit = read(&env.systemd_unit());
    assert!(unit.contains(&format!("ExecStart={exe} daemon\n")), "{unit}");
    assert!(unit.contains("\nEnvironment=\"PATH=/usr/local/bin:/usr/bin:/bin\"\n"), "{unit}");
    // the service resolves the same socket as the CLI that ran setup (as the launchd agent does)
    assert!(unit.contains(&format!("\nEnvironment=\"CONDE_SOCK={}\"\n", env.socket.display())), "{unit}");
    assert!(unit.contains("Restart=always"), "{unit}");
    assert_eq!(read(&env.claude_skill()), setup::CLAUDE_SKILL);
    assert_eq!(read(&env.codex_skill()), setup::CODEX_SKILL);
    assert!(setup::CLAUDE_SKILL.starts_with("---\nname: conde\n") && setup::CODEX_SKILL.contains("Codex"));
    let claude_json: Value = serde_json::from_str(&read(&env.claude_json())).unwrap();
    assert_eq!(claude_json["mcpServers"]["conde"]["command"], exe);
    let settings: Value = serde_json::from_str(&read(&env.claude_settings())).unwrap();
    assert_eq!(settings["crossSessionInbound"], "accept");
    assert!(read(&env.codex_config()).contains(&format!("[mcp_servers.conde]\ncommand = \"{exe}\"\nargs = [\"mcp\"]\n")));
    let written = files(&root);
    assert_eq!(
        written,
        [
            "home/.claude.json",
            "home/.claude/settings.json",
            "home/.claude/skills/conde/SKILL.md",
            "home/.codex/config.toml",
            "home/.codex/skills/conde/SKILL.md",
            "home/.config/systemd/user/conde.service",
        ]
    );

    // second run: nothing rewritten, nothing re-registered, nobody asked
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(host.take_calls(), ["systemctl --user enable conde.service", "systemctl --user start conde.service"]);
    for line in [
        "service: unchanged ~/.config/systemd/user/conde.service",
        "claude: skill unchanged ~/.claude/skills/conde/SKILL.md",
        "claude: MCP server conde unchanged (user scope)",
        "claude: crossSessionInbound already \"accept\" in ~/.claude/settings.json",
        "codex: skill unchanged ~/.codex/skills/conde/SKILL.md",
        "codex: MCP server conde unchanged in ~/.codex/config.toml",
    ] {
        assert!(out.contains(line), "missing {line:?} in:\n{out}");
    }
    assert_eq!(files(&root), written);

    // the binary moved: the unit restarts, the MCP entry is replaced
    let moved = Env { exe: root.join("bin2/conde"), ..env.clone() };
    let (ok, out) = run(&moved, &opts, &mut host);
    assert!(ok, "{out}");
    let calls = host.take_calls();
    assert!(calls.contains(&"systemctl --user restart conde.service".to_string()), "{calls:?}");
    assert!(calls.contains(&"claude mcp remove --scope user conde".to_string()), "{calls:?}");
    assert!(read(&env.codex_config()).matches("[mcp_servers.conde]").count() == 1);
    assert!(read(&env.codex_config()).contains("bin2/conde"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn macos_launch_agent_with_server() {
    let root = temp_root();
    let env = env(&root, Os::MacOs);
    let mut opts = Options::select(false, false, true, false);
    opts.server = Some("wss://conde.nativeai.space".into());
    let mut host = Fake::new(&env, &[]);
    let plist_path = env.launchd_plist();
    assert!(plist_path.ends_with("home/Library/LaunchAgents/space.nativeai.conde.plist"));

    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    let target = "gui/501/space.nativeai.conde";
    let bootstrap = format!("launchctl bootstrap gui/501 {}", plist_path.display());
    assert_eq!(host.take_calls(), [format!("launchctl print {target}"), bootstrap.clone()]);
    let plist = read(&plist_path);
    for needle in [
        "<key>Label</key>\n  <string>space.nativeai.conde</string>",
        &format!("<string>{}</string>\n    <string>daemon</string>\n    <string>--server</string>\n    <string>wss://conde.nativeai.space</string>", env.exe.display()),
        "<key>RunAtLoad</key>\n  <true/>",
        "<key>KeepAlive</key>\n  <true/>",
        &format!("<key>CONDE_SOCK</key>\n    <string>{}</string>", env.socket.display()),
        &format!("<string>{}</string>", root.join("home/Library/Logs/conde/conde.log").display()),
    ] {
        assert!(plist.contains(needle), "missing {needle:?} in\n{plist}");
    }
    assert!(root.join("home/Library/Logs/conde").is_dir());
    assert!(out.contains("service: loaded space.nativeai.conde"), "{out}");
    assert!(!files(&root).iter().any(|f| f.contains(".config")), "no systemd unit on macOS");

    let (ok, _) = run(&env, &opts, &mut host);
    assert!(ok);
    assert_eq!(host.take_calls(), [format!("launchctl print {target}"), format!("launchctl kickstart {target}")]);

    opts.server = None;
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(
        host.take_calls(),
        [format!("launchctl print {target}"), format!("launchctl bootout {target}"), bootstrap]
    );
    assert!(!read(&plist_path).contains("--server"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn receiver_policy_unset_becomes_accept_explicit_values_stay() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let settings = env.claude_settings();
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let mut opts = Options::select(true, false, false, false);
    let mut host = Fake::new(&env, &["claude"]);

    // unset (other keys present): set to accept, nothing asked, key order kept
    let original = "{\n  \"theme\": \"dark\",\n  \"a\": [1]\n}\n";
    std::fs::write(&settings, original).unwrap();
    opts.keep_receiver_policy = true;
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(out.contains("claude: crossSessionInbound left unset in ~/.claude/settings.json (--keep-receiver-policy)"), "{out}");
    assert_eq!(read(&settings), original);
    opts.keep_receiver_policy = false;
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(out.contains(
        "claude: set crossSessionInbound to \"accept\" in ~/.claude/settings.json (bypass-mode sessions receive conde \
notifications without approval; --keep-receiver-policy leaves it)"
    ), "{out}");
    assert_eq!(read(&settings), "{\n  \"crossSessionInbound\": \"accept\",\n  \"theme\": \"dark\",\n  \"a\": [1]\n}\n");
    let (_, out) = run(&env, &opts, &mut host);
    assert!(out.contains("claude: crossSessionInbound already \"accept\""), "{out}");

    // an explicit hold or refuse is the human's: reported, never changed
    for value in ["hold", "refuse"] {
        let explicit = format!("{{\"crossSessionInbound\": \"{value}\"}}\n");
        std::fs::write(&settings, &explicit).unwrap();
        let (ok, out) = run(&env, &opts, &mut host);
        assert!(ok, "{out}");
        assert!(out.contains(&format!("claude: note: crossSessionInbound is \"{value}\" in ~/.claude/settings.json, an explicit setting, left as is")), "{out}");
        assert_eq!(read(&settings), explicit);
    }
    // no settings file at all: created with accept
    std::fs::remove_file(&settings).unwrap();
    let (ok, _) = run(&env, &opts, &mut host);
    assert!(ok);
    assert_eq!(read(&settings), "{\n  \"crossSessionInbound\": \"accept\"\n}\n");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn without_claude_cli_and_with_relocated_config_dirs() {
    let root = temp_root();
    let mut env = env(&root, Os::Linux);
    env.claude_config_dir = Some(root.join("claude-cfg"));
    env.codex_home = Some(root.join("codex-home"));
    env.xdg_config_home = Some(root.join("xdg"));
    std::fs::create_dir_all(root.join("claude-cfg")).unwrap();
    std::fs::write(root.join("claude-cfg/.claude.json"), "{\"numStartups\": 3, \"mcpServers\": {\"x\": {\"command\": \"y\"}}}").unwrap();
    std::fs::create_dir_all(root.join("codex-home")).unwrap();
    let codex_cfg = "# keep me\nmodel = \"gpt\"\n[mcp_servers.docs]\nurl = \"https://d\"";
    std::fs::write(root.join("codex-home/config.toml"), codex_cfg).unwrap();
    let opts = Options::select(false, false, false, true);
    let mut host = Fake::new(&env, &[]);

    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(out.contains("service: systemctl not found"), "{out}");
    assert!(out.contains("claude CLI not found; wrote the user-scope MCP entry conde into"), "{out}");
    assert!(host.calls.is_empty(), "{:?}", host.calls);
    let cj: Value = serde_json::from_str(&read(&root.join("claude-cfg/.claude.json"))).unwrap();
    assert_eq!(cj["numStartups"], 3);
    assert_eq!(cj["mcpServers"]["x"]["command"], "y");
    assert_eq!(cj["mcpServers"]["conde"], json!({"type": "stdio", "command": env.exe.display().to_string(), "args": ["mcp"], "env": {}}));
    let toml = read(&root.join("codex-home/config.toml"));
    assert!(toml.starts_with(codex_cfg), "{toml}");
    assert!(toml.ends_with(&format!("[mcp_servers.conde]\ncommand = \"{}\"\nargs = [\"mcp\"]\n", env.exe.display())), "{toml}");
    let written = files(&root);
    for f in [
        "claude-cfg/settings.json",
        "claude-cfg/skills/conde/SKILL.md",
        "codex-home/skills/conde/SKILL.md",
        "xdg/systemd/user/conde.service",
    ] {
        assert!(written.contains(&f.to_string()), "{f} not in {written:?}");
    }
    assert!(!written.iter().any(|f| f.starts_with("home/")), "{written:?}");

    let before = written;
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok && !out.contains("wrote") && !out.contains("updated"), "{out}");
    assert_eq!(files(&root), before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failing_part_is_reported_and_the_rest_still_runs() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let opts = Options::select(false, false, false, false);
    let mut host = Fake::new(&env, &["systemctl"]);
    host.fail = Some("systemctl --user enable");
    std::fs::create_dir_all(env.codex_config().parent().unwrap()).unwrap();
    std::fs::write(env.codex_config(), "[mcp_servers.a\ncommand = \"a\"\n").unwrap();

    let (ok, out) = run(&env, &opts, &mut host);
    assert!(!ok);
    assert!(out.contains("service: FAILED: `systemctl --user enable conde.service` failed: boom"), "{out}");
    assert!(out.contains("codex: FAILED: ~/.codex/config.toml left as is: not valid TOML"), "{out}");
    assert_eq!(read(&env.codex_config()), "[mcp_servers.a\ncommand = \"a\"\n", "invalid TOML is never rewritten");
    assert!(env.claude_skill().exists() && env.codex_skill().exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn existing_codex_entry_is_updated_in_place() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    std::fs::create_dir_all(env.codex_config().parent().unwrap()).unwrap();
    let mine = "# my servers\n[mcp_servers.conde]\ncommand = \"/elsewhere/conde\"\nargs = [\"mcp\"]\nstartup_timeout_sec = 20\n";
    std::fs::write(env.codex_config(), mine).unwrap();
    let mut host = Fake::new(&env, &[]);
    let (ok, out) = run(&env, &Options::select(false, true, false, false), &mut host);
    assert!(ok, "{out}");
    assert!(out.contains("codex: updated MCP server conde in ~/.codex/config.toml"), "{out}");
    assert_eq!(read(&env.codex_config()), mine.replace("/elsewhere/conde", &env.exe.display().to_string()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn process_env_resolves_paths() {
    // only reads: HOME and friends of this test process, nothing is written
    let e = Env::from_process().unwrap();
    assert!(e.exe.is_absolute());
    assert_eq!(e.socket, paths::socket_path());
}

#[test]
fn services_carry_every_relocated_directory() {
    let root = temp_root();
    for os in [Os::Linux, Os::MacOs] {
        let mut e = env(&root, os);
        e.xdg_config_home = Some(root.join("relocated-config"));
        e.xdg_data_home = Some(root.join("relocated-data"));
        e.conde_data = Some(root.join("board"));
        e.claude_config_dir = Some(root.join("relocated-claude"));
        e.codex_home = Some(root.join("relocated-codex"));
        let mut opts = Options::select(false, false, true, false);
        opts.server = Some("https://conde.test".into());
        let (unit, plist) = (setup::systemd_unit(&e, &opts), setup::launchd_plist(&e, &opts));
        for (name, dir) in [
            ("XDG_CONFIG_HOME", "relocated-config"),
            ("XDG_DATA_HOME", "relocated-data"),
            ("CONDE_DATA", "board"),
            ("CLAUDE_CONFIG_DIR", "relocated-claude"),
            ("CODEX_HOME", "relocated-codex"),
        ] {
            let path = root.join(dir).display().to_string();
            assert!(unit.contains(&format!("\nEnvironment=\"{name}={path}\"\n")), "{name} not in\n{unit}");
            assert!(plist.contains(&format!("<key>{name}</key>\n    <string>{path}</string>")), "{name} not in\n{plist}");
        }
    }
    // unset overrides are not invented
    let unit = setup::systemd_unit(&env(&root, Os::Linux), &Options::default());
    assert!(!unit.contains("XDG_CONFIG_HOME") && !unit.contains("CODEX_HOME"), "{unit}");
    let _ = std::fs::remove_dir_all(&root);
}

fn verify(env: &Env, opts: &Options, host: &mut Fake) -> (bool, String) {
    let mut out = Vec::new();
    let ok = setup::verify(env, opts, host, &mut out).unwrap();
    (ok, String::from_utf8(out).unwrap())
}

#[test]
fn check_reports_each_part_and_changes_nothing() {
    let root = temp_root();
    let env = env(&root, Os::MacOs);
    let opts = Options::select(false, false, false, true);
    let mut host = Fake::new(&env, &["claude", "codex"]);

    let (ok, out) = verify(&env, &opts, &mut host);
    assert!(!ok, "{out}");
    assert!(out.contains("ISSUE   service: ~/Library/LaunchAgents/space.nativeai.conde.plist missing; run: conde setup"), "{out}");
    assert!(out.contains("ISSUE   service: the bridge does not answer on"), "{out}");
    assert!(out.contains("launchd: space.nativeai.conde is not loaded"), "{out}");
    assert!(out.contains("ISSUE   claude: MCP server conde not registered"), "{out}");
    assert!(out.contains("ISSUE   codex: skill ~/.codex/skills/conde/SKILL.md missing"), "{out}");
    assert!(files(&root).is_empty(), "--check wrote {:?}", files(&root));
    assert_eq!(host.take_calls(), ["launchctl print gui/501/space.nativeai.conde"]);

    let set = opts.clone();
    let (ok, _) = run(&env, &set, &mut host);
    assert!(ok);
    host.take_calls();
    host.alive = true;
    let (ok, out) = verify(&env, &opts, &mut host);
    assert!(ok, "{out}");
    let exe = env.exe.display();
    for want in [
        "ok      service: ~/Library/LaunchAgents/space.nativeai.conde.plist up to date".to_string(),
        format!("ok      service: the bridge answers on {}", env.socket.display()),
        "ok      claude: skill ~/.claude/skills/conde/SKILL.md up to date".into(),
        format!("ok      claude: MCP server conde registered at user scope ({exe} mcp)"),
        "note    claude: crossSessionInbound is \"accept\"".into(),
        format!("ok      codex: MCP server conde registered in ~/.codex/config.toml ({exe} mcp)"),
        "all clear".into(),
    ] {
        assert!(out.contains(&want), "missing {want:?} in\n{out}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn check_skips_a_harness_that_is_not_installed_and_reads_toml_semantically() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    // Codex only, with a valid inline mcp_servers layout that toml_edit keeps inline
    std::fs::create_dir_all(env.codex_config().parent().unwrap()).unwrap();
    std::fs::write(env.codex_config(), "mcp_servers = { docs = { command = \"docs\" } }\n").unwrap();
    let mut host = Fake::new(&env, &["codex", "systemctl"]);
    host.alive = true;
    let (ok, out) = run(&env, &Options::select(false, true, true, false), &mut host);
    assert!(ok, "{out}");
    assert!(!read(&env.codex_config()).contains("[mcp_servers.conde]"), "the layout stays inline");

    let (ok, out) = verify(&env, &Options::select(false, false, false, true), &mut host);
    assert!(ok, "{out}");
    assert!(out.contains("skip    claude: Claude Code not found on this machine"), "{out}");
    assert!(out.contains("ok      codex: MCP server conde registered in ~/.codex/config.toml"), "{out}");
    assert!(out.contains("ok      service: the bridge answers on"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_exited_job_is_not_a_running_bridge() {
    let root = temp_root();
    let opts = Options::select(false, false, true, false);

    // launchd still has the job loaded, but it exited and nobody answers
    let mac = env(&root, Os::MacOs);
    let mut host = Fake::new(&mac, &[]);
    host.launchd_loaded = true;
    host.print_out = "gui/501/space.nativeai.conde = {\n\tstate = not running\n\tlast exit code = 1\n}\n".into();
    let (ok, out) = verify(&mac, &opts, &mut host);
    assert!(!ok, "{out}");
    assert!(out.contains("ISSUE   service: the bridge does not answer on"), "{out}");
    assert!(out.contains("launchd: state = not running, last exit code = 1"), "{out}");

    // systemd: the unit failed
    let linux = env(&root, Os::Linux);
    let mut host = Fake::new(&linux, &["systemctl"]);
    host.show_out = "ActiveState=failed\nExecMainStatus=1\n".into();
    let (ok, out) = verify(&linux, &opts, &mut host);
    assert!(!ok, "{out}");
    assert!(out.contains("systemd: ActiveState=failed, ExecMainStatus=1"), "{out}");
    assert!(host.calls.contains(&"systemctl --user show -p ActiveState,ExecMainStatus conde.service".to_string()));

    // a bridge that answers is ok whatever the manager says
    host.alive = true;
    let (_, out) = verify(&linux, &opts, &mut host);
    assert!(out.contains("ok      service: the bridge answers on"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_real_probe_needs_an_answer_within_the_timeout() {
    use std::os::unix::net::UnixListener;
    let root = temp_root();
    let mut sys = setup::System;
    let missing = root.join("nothing.sock");
    assert!(sys.reach(&missing).is_err());

    // accepts but never answers: refused after the bounded wait
    let silent = root.join("silent.sock");
    let listener = UnixListener::bind(&silent).unwrap();
    let hold = std::thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));
        drop(conn);
    });
    let started = std::time::Instant::now();
    let err = sys.reach(&silent).unwrap_err();
    assert!(err.contains("no answer within"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
    hold.join().unwrap();

    // closes at once (the macOS release-run symptom)
    let closing = root.join("closing.sock");
    let listener = UnixListener::bind(&closing).unwrap();
    let hold = std::thread::spawn(move || drop(listener.accept().unwrap()));
    let err = sys.reach(&closing).unwrap_err();
    hold.join().unwrap();
    assert!(err.contains("closed") || err.contains("Broken pipe") || err.contains("reset"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}
