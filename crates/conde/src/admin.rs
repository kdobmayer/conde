//! Commands that do not go through the daemon: `conde server`, `conde enroll`, `conde admin key`
//! (run on the board directly) and `conde admin slack` and `conde admin user` (through the
//! server's admin API), by the human only.

use std::io::Read;
use std::net::SocketAddr;
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
        return Dispatcher::connect(&url, how)
            .context("connecting to the Postgres board (DATABASE_URL)");
    }
    let dir = data.unwrap_or_else(paths::data_dir);
    crate::private::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let db = dir.join("conde.db");
    Dispatcher::open(&db).with_context(|| format!("opening {}", db.display()))
}

/// `conde server`: listen on `listen`, default `0.0.0.0:$PORT` (8080).
pub fn server(listen: Option<&str>, data: Option<PathBuf>) -> anyhow::Result<()> {
    let listen = match listen {
        Some(l) => l.to_string(),
        None => format!(
            "0.0.0.0:{}",
            std::env::var("PORT")
                .ok()
                .filter(|p| !p.is_empty())
                .unwrap_or("8080".into())
        ),
    };
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("bad listen address '{listen}'"))?;
    let board = Arc::new(open_board(data, PgOpen::SERVER)?);
    cloud::run(board, addr)
}

/// A new host key: 32 random bytes, hex, with a recognizable prefix.
fn new_key() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!(
        "conde_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

pub fn key_create(
    data: Option<PathBuf>,
    user: &str,
    host: &str,
    label: &str,
) -> anyhow::Result<i32> {
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

/// The admin key for `server`: `CONDE_ADMIN_KEY` (or the file `CONDE_ADMIN_KEY_FILE` names), else
/// a prompt at a terminal.
fn admin_key(server: &str, no_tty: &'static str) -> anyhow::Result<String> {
    match conde_remote::secret_env("CONDE_ADMIN_KEY")?.filter(|k| !k.trim().is_empty()) {
        Some(k) => Ok(k.trim().to_string()),
        None => credentials::prompt_secret(&format!("admin key for {server}: "), no_tty),
    }
}

/// One admin API request (`path` under the server's origin) with the admin key: the answer's
/// JSON, or its message as the error. The key never travels over plain http off this machine.
fn admin_request(
    server: &str,
    key: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> anyhow::Result<serde_json::Value> {
    admin_request_within(server, key, path, body, std::time::Duration::from_secs(30))
}

/// [`admin_request`] waiting up to `timeout` for the answer.
fn admin_request_within(
    server: &str,
    key: &str,
    path: &str,
    body: Option<serde_json::Value>,
    timeout: std::time::Duration,
) -> anyhow::Result<serde_json::Value> {
    let base = credentials::rpc_url(server)?;
    let parsed = reqwest::Url::parse(&base)?;
    let host = parsed
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if parsed.scheme() == "http" && !loopback {
        bail!("refusing to send the admin key over plain http to {server}; use https://");
    }
    let url = format!("{}{path}", base.trim_end_matches("/v1/rpc"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (status, answer) = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(crate::login::tls()?)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()?;
        let request = match body {
            Some(b) => client.post(&url).json(&b),
            None => client.get(&url),
        };
        let r = request.bearer_auth(key).send().await?;
        let status = r.status();
        anyhow::Ok((
            status,
            r.json::<serde_json::Value>().await.unwrap_or_default(),
        ))
    })?;
    if !status.is_success() {
        bail!(
            "{} ({status})",
            answer["message"].as_str().unwrap_or("the server refused")
        );
    }
    Ok(answer)
}

/// `conde admin slack <owner> <member-id> --server <url>`: the admin API sets the owner's Slack
/// member id (`slack_source = admin`) with the admin key from `CONDE_ADMIN_KEY` or a prompt.
pub fn slack(server: &str, owner: &str, member_id: &str) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin slack needs the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    println!("{}", set_slack(server, &key, owner, member_id)?);
    Ok(0)
}

/// `POST /v1/admin/users/<owner>/slack {member_id}` with the admin key; the line to print.
pub fn set_slack(server: &str, key: &str, owner: &str, member_id: &str) -> anyhow::Result<String> {
    let path = format!("/v1/admin/users/{}/slack", owner.trim().to_lowercase());
    let answer = admin_request(
        server,
        key,
        &path,
        Some(serde_json::json!({"member_id": member_id})),
    )?;
    let user = answer["user"].as_str().unwrap_or(owner);
    Ok(format!(
        "slack member id of {user} on {server}: {member_id} (set by the admin)"
    ))
}

/// `conde admin user invite <name> <email> [--by <name>] --server <url>`: creates the user the
/// first sign-in with this verified email lands on; the server sends the invite email.
pub fn user_invite(server: &str, name: &str, email: &str, by: Option<&str>) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin user invite needs the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    println!("{}", invite(server, &key, name, email, by)?);
    Ok(0)
}

/// `POST /v1/admin/users {name, email, inviter?}` with the admin key; the line to print. A mail
/// failure after the user was created is the error, with the server's message.
pub fn invite(
    server: &str,
    key: &str,
    name: &str,
    email: &str,
    by: Option<&str>,
) -> anyhow::Result<String> {
    let mut body = serde_json::json!({"name": name, "email": email});
    if let Some(by) = by {
        body["inviter"] = by.into();
    }
    // the server waits up to 45 s for the mail service or the sign-in server
    let answer = admin_request_within(
        server,
        key,
        "/v1/admin/users",
        Some(body),
        std::time::Duration::from_secs(90),
    )?;
    let user = answer["user"].as_str().unwrap_or(name);
    let email = answer["email"].as_str().unwrap_or(email);
    let status = answer["status"].as_str().unwrap_or("invited");
    let mut line = match answer["mail"].as_str() {
        Some("sent") => format!("invited {user} {email}, email sent"),
        Some("keycloak") => {
            format!("invited {user} {email}, account-setup email sent by the sign-in server")
        }
        Some("unconfigured") => format!(
            "invited {user} {email}, no mail configured: tell them the steps at https://conde.cloud/docs/setup"
        ),
        Some("none") if answer["reason"] == "disabled" => {
            format!("{user} {email} is disabled: no email sent (enable the user first)")
        }
        Some("none") if answer["reason"] == "set up" => {
            format!("{user} {email} has set up the sign-in account already: no email sent")
        }
        Some("none") => format!("{user} {email} has signed in already: no email sent"),
        _ => format!("{user} <{email}> on {server}: {status}"),
    };
    if by.is_some() && answer["mail"] == "keycloak" {
        line.push_str("\ninviter not shown: the sign-in server sends this email");
    }
    Ok(line)
}

/// `conde admin user list --server <url>`.
pub fn user_list(server: &str) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin user list needs the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    let lines = list_users(server, &key)?;
    if lines.is_empty() {
        eprintln!("(no users)");
    }
    for line in lines {
        println!("{line}");
    }
    Ok(0)
}

