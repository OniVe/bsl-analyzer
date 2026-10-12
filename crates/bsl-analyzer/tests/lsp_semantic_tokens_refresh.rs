//! `workspace/semanticTokens/refresh` after a change made to a module on disk while the
//! editor has another module open: a `git pull` or a configuration re-dump. The editor's
//! semanticTokens requests issued during the resulting reindex are declined or cancelled,
//! and it asks again only if the server tells it to — and the server may tell it only when
//! it advertised `workspace.semanticTokens.refreshSupport`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::json;

const REFRESH: &str = "workspace/semanticTokens/refresh";

fn is_refresh(message: &serde_json::Value) -> bool {
    message["method"] == REFRESH
}

fn refresh_capable() -> serde_json::Value {
    json!({"workspace": {"semanticTokens": {"refreshSupport": true}}})
}

/// A project of two modules, `Main.bsl` to open and `Other.bsl` to change on disk.
fn two_module_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("bsl-analyzer.toml"), "[source]\nroot = \"src\"\n").unwrap();
    std::fs::write(root.join("src/Main.bsl"), MAIN).unwrap();
    std::fs::write(root.join("src/Other.bsl"), "Процедура Другая()\r\nКонецПроцедуры\r\n").unwrap();
    (dir, root)
}

const MAIN: &str = "Процедура Основная()\r\nКонецПроцедуры\r\n";

fn rewrite_other(root: &std::path::Path, revision: u32) {
    std::fs::write(
        root.join("src/Other.bsl"),
        format!("Процедура Другая()\r\n    Перем Р{revision};\r\nКонецПроцедуры\r\n"),
    )
    .unwrap();
}

#[test]
fn an_external_module_change_asks_the_client_to_refresh_semantic_tokens() {
    let (_dir, root) = two_module_project();
    let mut lsp = Lsp::start_with_capabilities(&root, refresh_capable());

    // The refresh that ends the initial load is not the one under test: wait it out, so
    // that any refresh seen below was caused by the disk change.
    lsp.wait_for(is_refresh);

    lsp.open(&root.join("src/Main.bsl"), MAIN);

    // The file watcher is armed asynchronously and nothing announces when, so the change
    // is written again until the server reacts or the deadline passes.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut revision = 0;
    let refreshed = loop {
        revision += 1;
        rewrite_other(&root, revision);
        if lsp.wait_for_within(Duration::from_secs(2), is_refresh).is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
    };

    assert!(
        refreshed,
        "a module changed on disk while another one is open must make the server ask the \
         client for {REFRESH}; none arrived in 30 s of {revision} rewrites"
    );
}

/// A client that did not advertise `refreshSupport` is never sent the request: not when the
/// initial load ends and not after an external change, the two places that send it.
///
/// The external change is proven to have been handled, not merely written: its handler
/// re-publishes the open document's diagnostics, so two diagnostics for `Main.bsl`, each
/// following a rewrite of `Other.bsl`, mean the second came after the initial load (which
/// re-publishes once) through the very branch that would ask for the refresh.
#[test]
fn a_client_without_refresh_support_is_never_asked_to_refresh_semantic_tokens() {
    let (_dir, root) = two_module_project();
    let mut lsp = Lsp::start(&root);
    let main_uri = lsp_types::Url::from_file_path(root.join("src/Main.bsl")).unwrap();

    // Every message is looked at, from the first: `Lsp::open` would pass over a refresh that
    // arrived before the document's diagnostics.
    let refused = |message: &serde_json::Value| {
        assert!(
            !is_refresh(message),
            "{REFRESH} was sent to a client that did not advertise refreshSupport"
        );
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == main_uri.as_str()
    };
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": {"textDocument": {
            "uri": main_uri, "languageId": "bsl", "version": 1, "text": MAIN
        }}
    }));
    lsp.wait_for(refused);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut revision = 0;
    let mut republished = 0;
    while republished < 2 {
        assert!(
            Instant::now() < deadline,
            "the external change was not handled in 30 s of {revision} rewrites: \
             {republished} of 2 re-publications of the open document seen"
        );
        revision += 1;
        rewrite_other(&root, revision);
        if lsp.wait_for_within(Duration::from_secs(2), refused).is_some() {
            republished += 1;
        }
    }
}
