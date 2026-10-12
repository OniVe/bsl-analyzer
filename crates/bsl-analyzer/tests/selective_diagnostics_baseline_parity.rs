mod common;

use common::*;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

async fn mcp_client(root: &Path) -> McpClient {
    use rmcp::ServiceExt;
    let server = mcp_server::McpServer::new(
        mcp_server::McpProfile::Workspace,
        mcp_server::SharedState::workspace(root.to_path_buf()).unwrap(),
    );
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(mcp_server::serve_stream(server, server_io));
    ().serve(client_io).await.unwrap()
}

async fn mcp_diagnostics(client: &McpClient, action: &str, path: Option<&str>) -> Value {
    let mut arguments = serde_json::Map::from_iter([
        ("action".to_owned(), json!(action)),
        ("min_severity".to_owned(), json!("hint")),
    ]);
    if let Some(path) = path {
        arguments.insert("path".to_owned(), json!(path));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let result = client
            .call_tool(
                rmcp::model::CallToolRequestParams::new("diagnostics")
                    .with_arguments(arguments.clone()),
            )
            .await
            .unwrap();
        let body = result.structured_content.unwrap();
        if body["status"] != "loading" {
            return body;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selective_baseline_cli_mcp_lsp_parity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for source in ["src/cf", "src/cfe/Ext"] {
        std::fs::create_dir_all(root.join(source)).unwrap();
        std::fs::write(root.join(source).join("Configuration.xml"), "<Configuration/>").unwrap();
    }
    let main_known = "src/cf/Known.bsl";
    let main_new = "src/cf/New.bsl";
    let extension = "src/cfe/Ext/Ext.bsl";
    std::fs::write(root.join(main_known), BROKEN).unwrap();
    std::fs::write(root.join(extension), BROKEN).unwrap();
    std::fs::write(
        root.join("bsl-analyzer.toml"),
        r#"[source]
root = "src/cf"
extensions = [{ name = "Ext", path = "src/cfe/Ext", scope = "InfoBase" }]
[diagnostics.baseline]
directory = "baselines"
include = ["main"]
"#,
    )
    .unwrap();
    let created = Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
        .current_dir(root)
        .args(["diagnostics", "baseline", "create", "-s", "."])
        .output()
        .unwrap();
    assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));
    std::fs::write(root.join(main_new), BROKEN).unwrap();

    std::fs::create_dir(root.join("reports")).unwrap();
    let analyzed = Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
        .current_dir(root)
        .args(["analyze", "-s", ".", "-r", "json,codequality", "-o", "reports", "-q"])
        .output()
        .unwrap();
    assert!(analyzed.status.success(), "{}", String::from_utf8_lossy(&analyzed.stderr));
    let cli: Value =
        serde_json::from_slice(&std::fs::read(root.join("reports/bsl-json.json")).unwrap())
            .unwrap();
    let cli_main = cli["baseline"]["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|partition| partition["id"] == "main")
        .unwrap();
    let cli_ext = cli["baseline"]["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|partition| partition["id"] == "extension:Ext")
        .unwrap();
    assert_eq!(cli_main["policy"], "baseline");
    assert!(cli_main["known"].as_u64().unwrap() > 0 && cli_main["new"].as_u64().unwrap() > 0);
    assert_eq!(cli_ext["policy"], "unsuppressed");
    assert!(cli_ext["unsuppressed"].as_u64().unwrap() > 0);

    let client = mcp_client(root).await;
    let mcp_known = mcp_diagnostics(&client, "file", Some(main_known)).await;
    assert!(mcp_known["result"]["findings"].as_array().unwrap().is_empty());
    assert_eq!(mcp_known["result"]["baseline"]["partitions"][0]["id"], "main");
    assert_eq!(mcp_known["result"]["baseline"]["partitions"][0]["policy"], "baseline");
    let mcp_ext = mcp_diagnostics(&client, "file", Some(extension)).await;
    let finding = mcp_ext["result"]["findings"].as_array().unwrap().first().unwrap();
    assert_eq!(mcp_ext["result"]["baseline"]["partitions"][0]["id"], "extension:Ext");
    assert_eq!(mcp_ext["result"]["baseline"]["partitions"][0]["policy"], "unsuppressed");
    assert!(mcp_ext["result"]["baseline"]["unsuppressed"].as_u64().unwrap() > 0);
    let workspace = mcp_diagnostics(&client, "workspace", None).await;
    assert_eq!(workspace["result"]["baseline"]["selection"], "selective");
    assert!(workspace["result"]["baseline"]["unsuppressed"].as_u64().unwrap() > 0);

    let line = finding["range"]["start_line"].as_u64().unwrap() as usize;
    let snippet = ide::diagnostics_baseline::normalize_diagnostic_snippet(
        BROKEN.lines().nth(line).unwrap_or_default(),
    );
    let fingerprint = ide::diagnostics_baseline::diagnostic_fingerprint(
        extension,
        finding["code"].as_str().unwrap(),
        &snippet,
        0,
    );
    let quality: Value = serde_json::from_slice(
        &std::fs::read(root.join("reports/gl-code-quality-report.json")).unwrap(),
    )
    .unwrap();
    assert!(quality.as_array().unwrap().iter().any(|item| item["fingerprint"] == fingerprint));

    let mut lsp = Lsp::start(root);
    assert!(lsp.open(&root.join(main_known), BROKEN)["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!lsp.open(&root.join(main_new), BROKEN)["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(lsp.open(&root.join(extension), BROKEN)["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| diagnostic["code"] == finding["code"]));

    client.cancel().await.unwrap();
}
