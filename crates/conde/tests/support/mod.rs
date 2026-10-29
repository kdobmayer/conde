#![allow(dead_code)]
//! Isolated hosts for end-to-end tests: every path under a fresh temp root,
//! a cleared environment (no inherited harness markers, an empty PATH so a
//! codex delivery can never start the real codex), and a fake Claude Code
//! session that records frames and answers with scripted receipts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

// the fake Claude Code session's (unix only)
#[cfg(unix)]
use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use serde_json::{Value, json};

#[path = "../os/mod.rs"]
pub mod os;

pub const BIN: &str = env!("CARGO_BIN_EXE_conde");
pub const WAIT: Duration = Duration::from_secs(15);

pub fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// One machine: its own home, config, sockets, Claude sessions dir.
pub struct Host {
    pub root: PathBuf,
    pub user: String,
    pub host: String,
    daemons: Vec<Child>,
}

impl Host {
    pub fn new(user: &str, host: &str) -> Host {
        // Short paths: unix socket paths are limited to 107 bytes.
        let ulid = conde::util::new_ulid();
        let root = std::env::temp_dir().join(format!("cde-{}", &ulid[14..]));
        for d in [
            "bin",
            "nopath",
            "claude/sessions",
            "cc",
            "codex",
            "config",
            "run",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for harness in ["codex", "claude"] {
            os::harness(&root.join("bin"), harness);
        }
        Host {
            root,
            user: user.into(),
            host: host.into(),
            daemons: Vec::new(),
        }
    }

    pub fn sock(&self) -> PathBuf {
        os::socket_in(&self.root)
    }

    /// Another daemon's address on this machine.
    pub fn sock_named(&self, name: &str) -> PathBuf {
        os::named_socket_in(&self.root, name)
    }

    pub fn env<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        os::clear_env(cmd).env("PATH", self.root.join("nopath"));
        os::user_home(cmd, &self.user, &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env("CONDE_DATA", self.root.join("data"))
            .env("CONDE_SOCK", self.sock())
            .env("CONDE_HOST", &self.host)
    }

    /// Start `conde daemon <args>` with its log in `<root>/<log>`; waits for the socket.
    pub fn daemon(&mut self, log: &str, sock: &Path, args: &[&str]) -> u32 {
        let file = std::fs::File::create(self.root.join(log)).unwrap();
        let child = self
            .env(&mut Command::new(BIN))
            .env("CONDE_SOCK", sock)
            .arg("daemon")
            .args(args)
            .stdout(Stdio::null())
            .stderr(file)
            .spawn()
            .unwrap();
        let pid = child.id();
        self.daemons.push(child);
        let deadline = Instant::now() + WAIT;
        while !os::answers(sock) {
            assert!(
                Instant::now() < deadline,
                "daemon did not come up: {}",
                self.log(log)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        pid
    }

    pub fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.root.join(name)).unwrap_or_default()
    }

    pub fn wait_log(&self, name: &str, needle: &str) {
        wait_for(&format!("{needle:?} in {name}"), || {
            self.log(name).contains(needle).then_some(())
        });
    }

    /// The exit status of a daemon this host started, once it exits.
    pub fn wait_exit(&mut self, pid: u32) -> ExitStatus {
        let child = self
            .daemons
            .iter_mut()
            .find(|c| c.id() == pid)
            .expect("our daemon");
        wait_for("daemon exit", || child.try_wait().unwrap())
    }

    #[cfg(unix)]
    pub fn terminate(&mut self, pid: u32) -> ExitStatus {
        assert!(
            self.daemons.iter().any(|c| c.id() == pid),
            "only our own children are signalled"
        );
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        self.wait_exit(pid)
    }

    /// Run the CLI, optionally beneath a fake harness process.
    pub fn conde(
        &self,
        harness: Option<&str>,
        env: &[(&str, &str)],
        args: &[&str],
        stdin: Option<&str>,
    ) -> Output {
        let mut cmd = match harness {
            Some(h) => os::under(&os::harness_file(&self.root.join("bin"), h), BIN),
            None => Command::new(BIN),
        };
        self.env(&mut cmd).envs(env.iter().copied()).args(args);
        match stdin {
            None => cmd.stdin(Stdio::null()).output().unwrap(),
            Some(input) => {
                let mut child = cmd
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(input.as_bytes())
                    .unwrap();
                child.wait_with_output().unwrap()
            }
        }
    }

    pub fn ok(&self, harness: Option<&str>, env: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.conde(harness, env, args, None);
        assert!(
            out.status.success(),
            "conde {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Like `ok`, against the daemon listening on `sock`.
    pub fn ok_at(
        &self,
        sock: &Path,
        harness: Option<&str>,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> String {
        let sock = sock.to_str().unwrap();
        let mut env = env.to_vec();
        env.push(("CONDE_SOCK", sock));
        self.ok(harness, &env, args)
    }

    /// `conde sent` as `actor`, the line for message `id`.
    pub fn sent_line(&self, harness: &str, env: &[(&str, &str)], id: &str) -> String {
        // `<board>  <id>  <to>  <status>...`
        self.ok(Some(harness), env, &["sent"])
            .lines()
            .find(|l| l.split("  ").nth(1) == Some(id))
            .unwrap_or_default()
            .to_string()
    }

    pub fn wait_delivery(
        &self,
        harness: &str,
        env: &[(&str, &str)],
        id: &str,
        state: &str,
    ) -> String {
        wait_for(&format!("delivery {state} of {id}"), || {
            let line = self.sent_line(harness, env, id);
            line.ends_with(&format!("delivery: {state}"))
                .then_some(line)
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for c in &mut self.daemons {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One received frame.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub struct Frame {
    pub msg_id: String,
    pub from: String,
    pub text: String,
    pub raw: String,
}

#[cfg(unix)]
impl Frame {
    /// The conde message id the notification points at.
    pub fn message_id(&self) -> Option<String> {
        // v1 `conde: new message <id>`, v2 `conde [<board>]: new message <board>:<id>`
        let rest = self.text.split("new message ").nth(1)?;
        Some(rest.split_whitespace().next()?.to_string())
    }
}

/// A receipt to send back: after `delay`, `status` plus extra fields.
#[cfg(unix)]
pub type Step = (Duration, &'static str, Value);

/// One receipt list per frame of a message.
#[cfg(unix)]
type Plan = Vec<Vec<Step>>;
/// Plans waiting for a new message, and the plan each seen message took.
#[cfg(unix)]
type Plans = (VecDeque<Plan>, HashMap<String, Plan>);

/// A Claude Code session's receiving side on a socket in the host's temp dir,
/// published in the host's sessions dir. Each new conde message takes the next
/// queued plan; the plan's n-th entry answers that message's n-th frame.
#[cfg(unix)]
pub struct FakeClaude {
    pub frames: Arc<Mutex<Vec<Frame>>>,
    plans: Arc<Mutex<Plans>>,
}

#[cfg(unix)]
fn proc_start() -> String {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    stat[stat.rfind(')').unwrap() + 2..]
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_string()
}

#[cfg(unix)]
fn decode_uds(addr: &str) -> PathBuf {
    let raw = addr.strip_prefix("uds:").expect("uds address").as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < raw.len() {
        if raw[i] == b'%' {
            out.push(
                u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).unwrap(), 16).unwrap(),
            );
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&out))
}

#[cfg(unix)]
impl FakeClaude {
    pub fn start(host: &Host, session_id: &str) -> FakeClaude {
        let sock = host.root.join("cc/inbox.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let pid = std::process::id();
        let info = json!({"pid": pid, "sessionId": session_id, "name": session_id, "status": "idle",
            "procStart": proc_start(), "messagingSocketPath": sock, "cwd": host.root});
        std::fs::write(
            host.root.join(format!("claude/sessions/{pid}.json")),
            info.to_string(),
        )
        .unwrap();
        let fake = FakeClaude {
            frames: Arc::default(),
            plans: Arc::default(),
        };
        let (frames, plans) = (fake.frames.clone(), fake.plans.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let lines: Vec<String> =
                    BufReader::new(conn).lines().map_while(Result::ok).collect();
                let Some(last) = lines.last() else { continue };
                let f: Value = serde_json::from_str(last).unwrap();
                let frame = Frame {
                    msg_id: f["msg_id"].as_str().unwrap_or_default().into(),
                    from: f["from"].as_str().unwrap_or_default().into(),
                    text: f["message"]["content"].as_str().unwrap_or_default().into(),
                    raw: lines.join("\n"),
                };
                let steps = {
                    let mut frames = frames.lock().unwrap();
                    let key = frame.message_id().unwrap_or_default();
                    let nth = frames
                        .iter()
                        .filter(|x| x.message_id().unwrap_or_default() == key)
                        .count();
                    frames.push(frame.clone());
                    let mut p = plans.lock().unwrap();
                    if !p.1.contains_key(&key) {
                        let next = p.0.pop_front().unwrap_or_default();
                        p.1.insert(key.clone(), next);
                    }
                    p.1[&key].get(nth).cloned().unwrap_or_default()
                };
                std::thread::spawn(move || {
                    for (delay, status, extra) in steps {
                        std::thread::sleep(delay);
                        let mut receipt = json!({"type": "control", "action": "peer_message_status",
                            "status": status, "orig_msg_id": frame.msg_id});
                        for (k, v) in extra.as_object().unwrap() {
                            receipt[k] = v.clone();
                        }
                        if let Ok(mut c) = UnixStream::connect(decode_uds(&frame.from)) {
                            let _ = c.write_all(format!("{receipt}\n").as_bytes());
                        }
                    }
                });
            }
        });
        fake
    }

    /// The plan for the next new message: one receipt list per frame.
    pub fn plan(&self, per_frame: Plan) {
        self.plans.lock().unwrap().0.push_back(per_frame);
    }

    pub fn frames_for(&self, message_id: &str) -> Vec<Frame> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.message_id().as_deref() == Some(message_id))
            .cloned()
            .collect()
    }

    pub fn wait_frames(&self, message_id: &str, n: usize) -> Vec<Frame> {
        wait_for(&format!("{n} frame(s) for {message_id}"), || {
            let f = self.frames_for(message_id);
            (f.len() >= n).then_some(f)
        })
    }
}

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
