//! The server's side of the agent machines: where a person's VM is deployed (`CONDE_MACHINES_*`),
//! the Azure Resource Manager client of the server's own managed identity (id-conde), and the two
//! triggers of a deployment: a person's first sign-in and the page's "Set up my agent"
//! (`machine.setup`, whose answer says when a PUT is due). The deployment is read on demand by
//! the page's `machine.status` and `machine.setup`, never by a timer.
//!
//! Azure calls, all with id-conde's token for `https://management.azure.com/`:
//! - the token: `GET $IDENTITY_ENDPOINT` (Container Apps), cached until 5 minutes before expiry;
//! - the deployment: `PUT .../resourceGroups/<rg>/providers/Microsoft.Resources/deployments/<host>`
//!   from the template spec version, Incremental; a 200 or 201 is accepted, anything else is the
//!   record's error (the next setup retries);
//! - its state: `GET` of the same deployment, at most once every 30 s per pending record; a failed
//!   read changes nothing (the next read retries);
//! - the VM: `GET .../providers/Microsoft.Compute/virtualMachines/<host>` for its identity's
//!   `principalId`, only from an enrol whose identity no record has yet;
//! - the removal, only from an admin's disable of the owner ([`remove`]): the deployment's cancel
//!   while it runs, the template's one login assignment on the VM, the VM's DELETE and its reads
//!   until 404.
//!
//! The VM joins through `POST /v1/machines/enrol` ([`enrol`]) with its own identity's v1 token for
//! the conde-machines audience: the host key is minted once per VM identity.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openidconnect::JsonWebKey as _;
use openidconnect::core::{CoreJsonWebKeySet, CoreJwsSigningAlgorithm};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use super::ratelimit::ClientIp;
use super::session::digest;
use super::{ConfigError, Enrolled, Enrollment, WebState, WebUser};

/// The VM size a deployment asks for when `CONDE_MACHINES_VM_SIZE` is unset.
pub const DEFAULT_VM_SIZE: &str = "Standard_B2as_v2";
const ARM_RESOURCE: &str = "https://management.azure.com/";
const IDENTITY_API_VERSION: &str = "2019-08-01";
/// A cached token is asked for again this long before it expires.
const RENEW_BEFORE_SECS: u64 = 300;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Resource Manager's public endpoint.
pub const ARM_URL: &str = "https://management.azure.com";
const DEPLOYMENTS_API_VERSION: &str = "2021-04-01";
const VM_API_VERSION: &str = "2024-07-01";
const ROLES_API_VERSION: &str = "2022-04-01";
/// Virtual Machine Administrator Login: the role of the template's one assignment to the owner.
const LOGIN_ROLE: &str = "1c0163c0-47e6-4577-8991-ea5c82e286e4";
/// How often the removal reads a deployment or a VM it waits for, unless configured.
pub const POLL_EVERY: Duration = Duration::from_secs(15);
/// How many reads the removal waits for each (10 minutes at [`POLL_EVERY`]).
const POLLS: u32 = 40;
/// Entra's sign-in origin; a tenant's v1 signing keys are at `<origin>/<tenant>/discovery/keys`.
pub const LOGIN_URL: &str = "https://login.microsoftonline.com";
/// The tenant's signing keys are read again after this long, and at once for an unknown `kid`.
const KEYS_MAX_AGE: Duration = Duration::from_secs(3600);
/// A pending record's deployment is read at most this often (seconds).
pub const DEPLOY_CHECK_EVERY: i64 = 30;
/// The longest Azure error text kept on a record.
const ERROR_CHARS: usize = 500;

const REQUIRED: [&str; 5] = [
    "CONDE_MACHINES_SUBSCRIPTION",
    "CONDE_MACHINES_RESOURCE_GROUP",
    "CONDE_MACHINES_TEMPLATE_SPEC",
    "CONDE_MACHINES_AUDIENCE",
    "CONDE_MACHINES_IDENTITY_CLIENT_ID",
];

/// Where the people's agent VMs are deployed and what their tokens are checked against.
#[derive(Clone, Debug)]
pub struct MachinesConfig {
    pub subscription: String,
    /// The resource group the deployments and the VMs are in (`conde-vm`).
    pub resource_group: String,
    /// The template spec version's resource id (`.../templateSpecs/agent-vm/versions/<n>`).
    pub template_spec: String,
    /// The `conde-machines` application id: the `aud` a VM's token must carry, passed to every
    /// deployment as `condeResource`.
    pub audience: String,
    pub vm_size: String,
    pub identity: ManagedIdentity,
    /// Resource Manager's base URL ([`ARM_URL`]; a test's fake otherwise).
    pub arm_url: String,
    /// Entra's sign-in origin, where the tenant's signing keys are read ([`LOGIN_URL`]; a test's
    /// fake otherwise).
    pub login_url: String,
    /// The removal's wait between reads ([`POLL_EVERY`]; a test's shorter one otherwise).
    pub poll_every: Duration,
}

