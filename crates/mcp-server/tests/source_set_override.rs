//! The source set given on the command line must reach backend identity.
//!
//! Its own test binary on purpose: the override is process state installed once,
//! so "before" and "after" cannot both be observed from a suite that shares a
//! process with other tests.

use std::path::Path;

use mcp_server::broker::workspace_topology_fingerprint;
use mcp_server::project::set_source_set_override;
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

#[test]
fn a_cli_source_set_changes_the_backend_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    configuration(root, "cf");
    configuration(root, "ext");

    // Read before installing anything: this is the identity a daemon launched
    // without flags would key on.
    let without_flags = workspace_topology_fingerprint(root);

    assert!(
        set_source_set_override(SourceSetOverride {
            configuration_root: Some("cf".to_owned()),
            extensions: Some(vec![ExtensionDecl::Structured(StructuredExtensionDecl {
                name: "EXT".to_owned(),
                path: "ext".to_owned(),
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
        roots.contains(&root.join("ext")),
        "the declared extension must be among the roots this process analyzes: {roots:?}"
    );
    assert!(
        roots.contains(&root.join("cf")),
        "the declared main configuration must be among them too: {roots:?}"
    );
}
