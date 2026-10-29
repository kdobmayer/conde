//! Server-rendered pages: askama templates (auto-escaped), one stylesheet, and one script
//! (the copy button) allowed by a per-response CSP nonce. The favicons and the header mark come
//! from the embedded web app.

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::HostKey;
use super::session::random_token;

/// Design B's tokens and type from web/src/app.css, inlined (the CSP allows no stylesheet or
/// font source, so the fonts are the app's stacks without its bundled files). Dark only.
const CSS: &str = r#":root{--bg:#131518;--panel:#191c20;--soft:#20242a;--line:#2a2e34;--fg:#ebe6dc;--muted:#b8b2a7;--faint:#8e887e;--accent:#d4886b;--on-accent:#131518;--danger:#d4886b;--serif:'Source Serif 4','Source Serif Pro',Georgia,'Times New Roman',serif;--sans:Inter,system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 var(--sans);-webkit-text-size-adjust:100%}
a{color:var(--accent)}
a:focus-visible,button:focus-visible,input:focus-visible{outline:2px solid var(--accent);outline-offset:1px}
header{display:flex;align-items:center;gap:28px;padding:18px 32px;border-bottom:1px solid var(--line)}
header form{flex:none;margin:0}
header button{white-space:nowrap}
.brand{display:flex;align-items:center;gap:9px;font:600 24px/1 var(--serif);letter-spacing:-.01em;color:inherit;text-decoration:none}
.brand .mark{width:18px;height:18px}
header nav{display:flex;gap:24px;margin-right:auto}
header nav a{font:17px/1 var(--serif);color:var(--muted);text-decoration:none;padding:6px 0}
header nav a:hover,header nav a[aria-current]{color:var(--fg)}
header nav a[aria-current]{text-decoration:underline;text-decoration-color:var(--accent);text-decoration-thickness:2px;text-underline-offset:8px}
.who{min-width:0;font-size:13px;color:var(--faint);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
main{max-width:960px;margin:0 auto;padding:40px 32px 56px}
h1{font:600 34px/1.15 var(--serif);letter-spacing:-.01em;margin:0 0 14px;overflow-wrap:anywhere}
*+h1{margin-top:48px}
main>p{max-width:660px;font:17px/1.55 var(--serif)}
.muted{color:var(--muted)}
.wrap{overflow-x:auto}
table{width:100%;border-collapse:collapse;margin:24px 0}
th,td{text-align:left;padding:11px 10px;border-bottom:1px solid var(--line);vertical-align:middle}
th,td code,td:last-child{white-space:nowrap}
th:first-child,td:first-child{padding-left:0}
th:last-child,td:last-child{padding-right:0;text-align:right}
th{font:400 10.5px/1.4 var(--sans);letter-spacing:.08em;text-transform:uppercase;color:var(--faint)}
td{font-size:13.5px}
form.row{display:flex;gap:8px;max-width:480px;margin:20px 0}
form.inline{margin:0}
input[type=text]{font:inherit;flex:1;min-width:0;padding:7px 9px;border:1px solid var(--line);border-radius:6px;background:var(--panel);color:inherit}
input::placeholder{color:var(--faint)}
button{font:inherit;padding:6px 12px;border-radius:6px;border:1px solid var(--accent);background:var(--accent);color:var(--on-accent);cursor:pointer}
button.quiet{background:transparent;color:var(--fg);border-color:var(--line)}
button.danger{background:transparent;color:var(--danger);border-color:var(--danger);padding:3px 10px}
code,pre{font:13px/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:var(--soft);border-radius:4px}
code{padding:1px 4px}
pre{padding:10px 12px;white-space:pre-wrap;word-break:break-all}
.notice{border-left:2px solid var(--accent);padding:10px 14px;background:var(--panel)}
.join>p,.join>ol,.join>pre{max-width:660px}
.join{overflow-wrap:anywhere}
.join>p{font:17px/1.55 var(--serif)}
.join>p.muted{font:15px/1.55 var(--sans)}
.join h2{font:600 22px/1.25 var(--serif);margin:36px 0 10px}
.join ol{padding-left:22px;font:17px/1.55 var(--serif)}
.join li{margin:0 0 16px}
.join li pre{margin:8px 0 0;font-size:13px}
.join #key{font-size:14px;padding:14px 16px;border:1px solid var(--line)}
@media (max-width:720px){
header{flex-wrap:wrap;gap:12px;padding:14px 16px}
.brand{font-size:22px}
.who{flex:1 1 0;text-align:right}
header nav{order:1;flex-basis:calc(100% + 32px);gap:0;margin:2px -16px -14px;border-top:1px solid var(--line)}
header nav a{flex:1;text-align:center;font-size:15px;padding:14px 0 13px;color:var(--faint)}
header nav a[aria-current]{text-underline-offset:6px}
main{padding:24px 16px 40px}
h1{font-size:28px}
*+h1{margin-top:36px}
.wrap{overflow:visible}
table,tbody,tr,td{display:block}
thead{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}
tr{margin:0 0 12px;padding:8px 14px;border:1px solid var(--line);border-radius:8px;background:var(--panel)}
td,td:first-child,td:last-child{display:flex;justify-content:flex-end;align-items:baseline;gap:6px;padding:6px 0;border:0;white-space:normal;text-align:right;overflow-wrap:anywhere}
td::before{content:attr(data-label);flex:none;margin-right:auto;padding-right:10px;text-align:left;font-size:10.5px;letter-spacing:.08em;text-transform:uppercase;color:var(--faint)}
td:empty{display:none}
td:last-child{justify-content:flex-end;padding-top:10px}
button.danger{padding:6px 14px}
}"#;

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
<link rel="icon" href="/app/favicon-light-32.png" sizes="32x32" type="image/png" media="(prefers-color-scheme: light)">
<link rel="icon" href="/app/favicon-dark-32.png" sizes="32x32" type="image/png" media="(prefers-color-scheme: dark)">
<link rel="icon" href="/app/favicon.svg" type="image/svg+xml">
<style>{{ css|safe }}</style>
</head>
<body>
<header>
<a class="brand" href="/">{{ mark|safe }}conde</a>
{% if let Some(h) = header %}<nav aria-label="Sections"><a href="/#/">Feed</a><a href="/#/agents">Agents</a><a href="/#/channels">Channels</a><a href="/#/fleet">Fleet</a><a href="/keys" aria-current="page">Keys</a></nav>
<span class="who">{{ h.display }}</span>
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
    mark: &'a str,
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
<td data-label="Host"><code>{{ k.host_id }}</code></td><td data-label="Label">{{ k.label }}</td><td data-label="Created">{{ k.created }}</td><td data-label="Last seen">{{ k.last_seen }}</td>
{% if let Some(at) = k.revoked %}<td class="muted" data-label="State">revoked {{ at }}</td><td></td>
{% else %}<td data-label="State">live</td><td><form method="post" action="/keys/{{ k.host_id|urlencode }}/revoke" class="inline"><input type="hidden" name="csrf" value="{{ csrf }}"><button class="danger">Revoke</button></form></td>{% endif %}
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
    source = r#"<h1>Confirm conde login</h1>
<p>A <code>conde login</code> wants a host key for <strong>{{ account }}</strong>.</p>
<p class="notice">Continue only if you started <code>conde login --code</code> yourself in the last few minutes. Whoever holds the code the next page shows gets a host key for your account.</p>
<form method="post" action="/auth/cli" class="row">
<input type="hidden" name="csrf" value="{{ csrf }}">
<input type="hidden" name="state" value="{{ state }}">
<input type="hidden" name="challenge" value="{{ challenge }}">
<button>I started it: show the code</button>
</form>
<p><a href="/keys">Cancel</a></p>"#
)]
struct CliConfirmBody<'a> {
    account: &'a str,
    csrf: &'a str,
    state: &'a str,
    challenge: &'a str,
}

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<h1>Sign-in code for conde login</h1>
<p>Type this code at the <code>conde login</code> prompt, or read it to the agent that started the login.</p>
<pre id="key">{{ code }}</pre>
<p><button type="button" id="copy">Copy code</button></p>
<p class="notice">It works once, only for the <code>conde login</code> run that opened this page, within {{ minutes }} minutes. Give it only to a login you started: whoever enters it gets a host key for your account.</p>
<p><a href="/keys">Host keys</a></p>"#
)]
struct CliCodeBody<'a> {
    code: &'a str,
    minutes: u64,
}

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<h1>{{ title }}</h1>
<p>{{ message }}</p>
<p><a href="/">Go to the board</a></p>"#
)]
struct ErrorBody<'a> {
    title: &'a str,
    message: &'a str,
}