/// The server's user-assigned identity as the Container Apps identity endpoint serves it.
#[derive(Clone)]
pub struct ManagedIdentity {
    /// `IDENTITY_ENDPOINT`, set by the platform.
    pub endpoint: String,
    /// `IDENTITY_HEADER`, set by the platform; sent as `X-IDENTITY-HEADER`, never shown.
    pub header: String,
    /// id-conde's client id: the app may carry more than one identity.
    pub client_id: String,
}

impl std::fmt::Debug for ManagedIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedIdentity")
            .field("endpoint", &self.endpoint)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl MachinesConfig {
    /// From the process environment; see [`MachinesConfig::from_vars`].
    pub fn from_env() -> Result<Option<MachinesConfig>, ConfigError> {
        MachinesConfig::from_vars(|k| std::env::var(k).ok())
    }

    /// From `CONDE_MACHINES_SUBSCRIPTION`, `CONDE_MACHINES_RESOURCE_GROUP`,
    /// `CONDE_MACHINES_TEMPLATE_SPEC`, `CONDE_MACHINES_AUDIENCE`,
    /// `CONDE_MACHINES_IDENTITY_CLIENT_ID` and the optional `CONDE_MACHINES_VM_SIZE`, with the
    /// platform's `IDENTITY_ENDPOINT` and `IDENTITY_HEADER`. `Ok(None)` when no `CONDE_MACHINES_*`
    /// is set; an error naming the first missing one when only some are.
    pub fn from_vars(
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<MachinesConfig>, ConfigError> {
        let var = |name: &str| var(name).filter(|v| !v.is_empty());
        let vm_size = var("CONDE_MACHINES_VM_SIZE");
        let values = REQUIRED.map(var);
        if vm_size.is_none() && values.iter().all(Option::is_none) {
            return Ok(None);
        }
        if let Some(i) = values.iter().position(Option::is_none) {
            return Err(ConfigError::Missing(REQUIRED[i]));
        }
        let [
            subscription,
            resource_group,
            template_spec,
            audience,
            client_id,
        ] = values.map(Option::unwrap_or_default);
        let platform = |name: &'static str, missing: &'static str| {
            var(name).ok_or(ConfigError::Missing(missing))
        };
        let identity = ManagedIdentity {
            endpoint: platform(
                "IDENTITY_ENDPOINT",
                "IDENTITY_ENDPOINT (the server's managed identity, which CONDE_MACHINES_* needs)",
            )?,
            header: platform(
                "IDENTITY_HEADER",
                "IDENTITY_HEADER (the server's managed identity, which CONDE_MACHINES_* needs)",
            )?,
            client_id,
        };
        Ok(Some(MachinesConfig {
            subscription,
            resource_group,
            template_spec,
            audience,
            vm_size: vm_size.unwrap_or_else(|| DEFAULT_VM_SIZE.into()),
            identity,
            arm_url: ARM_URL.into(),
            login_url: LOGIN_URL.into(),
            poll_every: POLL_EVERY,
        }))
    }
}

/// A failed Azure call; the message names the step and the status, never a token or a header.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct AzureError(String);

/// The server's Azure Resource Manager client.
pub struct Azure {
    http: reqwest::Client,
    identity: ManagedIdentity,
    /// The last token and its expiry (epoch seconds).
    token: tokio::sync::Mutex<Option<(String, u64)>>,
}

#[derive(Deserialize)]
struct IdentityToken {
    access_token: String,
    /// Epoch seconds; the platform sends them as a string.
    expires_on: serde_json::Value,
}

impl Azure {
    pub fn new(identity: ManagedIdentity) -> Azure {
        Azure {
            http: super::auth::http_client(HTTP_TIMEOUT),
            identity,
            token: tokio::sync::Mutex::new(None),
        }
    }

    /// A Resource Manager bearer token of the server's identity, reused until
    /// [`RENEW_BEFORE_SECS`] before it expires.
    pub async fn token(&self) -> Result<String, AzureError> {
        let mut cached = self.token.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some((token, expires_on)) = cached.as_ref()
            && now + RENEW_BEFORE_SECS < *expires_on
        {
            return Ok(token.clone());
        }
        let response = self
            .http
            .get(&self.identity.endpoint)
            .query(&[
                ("api-version", IDENTITY_API_VERSION),
                ("resource", ARM_RESOURCE),
                ("client_id", &self.identity.client_id),
            ])
            .header("X-IDENTITY-HEADER", &self.identity.header)
            .send()
            .await
            .map_err(|e| AzureError(format!("the managed identity endpoint failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(AzureError(format!(
                "the managed identity endpoint answered {status}"
            )));
        }
        let body: IdentityToken = response.json().await.map_err(|e| {
            AzureError(format!(
                "the managed identity endpoint's answer is not a token: {e}"
            ))
        })?;
        let expires_on = match &body.expires_on {
            serde_json::Value::String(s) => s.parse().ok(),
            serde_json::Value::Number(n) => n.as_u64(),
            _ => None,
        }
        .ok_or_else(|| AzureError("the managed identity token has no expires_on".into()))?;
        *cached = Some((body.access_token.clone(), expires_on));
        Ok(body.access_token)
    }
}

