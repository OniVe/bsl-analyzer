//! `outline` over the real MCP transport.
//!
//! The unit suite checks the shape of the map. What it cannot check is the promise the tool
//! is built on — that the answer comes from one parse of one file and the resident takes no
//! part in it — because that is a property of the SERVER, not of a function: it holds only
//! while nothing in the wiring reaches for the resident when it happens to be warm.

use std::path::Path;
use std::time::Duration;

use mcp_server::{serve_stream, McpProfile, McpServer, SharedState};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Map, Value};
use tempfile::TempDir;

// Shared helpers for the gates in this binary; not every binary uses every one.
#[allow(dead_code)]
mod common;

type Client = RunningService<RoleClient, ()>;

const MODULE_REL: &str = "CommonModules/ПервыйОбщийМодуль/Ext/Module.bsl";

/// Copy the checked-in metadata fixture into a scratch dir, so the derived caches never land
/// in the repo tree and each run starts cold.
fn stage_workspace() -> TempDir {
    let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../bsl-metadata/fixtures/designer"));
    let dst = TempDir::new().expect("scratch workspace");
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.expect("walk fixture");
        let rel = entry.path().strip_prefix(src).expect("path under fixture root");
        let target = dst.path().join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).expect("mkdir");
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
    dst
}

/// A workspace server. `warm` decides whether the resident is asked to build BEFORE the
/// session starts — the difference between the two stands below.
async fn workspace_client(root: &Path, warm: bool) -> Client {
    let state = SharedState::workspace(root.to_path_buf()).expect("valid workspace project");
    if warm {
        state.warm_start();
    }
    let server = McpServer::new(McpProfile::Workspace, state);
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(serve_stream(server, server_io));
    ().serve(client_io).await.expect("session initialized")
}

async fn reference_client() -> Client {
    let server = McpServer::new(McpProfile::Reference, SharedState::reference(None));
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(serve_stream(server, server_io));
    ().serve(client_io).await.expect("session initialized")
}

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

async fn call(client: &Client, tool: &'static str, call_args: Map<String, Value>) -> Value {
    let request = CallToolRequestParams::new(tool).with_arguments(call_args);
    client
        .call_tool(request)
        .await
        .expect("transport ok")
        .structured_content
        .expect("a structured body")
}

