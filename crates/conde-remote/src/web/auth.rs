//! Entra ID sign-in (OIDC authorization code + PKCE, state and nonce, confidential client)
//! and the browser leg of `conde login`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json};
use axum_extra::extract::cookie::CookieJar;
use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType,
    CoreGenderClaim, CoreJsonWebKey, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm,
    CoreRevocableToken, CoreRevocationErrorResponse, CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AdditionalClaims, AsyncHttpClient, AuthType, AuthorizationCode, ClaimsVerificationError,
    Client, ClientId, ClientSecret, CsrfToken, EmptyExtraTokenFields, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, HttpRequest, HttpResponse, IdTokenFields, IssuerUrl,
    LogoutRequest, Nonce, PkceCodeChallenge, PkceCodeVerifier, PostLogoutRedirectUrl,
    ProviderMetadataWithLogout, RedirectUrl, Scope, SignatureVerificationError,
    StandardErrorResponse, StandardTokenResponse, TokenResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use super::keys::{self, host_slug};
use super::session::{self, WebError, digest, login_cookie, random_token, removal, same_secret};
use super::{CliCode, CreateUser, Identity, LoginTxn, PASTE_TRIES, WebConfig, WebState, WebUser, pages};

/// Discovery and JWKS are re-read after this long (and at once when a token's key is unknown).
const METADATA_MAX_AGE: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The Entra claims the board keys users on.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EntraClaims {
    #[serde(default)]
    tid: Option<String>,
    #[serde(default)]
    oid: Option<String>,
}

impl AdditionalClaims for EntraClaims {}

type EntraTokenResponse = StandardTokenResponse<
    IdTokenFields<
        EntraClaims,
        EmptyExtraTokenFields,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
    >,
    CoreTokenType,
>;

type EntraClient = Client<
    EntraClaims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    EntraTokenResponse,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Debug, Error)]
pub enum OidcError {
    #[error("discovery failed: {0}")]
    Discovery(String),
    #[error("configuration: {0}")]
    Config(String),
    #[error("token exchange failed: {0}")]
    Exchange(String),
    #[error("id token rejected: {0}")]
    Verify(String),
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct HttpError(String);

/// The openidconnect HTTP client over reqwest 0.13 (rustls with ring, Mozilla roots, no
/// redirects: discovery and token responses must come from the URLs asked).
#[derive(Clone)]
struct Http(reqwest::Client);

impl Http {
    fn new() -> Http {
        Http(http_client(HTTP_TIMEOUT))
    }
}

/// The server's outbound HTTP client: rustls with ring and the Mozilla roots, no redirects,
/// `timeout` for the whole exchange.
pub(crate) fn http_client(timeout: Duration) -> reqwest::Client {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .expect("the HTTP client configuration is static")
}

impl<'c> AsyncHttpClient<'c> for Http {
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let request =
                reqwest::Request::try_from(request).map_err(|e| HttpError(e.to_string()))?;
            let response = self
                .0
                .execute(request)
                .await
                .map_err(|e| HttpError(e.to_string()))?;
            let mut out = openidconnect::http::Response::builder().status(response.status());
            for (name, value) in response.headers() {
                out = out.header(name, value);
            }
            let body = response
                .bytes()
                .await
                .map_err(|e| HttpError(e.to_string()))?;
            out.body(body.to_vec())
                .map_err(|e| HttpError(e.to_string()))
        })
    }
}

/// A started authorization request.
struct Begin {
    url: String,
    state: String,
    pkce_verifier: String,
    nonce: String,
}

/// The OIDC relying party: provider metadata cached in memory (re-readable at any time, so
/// the server stays stateless), the confidential client built from it per request.
pub struct Oidc {
    issuer: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    post_logout_redirect_uri: String,
    http: Http,
    metadata: tokio::sync::Mutex<Option<(ProviderMetadataWithLogout, Instant)>>,
}

