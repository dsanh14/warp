//! Client for the pre-warmed sandbox endpoints a standby sandbox uses to fetch its
//! configuration, report preparation state, and prove it is still alive.
//!
//! Every endpoint requires two credentials: the agent-scoped API key (normal auth) and a
//! Namespace workload token naming this instance. The server rejects a request carrying only
//! one with a 401, so the token is attached to each request alongside the bearer token.
// Pre-warmed sandboxes only run in a Namespace container, never in a WASM build.
#![cfg_attr(target_family = "wasm", expect(dead_code))]

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cloud_object_models::SourceRepo;
use serde::{Deserialize, Serialize};
use warpui::r#async::FutureExt as _;

use super::ServerApi;
use super::ai::GitCredential;
use super::repository::ResolvedRepository;
use crate::ChannelState;

pub const PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER: &str = "X-Warp-Prewarmed-Sandbox-Workload-Token";
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The agent-scoped subset of an execution configuration that a standby can act on without a
/// task. Mirrors `types.PrewarmedSandboxPreparation` in warp-server.
#[derive(Debug, Clone, Deserialize)]
pub struct PrewarmedSandboxPreparation {
    pub harness: String,
    #[serde(default)]
    pub repositories: Vec<ResolvedRepository>,
    /// Repositories the agent clones on demand. Preparation retains the inventory without
    /// materializing them.
    #[serde(default)]
    pub deferred_repositories: Vec<SourceRepo>,
    #[serde(default)]
    pub setup_commands: Vec<String>,
    /// Raw `mcp_servers` map, in the same shape an agent config file uses, so it can go
    /// through the shared MCP spec validation rather than a second parser.
    #[serde(default)]
    pub mcp_servers: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub factory_skill_dirs: Vec<String>,
    #[serde(default)]
    pub computer_use_enabled: bool,
}

/// Everything a standby sandbox needs to prepare itself.
///
/// Carries ephemeral credentials in `git_credentials` and `managed_secrets`; neither may be
/// logged or persisted.
#[derive(Debug, Clone, Deserialize)]
pub struct PrewarmedSandboxConfiguration {
    pub generation: String,
    pub preparation: PrewarmedSandboxPreparation,
    #[serde(default)]
    pub environment_variables: HashMap<String, String>,
    #[serde(default)]
    pub git_credentials: Vec<GitCredential>,
    #[serde(default)]
    pub managed_secrets: HashMap<String, String>,
    pub expires_at: DateTime<Utc>,
}

/// The preparation outcome a standby reports. The server accepts no other states.
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

#[derive(Debug, Serialize)]
struct PrewarmedSandboxStatusInput {
    generation: String,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<String>,
}

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

    /// Reports the terminal preparation outcome. `manifest_hash` must equal the generation
    /// for a ready report; the server rejects any other value.
    async fn report_status(
        &self,
        sandbox_id: &str,
        generation: &str,
        state: PrewarmedSandboxState,
    ) -> Result<()>;

    /// Keeps the sandbox out of the reconciler's unhealthy set.
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
                // The server accepts a ready report only when its hash matches the generation.
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
