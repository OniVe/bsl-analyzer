mod common;

use ide::diagnostics_baseline::{
    diagnostic_fingerprint, DiagnosticsBaseline, DiagnosticsBaselineEntry, DiagnosticsBaselineRange,
};
use serde_json::{json, Value};

use std::time::Duration;

use common::*;

#[test]
fn parity() {
    let dir = project();
    let baseline: DiagnosticsBaseline =
        serde_json::from_slice(&std::fs::read(dir.path().join("baseline.json")).unwrap()).unwrap();
    assert!(!baseline.diagnostics.is_empty(), "CLI must create at least one known diagnostic");

    let mut lsp = Lsp::start(dir.path());
    let published = lsp.open(&dir.path().join("src/Main.bsl"), BROKEN);
    assert!(
        published["params"]["diagnostics"].as_array().unwrap().is_empty(),
        "LSP must suppress the same diagnostics the CLI recorded: {published}"
    );
}

#[test]
fn partial_document() {
    let dir = project();
    let baseline_path = dir.path().join("baseline.json");
    let mut baseline: DiagnosticsBaseline =
        serde_json::from_slice(&std::fs::read(&baseline_path).unwrap()).unwrap();
    baseline.diagnostics.push(DiagnosticsBaselineEntry {
        fingerprint: diagnostic_fingerprint("src/Other.bsl", "UnreachableCode", "Возврат;", 0),
        path: "src/Other.bsl".to_owned(),
        code: "UnreachableCode".to_owned(),
        snippet: "Возврат;".to_owned(),
        occurrence: 0,
        message: "resolved outside the open document".to_owned(),
        severity: "warning".to_owned(),
        range: DiagnosticsBaselineRange {
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 8,
        },
    });
    std::fs::write(
        &baseline_path,
        ide::diagnostics_baseline::diagnostics_baseline_json(&baseline).unwrap(),
    )
    .unwrap();

    let mut lsp = Lsp::start(dir.path());
    let published = lsp.open(&dir.path().join("src/Main.bsl"), BROKEN);
    assert!(published["params"]["uri"].as_str().unwrap().ends_with("Main.bsl"));
    assert!(published["params"]["diagnostics"].as_array().unwrap().is_empty());

    // The resolved entry belongs to another file, so no publication may carry it —
    // neither this one nor any that follows. Asserting only on the (already empty)
    // publication above could not tell the two apart.
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match lsp.messages.recv_timeout(Duration::from_millis(50)) {
            Ok(message) => assert!(
                !message.to_string().contains("resolved outside the open document"),
                "a resolved entry of another file must not be synthesized: {message}"
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[test]
fn legacy_extension_scope_warning_uses_lsp_channel_and_tracks_config_reload() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("src/cf");
    let extension = dir.path().join("ext");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(main.join("Configuration.xml"), "<Configuration/>").unwrap();
    std::fs::write(
        extension.join("Configuration.xml"),
        "<Properties><ConfigurationExtensionPurpose>Customization</ConfigurationExtensionPurpose></Properties>",
    )
    .unwrap();
    std::fs::write(main.join("Main.bsl"), "Процедура Проверка()\nКонецПроцедуры\n").unwrap();
    let config = dir.path().join("bsl-analyzer.toml");
    let write_config = |scope: Option<&str>| {
        let scope = scope.map_or_else(String::new, |scope| format!(", scope = \"{scope}\""));
        std::fs::write(
            &config,
            format!(
                "[source]\nroot = \"src/cf\"\nextensions = [{{ name = \"Feature\", path = \"ext\"{scope} }}]\n"
            ),
        )
        .unwrap();
    };
    write_config(None);

    // initialize and all following traffic pass through the real Content-Length LSP
    // transport. The harness retains notifications delivered before initialize's reply.
    let mut lsp = Lsp::start(dir.path());
    let warning = lsp.wait_for(|message| message["method"] == "window/showMessage");
    assert_eq!(warning["params"]["type"], 2, "warning severity: {warning}");
    let warning_text = warning["params"]["message"].as_str().unwrap();
    assert!(warning_text.contains("Feature") && warning_text.contains("ext"), "{warning}");
    assert!(
        warning_text.contains("InfoBase") && warning_text.contains("DataSeparation"),
        "the client gets the explicit-scope action and precision limit: {warning}"
    );

    let main_uri = lsp_types::Url::from_file_path(main.join("Main.bsl")).unwrap();
    let diagnostics = lsp.open(&main.join("Main.bsl"), "Процедура Проверка()\nКонецПроцедуры\n");
    assert_eq!(diagnostics["method"], "textDocument/publishDiagnostics");
    for id in [2, 3] {
        lsp.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
            "params": {"textDocument": {"uri": main_uri.clone()}, "position": {"line": 0, "character": 10}}
        }));
        let response = lsp.wait_for(|message| message["id"] == id);
        assert!(response.get("result").is_some(), "hover is answered: {response}");
    }
    assert!(
        lsp.wait_for_within(Duration::from_millis(250), |message| {
            message["method"] == "window/showMessage"
        })
        .is_none(),
        "ordinary document requests do not repeat the snapshot warning"
    );

    // didSave is the existing project-config reload path. The explicit snapshot loses
    // the warning; deleting the field later produces one new warning for that snapshot.
    write_config(Some("InfoBase"));
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "textDocument/didSave",
        "params": {"textDocument": {"uri": lsp_types::Url::from_file_path(&config).unwrap()}}
    }));
    // The save can cancel requests already running against the old snapshot. Retry
    // only that defined JSON-RPC cancellation; any other error remains a test failure.
    let mut response = Value::Null;
    for id in 4..=8 {
        lsp.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
            "params": {"textDocument": {"uri": main_uri.clone()}, "position": {"line": 0, "character": 10}}
        }));
        response = lsp.wait_for(|message| message["id"] == id);
        if response.get("result").is_some() {
            break;
        }
        assert_eq!(
            response["error"]["code"], -32800,
            "only a reload cancellation may be retried: {response}"
        );
    }
    assert!(response.get("result").is_some(), "explicit snapshot serves hover: {response}");
    assert!(
        lsp.wait_for_within(Duration::from_millis(250), |message| {
            message["method"] == "window/showMessage"
        })
        .is_none(),
        "the explicit snapshot removes the compatibility warning"
    );

    write_config(None);
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "textDocument/didSave",
        "params": {"textDocument": {"uri": lsp_types::Url::from_file_path(&config).unwrap()}}
    }));
    let warning = lsp.wait_for(|message| message["method"] == "window/showMessage");
    assert!(warning["params"]["message"].as_str().unwrap().contains("Feature"));
}

/// The control for the harness's own judgement: a wait on a server that is idle and
/// will never meet the condition fails on the FIRST silent window, and says why. A
/// wait that could only ever report "Timeout" after a minute could not tell this case
/// from a server still working, which is what every timeout in this family looked like.
#[test]
fn a_wait_an_idle_server_will_never_satisfy_is_judged_lost_not_timed_out() {
    let dir = project();
    let lsp = Lsp::start(dir.path());
    let silence = Duration::from_secs(2);

    let started = std::time::Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lsp.wait_for_judging(silence, |_| false);
    }));
    let waited = started.elapsed();

    let payload = outcome.expect_err("a condition no message meets must fail the wait");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(message.contains("used no CPU"), "the verdict names idleness: {message}");
    assert!(message.contains(file!()), "the verdict names the waiting line: {message}");
    assert!(
        waited < SILENCE,
        "an idle server is judged on its own silence window, not the default: {waited:?}",
    );
}