/// `GET /v1/admin/users` with the admin key: one line per user, `name  email  issuers  state`,
/// then `admin` for an admin.
pub fn list_users(server: &str, key: &str) -> anyhow::Result<Vec<String>> {
    let answer = admin_request(server, key, "/v1/admin/users", None)?;
    let users = answer["users"].as_array().cloned().unwrap_or_default();
    Ok(users
        .iter()
        .map(|u| {
            let issuers: Vec<&str> = u["issuers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| i.as_str())
                .collect();
            format!(
                "{}\t{}\t{}\t{}{}",
                u["name"].as_str().unwrap_or(""),
                u["email"].as_str().unwrap_or("-"),
                if issuers.is_empty() {
                    "not signed in yet".to_string()
                } else {
                    issuers.join(",")
                },
                if u["disabled"].as_bool() == Some(true) {
                    "disabled"
                } else {
                    "enabled"
                },
                if u["admin"].as_bool() == Some(true) {
                    "\tadmin"
                } else {
                    ""
                },
            )
        })
        .collect())
}

/// `conde admin user promote|demote <name> --server <url>`.
pub fn user_role(server: &str, name: &str, admin: bool) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin user promote and demote need the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    println!("{}", set_role(server, &key, name, admin)?);
    Ok(0)
}

/// `POST /v1/admin/users/<name>/promote` (or `/demote`) with the admin key; the line to print.
pub fn set_role(server: &str, key: &str, name: &str, admin: bool) -> anyhow::Result<String> {
    let name = user_name(name)?;
    let verb = if admin { "promote" } else { "demote" };
    let path = format!("/v1/admin/users/{name}/{verb}");
    let answer = admin_request(server, key, &path, Some(serde_json::json!({})))?;
    let user = answer["user"].as_str().unwrap_or(&name);
    Ok(if admin {
        format!("{user} is an admin of {server}")
    } else {
        format!("{user} is no longer an admin of {server}")
    })
}

