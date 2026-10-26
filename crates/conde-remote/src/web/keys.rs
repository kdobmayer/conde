//! The keys page (list, create shown once, revoke) and host-key minting shared with the
//! `conde login` redeem and the admin API. A key is one host's bearer token; the store keeps
//! its digest only.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{CookieJar, SameSite};
use serde::Deserialize;

use super::session::{self, Session, WebError, digest, random_token};
use super::{Core, HostKey, Minted, WebState, pages};

/// How many `-N` suffixes the browser flows try when a host id belongs to another user.
pub(crate) const HOST_SUFFIXES: u32 = 20;
/// The server's acceptance window (seconds) for a created key's one display after the
/// redirect. The cookie carries no Max-Age; it is cleared at the display.
pub(crate) const SHOW_WITHIN: i64 = 120;
const CREATED_PATH: &str = "/keys/created";

/// A new host key: 32 random bytes, hex, with the prefix the admin command uses.
pub fn new_host_key() -> String {
    format!("conde_{}", random_token())
}

/// A host id from a machine name or label: lowercase `[a-z0-9._-]`, other runs become `-`,
/// at most 63 characters; `None` when nothing usable remains.
pub(crate) fn host_slug(input: &str) -> Option<String> {
    let mut out = String::new();
    for c in input.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out
        .trim_matches(|c| c == '-' || c == '.')
        .chars()
        .take(63)
        .collect();
    let out = out.trim_end_matches(['-', '.']).to_string();
    (!out.is_empty()).then_some(out)
}

/// A minted key: shown once to the caller.
pub(crate) struct NewKey {
    pub host_id: String,
    pub key: String,
    /// Live keys of the same host that the new one replaced.
    pub replaced: u64,
    /// The host did not exist before this key (else it is a re-enrollment).
    pub new_host: bool,
}

/// Why a key was not minted.
#[derive(Debug)]
pub(crate) enum MintError {
    Disabled,
    /// Every candidate host id belongs to another user.
    HostTaken,
    Store(super::WebStoreError),
}

impl From<MintError> for WebError {
    fn from(e: MintError) -> WebError {
        match e {
            MintError::Disabled => WebError::Forbidden("This account is disabled.".into()),
            MintError::HostTaken => WebError::BadRequest(
                "Every host id derived from this name is taken by other users. Choose another name."
                    .into(),
            ),
            MintError::Store(e) => WebError::Store(e),
        }
    }
}

/// Mints a key for the user on `base` (or `base-2`.. up to `suffixes` candidates when another
/// user owns it), revoking the host's previous live keys and disconnecting its bridge when
/// there were any. A disabled user gets no key.
pub(crate) async fn mint(
    core: &Core,
    user_id: i64,
    base: String,
    label: String,
    suffixes: u32,
) -> Result<NewKey, MintError> {
    let key = new_host_key();
    let token_digest = digest(&key);
    let now = core.now();
    let minted = core
        .store(move |s| {
            for n in 1..=suffixes {
                let host = if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                if host == super::BROWSER_HOST
                    || host == crate::SLACK_INSTANCE
                    || s.host_owner(&host)?.is_some_and(|owner| owner != user_id)
                {
                    continue;
                }
                match s.mint_host_key(user_id, &host, &label, &token_digest, now)? {
                    Minted::Stored { replaced, new_host } => {
                        return Ok(Ok((host, replaced, new_host)));
                    }
                    Minted::Disabled => return Ok(Err(MintError::Disabled)),
                    Minted::HostTaken => continue,
                }
            }
            Ok(Err(MintError::HostTaken))
        })
        .await
        .map_err(MintError::Store)??;
    let (host_id, replaced, new_host) = minted;
    if replaced > 0 {
        core.registry.disconnect_host(&host_id);
    }
    tracing::info!(user_id, host = %host_id, replaced, new_host, "host key minted");
    Ok(NewKey {
        host_id,
        key,
        replaced,
        new_host,
    })
}

