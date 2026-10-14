//! `conde setup`: make this machine's Claude Code and Codex sessions use
//! conde. Installs the bridge as a user service (systemd on Linux, launchd on
//! macOS, a scheduled task at logon on Windows) running the single daemon with the local board, the skills, and the MCP
//! registration for both harnesses; offers the Claude receiver setting; then the
//! host's boards (DESIGN-v2 §7, §9): the default board, and for an enrolled host
//! the one review of which earlier names stay published. Enrolling a shared board
//! is a separate step (`conde login`). Idempotent: a second run reports "unchanged".
//!
//! Everything the run reads from the machine (OS, home, config dirs, the
//! binary's path) arrives in [`Env`], and every external command goes through
//! [`Host`], so the macOS and Windows branches are exercised on Linux in tests.

use std::fmt::Write as _;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::boards::{self, BoardConfig};
use crate::protocol::v2::{BoardInfo, LOCAL_BOARD, Manifest, MigratePreview, PreviewRow, op};

pub const CLAUDE_SKILL: &str = include_str!("../../../skills/claude/SKILL.md");
pub const CODEX_SKILL: &str = include_str!("../../../skills/codex/SKILL.md");

pub const LAUNCHD_LABEL: &str = "space.nativeai.conde";
/// The Windows scheduled task that runs the bridge at logon.
pub const TASK_NAME: &str = "conde";
/// The last step of every setup (owner's decision): report how it went.
pub const FEEDBACK: &str = "Send a DM to kdobmayer/desk with how the setup went: what worked, what failed, what you \
had to figure out yourself, roughly how long it took, and the OS.";
pub const FEEDBACK_COMMAND: &str = "conde send kdobmayer/desk \"<how the setup went>\" --board dakai";
pub const FEEDBACK_KEEP: &str = "desk is on the team's board: the name you send it from must be published there \
first (conde publish <name> --board dakai, when your human told you to publish it); otherwise, or on a host without \
that board, tell your human instead. Keep the name registered: desk replies to it.";
pub const GUIDE: &str = "Setup guide: https://conde.nativeai.space/readme";
/// What enrolling a shared board does and does not do, for the human who runs it.
pub const ENROLL_SCOPE: &str = "Enrolling publishes nothing and uploads no history: every agent stays on this \
machine's local board, and teammates cannot reach one until you publish it";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

/// What setup needs on Windows only.
#[derive(Clone, Debug)]
pub struct WindowsEnv {
    /// `%APPDATA%`: conde's config (board files, the default board).
    pub appdata: PathBuf,
    /// `%LOCALAPPDATA%`: the local board and the bridge's log.
    pub local_appdata: PathBuf,
    /// The user's SID, whose logon starts the task and who it runs as (a SID, not
    /// `DOMAIN\user`: Entra-joined machines name users `AzureAD\...`).
    pub user_sid: String,
}

/// The machine as setup sees it.
#[derive(Clone, Debug)]
pub struct Env {
    pub os: Os,
    pub home: PathBuf,
    pub uid: u32,
    /// `$XDG_CONFIG_HOME` (Linux service unit location).
    pub xdg_config_home: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR`, Claude Code's override of `~/.claude`.
    pub claude_config_dir: Option<PathBuf>,
    /// `$CODEX_HOME`, Codex's override of `~/.codex`.
    pub codex_home: Option<PathBuf>,
    /// `$XDG_DATA_HOME` and `$CONDE_DATA`, passed on to the service as found.
    pub xdg_data_home: Option<PathBuf>,
    pub conde_data: Option<PathBuf>,
    /// Absolute path of the conde binary the service and MCP entries run.
    pub exe: PathBuf,
    /// `$PATH` handed to the service (the bridge runs `codex` from it).
    pub path: String,
    /// The socket the CLI resolves; pinned into the launchd agent, whose
    /// environment need not carry the shell's `$TMPDIR`, and into every MCP entry.
    pub socket: PathBuf,
    /// The socket a conde process resolves without `$CONDE_SOCK` (an MCP entry without one).
    pub default_socket: PathBuf,
    /// Set when `os` is Windows.
    pub windows: Option<WindowsEnv>,
}

impl Env {
    pub fn from_process() -> anyhow::Result<Env> {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = var(home_var).ok_or_else(|| anyhow!("{home_var} is not set"))?;
        let exe = std::env::current_exe().context("locating the conde binary")?;
        #[cfg(unix)]
        let uid = crate::util::own_uid();
        #[cfg(not(unix))]
        let uid = 0;
        #[cfg(windows)]
        let windows = Some(WindowsEnv {
            appdata: var("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming")),
            local_appdata: var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local")),
            user_sid: conde_transport::win::own_user_sid().context("reading this user's SID")?,
        });
        #[cfg(not(windows))]
        let windows = None;
        // a Windows path from current_exe() is used as is: canonicalize() gives a \\?\ path
        let exe = if cfg!(windows) { exe } else { exe.canonicalize().unwrap_or(exe) };
        Ok(Env {
            os: if cfg!(target_os = "macos") {
                Os::MacOs
            } else if cfg!(windows) {
                Os::Windows
            } else {
                Os::Linux
            },
            home,
            uid,
            xdg_config_home: var("XDG_CONFIG_HOME"),
            claude_config_dir: var("CLAUDE_CONFIG_DIR"),
            codex_home: var("CODEX_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            conde_data: var("CONDE_DATA"),
            exe,
            path: std::env::var("PATH").unwrap_or_default(),
            socket: crate::paths::socket_path(),
            default_socket: crate::paths::default_socket_path(),
            windows,
        })
    }

    /// The environment the service needs to resolve the same credential,
    /// board, socket and harness directories as the shell that ran setup.
    pub fn service_env(&self) -> Vec<(&'static str, String)> {
        let mut vars = vec![("PATH", self.path.clone()), ("CONDE_SOCK", self.socket.to_string_lossy().into_owned())];
        for (name, dir) in [
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("XDG_DATA_HOME", &self.xdg_data_home),
            ("CONDE_DATA", &self.conde_data),
            ("CLAUDE_CONFIG_DIR", &self.claude_config_dir),
            ("CODEX_HOME", &self.codex_home),
        ] {
            if let Some(d) = dir {
                vars.push((name, d.to_string_lossy().into_owned()));
            }
        }
        vars
    }

    fn claude_dir(&self) -> PathBuf {
        self.claude_config_dir.clone().unwrap_or_else(|| self.home.join(".claude"))
    }

    /// User-scope MCP servers live in `~/.claude.json`, or inside the
    /// config dir when `CLAUDE_CONFIG_DIR` relocates it.
    pub fn claude_json(&self) -> PathBuf {
        match &self.claude_config_dir {
            Some(d) => d.join(".claude.json"),
            None => self.home.join(".claude.json"),
        }
    }

    pub fn claude_settings(&self) -> PathBuf {
        self.claude_dir().join("settings.json")
    }

    pub fn claude_skill(&self) -> PathBuf {
        self.claude_skill_for(MCP_NAME)
    }

    /// The skill of the instance registered as MCP server `name` (`skills/<name>/SKILL.md`).
    pub fn claude_skill_for(&self, name: &str) -> PathBuf {
        self.claude_dir().join("skills").join(name).join("SKILL.md")
    }

    fn codex_dir(&self) -> PathBuf {
        self.codex_home.clone().unwrap_or_else(|| self.home.join(".codex"))
    }

    pub fn codex_config(&self) -> PathBuf {
        self.codex_dir().join("config.toml")
    }

    pub fn codex_skill(&self) -> PathBuf {
        self.codex_skill_for(MCP_NAME)
    }

    pub fn codex_skill_for(&self, name: &str) -> PathBuf {
        self.codex_dir().join("skills").join(name).join("SKILL.md")
    }

    /// `$XDG_CONFIG_HOME/conde` (else `~/.config/conde`; Windows `%APPDATA%\conde`): board
    /// files and the default board.
    pub fn conde_config(&self) -> PathBuf {
        match (&self.xdg_config_home, &self.windows) {
            (Some(x), _) => x.join("conde"),
            (None, Some(w)) => w.appdata.join("conde"),
            (None, None) => self.home.join(".config").join("conde"),
        }
    }

    pub fn boards_dir(&self) -> PathBuf {
        self.conde_config().join("boards")
    }

    pub fn default_board(&self) -> PathBuf {
        self.conde_config().join("default-board")
    }

    /// The v1 credential, moved to `boards/<alias>.json` by setup.
    pub fn v1_credential(&self) -> PathBuf {
        self.conde_config().join("credentials")
    }

    pub fn systemd_unit(&self) -> PathBuf {
        self.xdg_config_home.clone().unwrap_or_else(|| self.home.join(".config")).join("systemd/user/conde.service")
    }

    pub fn launchd_plist(&self) -> PathBuf {
        self.home.join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist"))
    }

    pub fn launchd_logs(&self) -> PathBuf {
        self.home.join("Library/Logs/conde")
    }

    /// Windows: the local board's directory, as the daemon resolves it.
    fn windows_data(&self) -> PathBuf {
        match &self.conde_data {
            Some(d) => d.clone(),
            None => crate::paths::windows_data_dir(self.windows.as_ref().map(|w| w.local_appdata.clone()), Some(self.home.clone())),
        }
    }

    /// Windows: the task definition setup registers (`schtasks /Create /XML`).
    pub fn windows_task(&self) -> PathBuf {
        self.windows_data().with_file_name("conde-task.xml")
    }

    /// Windows: where the bridge writes its log once it gives up its console.
    pub fn windows_log(&self) -> PathBuf {
        self.windows_data().join("conde.log")
    }

    fn show(&self, p: &Path) -> String {
        match p.strip_prefix(&self.home) {
            Ok(rest) if self.os == Os::Windows => format!("%USERPROFILE%\\{}", rest.display()),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => p.display().to_string(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub claude: bool,
    pub codex: bool,
    pub service: bool,
    /// The host's boards: the v1 credential move, the migration review, the default board.
    pub boards: bool,
    /// Leave Claude's crossSessionInbound as it is (default: an unset value becomes "accept").
    pub keep_receiver_policy: bool,
    /// Set the default board without asking.
    pub default_board: Option<String>,
    /// What stays published when an enrolled host switches to v2; None: asked at a terminal,
    /// nothing decided without one.
    pub publish: Option<Publish>,
}

/// Which of an enrolled host's v1 names stay published at its switch to v2 (`--publish*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Publish {
    /// Exactly these names (`--publish a,b`).
    Names(Vec<String>),
    /// The names the preview marks as candidates: messages with another owner's agents.
    Candidates,
    /// Every name bound on the board.
    All,
    /// None: the host switches publishing nothing.
    Nothing,
}

impl Publish {
    fn flag(&self) -> String {
        match self {
            Publish::Names(names) => format!("--publish {}", names.join(",")),
            Publish::Candidates => "--publish-candidates".into(),
            Publish::All => "--publish-all".into(),
            Publish::Nothing => "--publish-none".into(),
        }
    }

