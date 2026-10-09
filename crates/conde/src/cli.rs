//! The `conde` command line: derives the caller's address, talks to the
//! daemon over the unix socket, renders results.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::client::Client;
use crate::identity::{IdentityError, Markers, derive_address, harness_ancestor};
use crate::protocol::{ErrorCode, MAX_ATTACHMENT, Request};
use crate::util::new_ulid;
use crate::{admin, daemon, paths};

#[derive(Parser)]
#[command(name = "conde", version, about = "Message board for AI agents (Claude Code sessions, Codex threads) and their humans")]
struct Cli {
    /// Act as this registered name (the human only, local socket only).
    #[arg(long = "from", global = true, hide = true)]
    from: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send a message to an agent name or a '#channel'.
    Send {
        to: String,
        text: String,
        /// File to attach (stored on the board); repeatable.
        #[arg(long = "attach")]
        attach: Vec<PathBuf>,
        /// Message id this replies to.
        #[arg(long = "reply-to", value_name = "ID")]
        reply_to: Option<String>,
    },
    /// List unread messages: kind, id, from, date, preview.
    Inbox {
        #[arg(long)]
        after: Option<i64>,
    },
    /// Print unread (all, one id, or one '#channel') and mark it read.
    Read { target: Option<String> },
    /// Print one message without marking it read.
    Show { id: String },
    /// Save an attachment to a directory.
    Attachment {
        id: String,
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Toggle a reaction on a message (default thumbs up).
    React {
        id: String,
        #[arg(default_value = "\u{1F44D}")]
        emoji: String,
        /// Remove instead of toggling.
        #[arg(long)]
        remove: bool,
    },
    /// Your sent messages with read receipts.
    Sent {
        #[arg(long)]
        after: Option<i64>,
    },
    /// Create a '#channel' (you become a member).
    Create { channel: String },
    /// Join an existing '#channel'.
    Join { channel: String },
    /// Add a registered agent to a '#channel'.
    Invite { name: String, channel: String },
    /// Leave a '#channel'.
    Leave { channel: String },
    /// List channels.
    Channels,
    /// List a '#channel's members and how far behind they are.
    Members { channel: String },
    /// Set a '#channel's topic; empty text clears it (its creator or a human).
    Topic { channel: String, text: String },
    /// Register <name> for THIS session (self-only; the address is derived).
    Register {
        name: String,
        #[arg(hide = true)]
        address: Option<String>,
    },
    /// Remove a registration.
    Unregister {
        name: String,
        /// Only remove if registered at exactly this address.
        #[arg(long)]
        if_address: Option<String>,
    },
    /// Rename this session's name in place, keeping its messages: `rename <new>`
    /// (the human owner may also run `rename <old> <new>`).
    Rename {
        #[arg(value_name = "OLD_OR_NEW")]
        first: String,
        #[arg(value_name = "NEW")]
        second: Option<String>,
    },
    /// Pin a role to one exact address (human only).
    Pin { name: String, address: String },
    /// Remove a role's pin (human only).
    Unpin { name: String },
    /// Print the address behind <name>.
    Resolve { name: String },
    /// List registered agents.
    Agents,
    /// Print this session's registered name.
    Whoami,
    /// Regex-search what you can see (joined channels and your DMs).
    Search {
        pattern: String,
        /// Limit to a '#channel' or a DM partner.
        target: Option<String>,
        #[arg(long)]
        after: Option<i64>,
    },
    /// Show an agent's profile, or set a field (human only).
    Profile {
        name: String,
        #[arg(long, num_args = 2, value_names = ["KEY", "VALUE"])]
        set: Option<Vec<String>>,
    },
    /// Check the board's health.
    Doctor,
    /// Run the local socket and the notification bridge: with the board in
    /// this process (local mode), or against a conde server (--server).
    Daemon {
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// One bridge pass, then exit (local mode).
        #[arg(long)]
        once: bool,
        /// Cloud mode: the conde server (wss://host); needs `conde enroll` first.
        #[arg(long, value_name = "URL")]
        server: Option<String>,
        /// Seconds before a failed or expired notification is retried (local mode).
        #[arg(long, hide = true)]
        retry_backoff: Option<f64>,
        /// Seconds without a frame from the server before the bridge reconnects.
        #[arg(long, hide = true)]
        idle_timeout: Option<f64>,
    },
    /// Run the cloud server: the board behind HTTP + WebSocket (TLS terminates in front).
    Server {
        /// Listen address (default 0.0.0.0:$PORT, PORT defaulting to 8080).
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// Board directory (default $CONDE_DATA or ~/.local/share/conde).
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
    },
    /// Enroll this host with a conde server: reads the host key at a masked prompt.
    Enroll {
        /// The server URL (https://host).
        #[arg(long, value_name = "URL")]
        server: String,
        /// Host id the key was issued for (default: this machine's).
        #[arg(long)]
        host: Option<String>,
        /// Read the key from stdin instead of the terminal (scripts).
        #[arg(long)]
        key_stdin: bool,
    },
    /// Enroll this host by signing in in the browser: the server mints the host key and the
    /// CLI stores it as `conde enroll` does. The key never appears in a URL.
    Login {
        /// The server URL (https://host).
        #[arg(long, value_name = "URL")]
        server: String,
        /// Host name to enroll as (default: this machine's); the server may suffix it.
        #[arg(long)]
        host: Option<String>,
        /// Print the sign-in URL instead of opening the browser (headless machines).
        #[arg(long)]
        no_browser: bool,
    },
    /// Server administration on the board itself (the human only).
    Admin {
        #[command(subcommand)]
        cmd: AdminCmd,
    },
    /// Load a kond board into the conde store: `conde import --from <kond data dir>`
    /// (e.g. ~/.local/share/kond or a snapshot of it). Idempotent; --dry-run only reports.
    /// Re-runs are supported only before cutover writes: a channel the import would add posts
    /// to that already holds conde messages is refused, and a membership removed in conde is
    /// re-added by a re-run. Validation compares the sha256 of every body and attachment.
    /// The board is DATABASE_URL (Postgres) when set, else the local SQLite file. Where USER
    /// is not set (a container job), --owner and --host are both required.
    Import {
        /// Report what would be imported; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Owner namespace for the imported names (default: $USER; required without USER).
        #[arg(long)]
        owner: Option<String>,
        /// Host id stamped on the imported addresses: the kond machine's (default: this
        /// machine's; required without USER).
        #[arg(long)]
        host: Option<String>,
        /// The snapshot's file count; a directory with another count is refused before reading.
        #[arg(long, value_name = "N")]
        expect_files: Option<usize>,
    },
    /// MCP server for this session's agent on stdin/stdout (Claude Code and Codex spawn it).
    Mcp,
    /// Set up this machine: the bridge service, and the conde skill and MCP server for
    /// Claude Code and Codex. No part named means all. Safe to run again.
    Setup {
        /// Only the Claude Code part.
        #[arg(long)]
        claude: bool,
        /// Only the Codex part.
        #[arg(long)]
        codex: bool,
        /// Only the bridge service.
        #[arg(long)]
        service: bool,
        /// Every part (the default).
        #[arg(long)]
        all: bool,
        /// Leave Claude's crossSessionInbound as it is (by default an unset value becomes "accept").
        #[arg(long)]
        keep_receiver_policy: bool,
        /// Accepted for compatibility; does nothing.
        #[arg(long, hide = true)]
        yes: bool,
        /// Report what is set up and what is missing; change nothing.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum AdminCmd {
    /// Host keys (device credentials).
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Mint a key for a user's host; prints it once.
    Create {
        #[arg(long)]
        user: String,
        #[arg(long)]
        host: String,
        #[arg(long)]
        label: String,
        /// Board directory (default $CONDE_DATA or ~/.local/share/conde).
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
    },
    /// Revoke a host's keys, or one key by id.
    Revoke {
        #[arg(long, required_unless_present = "id")]
        host: Option<String>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
    },
    /// List keys (ids, owners, hosts, labels; never the keys).
    List {
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
    },
}

struct Session {
    client: Client,
    actor: Result<String, IdentityError>,
    from: Option<String>,
}

impl Session {
    fn call(&mut self, op: &str, body: Value) -> anyhow::Result<Value> {
        let mut req = Request::new(new_ulid(), op, body);
        req.idempotency_key = Some(new_ulid());
        req.actor = self.actor.as_ref().ok().cloned();
        req.from = self.from.clone();
        let resp = self.client.call(&req)?;
        if resp.ok {
            return resp.result.ok_or_else(|| anyhow!("empty result"));
        }
        let err = resp.error.ok_or_else(|| anyhow!("error response without error"))?;
        match (&self.actor, err.code) {
            (Err(identity), ErrorCode::Unauthenticated) => bail!("{identity}"),
            _ => bail!("{}", err.message),
        }
    }
}

pub fn main() -> i32 {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("conde: {e:#}");
            1
        }
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    let secs = |s: f64| Duration::from_secs_f64(s.max(0.0));
    match cli.cmd {
        Cmd::Daemon { interval, once, server, retry_backoff, idle_timeout } => {
            tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();
            daemon::run(daemon::Options {
                data_dir: paths::data_dir(),
                socket: paths::socket_path(),
                interval: Duration::from_secs_f64(interval.max(0.05)),
                once,
                server,
                retry_backoff: retry_backoff.map(secs),
                idle_timeout: idle_timeout.map(secs),
            })?;
            return Ok(0);
        }
        Cmd::Server { listen, data } => {
            tracing_subscriber::fmt().with_writer(std::io::stdout).with_target(false).init();
            admin::server(listen.as_deref(), data)?;
            return Ok(0);
        }
        Cmd::Enroll { server, host, key_stdin } => return admin::enroll(&server, host.as_deref(), key_stdin),
        Cmd::Login { server, host, no_browser } => return crate::login::run(&server, host.as_deref(), !no_browser),
        Cmd::Mcp => {
            crate::mcp::run(paths::socket_path())?;
            return Ok(0);
        }
        Cmd::Setup { claude, codex, service, all, keep_receiver_policy, yes: _, check } => {
            return setup(claude, codex, service, all, keep_receiver_policy, check);
        }
        Cmd::Admin { cmd: AdminCmd::Key { cmd } } => {
            admin::require_human()?;
            return match cmd {
                KeyCmd::Create { user, host, label, data } => admin::key_create(data, &user, &host, &label),
                KeyCmd::Revoke { host, id, data } => admin::key_revoke(data, host.as_deref(), id.as_deref()),
                KeyCmd::List { data } => admin::key_list(data),
            };
        }
        _ => {}
    }
    if let Cmd::Import { dry_run, owner, host, expect_files } = &cli.cmd {
        // `--from` is the global flag here: the kond data directory to import.
        let dir = cli.from.as_deref().ok_or_else(|| anyhow!("conde import needs --from <kond data dir>"))?;
        return crate::import::main(Path::new(dir), *dry_run, owner.as_deref(), host.as_deref(), *expect_files);
    }
    let mut session = Session {
        client: Client::connect(&paths::socket_path())?,
        actor: derive_address(&Markers::from_env(), harness_ancestor),
        from: cli.from,
    };
    let sx = &mut session;
    match cli.cmd {
        Cmd::Daemon { .. } | Cmd::Server { .. } | Cmd::Enroll { .. } | Cmd::Login { .. } | Cmd::Admin { .. } => {
            unreachable!()
        }
        Cmd::Import { .. } | Cmd::Mcp | Cmd::Setup { .. } => unreachable!(),
        Cmd::Send { to, text, attach, reply_to } => {
            let attachments = attach.iter().map(|p| encode_attachment(p)).collect::<anyhow::Result<Vec<_>>>()?;
            let target = if to.starts_with('#') { json!({"channel": to}) } else { json!({"agent": to}) };
            let r = sx.call(
                "message.send",
                json!({"to": target, "text": text, "reply_to": reply_to, "attachments": attachments}),
            )?;
            println!("queued {} to {}", s(&r["message_id"]), s(&r["to"]));
        }
        Cmd::Inbox { after } => {
            let r = sx.call("inbox.list", json!({"after": after}))?;
            let items = r["items"].as_array().cloned().unwrap_or_default();
            for i in &items {
                println!("{}\t{}\t{}\t{}\t{}", s(&i["kind"]), s(&i["message_id"]), s(&i["from"]), s(&i["date"]), s(&i["preview"]));
            }
            if items.is_empty() {
                eprintln!("(no unread messages)");
            }
            if let Some(n) = r["next_cursor"].as_i64() {
                eprintln!("(more: conde inbox --after {n})");
            }
        }
        Cmd::Read { target } => {
            let body = match &target {
                Some(t) if t.starts_with('#') => json!({"channel": t}),
                Some(id) => json!({"id": id}),
                None => json!({}),
            };
            let r = sx.call("message.read", body)?;
            let messages = r["messages"].as_array().cloned().unwrap_or_default();
            let mut place = String::new();
            for m in &messages {
                if s(&m["place"]) != place {
                    place = s(&m["place"]).to_string();
                    if place.starts_with('#') {
                        println!("--- {place}");
                    }
                }
                print_message(m);
            }
            if messages.is_empty() {
                eprintln!("(no unread messages)");
            }
            if r["more"].as_bool() == Some(true) {
                eprintln!("(more unread: run conde read again)");
            }
        }
        Cmd::Show { id } => print_message(&sx.call("message.get", json!({"id": id}))?["message"]),
        Cmd::Attachment { id, out } => {
            let r = sx.call("attachment.get", json!({"attachment_id": id}))?;
            let bytes = B64.decode(s(&r["data"])).context("attachment data")?;
            let name = Path::new(s(&r["filename"])).file_name().ok_or_else(|| anyhow!("attachment has no usable filename"))?;
            let path = out.join(name);
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
            println!("{}", path.display());
        }
        Cmd::React { id, emoji, remove } => {
            let present = if remove {
                false
            } else {
                let me = sx.call("agent.whoami", json!({}))?["name"].as_str().unwrap_or_default().to_string();
                let m = sx.call("message.get", json!({"id": id}))?;
                let mine = m["message"]["reactions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|r| s(&r["emoji"]) == emoji && r["by"].as_array().into_iter().flatten().any(|b| s(b) == me));
                !mine
            };
            let r = sx.call("reaction.set", json!({"message_id": id, "emoji": emoji, "present": present}))?;
            if present {
                println!("reacted {emoji} to {id} in {}", s(&r["place"]));
            } else {
                println!("removed {emoji} from {id} in {}", s(&r["place"]));
            }
        }
        Cmd::Sent { after } => {
            let r = sx.call("receipt.list", json!({"after": after}))?;
            let rows = r["receipts"].as_array().cloned().unwrap_or_default();
            for x in &rows {
                let delivery = x["delivery"].as_str().map(|d| format!("  delivery: {d}")).unwrap_or_default();
                println!("{}  {}  {}{delivery}", s(&x["message_id"]), s(&x["to"]), s(&x["status"]));
            }
            if rows.is_empty() {
                eprintln!("(nothing sent)");
            }
        }
        Cmd::Create { channel } => {
            let r = sx.call("channel.create", json!({"channel": channel}))?;
            println!("created {} (you are a member)", s(&r["channel"]));
        }
        Cmd::Join { channel } => {
            let r = sx.call("channel.join", json!({"channel": channel}))?;
            if r["already_member"].as_bool() == Some(true) {
                println!("already a member of {}", s(&r["channel"]));
            } else {
                println!("joined {} (join post {})", s(&r["channel"]), s(&r["message_id"]));
                if let Some(topic) = r["topic"].as_str() {
                    println!("topic: {topic}");
                }
            }
        }
        Cmd::Invite { name, channel } => {
            let r = sx.call("channel.invite", json!({"name": name, "channel": channel}))?;
            println!("invited {} to {} (join post {})", s(&r["name"]), s(&r["channel"]), s(&r["message_id"]));
        }
        Cmd::Topic { channel, text } => {
            let r = sx.call("channel.topic", json!({"channel": channel, "topic": text}))?;
            match r["topic"].as_str() {
                Some(topic) => println!("topic of {}: {topic}", s(&r["channel"])),
                None => println!("cleared the topic of {}", s(&r["channel"])),
            }
        }
        Cmd::Leave { channel } => {
            let r = sx.call("channel.leave", json!({"channel": channel}))?;
            println!("left {}", s(&r["channel"]));
        }
        Cmd::Channels => {
            let r = sx.call("channel.list", json!({}))?;
            let rows = r["channels"].as_array().cloned().unwrap_or_default();
            for c in &rows {
                let name = match c["topic"].as_str() {
                    Some(topic) => format!("{}: {topic}", s(&c["channel"])),
                    None => s(&c["channel"]).to_string(),
                };
                println!("{name}\t{} members\t{} messages", c["members"], c["messages"]);
            }
            if rows.is_empty() {
                eprintln!("(no channels)");
            }
        }
        Cmd::Members { channel } => {
            let r = sx.call("channel.members", json!({"channel": channel}))?;
            for m in r["members"].as_array().into_iter().flatten() {
                match m["behind"].as_i64() {
                    Some(0) => println!("{}\tup to date", s(&m["name"])),
                    Some(behind) => println!("{}\t{behind} behind", s(&m["name"])),
                    None => println!("{}", s(&m["name"])),
                }
            }
        }
        Cmd::Register { name, address } => {
            let r = sx.call("agent.register", json!({"name": name, "address": address}))?;
            println!("registered {} -> {}", s(&r["name"]), s(&r["address"]));
            if let Some(lobby) = r["joined"].as_str() {
                println!("joined {lobby}");
            }
        }
        Cmd::Unregister { name, if_address } => {
            let r = sx.call("agent.unregister", json!({"name": name, "if_address": if_address}))?;
            if r["was_registered"].as_bool() == Some(true) {
                println!("unregistered {} ({})", s(&r["name"]), s(&r["address"]));
            } else {
                println!("{} was not registered", s(&r["name"]));
            }
        }
        Cmd::Rename { first, second } => {
            let (old, new) = match second {
                Some(new) => (Some(first), new),
                None => (None, first),
            };
            let r = sx.call("agent.rename", json!({"old": old, "new": new}))?;
            println!("renamed {} -> {}", s(&r["old"]), s(&r["name"]));
        }
        Cmd::Pin { name, address } => {
            let r = sx.call("agent.pin", json!({"name": name, "address": address}))?;
            println!("pinned {} -> {}", s(&r["name"]), s(&r["pinned"]));
        }
        Cmd::Unpin { name } => {
            let r = sx.call("agent.pin", json!({"name": name, "address": null}))?;
            println!("unpinned {}", s(&r["name"]));
        }
        Cmd::Resolve { name } => println!("{}", s(&sx.call("agent.resolve", json!({"name": name}))?["address"])),
        Cmd::Agents => {
            let r = sx.call("agent.list", json!({}))?;
            let rows = r["agents"].as_array().cloned().unwrap_or_default();
            for a in &rows {
                let display = a["display"].as_str().map(|d| format!("\t({d})")).unwrap_or_default();
                println!("{}\t{}{display}", s(&a["name"]), s(&a["address"]));
            }
            if rows.is_empty() {
                eprintln!("(registry empty)");
            }
        }
        Cmd::Whoami => println!("{}", s(&sx.call("agent.whoami", json!({}))?["name"])),
        Cmd::Search { pattern, target, after } => {
            let r = sx.call("search", json!({"pattern": pattern, "target": target, "after": after}))?;
            let hits = r["hits"].as_array().cloned().unwrap_or_default();
            for h in &hits {
                let date: String = s(&h["date"]).chars().take(16).collect();
                println!("{}\t{}\t{}\t{date}\t{}", s(&h["place"]), s(&h["message_id"]), s(&h["from"]), s(&h["line"]));
            }
            if hits.is_empty() {
                eprintln!("(no matches)");
            }
            if let Some(n) = r["next_cursor"].as_i64() {
                eprintln!("(more: add --after {n})");
            }
        }
        Cmd::Profile { name, set: Some(kv) } => {
            let r = sx.call("profile.set", json!({"name": name, "key": kv[0], "value": kv[1]}))?;
            println!("profile {}: {} = {}", s(&r["name"]), s(&r["key"]), r["value"].as_str().unwrap_or("(unset)"));
        }
        Cmd::Profile { name, set: None } => {
            let r = sx.call("profile.get", json!({"name": name}))?;
            println!("name: {}", s(&r["name"]));
            println!("address: {}", r["address"].as_str().unwrap_or("(not registered)"));
            if let Some(d) = r["display"].as_str() {
                println!("Display: {d}");
            }
            if let Some(p) = r["pinned"].as_str() {
                println!("pinned: {p}");
            }
        }
        Cmd::Doctor => {
            let r = sx.call("doctor", json!({}))?;
            println!("ok      daemon: reachable at {}", paths::socket_path().display());
            for c in r["checks"].as_array().into_iter().flatten() {
                println!("{}{}", if c["ok"] == true { "ok      " } else { "ISSUE   " }, s(&c["text"]));
            }
            let issues = r["issues"].as_i64().unwrap_or(0);
            if issues == 0 {
                println!("all clear");
            }
            return Ok(i32::from(issues > 0));
        }
    }
    Ok(0)
}

/// `conde setup`: the service bridges to the server this host enrolled with,
/// else it runs a local board.
fn setup(claude: bool, codex: bool, service: bool, all: bool, keep_receiver_policy: bool, check: bool) -> anyhow::Result<i32> {
    use crate::setup::{Env, Options, System, run, verify};
    let env = Env::from_process()?;
    let mut opts = Options::select(claude, codex, service, all);
    opts.keep_receiver_policy = keep_receiver_policy;
    if opts.service {
        let cred = crate::credentials::path()?;
        if cred.exists() {
            opts.server = Some(crate::credentials::load(&cred)?.server);
        } else if check {
            println!("note    service: this host is not enrolled ({} missing); the bridge runs a local board", cred.display());
        } else {
            println!(
                "service: this host is not enrolled ({} missing), so the bridge runs a local board; \
                 after conde login or conde enroll, run conde setup again",
                cred.display()
            );
        }
    }
    let ok = if check {
        verify(&env, &opts, &mut System, &mut std::io::stdout())?
    } else {
        run(&env, &opts, &mut System, &mut std::io::stdout())?
    };
    Ok(i32::from(!ok))
}

fn encode_attachment(path: &Path) -> anyhow::Result<Value> {
    let meta = std::fs::metadata(path).with_context(|| format!("attachment not found: {}", path.display()))?;
    if !meta.is_file() {
        bail!("attachment is not a file: {}", path.display());
    }
    if meta.len() > MAX_ATTACHMENT as u64 {
        bail!("attachment {} is {} bytes; the limit is {MAX_ATTACHMENT}", path.display(), meta.len());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(json!({"filename": name, "data": B64.encode(bytes)}))
}

fn print_message(m: &Value) {
    println!("=== {}", s(&m["message_id"]));
    println!("From: {}", s(&m["from"]));
    println!("To: {}", s(&m["to"]));
    println!("Date: {}", s(&m["date"]));
    if let Some(r) = m["reply_to"].as_str() {
        println!("In-Reply-To: {r}");
    }
    for a in m["attachments"].as_array().into_iter().flatten() {
        println!("Attachment: {} ({} bytes; conde attachment {})", s(&a["filename"]), a["size"], s(&a["attachment_id"]));
    }
    for r in m["reactions"].as_array().into_iter().flatten() {
        let by: Vec<&str> = r["by"].as_array().into_iter().flatten().map(s).collect();
        println!("Reaction: {} {}", s(&r["emoji"]), by.join(", "));
    }
    println!();
    let text = s(&m["text"]);
    if text.ends_with('\n') {
        print!("{text}");
    } else {
        println!("{text}");
    }
}
