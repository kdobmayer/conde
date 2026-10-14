//! `conde login --server <url>`: enrolls this host through a browser sign-in. A loopback
//! listener on 127.0.0.1 receives a one-time code bound to this run (state + PKCE S256);
//! the code is redeemed over TLS for the host key, which never appears in a URL. The key is
//! stored exactly as `conde enroll` stores it (`boards/<alias>.json`, mode 0600).
//!
//! A headless host (`--code`, `--no-browser`, or no display) uses the pasted code instead: the
//! human opens the printed URL on any device, signs in, and the page shows a short one-time
//! code, which is typed at the masked prompt or given back with `--code <CODE>`. The run's
//! state and PKCE verifier wait in `login/<server>.json` (mode 0600) between the two steps.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, timeout, timeout_at};

use conde_remote::web::CODE_ALPHABET;

use crate::boards::{self, BoardConfig};

/// How long the browser may take to sign in (MFA included).
pub const BROWSER_WAIT: Duration = Duration::from_secs(300);
/// The redeem request's deadline.
pub const REDEEM_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a started pasted-code login waits on this host for its code.
pub const PENDING_FOR: Duration = Duration::from_secs(900);

/// What the server returns once for a redeemed code.
#[derive(Clone, Deserialize)]
pub struct Enrolled {
    pub host_id: String,
    pub key: String,
    pub user: String,
    /// The host existed on the server, which revoked its previous key (absent from an older
    /// server: false).
    #[serde(default)]
    pub reenrolled: bool,
}

impl std::fmt::Debug for Enrolled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enrolled")
            .field("host_id", &self.host_id)
            .field("user", &self.user)
            .field("reenrolled", &self.reenrolled)
            .finish_non_exhaustive()
    }
}

fn is_loopback(host: &str) -> bool {
    let name = host.rsplit_once(':').filter(|(h, p)| !h.is_empty() && p.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

/// The server's web origin: `https://host[:port]`. Plain http only to a loopback server.
pub fn origin(server: &str) -> anyhow::Result<String> {
    let (scheme, rest) = server.split_once("://").with_context(|| format!("'{server}' is not a URL (https://host)"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    if authority.is_empty() || authority.contains('@') {
        bail!("'{server}' has no usable host");
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" | "wss" => Ok(format!("https://{authority}")),
        "http" | "ws" if is_loopback(&authority) => Ok(format!("http://{authority}")),
        "http" | "ws" => bail!("'{server}' is not TLS; the key is only fetched over https"),
        other => bail!("unsupported scheme '{other}' in '{server}' (https://)"),
    }
}

/// One `conde login` run: its state and PKCE pair.
struct Run {
    state: String,
    verifier: String,
    challenge: String,
}

impl Run {
    fn new() -> anyhow::Result<Run> {
        let mut bytes = [0u8; 64];
        getrandom(&mut bytes)?;
        let state: String = bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
        let verifier = URL_SAFE_NO_PAD.encode(&bytes[32..]);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Ok(Run { state, verifier, challenge })
    }
}

fn getrandom(buf: &mut [u8]) -> anyhow::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf).context("reading /dev/urandom")
}

/// Signs in through the browser and redeems the code. `open` is handed the sign-in URL.
pub async fn sign_in(server: &str, host: &str, open: impl FnOnce(&str), wait: Duration) -> anyhow::Result<Enrolled> {
    let origin = origin(server)?;
    let run = Run::new()?;
    let listener = TcpListener::bind("127.0.0.1:0").await.context("opening the loopback listener")?;
    let port = listener.local_addr()?.port();
    let url = format!("{origin}/auth/cli?port={port}&state={}&challenge={}", run.state, run.challenge);
    open(&url);
    let code = wait_for_code(&listener, &run.state, Instant::now() + wait).await?;
    drop(listener);
    redeem(&origin, &run, &code, host).await
}

async fn wait_for_code(listener: &TcpListener, state: &str, deadline: Instant) -> anyhow::Result<String> {
    loop {
        let (mut sock, _) = timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| anyhow::anyhow!("the browser did not finish the sign-in in time; run conde login again"))??;
        let Ok(Ok(target)) = timeout(Duration::from_secs(5), request_target(&mut sock)).await else {
            continue;
        };
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path != "/done" {
            respond(&mut sock, "404 Not Found", "Not found.").await;
            continue;
        }
        let param = |name: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v);
        if param("state") != Some(state) {
            respond(&mut sock, "400 Bad Request", "This sign-in belongs to another conde login run.").await;
            continue;
        }
        match param("code").filter(|c| c.len() == 64 && c.chars().all(|ch| ch.is_ascii_hexdigit())) {
            Some(code) => {
                respond(&mut sock, "200 OK", "Signed in. You can close this tab and return to the terminal.").await;
                return Ok(code.to_string());
            }
            None => respond(&mut sock, "400 Bad Request", "The sign-in response has no code.").await,
        }
    }
}