/// Wait until the resident reports `ready`, so the second stand below really is warm.
async fn wait_until_resident_is_ready(client: &Client) {
    let mut deadline = common::Patience::new(
        std::time::Duration::from_secs(180),
        std::time::Duration::from_millis(100),
    );
    loop {
        let status = call(client, "diagnostics", args(&[("action", Value::from("status"))])).await;
        if status["state"] == "ready" {
            return;
        }
        assert!(deadline.allows(), "the resident never became ready, last status {status}",);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn region_names(body: &Value) -> Vec<String> {
    body["regions"]
        .as_array()
        .unwrap_or_else(|| panic!("the map carries regions: {body}"))
        .iter()
        .map(|node| node["name"].as_str().expect("a name").to_owned())
        .collect()
}

fn member_names(body: &Value) -> Vec<String> {
    body["members"]
        .as_array()
        .unwrap_or_else(|| panic!("the map carries members: {body}"))
        .iter()
        .map(|node| node["name"].as_str().expect("a name").to_owned())
        .collect()
}

/// И1. The tool is served where it belongs and nowhere else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tool_is_served_in_the_workspace_profile_only() {
    let ws = stage_workspace();
    let workspace = workspace_client(ws.path(), false).await;
    let reference = reference_client().await;

    let served: Vec<String> = workspace
        .list_all_tools()
        .await
        .expect("tools/list")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert!(served.iter().any(|name| name == "outline"), "{served:?}");

    let reference_tools: Vec<String> = reference
        .list_all_tools()
        .await
        .expect("tools/list")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    // The reference profile has no project code at all, so a map of a project file is not a
    // question it can be asked.
    assert!(!reference_tools.iter().any(|name| name == "outline"), "{reference_tools:?}");
}

/// И8(б). The answer is the same whether the resident has never been built or is fully
/// ready, and it always says a file parse produced it.
///
/// Both stands are needed and neither is spare. Without the cold one, a purely resident
/// implementation would pass — it would simply answer `loading` on the first call, which the
/// warm stand never sees. Without the warm one, a hybrid ("parse it myself while the resident
/// idles, serve from the resident once it is up") would pass too, and the promise this tool
/// is built on would hold only until the workspace finished loading.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_parse_answers_whether_or_not_the_resident_is_up() {
    let ws = stage_workspace();

    // Stand 1: nothing built. `SharedState::workspace` does not start the resident, and
    // `outline` is the first and only call this session makes.
    let cold_client = workspace_client(ws.path(), false).await;
    let cold = call(&cold_client, "outline", args(&[("path", Value::from(MODULE_REL))])).await;

    // Stand 2: the resident is up and reporting ready before `outline` is called at all.
    let warm_client = workspace_client(ws.path(), true).await;
    wait_until_resident_is_ready(&warm_client).await;
    let warm = call(&warm_client, "outline", args(&[("path", Value::from(MODULE_REL))])).await;

    for (stand, body) in [("cold", &cold), ("warm", &warm)] {
        assert_eq!(body["freshness"]["source"], "file-parse", "{stand}: {body}");
        assert!(body["freshness"]["revision"].is_null(), "{stand}: {body}");
        assert!(body["freshness"]["topology_fingerprint"].is_null(), "{stand}: {body}");
        assert!(body["freshness"]["stale"].is_null(), "{stand}: {body}");
        // A retry envelope is what a resident-backed tool answers while it builds. This tool
        // has nothing to build, so the key never appears — not even on the cold stand, where
        // the workspace has been analysed by nothing at all.
        assert!(body.get("status").is_none(), "{stand}: {body}");
        assert_eq!(body["freshness"]["completeness"]["status"], "complete", "{stand}: {body}");

        assert!(region_names(body).contains(&"ПрограммныйИнтерфейс".to_owned()), "{stand}: {body}",);
        assert!(
            member_names(body).contains(&"НеУстаревшаяПроцедура".to_owned()),
            "{stand}: {body}",
        );
    }
    assert_eq!(cold, warm, "one file, one parse — the resident changes nothing");
}

/// И7(б). The pair `outline` publishes addresses the same file when handed to another tool.
///
/// An address a consumer cannot feed back is decoration, and this is the property the whole
/// location contract exists for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_pair_addresses_the_same_file_in_diagnostics() {
    let ws = stage_workspace();
    let client = workspace_client(ws.path(), true).await;
    wait_until_resident_is_ready(&client).await;

    let map = call(&client, "outline", args(&[("path", Value::from(MODULE_REL))])).await;
    let location = &map["location"];
    assert_eq!(location["position_encoding"], "utf-16", "{map}");
    assert_eq!(location["schema_version"], "1", "{map}");
    let root_id = location["root_id"].as_str().expect("root_id").to_owned();
    let path = location["path"].as_str().expect("path").to_owned();

    let diagnostics = call(
        &client,
        "diagnostics",
        args(&[
            ("action", Value::from("file")),
            ("root_id", Value::from(root_id)),
            ("path", Value::from(path.clone())),
        ]),
    )
    .await;
    let result = &diagnostics["result"];
    assert!(
        result.get("error").is_none(),
        "the pair from outline must address a file diagnostics can serve: {result}",
    );
    assert!(result["result_id"].as_str().unwrap().contains("ПервыйОбщийМодуль"), "{result}");

    // Positive control: the same path under a root that is not registered is refused by name.
    // Without it, the acceptance above would also hold for a tool that accepts anything.
    let foreign = call(
        &client,
        "diagnostics",
        args(&[
            ("action", Value::from("file")),
            ("root_id", Value::from("no-such-root")),
            ("path", Value::from(path.clone())),
        ]),
    )
    .await;
    assert_eq!(foreign["result"]["error"], "unknown_root", "{foreign}");

    // And `outline` refuses that same root by the same code: one vocabulary, two tools.
    let refused = call(
        &client,
        "outline",
        args(&[("path", Value::from(path)), ("root_id", Value::from("no-such-root"))]),
    )
    .await;
    assert_eq!(refused["error"], "unknown_root", "{refused}");
}
