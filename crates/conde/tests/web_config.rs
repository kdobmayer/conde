//! The server's web configuration from its environment (`OIDC_*` or the older `ENTRA_*`,
//! `CONDE_ADMIN_KEY`, each secret also from a `*_FILE`; the retired
//! remote MCP's `CONDE_MCP_*` are ignored), as the container sets it. It changes the process environment, so the
//! binary runs without the libtest harness (`harness = false`): its cases run on the main thread.

mod serial;

use std::sync::Arc;

use conde::dispatch::Dispatcher;
use conde::web_store::WebBoard;
use conde_remote::web::{WebConfig, WebSetup};

const VARS: [&str; 18] = [
    "CONDE_MOVED_TO",
    "CONDE_SLACK_BOT_TOKEN",
    "CONDE_SLACK_BOT_TOKEN_FILE",
    "CONDE_SLACK_NOTIFY",
    "ENTRA_TENANT_ID",
    "ENTRA_CLIENT_ID",
    "ENTRA_CLIENT_SECRET",
    "ENTRA_REDIRECT_URI",
    "OIDC_ISSUER",
    "OIDC_CLIENT_ID",
    "OIDC_CLIENT_SECRET",
    "OIDC_CLIENT_SECRET_FILE",
    "OIDC_REDIRECT_URI",
    "CONDE_MCP_CLIENT_HOSTS",
    "CONDE_MCP_GRANTS_PER_CLIENT",
    "CONDE_ADMIN_KEY",
    "CONDE_ADMIN_KEY_FILE",
    "ENTRA_CLIENT_SECRET_FILE",
];

/// Sets exactly `vars` among [`VARS`] (the rest unset).
fn env(vars: &[(&str, &str)]) {
    for name in VARS {
        match vars.iter().find(|(n, _)| *n == name) {
            // SAFETY: cases run on the main thread (`serial`); no other thread reads the environment.
            Some((_, v)) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
    }
}

const ENTRA: [(&str, &str); 4] = [
    ("ENTRA_TENANT_ID", "2e51b484-b008-4fc0-8f70-555c1317b555"),
    ("ENTRA_CLIENT_ID", "conde-client"),
    ("ENTRA_CLIENT_SECRET", "conde-secret"),
    ("ENTRA_REDIRECT_URI", "https://conde.example/auth/callback"),
];

fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<_> = ENTRA.to_vec();
    for (k, val) in extra {
        v.retain(|(n, _)| n != k);
        v.push((k, val));
    }
    v
}

fn error(r: Result<Option<WebConfig>, conde_remote::web::ConfigError>) -> String {
    r.expect_err("refused").to_string()
}

const OIDC: [(&str, &str); 4] = [
    ("OIDC_ISSUER", "https://auth.conde.cloud/realms/dakai"),
    ("OIDC_CLIENT_ID", "conde-dakai"),
    ("OIDC_CLIENT_SECRET", "realm-secret"),
    ("OIDC_REDIRECT_URI", "https://dakai.conde.cloud/auth/callback"),
];

fn main() {
    serial::run(
        "web_config",
        &[
            ("the_web_configuration_comes_from_the_environment", the_web_configuration_comes_from_the_environment),
            ("an_oidc_issuer_is_configured_and_entra_variables_map_onto_it", an_oidc_issuer_is_configured_and_entra_variables_map_onto_it),
            ("secret_files_win_over_the_plain_variables", secret_files_win_over_the_plain_variables),
            ("the_slack_token_may_come_from_a_file", the_slack_token_may_come_from_a_file),
            ("conde_moved_to_names_an_http_url", conde_moved_to_names_an_http_url),
        ],
    );
}