impl Oidc {
    pub fn new(config: &WebConfig) -> Oidc {
        Oidc {
            issuer: config.issuer.clone(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            redirect_uri: config.redirect_uri.clone(),
            post_logout_redirect_uri: config.post_logout_redirect_uri.clone(),
            http: Http::new(),
            metadata: tokio::sync::Mutex::new(None),
        }
    }

    async fn metadata(&self, refresh: bool) -> Result<ProviderMetadataWithLogout, OidcError> {
        let mut cached = self.metadata.lock().await;
        if let Some((md, at)) = cached.as_ref()
            && !refresh
            && at.elapsed() < METADATA_MAX_AGE
        {
            return Ok(md.clone());
        }
        let issuer =
            IssuerUrl::new(self.issuer.clone()).map_err(|e| OidcError::Config(e.to_string()))?;
        let md = ProviderMetadataWithLogout::discover_async(issuer, &self.http)
            .await
            .map_err(|e| OidcError::Discovery(format!("{e:?}")))?;
        *cached = Some((md.clone(), Instant::now()));
        Ok(md)
    }

    fn client(&self, md: ProviderMetadataWithLogout) -> Result<EntraClient, OidcError> {
        let redirect = RedirectUrl::new(self.redirect_uri.clone())
            .map_err(|e| OidcError::Config(e.to_string()))?;
        Ok(EntraClient::from_provider_metadata(
            md,
            ClientId::new(self.client_id.clone()),
            Some(ClientSecret::new(self.client_secret.clone())),
        )
        // client_secret_post: the secret goes in the form body, as Entra documents it.
        .set_auth_type(AuthType::RequestBody)
        .set_redirect_uri(redirect))
    }

    async fn begin(&self) -> Result<Begin, OidcError> {
        let client = self.client(self.metadata(false).await?)?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("profile".into()))
            .add_scope(Scope::new("email".into()))
            .set_pkce_challenge(challenge)
            .url();
        Ok(Begin {
            url: url.to_string(),
            state: state.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
            nonce: nonce.secret().clone(),
        })
    }

    /// Redeems the code on the token endpoint and validates the id token (signature against
    /// the JWKS, issuer, audience, expiry, nonce).
    async fn finish(
        &self,
        code: &str,
        pkce_verifier: &str,
        nonce: &str,
    ) -> Result<Identity, OidcError> {
        let client = self.client(self.metadata(false).await?)?;
        let token = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| OidcError::Config(e.to_string()))?
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier.to_string()))
            .request_async(&self.http)
            .await
            .map_err(|e| OidcError::Exchange(format!("{e:?}")))?;
        let id_token = token
            .id_token()
            .ok_or_else(|| OidcError::Exchange("the token response has no id_token".into()))?;
        let nonce = Nonce::new(nonce.to_string());
        let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
            Ok(claims) => claims.clone(),
            // The provider rotated its signing keys: read the JWKS again, verify once more.
            Err(ClaimsVerificationError::SignatureVerification(
                SignatureVerificationError::NoMatchingKey,
            )) => {
                let fresh = self.client(self.metadata(true).await?)?;
                id_token
                    .claims(&fresh.id_token_verifier(), &nonce)
                    .map_err(|e| OidcError::Verify(e.to_string()))?
                    .clone()
            }
            Err(e) => return Err(OidcError::Verify(e.to_string())),
        };
        let extra = claims.additional_claims();
        let (Some(tid), Some(oid)) = (extra.tid.clone(), extra.oid.clone()) else {
            return Err(OidcError::Verify(
                "the id token has no tid or oid claim".into(),
            ));
        };
        Ok(Identity {
            tid,
            oid,
            display_name: claims
                .name()
                .and_then(|n| n.get(None))
                .map(|n| n.as_str().to_string()),
            preferred_username: claims.preferred_username().map(|u| u.as_str().to_string()),
        })
    }

    /// The provider's sign-out URL returning to the board, or the board itself when the
    /// provider publishes none.
    async fn end_session_url(&self) -> String {
        let end = match self.metadata(false).await {
            Ok(md) => md.additional_metadata().end_session_endpoint.clone(),
            Err(e) => {
                tracing::warn!(error = %e, "no provider metadata for sign-out");
                None
            }
        };
        let Some(end) = end else {
            return self.post_logout_redirect_uri.clone();
        };
        let mut request =
            LogoutRequest::from(end).set_client_id(ClientId::new(self.client_id.clone()));
        if let Ok(back) = PostLogoutRedirectUrl::new(self.post_logout_redirect_uri.clone()) {
            request = request.set_post_logout_redirect_uri(back);
        }
        request.http_get_url().to_string()
    }
}

