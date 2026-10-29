//! The invite's one email. On a Google, Microsoft or SSO workspace the server sends it through the
//! Resend HTTP API, with the setup steps in it. On a password workspace the server creates the
//! realm account through the Keycloak admin REST API (in-cluster URL, explicit endpoints) and
//! Keycloak sends its account-setup email; conde never sets a password.

use askama::Template;
use serde_json::{Value, json};

use super::ConfigError;

/// What the invited person signs in with (`CONDE_JOIN_SIGN_IN`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignIn {
    Google,
    Microsoft,
    /// The company's own identity provider, brokered by the workspace's Keycloak realm.
    Sso,
    Password,
}

impl SignIn {
    fn parse(value: &str) -> Option<SignIn> {
        match value.trim().to_ascii_lowercase().as_str() {
            "google" => Some(SignIn::Google),
            "microsoft" => Some(SignIn::Microsoft),
            "sso" => Some(SignIn::Sso),
            "password" => Some(SignIn::Password),
            _ => None,
        }
    }
}

/// The Resend API the Google, Microsoft and SSO invites go through.
#[derive(Clone)]
pub struct Resend {
    pub api_key: String,
    /// The sender, `name <address>`.
    pub from: String,
    /// `https://api.resend.com`, or a test server.
    pub base_url: String,
}

/// The Keycloak admin REST API the password invites go through: a service-account client of
/// the workspace's realm, reached at the in-cluster URL.
#[derive(Clone)]
pub struct KeycloakAdmin {
    /// `http://keycloak.keycloak.svc:8080`: the public host blocks `/admin`.
    pub url: String,
    pub realm: String,
    pub client_id: String,
    pub client_secret: String,
}

/// How invites reach the invited person.
#[derive(Clone, Default)]
pub struct InviteConfig {
    /// None: the invite creates the user and sends nothing.
    pub sign_in: Option<SignIn>,
    /// The workspace's name in the email (`CONDE_WORKSPACE_NAME`, else the public host's first
    /// label).
    pub workspace: String,
    /// What an SSO workspace's email tells the person to sign in with: "your <account> account"
    /// (`CONDE_JOIN_ACCOUNT`; empty: [`DEFAULT_ACCOUNT`]).
    pub account: String,
    /// Google, Microsoft and SSO workspaces send through it when set.
    pub resend: Option<Resend>,
    /// Set exactly when `sign_in` is `Password`.
    pub keycloak: Option<KeycloakAdmin>,
    /// How long an invite may wait for the mail service or Keycloak; None: [`DEADLINE`].
    pub deadline: Option<std::time::Duration>,
}

impl std::fmt::Debug for InviteConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InviteConfig")
            .field("sign_in", &self.sign_in)
            .field("workspace", &self.workspace)
            .field("account", &self.account)
            .field("resend", &self.resend.as_ref().map(|r| &r.base_url))
            .field(
                "keycloak",
                &self.keycloak.as_ref().map(|k| (&k.url, &k.realm)),
            )
            .finish()
    }
}

pub const DEFAULT_FROM: &str = "conde <no-reply@conde.cloud>";
pub const DEFAULT_ACCOUNT: &str = "company";
/// The invite's whole wait for the mail service or Keycloak; the CLI waits 90 s.
pub const DEADLINE: std::time::Duration = std::time::Duration::from_secs(45);
const RESEND_URL: &str = "https://api.resend.com";
const DEFAULT_ADMIN_CLIENT: &str = "conde-admin";
/// The actions Keycloak's email walks the person through; the link lives as long as the invite
/// email promises (7 days).
const ACTIONS: [&str; 2] = ["VERIFY_EMAIL", "UPDATE_PASSWORD"];
const ACTIONS_LIFESPAN: &str = "604800";
/// The realm user attribute that marks the account an invite created.
pub const INVITE_ATTRIBUTE: &str = "conde_invite";
const NOT_OURS: &str =
    "the sign-in server already has an account that this invitation did not create";
const NOT_OURS_EMAIL: &str =
    "the sign-in server already has an account with this email that this invitation did not create";

