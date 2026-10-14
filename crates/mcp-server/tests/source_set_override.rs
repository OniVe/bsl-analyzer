//! The source set given on the command line must reach backend identity — and the boot
//! must compose the project under it.
//!
//! Its own test binary on purpose: the override is process state installed once,
//! so "before" and "after" cannot both be observed from a suite that shares a
//! process with other tests. That is also why both gates live in ONE test: the
//! second install would be refused, and whether it was attempted at all would
//! depend on which test ran first.

use std::path::{Path, PathBuf};

use mcp_server::broker::workspace_topology_fingerprint;
use mcp_server::project::set_source_set_override;
use mcp_server::SharedState;
use project_model::{ExtensionDecl, SourceSetOverride, StructuredExtensionDecl};

fn configuration(root: &Path, rel: &str) {
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Configuration.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses">
	<Configuration uuid="11111111-0000-0000-0000-000000000001">
		<Properties><Name>C</Name></Properties><ChildObjects/>
	</Configuration>
</MetaDataObject>"#,
    )
    .unwrap();
}

/// Canonical on both sides, or a Windows temp path compared against its `\\?\` spelling
/// would fail this test for a reason that has nothing to do with source sets.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[test]
fn a_cli_source_set_changes_the_backend_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A layout the auto-discovery walk cannot reach: `Project::new` searches two levels
    // down for `Configuration.xml`, and the configuration here sits three (`a/b/main`).
    // The workspace therefore has no discoverable configuration at all, so what the flags
    // name is the ONLY thing that can become its source root.
    configuration(root, "a/b/main");
    configuration(root, "a/b/ext");

    // Read before installing anything: this is the identity a daemon launched
    // without flags would key on.
    let without_flags = workspace_topology_fingerprint(root);

    assert!(
        set_source_set_override(SourceSetOverride {
            configuration_root: Some("a/b/main".to_owned()),
            extensions: Some(vec![ExtensionDecl::Structured(StructuredExtensionDecl {
                name: "EXT".to_owned(),
                path: "a/b/ext".to_owned(),
                depends_on: Vec::new(),
                scope: Some(bsl_metadata::ExtensionScope::InfoBase),
            })]),
            externals: None,
        }),
        "the override must install on a fresh process"
    );

    let with_flags = workspace_topology_fingerprint(root);

    // Equal fingerprints would mean two clients with different source sets
    // rendezvous on one daemon, whose caches were built for the other set.
    assert_ne!(
        without_flags, with_flags,
        "a source set given on the command line must fork a separate backend"
    );

    // Everything in the crate that re-derives the project goes through this
    // helper, so the roots it reports are the roots the graph scan, the search
    // index and the resident host will walk.
    let roots = mcp_server::project::at(root).unwrap().source_roots();
    assert!(
        roots.contains(&root.join("a/b/ext")),
        "the declared extension must be among the roots this process analyzes: {roots:?}"
    );
    assert!(
        roots.contains(&root.join("a/b/main")),
        "the declared main configuration must be among them too: {roots:?}"
    );

    // The BOOT composes a project of its own (`SharedState::workspace`, and twice inside
    // `workspace_with_cache_and_prefixes`), and it must reach the same project the flags
    // name. `Project::new` there would find no configuration at this depth, and the server
    // would serve the workspace directory (or nothing) as its source root while the daemon
    // it lives in was keyed on the flags: the identity forks and the served roots do not,
    // which is worse than either alone.
    let state = SharedState::workspace(root.to_path_buf()).expect("valid workspace project");
    // One assertion for the whole property: a `None` here (no source root at all), the
    // workspace directory, or any other path all mean the boot composed its project without
    // the flags — the exact divergence this gate exists for.
    assert_eq!(
        state.source_root().map(|path| canonical(path)),
        Some(canonical(&root.join("a/b/main"))),
        "the boot must serve the configuration the FLAGS name, not a path its own walk found",
    );
    assert_eq!(
        canonical(state.workspace_root().expect("a workspace boot knows its workspace")),
        canonical(root),
        "and the workspace itself is still the directory it was launched on",
    );
    state.shutdown();
}
