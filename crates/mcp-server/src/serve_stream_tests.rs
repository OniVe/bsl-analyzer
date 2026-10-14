use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientRequest, PingRequest};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{serve_stream_with_shutdown, McpProfile, McpServer, SharedState};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A completed MCP handshake exercises the service task that owns the transport.
async fn initialized_session(
    shutdown: &CancellationToken,
) -> (RunningService<RoleClient, ()>, JoinHandle<anyhow::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = McpServer::new(McpProfile::Reference, SharedState::shared());
    let task = tokio::spawn(serve_stream_with_shutdown(server, server_io, shutdown.clone()));
    let client = ().serve(client_io).await.expect("client initializes");
    (client, task)
}

async fn initialized_workspace_session(
    state: SharedState,
    profile: McpProfile,
    shutdown: &CancellationToken,
) -> (RunningService<RoleClient, ()>, JoinHandle<anyhow::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = McpServer::new(profile, state);
    let task = tokio::spawn(serve_stream_with_shutdown(server, server_io, shutdown.clone()));
    let client = ().serve(client_io).await.expect("workspace client initializes");
    (client, task)
}

/// A stalled handshake or detached service task must not make shutdown wait forever.
async fn session_finished(task: JoinHandle<anyhow::Result<()>>) {
    tokio::time::timeout(SHUTDOWN_TIMEOUT, task)
        .await
        .expect("session finishes within the shutdown budget")
        .expect("session task joins")
        .expect("session closes cleanly");
}

/// One disconnected client must not cancel its siblings; broker shutdown must close them.
#[tokio::test]
async fn stream_shutdown_closes_transport_without_cancelling_sibling_sessions() {
    let shutdown = CancellationToken::new();
    let (first_client, first_task) = initialized_session(&shutdown).await;
    let (second_client, second_task) = initialized_session(&shutdown).await;

    first_client.cancel().await.expect("first client disconnects");
    session_finished(first_task).await;
    let request = ClientRequest::PingRequest(PingRequest::default());
    second_client.send_request(request.clone()).await.expect("sibling remains connected");

    shutdown.cancel();
    session_finished(second_task).await;
    tokio::time::timeout(SHUTDOWN_TIMEOUT, second_client.send_request(request))
        .await
        .expect("closed transport answers without hanging")
        .expect_err("no request is served after session shutdown completes");
}

