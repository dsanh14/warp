//! Client for the pre-warmed sandbox endpoints a standby sandbox uses to fetch its
//! configuration, report preparation state, and prove it is still alive.
//!
//! Every endpoint requires two credentials: the agent-scoped API key (normal auth) and a
//! Namespace workload token naming this instance. The server rejects a request carrying only
//! one with a 401, so the token is attached to each request alongside the bearer token.
// Pre-warmed sandboxes only run in a Namespace container, never in a WASM build.
#![cfg_attr(target_family = "wasm", expect(dead_code))]

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cloud_object_models::SourceRepo;
use http::HeaderValue;
use serde::{Deserialize, Serialize};
use warpui::r#async::FutureExt as _;

use super::ServerApi;
use super::ai::GitCredential;
use super::repository::ResolvedRepository;
use crate::ChannelState;

/// Carries the Namespace instance identity in addition to the normal bearer credential.
pub const PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER: &str = "X-Warp-Prewarmed-Sandbox-Workload-Token";
/// Bounds each complete request, including credential lookup and response decoding.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Matches the server's `PrewarmedSandboxPreparation` contract for setup that does not need a task.
#[derive(Clone, Deserialize)]
pub struct PrewarmedSandboxPreparation {
    pub harness: String,
    /// Defines the repository checkouts that preparation must create.
    #[serde(default)]
    pub repositories: Vec<ResolvedRepository>,
    /// Keeps repositories for later on-demand clones without cloning them during setup.
    #[serde(default)]
    pub deferred_repositories: Vec<SourceRepo>,
    #[serde(default)]
    pub setup_commands: Vec<String>,
    /// Uses the agent config file format so normal runs and standby share MCP validation.
    #[serde(default)]
    pub mcp_servers: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub factory_skill_dirs: Vec<String>,
    #[serde(default)]
    pub computer_use_enabled: bool,
}

// Debug output omits values because setup commands and MCP settings can contain secrets.
impl fmt::Debug for PrewarmedSandboxPreparation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrewarmedSandboxPreparation")
            .field("harness", &self.harness)
            .field("repository_count", &self.repositories.len())
            .field(
                "deferred_repository_count",
                &self.deferred_repositories.len(),
            )
            .field("setup_command_count", &self.setup_commands.len())
            .field("mcp_server_count", &self.mcp_servers.len())
            .field("skill_count", &self.skills.len())
            .field("factory_skill_dir_count", &self.factory_skill_dirs.len())
            .field("computer_use_enabled", &self.computer_use_enabled)
            .finish()
    }
}

/// Supplies a standby configuration whose secrets must stay out of logs and saved app state.
#[derive(Clone, Deserialize)]
pub struct PrewarmedSandboxConfiguration {
    /// Identifies the configuration version that status reports and heartbeats must match.
    pub generation: String,
    pub preparation: PrewarmedSandboxPreparation,
    /// Supplies terminal variables that can include authenticated Factory clone URLs.
    #[serde(default)]
    pub environment_variables: HashMap<String, String>,
    /// Supplies temporary credentials for the sandbox's Git credential files.
    #[serde(default)]
    pub git_credentials: Vec<GitCredential>,
    /// Supplies secret values for the terminal environment and MCP configuration.
    #[serde(default)]
    pub managed_secrets: HashMap<String, String>,
    /// Sets the server deadline that the standby process must exit before.
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for PrewarmedSandboxConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrewarmedSandboxConfiguration")
            .field("generation", &self.generation)
            .field("preparation", &self.preparation)
            .field(
                "environment_variable_count",
                &self.environment_variables.len(),
            )
            .field("git_credential_count", &self.git_credentials.len())
            .field("managed_secret_count", &self.managed_secrets.len())
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Restricts standby status reports to the two preparation outcomes the server accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrewarmedSandboxState {
    Ready,
    Failed,
}

impl PrewarmedSandboxState {
    fn as_wire_value(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// Reports a preparation outcome for a specific configuration generation.
#[derive(Debug, Serialize)]
struct PrewarmedSandboxStatusInput {
    generation: String,
    state: &'static str,
    /// Must match the generation for a ready report.
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<String>,
}

/// Confirms that a sandbox is still alive with the expected configuration generation.
#[derive(Debug, Serialize)]
struct PrewarmedSandboxHeartbeatInput {
    generation: String,
}

/// Endpoints a pre-warmed sandbox calls while it prepares itself and waits to be claimed.
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait PrewarmedSandboxClient: 'static + Send + Sync {
    /// Fetches this sandbox's preparation plan and credentials.
    async fn fetch_configuration(&self, sandbox_id: &str) -> Result<PrewarmedSandboxConfiguration>;

    /// Reports whether preparation succeeded for the supplied configuration generation.
    async fn report_status(
        &self,
        sandbox_id: &str,
        generation: &str,
        state: PrewarmedSandboxState,
    ) -> Result<()>;

    /// Keeps the sandbox healthy during preparation and the idle wait.
    async fn heartbeat(&self, sandbox_id: &str, generation: &str) -> Result<()>;
}

impl ServerApi {
    fn prewarmed_sandbox_url(sandbox_id: &str, action: &str) -> String {
        format!(
            "{}/internal/prewarmed-sandboxes/{}/{action}",
            ChannelState::server_root_url(),
            urlencoding::encode(sandbox_id),
        )
    }

