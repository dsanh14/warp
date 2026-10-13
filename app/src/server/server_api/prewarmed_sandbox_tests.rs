#[cfg(not(feature = "skip_login"))]
use std::sync::Arc;

use cloud_object_models::CodeForge;
#[cfg(not(feature = "skip_login"))]
use futures::executor::block_on;
#[cfg(not(feature = "skip_login"))]
use mockito::Matcher;
use warp_cli::agent::RepositoryHeadRef;

use super::*;

/// A `/config` body in exactly the shape `types.PrewarmedSandboxConfiguration` marshals to.
pub(crate) const CONFIGURATION_JSON: &str = r#"{
  "sandbox_id": "11111111-2222-3333-4444-555555555555",
  "generation": "abc123",
  "preparation": {
    "harness": "oz",
    "repositories": [
      {
        "forge": "GITHUB",
        "owner": "warpdotdev",
        "name": "warp",
        "ref": { "type": "BRANCH", "value": "main" },
        "clone_from": { "code_forge": "GITHUB", "owner": "warpdotdev", "repo": "warp-fork" },
        "preserve_origin": true
      }
    ],
    "deferred_repositories": [
      { "code_forge": "GITHUB", "owner": "warpdotdev", "repo": "warp-server" }
    ],
    "setup_commands": ["./script/bootstrap"],
    "mcp_servers": { "linear": { "warp_id": "7f1d2c3b-4a59-4e6f-8b70-1c2d3e4f5a6b" } },
    "skills": ["warpdotdev/warp:review"],
    "factory_skill_dirs": [".agents/skills"],
    "computer_use_enabled": true
  },
  "environment_variables": { "WARP_FACTORY_REPO_DIR": "factory" },
  "git_credentials": [
    { "host": "github.com", "username": "x-access-token", "token": "ghs_secret" }
  ],
  "managed_secrets": { "API_TOKEN": "value" },
  "expires_at": "2026-01-01T00:00:00Z"
}"#;

pub(crate) fn parse_configuration() -> PrewarmedSandboxConfiguration {
    serde_json::from_str(CONFIGURATION_JSON).expect("the fixture matches the server's wire shape")
}

#[test]
#[cfg(not(feature = "skip_login"))]
fn configuration_errors_return_after_one_request() {
    let api = authenticated_client();
    for status in [403, 503] {
        let mut server = ChannelState::mock_server();
        let request = server
            .mock("GET", "/internal/prewarmed-sandboxes/sandbox/config")
            .with_status(status)
            .with_header("Retry-After", "1")
            .with_body(r#"{"error":"configuration unavailable"}"#)
            .expect(1)
            .create();
        drop(server);

        let error = block_on(api.fetch_configuration("sandbox")).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::server::server_api::presigned_upload::HttpStatusError>()
                .unwrap()
                .status,
            status as u16,
        );
        request.assert();
    }
}

#[test]
fn deserializes_a_server_configuration() {
    let config = parse_configuration();

    assert_eq!(config.generation, "abc123");
    assert_eq!(config.preparation.harness, "oz");

    let repository = &config.preparation.repositories[0];
    assert_eq!(repository.forge, CodeForge::GitHub);
    assert_eq!(repository.owner, "warpdotdev");
    assert_eq!(repository.name, "warp");
    assert_eq!(
        repository.checkout,
        Some(RepositoryHeadRef::Branch("main".to_string()))
    );
    assert_eq!(
        repository
            .clone_from
            .as_ref()
            .map(|repo| repo.repo.as_str()),
        Some("warp-fork")
    );
    assert!(repository.preserve_origin);

    let deferred = &config.preparation.deferred_repositories[0];
    assert_eq!(deferred.code_forge, Some(CodeForge::GitHub));
    assert_eq!(deferred.owner, "warpdotdev");
    assert_eq!(deferred.repo, "warp-server");

    assert_eq!(config.preparation.setup_commands, ["./script/bootstrap"]);
    assert!(config.preparation.mcp_servers.contains_key("linear"));
    assert_eq!(config.preparation.skills, ["warpdotdev/warp:review"]);
    assert_eq!(config.preparation.factory_skill_dirs, [".agents/skills"]);
    assert!(config.preparation.computer_use_enabled);

    assert_eq!(
        config.environment_variables.get("WARP_FACTORY_REPO_DIR"),
        Some(&"factory".to_string())
    );
    assert_eq!(config.git_credentials[0].host, "github.com");
    assert_eq!(config.git_credentials[0].token, "ghs_secret");
    assert_eq!(
        config.git_credentials[0].username.as_deref(),
        Some("x-access-token")
    );
    assert!(config.git_credentials[0].email.is_none());
    assert_eq!(config.managed_secrets.len(), 1);
}

