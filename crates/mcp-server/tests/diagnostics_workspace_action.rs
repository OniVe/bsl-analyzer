//! The `workspace` action over a real MCP session.
//!
//! The action fans out to rayon while the caller holds the resident session, and
//! rayon runs part of that fan-out on the calling thread. Only an end-to-end call
//! exercises that combination — an in-process `state.read` does not.

use std::path::Path;
use std::time::Duration;

use mcp_server::{serve_stream, McpProfile, McpServer, SharedState};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Map, Value};

// Shared helpers for the gates in this binary; not every binary uses every one.
#[allow(dead_code)]
mod common;

type Client = RunningService<RoleClient, ()>;
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn write_module(root: &Path, name: &str, body: &str) {
    write(
        root,
        &format!("CommonModules/{name}.xml"),
        &format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject>
  <CommonModule uuid="00000000-0000-0000-0000-0000000000{id:02}">
    <Properties><Name>{name}</Name><Server>true</Server></Properties>
  </CommonModule>
</MetaDataObject>"#,
            id = name.len()
        ),
    );
    write(root, &format!("CommonModules/{name}/Ext/Module.bsl"), body);
}

async fn client(root: &Path) -> Client {
    let server = McpServer::new(
        McpProfile::Workspace,
        SharedState::workspace(root.to_path_buf()).expect("valid project"),
    );
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(serve_stream(server, server_io));
    ().serve(client_io).await.expect("session initialized")
}

async fn diagnostics(client: &Client, arguments: Map<String, Value>) -> Value {
    let mut deadline =
        common::Patience::new(common::STATE_CEILING, std::time::Duration::from_millis(20));
    loop {
        let result = client
            .call_tool(CallToolRequestParams::new("diagnostics").with_arguments(arguments.clone()))
            .await
            .expect("diagnostics call");
        let body = result.structured_content.expect("structured response");
        if body["status"] != "loading" {
            return body;
        }
        assert!(deadline.allows(), "resident did not become ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_action_answers_without_a_baseline() {
    let _guard = TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_module(root, "Первый", "Процедура Выполнить() Экспорт\n    А = 1;\nКонецПроцедуры\n");
    write_module(root, "Второй", "Процедура Ещё() Экспорт\n    Б = 2;\nКонецПроцедуры\n");

    let client = client(root).await;
    let mut arguments = Map::from_iter([("action".to_owned(), json!("workspace"))]);
    arguments.insert("min_severity".to_owned(), json!("hint"));
    let body = diagnostics(&client, arguments.clone()).await;

    assert!(body["result"]["files_total"].as_u64().unwrap() >= 2, "{body}");

    // An unreadable file turns into a resident hole and rebuilds the database under
    // the session — the state in which this sweep used to panic. Mode bits are the
    // only trigger found for it, and a privileged user ignores them, so the leg
    // states plainly when it did not run instead of passing vacuously.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let source = root.join("CommonModules/Первый/Ext/Module.bsl");
        let original = std::fs::metadata(&source).unwrap().permissions();
        std::fs::write(
            &source,
            "Процедура Выполнить() Экспорт\n    А = 1;\n    // drift\nКонецПроцедуры\n",
        )
        .unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&source).is_err() {
            let stale = diagnostics(&client, arguments.clone()).await;
            assert!(stale["result"]["files_total"].as_u64().unwrap() >= 2, "{stale}");
        } else {
            eprintln!(
                "skipping the rebuilt-resident leg: mode 0o000 is not an obstacle for this user"
            );
        }
        std::fs::set_permissions(&source, original).unwrap();
    }
    client.cancel().await.expect("session closed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_action_answers_with_a_baseline_configured() {
    let _guard = TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_module(root, "Первый", "Процедура Выполнить() Экспорт\n    А = 1;\nКонецПроцедуры\n");
    write_module(root, "Второй", "Процедура Ещё() Экспорт\n    Б = 2;\nКонецПроцедуры\n");
    write(root, "bsl-analyzer.toml", "[diagnostics.baseline]\npath = \"baseline.json\"\n");
    write(
        root,
        "baseline.json",
        r#"{"schema_version":1,"scope":{"source_root":"","extensions":[]},"diagnostics":[]}"#,
    );

    let client = client(root).await;
    let mut arguments = Map::from_iter([("action".to_owned(), json!("workspace"))]);
    arguments.insert("min_severity".to_owned(), json!("hint"));
    let body = diagnostics(&client, arguments.clone()).await;

    assert!(body["result"]["files_total"].as_u64().unwrap() >= 2, "{body}");
    assert_eq!(body["result"]["baseline"]["state"], "full", "{body}");

    // A file that becomes unreadable turns into a resident hole and rebuilds the
    // database behind the session — the sweep must survive that, not panic.
    // An unreadable file turns into a resident hole and rebuilds the database under
    // the session — the state in which this sweep used to panic. Mode bits are the
    // only trigger found for it, and a privileged user ignores them, so the leg
    // states plainly when it did not run instead of passing vacuously.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let source = root.join("CommonModules/Первый/Ext/Module.bsl");
        let original = std::fs::metadata(&source).unwrap().permissions();
        std::fs::write(
            &source,
            "Процедура Выполнить() Экспорт\n    А = 1;\n    // drift\nКонецПроцедуры\n",
        )
        .unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&source).is_err() {
            let stale = diagnostics(&client, arguments.clone()).await;
            assert!(stale["result"]["files_total"].as_u64().unwrap() >= 2, "{stale}");
        } else {
            eprintln!(
                "skipping the rebuilt-resident leg: mode 0o000 is not an obstacle for this user"
            );
        }
        std::fs::set_permissions(&source, original).unwrap();
    }
    client.cancel().await.expect("session closed");
}
