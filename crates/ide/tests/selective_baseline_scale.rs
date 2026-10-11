#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::io::{BufWriter, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ide::diagnostics_baseline::{
    diagnostic_fingerprint, DiagnosticsBaselineEntry, DiagnosticsBaselineRange,
};
use ide::partitioned_diagnostics_baseline::{
    diagnostics_manifest, diagnostics_manifest_json, load_diagnostics_baseline_set_reusing,
    partition_object_path, DiagnosticsBaselineManifestEntry,
};
use project_model::{
    DiagnosticsBaselinePartition, DiagnosticsBaselinePartitionIdentity,
    DiagnosticsBaselinePartitionPlan, DiagnosticsBaselineProjectExtension,
    DiagnosticsBaselineProjectScope, DiagnosticsBaselineRootOwner, DiagnosticsBaselineSelection,
    ManagedBaselineDirectory,
};

const RECORDS: usize = 1_600_000;
const ENABLED_RECORDS: usize = RECORDS / 10;
const TEST_NAME: &str = "large_selective_baseline_load_skips_unsuppressed_objects";

fn plan(selection: DiagnosticsBaselineSelection) -> DiagnosticsBaselinePartitionPlan {
    let main = DiagnosticsBaselinePartition {
        id: "main".to_owned(),
        key: blake3::hash(b"main").to_hex().to_string(),
        identity: DiagnosticsBaselinePartitionIdentity::Main { path: String::new() },
    };
    let extension = DiagnosticsBaselinePartition {
        id: "extension:Ext".to_owned(),
        key: blake3::hash(b"extension:Ext").to_hex().to_string(),
        identity: DiagnosticsBaselinePartitionIdentity::Extension {
            name: "Ext".to_owned(),
            path: "ext".to_owned(),
            scope: Some(bsl_metadata::ExtensionScope::InfoBase),
            dependency_scopes: vec![],
            depends_on: vec![],
        },
    };
    DiagnosticsBaselinePartitionPlan {
        project_scope: DiagnosticsBaselineProjectScope {
            source_root: None,
            extensions: vec![DiagnosticsBaselineProjectExtension {
                name: "Ext".to_owned(),
                path: "ext".to_owned(),
                scope: Some(bsl_metadata::ExtensionScope::InfoBase),
                dependency_scopes: vec![],
                depends_on: vec![],
            }],
            externals: vec![],
        },
        project_scope_fingerprint: "a".repeat(64),
        selection_fingerprint: match selection {
            DiagnosticsBaselineSelection::All => "b".repeat(64),
            DiagnosticsBaselineSelection::Selective => "c".repeat(64),
        },
        partitions: vec![main, extension],
        enabled_partition_ids: match selection {
            DiagnosticsBaselineSelection::All => {
                vec!["main".to_owned(), "extension:Ext".to_owned()]
            }
            DiagnosticsBaselineSelection::Selective => vec!["main".to_owned()],
        },
        selection,
        roots: vec![
            DiagnosticsBaselineRootOwner {
                root: "ext".to_owned(),
                partition_id: "extension:Ext".to_owned(),
            },
            DiagnosticsBaselineRootOwner { root: String::new(), partition_id: "main".to_owned() },
        ],
    }
}

