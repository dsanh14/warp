use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::TimeDelta;
use futures::executor::block_on;
use warpui::App;

use super::*;
use crate::server::server_api::ai::MockAIClient;
use crate::server::server_api::prewarmed_sandbox::tests::parse_configuration;

#[test]
fn invalid_preparation_inputs_fail_before_driver_creation_or_metadata_access() {
    App::test((), |mut app| async move {
        let runner = app.add_singleton_model(|_| AgentDriverRunner);
        let foreground = runner.update(&mut app, |_, ctx| ctx.spawner());
        let setup_events = SetupClientEventReporter::noop(
            Arc::new(MockAIClient::new()),
            app.background_executor(),
        );
        let mut wrong_harness = parse_configuration();
        wrong_harness.preparation.harness = "claude".to_string();
        let mut escaping_skills = parse_configuration();
        escaping_skills.preparation.factory_skill_dirs = vec!["../outside".to_string()];
        let mut duplicate_repository = parse_configuration();
        duplicate_repository
            .preparation
            .repositories
            .push(duplicate_repository.preparation.repositories[0].clone());
        let mut invalid_mcp = parse_configuration();
        invalid_mcp.preparation.mcp_servers =
            serde_json::from_value(serde_json::json!({"invalid": {"url": 4}})).unwrap();
        for config in [
            wrong_harness,
            escaping_skills,
            duplicate_repository,
            invalid_mcp,
        ] {
            let error = prepare(&foreground, config, &setup_events)
                .await
                .unwrap_err();
            assert!(
                matches!(error, AgentDriverError::ConfigBuildFailed(_)),
                "{error}"
            );
            assert!(!app.update(|ctx| ctx.has_singleton_model::<AgentDriver>()));
        }
    });
}

/// A recorded call against the fake client.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Status(PrewarmedSandboxState),
    Heartbeat,
}

#[derive(Default)]
struct FakeClientState {
    calls: Vec<Call>,
    fail_status: bool,
    fail_heartbeat: bool,
}

#[derive(Default)]
struct FakeClient {
    state: Mutex<FakeClientState>,
    events: Option<async_channel::Sender<Call>>,
}

impl FakeClient {
    fn calls(&self) -> Vec<Call> {
        self.state.lock().expect("uncontended").calls.clone()
    }
}

#[async_trait]
impl PrewarmedSandboxClient for FakeClient {
    async fn fetch_configuration(
        &self,
        sandbox_id: &str,
    ) -> anyhow::Result<PrewarmedSandboxConfiguration> {
        panic!("Unexpected configuration fetch for {sandbox_id}")
    }

    async fn report_status(
        &self,
        sandbox_id: &str,
        generation: &str,
        state: PrewarmedSandboxState,
    ) -> anyhow::Result<()> {
        assert_eq!(sandbox_id, "sandbox");
        assert_eq!(generation, "abc123");
        let mut client = self.state.lock().expect("uncontended");
        client.calls.push(Call::Status(state));
        if let Some(events) = &self.events {
            events.try_send(Call::Status(state)).unwrap();
        }
        if client.fail_status {
            anyhow::bail!("workload token acquisition failed");
        }
        Ok(())
    }

    async fn heartbeat(&self, sandbox_id: &str, generation: &str) -> anyhow::Result<()> {
        assert_eq!(sandbox_id, "sandbox");
        assert_eq!(generation, "abc123");
        let mut client = self.state.lock().expect("uncontended");
        client.calls.push(Call::Heartbeat);
        if let Some(events) = &self.events {
            events.try_send(Call::Heartbeat).unwrap();
        }
        if client.fail_heartbeat {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into());
        }
        Ok(())
    }
}

#[test]
fn reports_ready_after_a_successful_preparation() {
    let client = FakeClient::default();
    let prepared = PreparedSandbox {
        generation: "abc123".to_string(),
        harness_working_dir: PathBuf::from("/workspace/warp"),
        deferred_repositories: Vec::new(),
    };

    let reported = futures::executor::block_on(report_preparation_outcome(
        &client,
        "sandbox",
        "abc123",
        Ok(prepared.clone()),
    ))
    .expect("a successful preparation is reported ready");

    assert_eq!(reported, prepared);
    assert_eq!(client.calls(), [Call::Status(PrewarmedSandboxState::Ready)]);
}

