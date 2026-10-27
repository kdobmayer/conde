//! The server's web configuration from its environment (`ENTRA_*`, `CONDE_ADMIN_KEY`; the retired
//! remote MCP's `CONDE_MCP_*` are ignored), as the container sets it. It changes the process environment, so the
//! binary runs without the libtest harness (`harness = false`): its cases run on the main thread.

mod serial;

use std::sync::Arc;

use conde::dispatch::Dispatcher;
use conde::web_store::WebBoard;
use conde_remote::web::{WebConfig, WebSetup};

const VARS: [&str; 7] = [
    "ENTRA_TENANT_ID",
    "ENTRA_CLIENT_ID",
    "ENTRA_CLIENT_SECRET",
    "ENTRA_REDIRECT_URI",
    "CONDE_MCP_CLIENT_HOSTS",
    "CONDE_MCP_GRANTS_PER_CLIENT",
    "CONDE_ADMIN_KEY",
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

fn main() {
    serial::run("web_config", &[("the_web_configuration_comes_from_the_environment", the_web_configuration_comes_from_the_environment)]);
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
    assert_eq!(error(WebConfig::from_env()), "ENTRA_CLIENT_ID is not set");
    env(&with(&[("ENTRA_CLIENT_SECRET", "")]));
    assert_eq!(error(WebConfig::from_env()), "ENTRA_CLIENT_SECRET is not set");

    // The redirect must be an https URL ending in /auth/callback (the public origin's).
    for bad in ["http://conde.example/auth/callback", "https://conde.example/callback", "conde.example/auth/callback"] {
        env(&with(&[("ENTRA_REDIRECT_URI", bad)]));
        assert_eq!(
            error(WebConfig::from_env()),
            format!("ENTRA_REDIRECT_URI '{bad}' must be an https URL ending in /auth/callback"),
            "{bad}"
        );
    }

    // All four: the origin and every URL derived from it, the defaults for the remote MCP.
    env(&ENTRA);
    let c = WebConfig::from_env().unwrap().unwrap();
    assert_eq!(c.issuer, "https://login.microsoftonline.com/2e51b484-b008-4fc0-8f70-555c1317b555/v2.0");
    assert_eq!((c.tenant_id.as_str(), c.client_id.as_str(), c.client_secret.as_str()),
               ("2e51b484-b008-4fc0-8f70-555c1317b555", "conde-client", "conde-secret"));
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
    assert_eq!(WebSetup::from_env(store).unwrap_err().to_string(), "ENTRA_CLIENT_ID is not set");
    env(&[]);
}