/// A local path to return to after sign-in; anything else becomes `/keys`.
fn local_path(next: Option<&str>) -> String {
    match next {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && p.len() <= 2048
                && !p.chars().any(char::is_control) =>
        {
            p.to_string()
        }
        _ => "/keys".into(),
    }
}

#[derive(Deserialize)]
pub(crate) struct LoginQuery {
    next: Option<String>,
}

/// `GET /auth/login?next=/path`: starts the authorization code flow.
pub(crate) async fn login(
    State(state): State<WebState>,
    jar: CookieJar,
    Query(q): Query<LoginQuery>,
) -> Result<Response, WebError> {
    let begin = state
        .0
        .oidc
        .begin()
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?;
    let txn = LoginTxn {
        pkce_verifier: begin.pkce_verifier,
        nonce: begin.nonce,
        return_to: local_path(q.next.as_deref()),
        created_at: state.now(),
    };
    let (key, now, limits) = (digest(&begin.state), state.now(), state.limits());
    state
        .store(move |s| {
            s.purge_expired(now, &limits)?;
            s.insert_login(&key, &txn)
        })
        .await?;
    let secure = state.config().secure_cookies();
    let jar = jar.add(session::cookie(login_cookie(secure), begin.state, secure));
    Ok((jar, Redirect::to(&begin.url)).into_response())
}

#[derive(Deserialize)]
pub(crate) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// `GET /auth/callback`: finishes sign-in and starts the browser session.
pub(crate) async fn callback(
    State(state): State<WebState>,
    jar: CookieJar,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, WebError> {
    let secure = state.config().secure_cookies();
    let started = jar.get(login_cookie(secure)).map(|c| c.value().to_string());
    let jar = jar.remove(removal(login_cookie(secure), "/", secure));
    if let Some(error) = q.error {
        let detail = q.error_description.unwrap_or(error);
        return Ok((
            jar,
            WebError::Forbidden(format!("Entra ID refused the sign-in: {detail}")),
        )
            .into_response());
    }
    let (Some(code), Some(returned_state)) = (q.code, q.state) else {
        return Err(WebError::BadRequest(
            "The sign-in response is incomplete.".into(),
        ));
    };
    if !started.is_some_and(|s| same_secret(&s, &returned_state)) {
        return Err(WebError::BadRequest(
            "This sign-in was started in another browser or has already finished. Start again."
                .into(),
        ));
    }
    let key = digest(&returned_state);
    let txn = state.store(move |s| s.take_login(&key)).await?;
    let now = state.now();
    let Some(txn) = txn.filter(|t| now - t.created_at <= state.limits().login_ttl) else {
        return Err(WebError::BadRequest(
            "This sign-in has expired. Start again.".into(),
        ));
    };
    let identity = state
        .0
        .oidc
        .finish(&code, &txn.pkce_verifier, &txn.nonce)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "sign-in refused");
            WebError::Forbidden("The sign-in could not be verified. Start again.".into())
        })?;
    if identity.tid != state.config().tenant_id {
        tracing::warn!(tid = %identity.tid, "sign-in from another tenant refused");
        return Err(WebError::Forbidden(
            "This account belongs to another directory. Sign in with your organization account."
                .into(),
        ));
    }
    let user = resolve_user(&state, identity.clone()).await?;
    if user.disabled {
        tracing::warn!(user = %user.name, "sign-in of a disabled user refused");
        return Err(WebError::Forbidden(
            "This account is disabled on the board. Ask the board admin.".into(),
        ));
    }
    let display = identity
        .display_name
        .clone()
        .or(identity.preferred_username.clone())
        .unwrap_or_else(|| user.name.clone());
    tracing::info!(user = %user.name, "signed in");
    let jar = session::start(&state, jar, user.user_id, user.name, display).await?;
    Ok((jar, Redirect::to(&txn.return_to)).into_response())
}

