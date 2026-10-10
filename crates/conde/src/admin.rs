//! Commands that do not go through the daemon: `conde server`, `conde enroll`
//! and `conde admin key` (run on the board directly, by the human only).

use std::io::Read;
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};

use crate::boards::{self, BoardConfig};
use crate::credentials;
use crate::dispatch::Dispatcher;
use crate::identity::{Markers, derive_address, harness_ancestor};
use crate::ops::hosts;
use crate::store::postgres::PgOpen;
use crate::util::{iso_from_ms, local_host, now_ms};
use crate::{cloud, paths};

/// Admin commands act on credentials: never from an agent's shell.
pub fn require_human() -> anyhow::Result<()> {
    match derive_address(&Markers::from_env(), harness_ancestor) {
        Ok(a) if a.starts_with("human:") => Ok(()),
        Ok(a) => bail!("admin commands are for the human only; this shell is {a}"),
        Err(e) => bail!("admin commands are for the human only: {e}"),
    }
}

/// The board: Postgres when DATABASE_URL is set (cloud mode; TLS verified
/// against the system CA bundle), else the SQLite file in the data directory.
fn open_board(data: Option<PathBuf>, how: PgOpen) -> anyhow::Result<Dispatcher> {
    if let Some(url) = std::env::var("DATABASE_URL").ok().filter(|u| !u.is_empty()) {
        return Dispatcher::connect(&url, how).context("connecting to the Postgres board (DATABASE_URL)");
    }
    let dir = data.unwrap_or_else(paths::data_dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let db = dir.join("conde.db");
    Dispatcher::open(&db).with_context(|| format!("opening {}", db.display()))
}

/// `conde server`: listen on `listen`, default `0.0.0.0:$PORT` (8080).
pub fn server(listen: Option<&str>, data: Option<PathBuf>) -> anyhow::Result<()> {
    let listen = match listen {
        Some(l) => l.to_string(),
        None => format!("0.0.0.0:{}", std::env::var("PORT").ok().filter(|p| !p.is_empty()).unwrap_or("8080".into())),
    };
    let addr: SocketAddr = listen.parse().with_context(|| format!("bad listen address '{listen}'"))?;
    let board = Arc::new(open_board(data, PgOpen::SERVER)?);
    cloud::run(board, addr)
}

/// A new host key: 32 random bytes, hex, with a recognizable prefix.
fn new_key() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!("conde_{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

pub fn key_create(data: Option<PathBuf>, user: &str, host: &str, label: &str) -> anyhow::Result<i32> {
    let board = open_board(data, PgOpen::COMMAND)?;
    let key = new_key()?;
    let digest = conde_remote::token_digest(&key);
    let now = iso_from_ms(now_ms());
    board.with_tx(|c| hosts::insert_key(c, user, host, &digest, label, &now))?;
    eprintln!(
        "key {} for {}@{} ({label}). It is shown once; hand it over privately for `conde enroll`:",
        hosts::key_id(&digest),
        user.to_lowercase(),
        host.to_lowercase()
    );
    println!("{key}");
    Ok(0)
}

pub fn key_revoke(data: Option<PathBuf>, host: Option<&str>, id: Option<&str>) -> anyhow::Result<i32> {
    let board = open_board(data, PgOpen::COMMAND)?;
    let now = iso_from_ms(now_ms());
    let revoked = board.with_tx(|c| hosts::revoke(c, host, id, &now))?;
    if revoked.is_empty() {
        bail!("no live key matches");
    }
    for k in revoked {
        println!("revoked {} ({}@{}, {})", k.key_id, k.user, k.host, k.label.unwrap_or_default());
    }
    eprintln!("a bridge connected with a revoked key keeps its connection until it reconnects");
    Ok(0)
}

pub fn key_list(data: Option<PathBuf>) -> anyhow::Result<i32> {
    let board = open_board(data, PgOpen::COMMAND)?;
    let keys = board.with_conn(hosts::list)?;
    for k in &keys {
        let state = k.revoked_at.as_deref().map(|r| format!("revoked {r}")).unwrap_or_else(|| "live".into());
        println!(
            "{}\t{}@{}\t{}\t{}\t{state}",
            k.key_id,
            k.user,
            k.host,
            k.label.as_deref().unwrap_or(""),
            k.created_at.as_deref().unwrap_or("")
        );
    }
    if keys.is_empty() {
        eprintln!("(no keys)");
    }
    Ok(0)
}

/// `conde enroll`: store the server URL, host id, account and key (0600) as the board
/// `alias` (default: the name the board advertises), refusing to retarget a pinned alias.
pub fn enroll(server: &str, host: Option<&str>, board: Option<&str>, key_stdin: bool) -> anyhow::Result<i32> {
    credentials::bridge_url(server)?;
    if let Some(alias) = board {
        boards::check_shared_alias(alias)?;
        boards::check_pin(&boards::path(alias)?, alias, server, None)?;
    }
    let host = host.map(str::to_lowercase).unwrap_or_else(local_host);
    let key = if key_stdin {
        credentials::read_stdin_secret()?
    } else {
        credentials::prompt_secret(
            &format!("host key for {host} at {server}: "),
            "enroll needs a terminal for the key prompt (or pipe the key with --key-stdin)",
        )?
    };
    if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("that is not a host key (empty or contains whitespace)");
    }
    let info = boards::board_info(server, &key)?;
    let alias = board.map(str::to_string).unwrap_or_else(|| info.name.clone());
    boards::check_shared_alias(&alias)?;
    let path = boards::path(&alias)?;
    let cfg = BoardConfig { server: server.to_string(), host_id: info.host.clone(), owner: info.owner.clone(), key };
    boards::store(&path, &alias, &cfg)?;
    print!("{}", enrolled_text(&alias, &cfg, &path));
    Ok(0)
}

/// What `conde enroll` and `conde login` print once the board file is stored.
pub fn enrolled_text(alias: &str, cfg: &BoardConfig, path: &std::path::Path) -> String {
    format!(
        "enrolled host {} with {} as board {alias} (account {}); credential stored at {} (mode 0600)\n\
         {}: conde publish <name> --board {alias}\n\
         next: conde setup (the default board, and the review of this host's earlier names on {alias})\n",
        cfg.host_id,
        cfg.server,
        cfg.owner,
        path.display(),
        crate::setup::ENROLL_SCOPE
    )
}
