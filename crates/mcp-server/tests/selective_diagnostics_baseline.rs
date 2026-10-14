use std::{io::Write, path::Path, time::Duration};

use ide::partitioned_diagnostics_baseline::{
    diagnostics_manifest, diagnostics_manifest_json, diagnostics_partition_json,
    partition_object_path, DiagnosticsBaselineManifestEntry,
};
use mcp_server::{serve_stream, McpProfile, McpServer, SharedState};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Map, Value};

// Shared helpers for the gates in this binary; not every binary uses every one.
#[allow(dead_code)]
mod common;

type Client = RunningService<RoleClient, ()>;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn config(root: &Path, include: &[&str]) {
    let include = include.iter().map(|id| format!(r#""{id}""#)).collect::<Vec<_>>().join(", ");
    // The scope guard must not see truncated TOML as a different source composition.
    let mut replacement = tempfile::NamedTempFile::new_in(root).unwrap();
    write!(
        replacement,
        r#"[source]
root = "src/cf"
extensions = [{{ name = "Ext", path = "src/cfe/Ext", scope = "InfoBase" }}]
[diagnostics.baseline]
directory = "baselines"
include = [{include}]
"#
    )
    .unwrap();
    replacement.persist(root.join("bsl-analyzer.toml")).unwrap();
}

fn setup() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for source in ["src/cf", "src/cfe/Ext"] {
        write(temp.path(), &format!("{source}/Configuration.xml"), "<Configuration/>");
    }
    write(temp.path(), "src/cf/Main.bsl", "Процедура Тест(\n");
    write(temp.path(), "src/cfe/Ext/Ext.bsl", "Процедура Тест(\n");
    config(temp.path(), &["main"]);

    let project = project_model::Project::new(temp.path()).unwrap();
    let plan = project.diagnostics_baseline_partition_plan().unwrap().unwrap();
    let directory =
        project_model::ManagedBaselineDirectory::open(temp.path(), "baselines", true).unwrap();
    let entries = plan
        .partitions
        .iter()
        .map(|partition| {
            let bytes = diagnostics_partition_json(partition.identity.clone(), vec![]).unwrap();
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let path = partition_object_path(&partition.id, &partition.key, &hash).unwrap();
            directory.create_file_new(&path).unwrap().write_all(&bytes).unwrap();
            DiagnosticsBaselineManifestEntry {
                partition_id: partition.id.clone(),
                file: path,
                blake3: hash,
            }
        })
        .collect();
    let manifest = diagnostics_manifest(plan.project_scope_fingerprint, entries);
    directory
        .create_file_new("manifest.json")
        .unwrap()
        .write_all(&diagnostics_manifest_json(&manifest).unwrap())
        .unwrap();
    temp
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
            .unwrap();
        let body = result.structured_content.unwrap();
        if body["status"] != "loading" {
            return body;
        }
        assert!(deadline.allows());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn args(action: &str) -> Map<String, Value> {
    Map::from_iter([
        ("action".to_owned(), json!(action)),
        ("min_severity".to_owned(), json!("hint")),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_selective_baseline_file_owner_unsuppressed() {
    let temp = setup();
    let client = client(temp.path()).await;
    let mut request = args("file");
    request.insert("path".to_owned(), json!("src/cfe/Ext/Ext.bsl"));
    let body = diagnostics(&client, request).await;
    let result = &body["result"];
    assert!(!result["findings"].as_array().unwrap().is_empty());
    assert_eq!(result["baseline"]["selection"], "selective");
    assert_eq!(result["baseline"]["partitions_enabled"], 1);
    assert_eq!(result["baseline"]["partitions_unsuppressed"], 1);
    assert!(result["baseline"]["unsuppressed"].as_u64().unwrap() > 0);
    assert_eq!(result["baseline"]["partitions"][0]["id"], "extension:Ext");
    assert_eq!(result["baseline"]["partitions"][0]["policy"], "unsuppressed");
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_selective_baseline_workspace_mixed_policy() {
    let temp = setup();
    let client = client(temp.path()).await;
    let body = diagnostics(&client, args("workspace")).await;
    let result = &body["result"];
    assert!(!result["aggregates"].as_array().unwrap().is_empty());
    assert_eq!(result["baseline"]["selection"], "selective");
    assert!(result["baseline"]["new"].as_u64().unwrap() > 0);
    assert!(result["baseline"]["unsuppressed"].as_u64().unwrap() > 0);
    assert!(result["baseline"]["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|partition| partition["policy"] == "unsuppressed"));
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_selective_baseline_config_reload_changes_result_id() {
    let temp = setup();
    let client = client(temp.path()).await;
    let mut request = args("file");
    request.insert("path".to_owned(), json!("src/cfe/Ext/Ext.bsl"));
    let first = diagnostics(&client, request.clone()).await;
    let first_id = first["result"]["result_id"].as_str().unwrap().to_owned();
    config(temp.path(), &["main", "extension:Ext"]);

    let mut deadline =
        common::Patience::new(common::STATE_CEILING, std::time::Duration::from_millis(50));
    let changed = loop {
        let current = diagnostics(&client, request.clone()).await;
        if current["result"]["result_id"] != first_id {
            break current;
        }
        assert!(deadline.allows(), "config reload did not change result_id");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(changed["result"]["baseline"]["partitions_enabled"], 2);
    assert_eq!(changed["result"]["baseline"]["partitions_unsuppressed"], 0);
    assert_eq!(changed["result"]["baseline"]["partitions"][0]["policy"], "baseline");
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_selective_baseline_schema_15_bounds_success_and_error_envelopes() {
    let temp = setup();
    let client = client(temp.path()).await;
    let mut request = args("workspace");
    request.insert("max_output_tokens".to_owned(), json!(256));
    let success = diagnostics(&client, request.clone()).await;
    assert_eq!(success["result"]["baseline"]["selection"], "selective");
    assert!(success["result"]["baseline"]["partitions_total"].is_number());
    assert!(success["result"]["baseline"]["partitions_returned"].is_number());

    let manifest: Value = serde_json::from_slice(
        &std::fs::read(temp.path().join("baselines/manifest.json")).unwrap(),
    )
    .unwrap();
    let main = manifest["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["partition_id"] == "main")
        .unwrap()["file"]
        .as_str()
        .unwrap();
    let object = temp.path().join("baselines").join(main);
    let mut changed = std::fs::read(&object).unwrap();
    changed.push(b'\n');
    std::fs::write(object, changed).unwrap();
    let mut deadline =
        common::Patience::new(common::STATE_CEILING, std::time::Duration::from_millis(50));
    let error = loop {
        let current = diagnostics(&client, request.clone()).await;
        if current["result"]["baseline"]["state"] == "error" {
            break current;
        }
        assert!(deadline.allows(), "enabled error was not reloaded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let baseline = &error["result"]["baseline"];
    assert_eq!(baseline["selection"], "selective");
    assert_eq!(baseline["partitions_enabled"], 1);
    assert!(baseline["errors_total"].as_u64().unwrap() >= 1);
    assert_eq!(baseline["errors"][0]["partition_id"], "main");
    assert!(error["result"].get("aggregates").is_none());
    client.cancel().await.unwrap();
}
