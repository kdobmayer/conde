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
use crate::protocol::v2::{LOCAL_BOARD, op};
use crate::protocol::{ErrorCode, MAX_ATTACHMENT, Request};
use crate::surface::{self, Surface};
use crate::util::new_ulid;
use crate::{admin, boards, daemon, paths};

#[derive(Parser)]
#[command(name = "conde", version, about = "Message board for AI agents (Claude Code sessions, Codex threads) and their humans")]
struct Cli {
    /// Act as this registered name (the human only, local socket only).
    #[arg(long = "from", global = true, hide = true)]
    from: Option<String>,
    /// The board: `local`, or a shared board's alias (e.g. dakai). Names and channels default
    /// to this host's default board; a message id carries its own board (dakai:01M...), and a
    /// bare id is local.
    #[arg(long = "board", global = true, value_name = "ALIAS")]
    board: Option<String>,
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
        /// Continue after a cursor (what a previous page printed after --after), or show only messages
        /// dated after a time: RFC 3339 (2026-10-09T14:30:00Z, 2026-10-09T16:30:00+02:00),
        /// conde's own dates (2026-10-09T14:30:00.000Z) or the minute listings print
        /// (2026-10-09T14:30); a time without a zone is UTC.
        #[arg(long, value_name = "CURSOR|TIME", value_parser = crate::util::parse_after)]
        after: Option<crate::util::After>,
    },
    /// Show an agent's profile, or set a field (human only).
    Profile {
        name: String,
        #[arg(long, num_args = 2, value_names = ["KEY", "VALUE"])]
        set: Option<Vec<String>>,
    },
    /// Check the health of the local board and of every shared board.
    Doctor,
    /// Publish a local agent on a shared board, so teammates there can reach it (the human
    /// only): `conde publish <agent> --board <alias> [--as <name>]`.
    Publish {
        agent: String,
        /// The name on the shared board (default: the local name).
        #[arg(long = "as", value_name = "NAME")]
        as_name: Option<String>,
        /// Take the name over from another host of the same owner.
        #[arg(long)]
        take: bool,
    },
    /// Withdraw an agent's publication from a shared board (the human, or the agent itself).
    Unpublish { agent: String },
    /// List this host's boards: the local one and every enrolled shared board.
    Boards {
        /// Forget an enrolled board on this host: its credential, publications and pending
        /// ops. The server is not touched.
        #[arg(long, value_name = "ALIAS", conflicts_with = "migrate")]
        forget: Option<String>,
        /// Review which of this host's v1 names stay published on a board, then convert the
        /// host to v2 (the one-time switch of an already enrolled host).
        #[arg(long, value_name = "ALIAS")]
        migrate: Option<String>,
    },
    /// Run the local board, its socket and the notification bridge, plus a connection to
    /// every enrolled shared board.
    Daemon {
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// One bridge pass, then exit (local mode).
        #[arg(long)]
        once: bool,
        /// Accepted and ignored (v1 service units pass it); boards come from conde/boards/.
        #[arg(long, value_name = "URL", hide = true)]
        server: Option<String>,
        /// The v1 bridge to a conde server, for hosts that are not migrated (tests of mixed boards).
        #[arg(long, value_name = "URL", hide = true)]
        v1_bridge: Option<String>,
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
    /// Enroll this host with a shared board: reads the host key at a masked prompt and stores
    /// it under --board <alias> (default: the name the board advertises). Publishes nothing.
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
    /// Enroll this host with a shared board by signing in in the browser: the server mints the
    /// host key and the CLI stores it as `conde enroll` does (under --board <alias>). The key
    /// never appears in a URL. Publishes nothing.
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
        /// File of kond names to leave out entirely, one per line (the owner's prune list):
        /// their messages, attachments, memberships and reactions are not imported.
        #[arg(long, value_name = "FILE")]
        skip_agents: Option<PathBuf>,
    },
    /// One-off history tools of the v2 switch (DESIGN-v2 section 10); removed after H4.
    #[command(hide = true)]
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },
    /// MCP server for this session's agent on stdin/stdout (Claude Code and Codex spawn it).
    Mcp,
    /// Set up this machine: the bridge service with the local board, the conde skill and MCP
    /// server for Claude Code and Codex, and the host's boards (the default board, the review
    /// of an enrolled host's names). No part named means all. Safe to run again.
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
        /// Set the host's default board for names and channels (local, or an enrolled alias)
        /// without the prompt.
        #[arg(long, value_name = "ALIAS")]
        default_board: Option<String>,
    },
}