    /// The preview rows this choice publishes; Err names the ones the board does not have.
    fn rows<'a>(&self, rows: &'a [PreviewRow]) -> Result<Vec<&'a PreviewRow>, Vec<String>> {
        match self {
            Publish::Candidates => Ok(rows.iter().filter(|r| r.candidate).collect()),
            Publish::All => Ok(rows.iter().collect()),
            Publish::Nothing => Ok(Vec::new()),
            Publish::Names(names) => {
                let mut chosen: Vec<&PreviewRow> = Vec::new();
                let mut unknown = Vec::new();
                for typed in names {
                    let bare = typed.rsplit('/').next().unwrap_or(typed);
                    match rows.iter().find(|r| r.name.eq_ignore_ascii_case(bare)) {
                        Some(r) if !chosen.iter().any(|c| c.agent_id == r.agent_id) => chosen.push(r),
                        Some(_) => {}
                        None => unknown.push(typed.clone()),
                    }
                }
                if unknown.is_empty() { Ok(chosen) } else { Err(unknown) }
            }
        }
    }
}

impl Options {
    /// No part named (or `--all`) means every part.
    pub fn select(claude: bool, codex: bool, service: bool, all: bool) -> Options {
        let every = all || !(claude || codex || service);
        Options {
            claude: claude || every,
            codex: codex || every,
            service: service || every,
            boards: every,
            ..Options::default()
        }
    }
}

pub struct CmdOut {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The outside world: programs on PATH and the person at the terminal.
pub trait Host {
    fn which(&self, prog: &str) -> Option<PathBuf>;
    fn run(&mut self, prog: &str, args: &[&str]) -> anyhow::Result<CmdOut>;
    /// Whether a daemon answers on `socket`: connect and one request frame,
    /// bounded by a short timeout. Err carries why not.
    fn reach(&mut self, socket: &Path) -> Result<(), String>;
    /// Whether a person is at a terminal (stdin is one); false in an agent's shell or a script.
    fn terminal(&self) -> bool;
    /// A line the human typed after `prompt`; None without a terminal or at end of input.
    fn ask(&mut self, prompt: &str) -> Option<String>;
    /// One request to the local daemon on `socket`, waiting briefly for a service that is
    /// starting. Err carries the daemon's refusal or why it was not reached.
    fn daemon(&mut self, socket: &Path, op: &str, board: Option<&str>, body: Value) -> Result<Value, String>;
    /// `board.info` on a shared board with a host key (over HTTPS, not through the daemon).
    fn board_info(&mut self, server: &str, key: &str) -> anyhow::Result<BoardInfo>;
}

/// The real machine.
pub struct System;

/// The probe frame: any response, even a refusal, proves a live daemon.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl Host for System {
    fn reach(&mut self, socket: &Path) -> Result<(), String> {
        use std::io::{BufRead, BufReader};
        #[cfg_attr(unix, allow(unused_mut))]
        let mut stream = crate::ipc::connect(socket).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(PROBE_TIMEOUT)).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(PROBE_TIMEOUT)).map_err(|e| e.to_string())?;
        let req = crate::protocol::Request::new(crate::util::new_ulid(), "agent.whoami", json!({}));
        let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
        line.push(b'\n');
        (&stream).write_all(&line).map_err(|e| e.to_string())?;
        let mut answer = String::new();
        match BufReader::new(&stream).read_line(&mut answer) {
            Ok(0) => Err("the daemon closed the connection without answering".into()),
            Ok(_) => serde_json::from_str::<crate::protocol::Response>(&answer)
                .map(|_| ())
                .map_err(|e| format!("unreadable answer: {e}")),
            Err(e) => Err(format!("no answer within {PROBE_TIMEOUT:?}: {e}")),
        }
    }

    #[cfg(unix)]
    fn which(&self, prog: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| {
            std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
        })
    }

    /// Windows: `prog` with each `%PATHEXT%` extension (`claude.exe`, `claude.cmd`).
    #[cfg(windows)]
    fn which(&self, prog: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::env::split_paths(&path)
            .flat_map(|d| exts.split(';').filter(|e| !e.is_empty()).map(move |e| d.join(format!("{prog}{e}"))))
            .find(|p| p.is_file())
    }

    fn run(&mut self, prog: &str, args: &[&str]) -> anyhow::Result<CmdOut> {
        let out = std::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("running {prog}"))?;
        Ok(CmdOut {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    #[cfg(unix)]
    fn terminal(&self) -> bool {
        // SAFETY: isatty on the process's own stdin descriptor.
        unsafe { libc::isatty(0) == 1 }
    }

    #[cfg(not(unix))]
    fn terminal(&self) -> bool {
        std::io::IsTerminal::is_terminal(&std::io::stdin())
    }

    fn ask(&mut self, prompt: &str) -> Option<String> {
        use std::io::BufRead;
        if !self.terminal() {
            return None;
        }
        print!("{prompt}");
        std::io::stdout().flush().ok()?;
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_string()),
        }
    }

    fn daemon(&mut self, socket: &Path, op: &str, board: Option<&str>, body: Value) -> Result<Value, String> {
        let deadline = std::time::Instant::now() + DAEMON_WAIT;
        let mut client = loop {
            match crate::client::Client::connect(socket) {
                Ok(c) => break c,
                Err(e) if std::time::Instant::now() >= deadline => return Err(format!("{e:#}")),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        };
        let mut req = crate::protocol::Request::new(crate::util::new_ulid(), op, body);
        req.idempotency_key = Some(crate::util::new_ulid());
        req.board = board.map(Into::into);
        req.actor = crate::identity::derive_address(&crate::identity::Markers::from_env(), crate::identity::harness_ancestor).ok();
        let resp = client.call(&req).map_err(|e| format!("{e:#}"))?;
        match (resp.ok, resp.result, resp.error) {
            (true, Some(r), _) => Ok(r),
            (_, _, Some(e)) => Err(e.message),
            _ => Err("the daemon answered without a result".into()),
        }
    }

    fn board_info(&mut self, server: &str, key: &str) -> anyhow::Result<BoardInfo> {
        boards::board_info(server, key)
    }
}

/// How long setup waits for the daemon it just (re)started to answer.
const DAEMON_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Created,
    Updated,
    Unchanged,
}

impl Change {
    fn verb(self) -> &'static str {
        match self {
            Change::Created => "wrote",
            Change::Updated => "updated",
            Change::Unchanged => "unchanged",
        }
    }
}

/// Write `content` unless the file already holds it; atomic, keeps an
/// existing file's mode, creates parents.
pub fn write_if_changed(path: &Path, content: &str) -> anyhow::Result<Change> {
    write_bytes_if_changed(path, content.as_bytes())
}

/// [`write_if_changed`] for bytes (the UTF-16 task definition).
pub fn write_bytes_if_changed(path: &Path, content: &[u8]) -> anyhow::Result<Change> {
    let old = match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if old.as_deref() == Some(content) {
        return Ok(Change::Unchanged);
    }
    let dir = path.parent().ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.conde-setup.{}", std::process::id()));
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(if old.is_some() { Change::Updated } else { Change::Created })
}