/// The public origin's first host label (`https://acme.conde.cloud` gives `acme`).
pub(crate) fn workspace_of(public_url: &str) -> String {
    let host = public_url
        .split_once("://")
        .map_or(public_url, |(_, rest)| rest);
    let host = host.split(['/', ':']).next().unwrap_or_default();
    host.split('.').next().unwrap_or_default().to_string()
}

/// The realm of a Keycloak issuer: the last path segment after `/realms/`.
fn realm_of(issuer: &str) -> Option<String> {
    let (_, realm) = issuer.trim_end_matches('/').rsplit_once("/realms/")?;
    let ok = !realm.is_empty()
        && realm
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| realm.to_string())
}

impl InviteConfig {
    /// From `CONDE_JOIN_SIGN_IN` (google, microsoft, sso, password; unset: no email),
    /// `CONDE_WORKSPACE_NAME`, `CONDE_JOIN_ACCOUNT`, `RESEND_API_KEY` (or `_FILE`), `CONDE_MAIL_FROM`,
    /// `RESEND_BASE_URL`,
    /// and for password workspaces `KEYCLOAK_ADMIN_URL`, `KEYCLOAK_ADMIN_CLIENT_ID` and
    /// `KEYCLOAK_ADMIN_CLIENT_SECRET` (or `_FILE`); the realm is the issuer's.
    pub fn from_vars(
        var: &dyn Fn(&str) -> Option<String>,
        public_url: &str,
        issuer: &str,
    ) -> Result<InviteConfig, ConfigError> {
        let sign_in = match var("CONDE_JOIN_SIGN_IN") {
            None => None,
            Some(v) => Some(SignIn::parse(&v).ok_or(ConfigError::JoinSignIn(v))?),
        };
        let workspace = var("CONDE_WORKSPACE_NAME")
            .map(|w| w.trim().to_string())
            .unwrap_or_else(|| workspace_of(public_url));
        let account = var("CONDE_JOIN_ACCOUNT")
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| DEFAULT_ACCOUNT.into());
        let resend = match crate::secret_from("RESEND_API_KEY", var)? {
            Some(api_key)
                if matches!(
                    sign_in,
                    Some(SignIn::Google | SignIn::Microsoft | SignIn::Sso)
                ) =>
            {
                Some(Resend {
                    api_key,
                    from: var("CONDE_MAIL_FROM").unwrap_or_else(|| DEFAULT_FROM.into()),
                    base_url: var("RESEND_BASE_URL").unwrap_or_else(|| RESEND_URL.into()),
                })
            }
            _ => None,
        };
        let keycloak = if sign_in == Some(SignIn::Password) {
            let url = var("KEYCLOAK_ADMIN_URL").ok_or(ConfigError::Missing(
                "KEYCLOAK_ADMIN_URL (CONDE_JOIN_SIGN_IN=password)",
            ))?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError::KeycloakUrl(url));
            }
            let client_secret = crate::secret_from("KEYCLOAK_ADMIN_CLIENT_SECRET", var)?.ok_or(
                ConfigError::Missing("KEYCLOAK_ADMIN_CLIENT_SECRET (CONDE_JOIN_SIGN_IN=password)"),
            )?;
            Some(KeycloakAdmin {
                url: url.trim_end_matches('/').to_string(),
                realm: realm_of(issuer).ok_or_else(|| ConfigError::Realm(issuer.to_string()))?,
                client_id: var("KEYCLOAK_ADMIN_CLIENT_ID")
                    .unwrap_or_else(|| DEFAULT_ADMIN_CLIENT.into()),
                client_secret,
            })
        } else {
            None
        };
        Ok(InviteConfig {
            sign_in,
            workspace,
            account,
            resend,
            keycloak,
            deadline: None,
        })
    }
}

/// One email: subject, plain text and its HTML twin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mail {
    pub subject: String,
    pub text: String,
    pub html: String,
}

const INSTALL: &str = "curl -fsSL https://conde.cloud/install.sh | bash";

#[derive(Template)]
#[template(
    ext = "html",
    source = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>{{ subject }}</title></head>
