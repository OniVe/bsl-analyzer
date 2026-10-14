//! The task branch of execution, end to end over the real transport.
//!
//! Spoken as raw JSON-RPC frames rather than through a client library, because no shipping
//! client declares the `io.modelcontextprotocol/tasks` extension yet: the only client that
//! can exercise this branch is one written here, and what the server puts on the wire is the
//! contract either way.
//!
//! Every gate below names the input on which it must fail. The two that matter most are the
//! pair in [`a_client_that_declared_nothing_reads_the_answer_it_always_read`]: without the
//! second half the first is green for a server that never opens a task at all, and without
//! the first the second is green for a server that answers every non-declaring caller with
//! an error.

use std::path::Path;
use std::time::Duration;

use mcp_server::{serve_stream, McpProfile, McpServer, SharedState, ToolGate};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// Shared gates helpers; each binary uses the subset it needs, so the rest is allowed to be unused.
#[allow(dead_code)]
mod common;
use common::Retired;

const TOOL: &str = "references";

/// The declaration every gate asks about. Declared by a module the fixture's configuration
/// actually lists, so it is visible to a caller at all.
const STAND_METHOD: &str = "ПервыйОбщийМодуль.НеУстаревшаяФункция";

/// The stand the cancellation gate needs: enough modules, each with enough bodies, that the
/// whole-config sweep over them takes long enough to tell a stopped read from an abandoned
/// one. Measured on this stand: the sweep runs 13 s while the resident build costs 0.7 s,
/// and a read issued while it runs waits 13.2 s for it. A lighter stand sweeps in under a
/// second, which is inside the noise of the answer that follows — the gate would then pass
/// on both builds.
const SWEEP_STAND_MODULES: usize = 800;
const SWEEP_STAND_BODIES: usize = 100;

/// How long the sweep is left to run before it is cancelled.
const SWEEP_DWELL: Duration = Duration::from_secs(2);

/// Bound on any wait for the server to answer or for an owner to leave. It is a hang guard:
/// every such wait ends on its own when the code is right, so the bound is only ever reached
/// by a server that never answers, and no assertion depends on how long the answer took.
const HANG_GUARD: Duration = Duration::from_secs(180);

/// Polls `tasks/get` at this interval, for at most [`HANG_GUARD`] worth of polls.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_ATTEMPTS: u32 = (HANG_GUARD.as_millis() / POLL_INTERVAL.as_millis()) as u32;

/// Enough modules that a cold resident is still building when the first call arrives. A
/// smaller stand lets the build win the race, and the gates that need an unready resident
/// would then measure the ready path instead.
const COLD_STAND_MODULES: usize = 200;

/// Every gate here exercises the branch, so the knob is on for the whole binary. Read per
/// call by the server, so setting it once before any server is built is enough.
fn arm_the_branch() {
    std::env::set_var("BSL_MCP_TASKS", "1");
}

fn stage(callers: usize, bodies: usize) -> TempDir {
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
    for index in 0..callers {
        let mut body = String::new();
        for nth in 0..bodies {
            body.push_str(&format!(
                "Процедура Вызвать{index:04}_{nth}() Экспорт\n    \
                 ПервыйОбщийМодуль.НеУстаревшаяФункция();\n    \
                 Знач = 1 + {nth};\nКонецПроцедуры\n"
            ));
        }
        write_module(dst.path(), &format!("Толпа{index:04}"), &body);
    }
    dst
}

fn write_module(root: &Path, name: &str, body: &str) {
    let dir = root.join("CommonModules").join(name).join("Ext");
    std::fs::create_dir_all(&dir).expect("mkdir module");
    std::fs::write(dir.join("Module.bsl"), body).expect("write module");
}

fn server_for(root: &Path) -> Retired {
    let state = SharedState::workspace(root.to_path_buf()).expect("valid workspace project");
    let gate = ToolGate::for_launch(McpProfile::Workspace, &[TOOL.to_owned()]);
    Retired(McpServer::with_gate(McpProfile::Workspace, state, &gate))
}

/// One client connection, spoken as frames.
struct Wire {
    write: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
    read: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
    next_id: i64,
}

