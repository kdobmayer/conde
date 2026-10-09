//! `conde login --server <url>`: enrolls this host through a browser sign-in. A loopback
//! listener on 127.0.0.1 receives a one-time code bound to this run (state + PKCE S256);
//! the code is redeemed over TLS for the host key, which never appears in a URL. The key is
//! stored exactly as `conde enroll` stores it (`boards/<alias>.json`, mode 0600).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, timeout, timeout_at};

use crate::boards::{self, BoardConfig};

/// How long the browser may take to sign in (MFA included).
pub const BROWSER_WAIT: Duration = Duration::from_secs(300);
/// The redeem request's deadline.
pub const REDEEM_TIMEOUT: Duration = Duration::from_secs(60);

/// What the server returns once for a redeemed code.
#[derive(Clone, Deserialize)]
pub struct Enrolled {
    pub host_id: String,
    pub key: String,
    pub user: String,
}

impl std::fmt::Debug for Enrolled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enrolled").field("host_id", &self.host_id).field("user", &self.user).finish_non_exhaustive()
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

async fn redeem(origin: &str, run: &Run, code: &str, host: &str) -> anyhow::Result<Enrolled> {
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(tls()?)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REDEEM_TIMEOUT)
        .build()?;
    let body = json!({"code": code, "state": run.state, "code_verifier": run.verifier, "host": host});
    let response = client
        .post(format!("{origin}/auth/cli/redeem"))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("redeeming the sign-in at {origin}"))?;
    let status = response.status();
    if !status.is_success() {
        let detail: Value = response.json().await.unwrap_or_default();
        let message = detail["message"].as_str().map(str::to_string).unwrap_or_else(|| status.to_string());
        bail!("the server refused the sign-in: {message}");
    }
    let enrolled: Enrolled = response.json().await.context("the server's redeem answer is not a host key")?;
    if enrolled.key.is_empty() || enrolled.key.chars().any(|c| c.is_whitespace() || c.is_control()) || enrolled.host_id.is_empty() {
        bail!("the server's redeem answer is not a host key");
    }
    Ok(enrolled)
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

/// `conde login --server <url> [--host <id>] [--board <alias>] [--no-browser]`.
pub fn run(server: &str, host: Option<&str>, board: Option<&str>, browser: bool) -> anyhow::Result<i32> {
    origin(server)?;
    if let Some(alias) = board {
        boards::check_shared_alias(alias)?;
        boards::check_pin(&boards::path(alias)?, alias, server, None)?;
    }
    let host = host.map(str::to_lowercase).unwrap_or_else(crate::util::local_host);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let enrolled = runtime.block_on(sign_in(
        server,
        &host,
        |url| {
            if browser {
                open_browser(url)
            } else {
                eprintln!("Open this URL in a browser to sign in:\n\n  {url}\n")
            }
        },
        BROWSER_WAIT,
    ))?;
    drop(runtime);
    let alias = match board {
        Some(a) => a.to_string(),
        None => boards::board_info(server, &enrolled.key)?.name,
    };
    boards::check_shared_alias(&alias)?;
    let path = boards::path(&alias)?;
    let cfg = save(&path, &alias, server, &enrolled)?;
    println!("signed in as {}", enrolled.user);
    print!("{}", crate::admin::enrolled_text(&alias, &cfg, &path));
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
