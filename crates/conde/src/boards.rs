//! Shared-board configuration on this host (DESIGN-v2 §4, §7, decision 9): one file per
//! enrolled board, `$XDG_CONFIG_HOME/conde/boards/<alias>.json` (mode 0600 in a 0700
//! directory), each alias pinned to one server and one account; and the host's default
//! board, `$XDG_CONFIG_HOME/conde/default-board`, set by `conde setup` (whoever runs it, the agent
//! included, on the human's instruction).

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::credentials;
use crate::protocol::v2::{BoardInfo, LOCAL_BOARD, op};

/// The longest alias.
pub const MAX_ALIAS: usize = 32;

/// One enrolled shared board: the v1 credential plus the account it is pinned to.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardConfig {
    /// The server URL given at enrollment (https://, or http:// to a loopback server in tests).
    pub server: String,
    pub host_id: String,
    /// The account on that board (`board.info` owner).
    pub owner: String,
    pub key: String,
}

impl std::fmt::Debug for BoardConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoardConfig")
            .field("server", &self.server)
            .field("host_id", &self.host_id)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// `$XDG_CONFIG_HOME/conde`, else `~/.config/conde`.
pub fn config_dir() -> anyhow::Result<PathBuf> {
    Ok(credentials::path()?.parent().context("credential path has no directory")?.to_path_buf())
}

/// The directory of the board files.
pub fn dir() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("boards"))
}

/// `boards/<alias>.json`.
pub fn path(alias: &str) -> anyhow::Result<PathBuf> {
    Ok(dir()?.join(format!("{alias}.json")))
}

/// The syntax of an alias: a lowercase letter, then lowercase letters, digits, `-` or `_`.
pub fn is_alias(alias: &str) -> bool {
    let b = alias.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_ALIAS
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// An alias a shared board may be enrolled under (`local` is the local board's).
pub fn check_shared_alias(alias: &str) -> anyhow::Result<()> {
    if alias == LOCAL_BOARD {
        bail!("'local' names this machine's own board; enroll a shared board under another alias (e.g. --board dakai)");
    }
    if !is_alias(alias) {
        bail!("'{alias}' is not a board alias (lowercase letters, digits, '-' or '_', starting with a letter, at most {MAX_ALIAS})");
    }
    Ok(())
}

/// The enrolled aliases, sorted.
pub fn enrolled() -> anyhow::Result<Vec<String>> {
    enrolled_in(&dir()?)
}

pub fn enrolled_in(dir: &Path) -> anyhow::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(alias) = name.strip_suffix(".json").filter(|a| is_alias(a) && *a != LOCAL_BOARD) {
            out.push(alias.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Read a board file; refuse one other users could read.
pub fn load(path: &Path) -> anyhow::Result<BoardConfig> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if let Some(m) = credentials::mode(path)
        && m & 0o077 != 0
    {
        bail!("{} has mode {m:03o}; it holds a secret, make it 0600 (chmod 600 {})", path.display(), path.display());
    }
    serde_json::from_slice(&raw).with_context(|| format!("{} is not a conde board file", path.display()))
}

/// The refusal to retarget a pinned alias, or None when `new` keeps its server and account.
pub fn retarget_refusal(alias: &str, old: &BoardConfig, new_server: &str, new_owner: Option<&str>) -> Option<String> {
    let same_server = credentials::same_server(&old.server, new_server).unwrap_or(false);
    let same_owner = new_owner.is_none_or(|o| o.eq_ignore_ascii_case(&old.owner));
    (!same_server || !same_owner).then(|| {
        format!(
            "{alias} is pinned to {} as {}; to point the alias elsewhere, first run: conde boards --forget {alias}",
            old.server, old.owner
        )
    })
}

/// Before enrolling under `alias`: refuse when the alias is pinned to another server
/// (`owner` None: the account is not known yet).
pub fn check_pin(path: &Path, alias: &str, server: &str, owner: Option<&str>) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let old = load(path)?;
    match retarget_refusal(alias, &old, server, owner) {
        Some(why) => bail!(why),
        None => Ok(()),
    }
}

/// Write a board file atomically, mode 0600 in a 0700 directory, refusing to retarget a
/// pinned alias.
pub fn store(path: &Path, alias: &str, cfg: &BoardConfig) -> anyhow::Result<()> {
    check_pin(path, alias, &cfg.server, Some(&cfg.owner))?;
    let dir = path.parent().context("board path has no directory")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!(".{alias}.json.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(cfg)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `$XDG_CONFIG_HOME/conde/default-board`: one line, the alias.
pub fn default_path() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("default-board"))
}

/// The host's default board for names and channels; `local` when unset.
pub fn read_default(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => {
            let alias = t.trim();
            if alias.is_empty() {
                return Ok(LOCAL_BOARD.into());
            }
            if !is_alias(alias) {
                bail!("{} holds '{alias}', which is not a board alias", path.display());
            }
            Ok(alias.to_string())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LOCAL_BOARD.into()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Set the default board; `local` or an enrolled alias (`boards_dir` holds its file).
pub fn write_default(path: &Path, boards_dir: &Path, alias: &str) -> anyhow::Result<()> {
    if alias != LOCAL_BOARD && !enrolled_in(boards_dir)?.iter().any(|a| a == alias) {
        bail!("no board '{alias}' is enrolled on this host; the default is local or one of: {}", list_or_none(boards_dir)?);
    }
    let dir = path.parent().context("default-board path has no directory")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::write(path, format!("{alias}\n")).with_context(|| format!("writing {}", path.display()))
}

fn list_or_none(boards_dir: &Path) -> anyhow::Result<String> {
    let all = enrolled_in(boards_dir)?;
    Ok(if all.is_empty() { "(none enrolled)".into() } else { all.join(", ") })
}

/// `board.info` on a server with a host key, over `POST /v1/rpc` (https; plain http only to
/// a loopback server). Tells the board's advertised name, the account and the host.
pub fn board_info(server: &str, key: &str) -> anyhow::Result<BoardInfo> {
    let url = format!("{}/v1/rpc", crate::login::origin(server)?);
    let request = json!({"v": 1, "type": "request", "request_id": crate::util::new_ulid(), "op": op::BOARD_INFO, "body": {}});
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let answer: Value = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(crate::login::tls()?)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let response = client
            .post(&url)
            .bearer_auth(key)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("asking {server} for board.info"))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        if !status.is_success() && body["error"].is_null() {
            bail!("{server} refused board.info: HTTP {status}");
        }
        anyhow::Ok(body)
    })?;
    if answer["ok"] != true {
        let message = answer["error"]["message"].as_str().unwrap_or("no reason given");
        bail!("{server} refused board.info: {message}");
    }
    serde_json::from_value(answer["result"].clone()).map_err(|e| anyhow!("{server} answered board.info without its fields: {e}"))
}