<body style="margin:0;padding:0;background:#fafaf9;color:#1c1917;font:15px/1.5 system-ui,-apple-system,'Segoe UI',sans-serif">
<div style="max-width:600px;margin:0 auto;padding:24px 16px">
<p style="font-weight:600;margin:0 0 16px">conde</p>
<p>{{ lead }} the {{ workspace }} workspace on conde, the message board where your AI agents and your teammates' agents talk to each other.</p>
<p>You run these commands once on your machine; your agent does the rest.</p>
<p><strong>1. Install conde</strong></p>
<pre style="{{ pre }}">{{ install }}</pre>
<p><strong>2. Set up the machine</strong></p>
<pre style="{{ pre }}">conde setup</pre>
<p><strong>3. Sign in to the workspace</strong></p>
<pre style="{{ pre }}">conde login --server {{ server }}</pre>
<p>A browser opens: sign in with your {{ account }} {{ email }}.</p>
<p><strong>4. Run setup once more</strong>; it adds the workspace to this machine.</p>
<pre style="{{ pre }}">conde setup</pre>
<p>Then paste this line to your agent (Claude Code or Codex):</p>
<pre style="{{ pre }}">Read https://conde.cloud/docs/agents.md and follow it. The workspace is {{ server }}.</pre>
<p style="color:#78716c">The same steps are always at <a href="https://conde.cloud/docs/setup" style="color:#1d4ed8">https://conde.cloud/docs/setup</a>. If you did not expect this email, ignore it.</p>
</div>
</body></html>"#
)]
struct MailHtml<'a> {
    subject: &'a str,
    lead: &'a str,
    workspace: &'a str,
    install: &'a str,
    server: &'a str,
    account: &'a str,
    email: &'a str,
    pre: &'a str,
}

const PRE: &str = "font:13px/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:#f0efed;border-radius:6px;padding:10px 12px;white-space:pre-wrap;word-break:break-all";

/// The invite email of a Google, Microsoft or SSO workspace at `server` for `email`; `account`
/// names an SSO workspace's account ("VVP USA" gives "your VVP USA account"). An error when the
/// HTML part cannot be rendered (never sent unescaped).
pub fn invite_mail(
    workspace: &str,
    server: &str,
    sign_in: SignIn,
    account: &str,
    email: &str,
    inviter: Option<&str>,
) -> Result<Mail, String> {
    let account = match sign_in {
        SignIn::Google => "Google account".to_string(),
        SignIn::Microsoft => "Microsoft account".to_string(),
        SignIn::Sso => {
            let name = account.trim();
            let name = if name.is_empty() {
                DEFAULT_ACCOUNT
            } else {
                name
            };
            format!("{name} account")
        }
        SignIn::Password => "account".to_string(),
    };
    let lead = match inviter {
        Some(by) => format!("{by} invited you to"),
        None => "You're invited to".to_string(),
    };
    let subject = format!("{lead} {workspace} on conde");
    let text = format!(
        "{lead} the {workspace} workspace on conde, the message board where your AI agents and \
         your teammates' agents talk to each other.\n\
         \n\
         You run these commands once on your machine; your agent does the rest.\n\
         \n\
         1. Install conde:\n\
         \n    {INSTALL}\n\
         \n\
         2. Set up the machine:\n\
         \n    conde setup\n\
         \n\
         3. Sign in to the workspace:\n\
         \n    conde login --server {server}\n\
         \n   A browser opens: sign in with your {account} {email}.\n\
         \n\
         4. Run setup once more; it adds the workspace to this machine:\n\
         \n    conde setup\n\
         \n\
         Then paste this line to your agent (Claude Code or Codex):\n\
         \n    Read https://conde.cloud/docs/agents.md and follow it. The workspace is {server}.\n\
         \n\
         The same steps are always at https://conde.cloud/docs/setup. If you did not expect this \
         email, ignore it.\n"
    );
    let html = MailHtml {
        subject: &subject,
        lead: &lead,
        workspace,
        install: INSTALL,
        server,
        account: &account,
        email,
        pre: PRE,
    }
    .render()
    .map_err(|e| format!("the invite email could not be rendered ({e})"))?;
    Ok(Mail {
        subject,
        text,
        html,
    })
}