impl Wire {
    /// A session on `server`, handshaken. `declares_tasks` is the whole difference between
    /// the two callers these gates compare.
    async fn connect(server: &McpServer, declares_tasks: bool) -> Self {
        let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
        tokio::spawn(serve_stream(server.clone(), server_io));
        let (read_half, write_half) = tokio::io::split(client_io);
        let mut wire = Self {
            write: Box::new(write_half),
            read: Box::new(BufReader::new(read_half)),
            next_id: 1,
        };

        let capabilities = if declares_tasks {
            json!({"extensions": {"io.modelcontextprotocol/tasks": {}}})
        } else {
            json!({})
        };
        let hello = wire
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": capabilities,
                    "clientInfo": {"name": "task-gate", "version": "0"}
                }),
            )
            .await;
        assert!(hello["result"]["serverInfo"].is_object(), "handshake answered: {hello}");
        wire.notify("notifications/initialized", json!({})).await;
        wire
    }

    async fn send(&mut self, value: Value) {
        self.write.write_all(format!("{value}\n").as_bytes()).await.expect("frame written");
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params})).await;
    }

    /// Send one request and read frames until its answer arrives.
    ///
    /// Bounded on purpose: the failure several of these gates exist to catch is a server
    /// that never answers, and an unbounded read would hang the run instead of reporting it.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await;
        loop {
            let mut line = String::new();
            tokio::time::timeout(HANG_GUARD, self.read.read_line(&mut line))
                .await
                .expect("the server must answer within the deadline")
                .expect("frame read");
            let frame: Value = serde_json::from_str(&line).expect("frame is JSON");
            if frame["id"] == json!(id) {
                return frame;
            }
        }
    }

    async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": arguments})).await
    }

    async fn references(&mut self, symbol: &str) -> Value {
        self.call(TOOL, json!({"symbol": symbol})).await
    }

    async fn get_task(&mut self, task_id: &str) -> Value {
        self.request("tasks/get", json!({"taskId": task_id})).await
    }

    async fn cancel_task(&mut self, task_id: &str) -> Value {
        self.request("tasks/cancel", json!({"taskId": task_id})).await
    }

    /// Poll until `settled` accepts a `tasks/get` answer, or give up loudly. The bound counts
    /// polls, so a slow machine waits longer for the same answer instead of failing on it.
    async fn poll_until(
        &mut self,
        task_id: &str,
        what: &str,
        settled: impl Fn(&Value) -> bool,
    ) -> Value {
        let mut last = Value::Null;
        for _ in 0..POLL_ATTEMPTS {
            let answer = self.get_task(task_id).await;
            let task = &answer["result"];
            if settled(task) {
                return task.clone();
            }
            last = task.clone();
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        panic!("the task never reached {what}; last seen: {last}");
    }
}

fn task_id(answer: &Value) -> String {
    assert_eq!(answer["result"]["resultType"], "task", "expected a task handle, got: {answer}");
    answer["result"]["taskId"].as_str().expect("a task carries an id").to_owned()
}

/// The branch is the caller's choice, and a caller that made no choice is untouched by it.
///
/// Both halves are required. The first alone is green for a build that never opens a task;
/// the second alone is green for a build that answers `-32021` to everyone who did not ask.
/// The two callers differ only in the capability they declared, and they share one server, so
/// nothing but that declaration can explain the difference.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_declared_nothing_reads_the_answer_it_always_read() {
    arm_the_branch();
    let dst = stage(COLD_STAND_MODULES, 1);
    let server = server_for(dst.path());

    let mut silent = Wire::connect(&server, false).await;
    let answer = silent.references(STAND_METHOD).await;
    assert!(
        answer["error"].is_null(),
        "a caller that declared no extension must not be answered with an error \
         (a `-32021` here means the branch was left to the dispatcher instead of taken \
         by the handler): {answer}"
    );
    assert_eq!(
        answer["result"]["structuredContent"]["status"], "loading",
        "the synchronous answer while the index builds is the envelope it has always been: \
         {answer}"
    );

    let mut declaring = Wire::connect(&server, true).await;
    let offered = declaring.references(STAND_METHOD).await;
    assert_eq!(
        offered["result"]["resultType"], "task",
        "a caller that declared the extension must be handed a handle instead of the \
         envelope: {offered}"
    );
}