/// One deployment's parameters, as `machine.setup` answers them in `deploy`.
#[derive(Clone, Debug, Deserialize)]
pub struct Deployment {
    pub owner: String,
    pub host_id: String,
    pub owner_object_id: String,
    pub vm_size: String,
}

/// An Azure error body's code and message, else the status alone.
async fn refusal(what: &str, response: reqwest::Response) -> AzureError {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    refused_with(what, status, &body)
}

fn refused_with(what: &str, status: reqwest::StatusCode, body: &Value) -> AzureError {
    let error = &body["error"];
    let text = match (error["code"].as_str(), error["message"].as_str()) {
        (Some(code), Some(message)) => format!("{what} answered {status}: {code}: {message}"),
        _ => format!("{what} answered {status}"),
    };
    AzureError(text.chars().take(ERROR_CHARS).collect())
}

impl Azure {
    fn deployment_url(&self, config: &MachinesConfig, name: &str) -> String {
        format!(
            "{}/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Resources/deployments/{name}?api-version={DEPLOYMENTS_API_VERSION}",
            config.arm_url.trim_end_matches('/'),
            config.subscription,
            config.resource_group
        )
    }

    /// The named deployment of the template spec version for one person; Ok on 200 or 201.
    pub async fn put_deployment(
        &self,
        config: &MachinesConfig,
        server: &str,
        d: &Deployment,
    ) -> Result<(), AzureError> {
        let token = self.token().await?;
        let body = json!({"properties": {
            "mode": "Incremental",
            "templateLink": {"id": config.template_spec},
            "parameters": {
                "owner": {"value": d.owner},
                "ownerObjectId": {"value": d.owner_object_id},
                "condeResource": {"value": config.audience},
                "vmSize": {"value": d.vm_size},
                "condeServer": {"value": server},
            },
        }});
        let response = self
            .http
            .put(self.deployment_url(config, &d.host_id))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| AzureError(format!("the deployment PUT failed: {e}")))?;
        match response.status().as_u16() {
            200 | 201 => Ok(()),
            _ => Err(refusal("the deployment PUT", response).await),
        }
    }

    /// The named deployment's `provisioningState`, with `properties.error` when it is `Failed` or
    /// `Canceled`.
    pub async fn deployment_state(
        &self,
        config: &MachinesConfig,
        name: &str,
    ) -> Result<(String, Option<String>), AzureError> {
        let token = self.token().await?;
        let response = self
            .http
            .get(self.deployment_url(config, name))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AzureError(format!("the deployment read failed: {e}")))?;
        if !response.status().is_success() {
            return Err(refusal("the deployment read", response).await);
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| AzureError(format!("the deployment read is not JSON: {e}")))?;
        let props = &body["properties"];
        let state = props["provisioningState"]
            .as_str()
            .ok_or_else(|| AzureError("the deployment has no provisioningState".into()))?
            .to_string();
        let error = matches!(state.as_str(), "Failed" | "Canceled").then(|| {
            let e = &props["error"];
            let detail = e["details"][0]["message"].as_str();
            let text = match (e["message"].as_str(), detail) {
                (Some(m), Some(d)) if m != d => format!("{m} {d}"),
                (Some(m), _) => m.to_string(),
                (None, Some(d)) => d.to_string(),
                (None, None) => format!("the deployment ended {state}"),
            };
            text.chars().take(ERROR_CHARS).collect()
        });
        Ok((state, error))
    }

    /// The VM's identity `principalId`; None while the VM does not exist or has no identity yet.
    pub async fn vm_principal(
        &self,
        config: &MachinesConfig,
        name: &str,
    ) -> Result<Option<String>, AzureError> {
        let token = self.token().await?;
        let url = format!(
            "{}/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Compute/virtualMachines/{name}?api-version={VM_API_VERSION}",
            config.arm_url.trim_end_matches('/'),
            config.subscription,
            config.resource_group
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AzureError(format!("the VM read failed: {e}")))?;
        match response.status().as_u16() {
            404 => return Ok(None),
            200 => {}
            _ => return Err(refusal("the VM read", response).await),
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| AzureError(format!("the VM read is not JSON: {e}")))?;
        Ok(body["identity"]["principalId"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(str::to_string))
    }
}