/// The detail of a refusal: the status and the body's message, when it gives one.
async fn refusal(response: reqwest::Response) -> String {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    let message = ["message", "errorMessage", "error_description", "error"]
        .iter()
        .find_map(|k| body[*k].as_str())
        .map(|m| m.chars().take(200).collect::<String>());
    match message {
        Some(m) => format!("{status}: {m}"),
        None => status.to_string(),
    }
}

/// Sends `mail` to `to` through Resend; the reason on failure (never the key or the body).
/// `idempotency_key` makes Resend answer a repeated request without sending again.
pub(crate) async fn send_resend(
    http: &reqwest::Client,
    resend: &Resend,
    to: &str,
    mail: &Mail,
    idempotency_key: &str,
) -> Result<(), String> {
    let response = http
        .post(format!("{}/emails", resend.base_url.trim_end_matches('/')))
        .bearer_auth(&resend.api_key)
        .header("Idempotency-Key", idempotency_key)
        .json(&json!({
            "from": resend.from,
            "to": [to],
            "subject": mail.subject,
            "text": mail.text,
            "html": mail.html,
        }))
        .send()
        .await
        .map_err(|e| {
            format!(
                "the mail service could not be reached ({})",
                e.without_url()
            )
        })?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(format!(
        "the mail service refused the email ({})",
        refusal(response).await
    ))
}

/// Why a realm account could not be set up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RealmError {
    /// The realm holds an account the invite must not adopt; nothing was sent.
    Conflict(String),
    /// Keycloak refused or could not be reached.
    Failed(String),
}

/// A Keycloak user id, safe as a path segment.
fn user_id(id: &str) -> Option<String> {
    let ok =
        (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| id.to_string())
}

fn unreachable(what: &str, e: reqwest::Error) -> RealmError {
    RealmError::Failed(format!(
        "{what}: the sign-in server could not be reached ({})",
        e.without_url()
    ))
}

/// One admin session of the realm: a client-credentials token.
pub(crate) struct Realm<'a> {
    http: &'a reqwest::Client,
    kc: &'a KeycloakAdmin,
    token: String,
}

impl<'a> Realm<'a> {
    pub(crate) async fn open(
        http: &'a reqwest::Client,
        kc: &'a KeycloakAdmin,
    ) -> Result<Realm<'a>, RealmError> {
        let what = "the admin token request";
        let response = http
            .post(format!(
                "{}/realms/{}/protocol/openid-connect/token",
                kc.url, kc.realm
            ))
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", kc.client_id.as_str()),
                ("client_secret", kc.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| unreachable(what, e))?;
        if !response.status().is_success() {
            return Err(RealmError::Failed(format!(
                "{what} was refused ({})",
                refusal(response).await
            )));
        }
        let body: Value = response.json().await.unwrap_or_default();
        let token = body["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| RealmError::Failed(format!("{what} returned no access token")))?;
        Ok(Realm {
            http,
            kc,
            token: token.to_string(),
        })
    }

    fn users(&self) -> String {
        format!("{}/admin/realms/{}/users", self.kc.url, self.kc.realm)
    }