/// Run the selected parts; Ok(false) when a part failed (each is reported).
pub fn run(env: &Env, opts: &Options, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<bool> {
    let mut ok = true;
    let mut part = |name: &str, r: anyhow::Result<()>, out: &mut dyn Write| -> anyhow::Result<()> {
        if let Err(e) = r {
            ok = false;
            writeln!(out, "{name}: FAILED: {e:#}")?;
        }
        Ok(())
    };
    if opts.service || opts.boards {
        // before the service (re)starts, so the daemon finds the board under its alias
        let r = adopt_v1_credential(env, host, out);
        part("boards", r, out)?;
    }
    if opts.service {
        let r = match env.os {
            Os::Linux => systemd(env, host, out),
            Os::MacOs => launchd(env, host, out),
            Os::Windows => schtasks(env, host, out),
        };
        part("service", r, out)?;
    }
    if opts.claude {
        if claude_installed(env, host) {
            let r = claude(env, opts, host, out);
            part("claude", r, out)?;
        } else {
            writeln!(out, "skip    {CLAUDE_ABSENT}")?;
        }
    }
    if opts.codex {
        if codex_installed(env, host) {
            let r = codex(env, out);
            part("codex", r, out)?;
        } else {
            writeln!(out, "skip    {CODEX_ABSENT}")?;
        }
    }
    if opts.boards {
        let r = boards_part(env, opts, host, out);
        part("boards", r, out)?;
    }
    writeln!(out, "\n{FEEDBACK}\n  {FEEDBACK_COMMAND}\n{FEEDBACK_KEEP}\n\n{GUIDE}")?;
    Ok(ok)
}

/// `conde setup --check`: compare this machine with what `conde setup` would
/// make it, without changing anything. A harness that is not installed is
/// skipped. Ok(false) when any line is an ISSUE.
pub fn verify(env: &Env, opts: &Options, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<bool> {
    let mut issues = 0;
    let mut line = |kind: &str, text: String, out: &mut dyn Write| -> anyhow::Result<()> {
        if kind == "ISSUE" {
            issues += 1;
        }
        writeln!(out, "{kind:<8}{text}")?;
        Ok(())
    };
    let file = |path: &Path, want: &[u8]| -> anyhow::Result<Option<&'static str>> {
        Ok(match std::fs::read(path) {
            Ok(b) if b == want => None,
            Ok(_) => Some("differs from what conde setup writes"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some("missing"),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        })
    };
    let fix = "; run: conde setup";
    writeln!(out, "conde {}", crate::VERSION)?;

    if opts.service {
        let (path, want) = match env.os {
            Os::Linux => (env.systemd_unit(), systemd_unit(env).into_bytes()),
            Os::MacOs => (env.launchd_plist(), launchd_plist(env).into_bytes()),
            Os::Windows => (env.windows_task(), utf16_file(&task_xml(env))),
        };
        match file(&path, &want)? {
            None => line("ok", format!("service: {} up to date", env.show(&path)), out)?,
            Some(why) => line("ISSUE", format!("service: {} {why}{fix}", env.show(&path)), out)?,
        }
        let socket = env.show(&env.socket);
        match host.reach(&env.socket) {
            Ok(()) => {
                line("ok", format!("service: the bridge answers on {socket}"), out)?;
                match bridge_build(env, host) {
                    BridgeBuild::Same => line("ok", format!("service: the bridge runs this build ({})", crate::VERSION), out)?,
                    BridgeBuild::Other(b) => line(
                        "ISSUE",
                        format!(
                            "service: the bridge runs another build ({b}) than this binary ({}); run: conde setup (it restarts the bridge)",
                            crate::VERSION
                        ),
                        out,
                    )?,
                    BridgeBuild::Unreported => line(
                        "ISSUE",
                        format!(
                            "service: the bridge does not report its build (older than {}); run: conde setup (it restarts the bridge)",
                            crate::VERSION
                        ),
                        out,
                    )?,
                    BridgeBuild::Unknown => {}
                }
            }
            Err(why) => {
                let state = service_state(env, host)?;
                line(
                    "ISSUE",
                    format!("service: the bridge does not answer on {socket} ({why}); {state}; see Troubleshooting in README.md"),
                    out,
                )?
            }
        }
    }

    let exe = env.exe.to_string_lossy().into_owned();
    if opts.claude {
        if !claude_installed(env, host) {
            line("skip", CLAUDE_ABSENT.into(), out)?;
        } else {
            let (name, displaced) = claude_name(env)?;
            if let Some(what) = displaced {
                line("note", format!("claude: MCP server {MCP_NAME} belongs to another conde instance ({what}); this one is {name}"), out)?;
            }
            let skill = env.claude_skill_for(&name);
            match file(&skill, skill_text(CLAUDE_SKILL, &name).as_bytes())? {
                None => line("ok", format!("claude: skill {} up to date", env.show(&skill)), out)?,
                Some(why) => line("ISSUE", format!("claude: skill {} {why}{fix}", env.show(&skill)), out)?,
            }
            let entry = read_json(&env.claude_json())?["mcpServers"][name.as_str()].clone();
            if entry["command"] == exe.as_str() && entry["args"] == json!(["mcp"]) && entry["env"]["CONDE_SOCK"] == *env.socket.to_string_lossy() {
                line("ok", format!("claude: MCP server {name} registered at user scope ({exe} mcp)"), out)?;
            } else {
                line("ISSUE", format!("claude: MCP server {name} not registered for {exe}{fix}"), out)?;
            }
            let inbound = read_json(&env.claude_settings())?["crossSessionInbound"].clone();
            let now = if inbound.is_null() { "unset".to_string() } else { inbound.to_string() };
            let effect = if inbound == "accept" {
                "bypass-mode sessions receive conde notifications without approval"
            } else if inbound.is_null() {
                "bypass-mode sessions hold conde notifications; conde setup sets \"accept\""
            } else {
                "an explicit setting, left as is by conde setup"
            };
            line("note", format!("claude: crossSessionInbound is {now} ({effect})"), out)?;
        }
    }
    if opts.codex {
        if !codex_installed(env, host) {
            line("skip", CODEX_ABSENT.into(), out)?;
        } else {
            let config = env.codex_config();
            let text = read_text(&config)?;
            let (name, displaced) = codex_name(env, &text).unwrap_or_else(|_| (MCP_NAME.to_string(), None));
            if let Some(what) = displaced {
                line("note", format!("codex: MCP server {MCP_NAME} belongs to another conde instance ({what}); this one is {name}"), out)?;
            }
            let skill = env.codex_skill_for(&name);
            match file(&skill, skill_text(CODEX_SKILL, &name).as_bytes())? {
                None => line("ok", format!("codex: skill {} up to date", env.show(&skill)), out)?,
                Some(why) => line("ISSUE", format!("codex: skill {} {why}{fix}", env.show(&skill)), out)?,
            }
            match codex_config_edit(&text, &name, &exe, &env.socket.to_string_lossy()) {
                Ok(None) => line("ok", format!("codex: MCP server {name} registered in {} ({exe} mcp)", env.show(&config)), out)?,
                Ok(Some(_)) => line("ISSUE", format!("codex: MCP server {name} not registered for {exe} in {}{fix}", env.show(&config)), out)?,
                Err(e) => line("ISSUE", format!("codex: {}: {e:#}", env.show(&config)), out)?,
            }
        }
    }
    if opts.boards {
        let v1 = env.v1_credential();
        if v1.exists() {
            line("ISSUE", format!("boards: the v1 credential {} is not moved to boards/ yet; run: conde setup", env.show(&v1)), out)?;
        }
        let enrolled = boards::enrolled_in(&env.boards_dir())?;
        if enrolled.is_empty() {
            line("note", "boards: no shared board enrolled; this host works on its local board".into(), out)?;
        }
        for alias in &enrolled {
            let path = env.boards_dir().join(format!("{alias}.json"));
            match boards::load(&path) {
                Ok(cfg) => line("ok", format!("boards: {alias}: {} as host {} (account {})", cfg.server, cfg.host_id, cfg.owner), out)?,
                Err(e) => line("ISSUE", format!("boards: {alias}: {e:#}"), out)?,
            }
        }
        match boards::read_default(&env.default_board()) {
            Ok(d) => line("ok", format!("boards: default board for names and channels: {d}"), out)?,
            Err(e) => line("ISSUE", format!("boards: {e:#}"), out)?,
        }
    }
    if issues == 0 {
        writeln!(out, "all clear")?;
    }
    Ok(issues == 0)
}

/// What the service manager says about the bridge, for an ISSUE line.
fn service_state(env: &Env, host: &mut dyn Host) -> anyhow::Result<String> {
    let field = |text: &str, key: &str| {
        text.lines().find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
    };
    Ok(match env.os {
        Os::Windows => {
            let r = host.run("schtasks", &["/Query", "/TN", TASK_NAME, "/FO", "LIST", "/V"])?;
            if !r.ok {
                format!("task scheduler: no task {TASK_NAME}")
            } else {
                let state = field(&r.stdout, "Status:").unwrap_or_else(|| "unknown".into());
                let last = field(&r.stdout, "Last Result:").unwrap_or_else(|| "unknown".into());
                format!("task scheduler: {TASK_NAME} status {state}, last result {last}")
            }
        }
        Os::MacOs => {
            let r = host.run("launchctl", &["print", &format!("gui/{}/{LAUNCHD_LABEL}", env.uid)])?;
            if !r.ok {
                format!("launchd: {LAUNCHD_LABEL} is not loaded")
            } else {
                let state = field(&r.stdout, "state = ").unwrap_or_else(|| "unknown".into());
                let exit = field(&r.stdout, "last exit code = ").unwrap_or_else(|| "unknown".into());
                format!("launchd: state = {state}, last exit code = {exit}")
            }
        }
        Os::Linux if host.which("systemctl").is_none() => "systemctl not found".to_string(),
        Os::Linux => {
            let r = host.run("systemctl", &["--user", "show", "-p", "ActiveState,ExecMainStatus", "conde.service"])?;
            let state = field(&r.stdout, "ActiveState=").unwrap_or_else(|| "unknown".into());
            let exit = field(&r.stdout, "ExecMainStatus=").unwrap_or_else(|| "unknown".into());
            format!("systemd: ActiveState={state}, ExecMainStatus={exit}")
        }
    })
}

/// One daemon shape on every machine: the local board plus every enrolled shared board.
fn daemon_args() -> Vec<String> {
    vec!["daemon".to_string()]
}

/// One systemd unit-file word: quoted when needed, `%` and `$` escaped.
fn systemd_word(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
    if escaped.is_empty() || escaped.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\' || c == ';') {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// One quoted `Environment=` assignment: `$` is literal there, `%` starts
/// a specifier.
fn systemd_env(name: &str, val: &str) -> String {
    let escaped = val.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    format!("\"{name}={escaped}\"")
}

pub fn systemd_unit(env: &Env) -> String {
    let environment: String =
        env.service_env().iter().map(|(k, v)| format!("Environment={}\n", systemd_env(k, v))).collect();
    let mut exec = systemd_word(&env.exe.to_string_lossy());
    for a in daemon_args() {
        exec.push(' ');
        exec.push_str(&systemd_word(&a));
    }
    format!(
        "# written by `conde setup`; rerun it instead of editing\n\
         [Unit]\n\
         Description=conde bridge\n\
         After=network.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         {environment}\
         Restart=always\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
    )
}

fn check(host: &mut dyn Host, prog: &str, args: &[&str]) -> anyhow::Result<()> {
    let r = host.run(prog, args)?;
    if !r.ok {
        bail!("`{prog} {}` failed: {}", args.join(" "), r.stderr.trim());
    }
    Ok(())
}

fn systemd(env: &Env, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let unit = env.systemd_unit();
    let change = write_if_changed(&unit, &systemd_unit(env))?;
    writeln!(out, "service: {} {}", change.verb(), env.show(&unit))?;
    if host.which("systemctl").is_none() {
        writeln!(out, "service: systemctl not found; start the bridge yourself: conde daemon")?;
        return Ok(());
    }
    if change != Change::Unchanged {
        check(host, "systemctl", &["--user", "daemon-reload"])?;
    }
    check(host, "systemctl", &["--user", "enable", "conde.service"])?;
    if change == Change::Updated || predates_boards(env, host, out)? {
        check(host, "systemctl", &["--user", "restart", "conde.service"])?;
        writeln!(out, "service: enabled and restarted conde.service")?;
    } else {
        check(host, "systemctl", &["--user", "start", "conde.service"])?;
        writeln!(out, "service: enabled and started conde.service")?;
    }
    writeln!(out, "service: without a desktop login session, also run: loginctl enable-linger \"$USER\"")?;
    Ok(())
}

/// What the running bridge says of its build (the `build` of its `doctor` answer).
enum BridgeBuild {
    /// This binary's.
    Same,
    /// Another build: the binary was replaced under the running service.
    Other(String),
    /// A daemon too old to report one.
    Unreported,
    /// No answer to tell by.
    Unknown,
}

fn bridge_build(env: &Env, host: &mut dyn Host) -> BridgeBuild {
    match host.daemon(&env.socket, "doctor", None, json!({})) {
        Err(_) => BridgeBuild::Unknown,
        Ok(r) => match r["build"].as_str() {
            Some(b) if b == crate::VERSION => BridgeBuild::Same,
            Some(b) => BridgeBuild::Other(b.to_string()),
            None => BridgeBuild::Unreported,
        },
    }
}

/// Whether the running bridge must restart to serve this machine's boards: it is an older
/// conde that does not know boards or runs another build (the binary was replaced under it),
/// or it started before a board was enrolled.
fn predates_boards(env: &Env, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<bool> {
    if host.reach(&env.socket).is_err() {
        return Ok(false);
    }
    let mut why = match host.daemon(&env.socket, crate::surface::BOARD_LIST, None, json!({})) {
        Err(e) if crate::surface::predates_boards(&e) => Some("predates boards (an older conde)".to_string()),
        Err(_) => None,
        Ok(r) => {
            let known: Vec<&str> = r["boards"].as_array().into_iter().flatten().filter_map(|b| b["board"].as_str()).collect();
            let missing: Vec<String> =
                boards::enrolled_in(&env.boards_dir())?.into_iter().filter(|a| !known.contains(&a.as_str())).collect();
            (!missing.is_empty()).then(|| format!("started before {} was enrolled", missing.join(", ")))
        }
    };
    if why.is_none() {
        why = match bridge_build(env, host) {
            BridgeBuild::Other(b) => Some(format!("runs another build ({b}) than this binary ({})", crate::VERSION)),
            BridgeBuild::Unreported => Some(format!("does not report its build (older than {})", crate::VERSION)),
            BridgeBuild::Same | BridgeBuild::Unknown => None,
        };
    }
    if let Some(why) = &why {
        writeln!(out, "service: the running bridge {why}; restarting it")?;
    }
    Ok(why.is_some())
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn launchd_plist(env: &Env) -> String {
    let mut args = format!("    <string>{}</string>\n", xml(&env.exe.to_string_lossy()));
    for a in daemon_args() {
        let _ = writeln!(args, "    <string>{}</string>", xml(&a));
    }
    let log = xml(&env.launchd_logs().join("conde.log").to_string_lossy());
    let environment: String = env
        .service_env()
        .iter()
        .map(|(k, v)| format!("    <key>{k}</key>\n    <string>{}</string>\n", xml(v)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- written by `conde setup`; rerun it instead of editing -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>EnvironmentVariables</key>
  <dict>
{environment}  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#
    )
}

fn launchd(env: &Env, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let logs = env.launchd_logs();
    std::fs::create_dir_all(&logs).with_context(|| format!("creating {}", logs.display()))?;
    let plist = env.launchd_plist();
    let change = write_if_changed(&plist, &launchd_plist(env))?;
    writeln!(out, "service: {} {}", change.verb(), env.show(&plist))?;
    let domain = format!("gui/{}", env.uid);
    let target = format!("{domain}/{LAUNCHD_LABEL}");
    let plist_s = plist.to_string_lossy().into_owned();
    let loaded = host.run("launchctl", &["print", &target])?.ok;
    let stale = loaded && change == Change::Unchanged && predates_boards(env, host, out)?;
    match (loaded, change) {
        (true, Change::Unchanged) if stale => {
            check(host, "launchctl", &["kickstart", "-k", &target])?;
            writeln!(out, "service: restarted {LAUNCHD_LABEL}")?;
        }
        (true, Change::Unchanged) => {
            check(host, "launchctl", &["kickstart", &target])?;
            writeln!(out, "service: {LAUNCHD_LABEL} already loaded")?;
        }
        (true, _) => {
            check(host, "launchctl", &["bootout", &target])?;
            check(host, "launchctl", &["bootstrap", &domain, &plist_s])?;
            writeln!(out, "service: reloaded {LAUNCHD_LABEL}")?;
        }
        (false, _) => {
            check(host, "launchctl", &["bootstrap", &domain, &plist_s])?;
            writeln!(out, "service: loaded {LAUNCHD_LABEL} (starts at login)")?;
        }
    }
    writeln!(out, "service: logs in {}", env.show(&logs.join("conde.log")))?;
    Ok(())
}

fn xml_utf16_decl(body: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n{body}")
}

/// The task definition: `conde daemon` at this user's logon, as this user in the
/// interactive session (where Claude Code's session-local pipes are), no time limit,
/// also on battery, one instance.
pub fn task_xml(env: &Env) -> String {
    let user = xml(env.windows.as_ref().map_or("", |w| w.user_sid.as_str()));
    let exe = xml(&env.exe.to_string_lossy());
    let args = xml(&daemon_args().join(" "));
    xml_utf16_decl(&format!(
        r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>conde bridge; written by conde setup, rerun it instead of editing</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    ))
}

/// UTF-16LE with a byte order mark and CRLF line ends, as Task Scheduler writes its files.
pub fn utf16_file(text: &str) -> Vec<u8> {
    let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let mut out = vec![0xFF, 0xFE];
    out.extend(crlf.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

/// `/End` then `/Run`, done only when both succeeded and the bridge answers again; else
/// Err naming each step that failed. End is a hard stop; the local board's SQLite survives it.
fn restart_task(env: &Env, host: &mut dyn Host) -> anyhow::Result<()> {
    let mut failed = Vec::new();
    for verb in ["/End", "/Run"] {
        let r = host.run("schtasks", &[verb, "/TN", TASK_NAME])?;
        if !r.ok {
            failed.push(format!("`schtasks {verb} /TN {TASK_NAME}` failed: {}", r.stderr.trim()));
        }
    }
    if failed.is_empty() {
        let deadline = std::time::Instant::now() + DAEMON_WAIT;
        while let Err(why) = host.reach(&env.socket) {
            if std::time::Instant::now() >= deadline {
                failed.push(format!("the bridge does not answer on {} afterwards ({why})", env.show(&env.socket)));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    if !failed.is_empty() {
        bail!("restart of {TASK_NAME} incomplete: {}", failed.join("; "));
    }
    Ok(())
}

fn schtasks(env: &Env, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let file = env.windows_task();
    let change = write_bytes_if_changed(&file, &utf16_file(&task_xml(env)))?;
    writeln!(out, "service: {} {}", change.verb(), env.show(&file))?;
    let file_s = file.to_string_lossy().into_owned();
    let registered = host.run("schtasks", &["/Query", "/TN", TASK_NAME])?.ok;
    if change != Change::Unchanged || !registered {
        check(host, "schtasks", &["/Create", "/TN", TASK_NAME, "/XML", &file_s, "/F"])?;
        writeln!(out, "service: registered the scheduled task {TASK_NAME} (runs conde daemon at your logon)")?;
    }
    let running = host.reach(&env.socket).is_ok();
    if running && (change == Change::Updated || predates_boards(env, host, out)?) {
        restart_task(env, host)?;
        writeln!(out, "service: restarted {TASK_NAME}")?;
    } else if running {
        writeln!(out, "service: {TASK_NAME} already running")?;
    } else {
        check(host, "schtasks", &["/Run", "/TN", TASK_NAME])?;
        writeln!(out, "service: started {TASK_NAME}")?;
    }
    let inherited: Vec<&str> = env
        .service_env()
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| *k != "PATH" && (*k != "CONDE_SOCK" || env.socket != env.default_socket))
        .collect();
    if !inherited.is_empty() {
        writeln!(
            out,
            "service: the task gets your logon environment, not this shell's: set {} persistently (setx NAME value) \
             so the bridge resolves the same directories, then run conde setup again",
            inherited.join(", ")
        )?;
    }
    writeln!(out, "service: logs in {}", env.show(&env.windows_log()))?;
    Ok(())
}

/// The switch of a host enrolled under v1: its `conde/credentials` becomes
/// `boards/<alias>.json`, the alias being the name the board advertises, pinned to the
/// account `board.info` reports. The file moves only once that board answered.
fn adopt_v1_credential(env: &Env, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let v1 = env.v1_credential();
    if !v1.exists() {
        return Ok(());
    }
    let creds = crate::credentials::load(&v1)?;
    let info = match host.board_info(&creds.server, &creds.key) {
        Ok(info) => info,
        Err(e) => {
            writeln!(
                out,
                "boards: the v1 credential {} stays where it is: {} did not answer board.info ({e:#}); run conde setup \
                 again once it is reachable (the local board works meanwhile)",
                env.show(&v1),
                creds.server
            )?;
            return Ok(());
        }
    };
    boards::check_shared_alias(&info.name)?;
    let path = env.boards_dir().join(format!("{}.json", info.name));
    let cfg = BoardConfig { server: creds.server.clone(), host_id: creds.host_id.clone(), owner: info.owner.clone(), key: creds.key };
    boards::store(&path, &info.name, &cfg)?;
    std::fs::remove_file(&v1).with_context(|| format!("removing {}", v1.display()))?;
    writeln!(
        out,
        "boards: moved the v1 credential {} to {} (board {}, account {}, host {})",
        env.show(&v1),
        env.show(&path),
        info.name,
        info.owner,
        cfg.host_id
    )?;
    Ok(())
}

/// The boards part of `conde setup`: each enrolled board's review, then the default board.
fn boards_part(env: &Env, opts: &Options, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let enrolled = boards::enrolled_in(&env.boards_dir())?;
    if enrolled.is_empty() {
        writeln!(
            out,
            "boards: no shared board enrolled; this host works on its local board. Joining a team's board is a separate \
             step: conde login --server https://conde.nativeai.space. {ENROLL_SCOPE}."
        )?;
    }
    let mut ok = true;
    for alias in &enrolled {
        ok &= migrate_review(env, host, out, alias, false, opts.publish.as_ref())?;
    }
    default_board(env, opts, host, out, &enrolled)?;
    if !ok {
        bail!("a board review did not complete (see above)");
    }
    Ok(())
}

/// The host's default board for names and channels (decision 9): from `--default-board`,
/// else asked once at a terminal, else left as it is.
fn default_board(env: &Env, opts: &Options, host: &mut dyn Host, out: &mut dyn Write, enrolled: &[String]) -> anyhow::Result<()> {
    let path = env.default_board();
    let current = boards::read_default(&path)?;
    if let Some(want) = &opts.default_board {
        boards::write_default(&path, &env.boards_dir(), want)?;
        writeln!(out, "boards: default board for names and channels: {want}")?;
        return Ok(());
    }
    if enrolled.is_empty() {
        if current != LOCAL_BOARD {
            writeln!(out, "boards: the default board {current} is not enrolled; run: conde setup --default-board local")?;
        }
        return Ok(());
    }
    let choices = std::iter::once(LOCAL_BOARD.to_string()).chain(enrolled.iter().cloned()).collect::<Vec<_>>().join(", ");
    for _ in 0..3 {
        let prompt = format!(
            "boards: names and channels without --board go to the default board; message ids always carry their own. \
             Default board ({choices}) [{current}]: "
        );
        let Some(answer) = host.ask(&prompt) else {
            writeln!(
                out,
                "boards: default board for names and channels: {current} (to change it: conde setup --default-board <alias>)"
            )?;
            return Ok(());
        };
        let want = if answer.is_empty() { current.clone() } else { answer.to_lowercase() };
        match boards::write_default(&path, &env.boards_dir(), &want) {
            Ok(()) => {
                writeln!(out, "boards: default board for names and channels: {want}")?;
                return Ok(());
            }
            Err(e) => writeln!(out, "boards: {e:#}")?,
        }
    }
    writeln!(out, "boards: default board left at {current}")?;
    Ok(())
}

fn short_date(at: &str) -> &str {
    at.get(..10).unwrap_or(at)
}

/// The one-time switch of an enrolled host to v2 on board `alias` (§9): the daemon fetches
/// the read-only preview, `publish` (the `--publish*` flag) or the human at a terminal picks
/// the names that stay published, and the daemon applies it as a resumable operation.
/// Without a flag and without a terminal (an agent running setup) nothing changes: the board
/// stays not migrated and the output names the flags. Ok(false) when the review could not run
/// (`explicit`: asked for with `conde boards --migrate`).
pub fn migrate_review(
    env: &Env,
    host: &mut dyn Host,
    out: &mut dyn Write,
    alias: &str,
    explicit: bool,
    publish: Option<&Publish>,
) -> anyhow::Result<bool> {
    let later = format!("until then {alias} stays local-only on this host; the review: conde boards --migrate {alias}");
    let (info, finished) = match host.daemon(&env.socket, op::BOARD_INFO, Some(alias), json!({})) {
        Ok(v) => (serde_json::from_value::<BoardInfo>(v.clone()).context("board.info answer")?, v.get("migration").cloned()),
        Err(why) => {
            writeln!(out, "boards: {alias}: not reached through the daemon ({why}); {later}")?;
            return Ok(false);
        }
    };
    if info.mailbox_only {
        // a switch the daemon finished just now (its local apply had been pending)
        if let Some(r) = finished {
            print_switch(out, alias, &r)?;
        }
        writeln!(out, "boards: {alias}: host {} (account {}) is on v2; publish names with: conde publish <name> --board {alias}", info.host, info.owner)?;
        return Ok(true);
    }
    let preview = match host.daemon(&env.socket, op::HOST_MIGRATE_PREVIEW, Some(alias), json!({})) {
        Ok(v) => serde_json::from_value::<MigratePreview>(v).context("host.migrate_preview answer")?,
        Err(why) => {
            writeln!(out, "boards: {alias}: the preview failed ({why}); {later}")?;
            return Ok(false);
        }
    };
    let unknown = |out: &mut dyn Write, names: &[String]| {
        writeln!(out, "boards: {alias}: not migrated: not names of host {} on {alias}: {}; {later}", info.host, names.join(", "))
    };
    if preview.rows.is_empty() {
        if let Some(Publish::Names(names)) = publish
            && !names.is_empty()
        {
            unknown(out, names)?;
            return Ok(false);
        }
        writeln!(out, "boards: {alias}: no names of host {} are bound there; switching it to v2 publishes nothing", info.host)?;
        return apply_migration(env, host, out, alias, &[]);
    }
    let interactive = publish.is_none() && host.terminal();
    show_preview(out, alias, &info, &preview, interactive)?;
    if let Some(choice) = publish {
        let head = format!("{} publishes", choice.flag());
        let chosen = match choice.rows(&preview.rows) {
            Ok(c) => c,
            Err(names) => {
                unknown(out, &names)?;
                return Ok(false);
            }
        };
        let names: Vec<&str> = chosen.iter().map(|r| r.name.as_str()).collect();
        let listed = if names.is_empty() { "nothing".to_string() } else { names.join(", ") };
        let rest = preview.rows.len() - chosen.len();
        writeln!(out, "boards: {alias}: {head} {listed}; the other {rest} names move to this host's local board only")?;
        let ids: Vec<String> = chosen.iter().map(|r| r.agent_id.clone()).collect();
        let ok = apply_migration(env, host, out, alias, &ids)?;
        if ok {
            writeln!(out, "boards: {alias}: to change it later: conde publish <name> --board {alias}, conde unpublish <name> --board {alias}")?;
        }
        return Ok(ok);
    }
    if !interactive {
        writeln!(
            out,
            "boards: {alias}: not a terminal and no --publish flag; nothing changes until you run conde setup (or conde \
             boards --migrate {alias}) with --publish <name,...>, --publish-candidates, --publish-all or --publish-none"
        )?;
        return Ok(!explicit);
    }
    let Some(chosen) = choose(host, out, alias, &preview.rows)? else {
        writeln!(out, "boards: {alias}: not migrated: no answer; {later}")?;
        return Ok(!explicit);
    };
    let names: Vec<&str> = chosen.iter().map(|r| r.name.as_str()).collect();
    let rest = preview.rows.len() - chosen.len();
    let listed = if names.is_empty() { "nothing".to_string() } else { names.join(", ") };
    let confirm = format!(
        "boards: {alias}: publish {listed}; the other {rest} names move to this host's local board only. Apply? [y/N] "
    );
    match host.ask(&confirm).map(|a| a.to_lowercase()) {
        Some(a) if a == "y" || a == "yes" => {}
        Some(_) => {
            writeln!(out, "boards: {alias}: not migrated (declined); {later}")?;
            return Ok(true);
        }
        None => {
            writeln!(out, "boards: {alias}: not migrated: no answer; {later}")?;
            return Ok(!explicit);
        }
    }
    let ids: Vec<String> = chosen.iter().map(|r| r.agent_id.clone()).collect();
    apply_migration(env, host, out, alias, &ids)
}

fn show_preview(out: &mut dyn Write, alias: &str, info: &BoardInfo, preview: &MigratePreview, interactive: bool) -> anyhow::Result<()> {
    let choose = if interactive { "; history alone is not consent, so you choose" } else { "" };
    writeln!(
        out,
        "boards: {alias}: host {} (account {}) still has {} names bound from v1. A candidate (*) has messages with \
         another owner's agents{choose}:",
        info.host,
        info.owner,
        preview.rows.len()
    )?;
    let mut others = Vec::new();
    for row in &preview.rows {
        if !row.candidate {
            others.push(row.name.as_str());
            continue;
        }
        let pin = if row.pin.is_some() { "  pinned" } else { "" };
        writeln!(out, "  * {}  {}{pin}", row.name, row.address)?;
        for e in &row.evidence {
            let via = match e.via {
                crate::protocol::v2::Via::Dm => "dm with",
                crate::protocol::v2::Via::Channel => "posts in",
            };
            let source = match e.source {
                crate::protocol::v2::EvidenceSource::Imported => "imported from kond",
                crate::protocol::v2::EvidenceSource::Later => "since the import",
            };
            writeln!(out, "      {via} {}: {} messages, last {} ({source})", e.with, e.messages, short_date(&e.last_at))?;
        }
    }
    if !others.is_empty() {
        writeln!(out, "  no messages with another owner's agents ({}): {}", others.len(), others.join(", "))?;
    }
    Ok(())
}

/// The rows the human keeps published; None without a terminal.
fn choose<'a>(host: &mut dyn Host, out: &mut dyn Write, alias: &str, rows: &'a [PreviewRow]) -> anyhow::Result<Option<Vec<&'a PreviewRow>>> {
    let prompt = format!(
        "boards: {alias}: names that stay published (separated by spaces; 'candidates' for the * ones; 'none') [candidates]: "
    );
    for _ in 0..3 {
        let Some(answer) = host.ask(&prompt) else { return Ok(None) };
        let mut chosen: Vec<&PreviewRow> = Vec::new();
        let mut unknown = Vec::new();
        let words: Vec<String> = answer.split([' ', ',']).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
        let words = if words.is_empty() { vec!["candidates".to_string()] } else { words };
        for w in &words {
            let picked: Vec<&PreviewRow> = match w.as_str() {
                "none" => Vec::new(),
                "candidates" => rows.iter().filter(|r| r.candidate).collect(),
                name => match rows.iter().find(|r| r.name.eq_ignore_ascii_case(name)) {
                    Some(r) => vec![r],
                    None => {
                        unknown.push(w.clone());
                        Vec::new()
                    }
                },
            };
            for r in picked {
                if !chosen.iter().any(|c| c.agent_id == r.agent_id) {
                    chosen.push(r);
                }
            }
        }
        if unknown.is_empty() {
            return Ok(Some(chosen));
        }
        writeln!(out, "boards: {alias}: not names of this host: {}", unknown.join(", "))?;
    }
    Ok(None)
}

fn apply_migration(env: &Env, host: &mut dyn Host, out: &mut dyn Write, alias: &str, ids: &[String]) -> anyhow::Result<bool> {
    let r = match host.daemon(&env.socket, op::HOST_MIGRATE_V2, Some(alias), json!({"publish": ids})) {
        Ok(r) => r,
        Err(why) => {
            writeln!(
                out,
                "boards: {alias}: the switch did not complete ({why}); the recorded operation resumes with \
                 conde boards --migrate {alias} or at the next daemon start, and conde doctor shows it"
            )?;
            return Ok(false);
        }
    };
    print_switch(out, alias, &r)?;
    Ok(true)
}

/// A switch's result: what is published, the kept conflicts, the moved sessions.
fn print_switch(out: &mut dyn Write, alias: &str, r: &Value) -> anyhow::Result<()> {
    let manifest: Manifest = serde_json::from_value(r.clone()).context("host.migrate_v2 answer")?;
    let published: Vec<&str> = manifest.bindings.iter().filter(|b| b.published.is_some()).map(|b| b.name.as_str()).collect();
    let local = manifest.bindings.len() - published.len();
    let listed = if published.is_empty() { "nothing".to_string() } else { published.join(", ") };
    writeln!(out, "boards: {alias}: switched to v2; published: {listed}; on this host's local board only: {local} names")?;
    for c in r["conflicts"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "boards: {alias}: {} kept its local binding (a live session holds it); its former address {} is in conde doctor",
            c["name"].as_str().unwrap_or_default(),
            c["address"].as_str().unwrap_or_default()
        )?;
    }
    for m in r["moved"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "boards: {alias}: session {} moved from the local name {} to {}; {} keeps its messages, unbound",
            m["address"].as_str().unwrap_or_default(),
            m["name"].as_str().unwrap_or_default(),
            m["to"].as_str().unwrap_or_default(),
            m["name"].as_str().unwrap_or_default()
        )?;
    }
    Ok(())
}

fn read_text(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    let text = read_text(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))
}

/// The MCP server name conde registers under when nothing else holds it.
pub const MCP_NAME: &str = "conde";

/// Who holds an MCP server name in a harness's config.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Holder {
    /// No entry, or one whose binary is gone (a stale registration, reused).
    Free,
    /// This instance: our binary, on our socket.
    Ours,
    /// Another conde instance (another binary or socket) or another server; what it runs.
    Other(String),
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// An entry's holder from its command and its `CONDE_SOCK` (absent: the default socket);
/// `command` None for an entry that runs no local command (a remote server). A bare command
/// name is resolved through PATH first.
fn holder(env: &Env, present: bool, command: Option<&str>, sock: Option<&str>) -> Holder {
    if !present {
        return Holder::Free;
    }
    let Some(cmd) = command.filter(|c| !c.is_empty()) else {
        return Holder::Other("a server that runs no local command".into());
    };
    let other = || Holder::Other(match sock {
        Some(s) => format!("{cmd}, CONDE_SOCK={s}"),
        None => cmd.to_string(),
    });
    let (sep, path_like) = match env.os {
        Os::Windows => (';', cmd.contains(['/', '\\'])),
        _ => (':', cmd.contains('/')),
    };
    let resolved = if path_like {
        Some(PathBuf::from(cmd))
    } else {
        env.path.split(sep).filter(|d| !d.is_empty()).map(|d| Path::new(d).join(cmd)).find(|p| p.is_file())
    };
    let socket = sock.map_or_else(|| env.default_socket.clone(), PathBuf::from);
    match resolved {
        Some(path) if same_file(&path, &env.exe) => {
            if socket == env.socket { Holder::Ours } else { other() }
        }
        Some(path) if path.exists() => other(),
        _ => Holder::Free,
    }
}

/// The alias a second registration is named after: the default shared board, else the first
/// enrolled one.
fn board_alias(env: &Env) -> Option<String> {
    let enrolled = boards::enrolled_in(&env.boards_dir()).ok()?;
    match boards::read_default(&env.default_board()) {
        Ok(d) if d != LOCAL_BOARD && enrolled.contains(&d) => Some(d),
        _ => enrolled.into_iter().next(),
    }
}

/// The name this instance's MCP server is registered under in one harness: a name it already
/// holds (`conde` first, then any `conde-*` among `existing`), else the first free of `conde`,
/// `conde-<board alias>`, `conde-2`..; and, when `conde` belongs to another instance, what
/// that one runs.
fn mcp_name(env: &Env, existing: &[String], held: impl Fn(&str) -> Holder) -> anyhow::Result<(String, Option<String>)> {
    let mut fresh = vec![MCP_NAME.to_string()];
    fresh.extend(board_alias(env).map(|a| format!("{MCP_NAME}-{a}")));
    fresh.extend((2..=9).map(|n| format!("{MCP_NAME}-{n}")));
    let mut held_before: Vec<&String> = existing.iter().filter(|n| n.starts_with(&format!("{MCP_NAME}-"))).collect();
    held_before.sort();
    let first = held(MCP_NAME);
    let pick = std::iter::once(&fresh[0])
        .chain(held_before)
        .find(|n| held(n) == Holder::Ours)
        .or_else(|| fresh.iter().find(|n| held(n) == Holder::Free))
        .ok_or_else(|| anyhow!("every MCP server name from {} to {} belongs to another server", fresh[0], fresh[fresh.len() - 1]))?
        .clone();
    let displaced = match first {
        Holder::Other(what) if pick != MCP_NAME => Some(what),
        _ => None,
    };
    Ok((pick, displaced))
}

/// The skill as written for an MCP server registered as `mcp`: under a second name it is
/// a skill of that name (`skills/<mcp>/`), beside the other instance's `conde` skill.
pub fn skill_text(skill: &str, mcp: &str) -> String {
    skill
        .replacen(&format!("---\nname: {MCP_NAME}\n"), &format!("---\nname: {mcp}\n"), 1)
        .replacen(&format!("tools of the `{MCP_NAME}` MCP server"), &format!("tools of the `{mcp}` MCP server"), 1)
}

fn claude_holder(env: &Env, entry: &Value) -> Holder {
    let command = if entry["url"].is_null() { entry["command"].as_str() } else { None };
    holder(env, !entry.is_null(), command, entry["env"]["CONDE_SOCK"].as_str())
}

fn claude_name(env: &Env) -> anyhow::Result<(String, Option<String>)> {
    let servers = read_json(&env.claude_json())?["mcpServers"].clone();
    let existing: Vec<String> = servers.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    mcp_name(env, &existing, |n| claude_holder(env, &servers[n]))
}

fn claude(env: &Env, opts: &Options, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let (name, displaced) = claude_name(env)?;
    if let Some(what) = displaced {
        writeln!(out, "claude: MCP server {MCP_NAME} belongs to another conde instance ({what}); this one registers as {name}")?;
    }
    let skill = env.claude_skill_for(&name);
    let text = skill_text(CLAUDE_SKILL, &name);
    writeln!(out, "claude: skill {} {}", write_if_changed(&skill, &text)?.verb(), env.show(&skill))?;
    claude_mcp(env, &name, host, out)?;
    claude_inbound(env, opts, out)
}

fn mcp_entry(env: &Env) -> Value {
    let sock = env.socket.to_string_lossy();
    json!({"type": "stdio", "command": env.exe.to_string_lossy(), "args": ["mcp"], "env": {"CONDE_SOCK": sock}})
}

fn claude_mcp(env: &Env, name: &str, host: &mut dyn Host, out: &mut dyn Write) -> anyhow::Result<()> {
    let path = env.claude_json();
    let mut want = mcp_entry(env);
    let current = read_json(&path)?["mcpServers"][name].clone();
    let sock = want["env"]["CONDE_SOCK"].clone();
    if current["command"] == want["command"] && current["args"] == want["args"] && current["env"]["CONDE_SOCK"] == sock {
        writeln!(out, "claude: MCP server {name} unchanged (user scope)")?;
        return Ok(());
    }
    // the entry's other env vars stay; CONDE_SOCK is ours
    if let Some(kept) = current["env"].as_object() {
        let env_obj = want["env"].as_object_mut().expect("mcp_entry has an env object");
        for (k, v) in kept {
            if k != "CONDE_SOCK" && v.is_string() {
                env_obj.insert(k.clone(), v.clone());
            }
        }
    }
    let exe = env.exe.to_string_lossy().into_owned();
    if let Some(claude) = host.which("claude") {
        let claude = claude.to_string_lossy().into_owned();
        if !current.is_null() {
            check(host, &claude, &["mcp", "remove", "--scope", "user", name])?;
        }
        let pairs: Vec<String> = want["env"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
            .collect();
        // the name before -e: -e takes several values
        let mut args = vec!["mcp", "add", "--scope", "user", name];
        for pair in &pairs {
            args.extend(["-e", pair.as_str()]);
        }
        args.extend(["--", &exe, "mcp"]);
        check(host, &claude, &args)?;
        writeln!(out, "claude: registered MCP server {name} at user scope (claude {})", args.join(" "))?;
        let back = read_json(&path)?["mcpServers"][name].clone();
        if back["command"] != want["command"] || back["env"]["CONDE_SOCK"] != sock {
            writeln!(out, "claude: note: the entry did not appear in {}; check with: claude mcp get {name}", env.show(&path))?;
        }
    } else {
        let text = read_text(&path)?;
        write_if_changed(&path, &json_set(&text, &["mcpServers", name], &want)?)?;
        writeln!(
            out,
            "claude: claude CLI not found; wrote the user-scope MCP entry {name} into {} directly",
            env.show(&path)
        )?;
    }
    Ok(())
}

/// Owner's decision: conde notifications never wait for per-message approval,
/// so an unset receiver policy becomes "accept"; an explicit value is the
/// human's and stays.
fn claude_inbound(env: &Env, opts: &Options, out: &mut dyn Write) -> anyhow::Result<()> {
    let path = env.claude_settings();
    let shown = env.show(&path);
    let current = read_json(&path)?["crossSessionInbound"].clone();
    if current == "accept" {
        writeln!(out, "claude: crossSessionInbound already \"accept\" in {shown}")?;
    } else if opts.keep_receiver_policy {
        let now = if current.is_null() { "unset".to_string() } else { current.to_string() };
        writeln!(out, "claude: crossSessionInbound left {now} in {shown} (--keep-receiver-policy)")?;
    } else if current.is_null() {
        let text = read_text(&path)?;
        write_if_changed(&path, &json_set(&text, &["crossSessionInbound"], &json!("accept"))?)?;
        writeln!(
            out,
            "claude: set crossSessionInbound to \"accept\" in {shown} (bypass-mode sessions receive conde \
notifications without approval; --keep-receiver-policy leaves it)"
        )?;
    } else {
        writeln!(
            out,
            "claude: note: crossSessionInbound is {current} in {shown}, an explicit setting, left as is; with it, \
bypass-mode sessions do not receive conde notifications without approval"
        )?;
    }
    Ok(())
}

/// The name this instance's MCP server has (or gets) in Codex's config, as [`mcp_name`].
pub fn codex_name(env: &Env, text: &str) -> anyhow::Result<(String, Option<String>)> {
    let doc: toml_edit::DocumentMut = text.parse().context("not valid TOML")?;
    let servers = doc.get("mcp_servers");
    let existing: Vec<String> =
        servers.and_then(|s| s.as_table_like()).map(|t| t.iter().map(|(k, _)| k.to_string()).collect()).unwrap_or_default();
    mcp_name(env, &existing, |n| {
        let entry = servers.and_then(|s| s.get(n));
        let field = |k: &str| entry.and_then(|e| e.get(k));
        let command = if field("url").is_none() { field("command").and_then(|c| c.as_str()) } else { None };
        let sock = field("env").and_then(|e| e.get("CONDE_SOCK")).and_then(|v| v.as_str());
        holder(env, entry.is_some(), command, sock)
    })
}

/// What setup and `--check` both say on a machine without Claude Code.
const CLAUDE_ABSENT: &str = "claude: Claude Code not found on this machine; rerun conde setup after installing it";

/// Claude Code is installed when `claude` is on PATH or its config directory or
/// `.claude.json` exists (installed somewhere PATH does not reach); setup and `--check` skip
/// it otherwise.
fn claude_installed(env: &Env, host: &dyn Host) -> bool {
    host.which("claude").is_some() || env.claude_dir().exists() || env.claude_json().exists()
}

/// What setup and `--check` both say on a machine without Codex.
const CODEX_ABSENT: &str = "codex: Codex not found on this machine; rerun conde setup after installing it";

/// Codex is installed when `codex` is on PATH or its config directory exists (installed
/// somewhere PATH does not reach); setup and `--check` skip it otherwise.
fn codex_installed(env: &Env, host: &dyn Host) -> bool {
    host.which("codex").is_some() || env.codex_dir().exists()
}

fn codex(env: &Env, out: &mut dyn Write) -> anyhow::Result<()> {
    let path = env.codex_config();
    let text = read_text(&path)?;
    // unreadable TOML: the skill still lands, the edit below reports the file
    let (name, displaced) = codex_name(env, &text).unwrap_or_else(|_| (MCP_NAME.to_string(), None));
    if let Some(what) = displaced {
        writeln!(out, "codex: MCP server {MCP_NAME} belongs to another conde instance ({what}); this one registers as {name}")?;
    }
    let skill = env.codex_skill_for(&name);
    let skill_body = skill_text(CODEX_SKILL, &name);
    writeln!(out, "codex: skill {} {}", write_if_changed(&skill, &skill_body)?.verb(), env.show(&skill))?;
    let edited = codex_config_edit(&text, &name, &env.exe.to_string_lossy(), &env.socket.to_string_lossy())
        .with_context(|| format!("{} left as is", env.show(&path)))?;
    let line = match edited {
        Some(new) => match write_if_changed(&path, &new)? {
            Change::Created => format!("added MCP server {name} to"),
            _ => format!("updated MCP server {name} in"),
        },
        None => format!("MCP server {name} unchanged in"),
    };
    writeln!(out, "codex: {line} {}", env.show(&path))?;
    Ok(())
}

/// Point `mcp_servers.<name>` at `exe mcp` on socket `sock` (its `env.CONDE_SOCK`) with a
/// TOML-aware edit that keeps comments, formatting and every other entry (other keys of an
/// existing entry and of its env too). None when it already does.
pub fn codex_config_edit(text: &str, name: &str, exe: &str, sock: &str) -> anyhow::Result<Option<String>> {
    use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value as TomlValue, value};
    let mut doc: DocumentMut = text.parse().context("not valid TOML")?;
    let wanted_args = || Array::from_iter(["mcp"]);
    let wanted_env = || {
        let mut t = InlineTable::new();
        t.insert("CONDE_SOCK", sock.into());
        t
    };
    let current = doc.get("mcp_servers").and_then(|s| s.get(name));
    let field = |k: &str| current.and_then(|c| c.get(k));
    let args_ok = field("args").and_then(Item::as_array).is_some_and(|a| {
        a.iter().map(|v| v.as_str()).collect::<Vec<_>>() == [Some("mcp")]
    });
    let sock_ok = field("env").and_then(|e| e.get("CONDE_SOCK")).and_then(Item::as_str) == Some(sock);
    if field("command").and_then(Item::as_str) == Some(exe) && args_ok && sock_ok {
        return Ok(None);
    }
    let servers = doc.get("mcp_servers");
    if servers.is_some_and(|s| s.as_table_like().is_none()) {
        bail!("mcp_servers is not a table");
    }
    let has_entry = servers.and_then(|s| s.get(name)).is_some();
    let inline_parent =
        servers.is_some_and(|s| s.is_inline_table() || s.as_table_like().is_some_and(|t| t.is_dotted()));
    if has_entry {
        let entry = doc["mcp_servers"][name]
            .as_table_like_mut()
            .ok_or_else(|| anyhow!("mcp_servers.{name} is not a table"))?;
        entry.insert("command", value(exe));
        entry.insert("args", value(wanted_args()));
        match entry.get_mut("env") {
            Some(Item::Value(TomlValue::InlineTable(env))) => {
                env.insert("CONDE_SOCK", sock.into());
                env.fmt();
            }
            Some(Item::Table(env)) => {
                env.insert("CONDE_SOCK", value(sock));
            }
            _ => {
                entry.insert("env", Item::Value(TomlValue::InlineTable(wanted_env())));
            }
        }
    } else if inline_parent {
        let mut t = InlineTable::new();
        t.insert("command", exe.into());
        t.insert("args", TomlValue::Array(wanted_args()));
        t.insert("env", TomlValue::InlineTable(wanted_env()));
        doc["mcp_servers"][name] = Item::Value(TomlValue::InlineTable(t));
    } else {
        let servers = doc.entry("mcp_servers").or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        let mut t = Table::new();
        t.insert("command", value(exe));
        t.insert("args", value(wanted_args()));
        t.insert("env", Item::Value(TomlValue::InlineTable(wanted_env())));
        servers
            .as_table_like_mut()
            .ok_or_else(|| anyhow!("mcp_servers is not a table"))?
            .insert(name, Item::Table(t));
    }
    let new = doc.to_string();
    let back: DocumentMut = new.parse().context("edited config.toml does not parse")?;
    let entry = back.get("mcp_servers").and_then(|s| s.get(name));
    if entry.and_then(|c| c.get("command")).and_then(Item::as_str) != Some(exe)
        || entry.and_then(|c| c.get("env")).and_then(|e| e.get("CONDE_SOCK")).and_then(Item::as_str) != Some(sock)
    {
        bail!("edited config.toml does not hold the {name} entry");
    }
    Ok(Some(new))
}

/// Set `path` in a JSON object document by splicing text, so the rest of
/// the file (key order, formatting) stays as it was. The result is parsed
/// back and compared with the intended value before it is returned.
pub fn json_set(text: &str, path: &[&str], value: &Value) -> anyhow::Result<String> {
    let mut expected = if text.trim().is_empty() { json!({}) } else { serde_json::from_str(text)? };
    let mut slot = expected.as_object_mut().ok_or_else(|| anyhow!("the document is not a JSON object"))?;
    for (i, key) in path.iter().enumerate() {
        if i + 1 == path.len() {
            slot.insert((*key).to_string(), value.clone());
        } else {
            let next = slot.entry(*key).or_insert_with(|| json!({}));
            slot = next.as_object_mut().ok_or_else(|| anyhow!("'{key}' is not a JSON object"))?;
        }
    }
    if text.trim().is_empty() {
        return Ok(serde_json::to_string_pretty(&expected)? + "\n");
    }
    let spliced = splice(text, 0, path, value)?;
    let back: Value = serde_json::from_str(&spliced).context("edited JSON does not parse")?;
    if back != expected {
        bail!("edited JSON does not hold the intended value");
    }
    Ok(spliced)
}

/// Splice into the object that starts at or after byte `at`.
fn splice(text: &str, at: usize, path: &[&str], value: &Value) -> anyhow::Result<String> {
    let b = text.as_bytes();
    let open = skip_ws(b, at);
    if b.get(open) != Some(&b'{') {
        bail!("expected a JSON object");
    }
    let mut i = skip_ws(b, open + 1);
    let mut first_key: Option<usize> = None;
    while b.get(i) == Some(&b'"') {
        first_key.get_or_insert(i);
        let key_end = scan_value(b, i)?;
        let key: String = serde_json::from_str(&text[i..key_end])?;
        let colon = skip_ws(b, key_end);
        if b.get(colon) != Some(&b':') {
            bail!("expected ':' after a key");
        }
        let v_start = skip_ws(b, colon + 1);
        let v_end = scan_value(b, v_start)?;
        if key == path[0] {
            if path.len() == 1 {
                return Ok(format!("{}{}{}", &text[..v_start], serde_json::to_string(value)?, &text[v_end..]));
            }
            return splice(text, v_start, &path[1..], value);
        }
        i = skip_ws(b, v_end);
        if b.get(i) == Some(&b',') {
            i = skip_ws(b, i + 1);
        }
    }
    // not present: insert as the first member, nesting what is missing
    let mut v = value.clone();
    for key in path[1..].iter().rev() {
        v = Value::Object(Map::from_iter([((*key).to_string(), v)]));
    }
    let member = format!("{}: {}", serde_json::to_string(path[0])?, serde_json::to_string(&v)?);
    let indent = match first_key {
        Some(k) => {
            let line_start = text[..k].rfind('\n').map_or(0, |n| n + 1);
            text[line_start..k].chars().all(char::is_whitespace).then(|| text[line_start..k].to_string())
        }
        None => None,
    };
    let indent_of_open = {
        let line_start = text[..open].rfind('\n').map_or(0, |n| n + 1);
        text[line_start..open].chars().take_while(|c| c.is_whitespace()).collect::<String>()
    };
    Ok(match (first_key, indent) {
        (Some(_), Some(ind)) => format!("{}\n{ind}{member},{}", &text[..=open], &text[open + 1..]),
        (Some(_), None) => format!("{}{member}, {}", &text[..=open], &text[open + 1..]),
        (None, _) => {
            let close = skip_ws(b, open + 1);
            format!("{}\n{indent_of_open}  {member}\n{indent_of_open}{}", &text[..=open], &text[close..])
        }
    })
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// End (exclusive) of the JSON value starting at `i`.
fn scan_value(b: &[u8], i: usize) -> anyhow::Result<usize> {
    let mut depth = 0usize;
    let mut j = i;
    loop {
        let c = *b.get(j).ok_or_else(|| anyhow!("truncated JSON"))?;
        match c {
            b'"' => {
                j += 1;
                loop {
                    match *b.get(j).ok_or_else(|| anyhow!("unterminated string"))? {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                j += 1;
                if depth == 0 {
                    return Ok(j);
                }
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' if depth == 0 => return Ok(j),
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(j + 1);
                }
            }
            b',' | b' ' | b'\t' | b'\n' | b'\r' if depth == 0 => return Ok(j),
            _ => {}
        }
        j += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_set_keeps_order_and_formatting() {
        let text = "{\n  \"zeta\": 1,\n  \"alpha\": {\"x\": [1, \"}\"]},\n  \"crossSessionInbound\": \"hold\"\n}\n";
        let out = json_set(text, &["crossSessionInbound"], &json!("accept")).unwrap();
        assert_eq!(out, text.replace("\"hold\"", "\"accept\""));
        let out = json_set(text, &["mcpServers", "conde"], &json!({"command": "/c"})).unwrap();
        assert!(out.starts_with("{\n  \"mcpServers\": {\"conde\":{\"command\":\"/c\"}},\n  \"zeta\": 1,"), "{out}");
        let nested = "{\"mcpServers\": {\"other\": {\"command\": \"x\"}}, \"z\": true}";
        let out = json_set(nested, &["mcpServers", "conde"], &json!({"command": "/c"})).unwrap();
        assert_eq!(out, "{\"mcpServers\": {\"conde\": {\"command\":\"/c\"}, \"other\": {\"command\": \"x\"}}, \"z\": true}");
        assert_eq!(json_set("{}", &["a"], &json!(1)).unwrap(), "{\n  \"a\": 1\n}");
        assert_eq!(json_set("", &["a"], &json!(1)).unwrap(), "{\n  \"a\": 1\n}\n");
        assert!(json_set("[1]", &["a"], &json!(1)).is_err());
        assert!(json_set("{\"mcpServers\": 3}", &["mcpServers", "conde"], &json!(1)).is_err());
        let escaped = "{\"k\\\"ey\": \"v\\\\\", \"crossSessionInbound\": null}";
        let out = json_set(escaped, &["crossSessionInbound"], &json!("accept")).unwrap();
        assert_eq!(out, "{\"k\\\"ey\": \"v\\\\\", \"crossSessionInbound\": \"accept\"}");
    }

    fn conde_entry(toml: &str) -> (String, Vec<String>, usize) {
        let doc: toml_edit::DocumentMut = toml.parse().expect("edited TOML parses");
        let e = &doc["mcp_servers"]["conde"];
        let args = e["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        (e["command"].as_str().unwrap().to_string(), args, toml.matches("[mcp_servers.conde]").count())
    }

    #[test]
    fn codex_config_is_edited_in_place() {
        let user = "# my config\nmodel = \"x\" # trailing note\n\n[mcp_servers.docs]\nurl = \"https://example\"\n";
        let first = codex_config_edit(user, "conde", "/bin/conde", "/s").unwrap().unwrap();
        assert!(first.starts_with(user), "{first}");
        assert!(
            first.ends_with("[mcp_servers.conde]\ncommand = \"/bin/conde\"\nargs = [\"mcp\"]\nenv = { CONDE_SOCK = \"/s\" }\n"),
            "{first}"
        );
        assert_eq!(codex_config_edit(&first, "conde", "/bin/conde", "/s").unwrap(), None, "second run is a no-op");
        let moved = codex_config_edit(&first, "conde", "/opt/my conde", "/s").unwrap().unwrap();
        assert_eq!(moved, first.replace("\"/bin/conde\"", "\"/opt/my conde\""));
        let resocketed = codex_config_edit(&first, "conde", "/bin/conde", "/t").unwrap().unwrap();
        assert_eq!(resocketed, first.replace("\"/s\"", "\"/t\""));
        assert_eq!(
            codex_config_edit("", "conde-dakai", "/c", "/s").unwrap().unwrap(),
            "[mcp_servers.conde-dakai]\ncommand = \"/c\"\nargs = [\"mcp\"]\nenv = { CONDE_SOCK = \"/s\" }\n"
        );

        // an existing entry keeps its other keys, its env vars and its place
        let own = "[mcp_servers.conde]\ncommand = \"/old\"\nargs = [\"x\"]\nenv = { A = \"1\" }\n\n[other]\nk = 1\n";
        let out = codex_config_edit(own, "conde", "/c", "/s").unwrap().unwrap();
        assert_eq!(out, "[mcp_servers.conde]\ncommand = \"/c\"\nargs = [\"mcp\"]\nenv = { A = \"1\", CONDE_SOCK = \"/s\" }\n\n[other]\nk = 1\n");

        // review repro: a comment with triple quotes must not hide the table (no duplicate)
        let tricky = "# example uses '''\n[mcp_servers.conde]\ncommand = \"/old\"\n";
        let out = codex_config_edit(tricky, "conde", "/new", "/s").unwrap().unwrap();
        assert_eq!(out, "# example uses '''\n[mcp_servers.conde]\ncommand = \"/new\"\nargs = [\"mcp\"]\nenv = { CONDE_SOCK = \"/s\" }\n");
        assert_eq!(conde_entry(&out), ("/new".into(), vec!["mcp".into()], 1));

        // dotted keys and inline tables are extended, not shadowed
        let dotted = "[mcp_servers]\nconde.command = \"c\"\n";
        assert_eq!(conde_entry(&codex_config_edit(dotted, "conde", "/c", "/s").unwrap().unwrap()).0, "/c");
        let inline = "mcp_servers = { x = { command = \"x\" } }\n";
        let out = codex_config_edit(inline, "conde", "/c", "/s").unwrap().unwrap();
        assert_eq!(conde_entry(&out).0, "/c");
        assert!(out.contains("x = { command = \"x\" }"), "{out}");

        assert!(codex_config_edit("mcp_servers = 3\n", "conde", "/c", "/s").is_err());
        assert!(codex_config_edit("[broken\n", "conde", "/c", "/s").is_err(), "invalid TOML is never rewritten");
    }

    #[test]
    fn unit_words_are_escaped() {
        assert_eq!(systemd_word("/usr/bin/conde"), "/usr/bin/conde");
        assert_eq!(systemd_word("/My Apps/conde"), "\"/My Apps/conde\"");
        assert_eq!(systemd_word("/opt/$x/%i"), "/opt/$$x/%%i");
        // Environment= takes $ literally; only % (specifiers) and quoting are escaped
        assert_eq!(systemd_env("PATH", "/opt/$tools/bin:/usr/bin"), "\"PATH=/opt/$tools/bin:/usr/bin\"");
        assert_eq!(systemd_env("CONDE_SOCK", "/run/a b/%i\"q\\"), "\"CONDE_SOCK=/run/a b/%%i\\\"q\\\\\"");
    }
}