/// An owner name from the identity: the local part of preferred_username (else the display
/// name), lowercased to `[a-z0-9_-]`.
pub(crate) fn base_name(identity: &Identity) -> String {
    let source = identity
        .preferred_username
        .as_deref()
        .map(|u| u.split('@').next().unwrap_or(u))
        .or(identity.display_name.as_deref())
        .unwrap_or("");
    let mut out = String::new();
    for c in source.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "user".into() } else { out }
}

/// The board user of the identity; created on first sign-in. A taken name that no identity
/// owns is refused, not adopted: binding an admin-created user is an explicit admin step.
async fn resolve_user(state: &WebState, identity: Identity) -> Result<WebUser, WebError> {
    let base = base_name(&identity);
    let outcome = state
        .store(move |s| {
            if let Some(user) = s.user_by_identity(&identity.tid, &identity.oid)? {
                s.set_profile(user.user_id, &identity)?;
                return Ok(Ok(user));
            }
            for n in 1..=9 {
                let name = if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                match s.create_user(&identity, &name)? {
                    CreateUser::Created(user) => return Ok(Ok(user)),
                    CreateUser::NameTaken { bound: false } if n == 1 => {
                        return Ok(Err(format!(
                            "A board user named '{name}' already exists and is not linked to an \
                             Entra account. Ask the board admin to link it to your account \
                             (object id {}).",
                            identity.oid
                        )));
                    }
                    CreateUser::NameTaken { .. } => continue,
                }
            }
            Ok(Err(format!(
                "No free user name derived from '{base}'. Ask the board admin."
            )))
        })
        .await?;
    outcome.map_err(WebError::Forbidden)
}

#[derive(Deserialize)]
pub(crate) struct LogoutForm {
    #[serde(default)]
    csrf: String,
}

/// `POST /auth/logout`: ends the board session, then the Entra session.
pub(crate) async fn logout(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<LogoutForm>,
) -> Result<Response, WebError> {
    let Some(current) = session::current(&state, &jar).await? else {
        return Ok(Redirect::to(&state.config().post_logout_redirect_uri).into_response());
    };
    current.check_csrf(&form.csrf)?;
    let jar = session::end(&state, jar, &current).await?;
    let target = state.0.oidc.end_session_url().await;
    Ok((jar, Redirect::to(&target)).into_response())
}

#[derive(Deserialize)]
pub(crate) struct CliQuery {
    port: Option<u16>,
    state: Option<String>,
    challenge: Option<String>,
    /// `code`: the pasted-code flow (no loopback); the page shows a short code.
    mode: Option<String>,
}

fn url_safe(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

const PASTE_ALPHABET: &[u8] = super::CODE_ALPHABET.as_bytes();
/// Characters in a pasted code (about 39 bits).
const PASTE_LEN: usize = 8;

/// A fresh pasted code, unbiased over [`PASTE_ALPHABET`].
fn paste_code() -> String {
    let n = PASTE_ALPHABET.len() as u8;
    let limit = u8::MAX - u8::MAX % n;
    let mut out = String::with_capacity(PASTE_LEN);
    while out.len() < PASTE_LEN {
        let b: u8 = rand::random();
        if b < limit {
            out.push(PASTE_ALPHABET[(b % n) as usize] as char);
        }
    }
    out
}

/// A typed code in canonical form (upper case, no spaces or dashes), when it can be one.
fn paste_normalized(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == PASTE_LEN && code.bytes().all(|b| PASTE_ALPHABET.contains(&b))).then_some(code)
}

/// Where a pasted-code login is stored: never a loopback code's digest (those are 64 hex).
fn paste_key(cli_state: &str) -> String {
    digest(&format!("paste:{cli_state}"))
}

fn paste_digest(cli_state: &str, code: &str) -> String {
    digest(&format!("{cli_state}:{code}"))
}

