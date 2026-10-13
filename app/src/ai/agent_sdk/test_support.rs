//! Test-only helpers shared across `agent_sdk` unit tests.

/// Build a reqwest-backed `http_client::Client` with TLS/proxy disabled and the connection
/// pool disabled (`pool_max_idle_per_host(0)`).
///
/// The pool setting is the key bit: reqwest's default 90-second idle timeout holds sockets
/// open past a test's return, which nextest's leak detector flags on retry-exhaustion tests
/// that spin up multiple connections.
pub(super) fn build_test_http_client() -> http_client::Client {
    http_client::Client::new_with_options(http_client::ClientOptions {
        disable_tls_certificates: true,
        no_proxy: true,
        pool_max_idle_per_host: Some(0),
        ..Default::default()
    })
    .expect("should not fail to build test http client")
}
