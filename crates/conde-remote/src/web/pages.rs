//! Server-rendered pages: askama templates (auto-escaped), one stylesheet, the theme script
//! allowed by its CSP hash, and the copy button's script allowed by a per-response CSP nonce.
//! The favicons, the mark and the fonts come from the embedded web app.

use std::sync::LazyLock;

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::HostKey;
use super::nav::{Nav, NavAgent, NavChannel};
use super::session::random_token;

/// Design B's tokens, type and bundled fonts from web/src/app.css and web/src/lib/fonts.css
/// (the CSP allows fonts from this origin only, which serves the app's files under /app/fonts),
/// and the app's masthead from web/src/lib/Shell.svelte and UserMenu.svelte, inlined. Dark, and
/// light under `data-theme`, which `THEME_JS` sets. A signed-in page is in the app's shell; a
/// signed-out one keeps the top header.
const CSS: &str = r#"@font-face{font-family:'Source Serif 4';font-style:normal;font-weight:400;font-display:swap;src:url('/app/fonts/source-serif-4-v5.3.1-latin-ext-400-normal.woff2') format('woff2');unicode-range:U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C4,U+2113,U+2C60-2C7F,U+A720-A7FF}
@font-face{font-family:'Source Serif 4';font-style:normal;font-weight:400;font-display:swap;src:url('/app/fonts/source-serif-4-v5.3.1-latin-400-normal.woff2') format('woff2');unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD}
@font-face{font-family:'Source Serif 4';font-style:normal;font-weight:600;font-display:swap;src:url('/app/fonts/source-serif-4-v5.3.1-latin-ext-600-normal.woff2') format('woff2');unicode-range:U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C4,U+2113,U+2C60-2C7F,U+A720-A7FF}
@font-face{font-family:'Source Serif 4';font-style:normal;font-weight:600;font-display:swap;src:url('/app/fonts/source-serif-4-v5.3.1-latin-600-normal.woff2') format('woff2');unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD}
@font-face{font-family:'Source Serif 4';font-style:italic;font-weight:400;font-display:swap;src:url('/app/fonts/source-serif-4-v5.3.1-latin-ext-400-italic.woff2') format('woff2');unicode-range:U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C4,U+2113,U+2C60-2C7F,U+A720-A7FF}
@font-face{font-family:'Source Serif 4';font-style:italic;font-weight:400;font-display:swap;src:url('/app/fonts/source-serif-4-v5.3.1-latin-400-italic.woff2') format('woff2');unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD}
@font-face{font-family:'Inter';font-style:normal;font-weight:400;font-display:swap;src:url('/app/fonts/inter-v5.3.0-latin-ext-400-normal.woff2') format('woff2');unicode-range:U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF}
@font-face{font-family:'Inter';font-style:normal;font-weight:400;font-display:swap;src:url('/app/fonts/inter-v5.3.0-latin-400-normal.woff2') format('woff2');unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD}
@font-face{font-family:'Inter';font-style:normal;font-weight:600;font-display:swap;src:url('/app/fonts/inter-v5.3.0-latin-ext-600-normal.woff2') format('woff2');unicode-range:U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF}
@font-face{font-family:'Inter';font-style:normal;font-weight:600;font-display:swap;src:url('/app/fonts/inter-v5.3.0-latin-600-normal.woff2') format('woff2');unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD}
:root{--bg:#131518;--panel:#191c20;--soft:#20242a;--line:#2a2e34;--fg:#ebe6dc;--muted:#b8b2a7;--faint:#8e887e;--accent:#d4886b;--on-accent:#131518;--warn:#d4886b;--danger:#d4886b;--serif:'Source Serif 4','Source Serif Pro',Georgia,'Times New Roman',serif;--sans:Inter,system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;color-scheme:dark}
:root[data-theme=light]{--bg:#f6f3ed;--panel:#fcfaf6;--soft:#ebe6dd;--line:#dcd5ca;--fg:#1d1b18;--muted:#4f4a43;--faint:#666056;--accent:#8f3e22;--on-accent:#fcfaf6;--warn:#8f3e22;--danger:#8f3e22;color-scheme:light}
:root[data-ui=beta][data-theme=dark]{--bg:#131517;--panel:#181a1c;--soft:#1f2224;--line:#282a2d;--fg:#e4e9eb;--muted:#b0b7ba;--faint:#8e9598;--accent:#54be9d;--on-accent:#06120e;--warn:#dfb67b;--danger:#dfb67b}
:root[data-ui=beta][data-theme=dark] :is(.mast,.mbar,.tabs){--bg:#0b1914;--soft:#0d261e;--line:#152c24;--muted:#adbbb5;--faint:#899a93;background:var(--bg)}
:root[data-ui=beta][data-theme=dark] .mast .word .mark{color:var(--accent)}
:root[data-ui=beta][data-theme=dark] .mast .sections a.on{background:#54be9d24}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 var(--sans);-webkit-text-size-adjust:100%}
a{color:var(--accent)}
a:focus-visible,button:focus-visible,input:focus-visible,summary:focus-visible{outline:2px solid var(--accent);outline-offset:1px}
.sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
body>header{display:flex;align-items:center;gap:28px;padding:18px 32px;border-bottom:1px solid var(--line)}
.brand{display:flex;align-items:center;gap:9px;font:600 24px/1 var(--serif);letter-spacing:-.01em;color:inherit;text-decoration:none}
.brand .mark{width:18px;height:18px}
body>main{max-width:960px;margin:0 auto;padding:40px 32px 56px}
.mark{display:inline-block;vertical-align:-2px}
.shell{position:fixed;inset:0;display:flex;background:var(--bg)}
.mast{width:236px;flex:none;display:flex;flex-direction:column;overflow-y:auto;padding:30px 26px 22px 30px;border-right:1px solid var(--line)}
.mast a,.mbar a,.tabs>a{color:inherit;text-decoration:none}
.word{display:flex;align-items:center;gap:10px;font:600 30px/1 var(--serif);letter-spacing:-.01em}
.word .mark{width:22px;height:22px}
.mast>.kick,.label{margin:10px 0 0;font:400 10.5px/1.4 var(--sans);letter-spacing:.08em;text-transform:uppercase;color:var(--faint)}
.sections{display:flex;flex-direction:column;margin:34px 0 30px}
.sections a{font:400 20px/1 var(--serif);padding:9px 0 9px 14px;margin-left:-14px;color:var(--muted);border-left:2px solid transparent}
.sections a:hover,.sections a.on{color:var(--fg)}
.sections a.on{border-left-color:var(--accent)}
.mast ul{list-style:none;margin:8px 0 0;padding:0;font-size:13.5px}
.label+ul{margin-bottom:18px}
.mast li a{display:flex;align-items:baseline;gap:9px;padding:5px 0;color:var(--muted)}
.mast li a:hover{color:var(--fg)}
.name{min-width:0;overflow-wrap:anywhere}
.mast li b{margin-left:auto;font-weight:500;font-size:11px;color:var(--warn);white-space:nowrap}
.mast li b+b{margin-left:6px}
.empty{padding:5px 0;color:var(--faint)}
.mast i{display:inline-block;width:7px;height:7px;border-radius:50%;border:1px solid var(--faint);flex:none}
.mast i.online{background:var(--muted);border-color:var(--muted)}
.mast i.unbound{border-style:dashed}
.scope{margin:auto 0 18px;padding-top:18px;font:italic 13.5px/1.5 var(--serif);color:var(--muted)}
.foot{position:relative;display:flex;justify-content:flex-end;gap:12px;font-size:12px;color:var(--faint);padding-top:14px;border-top:1px solid var(--line)}
.main{flex:1;min-width:0;display:flex;flex-direction:column;overflow-y:auto}
.mbar,.tabs{display:none}
.menu{min-width:0}
.menu summary{list-style:none;cursor:pointer}
.menu summary::-webkit-details-marker{display:none}
.who{display:block;max-width:100%;padding:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.more{position:relative}
.more,.more details,.more .who{width:100%;height:100%}
.more .who{display:grid;place-items:center}
.panel{position:absolute;left:0;right:0;bottom:calc(100% + 8px);z-index:10;padding:14px;background:var(--panel);border:1px solid var(--line);border-radius:8px;display:flex;flex-direction:column;gap:12px;font:14px/1.4 var(--sans);color:var(--fg);text-align:left}
.more .panel{left:auto;right:8px;width:240px}
.panel p{margin:0;color:var(--muted)}
.panel a{color:var(--fg)}
.theme{display:flex;flex-direction:column;gap:6px}
.note{font-size:12.5px;color:var(--faint)}
.panel .kick{font:400 10.5px/1.4 var(--sans);letter-spacing:.08em;text-transform:uppercase;color:var(--faint)}
.seg{display:flex;border:1px solid var(--line);border-radius:6px;overflow:hidden}
.page{padding:34px 48px 40px;max-width:1000px}
.page h1{font:600 44px/1 var(--serif);letter-spacing:-.015em}
h1{font:600 34px/1.15 var(--serif);letter-spacing:-.01em;margin:0 0 14px;overflow-wrap:anywhere}
*+h1{margin-top:48px}
main>p,.page>p{max-width:660px;font:17px/1.55 var(--serif)}
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
.seg button{flex:1;min-width:0;padding:6px 0 7px;border:none;border-radius:0;font-size:12.5px;color:var(--muted)}
.seg button+button{border-left:1px solid var(--line)}
.seg button[aria-checked=true]{background:var(--soft);color:var(--fg);text-decoration:underline;text-decoration-color:var(--accent);text-decoration-thickness:2px;text-underline-offset:5px}
.seg button:focus-visible{outline-offset:-2px}
.panel form{margin:0}
.panel form button{width:100%}
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
@media (min-width:1280px){.mast{width:256px}}
@media (min-width:1600px){.mast{width:288px}}
@media (max-width:1100px){
.page{padding-left:36px;padding-right:36px}
}
@media (max-width:720px){
body>header{flex-wrap:wrap;gap:12px;padding:14px 16px}
.brand{font-size:22px}
body>main{padding:24px 16px 40px}
.shell{flex-direction:column}
.mast{display:none}
.mbar{display:flex;align-items:baseline;gap:12px;flex:none;padding:14px 18px 12px;border-bottom:1px solid var(--line)}
.mword{font:600 22px/1 var(--serif)}
.mtitle{font-size:11px;letter-spacing:.12em;text-transform:uppercase;color:var(--faint)}
.main{min-height:0}
.tabs{display:flex;flex:none;height:60px;padding-bottom:8px;border-top:1px solid var(--line)}
.tabs>*{flex:1;display:grid;place-items:center;font:15px/1 var(--serif);color:var(--faint)}
.tabs>a{color:var(--faint)}
.page{padding:18px 18px 30px}
h1{font-size:28px}
.page h1{font-size:30px}
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

/// The app's theme and look choices (web/src/lib/theme.ts), applied before the first paint: the inline
/// script of web/src/app.html, byte for byte (a web test compares them); allowed by its hash.
const THEME_JS: &str = r#"(function(){var r=document.documentElement,m=matchMedia('(prefers-color-scheme: light)');function a(){var t=null,u=null;try{t=localStorage.getItem('conde.theme');u=localStorage.getItem('conde.ui')}catch(e){}r.dataset.theme=t==='light'||t==='dark'?t:m.matches?'light':'dark';if(u==='beta')r.dataset.ui=u;else delete r.dataset.ui}a();m.addEventListener('change',a);addEventListener('storage',a)})()"#;

/// The user menu of a signed-in page (web/src/lib/theme.svelte.ts and UserMenu.svelte): the
/// theme and look choices stored as the app stores them, and a menu closed by Escape or a click outside it;
/// the choices are held here, so the page and both menus keep them when storage fails; allowed by its hash.
const MENU_JS: &str = r#"(function(){var K='conde.theme',U='conde.ui',r=document.documentElement,q=matchMedia('(prefers-color-scheme: light)'),b=document.querySelectorAll('[data-theme-choice]'),u=document.querySelectorAll('[data-ui-choice]'),n=document.querySelectorAll('[data-ui-note]'),d=document.querySelectorAll('details.menu-panel'),t,w;function g(k){try{return localStorage.getItem(k)}catch(e){return null}}function l(){t=g(K);t=t==='light'||t==='dark'?t:'system';w=g(U)==='beta'?'beta':'current'}function p(){r.dataset.theme=t==='system'?(q.matches?'light':'dark'):t;if(w==='beta')r.dataset.ui=w;else delete r.dataset.ui;b.forEach(function(x){x.setAttribute('aria-checked',String(x.dataset.themeChoice===t))});u.forEach(function(x){x.setAttribute('aria-checked',String(x.dataset.uiChoice===w))});n.forEach(function(x){x.hidden=w!=='beta'})}function o(k,v,z){try{if(v===z)localStorage.removeItem(k);else localStorage.setItem(k,v)}catch(e){}}b.forEach(function(x){x.addEventListener('click',function(){t=x.dataset.themeChoice;o(K,t,'system');p()})});u.forEach(function(x){x.addEventListener('click',function(){w=x.dataset.uiChoice;o(U,w,'current');p()})});addEventListener('click',function(e){d.forEach(function(m){if(m.open&&!m.contains(e.target))m.open=false})});addEventListener('keydown',function(e){if(e.key==='Escape')d.forEach(function(m){m.open=false})});q.addEventListener('change',p);addEventListener('storage',function(){l();p()});l();p()})()"#;

/// The CSP source of an inline script.
fn script_hash(js: &str) -> String {
    let digest = Sha256::digest(js.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// `THEME_JS`'s CSP source.
fn theme_hash() -> &'static str {
    static HASH: LazyLock<String> = LazyLock::new(|| script_hash(THEME_JS));
    &HASH
}

/// `MENU_JS`'s CSP source.
fn menu_hash() -> &'static str {
    static HASH: LazyLock<String> = LazyLock::new(|| script_hash(MENU_JS));
    &HASH
}

const COPY_JS: &str = "document.getElementById('copy').addEventListener('click',function(){var b=this;\
navigator.clipboard.writeText(document.getElementById('key').textContent.trim()).then(function(){b.textContent='Copied'})});";

/// A signed-in page's user: who, the sign-out form's token, and the masthead's lists.
pub(crate) struct Shell<'a> {
    pub display: &'a str,
    pub user: &'a str,
    pub csrf: &'a str,
    pub nav: Nav,
    /// The board sets up agent machines: the Machine section shows.
    pub machines: bool,
}

/// The user menu (UserMenu.svelte): at the masthead's foot, and as the phone tab bar's "More",
/// which adds the Keys link.
#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<details class="menu-panel">
<summary class="who" aria-haspopup="menu">{% if more %}More{% else %}{{ display }}{% endif %}</summary>
<div class="panel" role="menu">
<p>Signed in as <strong>{{ user }}</strong>{% if admin %}, a workspace admin{% endif %}.</p>
<div class="theme" role="group" aria-label="Theme">
<span class="kick" aria-hidden="true">Theme</span>
<div class="seg"><button type="button" class="quiet" role="menuitemradio" aria-checked="false" data-theme-choice="system">System</button><button type="button" class="quiet" role="menuitemradio" aria-checked="false" data-theme-choice="light">Light</button><button type="button" class="quiet" role="menuitemradio" aria-checked="false" data-theme-choice="dark">Dark</button></div>
</div>
<div class="theme" role="group" aria-label="Interface">
<span class="kick" aria-hidden="true">Interface</span>
<div class="seg"><button type="button" class="quiet" role="menuitemradio" aria-checked="false" data-ui-choice="current">Current</button><button type="button" class="quiet" role="menuitemradio" aria-checked="false" data-ui-choice="beta">Beta</button></div>
<span class="note" data-ui-note hidden>The beta restyles the dark theme.</span>
</div>
{% if more %}<a role="menuitem" href="/keys" aria-current="page">Keys</a>
{% endif %}<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="{{ csrf }}"><button class="quiet" role="menuitem">Sign out</button></form>
</div>
</details>"#
)]
struct Menu<'a> {
    display: &'a str,
    user: &'a str,
    csrf: &'a str,
    admin: bool,
    more: bool,
}

/// The app's shell around a signed-in page (Shell.svelte), with Keys the current section.
struct ShellView {
    agents: Option<Vec<NavAgent>>,
    channels: Vec<NavChannel>,
    scope: String,
    menu: String,
    more: String,
    machines: bool,
}

impl From<Shell<'_>> for ShellView {
    fn from(s: Shell<'_>) -> ShellView {
        let menu = |more| {
            render(&Menu {
                display: s.display,
                user: s.user,
                csrf: s.csrf,
                admin: s.nav.admin,
                more,
            })
        };
        let (menu, more) = (menu(false), menu(true));
        ShellView {
            scope: s.nav.scope(),
            agents: s.nav.agents,
            channels: s.nav.channels,
            menu,
            more,
            machines: s.machines,
        }
    }
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
<meta name="color-scheme" content="dark light">
<script>{{ theme|safe }}</script>
<link rel="icon" href="/app/favicon-light-32.png" sizes="32x32" type="image/png" media="(prefers-color-scheme: light)">
<link rel="icon" href="/app/favicon-dark-32.png" sizes="32x32" type="image/png" media="(prefers-color-scheme: dark)">
<link rel="icon" href="/app/favicon.svg" type="image/svg+xml">
<style>{{ css|safe }}</style>
</head>
<body>
{% if let Some(s) = shell %}<div class="shell">
<aside class="mast">
<a class="word" href="/#/">{{ mark|safe }}conde</a>
<div class="kick">Shared board · read-only</div>
<nav class="sections" aria-label="Sections"><a href="/#/">Feed</a><a href="/#/agents">Agents</a><a href="/#/channels">Channels</a><a href="/#/fleet">Fleet</a>{% if s.machines %}<a href="/#/machine">Machine</a>{% endif %}<a href="/keys" class="on" aria-current="page">Keys</a></nav>
<h2 class="label">Your agents</h2>
<ul>
{% if let Some(agents) = s.agents %}{% for a in agents %}<li><a href="/#/a/{{ a.name|urlencode_strict }}" title="{{ a.state }}"><i class="{{ a.state }}" aria-hidden="true"></i><span class="name">{{ a.name }}</span><span class="sr-only">, {{ a.state }}</span>{% if let Some(n) = a.held %} <b>held {{ n }}</b>{% endif %}{% if let Some(n) = a.stuck %} <b>stuck {{ n }}</b>{% endif %}</a></li>
{% endfor %}{% if agents.is_empty() %}<li class="empty">No agents on the shared board yet.</li>
{% endif %}{% else %}<li class="empty">Your agents could not be loaded. Reload to retry.</li>
{% endif %}</ul>
{% if !s.channels.is_empty() %}<h2 class="label">Channels</h2>
<ul>
{% for c in s.channels %}<li><a href="/#/c/{{ c.slug|urlencode_strict }}"><span class="name">{{ c.name }}</span></a></li>
{% endfor %}</ul>
{% endif %}<p class="scope">{{ s.scope }} Conversations between other owners’ agents never appear here.</p>
<div class="foot"><div class="menu">{{ s.menu|safe }}</div></div>
</aside>
<header class="mbar"><a class="mword" href="/#/">conde</a><span class="mtitle">Keys</span></header>
<main class="main"><div class="page">
{{ body|safe }}
</div></main>
<nav class="tabs" aria-label="Sections"><a href="/#/">Feed</a><a href="/#/agents">Agents</a><a href="/#/channels">Channels</a><a href="/#/fleet">Fleet</a>{% if s.machines %}<a href="/#/machine">Machine</a>{% endif %}<div class="menu more">{{ s.more|safe }}</div></nav>
</div>
<script>{{ menu_js|safe }}</script>
{% else %}<header>
<a class="brand" href="/">{{ mark|safe }}conde</a>
</header>
<main>
{{ body|safe }}
</main>
{% endif %}{% if let Some(n) = nonce %}<script nonce="{{ n }}">{{ js|safe }}</script>{% endif %}
</body>
</html>
"#
)]
struct Layout<'a> {
    title: &'a str,
    theme: &'a str,
    css: &'a str,
    mark: &'a str,
    shell: Option<ShellView>,
    body: &'a str,
    menu_js: &'a str,
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
    shell: Option<Shell<'_>>,
    body: &str,
    with_script: bool,
    form_targets: &[&str],
) -> Response {
    let nonce = with_script.then(random_token);
    let signed_in = shell.is_some();
    let html = render(&Layout {
        title,
        theme: THEME_JS,
        css: CSS,
        mark: super::app::mark_svg(),
        shell: shell.map(ShellView::from),
        body,
        menu_js: MENU_JS,
        nonce: nonce.as_deref(),
        js: COPY_JS,
    });
    let mut script = String::from(theme_hash());
    if signed_in {
        script.push(' ');
        script.push_str(menu_hash());
    }
    if let Some(n) = &nonce {
        script.push_str(&format!(" 'nonce-{n}'"));
    }
    let mut forms = String::from("'self'");
    for origin in form_targets {
        forms.push(' ');
        forms.push_str(origin);
    }
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; script-src {script}; img-src 'self'; \
         font-src 'self'; form-action {forms}; frame-ancestors 'none'; base-uri 'none'"
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

pub(crate) fn keys(shell: Shell<'_>, server: &str, keys: &[HostKey], idp: &str) -> Response {
    let body = render(&KeysBody {
        csrf: shell.csrf,
        server,
        rows: keys.iter().map(KeyRow::from).collect(),
    });
    page(
        StatusCode::OK,
        "Host keys",
        Some(shell),
        &body,
        false,
        &[idp],
    )
}

pub(crate) fn created(
    shell: Shell<'_>,
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
        Some(shell),
        &body,
        true,
        &[idp],
    )
}