/// `GET /auth/cli?port=&state=&challenge=`: signs the browser in if needed, then sends it to
/// the CLI's loopback listener with a one-time code bound to the CLI run's state and PKCE
/// challenge. With `mode=code` instead of `port` (a headless host has no loopback the browser
/// can reach), it shows a confirm page; its POST ([`cli_confirm`]) shows a short code for the
/// human to type at the CLI. The key is minted only when the CLI redeems the code.
pub(crate) async fn cli_start(
    State(state): State<WebState>,
    jar: CookieJar,
    Query(q): Query<CliQuery>,
) -> Result<Response, WebError> {
    let paste = match q.mode.as_deref() {
        None => false,
        Some("code") => true,
        Some(_) => {
            return Err(WebError::BadRequest(
                "This `conde login` link is malformed.".into(),
            ));
        }
    };
    let (Some(cli_state), Some(challenge)) = (q.state, q.challenge) else {
        return Err(WebError::BadRequest(
            "This link needs port, state and challenge; start it with `conde login`.".into(),
        ));
    };
    let port = match (q.port, paste) {
        (Some(port), false) => port,
        (None, true) => 0,
        (None, false) => {
            return Err(WebError::BadRequest(
                "This link needs port, state and challenge; start it with `conde login`.".into(),
            ));
        }
        (Some(_), true) => {
            return Err(WebError::BadRequest(
                "This `conde login` link is malformed.".into(),
            ));
        }
    };
    if (!paste && port < 1024) || !url_safe(&cli_state, 16, 128) || !url_safe(&challenge, 43, 43) {
        return Err(WebError::BadRequest(
            "This `conde login` link is malformed.".into(),
        ));
    }
    let Some(current) = session::current(&state, &jar).await? else {
        let back = if paste {
            format!("/auth/cli?mode=code&state={cli_state}&challenge={challenge}")
        } else {
            format!("/auth/cli?port={port}&state={cli_state}&challenge={challenge}")
        };
        let back: String =
            openidconnect::url::form_urlencoded::byte_serialize(back.as_bytes()).collect();
        return Ok(Redirect::to(&format!("/auth/login?next={back}")).into_response());
    };
    if paste {
        // GET only asks; the code is issued by the confirming POST (no side effect here)
        return Ok(pages::cli_confirm(
            keys::header(&current),
            &current.record.user_name,
            &cli_state,
            &challenge,
            &keys::idp_origin(&state),
        ));
    }
    let record = CliCode {
        user_id: current.record.user_id,
        user_name: current.record.user_name.clone(),
        state: cli_state.clone(),
        challenge,
        created_at: state.now(),
        paste: None,
        tries: 0,
    };
    let code = random_token();
    let key = digest(&code);
    state
        .store(move |s| s.insert_cli_code(&key, &record))
        .await?;
    Ok(Redirect::to(&format!(
        "http://127.0.0.1:{port}/done?code={code}&state={cli_state}"
    ))
    .into_response())
}

#[derive(Deserialize)]
pub(crate) struct CliConfirmForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    challenge: String,
}

/// `POST /auth/cli` (the confirm page's button): issues the pasted-code login's code and shows
/// it, to the signed-in session whose CSRF token the form carries. A new POST by the same
/// account issues a new code (the old one dies); a login another account confirmed stays theirs.
pub(crate) async fn cli_confirm(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<CliConfirmForm>,
) -> Result<Response, WebError> {
    let current = session::current(&state, &jar)
        .await?
        .ok_or_else(keys::signed_out)?;
    current.check_csrf(&form.csrf)?;
    let (cli_state, challenge) = (form.state, form.challenge);
    if !url_safe(&cli_state, 16, 128) || !url_safe(&challenge, 43, 43) {
        return Err(WebError::BadRequest(
            "This `conde login` link is malformed.".into(),
        ));
    }
    let code = paste_code();
    let record = CliCode {
        user_id: current.record.user_id,
        user_name: current.record.user_name.clone(),
        state: cli_state.clone(),
        challenge,
        created_at: state.now(),
        paste: Some(paste_digest(&cli_state, &code)),
        tries: 0,
    };
    let key = paste_key(&cli_state);
    let user_id = record.user_id;
    let issued = state
        .store(move |s| {
            if let Some(old) = s.take_cli_code(&key)?
                && old.user_id != user_id
            {
                s.insert_cli_code(&key, &old)?;
                return Ok(false);
            }
            s.insert_cli_code(&key, &record)?;
            Ok(true)
        })
        .await?;
    if !issued {
        return Err(WebError::Forbidden(
            "Another account already signed in for this conde login. Run conde login again.".into(),
        ));
    }
    let minutes = state.config().cli_code_ttl.as_secs().div_ceil(60);
    let shown = format!("{}-{}", &code[..4], &code[4..]);
    Ok(pages::cli_code(
        keys::header(&current),
        &shown,
        minutes,
        &keys::idp_origin(&state),
    ))
}