/// `conde admin welcome set --agent <owner/name> [--lobby] [--guide] [--text] --server <url>`.
pub fn welcome_set(server: &str, welcome: &conde_remote::web::Welcome) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin welcome set needs the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    print!("{}", set_welcome(server, &key, welcome)?);
    Ok(0)
}

/// `POST /v1/admin/welcome` with the admin key; the text to print.
pub fn set_welcome(
    server: &str,
    key: &str,
    welcome: &conde_remote::web::Welcome,
) -> anyhow::Result<String> {
    let body = serde_json::to_value(welcome)?;
    let answer = admin_request(server, key, "/v1/admin/welcome", Some(body))?;
    welcome_text(server, &answer)
}

/// `conde admin welcome show --server <url>`.
pub fn welcome_show(server: &str) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin welcome show needs the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    let answer = admin_request(server, &key, "/v1/admin/welcome", None)?;
    print!("{}", welcome_text(server, &answer)?);
    Ok(0)
}

/// The welcome an admin API answer carries, as `welcome set` and `welcome show` print it.
pub fn welcome_text(server: &str, answer: &serde_json::Value) -> anyhow::Result<String> {
    let w: conde_remote::web::Welcome =
        serde_json::from_value(answer["welcome"].clone()).context("the server's welcome answer")?;
    let none = "(none)";
    Ok(format!(
        "welcome of {server}:\n  admin agent: {}\n  lobby: {}\n  guide: {}\n  text: {}\n",
        w.admin_agent.as_deref().unwrap_or(none),
        w.lobby.as_deref().unwrap_or(none),
        w.guide.as_deref().unwrap_or(none),
        w.text.as_deref().unwrap_or(none),
    ))
}

/// A user name as the admin API takes it: `[a-z0-9_-]`.
fn user_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim().to_lowercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("a user name is [a-z0-9_-]");
    }
    Ok(name)
}

/// `conde admin user realm-clear <name> --server <url>`.
pub fn user_realm_clear(server: &str, name: &str) -> anyhow::Result<i32> {
    let key = admin_key(
        server,
        "admin user realm-clear needs the admin key: set CONDE_ADMIN_KEY or run it at a terminal",
    )?;
    println!("{}", realm_clear(server, &key, name)?);
    Ok(0)
}

/// `POST /v1/admin/users/<name>/realm-account/clear` with the admin key; the line to print.
pub fn realm_clear(server: &str, key: &str, name: &str) -> anyhow::Result<String> {
    let name = user_name(name)?;
    let path = format!("/v1/admin/users/{name}/realm-account/clear");
    let answer = admin_request(server, key, &path, Some(serde_json::json!({})))?;
    let user = answer["user"].as_str().unwrap_or(&name);
    Ok(if answer["cleared"] == true {
        format!(
            "cleared the recorded sign-in account of {user} on {server}: the next invite creates a new one"
        )
    } else {
        format!("{user} on {server} has no recorded sign-in account: nothing to clear")
    })
}

pub fn key_revoke(
    data: Option<PathBuf>,
    host: Option<&str>,
    id: Option<&str>,
) -> anyhow::Result<i32> {
    let board = open_board(data, PgOpen::COMMAND)?;
    let now = iso_from_ms(now_ms());
    let revoked = board.with_tx(|c| hosts::revoke(c, host, id, &now))?;
    if revoked.is_empty() {
        bail!("no live key matches");
    }
    for k in revoked {
        println!(
            "revoked {} ({}@{}, {})",
            k.key_id,
            k.user,
            k.host,
            k.label.unwrap_or_default()
        );
    }
    eprintln!("a bridge connected with a revoked key keeps its connection until it reconnects");
    Ok(0)
}

