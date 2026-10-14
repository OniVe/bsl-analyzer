//! What the server puts on the wire in answer to a real client's `initialize`.
//!
//! Every other session test drives the server with our own client from the same transport
//! library, so both ends agree on whatever the library currently does and a change in
//! negotiation is invisible: the two sides move together. This gate replays the exact
//! frames two shipping clients send — captured from live handshakes with the versions
//! named below — and pins the answer.
//!
//! Two of the frames are offers the server supports, and answering them is
//! indistinguishable from echoing them back: both revisions are known, so a server that
//! never consulted its own list would look identical on either. The third frame is what
//! separates the two — it offers a revision no build supports, where negotiation falls back
//! to the server's own and blind echo does not. Without it this gate would catch a server
//! pinned to a single revision and nothing else.

use mcp_server::{serve_stream, McpProfile, McpServer, SharedState};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// Shared helpers for the gates in this binary; not every binary uses every one.
#[allow(dead_code)]
mod common;

/// Capabilities are profile-independent and the reference profile needs no workspace, so
/// the handshake is answered without building anything.
fn server() -> McpServer {
    McpServer::new(McpProfile::Reference, SharedState::reference(None))
}

/// Sends one raw `initialize` frame and returns the parsed answer.
async fn negotiate(request: Value) -> Value {
    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
    tokio::spawn(serve_stream(server(), server_io));
    let (read_half, mut write) = tokio::io::split(client_io);
    let mut read = BufReader::new(read_half);
    write.write_all(format!("{request}\n").as_bytes()).await.expect("frame written");
    let mut line = String::new();
    tokio::time::timeout(common::STATE_CEILING, read.read_line(&mut line))
        .await
        .expect("the server answers a handshake")
        .expect("frame read");
    serde_json::from_str(&line).expect("the answer is JSON")
}

/// Everything a consumer reads to decide what this build can do. `instructions` is
/// deliberately excluded: it is prose meant to change.
fn negotiated_surface(answer: &Value) -> Value {
    json!({
        "protocolVersion": answer["result"]["protocolVersion"],
        "capabilities": answer["result"]["capabilities"],
        "serverInfo": answer["result"]["serverInfo"],
    })
}

/// Recorded from Claude Code 2.1.236.
fn claude_code_initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {"roots": {"listChanged": true}, "elicitation": {}},
            "clientInfo": {"name": "claude-code", "version": "2.1.236"}
        }
    })
}

/// An offer no build supports. Not recorded from anything: it exists to make the answer
/// differ between negotiation and echo, which neither recorded frame can do.
fn unsupported_version_initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "1999-01-01",
            "capabilities": {},
            "clientInfo": {"name": "probe", "version": "0"}
        }
    })
}

/// Recorded from codex 0.145.0.
fn codex_initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"elicitation": {"form": {}, "url": {}}},
            "clientInfo": {"name": "codex-mcp-client", "version": "0.145.0"}
        }
    })
}

/// The surface both clients read, pinned field by field.
///
/// `capabilities` is compared whole rather than probed key by key: a capability we never
/// meant to advertise — a protocol extension pulled in by a transport upgrade, say — is
/// exactly the kind of change that a `contains` check would wave through, and a client
/// opts into behaviour by reading this object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shipping_client_reads_the_surface_we_declare() {
    let expected_capabilities = json!({"resources": {}, "tools": {}});

    let claude = negotiate(claude_code_initialize()).await;
    assert_eq!(
        negotiated_surface(&claude),
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": expected_capabilities,
            "serverInfo": {"name": "bsl-analyzer", "version": env!("CARGO_PKG_VERSION")},
        }),
        "the surface offered to a client on 2025-11-25 changed: {claude}"
    );

    let codex = negotiate(codex_initialize()).await;
    assert_eq!(
        negotiated_surface(&codex),
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": expected_capabilities,
            "serverInfo": {"name": "bsl-analyzer", "version": env!("CARGO_PKG_VERSION")},
        }),
        "the surface offered to a client on 2025-06-18 changed: {codex}"
    );
}

/// A client is answered on the revision it asked for, and two clients asking differently
/// are answered differently.
///
/// Stated separately from the surface gate because it is the half that cannot pass by
/// accident: if negotiation ever collapses to one answer, the assertion below names which
/// direction it collapsed in, while the surface gate would only say a field changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negotiation_follows_the_client_not_the_server() {
    let claude = negotiate(claude_code_initialize()).await;
    let codex = negotiate(codex_initialize()).await;

    let claude_version = claude["result"]["protocolVersion"].clone();
    let codex_version = codex["result"]["protocolVersion"].clone();

    assert_ne!(
        claude_version, codex_version,
        "both clients were answered on the same revision, so the server is no longer \
         following the offer: {claude_version} for a 2025-11-25 client, {codex_version} \
         for a 2025-06-18 one"
    );
    assert_eq!(claude_version, json!("2025-11-25"));
    assert_eq!(codex_version, json!("2025-06-18"));

    // The half the recorded pair cannot cover: an offer outside the supported list must be
    // answered with the server's own revision. A server echoing offers back answers
    // `1999-01-01` here and passes every assertion above.
    let unsupported = negotiate(unsupported_version_initialize()).await;
    assert_eq!(
        unsupported["result"]["protocolVersion"],
        json!("2025-11-25"),
        "an unsupported offer was not answered with the server's own revision, so the \
         server is echoing offers instead of negotiating: {unsupported}"
    );
}