#[test]
fn configuration_debug_redacts_credentials_and_raw_preparation_values() {
    let mut config = parse_configuration();
    config
        .managed_secrets
        .insert("TOKEN".into(), "managed-secret-value".into());
    config.environment_variables.insert(
        "WARP_FACTORY_REPO_CLONE_URL".into(),
        "https://environment-secret-value@example.com/repo.git".into(),
    );
    config.git_credentials[0].token = "git-secret-value".into();
    config.preparation.setup_commands = vec!["login --token setup-secret-value".into()];
    config.preparation.mcp_servers = serde_json::from_value(serde_json::json!({
        "local": {
            "command": "mcp",
            "args": ["--token", "mcp-argument-secret-value"],
            "env": {"TOKEN": "mcp-environment-secret-value"}
        },
        "remote": {
            "url": "https://example.com/mcp",
            "headers": {"Authorization": "Bearer mcp-header-secret-value"}
        }
    }))
    .unwrap();

    for output in [
        format!("{config:?}"),
        format!("{config:#?}"),
        format!("{:?}", config.preparation),
        format!("{:#?}", config.preparation),
    ] {
        for secret in [
            "managed-secret-value",
            "environment-secret-value",
            "git-secret-value",
            "setup-secret-value",
            "mcp-argument-secret-value",
            "mcp-environment-secret-value",
            "mcp-header-secret-value",
        ] {
            assert!(!output.contains(secret), "Debug output exposed {secret}");
        }
        assert!(output.contains("mcp_server_count"));
        assert!(output.contains("computer_use_enabled"));
    }
    let output = format!("{config:?}");
    assert!(output.contains("abc123"));
    assert!(output.contains("managed_secret_count: 2"));
    assert!(output.contains("git_credential_count: 1"));
}

#[test]
fn ready_status_sends_the_generation_as_the_manifest_hash() {
    let ready = PrewarmedSandboxStatusInput {
        generation: "abc123".to_string(),
        state: PrewarmedSandboxState::Ready.as_wire_value(),
        manifest_hash: Some("abc123".to_string()),
    };
    assert_eq!(
        serde_json::to_value(&ready).unwrap(),
        serde_json::json!({
            "generation": "abc123",
            "state": "ready",
            "manifest_hash": "abc123",
        })
    );

    let failed = PrewarmedSandboxStatusInput {
        generation: "abc123".to_string(),
        state: PrewarmedSandboxState::Failed.as_wire_value(),
        manifest_hash: None,
    };
    assert_eq!(
        serde_json::to_value(&failed).unwrap(),
        serde_json::json!({ "generation": "abc123", "state": "failed" })
    );
}

#[test]
fn bounds_a_stalled_request() {
    let error = futures::executor::block_on(bounded_request(
        Duration::ZERO,
        futures::future::pending::<anyhow::Result<()>>(),
    ))
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::TimedOut,
    );
}

#[test]
fn request_deadline_preserves_completed_results() {
    let value =
        futures::executor::block_on(bounded_request(REQUEST_TIMEOUT, async { Ok(42) })).unwrap();
    assert_eq!(value, 42);
    let error = futures::executor::block_on(bounded_request::<()>(REQUEST_TIMEOUT, async {
        anyhow::bail!("credential failure")
    }))
    .unwrap_err();
    assert_eq!(error.to_string(), "credential failure");
}