/// A silent connection must release its socket even if it never initializes MCP.
#[tokio::test]
async fn stream_shutdown_closes_uninitialized_transport() {
    let shutdown = CancellationToken::new();
    let (mut client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = McpServer::new(McpProfile::Reference, SharedState::shared());
    let task = tokio::spawn(serve_stream_with_shutdown(server, server_io, shutdown.clone()));
    tokio::task::yield_now().await;

    shutdown.cancel();
    session_finished(task).await;
    let mut byte = [0];
    let read = tokio::time::timeout(SHUTDOWN_TIMEOUT, client_io.read(&mut byte))
        .await
        .expect("silent transport closes without hanging")
        .expect("read observes transport closure");
    assert_eq!(read, 0, "the silent peer receives EOF");
}

/// A cache-scope failure closes every session of that backend without relying on the daemon's
/// ordinary shutdown token (which remains available for the supersession drain protocol).
#[tokio::test]
async fn workspace_cache_scope_stops_all_session_transports() {
    let daemon_shutdown = CancellationToken::new();
    let state = SharedState::shared();
    let server = McpServer::new(McpProfile::Reference, state.clone());
    let first = server.clone();
    let second = server;
    let (first_client, first_task) = {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let task =
            tokio::spawn(serve_stream_with_shutdown(first, server_io, daemon_shutdown.clone()));
        (().serve(client_io).await.expect("first client initializes"), task)
    };
    let (second_client, second_task) = {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let task =
            tokio::spawn(serve_stream_with_shutdown(second, server_io, daemon_shutdown.clone()));
        (().serve(client_io).await.expect("second client initializes"), task)
    };

    state.scope_transport_stop().cancel();
    session_finished(first_task).await;
    session_finished(second_task).await;
    assert!(
        first_client
            .send_request(ClientRequest::PingRequest(PingRequest::default()))
            .await
            .is_err(),
        "scope retirement closes the first session"
    );
    assert!(
        second_client
            .send_request(ClientRequest::PingRequest(PingRequest::default()))
            .await
            .is_err(),
        "scope retirement closes the second session"
    );
    assert!(!daemon_shutdown.is_cancelled(), "scope retirement leaves broker shutdown distinct");
}

/// The warning is emitted to tracing during one successful workspace snapshot boot.
/// A real framed MCP session can then make repeated tool requests without receiving
/// that warning as protocol content or triggering per-request duplicates.
#[tokio::test]
async fn legacy_workspace_warning_stays_out_of_repeated_mcp_tool_responses() {
    let workspace = tempfile::tempdir().unwrap();
    let main = workspace.path().join("src/cf");
    let extension = workspace.path().join("ext");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(main.join("Configuration.xml"), "<Configuration/>").unwrap();
    std::fs::write(
        extension.join("Configuration.xml"),
        "<Properties><ConfigurationExtensionPurpose>Customization</ConfigurationExtensionPurpose></Properties>",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("bsl-analyzer.toml"),
        "[source]\nroot = \"src/cf\"\nextensions = [{ name = \"Feature\", path = \"ext\" }]\n",
    )
    .unwrap();
    let cache = crate::cache::WorkspaceCacheLayout::from_root(workspace.path().join("cache"));
    let state = SharedState::workspace_with_cache(workspace.path().to_path_buf(), cache)
        .expect("legacy extension project bootstraps");

    let shutdown = CancellationToken::new();
    let (client, task) =
        initialized_workspace_session(state.clone(), McpProfile::Workspace, &shutdown).await;
    let tools = client.list_all_tools().await.expect("tools/list is framed and answered");
    assert!(tools.iter().any(|tool| tool.name == "diagnostics"));
    for _ in 0..2 {
        let response = client
            .call_tool(CallToolRequestParams::new("diagnostics").with_arguments(
                serde_json::json!({"action": "schema"}).as_object().unwrap().clone(),
            ))
            .await
            .expect("tools/call is framed and answered");
        let wire_result = serde_json::to_string(&response).unwrap();
        assert!(wire_result.contains("schema"), "tool response has its schema: {response:?}");
        assert!(
            !wire_result.contains("legacy extension scope")
                && !wire_result.contains("DataSeparation"),
            "the boot advisory remains in tracing, outside MCP response payloads"
        );
    }

    client.cancel().await.expect("client disconnects");
    session_finished(task).await;
    state.shutdown();
}

/// A scope-only edit retires the old frozen workspace over the live stream; a client
/// reconnects against the newly resolved cache namespace and can issue a real tool call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_scope_change_retires_mcp_stream_and_reconnects_with_explicit_cache() {
    let workspace = tempfile::tempdir().unwrap();
    let cache_parent = tempfile::tempdir().unwrap();
    let root = workspace.path().to_path_buf();
    let main = root.join("src/cf");
    let extension = root.join("ext");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(main.join("Configuration.xml"), "<Configuration/>").unwrap();
    std::fs::write(
        extension.join("Configuration.xml"),
        "<Properties><ConfigurationExtensionPurpose>Customization</ConfigurationExtensionPurpose></Properties>",
    )
    .unwrap();
    let project_file = root.join("bsl-analyzer.toml");
    let config_for_scope = |scope: Option<&str>| {
        let scope = scope.map_or_else(String::new, |scope| format!(", scope = \"{scope}\""));
        format!(
            "[source]\nroot = \"src/cf\"\nextensions = [{{ name = \"Feature\", path = \"ext\"{scope} }}]\n"
        )
    };
    std::fs::write(&project_file, config_for_scope(None)).unwrap();

    let old_project = crate::project::at(&root).expect("legacy project resolves");
    let cache_base = cache_parent.path().join("cache");
    let old_cache = crate::cache::WorkspaceCacheLayout::for_project(
        &old_project,
        Some(&cache_base),
        cache_parent.path(),
        None,
    )
    .expect("legacy cache scope resolves");
    let old_layout = old_cache.root().to_path_buf();
    let old_state = SharedState::workspace_with_cache(root.clone(), old_cache)
        .expect("legacy workspace boots under its frozen cache scope");
    let old_stop = old_state.scope_transport_stop();
    let old_shutdown = CancellationToken::new();
    let (old_client, old_task) =
        initialized_workspace_session(old_state.clone(), McpProfile::Workspace, &old_shutdown)
            .await;
    let before =
        old_client
            .call_tool(CallToolRequestParams::new("diagnostics").with_arguments(
                serde_json::json!({"action": "schema"}).as_object().unwrap().clone(),
            ))
            .await
            .expect("old scope serves a real tool call");
    assert!(serde_json::to_string(&before).unwrap().contains("schema"));

    std::fs::write(&project_file, config_for_scope(Some("InfoBase"))).unwrap();
    tokio::time::timeout(crate::change_hub::test_support::STATE_CEILING, old_stop.cancelled())
        .await
        .expect("scope-only project drift retires the old transport");
    session_finished(old_task).await;
    assert!(
        old_client.send_request(ClientRequest::PingRequest(PingRequest::default())).await.is_err(),
        "the frozen legacy session does not answer after retirement"
    );
    old_state.shutdown();

    let explicit_project = crate::project::at(&root).expect("explicit project resolves");
    let explicit_cache = crate::cache::WorkspaceCacheLayout::for_project(
        &explicit_project,
        Some(&cache_base),
        cache_parent.path(),
        None,
    )
    .expect("InfoBase cache scope resolves");
    assert_ne!(explicit_cache.root(), old_layout, "scope changes the frozen cache layout");
    let explicit_state = SharedState::workspace_with_cache(root, explicit_cache)
        .expect("new workspace boots with the explicit cache scope");
    let explicit_shutdown = CancellationToken::new();
    let (explicit_client, explicit_task) = initialized_workspace_session(
        explicit_state.clone(),
        McpProfile::Workspace,
        &explicit_shutdown,
    )
    .await;
    let after =
        explicit_client
            .call_tool(CallToolRequestParams::new("diagnostics").with_arguments(
                serde_json::json!({"action": "schema"}).as_object().unwrap().clone(),
            ))
            .await
            .expect("reconnected explicit scope serves a real tool call");
    assert!(serde_json::to_string(&after).unwrap().contains("schema"));
    explicit_client.cancel().await.expect("explicit-scope client disconnects");
    session_finished(explicit_task).await;
    explicit_state.shutdown();
}