#[derive(Deserialize)]
pub(crate) struct RedeemRequest {
    /// The loopback's one-time code (64 hex), or the code the page showed (pasted flow).
    code: String,
    state: String,
    code_verifier: String,
    /// The machine's hostname: the host id (when free) and the key's label.
    host: String,
}

/// What `POST /auth/cli/redeem` returns, once.
#[derive(Debug, Deserialize, Serialize)]
pub struct RedeemResponse {
    pub host_id: String,
    pub key: String,
    pub user: String,
    /// The host existed before: its previous keys are revoked.
    #[serde(default)]
    pub reenrolled: bool,
}

fn redeem_error(status: StatusCode, code: &str, message: &str) -> Response {
    let mut r = (status, Json(json!({"error": code, "message": message}))).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn store_failed(e: super::WebStoreError) -> Response {
    tracing::error!(error = %e, "cli code store failed");
    redeem_error(StatusCode::INTERNAL_SERVER_ERROR, "internal", "store failed")
}

fn pkce_matches(verifier: String, challenge: &str) -> bool {
    if !(43..=128).contains(&verifier.len()) {
        return false;
    }
    let expected = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier));
    same_secret(expected.as_str(), challenge)
}

/// `POST /auth/cli/redeem` `{code, state, code_verifier, host}`: a 64-hex `code` is the
/// loopback's, taken whatever the outcome, then expiry, state and PKCE are checked; any other
/// `code` is a pasted one (see [`paste_redeem`]). Both mint the host key the same way.
pub(crate) async fn cli_redeem(
    State(state): State<WebState>,
    Json(req): Json<RedeemRequest>,
) -> Response {
    let invalid = |m: &str| redeem_error(StatusCode::BAD_REQUEST, "invalid_grant", m);
    if req.code.len() != 64 {
        return paste_redeem(state, req).await;
    }
    if !req.code.bytes().all(|b| b.is_ascii_hexdigit()) {
        return invalid("unknown code");
    }
    let key = digest(&req.code);
    let taken = match state.store(move |s| s.take_cli_code(&key)).await {
        Ok(t) => t,
        Err(e) => return store_failed(e),
    };
    let Some(code) = taken.filter(|c| c.paste.is_none()) else {
        return invalid("unknown or already used code");
    };
    if state.now() - code.created_at > state.limits().cli_code_ttl {
        return invalid("the code expired; run conde login again");
    }
    if !same_secret(&req.state, &code.state) {
        return invalid("the state does not match this code");
    }
    if !(43..=128).contains(&req.code_verifier.len()) {
        return invalid("bad code_verifier");
    }
    if !pkce_matches(req.code_verifier, &code.challenge) {
        return invalid("the code_verifier does not match the challenge");
    }
    mint_for(&state, code, &req.host).await
}