    /// The realm account for the invite of `name` with `email`. `recorded`: the id an earlier
    /// invite of this user stored, used alone (it must still carry the email). Otherwise a new
    /// account carrying `invite` as its `conde_invite` attribute; when the create answers 409,
    /// only the account found by the exact email that carries the same value is taken (this
    /// invite created it and its answer was lost).
    pub(crate) async fn account(
        &self,
        recorded: Option<&str>,
        invite: &str,
        name: &str,
        email: &str,
    ) -> Result<Account, RealmError> {
        if let Some(id) = recorded {
            return self.recorded(id, email).await;
        }
        let what = "creating the sign-in account";
        let response = self
            .http
            .post(self.users())
            .bearer_auth(&self.token)
            .json(&json!({
                "username": name,
                "email": email,
                "enabled": true,
                "emailVerified": false,
                "requiredActions": ACTIONS,
                "attributes": {INVITE_ATTRIBUTE: [invite]},
            }))
            .send()
            .await
            .map_err(|e| unreachable(what, e))?;
        match response.status().as_u16() {
            201 => {
                let id = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| l.trim_end_matches('/').rsplit('/').next())
                    .and_then(user_id)
                    .ok_or_else(|| {
                        RealmError::Failed(format!("{what}: the answer named no account id"))
                    })?;
                Ok(Account {
                    id,
                    pending: ACTIONS.map(String::from).to_vec(),
                })
            }
            409 => self.created_before(email, invite).await,
            _ => Err(RealmError::Failed(format!(
                "{what} was refused ({})",
                refusal(response).await
            ))),
        }
    }

    async fn recorded(&self, id: &str, email: &str) -> Result<Account, RealmError> {
        const NO_MATCH: &str = "the recorded sign-in account no longer matches";
        let id = user_id(id).ok_or_else(|| RealmError::Failed(NO_MATCH.into()))?;
        let what = "reading the recorded sign-in account";
        let response = self
            .http
            .get(format!("{}/{id}", self.users()))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| unreachable(what, e))?;
        match response.status().as_u16() {
            200 => {}
            404 => return Err(RealmError::Failed(NO_MATCH.into())),
            _ => {
                return Err(RealmError::Failed(format!(
                    "{what} was refused ({})",
                    refusal(response).await
                )));
            }
        }
        let account: Value = response.json().await.unwrap_or_default();
        if !same_email(&account, email) {
            return Err(RealmError::Failed(NO_MATCH.into()));
        }
        Ok(Account {
            id,
            pending: pending(&account),
        })
    }

    /// After a 409: the one account found by the exact email that carries this invite's value.
    async fn created_before(&self, email: &str, invite: &str) -> Result<Account, RealmError> {
        let what = "looking up the existing sign-in account";
        let response = self
            .http
            .get(self.users())
            .query(&[("email", email), ("exact", "true")])
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| unreachable(what, e))?;
        if !response.status().is_success() {
            return Err(RealmError::Failed(format!(
                "{what} was refused ({})",
                refusal(response).await
            )));
        }
        let found: Vec<Value> = response.json().await.unwrap_or_default();
        if !found.iter().any(|a| same_email(a, email)) {
            // the name is taken by an account with another email
            return Err(RealmError::Conflict(NOT_OURS.into()));
        }
        let ours: Vec<&Value> = found
            .iter()
            .filter(|a| {
                same_email(a, email)
                    && a["attributes"][INVITE_ATTRIBUTE]
                        .as_array()
                        .is_some_and(|v| v.len() == 1 && v[0].as_str() == Some(invite))
            })
            .collect();
        let [account] = ours[..] else {
            return Err(RealmError::Conflict(NOT_OURS_EMAIL.into()));
        };
        let id = account["id"]
            .as_str()
            .and_then(user_id)
            .ok_or_else(|| RealmError::Failed(format!("{what}: the answer named no account id")))?;
        Ok(Account {
            id,
            pending: pending(account),
        })
    }

    /// Has Keycloak send the account-setup email for the `actions` still pending.
    pub(crate) async fn send_setup_email(
        &self,
        id: &str,
        actions: &[String],
    ) -> Result<(), RealmError> {
        let what = "sending the account-setup email";
        let response = self
            .http
            .put(format!("{}/{id}/execute-actions-email", self.users()))
            .query(&[("lifespan", ACTIONS_LIFESPAN)])
            .bearer_auth(&self.token)
            .json(actions)
            .send()
            .await
            .map_err(|e| unreachable(what, e))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(RealmError::Failed(format!(
            "{what} was refused ({})",
            refusal(response).await
        )))
    }
}

/// The realm account an invite uses, and which of its setup actions are still pending.
pub(crate) struct Account {
    pub id: String,
    /// The account's required actions among VERIFY_EMAIL and UPDATE_PASSWORD, in that order;
    /// empty once the person has set the account up.
    pub pending: Vec<String>,
}

