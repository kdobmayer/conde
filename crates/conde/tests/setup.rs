//! `conde setup` on temp homes, Linux and macOS branches, with a fake host
//! standing in for systemctl, launchctl, the claude CLI and the terminal.
//! Never touches the real ~/.claude, ~/.codex or ~/.config.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use conde::protocol::v2::BoardInfo;
use conde::setup::{self, CmdOut, Env, Host, Options, Os, Publish};
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
    /// What the human types at each prompt; empty: there is no terminal.
    answers: VecDeque<String>,
    prompts: Vec<String>,
    /// The daemon's answer per op; an op without one is refused.
    daemon: HashMap<&'static str, Result<Value, String>>,
    daemon_calls: Vec<(String, Option<String>, Value)>,
    /// board.info over HTTPS (the v1 credential move).
    info: Option<Result<BoardInfo, String>>,
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
            answers: VecDeque::new(),
            prompts: Vec::new(),
            daemon: HashMap::new(),
            daemon_calls: Vec::new(),
            info: None,
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
            ("claude", ["mcp", "add", "--scope", "user", server, rest @ ..]) => {
                // what the real CLI (2.1.296) does: a user-scope stdio entry in ~/.claude.json,
                // its -e KEY=VALUE pairs as env
                let dash = rest.iter().position(|a| *a == "--").expect("-- before the command");
                let mut env = serde_json::Map::new();
                for pair in rest[..dash].chunks(2) {
                    assert_eq!(pair[0], "-e", "{line}");
                    let (k, v) = pair[1].split_once('=').unwrap();
                    env.insert(k.into(), v.into());
                }
                let (cmd, args) = (rest[dash + 1], &rest[dash + 2..]);
                let text = std::fs::read_to_string(&self.claude_json).unwrap_or_default();
                let entry = json!({"type": "stdio", "command": cmd, "args": args, "env": env});
                std::fs::write(&self.claude_json, setup::json_set(&text, &["mcpServers", server], &entry)?)?;
            }
            ("claude", ["mcp", "remove", "--scope", "user", server]) => {
                let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&self.claude_json)?)?;
                v["mcpServers"].as_object_mut().unwrap().remove(*server);
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

    fn terminal(&self) -> bool {
        !self.answers.is_empty()
    }

    fn ask(&mut self, prompt: &str) -> Option<String> {
        let answer = self.answers.pop_front()?;
        self.prompts.push(prompt.to_string());
        Some(answer)
    }

    fn daemon(&mut self, _socket: &Path, op: &str, board: Option<&str>, body: Value) -> Result<Value, String> {
        self.daemon_calls.push((op.to_string(), board.map(Into::into), body));
        self.daemon.get(op).cloned().unwrap_or_else(|| Err(format!("unknown op {op}")))
    }

    fn board_info(&mut self, _server: &str, _key: &str) -> anyhow::Result<BoardInfo> {
        match self.info.clone() {
            Some(Ok(i)) => Ok(i),
            Some(Err(e)) => Err(anyhow::anyhow!(e)),
            None => Err(anyhow::anyhow!("no board here")),
        }
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
        default_socket: root.join("run/conde.sock"),
    }
}

/// The `claude mcp add` setup runs for MCP server `name`.
fn add_call(env: &Env, name: &str) -> String {
    format!("claude mcp add --scope user {name} -e CONDE_SOCK={} -- {} mcp", env.socket.display(), env.exe.display())
}

