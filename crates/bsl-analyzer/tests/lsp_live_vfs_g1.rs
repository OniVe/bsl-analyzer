mod common;

use common::Lsp;
use serde_json::json;

#[test]
fn g1_framed_semantics_survive_watched_changes_and_keep_open_buffer_authoritative() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let caller = src.join("Caller.bsl");
    let caller_text = "Процедура TargetProcedure()\nКонецПроцедуры\n\nПроцедура Caller()\n    TargetProcedure();\nКонецПроцедуры\n";
    std::fs::write(&caller, caller_text).unwrap();
    std::fs::write(root.path().join("bsl-analyzer.toml"), "[source]\nroot = \"src\"\n").unwrap();

    let mut lsp = Lsp::start(root.path());
    lsp.open(&caller, caller_text);
    let caller_uri = lsp_types::Url::from_file_path(&caller).unwrap();
    let response = lsp.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": caller_uri.clone()},
            "position": {"line": 4, "character": 8}
        }),
    );

    assert!(response["error"].is_null(), "definition request failed: {response}");
    let result = &response["result"];
    let location = if result.is_array() { &result[0] } else { result };
    assert_eq!(location["uri"], caller_uri.as_str(), "{response}");
    assert_eq!(location["range"]["start"]["line"], 0, "{response}");

    let hover = lsp.request(
        3,
        "textDocument/hover",
        json!({
            "textDocument": {"uri": caller_uri.clone()},
            "position": {"line": 4, "character": 8}
        }),
    );
    assert!(hover["error"].is_null(), "hover failed: {hover}");
    assert!(!hover["result"].is_null(), "hover must return semantic content: {hover}");

    let added = src.join("Added.bsl");
    let added_text = "Процедура AddedTarget()\nКонецПроцедуры\n\nПроцедура AddedCaller()\n    AddedTarget();\nКонецПроцедуры\n";
    std::fs::write(&added, added_text).unwrap();
    let added_uri = lsp_types::Url::from_file_path(&added).unwrap();
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
        "params": {"changes": [{"uri": added_uri.clone(), "type": 1}]}
    }));
    lsp.open(&added, added_text);
    let added_definition = lsp.request(
        4,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": added_uri.clone()},
            "position": {"line": 4, "character": 8}
        }),
    );
    assert!(
        added_definition["error"].is_null(),
        "added-file definition failed: {added_definition}"
    );
    let added_result = &added_definition["result"];
    let added_location = if added_result.is_array() { &added_result[0] } else { added_result };
    assert_eq!(added_location["uri"], added_uri.as_str(), "{added_definition}");
    assert_eq!(added_location["range"]["start"]["line"], 0, "{added_definition}");

    let changed_disk = "Процедура DiskReplacement()\nКонецПроцедуры\n";
    std::fs::write(&added, changed_disk).unwrap();
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
        "params": {"changes": [{"uri": added_uri.clone(), "type": 2}]}
    }));
    let buffer_definition = lsp.request(
        5,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": added_uri.clone()},
            "position": {"line": 4, "character": 8}
        }),
    );
    assert!(
        buffer_definition["error"].is_null(),
        "open buffer definition failed: {buffer_definition}"
    );
    let buffer_result = &buffer_definition["result"];
    let buffer_location = if buffer_result.is_array() { &buffer_result[0] } else { buffer_result };
    assert_eq!(
        buffer_location, added_location,
        "disk watcher must not replace the open editor buffer"
    );

    lsp.send(json!({
        "jsonrpc": "2.0", "method": "textDocument/didClose",
        "params": {"textDocument": {"uri": added_uri.clone()}}
    }));
    std::fs::remove_file(&added).unwrap();
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "workspace/didChangeWatchedFiles",
        "params": {"changes": [{"uri": added_uri, "type": 3}]}
    }));

    let status = lsp.shutdown(6);
    assert!(status.success(), "LSP subprocess exited with {status}");
}

#[test]
fn g1_framed_initial_config_failure_keeps_semantic_readiness_closed() {
    let root = tempfile::tempdir().unwrap();
    let caller = root.path().join("Caller.bsl");
    let caller_text = "Процедура TargetProcedure()\nКонецПроцедуры\n\nПроцедура Caller()\n    TargetProcedure();\nКонецПроцедуры\n";
    std::fs::write(&caller, caller_text).unwrap();
    std::fs::write(root.path().join("bsl-analyzer.toml"), "[source\nroot = \"src\"\n").unwrap();

    let mut lsp = Lsp::start(root.path());
    let caller_uri = lsp_types::Url::from_file_path(&caller).unwrap();
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": {"textDocument": {
            "uri": caller_uri.clone(), "languageId": "bsl", "version": 1, "text": caller_text
        }}
    }));
    for (id, method) in [(2, "textDocument/hover"), (3, "textDocument/definition")] {
        let response = lsp.request(
            id,
            method,
            json!({
                "textDocument": {"uri": caller_uri.clone()},
                "position": {"line": 4, "character": 8}
            }),
        );
        assert_eq!(
            response["error"]["code"],
            json!(lsp_server::ErrorCode::ContentModified as i32),
            "a rejected initial project must never return fake semantic success: {response}"
        );
        assert!(response["result"].is_null(), "failed-load response has no result: {response}");
    }
    let status = lsp.shutdown(4);
    assert!(status.success(), "LSP subprocess exited with {status}");
}