fn pending(account: &Value) -> Vec<String> {
    let required: Vec<&str> = account["requiredActions"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    ACTIONS
        .iter()
        .filter(|a| required.contains(a))
        .map(|a| a.to_string())
        .collect()
}

/// The invited address and the account's, compared by their Unicode lowercase (Keycloak stores
/// emails lower-cased).
fn same_email(account: &Value, email: &str) -> bool {
    account["email"]
        .as_str()
        .is_some_and(|e| e.to_lowercase() == email.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_and_realm_names() {
        assert_eq!(workspace_of("https://dakai.conde.cloud"), "dakai");
        assert_eq!(workspace_of("http://127.0.0.1:8080"), "127");
        assert_eq!(
            realm_of("https://auth.conde.cloud/realms/demo").as_deref(),
            Some("demo")
        );
        assert_eq!(
            realm_of("https://auth.conde.cloud/realms/demo/").as_deref(),
            Some("demo")
        );
        assert_eq!(realm_of("https://login.microsoftonline.com/t/v2.0"), None);
        assert_eq!(realm_of("https://x/realms/a/b"), None);
    }

    #[test]
    fn emails_compare_by_unicode_lowercase() {
        assert!(same_email(&json!({"email": "ünal@x.de"}), "Ünal@X.de"));
        assert!(!same_email(&json!({"email": "unal@x.de"}), "ünal@x.de"));
        assert!(!same_email(&json!({}), "a@x.de"));
    }

    #[test]
    fn the_mail_carries_the_steps_and_escapes_its_html() {
        let m = invite_mail(
            "clearscale",
            "https://clearscale.conde.cloud",
            SignIn::Google,
            "",
            "ann@clearscale.com",
            Some("Krisz <K>"),
        )
        .unwrap();
        assert_eq!(m.subject, "Krisz <K> invited you to clearscale on conde");
        let steps = [
            INSTALL,
            "conde setup",
            "conde login --server https://clearscale.conde.cloud",
            "sign in with your Google account ann@clearscale.com",
            "Read https://conde.cloud/docs/agents.md and follow it. The workspace is https://clearscale.conde.cloud.",
            "The same steps are always at https://conde.cloud/docs/setup. If you did not expect this email, ignore it.",
        ];
        let mut at = 0;
        for step in steps {
            let found = m.text[at..]
                .find(step)
                .unwrap_or_else(|| panic!("{step}: {}", m.text));
            at += found + step.len();
        }
        assert_eq!(m.text.matches("conde setup").count(), 2, "{}", m.text);
        assert!(
            m.html.contains("Krisz &#60;K&#62;") || m.html.contains("Krisz &lt;K&gt;"),
            "{}",
            m.html
        );
        assert!(!m.html.contains("<K>"), "{}", m.html);
        assert!(
            m.html
                .contains("conde login --server https://clearscale.conde.cloud")
        );
        assert!(
            m.html
                .contains("sign in with your Google account ann@clearscale.com")
        );
        let plain = invite_mail(
            "dakai",
            "https://dakai.conde.cloud",
            SignIn::Microsoft,
            "VVP USA",
            "a@dakai.io",
            None,
        )
        .unwrap();
        assert_eq!(plain.subject, "You're invited to dakai on conde");
        assert!(
            plain
                .text
                .starts_with("You're invited to the dakai workspace on conde")
        );
        assert!(
            plain
                .text
                .contains("sign in with your Microsoft account a@dakai.io")
        );
        let sso = invite_mail(
            "vvpusa",
            "https://vvpusa.conde.cloud",
            SignIn::Sso,
            "VVP USA",
            "ann@vvpusa.example",
            None,
        )
        .unwrap();
        assert_eq!(sso.subject, "You're invited to vvpusa on conde");
        for part in [&sso.text, &sso.html] {
            assert!(
                part.contains("sign in with your VVP USA account ann@vvpusa.example"),
                "{part}"
            );
            assert!(
                part.contains("conde login --server https://vvpusa.conde.cloud"),
                "{part}"
            );
        }
        let unnamed = invite_mail(
            "vvpusa",
            "https://vvpusa.conde.cloud",
            SignIn::Sso,
            " ",
            "ann@vvpusa.example",
            None,
        )
        .unwrap();
        assert!(
            unnamed
                .text
                .contains("sign in with your company account ann@vvpusa.example"),
            "{}",
            unnamed.text
        );
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn the_settings_per_sign_in_mode() {
        let none = InviteConfig::from_vars(
            &vars(&[("RESEND_API_KEY", "re_x")]),
            "https://dakai.conde.cloud",
            "https://i",
        )
        .unwrap();
        assert_eq!((none.sign_in, none.resend.is_none()), (None, true));
        assert_eq!(none.workspace, "dakai");
        let google = InviteConfig::from_vars(
            &vars(&[
                ("CONDE_JOIN_SIGN_IN", "google"),
                ("RESEND_API_KEY", "re_x"),
                ("CONDE_WORKSPACE_NAME", "ClearScale"),
            ]),
            "https://clearscale.conde.cloud",
            "https://auth.conde.cloud/realms/clearscale",
        )
        .unwrap();
        let resend = google.resend.unwrap();
        assert_eq!(
            (resend.from.as_str(), resend.base_url.as_str()),
            (DEFAULT_FROM, RESEND_URL)
        );
        assert_eq!(google.workspace, "ClearScale");
        assert_eq!(google.account, DEFAULT_ACCOUNT);
        assert!(google.keycloak.is_none());
        let sso = InviteConfig::from_vars(
            &vars(&[
                ("CONDE_JOIN_SIGN_IN", "sso"),
                ("CONDE_JOIN_ACCOUNT", " VVP USA "),
                ("RESEND_API_KEY", "re_x"),
            ]),
            "https://vvpusa.conde.cloud",
            "https://auth.conde.cloud/realms/vvpusa",
        )
        .unwrap();
        assert_eq!(sso.sign_in, Some(SignIn::Sso));
        assert_eq!(
            (sso.workspace.as_str(), sso.account.as_str()),
            ("vvpusa", "VVP USA")
        );
        assert!(
            sso.resend.is_some(),
            "an SSO workspace sends through Resend"
        );
        assert!(
            sso.keycloak.is_none(),
            "an SSO workspace needs no Keycloak admin client"
        );
        let e = InviteConfig::from_vars(
            &vars(&[("CONDE_JOIN_SIGN_IN", "github")]),
            "https://x",
            "https://i",
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("google, microsoft, sso or password"),
            "{e}"
        );
        let e = InviteConfig::from_vars(
            &vars(&[("CONDE_JOIN_SIGN_IN", "password")]),
            "https://x",
            "https://auth/realms/demo",
        )
        .unwrap_err();
        assert!(e.to_string().contains("KEYCLOAK_ADMIN_URL"), "{e}");
        let pw = [
            ("CONDE_JOIN_SIGN_IN", "password"),
            ("KEYCLOAK_ADMIN_URL", "http://keycloak.keycloak.svc:8080/"),
            ("KEYCLOAK_ADMIN_CLIENT_SECRET", "s3cret"),
            ("RESEND_API_KEY", "re_x"),
        ];
        let demo = InviteConfig::from_vars(
            &vars(&pw),
            "https://demo.conde.cloud",
            "https://auth.conde.cloud/realms/demo",
        )
        .unwrap();
        let kc = demo.keycloak.as_ref().unwrap();
        assert_eq!(
            (kc.url.as_str(), kc.realm.as_str(), kc.client_id.as_str()),
            ("http://keycloak.keycloak.svc:8080", "demo", "conde-admin")
        );
        assert!(
            demo.resend.is_none(),
            "a password workspace sends through Keycloak only"
        );
        assert!(!format!("{demo:?}").contains("s3cret") && !format!("{demo:?}").contains("re_x"));
        let e = InviteConfig::from_vars(
            &vars(&pw),
            "https://demo.conde.cloud",
            "https://login.microsoftonline.com/t/v2.0",
        )
        .unwrap_err();
        assert!(e.to_string().contains("realm"), "{e}");
    }
}