#[test]
fn reports_failed_when_preparation_errors() {
    let client = FakeClient::default();

    let error = futures::executor::block_on(report_preparation_outcome(
        &client,
        "sandbox",
        "abc123",
        Err(AgentDriverError::WarpDriveSyncFailed),
    ))
    .expect_err("a failed preparation propagates");

    assert_eq!(
        client.calls(),
        [Call::Status(PrewarmedSandboxState::Failed)]
    );
    assert!(
        error.to_string().contains("preparation failed"),
        "{error:#}"
    );
}

/// The reconciler marks a sandbox unhealthy after three minutes without a heartbeat, so the
/// cadence must leave room for a failed beat before that window closes.
#[test]
fn heartbeat_cadence_stays_inside_the_server_tolerance() {
    let timeout = crate::server::server_api::prewarmed_sandbox::REQUEST_TIMEOUT;
    assert!(
        (HEARTBEAT_INTERVAL + timeout) * 2 + HEARTBEAT_INTERVAL + timeout
            < SERVER_HEARTBEAT_TOLERANCE,
        "three complete heartbeat attempts must fit inside the server's tolerance"
    );
}

#[test]
fn waits_until_shortly_before_the_deadline() {
    let now = Utc::now();

    let window = wait_window(now + TimeDelta::minutes(30), now);
    assert_eq!(window, Duration::from_secs(30 * 60) - EXPIRY_MARGIN);

    // Already expired, or so close to expiry that the margin swallows the window.
    assert_eq!(
        wait_window(now - TimeDelta::minutes(1), now),
        Duration::ZERO
    );
    assert_eq!(
        wait_window(now + TimeDelta::seconds(5), now),
        Duration::ZERO
    );
}

#[test]
fn exits_cleanly_at_expiry() {
    let client = FakeClient::default();
    block_on(maintain_sandbox(
        &client,
        "sandbox",
        "abc123",
        async { panic!("expired sandboxes must not prepare") },
        future::ready(StandbyExit::Expired),
        stream::repeat(()),
    ))
    .unwrap();
    assert!(client.calls().is_empty(), "{:?}", client.calls());
}

#[test]
fn preparation_error_survives_a_status_credential_failure() {
    let client = FakeClient::default();
    client.state.lock().unwrap().fail_status = true;
    let error = block_on(maintain_sandbox(
        &client,
        "sandbox",
        "abc123",
        future::ready(Err(AgentDriverError::WarpDriveSyncFailed)),
        future::pending(),
        stream::pending(),
    ))
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<AgentDriverError>(),
        Some(AgentDriverError::WarpDriveSyncFailed)
    ));
    assert_eq!(
        client.calls(),
        [Call::Status(PrewarmedSandboxState::Failed)]
    );
}

#[test]
fn heartbeats_cover_preparation_and_idle_time_and_recover_after_timeouts() {
    block_on(async {
        let (events_tx, events) = async_channel::unbounded();
        let client = FakeClient {
            events: Some(events_tx),
            ..Default::default()
        };
        let (ticks_tx, ticks) = futures::channel::mpsc::unbounded();
        let (prepared_tx, prepared_rx) = oneshot::channel();
        let (expire_tx, expire_rx) = oneshot::channel();
        let lifecycle = maintain_sandbox(
            &client,
            "sandbox",
            "abc123",
            async { prepared_rx.await.unwrap() },
            expire_rx.map(|_| StandbyExit::Expired),
            ticks,
        );
        let exercise = async {
            client.state.lock().unwrap().fail_heartbeat = true;
            for _ in 0..3 {
                ticks_tx.unbounded_send(()).unwrap();
                assert_eq!(events.recv().await.unwrap(), Call::Heartbeat);
            }
            client.state.lock().unwrap().fail_heartbeat = false;
            prepared_tx
                .send(Ok(PreparedSandbox {
                    generation: "abc123".to_string(),
                    harness_working_dir: PathBuf::from("/workspace"),
                    deferred_repositories: Vec::new(),
                }))
                .unwrap();
            assert_eq!(
                events.recv().await.unwrap(),
                Call::Status(PrewarmedSandboxState::Ready)
            );
            ticks_tx.unbounded_send(()).unwrap();
            assert_eq!(events.recv().await.unwrap(), Call::Heartbeat);
            expire_tx.send(()).unwrap();
        };
        let (result, ()) = futures::join!(lifecycle, exercise);
        result.unwrap();
        assert_eq!(
            client.calls(),
            [
                Call::Heartbeat,
                Call::Heartbeat,
                Call::Heartbeat,
                Call::Status(PrewarmedSandboxState::Ready),
                Call::Heartbeat,
            ]
        );
    });
}
