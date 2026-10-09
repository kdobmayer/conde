//! Server-rendered pages: askama templates (auto-escaped), one stylesheet, and one script
//! (the copy button) allowed by a per-response CSP nonce.

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::HostKey;
use super::session::random_token;

const CSS: &str = r#":root{--bg:#fafaf9;--fg:#1c1917;--muted:#78716c;--line:#e7e5e4;--accent:#1d4ed8;--on-accent:#fff;--danger:#b91c1c;--code:#f0efed;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root{--bg:#1c1917;--fg:#f5f5f4;--muted:#a8a29e;--line:#44403c;--accent:#93c5fd;--on-accent:#1c1917;--danger:#fca5a5;--code:#292524}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
header{display:flex;align-items:center;gap:12px;padding:12px 16px;border-bottom:1px solid var(--line)}
header form{margin:0}
.brand{font-weight:600;color:inherit;text-decoration:none;margin-right:auto}
.who{color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
main{max-width:880px;margin:0 auto;padding:24px 16px}
h1{font-size:22px;font-weight:600;margin:0 0 12px}
a{color:var(--accent)}
.wrap{overflow-x:auto}
table{width:100%;border-collapse:collapse;margin:20px 0}
th,td{text-align:left;padding:8px 6px;border-bottom:1px solid var(--line);vertical-align:middle;white-space:nowrap}
th{font-weight:500;color:var(--muted);font-size:13px}
form.row{display:flex;gap:8px;max-width:480px;margin:16px 0}
form.inline{margin:0}
input[type=text]{font:inherit;flex:1;min-width:0;padding:6px 8px;border:1px solid var(--line);border-radius:6px;background:transparent;color:inherit}
button{font:inherit;padding:6px 12px;border-radius:6px;border:1px solid var(--accent);background:var(--accent);color:var(--on-accent);cursor:pointer}
button.quiet{background:transparent;color:var(--fg);border-color:var(--line)}
button.danger{background:transparent;color:var(--danger);border-color:var(--danger);padding:3px 10px}
.muted{color:var(--muted)}
code,pre{font:13px/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:var(--code);border-radius:6px}
code{padding:1px 5px}
pre{padding:10px 12px;white-space:pre-wrap;word-break:break-all}
.notice{border-left:3px solid var(--accent);padding:8px 12px;background:var(--code);border-radius:0 6px 6px 0}"#;

const COPY_JS: &str = "document.getElementById('copy').addEventListener('click',function(){var b=this;\
navigator.clipboard.writeText(document.getElementById('key').textContent.trim()).then(function(){b.textContent='Copied'})});";

/// The signed-in header: who, and the sign-out form.
pub(crate) struct Header<'a> {
    pub display: &'a str,
    pub csrf: &'a str,
}

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{ title }} · conde</title>
<style>{{ css|safe }}</style>
</head>
<body>
<header>
<a class="brand" href="/keys">conde</a>
{% if let Some(h) = header %}<span class="who">{{ h.display }}</span>
<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{{ h.csrf }}"><button class="quiet">Sign out</button></form>{% endif %}
</header>
<main>
{{ body|safe }}
</main>
{% if let Some(n) = nonce %}<script nonce="{{ n }}">{{ js|safe }}</script>{% endif %}
</body>
</html>
"#
)]
struct Layout<'a> {
    title: &'a str,
    css: &'a str,
    header: Option<Header<'a>>,
    body: &'a str,
    nonce: Option<&'a str>,
    js: &'a str,
}

/// One credential row of the keys page.
pub(crate) struct KeyRow {
    host_id: String,
    label: String,
    created: String,
    last_seen: String,
    revoked: Option<String>,
}

