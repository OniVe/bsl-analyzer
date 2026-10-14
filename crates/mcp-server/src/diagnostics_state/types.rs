/// Lifecycle status of the workspace diagnostics resident.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiagnosticsStatus {
    /// Not a workspace profile — diagnostics over files are unavailable.
    Disabled,
    /// A workspace is configured but the resident db has not been built yet (or was
    /// evicted); the next `diagnostics file` call triggers the build.
    Idle,
    /// Background build in progress.
    Loading,
    /// Ready to serve, with the resident `.bsl` file count.
    Ready { files: usize },
    /// Build failed.
    Failed(String),
}

/// State of an in-flight or last-attempted drift reload, surfaced so a failed reload
/// is visible rather than leaving the agent at `stale=true` forever.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReloadState {
    Idle,
    Running,
    Failed(String),
}

impl ReloadState {
    pub(super) fn label(&self) -> &'static str {
        match self {
            ReloadState::Idle => "none",
            ReloadState::Running => "running",
            ReloadState::Failed(_) => "failed",
        }
    }
}

/// Outcome of a resident read: the closure's result paired with the freshness verdict
/// computed under the SAME lock hold (so the envelope is atomic — `revision`/`stale`/
/// `reload` always describe the exact generation the result was read from), or why the
/// read could not run.
pub(crate) enum ResidentOutcome<R> {
    Ready(R, Freshness),
    /// Idle or loading — the agent should retry shortly.
    Loading,
    /// Reference profile.
    Disabled,
    Failed(String),
}

/// Freshness verdict for one diagnostics response, matching the graph envelope.
pub(crate) struct Freshness {
    pub revision: u64,
    pub stale: bool,
    pub reload: &'static str,
    /// Topology hash of the snapshot the answer was computed over, so a consumer can
    /// tell an answer from before an extension appeared from one after.
    pub topology: u64,
}

/// A snapshot of the resident lifecycle for the `status` action and the enriched
/// `loading` envelope — so an agent can tell "building, N ms in" from "stuck/failed"
/// instead of polling a flat `loading`.
#[derive(Clone, Debug)]
pub(crate) struct StatusReport {
    /// `disabled | idle | loading | ready | failed`.
    pub state: &'static str,
    pub generation: u64,
    /// SERVED resident `.bsl` count once `ready` — files held out as unreadable are
    /// not in it. See `unread_files` for those.
    pub files: Option<usize>,
    /// Workspace `.bsl` files that exist but could not be read, once `ready`.
    /// `Option`, like `files`: in a state with no resident there is no such count,
    /// and a flat `0` would read as "everything was read".
    pub unread_files: Option<usize>,
    /// Background reload state: `none | running | failed`.
    pub reload: &'static str,
    /// The failure message when `state == failed` (build panicked or errored), and the
    /// REASON the current `loading` attempt is being made when that attempt retries a
    /// failure. A retry that erased the cause would be indistinguishable from a first
    /// attempt for as long as it runs.
    pub error: Option<String>,
    /// Milliseconds since the current `loading` build started (`None` unless loading).
    pub elapsed_ms: Option<u64>,
    /// How long ago the last build failure was published (`None` when no failure is
    /// known) — the observable form of the state's `failed_since`. Kept while a retry of
    /// that failure runs, so a caller can still tell how stale the cause is.
    pub failed_age_ms: Option<u64>,
    /// Consecutive failed build attempts behind the current (or retried) failure. Zero
    /// once a build succeeds, and the number the retry backoff is scheduled from.
    pub attempts: u32,
    /// Milliseconds until the next attempt at a failed build is allowed. `Some` only
    /// while the state is `failed` (a build in flight owes no wait), so a client knows
    /// this failure is retryable and WHEN — `0` means the retry is due.
    pub retry_after_ms: Option<u64>,
    /// The workspace change-hub view, when this profile has one. Lets an agent tell
    /// event-driven freshness from a scan fallback.
    pub watch: Option<WatchReport>,
}

/// The change hub's contribution to the diagnostics status: whether drift is
/// served event-driven or via the scan fallback, its health, and how many raw
/// filesystem events it has observed.
#[derive(Clone, Debug)]
pub(crate) struct WatchReport {
    pub health: &'static str,
    pub events_seen: u64,
    /// `event-driven` while healthy, `scan-fallback` while degraded.
    pub mode: &'static str,
}