/// The branch belongs to the resident, not to one tool that reads it.
///
/// Fails on a build that wires the handle into a single tool: the four others would answer
/// with their `loading` envelope, and each is named separately here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_tool_that_waits_on_the_resident_offers_the_handle() {
    arm_the_branch();
    let dst = stage(COLD_STAND_MODULES, 1);
    let server = server_for(dst.path());
    let mut wire = Wire::connect(&server, true).await;

    // Fired back to back, before the cold resident can become ready: each call has to meet
    // an unready resident, which is the only state this branch answers for.
    let calls = [
        ("metadata", json!({"action": "info"})),
        ("symbol_info", json!({"symbol": STAND_METHOD})),
        (TOOL, json!({"symbol": STAND_METHOD})),
        (
            "diagnostics",
            json!({"action": "file", "path": "CommonModules/Толпа0000/Ext/Module.bsl"}),
        ),
        ("diagnostics", json!({"action": "workspace"})),
    ];
    for (tool, arguments) in calls {
        let answer = wire.call(tool, arguments.clone()).await;
        assert_eq!(
            answer["result"]["resultType"], "task",
            "`{tool}` {arguments} answered without a handle while the resident was building; \
             if this stand became fast enough to be ready by now, raise COLD_STAND_MODULES \
             rather than dropping the tool from this list: {answer}"
        );
    }
}

/// A handle that waited hands back the answer, not a prettier way of saying "still building".
///
/// Fails on the implementation this branch is easiest to mistake for: one that wraps the
/// `loading` envelope in a task and calls it completed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_handle_carries_the_real_answer() {
    arm_the_branch();
    let dst = stage(COLD_STAND_MODULES, 1);
    let server = server_for(dst.path());
    let mut wire = Wire::connect(&server, true).await;

    let id = task_id(&wire.references(STAND_METHOD).await);
    let task = wire.poll_until(&id, "a terminal status", |task| task["status"] != "working").await;

    assert_eq!(task["status"], "completed", "the task must complete: {task}");
    let body = &task["result"]["structuredContent"];
    assert_ne!(
        body["status"], "loading",
        "a completed task carrying the retry envelope is the envelope with extra steps: {task}"
    );
    assert_eq!(
        body["outcome"], "resolved",
        "the body must be the answer the synchronous call would have given a ready \
         resident: {task}"
    );
}

/// The handle outlives the connection that opened it.
///
/// The second session issues no tool call at all — it only asks about the id — so an answer
/// arriving there can only come from work that was already running. A build that started the
/// work over on demand would have nothing to answer from.
///
/// The boundary is not tested here because it is not a code path: a handle lives as long as
/// the daemon process, and the contract says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handle_outlives_the_connection_that_opened_it() {
    arm_the_branch();
    let dst = stage(COLD_STAND_MODULES, 1);
    let server = server_for(dst.path());

    let mut opener = Wire::connect(&server, true).await;
    let opened = opener.references(STAND_METHOD).await;
    let id = task_id(&opened);
    let created_at = opened["result"]["createdAt"].clone();
    assert!(created_at.is_string(), "a handle is stamped when it is opened: {opened}");
    drop(opener);

    let mut later = Wire::connect(&server, true).await;
    let task = later.poll_until(&id, "a terminal status", |task| task["status"] != "working").await;
    assert_eq!(task["taskId"], id.as_str(), "the same handle, not a new one: {task}");
    assert_eq!(
        task["createdAt"], created_at,
        "a re-stamped handle means the work was started again rather than continued: {task}"
    );
    assert_eq!(task["status"], "completed", "the work continued to its answer: {task}");
    assert_eq!(
        task["result"]["structuredContent"]["outcome"], "resolved",
        "and the answer is the real one: {task}"
    );
}

/// Cancelling a task that is still waiting for the index settles it, with no worker to reach.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_waiting_handle_settles_it() {
    arm_the_branch();
    let dst = stage(COLD_STAND_MODULES, 1);
    let server = server_for(dst.path());
    let mut wire = Wire::connect(&server, true).await;

    let id = task_id(&wire.references(STAND_METHOD).await);
    // Cancelled while it is demonstrably still waiting on the index rather than reading it:
    // without this the gate could be cancelling a task that had already moved on, and the
    // waiting phase — where there is no worker to signal — would never be exercised.
    let waiting = wire
        .poll_until(&id, "the waiting phase", |task| {
            task["statusMessage"].as_str().is_some_and(|m| m.contains("waiting"))
        })
        .await;
    assert_eq!(waiting["status"], "working", "still running while it waits: {waiting}");

    let ack = wire.cancel_task(&id).await;
    assert!(ack["error"].is_null(), "cancellation is acknowledged: {ack}");
    let task = wire.poll_until(&id, "a terminal status", |task| task["status"] != "working").await;
    assert_eq!(task["status"], "cancelled", "a waiting task settles itself: {task}");
}