pub(crate) fn render(t: &impl Template) -> String {
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
    form_targets: &[&str],
) -> Response {
    let nonce = with_script.then(random_token);
    let html = render(&Layout {
        title,
        css: CSS,
        mark: super::app::mark_svg(),
        header,
        body,
        nonce: nonce.as_deref(),
        js: COPY_JS,
    });
    let script = nonce
        .as_deref()
        .map(|n| format!("'nonce-{n}'"))
        .unwrap_or_else(|| "'none'".into());
    let mut forms = String::from("'self'");
    for origin in form_targets {
        forms.push(' ');
        forms.push_str(origin);
    }
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
        &[idp],
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
        &[idp],
    )
}

/// The pasted-code flow's confirm page: asks the signed-in user before any code exists.
pub(crate) fn cli_confirm(
    header: Header<'_>,
    account: &str,
    state: &str,
    challenge: &str,
    idp: &str,
) -> Response {
    let body = render(&CliConfirmBody {
        account,
        csrf: header.csrf,
        state,
        challenge,
    });
    page(
        StatusCode::OK,
        "Confirm conde login",
        Some(header),
        &body,
        false,
        &[idp],
    )
}

/// The pasted-code flow's code, shown to the signed-in user who completed that login.
pub(crate) fn cli_code(header: Header<'_>, code: &str, minutes: u64, idp: &str) -> Response {
    let body = render(&CliCodeBody { code, minutes });
    page(
        StatusCode::OK,
        "conde login code",
        Some(header),
        &body,
        true,
        &[idp],
    )
}