/// The Codex entry setup writes for MCP server `name`.
fn codex_entry(env: &Env, name: &str) -> String {
    format!(
        "[mcp_servers.{name}]\ncommand = \"{}\"\nargs = [\"mcp\"]\nenv = {{ CONDE_SOCK = \"{}\" }}\n",
        env.exe.display(),
        env.socket.display()
    )
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
            add_call(&env, "conde"),
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
    assert!(read(&env.codex_config()).contains(&codex_entry(&env, "conde")));
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
fn macos_launch_agent_runs_the_single_daemon() {
    let root = temp_root();
    let env = env(&root, Os::MacOs);
    let opts = Options::select(false, false, true, false);
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
        &format!("<string>{}</string>\n    <string>daemon</string>\n  </array>", env.exe.display()),
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

    // a v1 agent that named a server is rewritten to the single daemon and reloaded
    std::fs::write(&plist_path, plist.replace("<string>daemon</string>", "<string>daemon</string>\n    <string>--server</string>\n    <string>wss://conde.nativeai.space</string>")).unwrap();
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
    let sock = env.socket.display().to_string();
    assert_eq!(cj["mcpServers"]["conde"], json!({"type": "stdio", "command": env.exe.display().to_string(), "args": ["mcp"], "env": {"CONDE_SOCK": sock}}));
    let toml = read(&root.join("codex-home/config.toml"));
    assert!(toml.starts_with(codex_cfg), "{toml}");
    assert!(toml.ends_with(&codex_entry(&env, "conde")), "{toml}");
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
    let want = format!("{}env = {{ CONDE_SOCK = \"{}\" }}\n", mine.replace("/elsewhere/conde", &env.exe.display().to_string()), env.socket.display());
    assert_eq!(read(&env.codex_config()), want);
    let _ = std::fs::remove_dir_all(&root);
}

/// Another conde instance's binary on this machine, which its `conde` MCP entries run.
fn other_instance(root: &Path) -> String {
    let other = root.join("other/bin/conde");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, "#!/bin/sh\n").unwrap();
    other.display().to_string()
}

fn claude_servers(env: &Env, servers: Value) {
    std::fs::write(env.claude_json(), json!({"numStartups": 3, "mcpServers": servers}).to_string()).unwrap();
}