/// Cancelling a task that is reading the resident stops the read and releases the resident.
///
/// The release is the half that cannot be skipped. A build that drops the task's future
/// instead of cancelling it looks identical up to the status — `cancelled` either way — and
/// differs only here: the blocking read keeps running, and the next caller waits behind it.
///
/// How long that next caller waits is a speed, so the gate does not compare it with a fixed
/// bound. It compares it with the wait behind the same sweep left running on an identical stand,
/// taken under the same load: a released resident answers in a small fraction of the time the
/// sweep takes to finish, and an abandoned one answers only after the sweep has finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_working_handle_stops_the_read_and_frees_the_resident() {
    arm_the_branch();
    let live_stand = stage(SWEEP_STAND_MODULES, SWEEP_STAND_BODIES);
    let live = server_for(live_stand.path());
    let waited_behind_live = wait_behind_sweep(&live, false).await;

    let cancelled_stand = stage(SWEEP_STAND_MODULES, SWEEP_STAND_BODIES);
    let cancelled = server_for(cancelled_stand.path());
    let waited_behind_cancelled = wait_behind_sweep(&cancelled, true).await;

    assert!(
        waited_behind_cancelled * 3 < waited_behind_live,
        "the cancelled read still held the resident: the next caller waited \
         {waited_behind_cancelled:?} behind a cancelled sweep against {waited_behind_live:?} \
         behind a live one"
    );
}

/// Starts the whole-config sweep on `server`, waits until it is reading, then cancels it or
/// leaves it running, and returns how long the next caller waited for the resident.
async fn wait_behind_sweep(server: &McpServer, cancel: bool) -> Duration {
    let mut wire = Wire::connect(server, true).await;

    // The whole-config sweep, not a reference walk: it is the heaviest thing that runs under
    // the resident lock, and the difference this gate measures is only visible while that
    // lock is held. A walk over the same stand finishes in a few hundred milliseconds, which
    // is inside the noise of the answer that follows — a build that abandoned the read was
    // measured passing on it.
    let id = task_id(&wire.call("diagnostics", json!({"action": "workspace"})).await);
    // The injection point, fixed rather than timed: the task says on the wire when it stops
    // waiting and starts reading, and only then is there blocking work for a cancellation to
    // reach. Cancelling earlier would exercise the waiting phase instead — which is a
    // different gate — and pass on a build whose cancellation reaches no worker at all.
    wire.poll_until(&id, "the reading phase", |task| {
        task["statusMessage"].as_str().is_some_and(|m| m.contains("reading"))
    })
    .await;
    // The phase flips when the work is handed to a blocking thread, not when that thread
    // gets to run. Cancelling in that window stops work that had not started, which every
    // build survives; the dwell puts the sweep in flight — and holding the resident — before
    // anything is asked of the cancellation.
    tokio::time::sleep(SWEEP_DWELL).await;

    if cancel {
        wire.cancel_task(&id).await;
        let task =
            wire.poll_until(&id, "a terminal status", |task| task["status"] != "working").await;
        assert_eq!(task["status"], "cancelled", "the read was cancelled, not finished: {task}");
    }

    let started = std::time::Instant::now();
    let freed = wire.call("metadata", json!({"action": "info"})).await;
    let waited = started.elapsed();
    assert!(
        freed["error"].is_null() && freed["result"].is_object(),
        "the next caller gets a normal answer after {waited:?}: {freed}"
    );
    waited
}

/// A task that answered before the cancellation is not reported as cancelled.
///
/// Fails on a build that treats the arrival of `tasks/cancel` as the outcome rather than a
/// request the work may already have outrun.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handle_that_answered_first_is_not_reported_as_cancelled() {
    arm_the_branch();
    let dst = stage(COLD_STAND_MODULES, 1);
    let server = server_for(dst.path());
    let mut wire = Wire::connect(&server, true).await;

    let id = task_id(&wire.references(STAND_METHOD).await);
    let task = wire.poll_until(&id, "a terminal status", |task| task["status"] != "working").await;
    assert_eq!(task["status"], "completed", "the work finished on its own: {task}");

    wire.cancel_task(&id).await;
    let after = wire.get_task(&id).await;
    assert_eq!(
        after["result"]["status"], "completed",
        "a terminal task is not re-labelled by a late cancellation: {after}"
    );
}