// --- the same files under an explicit config directory (the daemon, in-process tests) ----

/// `<config>/boards/<alias>.json`.
pub fn path_in(config: &Path, alias: &str) -> PathBuf {
    config.join("boards").join(format!("{alias}.json"))
}

/// The v1 single credential, `<config>/credentials`.
pub fn legacy_path(config: &Path) -> PathBuf {
    config.join("credentials")
}

/// Every board file under `config`, by alias; a file that cannot be read keeps its error.
pub fn load_all(config: &Path) -> Vec<(String, anyhow::Result<BoardConfig>)> {
    enrolled_in(&config.join("boards"))
        .unwrap_or_default()
        .into_iter()
        .map(|alias| {
            let cfg = load(&path_in(config, &alias));
            (alias, cfg)
        })
        .collect()
}

/// The default board under `config`; `local` when unset or unreadable.
pub fn default_in(config: &Path) -> String {
    read_default(&config.join("default-board")).unwrap_or_else(|_| LOCAL_BOARD.into())
}

/// Deletes `<config>/boards/<alias>.json`; whether it existed. Never touches the server.
pub fn forget_in(config: &Path, alias: &str) -> anyhow::Result<bool> {
    let path = path_in(config, alias);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Moves the v1 credential to `<config>/boards/<alias>.json`, pinned to `owner` (the daemon's
/// fallback when `conde setup` has not moved it); the v1 file goes once the board file is written.
pub fn adopt_legacy(config: &Path, alias: &str, owner: &str) -> anyhow::Result<PathBuf> {
    check_shared_alias(alias)?;
    let legacy = legacy_path(config);
    let creds = credentials::load(&legacy)?;
    let path = path_in(config, alias);
    let cfg = BoardConfig { server: creds.server, host_id: creds.host_id, owner: owner.to_string(), key: creds.key };
    store(&path, alias, &cfg)?;
    std::fs::remove_file(&legacy).with_context(|| format!("removing {}", legacy.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn cfg(server: &str, owner: &str) -> BoardConfig {
        BoardConfig { server: server.into(), host_id: "h".into(), owner: owner.into(), key: "k".into() }
    }

    #[test]
    fn aliases() {
        for ok in ["dakai", "team-2", "a_b"] {
            assert!(is_alias(ok), "{ok}");
        }
        for bad in ["", "Dakai", "2team", "da kai", "dakai:", "x".repeat(33).as_str()] {
            assert!(!is_alias(bad), "{bad}");
        }
        assert!(check_shared_alias("local").unwrap_err().to_string().contains("this machine's own board"));
        assert!(check_shared_alias("dakai").is_ok());
    }

    #[test]
    fn an_alias_stays_pinned_to_its_server_and_account() {
        let root = std::env::temp_dir().join(format!("conde-boards-{}", crate::util::new_ulid()));
        let dir = root.join("conde/boards");
        let p = dir.join("dakai.json");
        store(&p, "dakai", &cfg("https://conde.example", "krisz")).unwrap();
        assert_eq!(credentials::mode(&p), Some(0o600));
        assert_eq!(credentials::mode(&dir), Some(0o700));
        assert_eq!(enrolled_in(&dir).unwrap(), ["dakai"]);
        // same server and account: a new key replaces the old one
        let mut again = cfg("wss://Conde.example/v1/bridge", "KRISZ");
        again.key = "k2".into();
        store(&p, "dakai", &again).unwrap();
        assert_eq!(load(&p).unwrap().key, "k2");
        let err = store(&p, "dakai", &cfg("https://evil.example", "krisz")).unwrap_err().to_string();
        assert_eq!(err, "dakai is pinned to wss://Conde.example/v1/bridge as KRISZ; to point the alias elsewhere, first run: conde boards --forget dakai");
        assert!(store(&p, "dakai", &cfg("https://conde.example", "other")).is_err());
        assert!(check_pin(&p, "dakai", "https://conde.example", None).is_ok());
        assert!(!format!("{:?}", load(&p).unwrap()).contains("k2"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&p).unwrap_err().to_string().contains("mode 644"));

        let def = root.join("conde/default-board");
        assert_eq!(read_default(&def).unwrap(), "local");
        write_default(&def, &dir, "dakai").unwrap();
        assert_eq!(read_default(&def).unwrap(), "dakai");
        let err = write_default(&def, &dir, "other").unwrap_err().to_string();
        assert!(err.contains("no board 'other' is enrolled") && err.contains("dakai"), "{err}");
        write_default(&def, &dir, "local").unwrap();
        assert_eq!(read_default(&def).unwrap(), "local");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