pub fn key_list(data: Option<PathBuf>) -> anyhow::Result<i32> {
    let board = open_board(data, PgOpen::COMMAND)?;
    let keys = board.with_conn(hosts::list)?;
    for k in &keys {
        let state = k
            .revoked_at
            .as_deref()
            .map(|r| format!("revoked {r}"))
            .unwrap_or_else(|| "live".into());
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

/// `conde board move <alias> <url>`: re-points an enrolled board at its new address (see
/// [`boards::move_board`]) and has the running bridge reconnect there.
pub fn board_move(alias: &str, url: &str, trust_new: bool) -> anyhow::Result<i32> {
    let config = boards::config_dir()?;
    let (old, new) = boards::move_board(
        &config,
        alias,
        url,
        trust_new,
        &mut boards::moved_probe,
        &mut |server, key| boards::board_info(server, key),
    )?;
    println!(
        "board {alias} moved: {} -> {} (credential, account {} and host {} kept)",
        old.server, new.server, new.owner, new.host_id
    );
    println!("{}", reload_bridge(&paths::socket_path(), alias));
    Ok(0)
}

/// `conde enroll`: store the server URL, host id, account and key (0600) as the board
/// `alias` (default: the name the board advertises), refusing to retarget a pinned alias.
pub fn enroll(
    server: &str,
    host: Option<&str>,
    board: Option<&str>,
    key_stdin: bool,
) -> anyhow::Result<i32> {
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
    let alias = board
        .map(str::to_string)
        .unwrap_or_else(|| info.name.clone());
    boards::check_shared_alias(&alias)?;
    let path = boards::path(&alias)?;
    let cfg = BoardConfig {
        server: server.to_string(),
        host_id: info.host.clone(),
        owner: info.owner.clone(),
        key,
    };
    boards::store(&path, &alias, &cfg)?;
    let bridge = reload_bridge(&paths::socket_path(), &alias);
    print!(
        "{}",
        enrolled_text(&alias, &cfg, &path, false, None, &bridge)
    );
    print!("{}", crate::welcome::lines(&alias, info.welcome.as_ref()));
    Ok(0)
}

/// Tells the running bridge to re-read board `alias`'s file, so a new key takes effect
/// without a restart; the line that says how it went.
pub fn reload_bridge(socket: &std::path::Path, alias: &str) -> String {
    let Ok(mut client) = crate::client::Client::connect(socket) else {
        return "bridge: not running; it reads the new key at start".into();
    };
    let mut req = crate::protocol::Request::new(
        crate::util::new_ulid(),
        crate::surface::BOARD_RELOAD,
        serde_json::json!({}),
    );
    req.board = Some(alias.to_string());
    let why = match client.call(&req) {
        Ok(r) if r.ok => {
            return match r.result.as_ref().and_then(|v| v["state"].as_str()) {
                Some("unchanged") => format!("bridge: {alias} uses the stored key"),
                Some("opened") => format!("bridge: {alias} loaded"),
                _ => format!("bridge: {alias} reloaded"),
            };
        }
        Ok(r) => r
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "no answer".into()),
        Err(e) => format!("{e:#}"),
    };
    format!("bridge: {alias} not reloaded ({why}); restart the bridge to use the new key")
}

/// What `conde enroll` and `conde login` print once the board file is stored; `reenrolled`:
/// the server said the host existed; `replaced`: whether that mint revoked a live key of it
/// (None: the server did not say); `bridge`: the [`reload_bridge`] line.
pub fn enrolled_text(
    alias: &str,
    cfg: &BoardConfig,
    path: &std::path::Path,
    reenrolled: bool,
    replaced: Option<bool>,
    bridge: &str,
) -> String {
    let (host, server, owner, path) = (&cfg.host_id, &cfg.server, &cfg.owner, path.display());
    let head = if reenrolled {
        let what = match replaced {
            Some(true) => ", previous key revoked",
            Some(false) => ", no live key before",
            None => "",
        };
        format!(
            "re-enrolled host {host}{what}\n\
             board {alias} at {server} (account {owner}); credential stored at {path} (mode 0600)\n"
        )
    } else {
        format!(
            "enrolled host {host} with {server} as board {alias} (account {owner}); credential stored at {path} (mode 0600)\n"
        )
    };
    format!(
        "{head}{}: conde publish <name> --board {alias}\n\
         {bridge}\n\
         next: conde setup (the default board, and the review of this host's earlier names on {alias})\n",
        crate::setup::ENROLL_SCOPE
    )
}