/// A host id that is one plain path segment: letters, digits, `-`, `_` and `.`, starting with a
/// letter or digit. Anything else is never put in a removal's URL.
fn plain_name(host: &str) -> bool {
    host.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether a role assignment listed at the VM is the template's login assignment of the owner:
/// on the VM itself (not inherited), of the login role, to the owner's object id.
fn is_owner_login(a: &Value, vm_id: &str, owner_object_id: &str) -> bool {
    let p = &a["properties"];
    let text = |v: &Value| v.as_str().unwrap_or_default().to_ascii_lowercase();
    text(&p["scope"]) == vm_id.to_ascii_lowercase()
        && text(&p["principalId"]) == owner_object_id.to_ascii_lowercase()
        && text(&p["roleDefinitionId"]).ends_with(&format!("/roledefinitions/{LOGIN_ROLE}"))
        && a["name"]
            .as_str()
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}

impl Azure {
    /// One Resource Manager call with the identity's token: the status and the JSON body (null
    /// when there is none).
    async fn call(
        &self,
        method: reqwest::Method,
        url: &str,
        what: &str,
    ) -> Result<(reqwest::StatusCode, Value), AzureError> {
        let token = self.token().await?;
        let mut request = self.http.request(method.clone(), url).bearer_auth(token);
        if method == reqwest::Method::POST {
            request = request.header(header::CONTENT_LENGTH, "0");
        }
        let response = request
            .send()
            .await
            .map_err(|e| AzureError(format!("{what} failed: {e}")))?;
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// `/subscriptions/<sub>/resourceGroups/<rg>/providers/Microsoft.Compute/virtualMachines/<host>`.
    fn vm_id(config: &MachinesConfig, host: &str) -> String {
        format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Compute/virtualMachines/{host}",
            config.subscription, config.resource_group
        )
    }

    /// Step 1: the named deployment ended. While it is `Accepted` or `Running` it is cancelled
    /// once, then read every `poll_every` until it is `Succeeded`, `Failed` or `Canceled`; no
    /// deployment of that name is ended too.
    async fn end_deployment(&self, config: &MachinesConfig, host: &str) -> Result<(), AzureError> {
        let url = self.deployment_url(config, host);
        let cancel = format!(
            "{}/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Resources/deployments/{host}/cancel?api-version={DEPLOYMENTS_API_VERSION}",
            config.arm_url.trim_end_matches('/'),
            config.subscription,
            config.resource_group
        );
        let mut cancelled = false;
        for read in 0..=POLLS {
            if read > 0 {
                tokio::time::sleep(config.poll_every).await;
            }
            let what = "the deployment read";
            let (status, body) = self.call(reqwest::Method::GET, &url, what).await?;
            match status.as_u16() {
                404 => return Ok(()),
                200 => {}
                _ => return Err(refused_with(what, status, &body)),
            }
            let state = body["properties"]["provisioningState"]
                .as_str()
                .ok_or_else(|| AzureError("the deployment has no provisioningState".into()))?;
            match state {
                "Succeeded" | "Failed" | "Canceled" => return Ok(()),
                "Accepted" | "Running" if !cancelled => {
                    let what = "the deployment cancel";
                    let (status, body) = self.call(reqwest::Method::POST, &cancel, what).await?;
                    // 409: it ended meanwhile, which the next read shows
                    if !matches!(status.as_u16(), 200 | 202 | 204 | 409) {
                        return Err(refused_with(what, status, &body));
                    }
                    cancelled = true;
                }
                _ => {}
            }
        }
        Err(AzureError(format!(
            "the deployment {host} did not end within {} s",
            config.poll_every.as_secs() * u64::from(POLLS)
        )))
    }

    /// Step 2: the template's login assignment of the owner on the VM, deleted. Only an entry
    /// [`is_owner_login`] accepts is deleted, by its name at the VM's scope; a VM that is gone
    /// has none.
    async fn delete_login(
        &self,
        config: &MachinesConfig,
        host: &str,
        owner_object_id: &str,
    ) -> Result<(), AzureError> {
        let base = config.arm_url.trim_end_matches('/');
        let vm_id = Azure::vm_id(config, host);
        let list = format!(
            "{base}{vm_id}/providers/Microsoft.Authorization/roleAssignments?api-version={ROLES_API_VERSION}&$filter=atScope()"
        );
        let what = "the VM's role assignment list";
        let (status, body) = self.call(reqwest::Method::GET, &list, what).await?;
        match status.as_u16() {
            404 => return Ok(()),
            200 => {}
            _ => return Err(refused_with(what, status, &body)),
        }
        let entries = body["value"].as_array().cloned().unwrap_or_default();
        for a in entries
            .iter()
            .filter(|a| is_owner_login(a, &vm_id, owner_object_id))
        {
            let name = a["name"].as_str().unwrap_or_default();
            let url = format!(
                "{base}{vm_id}/providers/Microsoft.Authorization/roleAssignments/{name}?api-version={ROLES_API_VERSION}"
            );
            let what = "the login assignment's DELETE";
            let (status, body) = self.call(reqwest::Method::DELETE, &url, what).await?;
            if !matches!(status.as_u16(), 200 | 204 | 404) {
                return Err(refused_with(what, status, &body));
            }
        }
        Ok(())
    }

    /// Steps 3 and 4: the VM deleted (its NIC and OS disk go with it, the template's
    /// `deleteOption`), then read every `poll_every` until it answers 404. A VM already gone is
    /// deleted; one already `Deleting` is only waited for.
    async fn delete_vm(&self, config: &MachinesConfig, host: &str) -> Result<(), AzureError> {
        let url = format!(
            "{}{}?api-version={VM_API_VERSION}",
            config.arm_url.trim_end_matches('/'),
            Azure::vm_id(config, host)
        );
        let what = "the VM read";
        let (status, body) = self.call(reqwest::Method::GET, &url, what).await?;
        match status.as_u16() {
            404 => return Ok(()),
            200 => {}
            _ => return Err(refused_with(what, status, &body)),
        }
        if body["properties"]["provisioningState"] != "Deleting" {
            let what = "the VM DELETE";
            let (status, body) = self.call(reqwest::Method::DELETE, &url, what).await?;
            match status.as_u16() {
                404 => return Ok(()),
                200 | 202 | 204 => {}
                _ => return Err(refused_with(what, status, &body)),
            }
        }
        for _ in 0..POLLS {
            tokio::time::sleep(config.poll_every).await;
            let (status, body) = self.call(reqwest::Method::GET, &url, what).await?;
            match status.as_u16() {
                404 => return Ok(()),
                200 => {}
                _ => return Err(refused_with(what, status, &body)),
            }
        }
        Err(AzureError(format!(
            "the VM {host} was not gone within {} s",
            config.poll_every.as_secs() * u64::from(POLLS)
        )))
    }
}

/// An admin disabled the machine's owner: the four steps, each skipped when Azure shows it done,
/// then the record is deleted. The first failure is the record's `delete_error` and ends the
/// task; the next disable of the owner runs it again.
pub(crate) async fn remove(core: super::Core, machines: Arc<Machines>, m: super::MachineCleanup) {
    let (config, azure) = (&machines.config, &machines.azure);
    let host = m.host_id.clone();
    let outcome = async {
        if !plain_name(&host) {
            return Err(AzureError(format!("the host id {host:?} is not a VM name")));
        }
        azure.end_deployment(config, &host).await?;
        azure
            .delete_login(config, &host, &m.owner_object_id)
            .await?;
        azure.delete_vm(config, &host).await
    }
    .await;
    let (user_id, h) = (m.user_id, host.clone());
    let stored = match outcome {
        Ok(()) => {
            tracing::info!(%host, "machine removed");
            core.store(move |s| s.machine_deleted(user_id, &h)).await
        }
        Err(e) => {
            tracing::warn!(%host, error = %e, "machine removal stopped");
            let error = e.to_string();
            core.store(move |s| s.machine_delete_failed(user_id, &h, &error))
                .await
        }
    };
    if let Err(e) = stored {
        tracing::error!(%host, error = %e, "the machine removal's outcome was not stored");
    }
}

/// Why a VM's token was not accepted.
#[derive(Debug, Error)]
pub(crate) enum TokenError {
    /// The token fails a check: 401, final for the VM.
    #[error("{0}")]
    Invalid(String),
    /// The tenant's signing keys could not be read: 503, the VM retries.
    #[error("the tenant's signing keys could not be read: {0}")]
    Keys(String),
}

fn invalid(why: impl Into<String>) -> TokenError {
    TokenError::Invalid(why.into())
}

/// The check of an agent VM's Entra token: a v1 access token of the tenant for the
/// conde-machines audience, signed with one of the tenant's v1 signing keys (RS256).
pub(crate) struct VmTokens {
    http: reqwest::Client,
    keys_url: String,
    issuer: String,
    tenant: String,
    audience: String,
    keys: tokio::sync::Mutex<Option<(CoreJsonWebKeySet, std::time::Instant)>>,
}

impl VmTokens {
    pub fn new(login_url: &str, tenant: &str, audience: &str) -> VmTokens {
        VmTokens {
            http: super::auth::http_client(HTTP_TIMEOUT),
            keys_url: format!(
                "{}/{tenant}/discovery/keys",
                login_url.trim_end_matches('/')
            ),
            issuer: format!("https://sts.windows.net/{tenant}/"),
            tenant: tenant.to_string(),
            audience: audience.to_string(),
            keys: tokio::sync::Mutex::new(None),
        }
    }

    /// The tenant's key set, read when none is cached, the cached one is older than
    /// [`KEYS_MAX_AGE`], or `refresh`.
    async fn key_set(&self, refresh: bool) -> Result<CoreJsonWebKeySet, TokenError> {
        let mut cached = self.keys.lock().await;
        if let Some((set, at)) = cached.as_ref()
            && !refresh
            && at.elapsed() < KEYS_MAX_AGE
        {
            return Ok(set.clone());
        }
        let response = self
            .http
            .get(&self.keys_url)
            .send()
            .await
            .map_err(|e| TokenError::Keys(e.to_string()))?;
        if !response.status().is_success() {
            return Err(TokenError::Keys(format!("answered {}", response.status())));
        }
        let set: CoreJsonWebKeySet = response
            .json()
            .await
            .map_err(|e| TokenError::Keys(format!("not a key set: {e}")))?;
        *cached = Some((set.clone(), std::time::Instant::now()));
        Ok(set)
    }

    /// The token's `oid` once its signature, `iss`, `tid`, `aud`, `nbf` and `exp` hold at `now`
    /// (unix seconds).
    pub async fn check(&self, token: &str, now: i64) -> Result<String, TokenError> {
        let mut parts = token.split('.');
        let (Some(head), Some(body), Some(sig), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid("not a compact JWS"));
        };
        let decode = |part: &str| {
            URL_SAFE_NO_PAD
                .decode(part)
                .map_err(|_| invalid("not base64url"))
        };
        let header: Value = serde_json::from_slice(&decode(head)?)
            .map_err(|_| invalid("the header is not JSON"))?;
        if header["alg"] != "RS256" {
            return Err(invalid("the algorithm is not RS256"));
        }
        let kid = header["kid"]
            .as_str()
            .ok_or_else(|| invalid("the header has no kid"))?;
        let signature = decode(sig)?;
        let find = |set: &CoreJsonWebKeySet| {
            set.keys()
                .iter()
                .find(|k| k.key_id().is_some_and(|id| id.as_str() == kid))
                .cloned()
        };
        let key = match find(&self.key_set(false).await?) {
            Some(key) => key,
            // the tenant rotated its keys: read them once more
            None => find(&self.key_set(true).await?)
                .ok_or_else(|| invalid("no signing key of the tenant has this kid"))?,
        };
        key.verify_signature(
            &CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            format!("{head}.{body}").as_bytes(),
            &signature,
        )
        .map_err(|e| invalid(format!("the signature does not verify: {e}")))?;
        let claims: Value = serde_json::from_slice(&decode(body)?)
            .map_err(|_| invalid("the claims are not JSON"))?;
        if claims["iss"] != self.issuer.as_str() {
            return Err(invalid("the issuer is not the tenant's v1 issuer"));
        }
        if claims["tid"] != self.tenant.as_str() {
            return Err(invalid("the tenant is not the configured tenant"));
        }
        let audience = match &claims["aud"] {
            Value::String(aud) => aud == &self.audience,
            Value::Array(auds) => auds.iter().any(|a| a == self.audience.as_str()),
            _ => false,
        };
        if !audience {
            return Err(invalid("the audience is not conde-machines"));
        }
        let exp = claims["exp"]
            .as_i64()
            .ok_or_else(|| invalid("the token has no exp"))?;
        if now >= exp {
            return Err(invalid("the token expired"));
        }
        if claims["nbf"].as_i64().is_some_and(|nbf| now < nbf) {
            return Err(invalid("the token is not valid yet"));
        }
        claims["oid"]
            .as_str()
            .filter(|oid| !oid.is_empty())
            .map(str::to_string)
            .ok_or_else(|| invalid("the token has no oid"))
    }
}