/// One row per host, in the newest-first order of `keys`: its live key, else its newest
/// (revoked) one. A re-enrolled host's revoked keys stay in the store, not on the page.
pub(crate) fn one_per_host(keys: Vec<HostKey>) -> Vec<HostKey> {
    let mut rows: Vec<HostKey> = Vec::new();
    for k in keys {
        match rows.iter_mut().find(|r| r.host_id == k.host_id) {
            Some(row) if row.revoked_at.is_some() && k.revoked_at.is_none() => *row = k,
            Some(_) => {}
            None => rows.push(k),
        }
    }
    rows
}

/// The identity provider's origin, which the sign-out form is allowed to reach.
pub(crate) fn idp_origin(state: &WebState) -> String {
    let issuer = &state.config().issuer;
    match issuer.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", rest.split('/').next().unwrap_or("")),
        None => issuer.clone(),
    }
}

pub(crate) fn header(session: &Session) -> pages::Header<'_> {
    pages::Header {
        display: &session.record.display,
        csrf: &session.record.csrf,
    }
}

pub(crate) fn to_login(next: &str) -> Response {
    let next: String =
        openidconnect::url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
    Redirect::to(&format!("/auth/login?next={next}")).into_response()
}

pub(crate) async fn page(
    State(state): State<WebState>,
    jar: CookieJar,
) -> Result<Response, WebError> {
    let Some(session) = session::current(&state, &jar).await? else {
        return Ok(to_login("/keys"));
    };
    let (user_id, now) = (session.record.user_id, state.now());
    let (keys, apps) = state.store(move |s| Ok((s.host_keys(user_id)?, s.mcp_grants(user_id, now)?))).await?;
    Ok(pages::keys(
        header(&session),
        &state.config().public_url,
        &one_per_host(keys),
        &apps,
        &idp_origin(&state),
    ))
}

#[derive(Deserialize)]
pub(crate) struct CreateForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    label: String,
}

pub(crate) fn signed_out() -> WebError {
    WebError::Forbidden("Your session has ended. Sign in again and retry.".into())
}

fn created_cookie(secure: bool) -> &'static str {
    if secure {
        "__Secure-conde_new_key"
    } else {
        "conde_new_key"
    }
}

/// `POST /keys`: mints the key, then redirects to its one display (POST-redirect-GET), so a
/// refresh shows nothing rather than minting again. The cookie, scoped to the display path,
/// carries a one-time display token and the key; the server keeps the display (user, host,
/// key digest, time) under the token's digest and accepts it once within [`SHOW_WITHIN`].
pub(crate) async fn create(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<CreateForm>,
) -> Result<Response, WebError> {
    let session = session::current(&state, &jar)
        .await?
        .ok_or_else(signed_out)?;
    session.check_csrf(&form.csrf)?;
    let label = form.label.trim().to_string();
    if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err(WebError::BadRequest(
            "A key needs a machine name of 1 to 64 characters.".into(),
        ));
    }
    let base = host_slug(&label).ok_or_else(|| {
        WebError::BadRequest("The machine name needs at least one letter or digit.".into())
    })?;
    let user_id = session.record.user_id;
    let minted = mint(state.core(), user_id, base, label, HOST_SUFFIXES).await?;
    let token = random_token();
    let display = super::KeyDisplay {
        user_id,
        host_id: minted.host_id,
        replaced: minted.replaced > 0,
        key_digest: digest(&minted.key),
        created_at: state.now(),
    };
    let key = digest(&token);
    state
        .store(move |s| s.insert_key_display(&key, &display))
        .await?;
    let secure = state.config().secure_cookies();
    let value = format!("{token}:{}", minted.key);
    let mut cookie = session::removal(created_cookie(secure), CREATED_PATH, secure);
    cookie.set_value(value);
    cookie.set_same_site(SameSite::Strict);
    Ok((jar.add(cookie), Redirect::to(CREATED_PATH)).into_response())
}

