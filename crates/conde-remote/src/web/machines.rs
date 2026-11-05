//! The server's side of the agent machines: where a person's VM is deployed (`CONDE_MACHINES_*`)
//! and the Azure Resource Manager token of the server's own managed identity (id-conde).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use thiserror::Error;

use super::ConfigError;

/// The VM size a deployment asks for when `CONDE_MACHINES_VM_SIZE` is unset.
pub const DEFAULT_VM_SIZE: &str = "Standard_B2as_v2";
const ARM_RESOURCE: &str = "https://management.azure.com/";
const IDENTITY_API_VERSION: &str = "2019-08-01";
/// A cached token is asked for again this long before it expires.
const RENEW_BEFORE_SECS: u64 = 300;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

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
