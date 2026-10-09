//! Browser sessions: a random 256-bit id in an HttpOnly, SameSite=Lax cookie (Secure and
//! `__Host-` over https), stored server-side as its digest with idle and absolute expiry;
//! one CSRF token per session, checked on every POST.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

use super::{SessionRecord, WebState, WebStoreError, pages};

/// A session's activity is written back at most this often (seconds).
const TOUCH_EVERY: i64 = 60;

/// 32 random bytes as lowercase hex.
pub(crate) fn random_token() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The form every secret is stored and looked up in.
pub(crate) fn digest(secret: &str) -> String {
    crate::token_digest(secret)
}

/// Compares two secrets through their digests, so the comparison time says nothing about them.
pub(crate) fn same_secret(a: &str, b: &str) -> bool {
    digest(a) == digest(b)
}

pub(crate) fn session_cookie(secure: bool) -> &'static str {
    if secure {
        "__Host-conde_session"
    } else {
        "conde_session"
    }
}

pub(crate) fn login_cookie(secure: bool) -> &'static str {
    if secure {
        "__Host-conde_login"
    } else {
        "conde_login"
    }
}

pub(crate) fn cookie(name: &'static str, value: String, secure: bool) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .build()
}

/// A removal cookie for `name`, with the attributes it was issued with: a browser ignores a
/// `__Host-` or `__Secure-` deletion that lacks `Secure`.
pub(crate) fn removal(name: &'static str, path: &'static str, secure: bool) -> Cookie<'static> {
    Cookie::build((name, ""))
        .path(path)
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .build()
}

/// A request failure, rendered as a small HTML page.
#[derive(Debug)]
pub(crate) enum WebError {
    Store(WebStoreError),
    /// A failure outside the store (the identity provider unreachable); logged, not shown.
    Internal(String),
    BadRequest(String),
    Forbidden(String),
}

impl From<WebStoreError> for WebError {
    fn from(e: WebStoreError) -> WebError {
        WebError::Store(e)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, title, message) = match self {
            WebError::Store(e) => {
                tracing::error!(error = %e, "web store failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong",
                    "The server could not complete the request. Try again in a moment.".to_string(),
                )
            }
            WebError::Internal(e) => {
                tracing::error!(error = %e, "web request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "Sign-in is unavailable",
                    "The identity provider could not be reached. Try again in a moment."
                        .to_string(),
                )
            }
            WebError::BadRequest(m) => (StatusCode::BAD_REQUEST, "Request refused", m),
            WebError::Forbidden(m) => (StatusCode::FORBIDDEN, "Not allowed", m),
        };
        pages::error(status, title, &message)
    }
}

/// The caller's live session.
pub(crate) struct Session {
    pub id_digest: String,
    pub record: SessionRecord,
}

impl Session {
    pub fn check_csrf(&self, token: &str) -> Result<(), WebError> {
        if same_secret(token, &self.record.csrf) {
            Ok(())
        } else {
            Err(WebError::Forbidden(
                "The form was stale or did not come from this site. Reload the page and try again."
                    .into(),
            ))
        }
    }
}

/// The session named by the request's cookie, if it exists, has not expired and its user is
/// not disabled. Any other session is deleted; a live one has its activity time refreshed.
pub(crate) async fn current(
    state: &WebState,
    jar: &CookieJar,
) -> Result<Option<Session>, WebError> {
    let secure = state.config().secure_cookies();
    let Some(id) = jar
        .get(session_cookie(secure))
        .map(|c| c.value().to_string())
    else {
        return Ok(None);
    };
    if id.len() != 64 {
        return Ok(None);
    }
    let id_digest = digest(&id);
    let record = lookup(state, &id_digest, true).await?;
    Ok(record.map(|record| Session { id_digest, record }))
}

/// The live session stored under `id_digest`; an expired one, or one whose user is disabled,
/// is deleted. With `touch`, a live one has its activity time refreshed.
pub(crate) async fn lookup(
    state: &WebState,
    id_digest: &str,
    touch: bool,
) -> Result<Option<SessionRecord>, WebError> {
    let now = state.now();
    let limits = state.limits();
    let key = id_digest.to_string();
    let record = state
        .store(move |s| {
            let Some(record) = s.session(&key)? else {
                return Ok(None);
            };
            let idle = now - record.last_seen > limits.session_idle;
            let old = now - record.created_at > limits.session_max;
            if idle || old || s.user_disabled(record.user_id)? {
                s.delete_session(&key)?;
                return Ok(None);
            }
            if touch && now - record.last_seen >= TOUCH_EVERY {
                s.touch_session(&key, now)?;
            }
            Ok(Some(record))
        })
        .await?;
    Ok(record)
}

/// Starts a session for the user and returns the jar that sets its cookie.
pub(crate) async fn start(
    state: &WebState,
    jar: CookieJar,
    user_id: i64,
    user_name: String,
    display: String,
) -> Result<CookieJar, WebError> {
    let id = random_token();
    let now = state.now();
    let record = SessionRecord {
        user_id,
        user_name,
        display,
        csrf: random_token(),
        created_at: now,
        last_seen: now,
    };
    let key = digest(&id);
    let stored = state
        .store(move |s| s.insert_session(&key, &record))
        .await?;
    if !stored {
        return Err(WebError::Forbidden(
            "This account is disabled on the board. Ask the board admin.".into(),
        ));
    }
    let secure = state.config().secure_cookies();
    Ok(jar.add(cookie(session_cookie(secure), id, secure)))
}

/// Ends the session and returns the jar that drops its cookie.
pub(crate) async fn end(
    state: &WebState,
    jar: CookieJar,
    session: &Session,
) -> Result<CookieJar, WebError> {
    let key = session.id_digest.clone();
    state.store(move |s| s.delete_session(&key)).await?;
    let secure = state.config().secure_cookies();
    Ok(jar.remove(removal(session_cookie(secure), "/", secure)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_256_bit_hex_and_distinct() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(same_secret(&a, &a.clone()));
        assert!(!same_secret(&a, &b));
    }

    #[test]
    fn secure_cookie_attributes() {
        let c = cookie(session_cookie(true), "v".into(), true).to_string();
        assert!(c.starts_with("__Host-conde_session=v"), "{c}");
        for attr in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
            assert!(c.contains(attr), "{c} lacks {attr}");
        }
        assert!(!c.contains("Domain"), "{c}");
    }

    #[test]
    fn https_cookie_deletions_carry_the_issuance_attributes() {
        use axum::http::{HeaderMap, HeaderValue, header};
        use axum::response::IntoResponse;
        for name in [session_cookie(true), login_cookie(true)] {
            let mut headers = HeaderMap::new();
            let old = HeaderValue::from_str(&format!("{name}=old-value")).unwrap();
            headers.insert(header::COOKIE, old);
            let response = CookieJar::from_headers(&headers)
                .remove(removal(name, "/", true))
                .into_response();
            let set = response.headers()[header::SET_COOKIE].to_str().unwrap();
            for attr in ["Max-Age=0", "Path=/", "Secure", "HttpOnly", "SameSite=Lax"] {
                assert!(set.contains(attr), "{set} lacks {attr}");
            }
            assert!(!set.contains("Domain"), "{set}");
        }
    }
}