/// `GET /keys/created`: shows a just-created key once, to the session that created it.
pub(crate) async fn created(
    State(state): State<WebState>,
    jar: CookieJar,
) -> Result<Response, WebError> {
    let Some(session) = session::current(&state, &jar).await? else {
        return Ok(to_login("/keys"));
    };
    let secure = state.config().secure_cookies();
    let name = created_cookie(secure);
    let value = jar.get(name).map(|c| c.value().to_string());
    let mut gone = session::removal(name, CREATED_PATH, secure);
    gone.set_same_site(SameSite::Strict);
    let jar = jar.remove(gone);
    let Some((token, key)) = value.as_deref().and_then(|v| v.split_once(':')) else {
        return Ok((jar, Redirect::to("/keys")).into_response());
    };
    let token_digest = digest(token);
    let display = state
        .store(move |s| s.take_key_display(&token_digest))
        .await?;
    let now = state.now();
    let accepted = display.filter(|d| {
        d.user_id == session.record.user_id
            && (0..=SHOW_WITHIN).contains(&(now - d.created_at))
            && digest(key) == d.key_digest
    });
    let Some(display) = accepted else {
        return Ok((jar, Redirect::to("/keys")).into_response());
    };
    let (host, replaced) = (display.host_id, display.replaced);
    let page = pages::created(
        header(&session),
        &state.config().public_url,
        &host,
        key,
        replaced,
        &idp_origin(&state),
    );
    Ok((jar, page).into_response())
}

#[derive(Deserialize)]
pub(crate) struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

pub(crate) async fn revoke(
    State(state): State<WebState>,
    jar: CookieJar,
    Path(host): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, WebError> {
    let session = session::current(&state, &jar)
        .await?
        .ok_or_else(signed_out)?;
    session.check_csrf(&form.csrf)?;
    let (user_id, now, target) = (session.record.user_id, state.now(), host.clone());
    let revoked = state
        .store(move |s| s.revoke_host(user_id, &target, now))
        .await?;
    if revoked > 0 {
        state.core().registry.disconnect_host(&host);
        tracing::info!(user_id, %host, revoked, "host keys revoked");
    }
    Ok(Redirect::to("/keys").into_response())
}

/// `POST /keys/apps/{grant_id}/revoke`: ends one of the user's connected apps at once (its
/// access and refresh tokens with it).
pub(crate) async fn revoke_app(
    State(state): State<WebState>,
    jar: CookieJar,
    Path(grant_id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, WebError> {
    let session = session::current(&state, &jar)
        .await?
        .ok_or_else(signed_out)?;
    session.check_csrf(&form.csrf)?;
    let (user_id, now, target) = (session.record.user_id, state.now(), grant_id.clone());
    let revoked = state
        .store(move |s| s.revoke_mcp_grant(user_id, &target, now))
        .await?;
    if revoked {
        tracing::info!(user = %session.record.user_name, grant = %grant_id, by = "keys page", "mcp grant revoked");
    }
    Ok(Redirect::to("/keys").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_shows_one_row_per_host() {
        let key = |id: &str, host: &str, revoked: bool| HostKey {
            key_id: id.into(),
            host_id: host.into(),
            label: None,
            created_at: None,
            last_seen: None,
            revoked_at: revoked.then(|| "2026-10-11T06:08:51Z".into()),
        };
        // newest first: nuc re-enrolled twice, box revoked, an older live key under a revoked one
        let rows = one_per_host(vec![
            key("n3", "nuc", false),
            key("b1", "box", true),
            key("o2", "old", true),
            key("n2", "nuc", true),
            key("o1", "old", false),
            key("n1", "nuc", true),
        ]);
        let ids: Vec<&str> = rows.iter().map(|k| k.key_id.as_str()).collect();
        assert_eq!(ids, ["n3", "b1", "o1"]);
    }

    #[test]
    fn host_slugs() {
        assert_eq!(
            host_slug("Krisztians-MacBook-Pro.local").as_deref(),
            Some("krisztians-macbook-pro.local")
        );
        assert_eq!(
            host_slug("  work laptop / 2 ").as_deref(),
            Some("work-laptop-2")
        );
        assert_eq!(host_slug("--x--").as_deref(), Some("x"));
        assert_eq!(host_slug("é"), None);
        assert_eq!(host_slug(&"a".repeat(80)).map(|s| s.len()), Some(63));
        assert!(new_host_key().starts_with("conde_") && new_host_key().len() == 70);
    }
}
