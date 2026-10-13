#![cfg(target_os = "linux")]

use std::io::{BufWriter, Write};
use std::process::Command;

use ide::diagnostics_baseline::{
    diagnostic_fingerprint, DiagnosticsBaselineEntry, DiagnosticsBaselineRange,
};
use ide::partitioned_diagnostics_baseline::{
    diagnostics_manifest, diagnostics_manifest_json, partition_object_path,
    DiagnosticsBaselineManifest,
};

const RECORDS: usize = 1_600_000;

#[test]
#[ignore = "release-only 1.6M-entry selected-update gate"]
fn large_selected_update_does_not_reserialize_unchanged_partitions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for source in ["src/cf", "src/cfe/Ext"] {
        std::fs::create_dir_all(root.join(source)).unwrap();
        std::fs::write(root.join(source).join("Configuration.xml"), "<Configuration/>").unwrap();
    }
    std::fs::write(root.join("src/cf/Main.bsl"), "Процедура П()\nКонецПроцедуры\n").unwrap();
    std::fs::write(root.join("src/cfe/Ext/Ext.bsl"), "Процедура П()\nКонецПроцедуры\n").unwrap();
    std::fs::write(
        root.join("bsl-analyzer.toml"),
        r#"[source]
root = "src/cf"
extensions = [{ name = "Ext", path = "src/cfe/Ext", scope = "InfoBase" }]
[diagnostics.baseline]
directory = "baselines"
"#,
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
            .current_dir(root)
            .args(args)
            .output()
            .unwrap()
    };
    let created = run(&["diagnostics", "baseline", "create", "-s", "."]);
    assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));

    let project = project_model::Project::new(root).unwrap();
    let plan = project.diagnostics_baseline_partition_plan().unwrap().unwrap();
    let extension =
        plan.partitions.iter().find(|partition| partition.id == "extension:Ext").unwrap();
    let baseline_root = root.join("baselines");
    let object_dir = baseline_root.join("objects/extensions").join(&extension.key);
    std::fs::create_dir_all(&object_dir).unwrap();
    let temporary = object_dir.join("large.tmp");
    let mut writer = BufWriter::new(std::fs::File::create(&temporary).unwrap());
    let mut hasher = blake3::Hasher::new();
    {
        let mut write = |bytes: &[u8]| {
            writer.write_all(bytes).unwrap();
            hasher.update(bytes);
        };
        write(br#"{"schema_version":2,"partition":"#);
        write(&serde_json::to_vec(&extension.identity).unwrap());
        write(br#","diagnostics":["#);
        for index in 0..RECORDS {
            if index > 0 {
                write(b",");
            }
            let path = format!("src/cfe/Ext/modules/{index}.bsl");
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
    let object = partition_object_path(&extension.id, &extension.key, &hash).unwrap();
    std::fs::rename(&temporary, baseline_root.join(&object)).unwrap();

    let mut manifest: DiagnosticsBaselineManifest =
        serde_json::from_slice(&std::fs::read(baseline_root.join("manifest.json")).unwrap())
            .unwrap();
    let entry =
        manifest.partitions.iter_mut().find(|entry| entry.partition_id == extension.id).unwrap();
    entry.file = object.clone();
    entry.blake3 = hash;
    manifest = diagnostics_manifest(plan.project_scope_fingerprint.clone(), manifest.partitions);
    std::fs::write(
        baseline_root.join("manifest.json"),
        diagnostics_manifest_json(&manifest).unwrap(),
    )
    .unwrap();
    let before = std::fs::metadata(baseline_root.join(&object)).unwrap();

    std::fs::write(root.join("src/cf/New.bsl"), "Процедура Тест(\n").unwrap();
    let updated = run(&["diagnostics", "baseline", "update", "-s", ".", "--partition", "main"]);
    assert!(updated.status.success(), "{}", String::from_utf8_lossy(&updated.stderr));
    let after_manifest: DiagnosticsBaselineManifest =
        serde_json::from_slice(&std::fs::read(baseline_root.join("manifest.json")).unwrap())
            .unwrap();
    let after_entry =
        after_manifest.partitions.iter().find(|entry| entry.partition_id == extension.id).unwrap();
    assert_eq!(after_entry.file, object);
    let after = std::fs::metadata(baseline_root.join(&after_entry.file)).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
}

#[test]
#[ignore = "release-only 1.6M-entry migration memory gate"]
fn large_v1_migration_streams_with_bounded_rss() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("src/cf")).unwrap();
    std::fs::write(root.join("src/cf/Configuration.xml"), "<Configuration/>").unwrap();
    std::fs::write(root.join("src/cf/Main.bsl"), "Процедура П()\nКонецПроцедуры\n").unwrap();
    std::fs::write(
        root.join("bsl-analyzer.toml"),
        r#"[source]
root = "src/cf"
[diagnostics.baseline]
directory = "baselines"
"#,
    )
    .unwrap();

    let legacy = root.join("legacy.json");
    let mut writer = BufWriter::new(std::fs::File::create(&legacy).unwrap());
    writer
        .write_all(
            br#"{"schema_version":1,"scope":{"source_root":"src/cf","extensions":[]},"diagnostics":["#,
        )
        .unwrap();
    // Zero-padded so the paths sort in index order: streaming migration cannot sort, so
    // it rejects a legacy set whose entries are out of canonical order, and unpadded
    // names put `modules/10.bsl` before `modules/2.bsl`.
    for index in 0..RECORDS {
        if index > 0 {
            writer.write_all(b",").unwrap();
        }
        let path = format!("src/cf/modules/{index:07}.bsl");
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
        serde_json::to_writer(&mut writer, &entry).unwrap();
    }
    writer.write_all(b"]}\n").unwrap();
    writer.flush().unwrap();
    let input_bytes = std::fs::metadata(&legacy).unwrap().len();

    let mut child = Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
        .current_dir(root)
        .args(["diagnostics", "baseline", "create", "-s", ".", "--from-v1", "legacy.json"])
        .spawn()
        .unwrap();
    // VmHWM, not sampled VmRSS: a peak shorter than the sampling interval — a partition
    // buffered whole before serialisation, say — is exactly the regression this gate
    // exists for, and polling would never see it. The kernel keeps the high-water mark,
    // so it is read on each turn and last before the process is reaped.
    let mut peak_rss = 0;
    let status = loop {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", child.id())) {
            if let Some(hwm) = status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")?.split_whitespace().next()?.parse::<u64>().ok()
            }) {
                peak_rss = peak_rss.max(hwm * 1024);
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success(), "migration child exited with {status}");
    assert!(peak_rss > 0, "migration process RSS was never observed");
    eprintln!("migration input={input_bytes} peak_rss={peak_rss}");
    assert!(
        peak_rss <= input_bytes * 3 / 2,
        "migration peak RSS {peak_rss} exceeds 1.5x input {input_bytes}"
    );
    assert!(root.join("baselines/manifest.json").is_file());
}