/// The target of the request line (`GET /done?.. HTTP/1.1`).
async fn request_target(sock: &mut TcpStream) -> anyhow::Result<String> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut buf).await?;
        if n == 0 || head.len() > 16 * 1024 {
            bail!("incomplete request");
        }
        head.extend_from_slice(&buf[..n]);
    }
    let line = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_string();
    match line.split(' ').collect::<Vec<_>>()[..] {
        ["GET", target, version] if version.starts_with("HTTP/1.") => Ok(target.to_string()),
        _ => bail!("not a GET request"),
    }
}

async fn respond(sock: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>conde login</title>\
         <body style=\"font:16px system-ui,sans-serif;margin:48px auto;max-width:520px;padding:0 16px\"><p>{message}</p></body>"
    );
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(reply.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// rustls with the ring provider and the system CA bundle (as the store's Postgres client).
pub(crate) fn tls() -> anyhow::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    if roots.is_empty() {
        bail!("no CA certificates found in the system bundle (install ca-certificates)");
    }
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// A redeem the server refused: its error code and message; on a 429, how long to wait and
/// whether the waiting login expires first.
#[derive(Debug)]
pub struct Refused {
    pub error: String,
    pub message: String,
    pub retry_after: Option<u64>,
    pub login_expires_first: bool,
}

async fn post_redeem(origin: &str, body: Value) -> anyhow::Result<Result<Enrolled, Refused>> {
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(tls()?)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REDEEM_TIMEOUT)
        .build()?;
    let response = client
        .post(format!("{origin}/auth/cli/redeem"))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("redeeming the sign-in at {origin}"))?;
    let status = response.status();
    if !status.is_success() {
        let header_wait = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let detail: Value = response.json().await.unwrap_or_default();
        let field = |k: &str| detail[k].as_str().map(str::to_string);
        return Ok(Err(Refused {
            error: field("error").unwrap_or_default(),
            message: field("message").unwrap_or_else(|| status.to_string()),
            retry_after: detail["retry_after"].as_u64().or(header_wait),
            login_expires_first: detail["login_expires_first"] == true,
        }));
    }
    let enrolled: Enrolled = response.json().await.context("the server's redeem answer is not a host key")?;
    if enrolled.key.is_empty() || enrolled.key.chars().any(|c| c.is_whitespace() || c.is_control()) || enrolled.host_id.is_empty() {
        bail!("the server's redeem answer is not a host key");
    }
    Ok(Ok(enrolled))
}

/// How long the loopback redeem waits in all on the server's rate limit before it gives up.
pub const LIMITED_WAIT: Duration = Duration::from_secs(60);

/// Redeems the loopback's code. A 429 (the per-address limit, checked before the code is taken)
/// is waited out and retried while the waits add up to at most [`LIMITED_WAIT`].
async fn redeem(origin: &str, run: &Run, code: &str, host: &str) -> anyhow::Result<Enrolled> {
    let body = json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": host});
    let mut waited = 0;
    loop {
        match post_redeem(origin, body.clone()).await? {
            Ok(enrolled) => return Ok(enrolled),
            Err(r) if r.error == "rate_limited" && r.retry_after.is_some_and(|w| waited + w <= LIMITED_WAIT.as_secs()) => {
                let wait = r.retry_after.unwrap_or_default().max(1);
                eprintln!("{}; retrying in {wait} s", r.message);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                waited += wait;
            }
            Err(r) => bail!("the server refused the sign-in: {}", r.message),
        }
    }
}