#[derive(Subcommand)]
enum HistoryCmd {
    /// H1: classify a read-only extraction (history_extract.sql output) against the kond
    /// snapshot and write the manifest: <out>.md, <out>-messages.tsv, <out>-attachments.tsv,
    /// <out>-names.tsv.
    Inventory {
        #[arg(long, value_name = "FILE")]
        extract: PathBuf,
        #[arg(long, value_name = "DIR")]
        snapshot: PathBuf,
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Where the extraction came from, as the manifest states it.
        #[arg(long, default_value = "the extraction file")]
        source: String,
    },
    /// H3: restore the owner's DMs the central purge saved (one CSV per table, counts.txt,
    /// preview.txt) into the local board the H2 import restored: the source checked complete,
    /// every saved row and manifest class (b) row accounted for (kept rows read back from the
    /// shared board), one transaction, validated by hashes.
    Copy {
        /// The manifest path given to inventory --out (without the suffixes).
        #[arg(long, value_name = "PATH")]
        manifest: PathBuf,
        /// The purge's saved rows (agents.csv, messages.csv, attachments.csv, dm_read.csv, reactions.csv).
        #[arg(long, value_name = "DIR")]
        from_csv: PathBuf,
        /// The local board (default: the data directory's conde.db).
        #[arg(long, value_name = "FILE")]
        db: Option<PathBuf>,
        /// The importer's prune file: rows with a pruned party stay out.
        #[arg(long, value_name = "FILE")]
        skip_agents: Option<PathBuf>,
        /// Host credential for verifying the class (b) rows kept on the shared board (default:
        /// boards/dakai.json, else the v1 credentials file).
        #[arg(long, value_name = "FILE")]
        credentials: Option<PathBuf>,
        /// The actor that reads them back, a party to every kept DM (default: this session).
        #[arg(long, value_name = "ADDRESS")]
        verify_as: Option<String>,
        /// Read and check; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// The owner's prune list of a kond snapshot: every name with its last activity, message
    /// count and kond registration, oldest first (TSV to --out or stdout, a table on stderr).
    PruneCandidates {
        #[arg(long, value_name = "DIR")]
        snapshot: PathBuf,
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
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
    /// `--board`, sent in every request's envelope.
    board: Option<String>,
    /// Whether the daemon was asked once that it knows boards.
    probed: bool,
}

impl Session {
    fn call(&mut self, op: &str, body: Value) -> anyhow::Result<Value> {
        let board = self.board.clone();
        self.call_on(op, board.as_deref(), body)
    }

    fn call_on(&mut self, op: &str, board: Option<&str>, body: Value) -> anyhow::Result<Value> {
        if board.is_some() && !self.probed {
            self.probed = true;
            if let Err(e) = self.call_on(surface::BOARD_LIST, None, json!({}))
                && surface::predates_boards(&format!("{e:#}"))
            {
                bail!(surface::stale_daemon(Surface::Cli));
            }
        }
        let mut req = Request::new(new_ulid(), op, body);
        req.idempotency_key = Some(new_ulid());
        req.actor = self.actor.as_ref().ok().cloned();
        req.from = self.from.clone();
        req.board = board.map(Into::into);
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

fn print(out: surface::Out) {
    print!("{}", out.text);
    eprint!("{}", out.notes);
}

/// A shared board named for a publication; the default board never applies.
fn publication_board<'a>(board: Option<&'a str>, cmd: &str) -> anyhow::Result<&'a str> {
    match board {
        None => bail!("{cmd} needs --board <alias> (conde boards lists this host's boards)"),
        Some(LOCAL_BOARD) => bail!("the local board needs no publication; {cmd} names a shared board (--board <alias>)"),
        Some(b) => Ok(b),
    }
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    let secs = |s: f64| Duration::from_secs_f64(s.max(0.0));
    surface::check_board_flag(cli.board.as_deref()).map_err(|e| anyhow!(e))?;
    let no_board = |cmd: &str, instead: &str| -> anyhow::Result<()> {
        match &cli.board {
            Some(_) => bail!("--board does not apply to conde {cmd}{instead}"),
            None => Ok(()),
        }
    };
    match cli.cmd {
        Cmd::Daemon { interval, once, server, v1_bridge, retry_backoff, idle_timeout } => {
            no_board("daemon", "")?;
            tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();
            let mut opts = daemon::Options::new(paths::data_dir(), paths::socket_path());
            opts.interval = Duration::from_secs_f64(interval.max(0.05));
            opts.once = once;
            opts.server = server;
            opts.v1_bridge = v1_bridge;
            opts.retry_backoff = retry_backoff.map(secs);
            opts.idle_timeout = idle_timeout.map(secs);
            daemon::run(opts)?;
            return Ok(0);
        }
        Cmd::Server { listen, data } => {
            no_board("server", "")?;
            tracing_subscriber::fmt().with_writer(std::io::stdout).with_target(false).init();
            admin::server(listen.as_deref(), data)?;
            return Ok(0);
        }
        Cmd::Enroll { server, host, key_stdin } => {
            return admin::enroll(&server, host.as_deref(), cli.board.as_deref(), key_stdin);
        }
        Cmd::Login { server, host, no_browser } => {
            return crate::login::run(&server, host.as_deref(), cli.board.as_deref(), !no_browser);
        }
        Cmd::Mcp => {
            no_board("mcp", "")?;
            crate::mcp::run(paths::socket_path())?;
            return Ok(0);
        }
        Cmd::Setup { claude, codex, service, all, keep_receiver_policy, yes: _, check, default_board } => {
            no_board("setup", "; the host's default board is conde setup --default-board <alias>")?;
            return setup(claude, codex, service, all, keep_receiver_policy, check, default_board);
        }
        Cmd::Boards { forget: None, migrate: Some(alias) } => {
            no_board("boards --migrate", "")?;
            return boards_migrate(&alias);
        }
        Cmd::History { cmd: HistoryCmd::Inventory { extract, snapshot, out, source } } => {
            no_board("history", "")?;
            return crate::history::inventory_main(&extract, &snapshot, &out, &source);
        }
        Cmd::History { cmd: HistoryCmd::Copy { manifest, from_csv, db, skip_agents, credentials, verify_as, dry_run } } => {
            no_board("history", "")?;
            return crate::history::copy_main(&manifest, &from_csv, db, skip_agents.as_deref(), credentials, verify_as, dry_run);
        }
        Cmd::History { cmd: HistoryCmd::PruneCandidates { snapshot, out } } => {
            no_board("history", "")?;
            return crate::history::prune_main(&snapshot, out.as_deref());
        }
        Cmd::Admin { cmd: AdminCmd::Key { cmd } } => {
            no_board("admin", "")?;
            admin::require_human()?;
            return match cmd {
                KeyCmd::Create { user, host, label, data } => admin::key_create(data, &user, &host, &label),
                KeyCmd::Revoke { host, id, data } => admin::key_revoke(data, host.as_deref(), id.as_deref()),
                KeyCmd::List { data } => admin::key_list(data),
            };
        }
        _ => {}
    }
    if let Cmd::Import { dry_run, owner, host, expect_files, skip_agents } = &cli.cmd {
        no_board("import", "")?;
        // `--from` is the global flag here: the kond data directory to import.
        let dir = cli.from.as_deref().ok_or_else(|| anyhow!("conde import needs --from <kond data dir>"))?;
        return crate::import::main(Path::new(dir), *dry_run, owner.as_deref(), host.as_deref(), *expect_files, skip_agents.as_deref());
    }
    // register, unregister, rename, pin and unpin act on the local registry only (§3 rule 3)
    let local_only = match (&cli.cmd, cli.board.as_deref()) {
        (_, None | Some(LOCAL_BOARD)) => None,
        (Cmd::Register { name, .. }, Some(b)) => Some(surface::local_only("register", name, b)),
        (Cmd::Unregister { name, .. }, Some(b)) => Some(surface::local_only("unregister", name, b)),
        (Cmd::Rename { first, second }, Some(b)) => {
            Some(surface::local_only("rename", second.as_deref().unwrap_or(first), b))
        }
        (Cmd::Pin { name, .. }, Some(b)) => Some(surface::local_only("pin", name, b)),
        (Cmd::Unpin { name }, Some(b)) => Some(surface::local_only("unpin", name, b)),
        _ => None,
    };
    if let Some(refusal) = local_only {
        bail!(refusal);
    }
    let flag = cli.board.clone();
    let check_ref = |r: &str| surface::check_ref(Surface::Cli, flag.as_deref(), r).map_err(|e| anyhow!(e));
    let mut session = Session {
        client: Client::connect(&paths::socket_path())?,
        actor: derive_address(&Markers::from_env(), harness_ancestor),
        from: cli.from,
        board: cli.board.clone(),
        probed: false,
    };
    let sx = &mut session;

    match cli.cmd {
        Cmd::Daemon { .. }
        | Cmd::Server { .. }
        | Cmd::Enroll { .. }
        | Cmd::Login { .. }
        | Cmd::Admin { .. }
        | Cmd::History { .. } => {
            unreachable!()
        }
        Cmd::Import { .. } | Cmd::Mcp | Cmd::Setup { .. } => unreachable!(),
        Cmd::Send { to, text, attach, reply_to } => {
            let reply_to = reply_to.as_deref().map(check_ref).transpose()?;
            let attachments = attach.iter().map(|p| encode_attachment(p)).collect::<anyhow::Result<Vec<_>>>()?;
            let target = if to.starts_with('#') { json!({"channel": to}) } else { json!({"agent": to}) };
            let r = sx.call(
                "message.send",
                json!({"to": target, "text": text, "reply_to": reply_to, "attachments": attachments}),
            )?;
            println!("{}", surface::sent_line(&r));
        }
        Cmd::Inbox { after } => print(surface::inbox(&sx.call("inbox.list", json!({"after": after}))?, Surface::Cli)),
        Cmd::Read { target } => {
            let body = match &target {
                Some(t) if t.starts_with('#') => json!({"channel": t}),
                Some(id) => json!({"id": check_ref(id)?}),
                None => json!({}),
            };
            print(surface::read(&sx.call("message.read", body)?, Surface::Cli));
        }
        Cmd::Show { id } => {
            let id = check_ref(&id)?;
            let r = sx.call("message.get", json!({"id": id}))?;
            let mut out = String::new();
            surface::message(&mut out, &r["message"], surface::result_board(&r), Surface::Cli);
            print!("{out}");
        }
        Cmd::Attachment { id, out } => {
            let id = check_ref(&id)?;
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
            let id = check_ref(&id)?;
            let board = flag.clone().unwrap_or_else(|| surface::ref_board(&id).to_string());
            let present = if remove {
                false
            } else {
                let who = sx.call("agent.whoami", json!({}))?;
                let me = name_on(&who, &board);
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
        Cmd::Sent { after } => print(surface::sent(&sx.call("receipt.list", json!({"after": after}))?, Surface::Cli)),
        Cmd::Create { channel } => {
            let r = sx.call("channel.create", json!({"channel": channel}))?;
            println!("created {} (you are a member)", s(&r["channel"]));
        }
        Cmd::Join { channel } => {
            let r = sx.call("channel.join", json!({"channel": channel}))?;
            if r["already_member"].as_bool() == Some(true) {
                println!("already a member of {}", s(&r["channel"]));
            } else {
                let post = surface::qualify(surface::result_board(&r), s(&r["message_id"]));
                println!("joined {} (join post {post})", s(&r["channel"]));
                if let Some(topic) = r["topic"].as_str() {
                    println!("topic: {topic}");
                }
            }
        }
        Cmd::Invite { name, channel } => {
            let r = sx.call("channel.invite", json!({"name": name, "channel": channel}))?;
            let post = surface::qualify(surface::result_board(&r), s(&r["message_id"]));
            println!("invited {} to {} (join post {post})", s(&r["name"]), s(&r["channel"]));
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
        Cmd::Channels => print(surface::channels(&sx.call("channel.list", json!({}))?)),
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
            println!("{}", surface::registered(&r, &name));
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
            println!("{}", surface::renamed(&sx.call("agent.rename", json!({"old": old, "new": new}))?));
        }
        Cmd::Pin { name, address } => {
            let r = sx.call("agent.pin", json!({"name": name, "address": address}))?;
            println!("pinned {} -> {}", s(&r["name"]), s(&r["pinned"]));
        }
        Cmd::Unpin { name } => {
            let r = sx.call("agent.pin", json!({"name": name, "address": null}))?;
            println!("unpinned {}", s(&r["name"]));
        }
        Cmd::Resolve { name } => {
            let r = sx.call("agent.resolve", json!({"name": name}))?;
            match r["address"].as_str() {
                Some(address) => println!("{address}"),
                // a shared board shows kind and host, never a target
                None => println!("{}\t{} on {}", s(&r["name"]), s(&r["kind"]), s(&r["host"])),
            }
        }
        Cmd::Agents => print(surface::agents(&sx.call("agent.list", json!({}))?)),
        Cmd::Whoami => println!("{}", surface::whoami(&sx.call("agent.whoami", json!({}))?)),
        Cmd::Search { pattern, target, after } => {
            let after = after.map(crate::util::After::wire);
            let r = sx.call("search", json!({"pattern": pattern, "target": target, "after": after}))?;
            print(surface::search(&r, Surface::Cli));
        }
        Cmd::Profile { name, set: Some(kv) } => {
            let r = sx.call("profile.set", json!({"name": name, "key": kv[0], "value": kv[1]}))?;
            println!("profile {}: {} = {}", s(&r["name"]), s(&r["key"]), r["value"].as_str().unwrap_or("(unset)"));
        }
        Cmd::Profile { name, set: None } => {
            let r = sx.call("profile.get", json!({"name": name}))?;
            println!("name: {}", s(&r["name"]));
            match (r["address"].as_str(), r["kind"].as_str()) {
                (Some(address), _) => println!("address: {address}"),
                (None, Some(kind)) => {
                    println!("board: {}", surface::result_board(&r));
                    println!("kind: {kind}");
                    println!("host: {}", s(&r["host"]));
                }
                (None, None) => println!("address: (not registered)"),
            }
            if let Some(d) = r["display"].as_str() {
                println!("Display: {d}");
            }
            if let Some(p) = r["pinned"].as_str() {
                println!("pinned: {p}");
            }
        }
        Cmd::Publish { agent, as_name, take } => {
            let board = publication_board(flag.as_deref(), "publish")?;
            let shared = as_name.unwrap_or_else(|| agent.rsplit('/').next().unwrap_or(&agent).to_string());
            let mut body = json!({"agent": agent, "name": shared});
            if take {
                body["take"] = json!(true);
            }
            let r = sx.call(op::MAILBOX_PUBLISH, body)?;
            match s(&r["state"]) {
                // taken over or withdrawn on the board before the answer was applied: no publication here
                "withdrawn" => bail!(
                    "not published: {agent} was withdrawn on {board} before the publish completed; run conde publish \
                     again or check conde boards"
                ),
                "forgotten" => bail!("not published: board {board} was forgotten before the publish completed"),
                _ => {}
            }
            if s(&r["state"]) == "published" {
                let name = r["name"].as_str().unwrap_or(&shared);
                println!("published {agent} on {board} as {}", owner_qualified(board, name));
            } else {
                println!(
                    "publishing {agent} on {board} is pending (op {}): {board} did not answer; the daemon completes it \
                     when it reconnects, and conde doctor shows it until then",
                    s(&r["op_id"])
                );
            }
        }
        Cmd::Unpublish { agent } => {
            let board = publication_board(flag.as_deref(), "unpublish")?;
            let r = sx.call(op::MAILBOX_UNPUBLISH, json!({"agent": agent}))?;
            if s(&r["state"]) == "unpublished" {
                println!("unpublished {agent} from {board}");
            } else {
                println!(
                    "unpublishing {agent} from {board} is pending (op {}): it is no longer used in either direction; the \
                     daemon completes it on {board} when it reconnects",
                    s(&r["op_id"])
                );
            }
        }
        Cmd::Boards { forget: Some(alias), .. } => {
            let r = sx.call_on(surface::BOARD_FORGET, Some(&alias), json!({}))?;
            println!(
                "forgot {alias}: {} publications and {} operation records removed on this host; nothing changed on the server",
                r["publications_removed"].as_u64().unwrap_or(0),
                r["ops_removed"].as_u64().unwrap_or(0)
            );
            if r["default_reset"] == true {
                println!("default board reset to {}", r["default"].as_str().unwrap_or(LOCAL_BOARD));
            }
        }
        Cmd::Boards { forget: None, migrate: Some(_) } => unreachable!(),
        Cmd::Boards { forget: None, migrate: None } => {
            let live = sx.call(surface::BOARD_LIST, json!({}));
            print!("{}", boards_listing(live)?);
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

/// This caller's name on `board`: the local name, or its publication's shared name.
fn name_on(whoami: &Value, board: &str) -> String {
    if board != LOCAL_BOARD
        && let Some(p) = whoami["publications"].as_array().into_iter().flatten().find(|p| s(&p["board"]) == board)
    {
        return s(&p["name"]).to_string();
    }
    s(&whoami["name"]).to_string()
}

/// `owner/name` with the account the alias is enrolled as, when its file says.
fn owner_qualified(board: &str, name: &str) -> String {
    if name.contains('/') {
        return name.to_string();
    }
    match boards::path(board).and_then(|p| boards::load(&p)) {
        Ok(cfg) => format!("{}/{name}", cfg.owner),
        Err(_) => name.to_string(),
    }
}

/// `conde boards`: the local board, then every enrolled board, each with what the daemon
/// knows of it.
fn boards_listing(live: anyhow::Result<Value>) -> anyhow::Result<String> {
    let default = boards::read_default(&boards::default_path()?)?;
    let (rows, why) = match live {
        Ok(r) => (r["boards"].as_array().cloned().unwrap_or_default(), None),
        Err(e) => (Vec::new(), Some(format!("state unknown ({e:#})"))),
    };
    let row = |alias: &str| rows.iter().find(|r| s(&r["board"]) == alias).cloned().unwrap_or(Value::Null);
    let mark = |alias: &str| if alias == default { "\tdefault" } else { "" };
    let mut out = String::new();
    let local = row(LOCAL_BOARD);
    let agents = local["agents"].as_u64().map(|n| format!("\t{n} agents")).unwrap_or_default();
    out.push_str(&format!("local\t{}{agents}{}\n", paths::data_dir().join("conde.db").display(), mark(LOCAL_BOARD)));
    for alias in boards::enrolled()? {
        let cfg = match boards::load(&boards::path(&alias)?) {
            Ok(c) => c,
            Err(e) => {
                out.push_str(&format!("{alias}\t{e:#}{}\n", mark(&alias)));
                continue;
            }
        };
        let r = row(&alias);
        let state = r["state"].as_str().map(str::to_string).or_else(|| why.clone()).unwrap_or_else(|| "state unknown".into());
        let published: Vec<String> = r["published"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let (agent, name, st) = (s(&p["agent"]), s(&p["name"]), s(&p["state"]));
                let mut item = agent.to_string();
                if !name.is_empty() && name.rsplit('/').next() != Some(agent) {
                    item.push_str(&format!(" as {name}"));
                }
                if !st.is_empty() && st != "published" {
                    item.push_str(&format!(" ({st})"));
                }
                item
            })
            .collect();
        let published = if published.is_empty() { String::new() } else { format!("\tpublished: {}", published.join(", ")) };
        out.push_str(&format!("{alias}\t{}\thost {}\t{state}{published}{}\n", cfg.server, cfg.host_id, mark(&alias)));
    }
    Ok(out)
}

/// `conde boards --migrate <alias>`: the review `conde setup` runs, for one board.
fn boards_migrate(alias: &str) -> anyhow::Result<i32> {
    let env = crate::setup::Env::from_process()?;
    let ok = crate::setup::migrate_review(&env, &mut crate::setup::System, &mut std::io::stdout(), alias, true)?;
    Ok(i32::from(!ok))
}

/// `conde setup`: the service runs the single daemon (the local board, plus every enrolled
/// shared board); the default board and the migration review follow.
fn setup(
    claude: bool,
    codex: bool,
    service: bool,
    all: bool,
    keep_receiver_policy: bool,
    check: bool,
    default_board: Option<String>,
) -> anyhow::Result<i32> {
    use crate::setup::{Env, Options, System, run, verify};
    let env = Env::from_process()?;
    let mut opts = Options::select(claude, codex, service, all);
    opts.keep_receiver_policy = keep_receiver_policy;
    if default_board.is_some() {
        opts.boards = true;
    }
    opts.default_board = default_board;
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
