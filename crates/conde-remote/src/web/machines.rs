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
//!   read changes nothing (the next read retries).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use super::{ConfigError, WebState, WebUser};

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
}

/// The agent machines as the web routes use them: the configuration and its Azure client.
pub(crate) struct Machines {
    pub config: MachinesConfig,
    pub azure: Azure,
}

impl Machines {
    pub fn new(config: MachinesConfig) -> Machines {
        Machines {
            azure: Azure::new(config.identity.clone()),
            config,
        }
    }
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