/// `OIDC_*` configure any issuer; the `ENTRA_*` of an older deployment map onto them (the tenant
/// becomes its v2.0 issuer), and an `OIDC_*` set beside its Entra counterpart wins.
fn an_oidc_issuer_is_configured_and_entra_variables_map_onto_it() {
    env(&OIDC);
    let c = WebConfig::from_env().unwrap().unwrap();
    assert_eq!(c.issuer, "https://auth.conde.cloud/realms/dakai");
    assert_eq!(c.entra_tenant, None, "a Keycloak realm is not Entra");
    assert_eq!((c.client_id.as_str(), c.client_secret.as_str()), ("conde-dakai", "realm-secret"));
    assert_eq!(c.public_url, "https://dakai.conde.cloud");

    // The Entra variables alone: the same configuration the Azure deployment has today.
    env(&ENTRA);
    let c = WebConfig::from_env().unwrap().unwrap();
    assert_eq!(c.issuer, "https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0");
    assert_eq!(c.entra_tenant.as_deref(), Some("2e51b484-b008-4fc0-8f70-555c1317b555"));
    assert_eq!((c.client_id.as_str(), c.client_secret.as_str()), ("conde-client", "conde-secret"));
    // An Entra issuer given as OIDC_ISSUER is recognized as Entra too.
    env(&[OIDC[1], OIDC[2], OIDC[3], ("OIDC_ISSUER", "https://login.microsoftonline.com/t-1/v2.0")]);
    assert_eq!(WebConfig::from_env().unwrap().unwrap().entra_tenant.as_deref(), Some("t-1"));

    // Mixed: each OIDC_* wins over its Entra variable, a missing one falls back.
    let mut mixed = ENTRA.to_vec();
    mixed.extend([OIDC[0], OIDC[2]]);
    env(&mixed);
    let c = WebConfig::from_env().unwrap().unwrap();
    assert_eq!((c.issuer.as_str(), c.entra_tenant.as_deref()), ("https://auth.conde.cloud/realms/dakai", None));
    assert_eq!((c.client_id.as_str(), c.client_secret.as_str()), ("conde-client", "realm-secret"));
    assert_eq!(c.redirect_uri, "https://conde.example/auth/callback");

    // Missing pieces are named with both spellings; the issuer must be https.
    env(&[OIDC[0]]);
    assert_eq!(error(WebConfig::from_env()), "OIDC_CLIENT_ID (or ENTRA_CLIENT_ID) is not set");
    env(&[OIDC[0], OIDC[1], OIDC[2]]);
    assert_eq!(error(WebConfig::from_env()), "OIDC_REDIRECT_URI (or ENTRA_REDIRECT_URI) is not set");
    env(&[OIDC[1], OIDC[2], OIDC[3]]);
    assert_eq!(error(WebConfig::from_env()), "OIDC_ISSUER (or ENTRA_TENANT_ID) is not set");
    env(&[OIDC[1], OIDC[2], OIDC[3], ("OIDC_ISSUER", "http://auth.conde.cloud/realms/dakai")]);
    assert_eq!(error(WebConfig::from_env()), "OIDC_ISSUER 'http://auth.conde.cloud/realms/dakai' must be an https URL");
    env(&[]);
}

/// `CONDE_ADMIN_KEY_FILE` and `OIDC_CLIENT_SECRET_FILE` (as the secret store mounts them, with
/// a trailing newline) win over the plain variables; an unreadable one stops the server.
fn secret_files_win_over_the_plain_variables() {
    let store = Arc::new(WebBoard::new(Arc::new(Dispatcher::open_in_memory().unwrap())));
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("admin-key");
    let file_key = "f1Le2Ke3y4Fr5om6Th7eS8ec9re0tSt1ore";
    std::fs::write(&key_file, format!("{file_key}\n")).unwrap();
    let secret_file = dir.path().join("client-secret");
    std::fs::write(&secret_file, "secret-from-the-file\n").unwrap();
    let (key_path, secret_path) = (key_file.to_str().unwrap(), secret_file.to_str().unwrap());

    let mut vars = OIDC.to_vec();
    vars.extend([
        ("OIDC_CLIENT_SECRET_FILE", secret_path),
        ("CONDE_ADMIN_KEY", "q7Zx2Lm9Vt4Rk8Wp3Ny6Hb1Jc5Fd0GsUe"),
        ("CONDE_ADMIN_KEY_FILE", key_path),
    ]);
    env(&vars);
    let setup = WebSetup::from_env(store.clone()).unwrap().unwrap();
    assert_eq!(setup.admin_key.as_deref(), Some(file_key));
    assert_eq!(setup.entra.unwrap().client_secret, "secret-from-the-file");

    // The files alone, with the plain variables unset.
    env(&[OIDC[0], OIDC[1], OIDC[3], ("OIDC_CLIENT_SECRET_FILE", secret_path), ("CONDE_ADMIN_KEY_FILE", key_path)]);
    let setup = WebSetup::from_env(store.clone()).unwrap().unwrap();
    assert_eq!(setup.admin_key.as_deref(), Some(file_key));
    assert_eq!(setup.entra.unwrap().client_secret, "secret-from-the-file");

    // An unreadable file is a named error, never a fallback to the plain variable.
    let gone = dir.path().join("gone");
    env(&[("CONDE_ADMIN_KEY", "q7Zx2Lm9Vt4Rk8Wp3Ny6Hb1Jc5Fd0GsUe"), ("CONDE_ADMIN_KEY_FILE", gone.to_str().unwrap())]);
    let e = WebSetup::from_env(store.clone()).unwrap_err().to_string();
    assert!(e.starts_with(&format!("CONDE_ADMIN_KEY_FILE: cannot read {}", gone.display())), "{e}");
    let mut vars = OIDC.to_vec();
    vars.push(("OIDC_CLIENT_SECRET_FILE", gone.to_str().unwrap()));
    env(&vars);
    let e = error(WebConfig::from_env());
    assert!(e.starts_with("OIDC_CLIENT_SECRET_FILE: cannot read"), "{e}");
    // A short key from a file is refused like a short variable.
    std::fs::write(&key_file, "short\n").unwrap();
    env(&[("CONDE_ADMIN_KEY_FILE", key_path)]);
    assert_eq!(WebSetup::from_env(store).unwrap_err().to_string(), "CONDE_ADMIN_KEY must be at least 32 characters");
    env(&[]);
}