#[cfg(not(feature = "skip_login"))]
fn authenticated_client() -> ServerApi {
    let auth_state = Arc::new(crate::auth::auth_state::AuthState::new_for_test());
    auth_state.set_remote_server_bearer_token("test-access-token".to_string());
    let (events, _) = async_channel::unbounded();
    let api = ServerApi::new_with_parts(
        Arc::new(http_client::Client::new_for_test()),
        auth_state,
        events,
        None,
        None,
        crate::server::telemetry::TelemetryApi::new(),
    );
    api.base_client.set_ambient_workload_token_for_test(
        "test-workload-token".to_string(),
        Some(Utc::now() + chrono::TimeDelta::hours(1)),
    );
    api
}

#[test]
#[cfg(not(feature = "skip_login"))]
fn all_endpoints_use_cached_credentials_and_expected_payloads() {
    let api = authenticated_client();
    let mut server = ChannelState::mock_server();
    let config = server
        .mock("GET", "/internal/prewarmed-sandboxes/sandbox/config")
        .match_header("authorization", "Bearer test-access-token")
        .match_header(
            PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER,
            "test-workload-token",
        )
        .with_status(200)
        .with_body(CONFIGURATION_JSON)
        .create();
    let status = server
        .mock("POST", "/internal/prewarmed-sandboxes/sandbox/status")
        .match_header("authorization", "Bearer test-access-token")
        .match_header(
            PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER,
            "test-workload-token",
        )
        .match_body(Matcher::Json(serde_json::json!({
            "generation": "abc123", "state": "ready", "manifest_hash": "abc123",
        })))
        .with_status(204)
        .create();
    let heartbeat = server
        .mock("POST", "/internal/prewarmed-sandboxes/sandbox/heartbeat")
        .match_header("authorization", "Bearer test-access-token")
        .match_header(
            PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER,
            "test-workload-token",
        )
        .match_body(Matcher::Json(serde_json::json!({"generation": "abc123"})))
        .with_status(204)
        .create();
    drop(server);
    block_on(async {
        assert_eq!(
            api.fetch_configuration("sandbox").await.unwrap().generation,
            "abc123"
        );
        api.report_status("sandbox", "abc123", PrewarmedSandboxState::Ready)
            .await
            .unwrap();
        api.heartbeat("sandbox", "abc123").await.unwrap();
    });
    config.assert();
    status.assert();
    heartbeat.assert();
}

#[test]
#[cfg(not(feature = "skip_login"))]
fn request_debug_redacts_both_credentials() {
    let api = authenticated_client();
    let mut client = http_client::Client::new_for_test();
    client.set_before_request_fn(Box::new(|request, _| {
        for name in ["authorization", PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER] {
            assert!(request.headers()[name].is_sensitive());
        }
        let debug = format!("{request:?}");
        assert!(!debug.contains("test-access-token"));
        assert!(!debug.contains("test-workload-token"));
    }));
    let mut server = ChannelState::mock_server();
    let request = server
        .mock("GET", "/internal/prewarmed-sandboxes/sandbox/config")
        .match_header("authorization", "Bearer test-access-token")
        .match_header(
            PREWARMED_SANDBOX_WORKLOAD_TOKEN_HEADER,
            "test-workload-token",
        )
        .with_status(204)
        .expect(1)
        .create();
    drop(server);
    let url = ServerApi::prewarmed_sandbox_url("sandbox", "config");

    block_on(async {
        let response = api
            .prewarmed_sandbox_request(client.get(&url))
            .await
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 204);
    });
    request.assert();
}

#[test]
#[cfg(not(feature = "skip_login"))]
fn rejects_requests_without_access_credentials() {
    let api = ServerApi::new_for_test();
    api.base_client
        .set_ambient_workload_token_for_test("test-workload-token".to_string(), None);
    let error = block_on(api.heartbeat("sandbox", "abc123")).unwrap_err();
    assert!(
        error.to_string().contains("requires an access token"),
        "{error:#}"
    );
}