    /// Attaches both credentials the prewarmed endpoints require.
    async fn prewarmed_sandbox_request<'a>(
        &self,
        builder: http_client::RequestBuilder<'a>,
    ) -> Result<http_client::RequestBuilder<'a>> {
        let workload_token = self
            .base_client
            .get_or_create_ambient_workload_token()
            .await?
            .context("A pre-warmed sandbox request requires a workload token")?;
        let mut workload_token = HeaderValue::from_str(&workload_token)
            .context("Invalid pre-warmed sandbox workload token header")?;
        workload_token.set_sensitive(true);
        let auth_token = self
            .get_or_refresh_access_token()
            .await
            .context("Failed to get access token for a pre-warmed sandbox request")?;
        let token = auth_token
            .as_bearer_token()
            .context("A pre-warmed sandbox request requires an access token")?;
        Ok(builder
            .header(PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER, workload_token)
            .bearer_auth(token))
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl PrewarmedSandboxClient for ServerApi {
    async fn fetch_configuration(&self, sandbox_id: &str) -> Result<PrewarmedSandboxConfiguration> {
        bounded_request(REQUEST_TIMEOUT, async {
            let url = Self::prewarmed_sandbox_url(sandbox_id, "config");
            let response = self
                .prewarmed_sandbox_request(self.base_client.http_client().get(&url))
                .await?
                .send()
                .await
                .context("Failed to request the pre-warmed sandbox configuration")?;

            if !response.status().is_success() {
                return Err(Self::error_from_response(response).await);
            }
            response
                .json::<PrewarmedSandboxConfiguration>()
                .await
                .context("Failed to deserialize the pre-warmed sandbox configuration")
        })
        .await
    }

    async fn report_status(
        &self,
        sandbox_id: &str,
        generation: &str,
        state: PrewarmedSandboxState,
    ) -> Result<()> {
        bounded_request(REQUEST_TIMEOUT, async {
            let url = Self::prewarmed_sandbox_url(sandbox_id, "status");
            let body = PrewarmedSandboxStatusInput {
                generation: generation.to_string(),
                state: state.as_wire_value(),
                manifest_hash: matches!(state, PrewarmedSandboxState::Ready)
                    .then(|| generation.to_string()),
            };
            let response = self
                .prewarmed_sandbox_request(self.base_client.http_client().post(&url).json(&body))
                .await?
                .send()
                .await
                .context("Failed to report the pre-warmed sandbox status")?;
            if !response.status().is_success() {
                return Err(Self::error_from_response(response).await);
            }
            Ok(())
        })
        .await
    }

    async fn heartbeat(&self, sandbox_id: &str, generation: &str) -> Result<()> {
        bounded_request(REQUEST_TIMEOUT, async {
            let url = Self::prewarmed_sandbox_url(sandbox_id, "heartbeat");
            let body = PrewarmedSandboxHeartbeatInput {
                generation: generation.to_string(),
            };
            let response = self
                .prewarmed_sandbox_request(self.base_client.http_client().post(&url).json(&body))
                .await?
                .send()
                .await
                .context("Failed to send a pre-warmed sandbox heartbeat")?;
            if !response.status().is_success() {
                return Err(Self::error_from_response(response).await);
            }
            Ok(())
        })
        .await
    }
}

/// Stops the entire request at its deadline so a stalled phase cannot block later heartbeats.
async fn bounded_request<T>(
    timeout: Duration,
    request: impl Future<Output = Result<T>>,
) -> Result<T> {
    request
        .with_timeout(timeout)
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))
        .context("Pre-warmed sandbox request timed out")?
}

#[cfg(test)]
#[path = "prewarmed_sandbox_tests.rs"]
pub(crate) mod tests;