/// A public page (no header) in the keys page's style; `copy`: with the copy button's script.
pub(crate) fn public(status: StatusCode, title: &str, body: &str, copy: bool) -> Response {
    page(status, title, None, body, copy, &[])
}

/// A public document page (no header, no script) in the keys page's style.
pub(crate) fn document(title: &str, body: &str) -> Response {
    page(StatusCode::OK, title, None, body, false, &[])
}

pub(crate) fn error(status: StatusCode, title: &str, message: &str) -> Response {
    let body = render(&ErrorBody { title, message });
    page(status, title, None, &body, false, &[])
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

    fn keys_page(header: Option<Header<'_>>, revoked: Option<&str>) -> String {
        let key = HostKey {
            key_id: "k".into(),
            host_id: "work-laptop".into(),
            label: Some("Work Laptop".into()),
            created_at: None,
            last_seen: None,
            revoked_at: revoked.map(String::from),
        };
        let body = render(&KeysBody {
            csrf: "c",
            server: "https://s",
            rows: vec![KeyRow::from(&key)],
        });
        render(&Layout {
            title: "Host keys",
            css: CSS,
            mark: "",
            header,
            body: &body,
            nonce: None,
            js: COPY_JS,
        })
    }

    #[test]
    fn signed_in_pages_carry_the_app_nav() {
        let page = keys_page(
            Some(Header {
                display: "Ann",
                csrf: "c",
            }),
            None,
        );
        assert!(page.contains(r#"<a class="brand" href="/">"#), "{page}");
        for link in [
            r#"<a href="/#/">Feed</a>"#,
            r#"<a href="/#/agents">Agents</a>"#,
            r#"<a href="/#/channels">Channels</a>"#,
            r#"<a href="/#/fleet">Fleet</a>"#,
            r#"<a href="/keys" aria-current="page">Keys</a>"#,
        ] {
            assert!(page.contains(link), "{link} missing: {page}");
        }
        let public = render(&Layout {
            title: "Not found",
            css: CSS,
            mark: "",
            header: None,
            body: "",
            nonce: None,
            js: COPY_JS,
        });
        assert!(
            !public.contains("<nav"),
            "a signed-out page has no app nav: {public}"
        );
    }

    #[test]
    fn keys_forms_are_unchanged() {
        let page = keys_page(
            Some(Header {
                display: "Ann",
                csrf: "c",
            }),
            None,
        );
        for form in [
            r#"<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="c"><button class="quiet">Sign out</button></form>"#,
            "<form method=\"post\" action=\"/keys\" class=\"row\">\n<input type=\"hidden\" name=\"csrf\" value=\"c\">\n<input type=\"text\" name=\"label\" required maxlength=\"64\"",
            r#"<form method="post" action="/keys/work-laptop/revoke" class="inline"><input type="hidden" name="csrf" value="c"><button class="danger">Revoke</button></form>"#,
        ] {
            assert!(page.contains(form), "{form} missing: {page}");
        }
        assert_eq!(page.matches("<form").count(), 3, "{page}");
        assert!(
            page.contains(r#"<td data-label="State">live</td>"#),
            "{page}"
        );
        let revoked = keys_page(
            Some(Header {
                display: "Ann",
                csrf: "c",
            }),
            Some("2026-10-09"),
        );
        assert!(
            revoked.contains(r#"<td class="muted" data-label="State">revoked 2026-10-09</td>"#),
            "{revoked}"
        );
        assert!(!revoked.contains("/revoke"), "{revoked}");
    }

    #[test]
    fn the_stylesheet_is_dark_only_and_inline_safe() {
        assert!(CSS.contains("--bg:#131518") && CSS.contains("--accent:#d4886b"));
        assert!(CSS.contains("color-scheme:dark") && !CSS.contains("prefers-color-scheme"));
        assert!(
            !CSS.contains("@font-face") && !CSS.contains("url("),
            "the CSP allows no font or image source in CSS"
        );
    }
}