/// The pasted-code redeem: the login is found by the CLI run's state and must be signed in
/// (else `authorization_pending`) and live; the typed code and the PKCE verifier must both
/// match. A miss counts as a try and
/// puts the login back; the [`PASTE_TRIES`]th ends it.
async fn paste_redeem(state: WebState, req: RedeemRequest) -> Response {
    let invalid = |m: &str| redeem_error(StatusCode::BAD_REQUEST, "invalid_grant", m);
    let Some(typed) = paste_normalized(&req.code) else {
        return redeem_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "a sign-in code is 8 letters and digits, as the page shows it",
        );
    };
    if !url_safe(&req.state, 16, 128) {
        return invalid("bad state");
    }
    if host_slug(&req.host).is_none() {
        return redeem_error(StatusCode::BAD_REQUEST, "invalid_request", "bad host name");
    }
    let key = paste_key(&req.state);
    let lookup = key.clone();
    let taken = match state.store(move |s| s.take_cli_code(&lookup)).await {
        Ok(t) => t,
        Err(e) => return store_failed(e),
    };
    let Some(mut code) = taken.filter(|c| c.paste.is_some()) else {
        return redeem_error(
            StatusCode::BAD_REQUEST,
            "authorization_pending",
            "no signed-in login waits for this run: open its URL, sign in, then enter the code that page shows",
        );
    };
    if state.now() - code.created_at > state.limits().cli_code_ttl {
        return invalid("the code expired; run conde login again");
    }
    let right_code = code
        .paste
        .as_deref()
        .is_some_and(|d| same_secret(&paste_digest(&req.state, &typed), d));
    let right_run = same_secret(&req.state, &code.state) && pkce_matches(req.code_verifier.clone(), &code.challenge);
    if right_code && right_run {
        return mint_for(&state, code, &req.host).await;
    }
    code.tries += 1;
    let left = PASTE_TRIES.saturating_sub(code.tries);
    tracing::info!(user = %code.user_name, tries = code.tries, "wrong conde login code");
    if left == 0 {
        return invalid("wrong code, and that was the last try; run conde login again");
    }
    if let Err(e) = state.store(move |s| s.insert_cli_code(&key, &code)).await {
        return store_failed(e);
    }
    let mut r = (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": "invalid_code",
            "message": format!("wrong code; {left} {} left", if left == 1 { "try" } else { "tries" }),
            "tries_left": left,
        })),
    )
        .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Mints the host key a redeemed login asked for.
async fn mint_for(state: &WebState, code: CliCode, host: &str) -> Response {
    let Some(base) = host_slug(host) else {
        return redeem_error(StatusCode::BAD_REQUEST, "invalid_request", "bad host name");
    };
    let label = host.trim().chars().take(64).collect::<String>();
    match keys::mint(state.core(), code.user_id, base, label, keys::HOST_SUFFIXES).await {
        Ok(minted) => {
            let mut r = Json(RedeemResponse {
                host_id: minted.host_id,
                key: minted.key,
                user: code.user_name,
                reenrolled: !minted.new_host,
            })
            .into_response();
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(keys::MintError::HostTaken) => redeem_error(
            StatusCode::CONFLICT,
            "host_taken",
            "every host id derived from this host name belongs to another user; pass --host",
        ),
        Err(keys::MintError::Disabled) => redeem_error(
            StatusCode::FORBIDDEN,
            "disabled",
            "this account is disabled",
        ),
        Err(keys::MintError::Store(e)) => {
            tracing::error!(error = ?e, "cli key mint failed");
            redeem_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "minting failed",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(upn: Option<&str>, name: Option<&str>) -> Identity {
        Identity {
            tid: "t".into(),
            oid: "o".into(),
            display_name: name.map(Into::into),
            preferred_username: upn.map(Into::into),
        }
    }

    #[test]
    fn owner_names() {
        assert_eq!(
            base_name(&identity(Some("Krisztian.Dobmayer@dakai.hu"), None)),
            "krisztian-dobmayer"
        );
        assert_eq!(
            base_name(&identity(None, Some("Dávid Pacsuta"))),
            "d-vid-pacsuta"
        );
        assert_eq!(base_name(&identity(None, None)), "user");
    }

    #[test]
    fn return_paths_stay_local() {
        assert_eq!(local_path(Some("/auth/cli?port=1")), "/auth/cli?port=1");
        for bad in ["//evil.example", "https://evil.example", "/\\evil", "keys"] {
            assert_eq!(local_path(Some(bad)), "/keys", "{bad}");
        }
        assert_eq!(local_path(None), "/keys");
    }
}