/// The agent machines as the web routes use them: the configuration, its Azure client and the
/// VM token check.
pub(crate) struct Machines {
    pub config: MachinesConfig,
    pub azure: Azure,
    pub tokens: VmTokens,
}

impl Machines {
    pub fn new(config: MachinesConfig, tenant: &str) -> Machines {
        Machines {
            azure: Azure::new(config.identity.clone()),
            tokens: VmTokens::new(&config.login_url, tenant, &config.audience),
            config,
        }
    }
}

/// An enrol refusal: `{"error", "message"}`, never cached.
fn answer(status: StatusCode, body: Value) -> Response {
    let mut r = (status, axum::Json(body)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn refused(status: StatusCode, error: &str, message: &str) -> Response {
    answer(status, json!({"error": error, "message": message}))
}

/// Before an enrol looks its identity up again: every record still without a VM identity has its
/// VM read, and the `principalId` of each VM that exists is stored. A failed read is logged and
/// leaves its record as it was.
async fn fill_principals(state: &WebState, machines: &Machines) {
    let waiting = match state.store(|s| s.machines_awaiting_principal()).await {
        Ok(waiting) => waiting,
        Err(e) => {
            tracing::error!(error = %e, "the machines awaiting their VM were not read");
            return;
        }
    };
    let reads = waiting.into_iter().map(|(user_id, host)| async move {
        match machines.azure.vm_principal(&machines.config, &host).await {
            Ok(Some(principal)) => {
                if let Err(e) = state
                    .store(move |s| s.machine_set_principal(user_id, &principal))
                    .await
                {
                    tracing::error!(%host, error = %e, "the VM identity was not stored");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(%host, error = %e, "the VM was not read"),
        }
    });
    futures_util::future::join_all(reads).await;
}

/// `POST /v1/machines/enrol` with the VM identity's token as the bearer: the host key, once per
/// identity (the design's contract). 401 `invalid_token`, 404 `unknown_machine` (no record has
/// the identity, after the VM reads), 403 `disabled`, 409 `already_minted` (nothing changes),
/// 429 `rate_limited` with `Retry-After`; 503 `unavailable` while the tenant's keys cannot be read.
/// A replaced key's bridge is closed by the revocation's own announcement, on every replica.
pub(crate) async fn enrol(
    State(state): State<WebState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
) -> Response {
    if let Err(wait) = state.0.limits.machine_enrol(ip) {
        tracing::info!(client_ip = %ip, wait, endpoint = "machine enrol", "rate limited");
        let mut r = answer(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "rate_limited", "message": format!("too many enrol attempts from this address; retry in {wait} s"),
                   "retry_after": wait}),
        );
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            r.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return r;
    }
    let Some(machines) = state.machines().cloned() else {
        return refused(
            StatusCode::NOT_FOUND,
            "unknown_machine",
            "this board has no agent machines",
        );
    };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let checked = match bearer {
        Some(token) => machines.tokens.check(token.trim(), state.now()).await,
        None => Err(invalid("no bearer token")),
    };
    let oid = match checked {
        Ok(oid) => oid,
        Err(TokenError::Invalid(why)) => {
            tracing::info!(client_ip = %ip, %why, "machine enrol token refused");
            return refused(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "the token is not a valid machine token of this board's tenant",
            );
        }
        Err(e @ TokenError::Keys(_)) => {
            tracing::warn!(error = %e, "machine enrol not checked");
            return refused(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the tenant's signing keys could not be read; retry",
            );
        }
    };
    let key = super::keys::new_host_key();
    let token_digest = digest(&key);
    let try_mint = || {
        let (oid, token_digest) = (oid.clone(), token_digest.clone());
        state.store(move |s| s.machine_enrol(&oid, &token_digest))
    };
    let mut outcome = try_mint().await;
    if matches!(outcome, Ok(Enrolled::Unknown)) {
        fill_principals(&state, &machines).await;
        outcome = try_mint().await;
    }
    let (user_id, user, host_id, replaced, new_host) = match outcome {
        Ok(Enrolled::Minted {
            user_id,
            user,
            host_id,
            replaced,
            new_host,
        }) => (user_id, user, host_id, replaced, new_host),
        Ok(Enrolled::Unknown) => {
            return refused(
                StatusCode::NOT_FOUND,
                "unknown_machine",
                "no machine of this board has this identity yet",
            );
        }
        Ok(Enrolled::Disabled) => {
            return refused(
                StatusCode::FORBIDDEN,
                "disabled",
                "the machine's owner is disabled on this board, or the machine is being removed",
            );
        }
        Ok(Enrolled::AlreadyMinted) => {
            return refused(
                StatusCode::CONFLICT,
                "already_minted",
                "this machine's key was minted before",
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "machine enrol store failed");
            return refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the mint failed",
            );
        }
    };
    tracing::info!(user = %user, host = %host_id, replaced, new_host, "machine key minted");
    if new_host {
        let (host, now) = (host_id.clone(), state.now());
        if let Err(e) = state
            .store(move |s| s.enrolled(user_id, &host, Enrollment::Machine, now))
            .await
        {
            tracing::error!(host = %host_id, error = %e, "the welcome failed");
        }
    }
    let board = state
        .store(|s| Ok(s.board_name()))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| super::invite::workspace_of(&state.config().public_url));
    answer(
        StatusCode::OK,
        json!({
            "v": 1,
            "server": state.config().public_url,
            "board": board,
            "user": user,
            "host_id": host_id,
            "label": host_id,
            "key_id": &token_digest[..12],
            "key": key,
            "name": format!("{user}/vm.agent"),
        }),
    )
}

