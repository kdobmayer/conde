//! OAuth Client ID Metadata Documents (draft-ietf-oauth-client-id-metadata-document, as MCP
//! 2026-07-28 requires them): a hosted client's `client_id` is an https URL on an allowed host,
//! and the document there names the client and its redirect URIs. Fetched without redirects,
//! with a time and size cap, and cached per client_id. Every refusal has a fixed code.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openidconnect::url::{Host, Url};
use serde::Deserialize;

/// How long a fetch may take, connect to last byte.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest document accepted.
pub const MAX_DOCUMENT: usize = 64 * 1024;
/// How long a fetched document is reused.
pub const CACHE_FOR: Duration = Duration::from_secs(3600);

/// Why a fetch produced no document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError {
    Unreachable,
    Timeout,
    /// The server answered with a redirect; it is never followed.
    Redirect,
    /// Any status other than 200.
    Status(u16),
    TooLarge,
}

/// Fetches a client's metadata document (the server's HTTPS client; tests substitute their own).
pub trait ClientMetadataFetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, FetchError>> + Send + 'a>>;
}

/// The fetcher over reqwest: no redirects, `timeout` for the whole exchange, `cap` bytes.
pub struct HttpFetcher {
    client: reqwest::Client,
    cap: usize,
}

impl HttpFetcher {
    pub fn new(timeout: Duration, cap: usize) -> HttpFetcher {
        HttpFetcher { client: super::auth::http_client(timeout), cap }
    }
}

impl Default for HttpFetcher {
    fn default() -> HttpFetcher {
        HttpFetcher::new(FETCH_TIMEOUT, MAX_DOCUMENT)
    }
}

impl ClientMetadataFetcher for HttpFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, FetchError>> + Send + 'a>> {
        Box::pin(async move {
            let failed = |e: reqwest::Error| if e.is_timeout() { FetchError::Timeout } else { FetchError::Unreachable };
            let mut response = self
                .client
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(failed)?;
            let status = response.status();
            if status.is_redirection() {
                return Err(FetchError::Redirect);
            }
            if status != reqwest::StatusCode::OK {
                return Err(FetchError::Status(status.as_u16()));
            }
            if response.content_length().is_some_and(|n| n > self.cap as u64) {
                return Err(FetchError::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(failed)? {
                if body.len() + chunk.len() > self.cap {
                    return Err(FetchError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        })
    }
}

/// Why a client was refused, before its redirect URI could be trusted. The codes are the whole
/// vocabulary the authorization page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    ClientIdMissing,
    ClientIdNotHttps,
    ClientIdMalformed,
    ClientHostNotAllowed,
    MetadataUnreachable,
    MetadataTimeout,
    MetadataRedirected,
    MetadataStatus,
    MetadataTooLarge,
    MetadataNotJson,
    MetadataIncomplete,
    ClientIdMismatch,
    UnsupportedAuthMethod,
    RedirectUriMissing,
    RedirectUriNotRegistered,
    /// An authorization request names a parameter twice.
    DuplicateParameter,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::ClientIdMissing => "client_id_missing",
            Refusal::ClientIdNotHttps => "client_id_not_https",
            Refusal::ClientIdMalformed => "client_id_malformed",
            Refusal::ClientHostNotAllowed => "client_host_not_allowed",
            Refusal::MetadataUnreachable => "metadata_unreachable",
            Refusal::MetadataTimeout => "metadata_timeout",
            Refusal::MetadataRedirected => "metadata_redirected",
            Refusal::MetadataStatus => "metadata_status",
            Refusal::MetadataTooLarge => "metadata_too_large",
            Refusal::MetadataNotJson => "metadata_not_json",
            Refusal::MetadataIncomplete => "metadata_incomplete",
            Refusal::ClientIdMismatch => "client_id_mismatch",
            Refusal::UnsupportedAuthMethod => "unsupported_auth_method",
            Refusal::RedirectUriMissing => "redirect_uri_missing",
            Refusal::RedirectUriNotRegistered => "redirect_uri_not_registered",
            Refusal::DuplicateParameter => "duplicate_parameter",
        }
    }

    /// What the page tells the person.
    pub fn message(self) -> &'static str {
        match self {
            Refusal::ClientIdMissing | Refusal::ClientIdNotHttps | Refusal::ClientIdMalformed => {
                "The app did not identify itself with an https client metadata URL."
            }
            Refusal::ClientHostNotAllowed => "This board accepts connections only from the apps its admin allowed.",
            Refusal::MetadataUnreachable
            | Refusal::MetadataTimeout
            | Refusal::MetadataRedirected
            | Refusal::MetadataStatus
            | Refusal::MetadataTooLarge => "The app's client metadata could not be read. Try again later.",
            Refusal::MetadataNotJson | Refusal::MetadataIncomplete | Refusal::ClientIdMismatch => {
                "The app's client metadata is not valid."
            }
            Refusal::UnsupportedAuthMethod => "The app asks for a client authentication this board does not offer.",
            Refusal::RedirectUriMissing | Refusal::RedirectUriNotRegistered => {
                "The app's return address is not one its metadata lists."
            }
            Refusal::DuplicateParameter => "The request names a parameter twice.",
        }
    }
}