fn the_web_configuration_comes_from_the_environment() {
    let store = Arc::new(WebBoard::new(Arc::new(Dispatcher::open_in_memory().unwrap())));

    // Nothing set (or only empty values): web sign-in and the admin API are off.
    env(&[]);
    assert!(WebConfig::from_env().unwrap().is_none());
    assert!(WebSetup::from_env(store.clone()).unwrap().is_none());
    env(&[("ENTRA_TENANT_ID", ""), ("CONDE_ADMIN_KEY", "")]);
    assert!(WebSetup::from_env(store.clone()).unwrap().is_none(), "an empty value counts as unset");

    // Some of the four: the first missing one is named.
    env(&[("ENTRA_TENANT_ID", "t")]);
    assert_eq!(error(WebConfig::from_env()), "OIDC_CLIENT_ID (or ENTRA_CLIENT_ID) is not set");
    env(&with(&[("ENTRA_CLIENT_SECRET", "")]));
    assert_eq!(error(WebConfig::from_env()), "OIDC_CLIENT_SECRET (or ENTRA_CLIENT_SECRET) is not set");

    // The redirect must be an https URL ending in /auth/callback (the public origin's).
    for bad in ["http://conde.example/auth/callback", "https://conde.example/callback", "conde.example/auth/callback"] {
        env(&with(&[("ENTRA_REDIRECT_URI", bad)]));
        assert_eq!(
            error(WebConfig::from_env()),
            format!("OIDC_REDIRECT_URI '{bad}' must be an https URL ending in /auth/callback"),
            "{bad}"
        );
    }

    // All four: the origin and every URL derived from it, the defaults for the remote MCP.
    env(&ENTRA);
    let c = WebConfig::from_env().unwrap().unwrap();
    assert_eq!(c.issuer, "https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0");
    assert_eq!((c.entra_tenant.as_deref(), c.client_id.as_str(), c.client_secret.as_str()),
               (Some("2e51b484-b008-4fc0-8f70-555c1317b555"), "conde-client", "conde-secret"));
    assert_eq!(c.public_url, "https://conde.example");
    assert_eq!(c.post_logout_redirect_uri, "https://conde.example/");
    assert_eq!((c.session_idle.as_secs(), c.session_max.as_secs(), c.login_ttl.as_secs(), c.cli_code_ttl.as_secs()),
               (8 * 3600, 24 * 3600, 600, 300));
    // Printed (logs, panics), the configuration names its identity but never the client secret.
    let shown = format!("{c:?}");
    assert!(shown.contains("conde-client") && shown.contains("https://conde.example/auth/callback"), "{shown}");
    assert!(!shown.contains("conde-secret"), "{shown}");

    // The retired remote MCP's variables, still set in an older environment, are not read.
    env(&with(&[("CONDE_MCP_CLIENT_HOSTS", " , "), ("CONDE_MCP_GRANTS_PER_CLIENT", "0")]));
    assert_eq!(WebConfig::from_env().unwrap().unwrap().public_url, "https://conde.example");

    // The admin key: at least 32 characters; it alone turns the admin API on.
    env(&[("CONDE_ADMIN_KEY", "short")]);
    let refused = WebSetup::from_env(store.clone()).expect_err("a short key is refused").to_string();
    assert_eq!(refused, "CONDE_ADMIN_KEY must be at least 32 characters");
    let key = "q7Zx2Lm9Vt4Rk8Wp3Ny6Hb1Jc5Fd0GsUe";
    env(&[("CONDE_ADMIN_KEY", key)]);
    let setup = WebSetup::from_env(store.clone()).unwrap().unwrap();
    assert!(setup.entra.is_none() && setup.admin_key.as_deref() == Some(key));
    // Printed, the setup says the admin API is on, never the key nor any piece of it.
    let shown = format!("{setup:?}");
    assert!(shown.contains("admin: true"), "{shown}");
    for piece in key.as_bytes().windows(8).map(|w| std::str::from_utf8(w).unwrap()) {
        assert!(!shown.contains(piece), "{piece} in {shown}");
    }
    // Both: sign-in and the admin API; a bad Entra variable refuses the whole setup.
    let mut both = ENTRA.to_vec();
    both.push(("CONDE_ADMIN_KEY", key));
    env(&both);
    let setup = WebSetup::from_env(store.clone()).unwrap().unwrap();
    assert!(setup.entra.is_some() && setup.admin_key.is_some());
    both.retain(|(n, _)| *n != "ENTRA_CLIENT_ID");
    env(&both);
    assert_eq!(WebSetup::from_env(store).unwrap_err().to_string(), "OIDC_CLIENT_ID (or ENTRA_CLIENT_ID) is not set");
    env(&[]);
}