fn write_object(
    root: &std::path::Path,
    partition: &DiagnosticsBaselinePartition,
    records: std::ops::Range<usize>,
) -> (DiagnosticsBaselineManifestEntry, u64) {
    let temporary = root.join(format!("{}.tmp", partition.key));
    let mut writer = BufWriter::new(std::fs::File::create(&temporary).unwrap());
    let mut hasher = blake3::Hasher::new();
    {
        let mut write = |bytes: &[u8]| {
            writer.write_all(bytes).unwrap();
            hasher.update(bytes);
        };
        write(br#"{"schema_version":2,"partition":"#);
        write(&serde_json::to_vec(&partition.identity).unwrap());
        write(br#", "diagnostics":["#);
        for (position, index) in records.enumerate() {
            if position > 0 {
                write(b",");
            }
            let path = if partition.id == "main" {
                format!("main/{index}.bsl")
            } else {
                format!("ext/{index}.bsl")
            };
            let snippet = format!("Message({index});");
            let entry = DiagnosticsBaselineEntry {
                fingerprint: diagnostic_fingerprint(&path, "LineLength", &snippet, 0),
                path,
                code: "LineLength".to_owned(),
                snippet,
                occurrence: 0,
                message: "m".to_owned(),
                severity: "Warning".to_owned(),
                range: DiagnosticsBaselineRange {
                    start_line: 0,
                    start_column: 0,
                    end_line: 0,
                    end_column: 1,
                },
            };
            write(&serde_json::to_vec(&entry).unwrap());
        }
        write(b"]}");
    }
    writer.flush().unwrap();
    writer.get_ref().sync_all().unwrap();
    let hash = hasher.finalize().to_hex().to_string();
    let file = partition_object_path(&partition.id, &partition.key, &hash).unwrap();
    std::fs::create_dir_all(root.join(&file).parent().unwrap()).unwrap();
    std::fs::rename(&temporary, root.join(&file)).unwrap();
    let bytes = std::fs::metadata(root.join(&file)).unwrap().len();
    (
        DiagnosticsBaselineManifestEntry { partition_id: partition.id.clone(), file, blake3: hash },
        bytes,
    )
}

fn child_peak_rss(child: &mut Child) -> (std::process::ExitStatus, u64) {
    // VmHWM, not sampled VmRSS: the kernel keeps the true high-water mark, while polling
    // misses any peak shorter than the sampling interval — exactly the regression these
    // gates exist to catch (a partition buffered whole for a few milliseconds). Read
    // just before reaping, since /proc disappears with the process.
    let mut peak = 0;
    loop {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", child.id())) {
            if let Some(hwm) = status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")?.split_whitespace().next()?.parse::<u64>().ok()
            }) {
                peak = peak.max(hwm * 1024);
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            return (status, peak);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn run_child(mode: &str, root: &std::path::Path) -> u64 {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--ignored", "--exact"])
        .env("BSL_SELECTIVE_SCALE_MODE", mode)
        .env("BSL_SELECTIVE_SCALE_ROOT", root)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let (status, peak) = child_peak_rss(&mut child);
    assert!(status.success(), "{mode} scale child failed");
    assert!(peak > 0, "{mode} scale child RSS was never observed");
    peak
}

fn child_mode(mode: &str, root: &std::path::Path) {
    if mode == "idle" {
        std::thread::sleep(Duration::from_millis(150));
        return;
    }
    let directory =
        ManagedBaselineDirectory::open(root.parent().unwrap(), "baselines", false).unwrap();
    let active_plan = plan(if mode == "full" {
        DiagnosticsBaselineSelection::All
    } else {
        DiagnosticsBaselineSelection::Selective
    });
    let (snapshot, stats) =
        load_diagnostics_baseline_set_reusing(&directory, &active_plan, None, &BTreeSet::new())
            .unwrap();
    if mode == "full" {
        assert_eq!(stats.fingerprints_validated, RECORDS);
        assert_eq!(stats.objects_read.len(), 2);
    } else {
        assert_eq!(stats.fingerprints_validated, ENABLED_RECORDS);
        assert_eq!(stats.objects_read.len(), 1);
        assert_eq!(snapshot.partitions.len(), 1);
        let enabled = snapshot.partitions["main"].clone();
        let (reloaded, reused) = load_diagnostics_baseline_set_reusing(
            &directory,
            &active_plan,
            Some(&snapshot),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(reused.objects_read.len(), 0);
        assert!(std::sync::Arc::ptr_eq(&enabled, &reloaded.partitions["main"]));

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let dormant = manifest["partitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["partition_id"] == "extension:Ext")
            .unwrap()["file"]
            .as_str()
            .unwrap();
        std::fs::write(root.join(dormant), b"corrupt dormant object").unwrap();
        load_diagnostics_baseline_set_reusing(
            &directory,
            &active_plan,
            Some(&snapshot),
            &BTreeSet::new(),
        )
        .unwrap();
        // Measured while the resident snapshot is alive, so `black_box` below still sees
        // it; the independent load happens AFTER it is released, because that load parses
        // `main` in full before reaching the corrupted extension object and would
        // otherwise put two copies of the enabled partition in a child budgeted for one.
        std::hint::black_box(&snapshot);
        drop(snapshot);
        assert!(load_diagnostics_baseline_set_reusing(
            &directory,
            &plan(DiagnosticsBaselineSelection::All),
            None,
            &BTreeSet::new(),
        )
        .is_err());
    }
    std::thread::sleep(Duration::from_millis(150));
}

#[test]
#[ignore = "release-only paired 1.6M-entry selective load gate"]
fn large_selective_baseline_load_skips_unsuppressed_objects() {
    if let Ok(mode) = std::env::var("BSL_SELECTIVE_SCALE_MODE") {
        child_mode(
            &mode,
            &std::path::PathBuf::from(std::env::var_os("BSL_SELECTIVE_SCALE_ROOT").unwrap()),
        );
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("baselines");
    std::fs::create_dir_all(&root).unwrap();
    let full = plan(DiagnosticsBaselineSelection::All);
    let (main, enabled_bytes) = write_object(&root, &full.partitions[0], 0..ENABLED_RECORDS);
    let (extension, _) = write_object(&root, &full.partitions[1], ENABLED_RECORDS..RECORDS);
    let manifest =
        diagnostics_manifest(full.project_scope_fingerprint.clone(), vec![main, extension]);
    std::fs::write(root.join("manifest.json"), diagnostics_manifest_json(&manifest).unwrap())
        .unwrap();

    let idle = run_child("idle", &root);
    let full_added = run_child("full", &root).saturating_sub(idle);
    let selective_added = run_child("selective", &root).saturating_sub(idle);
    assert!(full_added > 0, "full load RSS growth was not observed");
    assert!(selective_added > 0, "selective load RSS growth was not observed");
    let absolute_limit = (128 * 1024 * 1024).max(enabled_bytes * 2);
    assert!(
        selective_added <= absolute_limit,
        "selective RSS growth {selective_added} exceeds {absolute_limit}"
    );
    assert!(
        selective_added <= full_added / 4,
        "selective RSS growth {selective_added} exceeds 25% of full {full_added}"
    );
}
