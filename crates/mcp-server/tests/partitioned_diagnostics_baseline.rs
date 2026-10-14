use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ide::partitioned_diagnostics_baseline::{
    diagnostics_manifest, diagnostics_manifest_json, diagnostics_partition_json,
    partition_object_path, DiagnosticsBaselineManifestEntry,
};
use mcp_server::{serve_stream, McpProfile, McpServer, SharedState};
use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest};
use rmcp::service::{PeerRequestOptions, RunningService};
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

fn module(root: &Path, relative: &str) {
    write(root, relative, "Процедура Тест(\n");
}

fn publish_empty_set(root: &Path) -> Vec<(String, String)> {
    let project = project_model::Project::new(root).unwrap();
    let plan = project.diagnostics_baseline_partition_plan().unwrap().unwrap();
    let directory = project_model::ManagedBaselineDirectory::open(root, "baselines", true).unwrap();
    let mut entries = Vec::new();
    let mut files = Vec::new();
    for partition in &plan.partitions {
        let bytes = diagnostics_partition_json(partition.identity.clone(), vec![]).unwrap();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let path = partition_object_path(&partition.id, &partition.key, &hash).unwrap();
        directory.create_file_new(&path).unwrap().write_all(&bytes).unwrap();
        files.push((partition.id.clone(), path.clone()));
        entries.push(DiagnosticsBaselineManifestEntry {
            partition_id: partition.id.clone(),
            file: path,
            blake3: hash,
        });
    }
    let manifest = diagnostics_manifest(plan.project_scope_fingerprint, entries);
    directory
        .create_file_new("manifest.json")
        .unwrap()
        .write_all(&diagnostics_manifest_json(&manifest).unwrap())
        .unwrap();
    files
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
async fn partitioned_baseline_uses_one_snapshot_for_file_workspace_error_and_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for source in ["src/cf", "src/cfe/A", "src/cfe/B"] {
        write(root, &format!("{source}/Configuration.xml"), "<Configuration/>");
    }
    module(root, "src/cf/Main.bsl");
    module(root, "src/cfe/A/A.bsl");
    module(root, "src/cfe/B/B.bsl");
    write(
        root,
        "bsl-analyzer.toml",
        r#"[source]
root = "src/cf"
extensions = [{ name = "A", path = "src/cfe/A", scope = "InfoBase" }, { name = "B", path = "src/cfe/B", scope = "InfoBase" }]

[diagnostics.baseline]
directory = "baselines"

[[diagnostics.baseline.groups]]
name = "vendor"
extensions = ["A"]
"#,
    );
    let files = publish_empty_set(root);
    let client = client(root).await;

    for (path, owner) in [
        ("src/cf/Main.bsl", "main"),
        ("src/cfe/A/A.bsl", "group:vendor"),
        ("src/cfe/B/B.bsl", "extension:B"),
    ] {
        let mut request = args("file");
        request.insert("path".to_owned(), json!(path));
        let body = diagnostics(&client, request).await;
        let baseline = &body["result"]["baseline"];
        assert_eq!(baseline["state"], "partial");
        assert_eq!(baseline["partitions_total"], 3);
        assert_eq!(baseline["partitions"][0]["id"], owner);
    }

    let mut workspace = args("workspace");
    workspace.insert("max_files".to_owned(), json!(1));
    let partial = diagnostics(&client, workspace.clone()).await;
    assert_eq!(partial["result"]["baseline"]["state"], "partial");
    assert_eq!(partial["result"]["baseline"]["partitions_total"], 3);

    let (_, object) = files.iter().find(|(id, _)| id == "group:vendor").unwrap();
    let object = root.join("baselines").join(object);
    let valid = std::fs::read(&object).unwrap();
    std::fs::write(&object, b"{broken").unwrap();
    let broken = diagnostics(&client, workspace.clone()).await;
    assert_eq!(broken["result"]["baseline"]["state"], "error");
    assert_eq!(broken["result"]["baseline"]["errors_total"], 1);
    assert!(broken["result"].get("aggregates").is_none());
    std::fs::write(&object, valid).unwrap();
    let recovered = diagnostics(&client, workspace.clone()).await;
    assert_eq!(recovered["result"]["baseline"]["state"], "partial");

    let request = ClientRequest::CallToolRequest(CallToolRequest::new(
        CallToolRequestParams::new("diagnostics").with_arguments(workspace),
    ));
    client
        .peer()
        .send_cancellable_request(request, PeerRequestOptions::no_options())
        .await
        .unwrap()
        .cancel(Some("partitioned cancellation".to_owned()))
        .await
        .unwrap();
    let mut file = args("file");
    file.insert("path".to_owned(), json!("src/cf/Main.bsl"));
    assert_eq!(diagnostics(&client, file).await["result"]["baseline"]["state"], "partial");

    client.cancel().await.unwrap();
}