/// Runs one browser request as `user` on a blocking thread.
async fn dispatch_web(state: &WebState, user: &WebUser, request: Value) -> Option<Value> {
    let dispatcher = state.dispatcher()?.clone();
    let principal = crate::WebPrincipal {
        user_id: user.user_id,
        user: user.name.clone(),
    };
    tokio::task::spawn_blocking(move || dispatcher.dispatch_web(&principal, request))
        .await
        .ok()
}

/// A person's first sign-in: `machine.setup` as they would click it, then its deployment.
pub(crate) async fn request(state: &WebState, user: &WebUser) {
    let id = format!("signin-{:032x}", rand::random::<u128>());
    let request = json!({"v": 1, "type": "request", "request_id": id, "idempotency_key": id,
                         "op": "machine.setup", "body": {}});
    match dispatch_web(state, user, request).await {
        Some(response) if response["ok"] == true => after_setup(state, user.user_id, &response),
        Some(response) => {
            tracing::warn!(user = %user.name, error = %response["error"], "no machine requested at the first sign-in")
        }
        None => {
            tracing::warn!(user = %user.name, "no machine requested at the first sign-in: no board")
        }
    }
}

/// After a `machine.setup` answer: the deployment PUT it asks for, in a spawned task.
pub(crate) fn after_setup(state: &WebState, user_id: i64, response: &Value) {
    let Some(machines) = state.machines().cloned() else {
        return;
    };
    let Some(d) = response["result"]
        .get("deploy")
        .filter(|d| !d.is_null())
        .and_then(|d| serde_json::from_value::<Deployment>(d.clone()).ok())
    else {
        return;
    };
    let (state, server) = (state.clone(), state.config().public_url.clone());
    tokio::spawn(async move {
        let error = match machines
            .azure
            .put_deployment(&machines.config, &server, &d)
            .await
        {
            Ok(()) => {
                tracing::info!(host = %d.host_id, "machine deployment accepted");
                None
            }
            Err(e) => {
                tracing::warn!(host = %d.host_id, error = %e, "machine deployment refused");
                Some(e.to_string())
            }
        };
        if let Err(e) = state
            .store(move |s| s.machine_deployed(user_id, error.as_deref()))
            .await
        {
            tracing::error!(error = %e, "the machine deployment's outcome was not stored");
        }
    });
}