/// A validated client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub client_id: String,
    /// The client_id URL's host: what the person is shown and the keys page lists.
    pub host: String,
    /// The document's `client_name`: shown quoted, never trusted.
    pub name: String,
    pub redirect_uris: Vec<String>,
}

#[derive(Deserialize)]
struct Document {
    client_id: Option<String>,
    client_name: Option<String>,
    redirect_uris: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
}

/// The allow-list, the fetcher and the cache.
pub struct Registry {
    hosts: Vec<String>,
    fetcher: Arc<dyn ClientMetadataFetcher>,
    cache_for: Duration,
    cache: Mutex<HashMap<String, (Client, Instant)>>,
}

fn loopback(url: &Url) -> bool {
    url.scheme() == "http"
        && match url.host() {
            Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        }
}

impl Registry {
    pub fn new(hosts: Vec<String>, fetcher: Arc<dyn ClientMetadataFetcher>, cache_for: Duration) -> Registry {
        let hosts = hosts.into_iter().map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect();
        Registry { hosts, fetcher, cache_for, cache: Mutex::new(HashMap::new()) }
    }

    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    fn allowed(&self, url: &Url) -> bool {
        url.host_str().is_some_and(|h| self.hosts.iter().any(|a| a == h))
    }

    /// The client_id's URL when it is an https URL with a path on an allowed host (no port, no
    /// credentials, no fragment).
    pub fn check_id(&self, client_id: &str) -> Result<Url, Refusal> {
        if client_id.is_empty() {
            return Err(Refusal::ClientIdMissing);
        }
        let url = Url::parse(client_id).map_err(|_| Refusal::ClientIdMalformed)?;
        if url.scheme() != "https" {
            return Err(Refusal::ClientIdNotHttps);
        }
        let plain = url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path() != "/"
            && client_id.len() <= 2048;
        if !plain {
            return Err(Refusal::ClientIdMalformed);
        }
        if !self.allowed(&url) {
            return Err(Refusal::ClientHostNotAllowed);
        }
        Ok(url)
    }

