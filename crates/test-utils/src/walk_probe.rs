//! How far a reference walk got, read from the span `ide` already emits.
//!
//! `ide::references` opens a `find_references_in_file` span per file, so a cancellation
//! gate can learn that a walk has started — and how many files it entered — without a
//! `sleep` and without a counter in production code. The price is that the subscriber
//! and the counter are process-global: every test in a binary that walks references
//! adds to the same number. A test that reads it must therefore serialise against every
//! other test in its binary that can move it — a gate mutex owned by that test crate —
//! else [`await_walk_start`] returns on somebody else's file and the gate measures an
//! interleaving instead of a cancellation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const WALK_SPAN: &str = "find_references_in_file";

/// Files the walk has entered since the last [`reset`].
static WALKED: AtomicUsize = AtomicUsize::new(0);

struct WalkCounter;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WalkCounter {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attrs.metadata().name() == WALK_SPAN {
            WALKED.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Install the counting subscriber once for the whole test binary.
///
/// A global subscriber installed earlier by something else wins silently, and the
/// counter then never moves: a positive control that expects the full walk count is
/// what exposes that, so every gate on this counter must keep one.
pub fn install() {
    use tracing_subscriber::prelude::*;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::registry()
            .with(WalkCounter)
            .with(tracing_subscriber::filter::LevelFilter::DEBUG)
            .try_init();
    });
}

pub fn reset() {
    WALKED.store(0, Ordering::SeqCst);
}

pub fn entered() -> usize {
    WALKED.load(Ordering::SeqCst)
}

/// Block until the walk has entered its first file, so a cancel lands mid-flight. A
/// fixed sleep would race the build of whatever the walk reads first. The bound counts
/// polls, so a slow machine waits longer for the walk to start instead of failing on it.
pub fn await_walk_start() {
    for _ in 0..60_000 {
        if entered() > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the walk never started");
}