#[test]
fn another_instances_conde_entry_stays_and_this_one_registers_as_conde_board() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let opts = Options::select(true, true, false, false);
    let other = other_instance(&root);
    let theirs = json!({"type": "stdio", "command": other, "args": ["mcp"], "env": {}});
    claude_servers(&env, json!({"conde": theirs}));
    std::fs::create_dir_all(env.codex_config().parent().unwrap()).unwrap();
    let codex_theirs = format!("[mcp_servers.conde]\ncommand = \"{other}\"\nargs = [\"mcp\"]\n");
    std::fs::write(env.codex_config(), &codex_theirs).unwrap();
    enrolled(&env);
    // the other instance's skills
    for skill in [env.claude_skill(), env.codex_skill()] {
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(&skill, "---\nname: conde\n---\ntheirs\n").unwrap();
    }
    let mut host = Fake::new(&env, &["claude"]);
    let exe = env.exe.display().to_string();

    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(host.take_calls(), [add_call(&env, "conde-dakai")], "theirs is never removed");
    for want in [
        format!("claude: MCP server conde belongs to another conde instance ({other}); this one registers as conde-dakai"),
        format!("codex: MCP server conde belongs to another conde instance ({other}); this one registers as conde-dakai"),
        "codex: updated MCP server conde-dakai in ~/.codex/config.toml".into(),
    ] {
        assert!(out.contains(&want), "missing {want:?} in:\n{out}");
    }
    let cj: Value = serde_json::from_str(&read(&env.claude_json())).unwrap();
    assert_eq!(cj["mcpServers"]["conde"], theirs);
    assert_eq!(cj["mcpServers"]["conde-dakai"]["command"], exe);
    let toml = read(&env.codex_config());
    assert!(toml.starts_with(&codex_theirs), "{toml}");
    for skill in [env.claude_skill(), env.codex_skill()] {
        assert_eq!(read(&skill), "---\nname: conde\n---\ntheirs\n", "the other instance's skill stays");
    }
    for skill in [env.claude_skill_for("conde-dakai"), env.codex_skill_for("conde-dakai")] {
        let text = read(&skill);
        assert!(text.starts_with("---\nname: conde-dakai\n"), "{}", skill.display());
        assert!(text.contains("tools of the `conde-dakai` MCP server"), "{}", skill.display());
        assert!(!text.contains("tools of the `conde` MCP server"));
    }
    assert!(out.contains("claude: skill wrote ~/.claude/skills/conde-dakai/SKILL.md"), "{out}");
    assert_eq!(cj["mcpServers"]["conde-dakai"]["env"]["CONDE_SOCK"], env.socket.display().to_string());
    assert!(toml.contains(&codex_entry(&env, "conde-dakai")), "{toml}");

    // a rerun finds its own entry under the second name and changes nothing
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(host.take_calls().is_empty());
    for want in [
        "claude: skill unchanged ~/.claude/skills/conde-dakai/SKILL.md",
        "claude: MCP server conde-dakai unchanged (user scope)",
        "codex: skill unchanged ~/.codex/skills/conde-dakai/SKILL.md",
        "codex: MCP server conde-dakai unchanged in ~/.codex/config.toml",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    host.on_path.insert("codex");
    let (ok, out) = verify(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(out.contains(&format!("ok      claude: MCP server conde-dakai registered at user scope ({exe} mcp)")), "{out}");
    assert!(out.contains("ok      codex: skill ~/.codex/skills/conde-dakai/SKILL.md up to date"), "{out}");
    assert!(out.contains("ok      claude: skill ~/.claude/skills/conde-dakai/SKILL.md up to date"), "{out}");
    assert!(out.contains(&format!("ok      codex: MCP server conde-dakai registered in ~/.codex/config.toml ({exe} mcp)")), "{out}");

    // enrolling another board later does not move the registration
    let cfg = conde::boards::BoardConfig { server: "https://b.test".into(), host_id: "box".into(), owner: "k".into(), key: "k".into() };
    conde::boards::store(&env.boards_dir().join("acme.json"), "acme", &cfg).unwrap();
    std::fs::write(env.default_board(), "acme\n").unwrap();
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok && host.take_calls().is_empty(), "{out}");
    assert!(out.contains("claude: MCP server conde-dakai unchanged (user scope)"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn another_socket_or_a_remote_server_is_another_instance_and_without_a_board_the_name_is_conde_2() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let opts = Options::select(true, true, false, false);
    let exe = env.exe.display().to_string();
    // our binary serving another socket
    let theirs = json!({"type": "stdio", "command": exe, "args": ["mcp"], "env": {"CONDE_SOCK": "/elsewhere/conde.sock"}});
    claude_servers(&env, json!({"conde": theirs}));
    // a remote server under the name in Codex
    std::fs::create_dir_all(env.codex_config().parent().unwrap()).unwrap();
    std::fs::write(env.codex_config(), "[mcp_servers.conde]\nurl = \"https://conde.example/mcp\"\n").unwrap();
    let mut host = Fake::new(&env, &["claude"]);

    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(host.take_calls(), [add_call(&env, "conde-2")]);
    assert!(out.contains(&format!("({exe}, CONDE_SOCK=/elsewhere/conde.sock); this one registers as conde-2")), "{out}");
    assert!(out.contains("codex: MCP server conde belongs to another conde instance (a server that runs no local command); this one registers as conde-2"), "{out}");
    let cj: Value = serde_json::from_str(&read(&env.claude_json())).unwrap();
    assert_eq!(cj["mcpServers"]["conde"], theirs);
    assert!(read(&env.codex_config()).starts_with("[mcp_servers.conde]\nurl = \"https://conde.example/mcp\"\n"));
    assert!(read(&env.codex_skill_for("conde-2")).contains("tools of the `conde-2` MCP server"));
    assert!(!env.codex_skill().exists() && !env.claude_skill().exists(), "skills/conde is the other instance's");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn our_own_or_a_dead_conde_entry_is_updated_in_place() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let opts = Options::select(true, false, false, false);
    let exe = env.exe.display().to_string();
    enrolled(&env);
    // ours on our socket, with other args: updated under its name
    let sock = env.socket.display().to_string();
    claude_servers(&env, json!({"conde": {"type": "stdio", "command": exe, "args": ["old"], "env": {"CONDE_SOCK": sock}}}));
    let mut host = Fake::new(&env, &["claude"]);
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(
        host.take_calls(),
        ["claude mcp remove --scope user conde".to_string(), add_call(&env, "conde")]
    );
    assert!(!out.contains("another conde instance"), "{out}");
    assert_eq!(read(&env.claude_skill()), setup::CLAUDE_SKILL);

    // a bare command not on PATH is a dead entry: reclaimed
    claude_servers(&env, json!({"conde": {"type": "stdio", "command": "conde-gone", "args": ["mcp"]}}));
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok, "{out}");
    assert_eq!(host.take_calls()[1], add_call(&env, "conde"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_entry_without_conde_sock_means_the_default_socket_and_a_bare_name_is_found_on_path() {
    let root = temp_root();
    let exe = root.join("bin/conde");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();
    let base = Env { path: format!("/nonexistent:{}", root.join("bin").display()), ..env(&root, Os::Linux) };
    let opts = Options::select(true, false, false, false);
    let old_style = json!({"type": "stdio", "command": "conde", "args": ["mcp"], "env": {"TOKEN": "t"}});

    // our binary by its bare name, no CONDE_SOCK, and this instance on the default socket: ours,
    // updated in place to carry the socket, its other env vars kept
    claude_servers(&base, json!({"conde": old_style}));
    let mut host = Fake::new(&base, &["claude"]);
    let (ok, out) = run(&base, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(!out.contains("another conde instance"), "{out}");
    let sock = base.socket.display().to_string();
    assert_eq!(
        host.take_calls(),
        [
            "claude mcp remove --scope user conde".to_string(),
            format!("claude mcp add --scope user conde -e CONDE_SOCK={sock} -e TOKEN=t -- {} mcp", exe.display())
        ]
    );
    let cj: Value = serde_json::from_str(&read(&base.claude_json())).unwrap();
    assert_eq!(cj["mcpServers"]["conde"]["env"], json!({"CONDE_SOCK": sock, "TOKEN": "t"}));
    let (ok, out) = verify(&base, &opts, &mut host);
    assert!(out.contains("ok      claude: MCP server conde registered at user scope"), "{ok} {out}");

    // the same entry while this instance runs on another socket: it is the default socket's
    // instance, not ours
    let elsewhere = Env { socket: root.join("run/second.sock"), ..base.clone() };
    claude_servers(&elsewhere, json!({"conde": old_style}));
    let (ok, out) = run(&elsewhere, &opts, &mut host);
    assert!(ok, "{out}");
    assert!(out.contains("claude: MCP server conde belongs to another conde instance (conde); this one registers as conde-2"), "{out}");
    assert_eq!(host.take_calls(), [add_call(&elsewhere, "conde-2")]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn both_skills_name_the_mcp_server_in_one_rewritable_place() {
    for skill in [setup::CLAUDE_SKILL, setup::CODEX_SKILL] {
        assert_eq!(skill.matches("tools of the `conde` MCP server").count(), 1);
        assert_eq!(setup::skill_text(skill, "conde"), skill);
        let renamed = setup::skill_text(skill, "conde-dakai");
        assert!(renamed.contains("tools of the `conde-dakai` MCP server") && renamed.starts_with("---\nname: conde-dakai\n"));
        assert_eq!(skill.matches("---\nname: conde\n").count(), 1);
    }
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
        let (unit, plist) = (setup::systemd_unit(&e), setup::launchd_plist(&e));
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
    let unit = setup::systemd_unit(&env(&root, Os::Linux));
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

// --- the host's boards (DESIGN-v2 §7, §9) -----------------------------------------------------

fn board_info(mailbox_only: bool) -> Value {
    json!({"name": "dakai", "owner": "krisz", "host": "box", "mailbox_only": mailbox_only})
}

fn preview() -> Value {
    json!({"rows": [
        {"agent_id": "01DESK", "name": "desk", "address": "claude:5f2c@box", "pin": "claude:5f2c@box", "candidate": true,
         "evidence": [{"via": "dm", "with": "balazs/relay", "source": "later", "messages": 4, "last_at": "2026-10-09T10:00:00Z"},
                      {"via": "channel", "with": "#lobby", "source": "imported", "messages": 1, "last_at": "2026-10-01T08:00:00Z"}]},
        {"agent_id": "01LANE", "name": "lane.v0", "address": "codex:th-7@box", "pin": null, "candidate": false, "evidence": []}]})
}

fn manifest(published: &[&str]) -> Value {
    let row = |id: &str, name: &str, addr: &str| {
        let p = if published.contains(&name) { json!({"mailbox_id": id, "route_generation": 2}) } else { Value::Null };
        json!({"agent_id": id, "name": name, "address": addr, "pin": null, "published": p})
    };
    json!({"bindings": [row("01DESK", "desk", "claude:5f2c@box"), row("01LANE", "lane.v0", "codex:th-7@box")],
           "conflicts": [{"name": "lane.v0", "address": "codex:th-7@box"}],
           "moved": [{"name": "desk.kond", "to": "desk", "address": "claude:5f2c@box"}]})
}

fn enrolled(env: &Env) {
    let path = env.boards_dir().join("dakai.json");
    let cfg = conde::boards::BoardConfig { server: "https://conde.test".into(), host_id: "box".into(), owner: "krisz".into(), key: "k".into() };
    conde::boards::store(&path, "dakai", &cfg).unwrap();
}

fn ops(host: &Fake) -> Vec<(String, Option<String>, Value)> {
    host.daemon_calls.clone()
}

#[test]
fn an_enrolled_v1_host_moves_its_credential_and_the_human_reviews_its_names() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let cred = env.v1_credential();
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    conde::credentials::store(&cred, &conde::credentials::Credentials { server: "https://conde.test".into(), host_id: "box".into(), key: "k1".into() }).unwrap();
    let mut host = Fake::new(&env, &["systemctl"]);
    host.info = Some(Ok(serde_json::from_value(board_info(false)).unwrap()));
    host.daemon.insert("board.info", Ok(board_info(false)));
    host.daemon.insert("host.migrate_preview", Ok(preview()));
    host.daemon.insert("host.migrate_v2", Ok(manifest(&["desk"])));
    host.answers = ["", "y", "dakai"].map(String::from).into();

    let (ok, out) = run(&env, &Options::select(false, false, false, false), &mut host);
    assert!(ok, "{out}");
    // the move: under the advertised alias, pinned to the reported account, before the service restarts
    assert!(!cred.exists());
    let stored = conde::boards::load(&env.boards_dir().join("dakai.json")).unwrap();
    assert_eq!((stored.server.as_str(), stored.owner.as_str(), stored.host_id.as_str(), stored.key.as_str()), ("https://conde.test", "krisz", "box", "k1"));
    let moved = out.find("boards: moved the v1 credential ~/.config/conde/credentials to ~/.config/conde/boards/dakai.json (board dakai, account krisz, host box)").expect(&out);
    assert!(moved < out.find("service: ").unwrap(), "{out}");
    // the preview: candidates with their evidence, the rest in one line
    for line in [
        "boards: dakai: host box (account krisz) still has 2 names bound from v1. A candidate (*) has messages with another owner's agents; history alone is not consent, so you choose:\n",
        "  * desk  claude:5f2c@box  pinned\n      dm with balazs/relay: 4 messages, last 2026-10-09 (since the import)\n      posts in #lobby: 1 messages, last 2026-10-01 (imported from kond)\n",
        "  no messages with another owner's agents (1): lane.v0\n",
        "boards: dakai: switched to v2; published: desk; on this host's local board only: 1 names\n",
        "boards: dakai: lane.v0 kept its local binding (a live session holds it); its former address codex:th-7@box is in conde doctor\n",
        "boards: dakai: session claude:5f2c@box moved from the local name desk.kond to desk; desk.kond keeps its messages, unbound\n",
        "boards: default board for names and channels: dakai\n",
    ] {
        assert!(out.contains(line), "missing {line:?} in\n{out}");
    }
    assert_eq!(host.prompts[1], "boards: dakai: publish desk; the other 1 names move to this host's local board only. Apply? [y/N] ");
    assert!(host.prompts[2].contains("Default board (local, dakai) [local]: "), "{:?}", host.prompts);
    let dakai = Some("dakai".to_string());
    assert_eq!(
        ops(&host),
        [
            ("board.info".to_string(), dakai.clone(), json!({})),
            ("host.migrate_preview".into(), dakai.clone(), json!({})),
            ("host.migrate_v2".into(), dakai, json!({"publish": ["01DESK"]})),
        ]
    );
    assert_eq!(read(&env.default_board()), "dakai\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// An agent runs setup from its shell: without a --publish flag nothing is published and the
/// board stays not migrated; setup finishes its other parts and names the four flags.
#[test]
fn without_an_instruction_nothing_is_published() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    enrolled(&env);
    let mut host = Fake::new(&env, &[]);
    host.daemon.insert("board.info", Ok(board_info(false)));
    host.daemon.insert("host.migrate_preview", Ok(preview()));
    host.daemon.insert("host.migrate_v2", Ok(manifest(&["desk"])));
    let (ok, out) = run(&env, &Options::select(false, false, false, false), &mut host);
    assert!(ok, "{out}");
    for line in [
        "boards: dakai: host box (account krisz) still has 2 names bound from v1. A candidate (*) has messages with another owner's agents:\n",
        "  * desk  claude:5f2c@box  pinned\n",
        "boards: dakai: not a terminal and no --publish flag; nothing changes until you run conde setup (or conde boards --migrate dakai) with --publish <name,...>, --publish-candidates, --publish-all or --publish-none\n",
        "boards: default board for names and channels: local (to change it: conde setup --default-board <alias>)\n",
    ] {
        assert!(out.contains(line), "missing {line:?} in\n{out}");
    }
    assert!(host.prompts.is_empty(), "{:?}", host.prompts);
    assert!(!ops(&host).iter().any(|(op, _, _)| op == "host.migrate_v2"), "nothing applied");
    assert!(!env.default_board().exists());

    // the explicit review without an instruction applies nothing either, and fails
    let mut sink = Vec::new();
    assert!(!setup::migrate_review(&env, &mut host, &mut sink, "dakai", true, None).unwrap());
    assert!(!ops(&host).iter().any(|(op, _, _)| op == "host.migrate_v2"), "nothing applied");
    // the flag sets the default without asking
    let mut opts = Options::select(false, false, false, false);
    opts.default_board = Some("dakai".into());
    let (_, out) = run(&env, &opts, &mut host);
    assert!(out.contains("boards: default board for names and channels: dakai\n"), "{out}");
    assert_eq!(read(&env.default_board()), "dakai\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// `--publish <names>`, `--publish-candidates`, `--publish-all`, `--publish-none`: each
/// decides without a prompt, even at a terminal; a name the board does not have applies
/// nothing.
#[test]
fn each_publish_flag_decides_the_switch() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    enrolled(&env);
    let cases = [
        (Publish::Names(vec!["lane.v0".into(), "krisz/desk".into()]), json!(["01LANE", "01DESK"]), "--publish lane.v0,krisz/desk publishes lane.v0, desk; the other 0 names"),
        (Publish::Candidates, json!(["01DESK"]), "--publish-candidates publishes desk; the other 1 names"),
        (Publish::All, json!(["01DESK", "01LANE"]), "--publish-all publishes desk, lane.v0; the other 0 names"),
        (Publish::Nothing, json!([]), "--publish-none publishes nothing; the other 2 names"),
    ];
    for (choice, ids, line) in cases {
        let mut host = Fake::new(&env, &[]);
        host.daemon.insert("board.info", Ok(board_info(false)));
        host.daemon.insert("host.migrate_preview", Ok(preview()));
        host.daemon.insert("host.migrate_v2", Ok(manifest(&[])));
        // a terminal is there, and nobody is asked
        host.answers = ["unused"].map(String::from).into();
        let mut out = Vec::new();
        assert!(setup::migrate_review(&env, &mut host, &mut out, "dakai", true, Some(&choice)).unwrap());
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains(&format!("boards: dakai: {line} move to this host's local board only\n")), "{choice:?}: {out}");
        assert!(out.contains("to change it later: conde publish <name> --board dakai"), "{out}");
        assert!(host.prompts.is_empty(), "{choice:?}: {:?}", host.prompts);
        assert_eq!(ops(&host).last().unwrap().2, json!({"publish": ids}), "{choice:?}");
    }

    // setup takes the flag for every enrolled board's review
    let mut host = Fake::new(&env, &[]);
    host.daemon.insert("board.info", Ok(board_info(false)));
    host.daemon.insert("host.migrate_preview", Ok(preview()));
    host.daemon.insert("host.migrate_v2", Ok(manifest(&[])));
    let mut opts = Options::select(false, false, false, false);
    opts.publish = Some(Publish::Nothing);
    let (ok, out) = run(&env, &opts, &mut host);
    assert!(ok && out.contains("boards: dakai: --publish-none publishes nothing"), "{out}");
    assert_eq!(ops(&host).last().unwrap().2, json!({"publish": []}));

    // an unknown name: nothing applied, and the review failed
    let mut host = Fake::new(&env, &[]);
    host.daemon.insert("board.info", Ok(board_info(false)));
    host.daemon.insert("host.migrate_preview", Ok(preview()));
    let mut out = Vec::new();
    let names = Publish::Names(vec!["desk".into(), "nope".into()]);
    assert!(!setup::migrate_review(&env, &mut host, &mut out, "dakai", false, Some(&names)).unwrap());
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("boards: dakai: not migrated: not names of host box on dakai: nope; until then dakai stays local-only on this host"), "{out}");
    assert!(!ops(&host).iter().any(|(op, _, _)| op == "host.migrate_v2"), "nothing applied");
    // also where the board has no names at all
    host.daemon.insert("host.migrate_preview", Ok(json!({"rows": []})));
    let mut out = Vec::new();
    assert!(!setup::migrate_review(&env, &mut host, &mut out, "dakai", false, Some(&names)).unwrap());
    assert!(String::from_utf8(out).unwrap().contains("not names of host box on dakai: desk, nope"));
    assert!(!ops(&host).iter().any(|(op, _, _)| op == "host.migrate_v2"), "nothing applied");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_review_takes_names_none_or_declines() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    enrolled(&env);
    let mut host = Fake::new(&env, &[]);
    host.daemon.insert("board.info", Ok(board_info(false)));
    host.daemon.insert("host.migrate_preview", Ok(preview()));
    host.daemon.insert("host.migrate_v2", Ok(manifest(&[])));

    // an unknown name is asked again; a "no" applies nothing
    host.answers = ["nope", "lane.v0 desk", "n"].map(String::from).into();
    let mut out = Vec::new();
    assert!(setup::migrate_review(&env, &mut host, &mut out, "dakai", true, None).unwrap());
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("boards: dakai: not names of this host: nope\n"), "{out}");
    assert!(out.contains("boards: dakai: not migrated (declined)"), "{out}");
    assert_eq!(host.prompts[2], "boards: dakai: publish lane.v0, desk; the other 0 names move to this host's local board only. Apply? [y/N] ");
    assert!(!ops(&host).iter().any(|(op, _, _)| op == "host.migrate_v2"));

    // none: the host switches with nothing published
    host.answers = ["none", "yes"].map(String::from).into();
    let mut out = Vec::new();
    assert!(setup::migrate_review(&env, &mut host, &mut out, "dakai", true, None).unwrap());
    assert!(String::from_utf8(out).unwrap().contains("switched to v2; published: nothing; on this host's local board only: 2 names"));
    assert_eq!(ops(&host).last().unwrap().2, json!({"publish": []}));
    let _ = std::fs::remove_dir_all(&root);
}

/// A switch the board applied and the daemon finished on board.info (after the human freed a
/// session): its result, the moved binding included, is printed like a fresh switch.
#[test]
fn a_switch_finished_on_board_info_prints_its_result() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    enrolled(&env);
    let mut host = Fake::new(&env, &[]);
    let mut info = board_info(true);
    info["migration"] = manifest(&["desk"]);
    host.daemon.insert("board.info", Ok(info));
    let mut out = Vec::new();
    assert!(setup::migrate_review(&env, &mut host, &mut out, "dakai", true, None).unwrap());
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        text,
        "boards: dakai: switched to v2; published: desk; on this host's local board only: 1 names\n\
         boards: dakai: lane.v0 kept its local binding (a live session holds it); its former address codex:th-7@box is in conde doctor\n\
         boards: dakai: session claude:5f2c@box moved from the local name desk.kond to desk; desk.kond keeps its messages, unbound\n\
         boards: dakai: host box (account krisz) is on v2; publish names with: conde publish <name> --board dakai\n"
    );
    assert_eq!(ops(&host).len(), 1, "no second migrate call");
}

#[test]
fn a_migrated_host_is_not_reviewed_and_an_empty_one_switches_with_nothing_published() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    enrolled(&env);
    let mut host = Fake::new(&env, &[]);
    host.daemon.insert("board.info", Ok(board_info(true)));
    let mut out = Vec::new();
    assert!(setup::migrate_review(&env, &mut host, &mut out, "dakai", true, None).unwrap());
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text, "boards: dakai: host box (account krisz) is on v2; publish names with: conde publish <name> --board dakai\n");
    assert_eq!(ops(&host).len(), 1);

    host.daemon.insert("board.info", Ok(board_info(false)));
    host.daemon.insert("host.migrate_preview", Ok(json!({"rows": []})));
    host.daemon.insert("host.migrate_v2", Ok(json!({"bindings": []})));
    let mut out = Vec::new();
    assert!(setup::migrate_review(&env, &mut host, &mut out, "dakai", false, None).unwrap());
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("no names of host box are bound there; switching it to v2 publishes nothing"), "{text}");
    assert!(host.prompts.is_empty(), "nothing to decide, nobody asked");
    assert_eq!(ops(&host).last().unwrap().0, "host.migrate_v2");

    // a daemon that does not answer leaves the board local-only and says how to resume
    host.daemon.clear();
    let mut out = Vec::new();
    assert!(!setup::migrate_review(&env, &mut host, &mut out, "dakai", false, None).unwrap());
    assert!(String::from_utf8(out).unwrap().contains("boards: dakai: not reached through the daemon (unknown op board.info); until then dakai stays local-only"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn check_reports_the_boards() {
    let root = temp_root();
    let env = env(&root, Os::Linux);
    let cred = env.v1_credential();
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    std::fs::write(&cred, "{}").unwrap();
    enrolled(&env);
    let mut host = Fake::new(&env, &[]);
    let opts = Options { boards: true, ..Options::default() };
    let (ok, out) = verify(&env, &opts, &mut host);
    assert!(!ok, "{out}");
    assert!(out.contains("ISSUE   boards: the v1 credential ~/.config/conde/credentials is not moved to boards/ yet; run: conde setup\n"), "{out}");
    assert!(out.contains("ok      boards: dakai: https://conde.test as host box (account krisz)\n"), "{out}");
    assert!(out.contains("ok      boards: default board for names and channels: local\n"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_running_bridge_that_predates_boards_is_restarted() {
    for os in [Os::Linux, Os::MacOs] {
        let root = temp_root();
        let env = env(&root, os);
        let opts = Options::select(false, false, true, false);
        let mut host = Fake::new(&env, &["systemctl"]);
        run(&env, &opts, &mut host);
        host.take_calls();
        // the unit is unchanged, the old binary still answers: no board.list
        host.alive = true;
        host.launchd_loaded = true;
        let (ok, out) = run(&env, &opts, &mut host);
        assert!(ok, "{out}");
        assert!(out.contains("service: the running bridge predates boards (an older conde); restarting it"), "{out}");
        let calls = host.take_calls();
        let restarted = match os {
            Os::Linux => calls.contains(&"systemctl --user restart conde.service".to_string()),
            Os::MacOs => calls.contains(&"launchctl kickstart -k gui/501/space.nativeai.conde".to_string()),
        };
        assert!(restarted, "{calls:?}");
        // a bridge that knows boards is left running
        host.daemon.insert("board.list", Ok(json!({"boards": [{"board": "local"}]})));
        let (_, out) = run(&env, &opts, &mut host);
        assert!(!out.contains("restarting"), "{out}");
        assert!(!host.take_calls().iter().any(|c| c.contains("restart") || c.contains("-k")));
        // until a board is enrolled that it has not loaded
        if os == Os::Linux {
            enrolled(&env);
            let (_, out) = run(&env, &opts, &mut host);
            assert!(out.contains("service: the running bridge started before dakai was enrolled; restarting it"), "{out}");
            assert!(host.take_calls().contains(&"systemctl --user restart conde.service".to_string()));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
