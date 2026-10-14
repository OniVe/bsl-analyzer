//! The location contract across tools, over the real MCP transport.
//!
//! The unit suites check each tool's own shaping; what cannot be checked there is whether
//! the tools AGREE — that a pair one of them hands out addresses a file another one can
//! serve, and that two subsystems answering about one workspace name the same topology.
//! Those are the properties the contract exists for, and they are only observable end to
//! end.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mcp_server::{serve_stream, McpProfile, McpServer, SharedState, ToolGate};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Map, Value};
use tempfile::TempDir;

// Shared helpers for the gates in this binary; not every binary uses every one.
#[allow(dead_code)]
mod common;

type Client = RunningService<RoleClient, ()>;

fn designer_fixture() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../bsl-metadata/fixtures/designer"))
}

/// Copy the checked-in metadata fixture into a scratch dir, so the derived caches never
/// land in the repo tree and each run starts cold.
fn stage_workspace() -> TempDir {
    let src = designer_fixture();
    let dst = TempDir::new().expect("scratch workspace");
    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.expect("walk fixture");
        let rel = entry.path().strip_prefix(&src).expect("path under fixture root");
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

fn copy_fixture_into(dst: &Path) {
    let src = designer_fixture();
    for entry in walkdir::WalkDir::new(&src) {
        let entry = entry.expect("walk fixture");
        let rel = entry.path().strip_prefix(&src).expect("path under fixture root");
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).expect("mkdir");
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("mkdir parent");
            }
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

async fn workspace_client(root: &Path) -> Client {
    let server = McpServer::new(
        McpProfile::Workspace,
        SharedState::workspace(root.to_path_buf()).expect("valid workspace project"),
    );
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(serve_stream(server, server_io));
    ().serve(client_io).await.expect("session initialized")
}

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

/// Call a tool, retrying past the "still building" envelope. The envelope is told apart by
/// its `status: "loading"` field — how a consumer is meant to read it — never by matching
/// the human sentence beside it.
async fn poll(client: &Client, tool: &'static str, call_args: Map<String, Value>) -> Value {
    let mut deadline = common::Patience::new(
        std::time::Duration::from_secs(120),
        std::time::Duration::from_millis(100),
    );
    loop {
        let call = CallToolRequestParams::new(tool).with_arguments(call_args.clone());
        let result = client.call_tool(call).await.expect("transport ok");
        if let Some(structured) = result.structured_content {
            if structured["status"] != "loading" {
                return structured;
            }
        }
        assert!(deadline.allows(), "{tool} never became ready for {call_args:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A pair handed out by one tool must address the same file when handed back to another.
/// This is the whole point of publishing `(root_id, path)` rather than a rendered string:
/// an address a consumer cannot use is decoration.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_published_pair_addresses_the_same_file_when_fed_back() {
    let ws = stage_workspace();
    let client = workspace_client(ws.path()).await;

    let card = poll(
        &client,
        "symbol_info",
        args(&[("symbol", Value::from("ПервыйОбщийМодуль.НеУстаревшаяПроцедура"))]),
    )
    .await;

    let definitions = card["definitions"].as_array().expect("definitions is a list");
    let location = definitions
        .first()
        .and_then(|d| d.get("location"))
        .unwrap_or_else(|| panic!("the method card carries a location: {card}"));
    assert_eq!(location["position_encoding"], "utf-16");
    assert_eq!(location["schema_version"], "1");
    let root_id = location["root_id"].as_str().expect("root_id is a string").to_owned();
    let path = location["path"].as_str().expect("path is a string").to_owned();

    // Fed back to `diagnostics file`, the pair resolves — the file is served, not refused.
    let diagnostics = poll(
        &client,
        "diagnostics",
        args(&[
            ("action", Value::from("file")),
            ("root_id", Value::from(root_id.clone())),
            ("path", Value::from(path.clone())),
        ]),
    )
    .await;
    let result = &diagnostics["result"];
    assert!(
        result.get("error").is_none(),
        "the pair from symbol_info must address a file diagnostics can serve, got {result}",
    );
    assert!(result["result_id"].as_str().unwrap().contains("ПервыйОбщийМодуль"), "{result}");

    // Positive control: the same relative path under a root that is not registered here is
    // refused by name — proving the acceptance above is about THIS pair, not about the tool
    // accepting anything at all.
    let foreign = poll(
        &client,
        "diagnostics",
        args(&[
            ("action", Value::from("file")),
            ("root_id", Value::from("no-such-root")),
            ("path", Value::from(path)),
        ]),
    )
    .await;
    assert_eq!(
        foreign["result"]["error"], "unknown_root",
        "an unregistered root is an honest refusal, not another file: {foreign}",
    );
}

/// The resident and the graph answer about one workspace, so the topology they name must
/// be the same value. A contract where two subsystems publish different fingerprints for
/// one tree tells a consumer nothing at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_resident_and_the_graph_name_the_same_topology() {
    let ws = stage_workspace();
    let client = workspace_client(ws.path()).await;

    let card = poll(
        &client,
        "symbol_info",
        args(&[("symbol", Value::from("ПервыйОбщийМодуль.НеУстаревшаяПроцедура"))]),
    )
    .await;
    let from_resident = card["freshness"]["topology_fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("the resident stamps a topology: {card}"))
        .to_owned();

    let overview = poll(&client, "graph", args(&[("action", Value::from("overview"))])).await;
    let from_graph = overview["freshness"]["topology_fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("the graph stamps a topology: {overview}"))
        .to_owned();

    assert_eq!(
        from_resident, from_graph,
        "one workspace, one topology — resident said {from_resident}, graph said {from_graph}",
    );
    // Sixteen hex digits, not a JSON number: a u64 loses precision above 2^53 in a JS
    // consumer, and a silently rounded fingerprint compares equal when it should not.
    assert_eq!(from_resident.len(), 16, "{from_resident}");
    assert!(from_resident.chars().all(|c| c.is_ascii_hexdigit()), "{from_resident}");
    assert_ne!(
        from_resident, "0000000000000000",
        "an all-zero fingerprint would make the equality above hold for any two subsystems",
    );

    // Sensitivity control: the SAME workspace directory, before and after it declares an
    // extension. Comparing two different temporary directories would not test this at all —
    // the fingerprint mixes the configuration's own path, so any two scratch dirs differ
    // whatever their root sets, and the assertion would hold even with extensions removed
    // from the hash entirely.
    // Inside this test's own TempDir, not beside it: `ws.path().parent()` is the shared
    // system temp directory, so a fixed name there survives the run and collides with any
    // other process running the same test.
    let ext = ws.path().join("declared-ext");
    copy_fixture_into(&ext);
    std::fs::write(
        ws.path().join("bsl-analyzer.toml"),
        format!("[source]\nroot = \".\"\nextensions = [{{ name = \"a\", path = {ext:?}, scope = \"InfoBase\" }}]\n"),
    )
    .expect("declare an extension in the same workspace");

    let redeclared = workspace_client(ws.path()).await;
    let after = poll(
        &redeclared,
        "symbol_info",
        args(&[("symbol", Value::from("ПервыйОбщийМодуль.НеУстаревшаяПроцедура"))]),
    )
    .await;
    let after_fingerprint = after["freshness"]["topology_fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("the resident stamps a topology: {after}"));
    assert_ne!(
        after_fingerprint, from_resident,
        "one directory, one extension declared: the declared topology moved, so the fingerprint must",
    );

    // Both envelopes name who answered, so a consumer never has to guess which subsystem's
    // freshness it is holding.
    assert_eq!(card["freshness"]["source"], "resident");
    assert_eq!(overview["freshness"]["source"], "graph");
}

/// The module both roots spell the same way. Two roots holding one relative path is the
/// shape the pair exists for: a key that forgets its root still lands on a real file here,
/// so a wrong answer arrives looking exactly like a right one.
const SHARED_MODULE_REL: &str = "CommonModules/Общий/Ext/Module.bsl";
const CONFIGURATION_SYMBOL: &str = "ФункцияКонфигурации";
const EXTENSION_SYMBOL: &str = "ФункцияРасширения";

/// A method the fixture's configuration declares, in a module only the configuration holds.
const STAND_METHOD: &str = "ПервыйОбщийМодуль.НеУстаревшаяФункция";

/// A configuration lists its common modules by name, and a module missing from that list is
/// invisible to the metadata however real its files are. Writing the descriptor without this
/// leaves the stand asking about a name nothing declares.
fn register_common_module(root: &Path, name: &str) {
    let configuration = root.join("Configuration.xml");
    let text = std::fs::read_to_string(&configuration).expect("read Configuration.xml");
    let anchor = "<CommonModule>";
    let at = text.find(anchor).expect("the fixture lists common modules");
    let mut listed = text.clone();
    listed.insert_str(at, &format!("<CommonModule>{name}</CommonModule>\n\t\t\t"));
    std::fs::write(&configuration, listed).expect("register the module");
}

fn write_common_module(root: &Path, name: &str, body: &str) {
    register_common_module(root, name);
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" xmlns:v8="http://v8.1c.ru/8.1/data/core">
	<CommonModule uuid="00000000-0000-0000-0000-0000000000{id:02}">
		<Properties>
			<Name>{name}</Name>
			<Global>false</Global>
			<ClientManagedApplication>false</ClientManagedApplication>
			<Server>true</Server>
			<ExternalConnection>false</ExternalConnection>
			<ClientOrdinaryApplication>false</ClientOrdinaryApplication>
			<ServerCall>false</ServerCall>
			<Privileged>false</Privileged>
			<ReturnValuesReuse>DontUse</ReturnValuesReuse>
		</Properties>
	</CommonModule>
</MetaDataObject>"#,
        id = name.len(),
    );
    std::fs::create_dir_all(root.join("CommonModules")).expect("mkdir CommonModules");
    std::fs::write(root.join(format!("CommonModules/{name}.xml")), xml).expect("write descriptor");
    let dir = root.join("CommonModules").join(name).join("Ext");
    std::fs::create_dir_all(&dir).expect("mkdir module");
    std::fs::write(dir.join("Module.bsl"), body).expect("write module");
}

/// The configuration and an extension declared beside it, both holding a module at
/// [`SHARED_MODULE_REL`].
///
/// Load-bearing parts of the shape:
///
/// - the extension lives OUTSIDE the workspace directory, because a root that canonically
///   lies inside the configuration is rejected rather than registered, and a stand built
///   that way would measure the rejection;
/// - the two modules differ in text and declare differently named functions, so an answer
///   derived from either file can be told from the other one;
/// - both call the same configuration method, so the reference walk has one declaration and
///   two occurrences whose paths collide.
fn stage_two_roots_sharing_a_path() -> (TempDir, TempDir) {
    let ws = stage_workspace();
    let ext = TempDir::new().expect("scratch extension");
    // A declared extension is recognised by its `Configuration.xml`; without one the
    // topology refuses the declaration outright.
    std::fs::copy(ws.path().join("Configuration.xml"), ext.path().join("Configuration.xml"))
        .expect("the extension needs a configuration file to be one");

    write_common_module(
        ws.path(),
        "Общий",
        &format!(
            "&НаСервере\nФункция {CONFIGURATION_SYMBOL}() Экспорт\n    \
             Возврат ПервыйОбщийМодуль.НеУстаревшаяФункция();\nКонецФункции\n"
        ),
    );
    write_common_module(
        ext.path(),
        "Общий",
        &format!(
            "&НаСервере\nФункция {EXTENSION_SYMBOL}() Экспорт\n    \
             Возврат ПервыйОбщийМодуль.НеУстаревшаяФункция();\n    \
             // Отличающийся текст: одинаковые байты сделали бы два файла неразличимыми\n\
             КонецФункции\n"
        ),
    );

    let path = ext.path();
    std::fs::write(
        ws.path().join("bsl-analyzer.toml"),
        format!("[source]\nroot = \".\"\nextensions = [{{ name = \"расш\", path = {path:?}, scope = \"InfoBase\" }}]\n"),
    )
    .expect("declare the extension");
    (ws, ext)
}

/// A workspace server with the opt-in `references` tool enabled, as `--enable-tool` does.
async fn client_with_references(root: &Path) -> Client {
    let state = SharedState::workspace(root.to_path_buf()).expect("valid workspace project");
    let gate = ToolGate::for_launch(McpProfile::Workspace, &["references".to_owned()]);
    let server = McpServer::with_gate(McpProfile::Workspace, state, &gate);
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    tokio::spawn(serve_stream(server, server_io));
    ().serve(client_io).await.expect("session initialized")
}

/// One relative path in two roots, carried across three tools.
///
/// Each tool already separates the roots inside its own suite. What none of them can show
/// is whether a pair one tool publishes still names the right file when another tool is
/// asked to serve it. Here both roots spell the same relative path, so a key that drops its
/// root does not fail loudly — it answers from the namesake, which is why the two modules
/// are given different text: identical bytes would make the wrong answer indistinguishable
/// from the right one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_relative_path_in_two_roots_addresses_two_files() {
    let (ws, _ext) = stage_two_roots_sharing_a_path();
    let client = client_with_references(ws.path()).await;

    // The declaration under test lives in the configuration alone, at a path no other root
    // spells. Its own place is the first pair this gate carries across tools.
    //
    // Its module is deliberately NOT one of the two namesake modules: a common module
    // present in both roots is an extension OVERRIDING it, and which of the two member sets
    // a card then shows is a merge question, not a root-separation one. Asking it here would
    // make this gate flake on a semantics it does not test.
    let card = poll(&client, "symbol_info", args(&[("symbol", Value::from(STAND_METHOD))])).await;
    assert_eq!(card["status"], "ok", "the stand must load before it can prove anything: {card}");
    let declaration = card["definitions"]
        .as_array()
        .and_then(|d| d.first())
        .and_then(|d| d.get("location"))
        .unwrap_or_else(|| panic!("the method card carries a location: {card}"));
    let declaration_root = declaration["root_id"].as_str().expect("root_id is a string").to_owned();
    let declaration_path = declaration["path"].as_str().expect("path is a string").to_owned();
    assert_eq!(declaration_root, "", "the declaration is the configuration's: {card}");
    assert_ne!(
        declaration_path, SHARED_MODULE_REL,
        "the declaration must sit OUTSIDE the shared path, or a key that drops its root \
         would still land on the right file and this gate would pass while blind",
    );

    // The reference walk sees the same collision: two occurrences of one declaration, in
    // two files whose relative paths are equal.
    let walk = poll(&client, "references", args(&[("symbol", Value::from(STAND_METHOD))])).await;
    assert_eq!(walk["outcome"], "resolved", "{walk}");
    let shared_buckets: Vec<String> = walk["files"]
        .as_array()
        .expect("a per-file histogram")
        .iter()
        .filter(|bucket| bucket["location"]["path"] == SHARED_MODULE_REL)
        .map(|bucket| bucket["location"]["root_id"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        shared_buckets.len(),
        2,
        "both namesake modules call the declaration, so the histogram holds two buckets \
         for one path: {walk}",
    );
    assert_ne!(shared_buckets[0], shared_buckets[1], "the two buckets name two roots: {walk}");

    // Each pair, handed to a third tool, is served by the root that named it. The handles
    // must differ: one relative path answered twice with one handle is the namesake being
    // served for both, which is the failure this whole stand exists to catch.
    let serve = |root_id: String, rel: String| {
        let client = &client;
        async move {
            let answer = poll(
                client,
                "diagnostics",
                args(&[
                    ("action", Value::from("file")),
                    ("root_id", Value::from(root_id)),
                    ("path", Value::from(rel)),
                ]),
            )
            .await;
            let result = &answer["result"];
            assert!(result.get("error").is_none(), "a published pair must be servable: {answer}");
            result["result_id"].as_str().expect("a result handle").to_owned()
        }
    };
    let served_configuration = serve(shared_buckets[0].clone(), SHARED_MODULE_REL.to_owned()).await;
    let served_extension = serve(shared_buckets[1].clone(), SHARED_MODULE_REL.to_owned()).await;
    assert_ne!(
        served_configuration, served_extension,
        "one path, two roots, two files — one handle for both means the root was dropped \
         from the key and the namesake was served",
    );

    // The pair `symbol_info` published is servable too, and names a third file: the gate
    // covers both producers of places, not the histogram alone.
    let served_declaration = serve(declaration_root, declaration_path).await;
    assert!(
        served_declaration != served_configuration && served_declaration != served_extension,
        "the declaration is its own file: {served_declaration}",
    );

    client.cancel().await.ok();
}

/// A place published on a graph EDGE is addressable, exactly as a place published on a
/// symbol is.
///
/// The edge is the point: `graph` answers from its own artefact with its own freshness,
/// while `diagnostics` answers from the resident. A pair that survives that crossing is
/// what makes the call site usable; one that does not is decoration attached to an edge.
///
/// The positive control is the same pair under an unregistered root — without it, "the file
/// was served" cannot be told from "this tool serves anything it is handed".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_site_published_on_an_edge_addresses_a_file_diagnostics_can_serve() {
    let ws = stage_workspace();
    write_common_module(
        ws.path(),
        "Вызыватель",
        &format!(
            "&НаСервере\nФункция Позвать() Экспорт\n    \
             Возврат {STAND_METHOD}();\nКонецФункции\n"
        ),
    );
    let client = workspace_client(ws.path()).await;

    let answer = poll(
        &client,
        "graph",
        args(&[
            ("action", Value::from("callers")),
            ("id", Value::from("method/common/ПервыйОбщийМодуль/НеУстаревшаяФункция")),
            ("call_sites", Value::from(true)),
        ]),
    )
    .await;

    let edges = answer["result"]["edges"].as_array().expect("edges is a list");
    let place = edges
        .iter()
        .find(|edge| edge["from"] == "method/common/Вызыватель/Позвать")
        .and_then(|edge| edge["call_sites"].as_array())
        .and_then(|places| places.first())
        .unwrap_or_else(|| panic!("the calling edge carries a place: {answer}"));

    assert_eq!(place["position_encoding"], "utf-16");
    assert_eq!(place["schema_version"], "1");
    let root_id = place["root_id"].as_str().expect("root_id is a string").to_owned();
    let path = place["path"].as_str().expect("path is a string").to_owned();
    assert!(path.contains("Вызыватель"), "the place addresses the CALLER's file: {place}");

    let diagnostics = poll(
        &client,
        "diagnostics",
        args(&[
            ("action", Value::from("file")),
            ("root_id", Value::from(root_id)),
            ("path", Value::from(path.clone())),
        ]),
    )
    .await;
    assert!(
        diagnostics["result"].get("error").is_none(),
        "a pair from a graph edge must address a file diagnostics can serve, got {}",
        diagnostics["result"],
    );

    let foreign = poll(
        &client,
        "diagnostics",
        args(&[
            ("action", Value::from("file")),
            ("root_id", Value::from("no-such-root")),
            ("path", Value::from(path)),
        ]),
    )
    .await;
    assert_eq!(
        foreign["result"]["error"], "unknown_root",
        "an unregistered root is an honest refusal, not another file: {foreign}",
    );
}
