//! OIDC sign-in (authorization code + PKCE, state and nonce, confidential client) against the
//! configured issuer, the invite gate, and the browser leg of `conde login`.

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
use super::ratelimit::ClientIp;
use super::session::{
    self, WebError, digest, login_cookie, login_retry_cookie, random_token, removal, same_secret,
};
use super::{CliCode, CliIssued, Identity, Invite, LoginTxn, PASTE_TRIES, WebConfig, WebState, WebUser, pages};

/// Discovery and JWKS are re-read after this long (and at once when a token's key is unknown).
const METADATA_MAX_AGE: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The Entra claims an Entra identity is keyed on: the tenant and the directory object (`sub`
/// is pairwise per app).
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

/// The id token's `iss` must be the configured issuer, character for character (OIDC Core 3.1.3.7).
fn same_issuer(token: &str, configured: &str) -> Result<(), OidcError> {
    if token == configured {
        Ok(())
    } else {
        Err(OidcError::Verify(format!(
            "the id token's issuer '{token}' is not the configured issuer '{configured}'"
        )))
    }
}

/// The Entra identity adapter: the subject an identity is bound by. Entra's `sub` is pairwise per
/// app, so an Entra token (from the configured Entra tenant, or any token carrying `tid` and
/// `oid`) binds by `oid`, the directory object an admin knows and migration 0017 kept; any other
/// token binds by `sub`. A token of another tenant than the configured one is refused.
pub(crate) fn binding_subject(
    entra_tenant: Option<&str>,
    sub: &str,
    tid: Option<&str>,
    oid: Option<&str>,
) -> Result<String, OidcError> {
    match (entra_tenant, tid, oid) {
        (Some(tenant), Some(tid), _) if tid != tenant => Err(OidcError::Verify(format!(
            "the id token's tenant '{tid}' is not the configured tenant '{tenant}'"
        ))),
        (Some(_), _, None) => Err(OidcError::Verify("the id token has no oid claim".into())),
        (Some(_), _, Some(oid)) | (None, Some(_), Some(oid)) => Ok(oid.to_string()),
        (None, ..) => Ok(sub.to_string()),
    }
}

/// The OIDC relying party: provider metadata cached in memory (re-readable at any time, so
/// the server stays stateless), the confidential client built from it per request.
pub struct Oidc {
    issuer: String,
    /// The tenant, when the issuer is an Entra tenant.
    entra_tenant: Option<String>,
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
            entra_tenant: config.entra_tenant.clone(),
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
        // client_secret_post: the secret goes in the form body (Entra documents it; Keycloak takes it).
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
    /// the JWKS, issuer, audience, expiry, nonce). Discovery was read from the configured issuer
    /// and refuses metadata naming another, and the library checks `iss` against the metadata;
    /// [`same_issuer`] checks it against the configuration once more.
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
        same_issuer(claims.issuer().as_str(), &self.issuer)?;
        let extra = claims.additional_claims();
        let subject = binding_subject(
            self.entra_tenant.as_deref(),
            claims.subject().as_str(),
            extra.tid.as_deref(),
            extra.oid.as_deref(),
        )?;
        let upn = claims.preferred_username().map(|u| u.as_str());
        let email = claims.email().map(|e| e.as_str());
        Ok(Identity {
            issuer: self.issuer.clone(),
            subject,
            display_name: claims
                .name()
                .and_then(|n| n.get(None))
                .map(|n| n.as_str().to_string()),
            preferred_username: upn.map(str::to_string),
            email: lookup_email(upn, email),
            verified_email: verified_email(self.entra_tenant.is_some(), upn, email, claims.email_verified()),
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

/// Sign-ins one browser may have pending at once (tabs, a page's concurrent 401s); a newer one
/// drops the oldest.
const MAX_PENDING: usize = 4;
/// Joins the pending states in the login cookie; a state is base64url, so never contains it.
const STATE_SEP: &str = ".";
/// The longest entry taken for a state: a real one is 22 characters (16 random bytes, base64url).
const MAX_STATE: usize = 64;
/// The longest login cookie read at all; a longer one is ignored unsplit.
const MAX_COOKIE: usize = MAX_PENDING * (MAX_STATE + 1);
/// How long the marker of a stale callback's automatic sign-in lives.
const RETRY_MARK: time::Duration = time::Duration::minutes(5);

/// The states of the sign-ins this browser has pending, oldest first: at most the newest
/// [`MAX_PENDING`], none from an oversized cookie.
fn pending_states(jar: &CookieJar, secure: bool) -> Vec<String> {
    let Some(cookie) = jar.get(login_cookie(secure)) else {
        return Vec::new();
    };
    let value = cookie.value();
    if value.len() > MAX_COOKIE {
        return Vec::new();
    }
    let states: Vec<String> = value
        .split(STATE_SEP)
        .filter(|s| !s.is_empty() && s.len() <= MAX_STATE)
        .map(String::from)
        .collect();
    states[states.len().saturating_sub(MAX_PENDING)..].to_vec()
}

/// The jar with the newest [`MAX_PENDING`] of `states` as the login cookie, or without the
/// cookie when none remain.
fn with_pending(jar: CookieJar, states: &[String], secure: bool) -> CookieJar {
    let states = &states[states.len().saturating_sub(MAX_PENDING)..];
    if states.is_empty() {
        jar.remove(removal(login_cookie(secure), "/", secure))
    } else {
        jar.add(session::cookie(
            login_cookie(secure),
            states.join(STATE_SEP),
            secure,
        ))
    }
}

/// The longest path a sign-in carries back (it is stored with the login until the callback).
const MAX_NEXT: usize = 2048;

/// A local path to return to after sign-in; anything else becomes `/keys`.
fn local_path(next: Option<&str>) -> String {
    match next {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && p.len() <= MAX_NEXT
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
    /// Set by a stale callback's one retry, which also set the retry marker.
    retry: Option<String>,
}

/// `GET /auth/login?next=/path`: starts the authorization code flow.
pub(crate) async fn login(
    State(state): State<WebState>,
    jar: CookieJar,
    Query(q): Query<LoginQuery>,
) -> Result<Response, WebError> {
    // Refused, not dropped: a page that sent here (an app's authorization request) would
    // otherwise end on /keys with no word of why.
    if q.next.as_deref().is_some_and(|n| n.len() > MAX_NEXT) {
        return Err(WebError::BadRequest(format!(
            "The page to return to after sign-in is longer than {MAX_NEXT} characters, so sign-in cannot carry it back. If an app sent you here, its request is too long."
        )));
    }
    let secure = state.config().secure_cookies();
    if q.retry.is_some() && jar.get(login_retry_cookie(secure)).is_none() {
        return Err(WebError::BadRequest(
            "This sign-in link is old, or this browser is not keeping conde's cookies, so sign-in cannot finish. Go to the board to start again; if this page comes back, allow cookies for this site."
                .into(),
        ));
    }
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
    let mut pending = pending_states(&jar, secure);
    pending.push(begin.state);
    let jar = with_pending(jar, &pending, secure);
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
    let mut pending = pending_states(&jar, secure);
    let started = q
        .state
        .as_deref()
        .and_then(|returned| pending.iter().position(|p| same_secret(p, returned)));
    if let Some(i) = started {
        pending.remove(i);
    }
    let jar = with_pending(jar, &pending, secure);
    if let Some(error) = q.error {
        let detail = q.error_description.unwrap_or(error);
        return Ok((
            jar,
            WebError::Forbidden(format!("The identity provider refused the sign-in: {detail}")),
        )
            .into_response());
    }
    let (Some(code), Some(returned_state)) = (q.code, q.state) else {
        return Err(WebError::BadRequest(
            "The sign-in response is incomplete.".into(),
        ));
    };
    if started.is_none() {
        return stale(
            &state,
            jar,
            "This sign-in was started in another browser or has already finished. Start again.",
            "/",
        )
        .await;
    }
    let key = digest(&returned_state);
    let txn = state.store(move |s| s.take_login(&key)).await?;
    let now = state.now();
    let expired = "This sign-in has expired. Start again.";
    let Some(txn) = txn else {
        return stale(&state, jar, expired, "/").await;
    };
    if now - txn.created_at > state.limits().login_ttl {
        return stale(&state, jar, expired, &txn.return_to).await;
    }
    let identity = state
        .0
        .oidc
        .finish(&code, &txn.pkce_verifier, &txn.nonce)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "sign-in refused");
            WebError::Forbidden("The sign-in could not be verified. Start again.".into())
        })?;
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
    let jar = jar.remove(removal(login_retry_cookie(secure), "/", secure));
    Ok((jar, Redirect::to(&txn.return_to)).into_response())
}

/// A callback whose sign-in is not pending here any more (another browser's, already finished,
/// or expired), as after a tab restore: a signed-in browser goes to the board, any other starts
/// one fresh sign-in back to `next`; a second such callback in a row shows `message` instead of
/// looping.
async fn stale(
    state: &WebState,
    jar: CookieJar,
    message: &str,
    next: &str,
) -> Result<Response, WebError> {
    if session::current(state, &jar).await?.is_some() {
        return Ok((jar, Redirect::to("/")).into_response());
    }
    let secure = state.config().secure_cookies();
    let retry = login_retry_cookie(secure);
    if jar.get(retry).is_some() {
        let jar = jar.remove(removal(retry, "/", secure));
        return Ok((jar, WebError::BadRequest(message.into())).into_response());
    }
    let mut mark = session::cookie(retry, "1".into(), secure);
    mark.set_max_age(RETRY_MARK);
    let next: String =
        openidconnect::url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
    let login = format!("/auth/login?next={next}&retry=1");
    Ok((jar.add(mark), Redirect::to(&login)).into_response())
}

/// The address the Slack worker looks the person up by: preferred_username (the UPN, which the
/// tenant controls), else the `email` claim, which Microsoft documents as mutable and unverified,
/// only when the UPN is not an address.
pub(crate) fn lookup_email(upn: Option<&str>, email: Option<&str>) -> Option<String> {
    let address = |v: Option<&str>| v.map(str::trim).filter(|v| v.contains('@')).map(str::to_string);
    address(upn).or_else(|| address(email))
}

/// The address the invite gate matches: on Entra the UPN when it is an address (the tenant
/// controls it), else the `email` claim when the provider marks it verified.
pub(crate) fn verified_email(
    entra: bool,
    upn: Option<&str>,
    email: Option<&str>,
    email_verified: Option<bool>,
) -> Option<String> {
    let address = |v: Option<&str>| v.map(str::trim).filter(|v| v.contains('@')).map(str::to_string);
    let upn = if entra { address(upn) } else { None };
    upn.or_else(|| address(email).filter(|_| email_verified == Some(true)))
}

/// An email for the log: the first character of the local part, then the domain.
fn masked(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => format!("{}***@{domain}", local.chars().next().unwrap_or('*')),
        None => "***".into(),
    }
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

const NOT_INVITED: &str = "This account is not invited to this board. Ask the board admin.";

/// The board user of the identity. An identity bound to no user is let in only by the invite
/// gate: its verified email must be the email of exactly one user, who holds no identity of
/// this issuer yet; that user is then bound to it.
async fn resolve_user(state: &WebState, identity: Identity) -> Result<WebUser, WebError> {
    let (issuer, email) = (identity.issuer.clone(), identity.verified_email.clone());
    let outcome = state
        .store(move |s| {
            if let Some(user) = s.user_by_identity(&identity.issuer, &identity.subject)? {
                s.set_profile(user.user_id, &identity)?;
                return Ok(Ok(user));
            }
            let Some(email) = identity.verified_email.clone() else {
                return Ok(Err("no verified email"));
            };
            Ok(match s.claim_invite(&identity, &email)? {
                Invite::Bound(user) => {
                    tracing::info!(user = %user.name, issuer = %identity.issuer, "invite claimed");
                    Ok(user)
                }
                Invite::NoUser => Err("no user has the email"),
                Invite::Ambiguous => Err("more than one user has the email"),
                Invite::IssuerBound => Err("the user is bound to another identity of this issuer"),
            })
        })
        .await?;
    outcome.map_err(|reason| {
        let email = email.as_deref().map_or_else(|| "none".to_string(), masked);
        tracing::warn!(%issuer, %email, reason, "sign-in of an uninvited identity refused");
        WebError::Forbidden(NOT_INVITED.into())
    })
}

#[derive(Deserialize)]
pub(crate) struct LogoutForm {
    #[serde(default)]
    csrf: String,
}

/// `POST /auth/logout`: ends the board session, then the provider's session.
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
/// account issues a new code (the old one dies); a login another account confirmed stays theirs;
/// a login first confirmed more than the code TTL ago expired, for every account.
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
    let ttl = state.limits().cli_code_ttl;
    match state.store(move |s| s.issue_cli_code(&key, &record, ttl)).await? {
        CliIssued::Issued => {}
        CliIssued::Expired => {
            return Err(WebError::BadRequest("This conde login expired. Run conde login again.".into()));
        }
        CliIssued::Theirs => {
            return Err(WebError::Forbidden(
                "Another account already signed in for this conde login. Run conde login again.".into(),
            ));
        }
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
    /// The host existed before (its earlier keys, if any were live, are revoked).
    #[serde(default)]
    pub reenrolled: bool,
    /// Live keys of the host were revoked by this mint (absent from an older server).
    #[serde(default)]
    pub replaced: bool,
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

/// The redeem's 429: `retry_after` in the body and the header; `login_expires_first` when the
/// waiting login expires before the next allowed attempt (the CLI then ends the run).
fn redeem_limited(wait: u64, message: String, login_expires_first: bool) -> Response {
    let mut body = json!({"error": "rate_limited", "message": message, "retry_after": wait});
    if login_expires_first {
        body["login_expires_first"] = json!(true);
    }
    let mut r = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
        h.insert(header::RETRY_AFTER, v);
    }
    r
}

/// `POST /auth/cli/redeem` `{code, state, code_verifier, host}`: a 64-hex `code` is the
/// loopback's, taken whatever the outcome, then expiry, state and PKCE are checked; any other
/// `code` is a pasted one (see [`paste_redeem`]). Both mint the host key the same way.
pub(crate) async fn cli_redeem(
    State(state): State<WebState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<RedeemRequest>,
) -> Response {
    if let Err(wait) = state.0.limits.cli_redeem(ip) {
        tracing::info!(client_ip = %ip, wait, endpoint = "cli redeem", "rate limited");
        return redeem_limited(wait, format!("too many sign-in attempts from this address; retry in {wait} s"), false);
    }
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
/// (else `authorization_pending`) and live. A request without the run's PKCE verifier (the state
/// alone is in the URL) is refused and spends nothing. With it, the account's attempt budget is
/// spent, then the typed code must match: a miss counts as a try and puts the login back; the
/// [`PASTE_TRIES`]th ends it.
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
    let age = state.now() - code.created_at;
    let ttl = state.limits().cli_code_ttl;
    if age > ttl {
        // ended like a last try: another account's record issued since the take goes too
        let user_id = code.user_id;
        if let Err(e) = state.store(move |s| s.end_cli_code(&key, user_id)).await {
            return store_failed(e);
        }
        return invalid("the code expired; run conde login again");
    }
    // The state is in the URL anyone may hold; the verifier never leaves the CLI's host. A
    // request without it is not this run's: the record goes back untouched, and neither the
    // per-code tries nor the account's budget is spent.
    let right_run = same_secret(&req.state, &code.state) && pkce_matches(req.code_verifier.clone(), &code.challenge);
    if !right_run {
        tracing::info!(user = %code.user_name, "conde login code redeem without this run's verifier");
        if let Err(e) = state.store(move |s| s.restore_cli_code(&key, &code)).await {
            return store_failed(e);
        }
        return invalid("this request is not from the conde login run that started this sign-in");
    }
    // Per account, across re-issued codes, for attempts that hold the run's verifier: the record
    // goes back untouched (no try spent).
    if let Err(wait) = state.0.limits.cli_guess(code.user_id) {
        tracing::info!(user = %code.user_name, wait, "conde login code attempts rate limited");
        let left = ttl - age;
        if let Err(e) = state.store(move |s| s.restore_cli_code(&key, &code)).await {
            return store_failed(e);
        }
        let expires_first = wait >= left as u64;
        let message = if expires_first {
            format!(
                "too many code attempts for this account; the next is allowed in {wait} s, after this login expires (in {left} s): run conde login --code again then"
            )
        } else {
            format!("too many code attempts for this account; retry in {wait} s")
        };
        return redeem_limited(wait, message, expires_first);
    }
    let right_code = code
        .paste
        .as_deref()
        .is_some_and(|d| same_secret(&paste_digest(&req.state, &typed), d));
    if right_code {
        return mint_for(&state, code, &req.host).await;
    }
    code.tries += 1;
    let left = PASTE_TRIES.saturating_sub(code.tries);
    tracing::info!(user = %code.user_name, tries = code.tries, "wrong conde login code");
    if left == 0 {
        let user_id = code.user_id;
        if let Err(e) = state.store(move |s| s.end_cli_code(&key, user_id)).await {
            return store_failed(e);
        }
        return invalid("wrong code, and that was the last try; run conde login again");
    }
    if let Err(e) = state.store(move |s| s.restore_cli_code(&key, &code)).await {
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
                replaced: minted.replaced > 0,
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
            issuer: "https://auth.example/realms/r".into(),
            subject: "s".into(),
            display_name: name.map(Into::into),
            preferred_username: upn.map(Into::into),
            email: upn.map(Into::into),
            verified_email: None,
        }
    }

    #[test]
    fn the_token_issuer_must_be_the_configured_one_exactly() {
        let realm = "https://auth.conde.cloud/realms/dakai";
        assert!(same_issuer(realm, realm).is_ok());
        for other in [
            "https://auth.conde.cloud/realms/demo",
            "https://auth.conde.cloud/realms/dakai/",
            "https://AUTH.conde.cloud/realms/dakai",
            "https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0",
        ] {
            let e = same_issuer(other, realm).unwrap_err().to_string();
            assert_eq!(e, format!("id token rejected: the id token's issuer '{other}' is not the configured issuer '{realm}'"));
        }
    }

    #[test]
    fn entra_binds_by_oid_and_any_other_issuer_by_sub() {
        const T: &str = "2e51b484-b008-4fc0-8f70-555c1317b555";
        let subject = |tenant, tid, oid| binding_subject(tenant, "pairwise-sub", tid, oid).map_err(|e| e.to_string());
        // Entra: the oid, never the per-app sub; a migrated (tid, oid) row is (issuer, oid).
        assert_eq!(subject(Some(T), Some(T), Some("oid-1")), Ok("oid-1".into()));
        assert_eq!(subject(Some(T), None, Some("oid-1")), Ok("oid-1".into()));
        // a token carrying tid and oid is Entra's whatever the issuer
        assert_eq!(subject(None, Some(T), Some("oid-1")), Ok("oid-1".into()));
        // Keycloak (or any other issuer): the sub
        assert_eq!(subject(None, None, None), Ok("pairwise-sub".into()));
        assert_eq!(subject(None, None, Some("oid-1")), Ok("pairwise-sub".into()), "oid alone is no Entra token");
        // refused: another tenant, or an Entra token without oid
        assert_eq!(
            subject(Some(T), Some("other-tenant"), Some("oid-1")),
            Err(format!("id token rejected: the id token's tenant 'other-tenant' is not the configured tenant '{T}'"))
        );
        assert_eq!(subject(Some(T), Some(T), None), Err("id token rejected: the id token has no oid claim".into()));
    }

    #[test]
    fn the_verified_email_is_entras_upn_or_a_verified_email_claim() {
        // Entra: the UPN when it is an address; the email claim only when marked verified.
        assert_eq!(verified_email(true, Some("k@dakai.io"), Some("x@y.io"), None).as_deref(), Some("k@dakai.io"));
        assert_eq!(verified_email(true, Some("krisz"), Some("x@y.io"), None), None);
        assert_eq!(verified_email(true, Some("krisz"), Some("x@y.io"), Some(true)).as_deref(), Some("x@y.io"));
        // Any other issuer: never the UPN, only a verified email claim.
        assert_eq!(verified_email(false, Some("k@dakai.io"), None, None), None);
        assert_eq!(verified_email(false, Some("k@dakai.io"), Some("k@dakai.io"), Some(false)), None);
        assert_eq!(verified_email(false, Some("krisz"), Some(" k@dakai.io "), Some(true)).as_deref(), Some("k@dakai.io"));
        assert_eq!(verified_email(false, None, Some("krisz"), Some(true)), None);
        assert_eq!(masked("krisztian@dakai.io"), "k***@dakai.io");
    }

    #[test]
    fn the_lookup_email_is_the_upn_unless_it_is_no_address() {
        assert_eq!(lookup_email(Some("k@dakai.io"), Some("other@x.io")).as_deref(), Some("k@dakai.io"));
        assert_eq!(lookup_email(Some("krisz"), Some("other@x.io")).as_deref(), Some("other@x.io"));
        assert_eq!(lookup_email(None, Some("other@x.io")).as_deref(), Some("other@x.io"));
        assert_eq!(lookup_email(Some("krisz"), None), None);
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
        let longest = format!("/keys?{}", "a".repeat(MAX_NEXT - 6));
        assert_eq!(local_path(Some(&longest)), longest);
    }
}