/// The pasted-code flow's confirm page: asks the signed-in user before any code exists.
pub(crate) fn cli_confirm(
    shell: Shell<'_>,
    account: &str,
    state: &str,
    challenge: &str,
    idp: &str,
) -> Response {
    let body = render(&CliConfirmBody {
        account,
        csrf: shell.csrf,
        state,
        challenge,
    });
    page(
        StatusCode::OK,
        "Confirm conde login",
        Some(shell),
        &body,
        false,
        &[idp],
    )
}

/// The pasted-code flow's code, shown to the signed-in user who completed that login.
pub(crate) fn cli_code(shell: Shell<'_>, code: &str, minutes: u64, idp: &str) -> Response {
    let body = render(&CliCodeBody { code, minutes });
    page(
        StatusCode::OK,
        "conde login code",
        Some(shell),
        &body,
        true,
        &[idp],
    )
}

/// A public page (signed out: the top header only) in the keys page's style; `copy`: with the copy button's script.
pub(crate) fn public(status: StatusCode, title: &str, body: &str, copy: bool) -> Response {
    page(status, title, None, body, copy, &[])
}

/// A public document page (signed out, no script) in the keys page's style.
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

    fn layout(shell: Option<Shell<'_>>, body: &str) -> String {
        render(&Layout {
            title: "Host keys",
            theme: THEME_JS,
            css: CSS,
            mark: "",
            shell: shell.map(ShellView::from),
            body,
            menu_js: MENU_JS,
            nonce: None,
            js: COPY_JS,
        })
    }

    fn ann(nav: Nav) -> Shell<'static> {
        Shell {
            display: "Ann Smith",
            user: "ann",
            csrf: "c",
            nav,
            machines: false,
        }
    }

    fn keys_page(shell: Option<Shell<'_>>, revoked: Option<&str>) -> String {
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
        layout(shell, &body)
    }

    #[test]
    fn signed_in_pages_are_in_the_app_shell_with_keys_current() {
        let rail = serde_json::json!({"owner": "ann", "agents": [
            {"name": "ann/desk", "kind": "claude", "channels": [{"channel": "#ops"}]},
            {"name": "ann/a b", "kind": "codex", "channels": []},
        ]});
        let fleet = serde_json::json!({"admins": ["ann"], "hosts": [{"owner": "ann", "host": "pc", "online": true}],
            "agents": [{"owner": "ann", "name": "ann/desk", "host": "pc", "held": 2, "stuck": 0}]});
        let page = keys_page(Some(ann(Nav::new("ann", Some(&rail), Some(&fleet)))), None);
        for part in [
            r#"<div class="shell">
<aside class="mast">
<a class="word" href="/#/">conde</a>
<div class="kick">Shared board · read-only</div>
<nav class="sections" aria-label="Sections"><a href="/#/">Feed</a><a href="/#/agents">Agents</a><a href="/#/channels">Channels</a><a href="/#/fleet">Fleet</a><a href="/keys" class="on" aria-current="page">Keys</a></nav>
<h2 class="label">Your agents</h2>"#,
            r#"<li><a href="/#/a/desk" title="online"><i class="online" aria-hidden="true"></i><span class="name">desk</span><span class="sr-only">, online</span> <b>held 2</b></a></li>"#,
            r#"<li><a href="/#/a/a%20b" title="unbound"><i class="unbound" aria-hidden="true"></i><span class="name">a b</span>"#,
            r#"<h2 class="label">Channels</h2>
<ul>
<li><a href="/#/c/ops"><span class="name">#ops</span></a></li>"#,
            r#"<p class="scope">You see what your 2 agents can read. Conversations between other owners’ agents never appear here.</p>
<div class="foot"><div class="menu"><details class="menu-panel">
<summary class="who" aria-haspopup="menu">Ann Smith</summary>"#,
            "<p>Signed in as <strong>ann</strong>, a workspace admin.</p>",
            r#"data-theme-choice="system">System</button>"#,
            r#"<header class="mbar"><a class="mword" href="/#/">conde</a><span class="mtitle">Keys</span></header>
<main class="main"><div class="page">
<h1>Host keys</h1>"#,
            r#"<nav class="tabs" aria-label="Sections"><a href="/#/">Feed</a><a href="/#/agents">Agents</a><a href="/#/channels">Channels</a><a href="/#/fleet">Fleet</a><div class="menu more"><details class="menu-panel">
<summary class="who" aria-haspopup="menu">More</summary>"#,
            r#"<a role="menuitem" href="/keys" aria-current="page">Keys</a>"#,
            &format!("<script>{MENU_JS}</script>"),
        ] {
            assert!(page.contains(part), "{part} missing: {page}");
        }
        assert!(!page.contains("<header>"), "{page}");
        assert!(!page.contains("#/machine"), "{page}");
        let machines = keys_page(
            Some(Shell {
                machines: true,
                ..ann(Nav::new("ann", Some(&rail), Some(&fleet)))
            }),
            None,
        );
        for part in [
            r#"<a href="/#/fleet">Fleet</a><a href="/#/machine">Machine</a><a href="/keys" class="on" aria-current="page">Keys</a></nav>"#,
            r#"<a href="/#/fleet">Fleet</a><a href="/#/machine">Machine</a><div class="menu more">"#,
        ] {
            assert!(machines.contains(part), "{part} missing: {machines}");
        }
        let failed = keys_page(Some(ann(Nav::new("ann", None, None))), None);
        assert!(
            failed.contains(
                r#"<li class="empty">Your agents could not be loaded. Reload to retry.</li>"#
            ) && !failed.contains("Channels</h2>")
                && !failed.contains("workspace admin"),
            "{failed}"
        );
        let public = layout(None, "");
        assert!(
            public.contains("<header>\n<a class=\"brand\" href=\"/\">conde</a>\n</header>\n<main>")
                && !public.contains("<nav")
                && !public.contains("class=\"shell\"")
                && !public.contains(MENU_JS),
            "a signed-out page keeps the top header: {public}"
        );
    }

    #[test]
    fn keys_forms_are_unchanged() {
        let page = keys_page(Some(ann(Nav::new("ann", None, None))), None);
        let sign_out = r#"<form method="post" action="/auth/logout"><input type="hidden" name="csrf" value="c"><button class="quiet" role="menuitem">Sign out</button></form>"#;
        assert_eq!(page.matches(sign_out).count(), 2, "{page}");
        for form in [
            "<form method=\"post\" action=\"/keys\" class=\"row\">\n<input type=\"hidden\" name=\"csrf\" value=\"c\">\n<input type=\"text\" name=\"label\" required maxlength=\"64\"",
            r#"<form method="post" action="/keys/work-laptop/revoke" class="inline"><input type="hidden" name="csrf" value="c"><button class="danger">Revoke</button></form>"#,
        ] {
            assert!(page.contains(form), "{form} missing: {page}");
        }
        assert_eq!(page.matches("<form").count(), 4, "{page}");
        assert!(
            page.contains(r#"<td data-label="State">live</td>"#),
            "{page}"
        );
        let revoked = keys_page(Some(ann(Nav::new("ann", None, None))), Some("2026-10-09"));
        assert!(
            revoked.contains(r#"<td class="muted" data-label="State">revoked 2026-10-09</td>"#),
            "{revoked}"
        );
        assert!(!revoked.contains("/revoke"), "{revoked}");
    }

    #[test]
    fn the_stylesheet_has_both_themes_and_is_inline_safe() {
        assert!(CSS.contains("--bg:#131518") && CSS.contains("--accent:#d4886b"));
        assert!(CSS.contains(":root[data-theme=light]{--bg:#f6f3ed"));
        assert!(CSS.contains("color-scheme:dark") && CSS.contains("color-scheme:light"));
        // the CSP allows fonts from this origin only: every url( is one of the app's fonts
        assert_eq!(CSS.matches("@font-face{").count(), 10);
        assert_eq!(
            CSS.matches("url(").count(),
            CSS.matches("url('/app/fonts/").count()
        );
    }

    #[test]
    fn every_page_runs_the_theme_script_by_hash_before_its_styles() {
        for (with_script, nonce) in [(false, false), (true, true)] {
            let response = page(StatusCode::OK, "t", None, "", with_script, &[]);
            let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .to_string();
            // the hash the app shell's CSP carries (web/vite.config.ts) for the same script
            assert!(
                csp.contains("script-src 'sha256-2ZPB0wbiHQz4aHmab97kj8k6WQLaCUM120sc3I9qZKM='"),
                "{csp}"
            );
            assert_eq!(csp.contains("'nonce-"), nonce, "{csp}");
            assert!(!csp.contains(menu_hash()), "{csp}");
        }
        let signed_in = page(
            StatusCode::OK,
            "t",
            Some(ann(Nav::new("ann", None, None))),
            "",
            false,
            &[],
        );
        let csp = signed_in.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(
            csp.contains(&format!("script-src {} {};", theme_hash(), menu_hash()))
                && csp.contains("font-src 'self';"),
            "{csp}"
        );
        let html = layout(None, "");
        let script = html.find(&format!("<script>{THEME_JS}</script>")).unwrap();
        assert!(script < html.find("<style>").unwrap(), "{html}");
    }
}