    /// The client behind `client_id`, from the cache or its document.
    pub async fn client(&self, client_id: &str) -> Result<Client, Refusal> {
        let url = self.check_id(client_id)?;
        if let Some((client, at)) = self.cache.lock().unwrap_or_else(|p| p.into_inner()).get(client_id)
            && at.elapsed() < self.cache_for
        {
            return Ok(client.clone());
        }
        let body = self.fetcher.fetch(client_id).await.map_err(|e| match e {
            FetchError::Unreachable => Refusal::MetadataUnreachable,
            FetchError::Timeout => Refusal::MetadataTimeout,
            FetchError::Redirect => Refusal::MetadataRedirected,
            FetchError::Status(_) => Refusal::MetadataStatus,
            FetchError::TooLarge => Refusal::MetadataTooLarge,
        })?;
        let doc: Document = serde_json::from_slice(&body).map_err(|e| {
            if e.is_data() { Refusal::MetadataIncomplete } else { Refusal::MetadataNotJson }
        })?;
        let (Some(id), Some(name), Some(redirect_uris)) = (doc.client_id, doc.client_name, doc.redirect_uris) else {
            return Err(Refusal::MetadataIncomplete);
        };
        if id != client_id {
            return Err(Refusal::ClientIdMismatch);
        }
        if name.trim().is_empty() || redirect_uris.is_empty() {
            return Err(Refusal::MetadataIncomplete);
        }
        if doc.token_endpoint_auth_method.as_deref().is_some_and(|m| m != "none") {
            return Err(Refusal::UnsupportedAuthMethod);
        }
        let client = Client {
            client_id: id,
            host: url.host_str().unwrap_or_default().to_string(),
            name: name.chars().filter(|c| !c.is_control()).take(100).collect(),
            redirect_uris,
        };
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|_, (_, at)| at.elapsed() < self.cache_for);
        cache.insert(client_id.to_string(), (client.clone(), Instant::now()));
        Ok(client)
    }

    /// The request's redirect URI, when the client's document lists it exactly; a loopback
    /// `http` URI matches with any port (RFC 8252 §7.3), any other must be https on an allowed host.
    pub fn check_redirect(&self, client: &Client, redirect_uri: &str) -> Result<Url, Refusal> {
        if redirect_uri.is_empty() {
            return Err(Refusal::RedirectUriMissing);
        }
        let url = Url::parse(redirect_uri).map_err(|_| Refusal::RedirectUriNotRegistered)?;
        if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
            return Err(Refusal::RedirectUriNotRegistered);
        }
        let listed = if loopback(&url) {
            let mut bare = url.clone();
            let _ = bare.set_port(None);
            client.redirect_uris.iter().any(|r| {
                Url::parse(r).is_ok_and(|mut r| {
                    let _ = r.set_port(None);
                    loopback(&r) && r == bare
                })
            })
        } else {
            url.scheme() == "https" && self.allowed(&url) && client.redirect_uris.iter().any(|r| r == redirect_uri)
        };
        if listed { Ok(url) } else { Err(Refusal::RedirectUriNotRegistered) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixed(Result<Vec<u8>, FetchError>, AtomicUsize);

    impl ClientMetadataFetcher for Fixed {
        fn fetch<'a>(&'a self, _: &'a str) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, FetchError>> + Send + 'a>> {
            self.1.fetch_add(1, Ordering::SeqCst);
            let r = self.0.clone();
            Box::pin(async move { r })
        }
    }

    const ID: &str = "https://claude.ai/oauth/claude-code-client-metadata";

    fn doc(v: serde_json::Value) -> Result<Vec<u8>, FetchError> {
        Ok(v.to_string().into_bytes())
    }

    fn claude_code() -> serde_json::Value {
        serde_json::json!({"client_id": ID, "client_name": "Claude Code", "client_uri": "https://claude.ai",
            "redirect_uris": ["http://localhost/callback", "http://127.0.0.1/callback", "https://claude.ai/api/mcp/auth_callback"],
            "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
            "token_endpoint_auth_method": "none"})
    }

    fn registry(r: Result<Vec<u8>, FetchError>) -> (Registry, Arc<Fixed>) {
        let f = Arc::new(Fixed(r, AtomicUsize::new(0)));
        (Registry::new(vec!["claude.ai".into(), "Claude.com".into()], f.clone(), CACHE_FOR), f)
    }

    fn block<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn the_client_id_must_be_https_on_an_allowed_host() {
        let (r, f) = registry(doc(claude_code()));
        for (id, refusal) in [
            ("", Refusal::ClientIdMissing),
            ("not a url", Refusal::ClientIdMalformed),
            ("http://claude.ai/oauth/x", Refusal::ClientIdNotHttps),
            ("https://claude.ai", Refusal::ClientIdMalformed),
            ("https://claude.ai:8443/x", Refusal::ClientIdMalformed),
            ("https://u:p@claude.ai/x", Refusal::ClientIdMalformed),
            ("https://claude.ai/x#f", Refusal::ClientIdMalformed),
            ("https://evil.example/x", Refusal::ClientHostNotAllowed),
            ("https://sub.claude.ai/x", Refusal::ClientHostNotAllowed),
            ("https://claude.ai.evil.example/x", Refusal::ClientHostNotAllowed),
        ] {
            assert_eq!(block(r.client(id)), Err(refusal), "{id}");
        }
        assert_eq!(f.1.load(Ordering::SeqCst), 0, "nothing refused by its id is fetched");
        assert_eq!(r.check_id("https://claude.com/x").map(|u| u.host_str().unwrap().to_string()), Ok("claude.com".into()));
    }

    #[test]
    fn fetch_failures_map_to_their_codes() {
        for (err, refusal) in [
            (FetchError::Unreachable, Refusal::MetadataUnreachable),
            (FetchError::Timeout, Refusal::MetadataTimeout),
            (FetchError::Redirect, Refusal::MetadataRedirected),
            (FetchError::Status(404), Refusal::MetadataStatus),
            (FetchError::TooLarge, Refusal::MetadataTooLarge),
        ] {
            assert_eq!(block(registry(Err(err)).0.client(ID)), Err(refusal));
        }
    }

    #[test]
    fn the_document_is_validated() {
        let mut other = claude_code();
        other["client_id"] = "https://claude.ai/oauth/other".into();
        let mut nameless = claude_code();
        nameless.as_object_mut().unwrap().remove("client_name");
        let mut no_uris = claude_code();
        no_uris["redirect_uris"] = serde_json::json!([]);
        let mut secret = claude_code();
        secret["token_endpoint_auth_method"] = "client_secret_basic".into();
        let mut wrong_type = claude_code();
        wrong_type["redirect_uris"] = "http://localhost/callback".into();
        for (body, refusal) in [
            (Ok(b"{not json".to_vec()), Refusal::MetadataNotJson),
            (doc(serde_json::json!([1, 2])), Refusal::MetadataIncomplete),
            (doc(other), Refusal::ClientIdMismatch),
            (doc(nameless), Refusal::MetadataIncomplete),
            (doc(no_uris), Refusal::MetadataIncomplete),
            (doc(wrong_type), Refusal::MetadataIncomplete),
            (doc(secret), Refusal::UnsupportedAuthMethod),
        ] {
            assert_eq!(block(registry(body).0.client(ID)), Err(refusal));
        }
    }

    #[test]
    fn a_document_is_cached_per_client_id() {
        let (r, f) = registry(doc(claude_code()));
        let c = block(r.client(ID)).unwrap();
        assert_eq!((c.host.as_str(), c.name.as_str()), ("claude.ai", "Claude Code"));
        block(r.client(ID)).unwrap();
        assert_eq!(f.1.load(Ordering::SeqCst), 1);
        let expired = Registry::new(vec!["claude.ai".into()], f.clone(), Duration::ZERO);
        block(expired.client(ID)).unwrap();
        block(expired.client(ID)).unwrap();
        assert_eq!(f.1.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn redirect_uris_match_exactly_but_for_a_loopback_port() {
        let (r, _) = registry(doc(claude_code()));
        let c = block(r.client(ID)).unwrap();
        for ok in [
            "http://localhost/callback",
            "http://localhost:53111/callback",
            "http://127.0.0.1:8080/callback",
            "https://claude.ai/api/mcp/auth_callback",
        ] {
            assert!(r.check_redirect(&c, ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "http://localhost:53111/callback/x",
            "http://localhost/other",
            "http://evil.example/callback",
            "https://localhost/callback",
            "https://claude.ai/api/mcp/auth_callback?x=1",
            "https://claude.ai/api/mcp/auth_callback#f",
            "https://claude.ai/other",
            "http://[::1]/callback",
        ] {
            assert!(r.check_redirect(&c, bad).is_err(), "{bad}");
        }
        // A listed https URI off the allow-list is still refused.
        let odd = Client { redirect_uris: vec!["https://evil.example/cb".into()], ..c };
        assert_eq!(r.check_redirect(&odd, "https://evil.example/cb"), Err(Refusal::RedirectUriNotRegistered));
    }

    // The real fetcher against a local server (the https rule is the registry's, above).
    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[test]
    fn the_http_fetcher_refuses_redirects_size_and_slowness() {
        use axum::response::{IntoResponse, Redirect};
        use axum::routing::get;
        let _ = rustls::crypto::ring::default_provider().install_default();
        block(async {
            let big = "x".repeat(MAX_DOCUMENT + 1);
            let app = axum::Router::new()
                .route("/ok", get(|| async { r#"{"client_id":"x"}"# }))
                .route("/moved", get(|| async { Redirect::temporary("/ok") }))
                .route("/missing", get(|| async { axum::http::StatusCode::NOT_FOUND }))
                .route("/big", get(move || async move { big }))
                .route(
                    "/chunked",
                    get(|| async {
                        let chunks = (0..80).map(|_| Ok::<_, std::io::Error>(vec![b'x'; 1024]));
                        axum::body::Body::from_stream(futures_util::stream::iter(chunks)).into_response()
                    }),
                )
                .route(
                    "/slow",
                    get(|| async {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        "{}"
                    }),
                );
            let base = serve(app).await;
            let f = HttpFetcher::new(Duration::from_millis(500), MAX_DOCUMENT);
            assert_eq!(f.fetch(&format!("{base}/ok")).await.unwrap(), br#"{"client_id":"x"}"#.to_vec());
            assert_eq!(f.fetch(&format!("{base}/moved")).await, Err(FetchError::Redirect));
            assert_eq!(f.fetch(&format!("{base}/missing")).await, Err(FetchError::Status(404)));
            assert_eq!(f.fetch(&format!("{base}/big")).await, Err(FetchError::TooLarge));
            assert_eq!(f.fetch(&format!("{base}/chunked")).await, Err(FetchError::TooLarge));
            assert_eq!(f.fetch(&format!("{base}/slow")).await, Err(FetchError::Timeout));
            assert_eq!(f.fetch("http://127.0.0.1:1/x").await, Err(FetchError::Unreachable));
        });
    }
}