/// `CONDE_SLACK_BOT_TOKEN_FILE` alone turns the Slack worker on, an empty one leaves it off; an
/// unreadable one is an error, not a silent "off".
fn the_slack_token_may_come_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("slack-bot-token");
    std::fs::write(&file, "xoxb-from-the-file\n").unwrap();
    env(&[]);
    assert!(conde::slack::SlackConfig::from_env("dakai", None).unwrap().is_none());
    env(&[("CONDE_SLACK_BOT_TOKEN_FILE", file.to_str().unwrap())]);
    assert!(conde::slack::SlackConfig::from_env("dakai", None).unwrap().is_some());
    // a workspace without Slack mounts an empty file: paging is off, not an error
    let empty = dir.path().join("empty-token");
    for content in ["", "\n", "  \n"] {
        std::fs::write(&empty, content).unwrap();
        env(&[("CONDE_SLACK_BOT_TOKEN", "xoxb-plain"), ("CONDE_SLACK_BOT_TOKEN_FILE", empty.to_str().unwrap())]);
        assert!(conde::slack::SlackConfig::from_env("dakai", None).unwrap().is_none(), "{content:?}");
    }
    let gone = dir.path().join("gone");
    env(&[("CONDE_SLACK_BOT_TOKEN", "xoxb-plain"), ("CONDE_SLACK_BOT_TOKEN_FILE", gone.to_str().unwrap())]);
    let e = conde::slack::SlackConfig::from_env("dakai", None).unwrap_err().to_string();
    assert!(e.starts_with("CONDE_SLACK_BOT_TOKEN_FILE: cannot read"), "{e}");
    env(&[]);
}

/// `CONDE_MOVED_TO` (the board's new address) must be an http(s) URL; unset or blank, the server
/// serves as usual.
fn conde_moved_to_names_an_http_url() {
    env(&[]);
    assert_eq!(conde::cloud::moved_to().unwrap(), None);
    env(&[("CONDE_MOVED_TO", " ")]);
    assert_eq!(conde::cloud::moved_to().unwrap(), None);
    env(&[("CONDE_MOVED_TO", " https://dakai.conde.cloud ")]);
    assert_eq!(conde::cloud::moved_to().unwrap().as_deref(), Some("https://dakai.conde.cloud"));
    for bad in ["dakai.conde.cloud", "ftp://dakai.conde.cloud", "https://"] {
        env(&[("CONDE_MOVED_TO", bad)]);
        assert!(conde::cloud::moved_to().is_err(), "{bad}");
    }
    env(&[]);
}