impl From<&HostKey> for KeyRow {
    fn from(k: &HostKey) -> KeyRow {
        KeyRow {
            host_id: k.host_id.clone(),
            label: k.label.clone().unwrap_or_default(),
            created: k.created_at.clone().unwrap_or_else(|| "unknown".into()),
            last_seen: k.last_seen.clone().unwrap_or_else(|| "never".into()),
            revoked: k.revoked_at.clone(),
        }
    }
}

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<h1>Host keys</h1>
<p class="muted">One key per machine. The bridge on that machine holds it, and every Claude and Codex session it finds acts as you. Revoking a key cuts that machine off at once.</p>
<form method="post" action="/keys" class="row">
<input type="hidden" name="csrf" value="{{ csrf }}">
<input type="text" name="label" required maxlength="64" placeholder="Machine name, e.g. work-laptop" aria-label="Machine name">
<button>Create key</button>
</form>
{% if rows.is_empty() %}<p class="muted">No host keys yet. Create one above, or run <code>conde login --server {{ server }}</code> on the machine.</p>
{% else %}<div class="wrap"><table>
<thead><tr><th>Host</th><th>Label</th><th>Created</th><th>Last seen</th><th>State</th><th></th></tr></thead>
<tbody>
{% for k in rows %}<tr>
<td><code>{{ k.host_id }}</code></td><td>{{ k.label }}</td><td>{{ k.created }}</td><td>{{ k.last_seen }}</td>
{% if let Some(at) = k.revoked %}<td class="muted">revoked {{ at }}</td><td></td>
{% else %}<td>live</td><td><form method="post" action="/keys/{{ k.host_id|urlencode }}/revoke" class="inline"><input type="hidden" name="csrf" value="{{ csrf }}"><button class="danger">Revoke</button></form></td>{% endif %}
</tr>
{% endfor %}</tbody>
</table></div>{% endif %}"#
)]
struct KeysBody<'a> {
    csrf: &'a str,
    server: &'a str,
    rows: Vec<KeyRow>,
}

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<h1>Key for {{ host_id }}</h1>
<p class="notice">Shown once. Copy it now: the server keeps only its digest and cannot show it again.</p>
<pre id="key">{{ key }}</pre>
<p><button type="button" id="copy">Copy key</button></p>
{% if replaced %}<p>The previous key of this host was revoked and its bridge disconnected.</p>{% endif %}
<p>On that machine, run this and paste the key at the prompt:</p>
<pre>conde enroll --server {{ server }} --host {{ host_id }}</pre>
<p><a href="/keys">Back to host keys</a></p>"#
)]
struct CreatedBody<'a> {
    host_id: &'a str,
    key: &'a str,
    server: &'a str,
    replaced: bool,
}

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<h1>{{ title }}</h1>
<p>{{ message }}</p>
<p><a href="/keys">Back to host keys</a></p>"#
)]
struct ErrorBody<'a> {
    title: &'a str,
    message: &'a str,
}

fn render(t: &impl Template) -> String {
    t.render().unwrap_or_else(|e| {
        tracing::error!(error = %e, "template failed");
        String::from("<p>The page could not be rendered.</p>")
    })
}

/// A complete page with the security headers. `form_target` is an extra origin forms may
/// end up at (the identity provider, after sign-out).
fn page(
    status: StatusCode,
    title: &str,
    header: Option<Header<'_>>,
    body: &str,
    with_script: bool,
    form_target: Option<&str>,
) -> Response {
    let nonce = with_script.then(random_token);
    let html = render(&Layout {
        title,
        css: CSS,
        header,
        body,
        nonce: nonce.as_deref(),
        js: COPY_JS,
    });
    let script = nonce
        .as_deref()
        .map(|n| format!("'nonce-{n}'"))
        .unwrap_or_else(|| "'none'".into());
    let forms = match form_target {
        Some(origin) => format!("'self' {origin}"),
        None => "'self'".into(),
    };
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; script-src {script}; img-src 'self'; \
         form-action {forms}; frame-ancestors 'none'; base-uri 'none'"
    );
    let mut response = (status, html).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    response
}

pub(crate) fn keys(header: Header<'_>, server: &str, keys: &[HostKey], idp: &str) -> Response {
    let body = render(&KeysBody {
        csrf: header.csrf,
        server,
        rows: keys.iter().map(KeyRow::from).collect(),
    });
    page(
        StatusCode::OK,
        "Host keys",
        Some(header),
        &body,
        false,
        Some(idp),
    )
}

pub(crate) fn created(
    header: Header<'_>,
    server: &str,
    host_id: &str,
    key: &str,
    replaced: bool,
    idp: &str,
) -> Response {
    let body = render(&CreatedBody {
        host_id,
        key,
        server,
        replaced,
    });
    page(
        StatusCode::OK,
        "New host key",
        Some(header),
        &body,
        true,
        Some(idp),
    )
}

/// A public document page (no header, no script) in the keys page's style.
pub(crate) fn document(title: &str, body: &str) -> Response {
    page(StatusCode::OK, title, None, body, false, None)
}

pub(crate) fn error(status: StatusCode, title: &str, message: &str) -> Response {
    let body = render(&ErrorBody { title, message });
    page(status, title, None, &body, false, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_escaped() {
        let key = HostKey {
            key_id: "k".into(),
            host_id: "a<b".into(),
            label: Some("<script>alert(1)</script>".into()),
            created_at: None,
            last_seen: None,
            revoked_at: None,
        };
        let body = render(&KeysBody {
            csrf: "c",
            server: "https://s",
            rows: vec![KeyRow::from(&key)],
        });
        assert!(!body.contains("<script>"), "{body}");
        assert!(
            body.contains("&#60;script&#62;") || body.contains("&lt;script&gt;"),
            "{body}"
        );
        assert!(body.contains("/keys/a%3Cb/revoke"), "{body}");
    }
}
