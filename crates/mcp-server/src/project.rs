//! The one place this crate turns a workspace path into a [`Project`].
//!
//! A source set given on the command line is not on disk, so a call that only
//! receives a root path cannot see it. That matters here more than elsewhere:
//! the crate re-derives the project from a bare path in a dozen places — graph
//! scans, freshness checks, drift identity, the resident diagnostics host, the
//! broker key — and none of them can reach `SharedState`. Threading a spec
//! through every one of them would mean widening a dozen signatures whose
//! callers do not know about source sets either.
//!
//! Instead the override is process state, set once from argv before anything
//! builds a project, and consulted here. A server process serves exactly one
//! workspace with exactly one source set for its whole life, so there is nothing
//! for a second value to belong to. The scope stays inside this crate:
//! `project-model` gains no global, and `Project::new` keeps meaning what it
//! says everywhere else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock, PoisonError, RwLock};

use project_model::{Project, ProjectConfig, ProjectError, SourceSetOverride};

static OVERRIDE: OnceLock<SourceSetOverride> = OnceLock::new();

/// The root table each workspace's search engine serves, by the workspace's canonical spelling.
static SERVED_ROOTS: LazyLock<RwLock<HashMap<PathBuf, bsl_search::WorkspaceRoots>>> =
    LazyLock::new(RwLock::default);

/// Installs the source set this process was launched with.
///
/// Must run before anything builds a project — in particular before the broker
/// computes its backend key, which happens before a server exists at all.
/// Returns `false` if a source set was already installed, which only happens if
/// a caller violates that ordering; the first value wins, because a project
/// already built under it cannot be un-built.
pub fn set_source_set_override(source_set: SourceSetOverride) -> bool {
    OVERRIDE.set(source_set).is_ok()
}

/// The source set installed for this process, if any.
pub fn source_set_override() -> Option<&'static SourceSetOverride> {
    OVERRIDE.get()
}

/// Builds the project at `root` under this process's source set.
pub fn at(root: &Path) -> Result<Project, ProjectError> {
    let mut config = ProjectConfig::load(root)?.unwrap_or_default();
    if let Some(source_set) = OVERRIDE.get() {
        source_set.apply_to(&mut config);
    }
    Project::with_config(root, config)
}

/// The root table for one project: the configuration root plus every declared extension,
/// identified relative to the PROJECT directory — the identity [`bsl_search::WorkspaceRoots`]
/// documents and the one stored rows carry across restarts.
///
/// It lives beside [`at`] for the same reason [`at`] does: more than one subsystem needs the
/// table, and two of them — the search engine and the diagnostics resident — must agree on it
/// exactly. Agreement that follows from calling one constructor cannot drift; agreement between
/// two constructors is a claim someone has to keep true.
///
/// The ids are the ones the workspace's search engine serves ([`record_served_roots`]): a root
/// declared through a link keeps its id when the link is retargeted
/// ([`bsl_search::WorkspaceRoots::inheriting_root_ids`]), while a fresh build would mint one from
/// the new target. Search hands its ids out in every hit, and a caller passes them on to any other
/// tool, so a table that took the fresh ids would call the id search just returned unknown. Before
/// the engine serves anything there is nothing to inherit from, and the fresh build is the table.
///
/// Rejected roots come back rather than being logged here. Whether a dropped root is worth a
/// line depends on how often the caller runs: the boot builds this once, while the resident
/// rebuilds on every config drift, and a warning repeated per rebuild buries the one that is
/// actually new. Callers that run once say it with [`warn_about_rejected_roots`].
///
/// **No structural test forbids a second hand-built root set anywhere in this crate, and
/// that is a decision.** Such a test can only scan source text, and telling a call from a
/// mention — or from a longer identifier ending in the same name — is Rust's lexical
/// grammar, which a text scan approximates and never matches. The accessor used as a
/// function value, a comment standing between the name and its parenthesis, a parenthetical
/// aside in prose: each falls on a different side of whatever line the scan draws, and
/// moving the line to admit one form expels another. What guards each root set instead is
/// behavioural, and each set has its own: this table has a boot test, so has the graph's scan
/// universe, and the hub's declared targets are read back on a second boot over a matching
/// graph cache — the one boot path that leaves them alone (see `state::bootstrap`).
pub fn workspace_roots(
    project: &Project,
    excluded: &[std::path::PathBuf],
) -> (bsl_search::WorkspaceRoots, Vec<bsl_search::RejectedRoot>) {
    // External objects register like extensions: their own root id keeps their
    // files apart from a same-named object inside the configuration.
    let extensions: Vec<std::path::PathBuf> = project
        .extension_paths()
        .iter()
        .chain(project.external_paths())
        .map(|(_, path)| path.clone())
        .collect();
    let (roots, rejected) = bsl_search::WorkspaceRoots::build_optional(
        &project.root,
        project.semantic_base_path(),
        &extensions,
    );
    let fresh =
        roots.with_excluded(excluded.to_vec()).with_user_excluded(project.source_exclusions());
    let served = SERVED_ROOTS.read().unwrap_or_else(PoisonError::into_inner);
    let roots = match served.get(fresh.workspace_canonical()) {
        Some(served) => served.inheriting_root_ids(&fresh),
        None => fresh,
    };
    (roots, rejected)
}

/// Records the root table the workspace's search engine serves from now on, so that every later
/// [`workspace_roots`] of that workspace is stated under its ids.
///
/// Called wherever the engine's table changes — its boot and every applied root transition —
/// and nowhere else: the recorded table is a statement about the engine, not a cache of builds.
pub fn record_served_roots(roots: &bsl_search::WorkspaceRoots) {
    SERVED_ROOTS
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(roots.workspace_canonical().to_path_buf(), roots.clone());
}

/// Name every root that did not make it into the table, with its reason: a silently dropped
/// root looks exactly like a tree nobody edited.
pub fn warn_about_rejected_roots(rejected: &[bsl_search::RejectedRoot]) {
    for rejection in rejected {
        tracing::warn!(
            path = ?rejection.path,
            reason = ?rejection.reason,
            "extension root is not registered in the search index",
        );
    }
}