/// A pasted-code login waiting on this host for its code.
#[derive(Serialize, Deserialize)]
pub struct Pending {
    /// The server's origin.
    pub server: String,
    state: String,
    verifier: String,
    /// Unix seconds.
    pub started_at: u64,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending").field("server", &self.server).field("started_at", &self.started_at).finish_non_exhaustive()
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// Where a pasted-code login for `server` waits: `$XDG_CONFIG_HOME/conde/login/<digest>.json`.
pub fn pending_path(server: &str) -> anyhow::Result<std::path::PathBuf> {
    let origin = origin(server)?;
    let name: String = Sha256::digest(origin.as_bytes())[..8].iter().map(|b| format!("{b:02x}")).collect();
    Ok(boards::config_dir()?.join("login").join(format!("{name}.json")))
}

/// Starts a pasted-code login: stores the run at `path` (mode 0600) and returns the URL the
/// human opens on any device.
pub fn start_code(server: &str, path: &std::path::Path) -> anyhow::Result<String> {
    let origin = origin(server)?;
    let run = Run::new()?;
    let pending = Pending { server: origin.clone(), state: run.state.clone(), verifier: run.verifier, started_at: unix_now() };
    let dir = path.parent().context("the login path has no directory")?;
    crate::private::create_dir(dir)?;
    let tmp = dir.join(format!(".login.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = crate::private::create_file(&tmp)?;
    std::io::Write::write_all(&mut f, serde_json::to_string(&pending)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(format!("{origin}/auth/cli?mode=code&state={}&challenge={}", run.state, run.challenge))
}

/// The login waiting at `path` for `server`, if it is still fresh.
fn load_pending(server: &str, path: &std::path::Path) -> anyhow::Result<Pending> {
    let start = format!("start one with: conde login --server {server} --code");
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("no conde login is waiting for a code from {} on this host; {start}", origin(server)?)
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let pending: Pending = serde_json::from_slice(&raw).with_context(|| format!("{} is not a conde login", path.display()))?;
    if pending.server != origin(server)? {
        bail!("{} belongs to {}; {start}", path.display(), pending.server);
    }
    if unix_now().saturating_sub(pending.started_at) > PENDING_FOR.as_secs() {
        let _ = std::fs::remove_file(path);
        bail!("the waiting conde login expired; {start}");
    }
    Ok(pending)
}

/// How a pasted code fared.
#[derive(Debug)]
pub enum CodeOutcome {
    Enrolled(Enrolled),
    /// A wrong code, or none signed in yet; the login waits for another try.
    Retry(String),
}

/// Redeems `typed` for the login waiting at `path`. The waiting login is removed once the
/// server is done with it (enrolled, expired, out of tries, or rate-limited past its expiry); a
/// wrong or mistyped code, or any other refusal, keeps it.
pub async fn finish_code(server: &str, path: &std::path::Path, typed: &str, host: &str) -> anyhow::Result<CodeOutcome> {
    let pending = load_pending(server, path)?;
    let code = match code_typed(typed) {
        Ok(code) => code,
        Err(why) => return Ok(CodeOutcome::Retry(why)),
    };
    let body = json!({"code": code, "state": pending.state, "code_verifier": pending.verifier, "host": host});
    match post_redeem(&pending.server, body).await? {
        Ok(enrolled) => {
            let _ = std::fs::remove_file(path);
            Ok(CodeOutcome::Enrolled(enrolled))
        }
        Err(r) if r.error == "rate_limited" && r.login_expires_first => {
            // the next allowed attempt comes after the login's end: nothing is left to wait for
            let _ = std::fs::remove_file(path);
            bail!("the server refused the sign-in: {}", r.message)
        }
        Err(r) if ["invalid_code", "authorization_pending", "rate_limited"].contains(&r.error.as_str()) => {
            Ok(CodeOutcome::Retry(r.message))
        }
        Err(r) => {
            // only an answer that ended the login on the server ends it here
            if FINAL.contains(&r.error.as_str()) {
                let _ = std::fs::remove_file(path);
            }
            bail!("the server refused the sign-in: {}", r.message)
        }
    }
}

/// Redeem errors after which the server holds no login for the run (it was taken).
const FINAL: &[&str] = &["invalid_grant", "host_taken", "disabled"];

/// A typed code in canonical form, or why it cannot be one (case, spaces and dashes are free).
fn code_typed(typed: &str) -> Result<String, String> {
    let code: String = typed.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_uppercase()).collect();
    let alphabet = format!("a sign-in code has 8 characters from {CODE_ALPHABET} (there is no 0/O, 1/I/L or U)");
    if let Some(bad) = code.chars().find(|c| !CODE_ALPHABET.contains(*c)) {
        return Err(format!("'{bad}' is not in a sign-in code: {alphabet}"));
    }
    if code.chars().count() != 8 {
        return Err(format!("that is {} characters: {alphabet}", code.chars().count()));
    }
    Ok(code)
}

/// Whether this machine has no desktop to open a browser on.
pub fn headless(os_is_linux: bool, display: Option<&str>, wayland: Option<&str>) -> bool {
    let set = |v: Option<&str>| v.is_some_and(|v| !v.is_empty());
    os_is_linux && !set(display) && !set(wayland)
}

/// The pasted-code login: `code` is what `--code` carried (empty: start one).
fn code_login(server: &str, host: &str, finish_hint: &str, code: &str, runtime: &tokio::runtime::Runtime) -> anyhow::Result<Option<Enrolled>> {
    let path = pending_path(server)?;
    if !code.is_empty() {
        return match runtime.block_on(finish_code(server, &path, code, host))? {
            CodeOutcome::Enrolled(e) => Ok(Some(e)),
            CodeOutcome::Retry(why) => bail!("{why}; run again with the code the page shows: {finish_hint}"),
        };
    }
    if let Ok(earlier) = load_pending(server, &path) {
        let minutes = unix_now().saturating_sub(earlier.started_at).div_ceil(60);
        eprintln!(
            "A conde login started {minutes} min ago was still waiting for its code; this one replaces it, so a code from that run's page no longer works here."
        );
    }
    let url = start_code(server, &path)?;
    eprintln!("Open this URL in a browser on any device and sign in:\n\n  {url}\n\nThe page then shows a one-time sign-in code.");
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("When you have it, finish with:\n\n  {finish_hint}\n");
        return Ok(None);
    }
    loop {
        let typed = crate::credentials::prompt_secret("Sign-in code: ", "conde login needs a terminal for the code prompt")?;
        if typed.is_empty() {
            eprintln!("No code entered. To finish later: {finish_hint}");
            return Ok(None);
        }
        match runtime.block_on(finish_code(server, &path, &typed, host))? {
            CodeOutcome::Enrolled(e) => return Ok(Some(e)),
            CodeOutcome::Retry(why) => eprintln!("{why}"),
        }
    }
}

/// Opens `url` in the desktop browser; the URL is printed first for headless use.
fn open_browser(url: &str) {
    eprintln!("Opening the browser to sign in. If it does not open, visit:\n\n  {url}\n");
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let spawned = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if spawned.is_err() {
        eprintln!("(could not start {opener}; open the URL above by hand)");
    }
}

/// The exit status of a pasted-code login that is waiting for its code (the URL was printed;
/// finish with `--code <CODE>`), so a script can tell it from an enrolled host (0).
pub const PENDING: i32 = 3;

/// `conde login --server <url> [--host <id>] [--board <alias>] [--no-browser] [--code [<CODE>]]`.
/// `code`: None without `--code`, empty for a bare `--code`.
pub fn run(server: &str, host: Option<&str>, board: Option<&str>, browser: bool, code: Option<&str>) -> anyhow::Result<i32> {
    origin(server)?;
    if let Some(alias) = board {
        boards::check_shared_alias(alias)?;
        boards::check_pin(&boards::path(alias)?, alias, server, None)?;
    }
    let paste = code.is_some()
        || !browser
        || headless(
            cfg!(target_os = "linux"),
            std::env::var("DISPLAY").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        );
    let given_host = host;
    let host = host.map(str::to_lowercase).unwrap_or_else(crate::util::local_host);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let enrolled = if paste {
        let mut hint = format!("conde login --server {server}");
        if let Some(h) = given_host {
            hint.push_str(&format!(" --host {h}"));
        }
        if let Some(b) = board {
            hint.push_str(&format!(" --board {b}"));
        }
        hint.push_str(" --code <CODE>");
        match code_login(server, &host, &hint, code.unwrap_or_default(), &runtime)? {
            Some(e) => e,
            None => return Ok(PENDING),
        }
    } else {
        runtime.block_on(sign_in(server, &host, open_browser, BROWSER_WAIT))?
    };
    drop(runtime);
    let alias = match board {
        Some(a) => a.to_string(),
        None => boards::board_info(server, &enrolled.key)?.name,
    };
    boards::check_shared_alias(&alias)?;
    let path = boards::path(&alias)?;
    let cfg = save(&path, &alias, server, &enrolled)?;
    println!("signed in as {}", enrolled.user);
    let bridge = crate::admin::reload_bridge(&crate::paths::socket_path(), &alias);
    print!("{}", crate::admin::enrolled_text(&alias, &cfg, &path, enrolled.reenrolled, &bridge));
    Ok(0)
}

/// Stores the key as the board file `alias` (account: the signed-in user), refusing to
/// retarget a pinned alias.
pub fn save(path: &std::path::Path, alias: &str, server: &str, enrolled: &Enrolled) -> anyhow::Result<BoardConfig> {
    let cfg = BoardConfig {
        server: server.to_string(),
        host_id: enrolled.host_id.clone(),
        owner: enrolled.user.clone(),
        key: enrolled.key.clone(),
    };
    boards::store(path, alias, &cfg)?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert_eq!(origin("https://Conde.nativeai.space/").unwrap(), "https://conde.nativeai.space");
        assert_eq!(origin("wss://conde.nativeai.space/v1/bridge").unwrap(), "https://conde.nativeai.space");
        assert_eq!(origin("http://127.0.0.1:8080").unwrap(), "http://127.0.0.1:8080");
        assert!(origin("http://conde.nativeai.space").is_err());
        assert!(origin("http://127.0.0.1.evil.example").is_err());
        assert!(origin("https://user@host").is_err());
        assert!(origin("conde.nativeai.space").is_err());
    }

    #[test]
    fn run_secrets() {
        let run = Run::new().unwrap();
        assert_eq!(run.state.len(), 64);
        assert_eq!(run.verifier.len(), 43);
        assert_eq!(run.challenge, URL_SAFE_NO_PAD.encode(Sha256::digest(run.verifier.as_bytes())));
    }
}