/// Before `machine.status` and `machine.setup`: the pending record's deployment, read when no
/// read happened in the last [`DEPLOY_CHECK_EVERY`] seconds.
pub(crate) async fn refresh(state: &WebState, user_id: i64) {
    let Some(machines) = state.machines().cloned() else {
        return;
    };
    let host = match state
        .store(move |s| s.machine_check_due(user_id, DEPLOY_CHECK_EVERY))
        .await
    {
        Ok(Some(host)) => host,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "machine deployment check not claimed");
            return;
        }
    };
    match machines
        .azure
        .deployment_state(&machines.config, &host)
        .await
    {
        Ok((deploy_state, error)) => {
            if let Err(e) = state
                .store(move |s| s.machine_checked(user_id, &deploy_state, error.as_deref()))
                .await
            {
                tracing::error!(error = %e, "the machine deployment's state was not stored");
            }
        }
        Err(e) => tracing::warn!(%host, error = %e, "machine deployment not read"),
    }
}

#[cfg(test)]
mod tests {
    /// A removal's URL carries the host id as one plain path segment or not at all.
    #[test]
    fn a_removal_names_only_a_plain_vm() {
        for ok in [
            "vm-agent-krisztian-dobmayer",
            "vm-agent-levente",
            "pilot.1_a",
        ] {
            assert!(super::plain_name(ok), "{ok}");
        }
        for bad in [
            "", "..", ".x", "-x", "a/b", "a/../b", "a?b", "a%2Fb", "a b", "a#b",
        ] {
            assert!(!super::plain_name(bad), "{bad}");
        }
    }

    /// The hand deployment and the template name a person's deployment as the server does
    /// (`vm-agent-<owner>`, the VM's own name), so both write one deployment record.
    #[test]
    fn a_hand_deployment_and_the_server_share_one_deployment_name() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../infra/agent-vm/");
        let read = |f: &str| std::fs::read_to_string(format!("{root}{f}")).unwrap();
        let (readme, template) = (read("README.md"), read("template/agent-vm.bicep"));
        assert!(
            readme.contains("az deployment group create -g conde-vm -n vm-agent-<owner>"),
            "the README's hand deployment"
        );
        for (file, text) in [("README.md", &readme), ("agent-vm.bicep", &template)] {
            assert!(
                !text.contains("agent-vm-<owner>"),
                "{file} names another deployment"
            );
        }
        assert!(template.contains("one named deployment per person (`vm-agent-<owner>`)"));
    }
}
