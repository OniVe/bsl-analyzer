use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

const BROKEN: &str = "Процедура Тест(\n";

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("run bsl-analyzer")
}

fn json(root: &Path, args: &[&str]) -> serde_json::Value {
    let output = run(root, args);
    assert!(
        output.status.success() || !output.stdout.is_empty(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn setup(directory: bool) -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    for root in ["src/cf", "src/cfe/Ext"] {
        fs::create_dir_all(temp.path().join(root)).unwrap();
        fs::write(temp.path().join(root).join("Configuration.xml"), "<Configuration/>").unwrap();
    }
    fs::write(temp.path().join("src/cf/Main.bsl"), BROKEN).unwrap();
    fs::write(temp.path().join("src/cfe/Ext/Ext.bsl"), BROKEN).unwrap();
    let baseline = if directory { "directory = \"baselines\"" } else { "path = \"legacy.json\"" };
    fs::write(
        temp.path().join("bsl-analyzer.toml"),
        format!(
            r#"[source]
root = "src/cf"
extensions = [{{ name = "Ext", path = "src/cfe/Ext", scope = "InfoBase" }}]

[diagnostics.baseline]
{baseline}
"#,
        ),
    )
    .unwrap();
    temp
}

#[test]
fn operations_all_and_selected_are_atomic_and_scoped() {
    let temp = setup(true);
    let root = temp.path();
    let created = json(root, &["diagnostics", "baseline", "create", "-s", ".", "--format", "json"]);
    assert_eq!(created["operation"], "created");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("baselines/manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["partitions"].as_array().unwrap().len(), 2);
    let extension_file = manifest["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["partition_id"] == "extension:Ext")
        .unwrap()["file"]
        .as_str()
        .unwrap()
        .to_owned();
    let main_file = manifest["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["partition_id"] == "main")
        .unwrap()["file"]
        .as_str()
        .unwrap()
        .to_owned();
    let extension_diagnostics = serde_json::from_slice::<serde_json::Value>(
        &fs::read(root.join("baselines").join(&extension_file)).unwrap(),
    )
    .unwrap()["diagnostics"]
        .as_array()
        .unwrap()
        .len();
    assert!(!run(
        root,
        &["diagnostics", "baseline", "create", "-s", ".", "--partition", "extension:Ext"],
    )
    .status
    .success());

    fs::remove_file(root.join("baselines").join(&extension_file)).unwrap();
    let main_bytes = fs::read(root.join("baselines").join(&main_file)).unwrap();
    fs::write(root.join("baselines").join(&main_file), b"corrupt").unwrap();
    assert!(!run(
        root,
        &["diagnostics", "baseline", "create", "-s", ".", "--partition", "extension:Ext",],
    )
    .status
    .success());
    assert!(!root.join("baselines").join(&extension_file).exists());
    fs::write(root.join("baselines").join(&main_file), main_bytes).unwrap();

    let repair = json(
        root,
        &[
            "diagnostics",
            "baseline",
            "create",
            "-s",
            ".",
            "--partition",
            "extension:Ext",
            "--format",
            "json",
        ],
    );
    assert_eq!(repair["added"], 0);
    assert_eq!(repair["removed"], 0);
    assert_eq!(repair["unchanged"], extension_diagnostics);
    assert_eq!(repair["diagnostics"].as_array().unwrap().len(), extension_diagnostics);
    assert_eq!(repair["partitions"].as_array().unwrap().len(), 1);
    assert_eq!(repair["selected_partition"], "extension:Ext");
    let repaired = fs::read(root.join("baselines").join(&extension_file)).unwrap();
    let extension_hash = manifest["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["partition_id"] == "extension:Ext")
        .unwrap()["blake3"]
        .as_str()
        .unwrap();
    assert_eq!(blake3::hash(&repaired).to_hex().as_str(), extension_hash);

    fs::write(root.join("baselines").join(&extension_file), b"corrupt").unwrap();
    assert!(run(
        root,
        &["diagnostics", "baseline", "create", "-s", ".", "--partition", "extension:Ext"],
    )
    .status
    .success());

    fs::write(root.join("src/cf/New.bsl"), BROKEN).unwrap();
    fs::remove_file(root.join("src/cfe/Ext/Ext.bsl")).unwrap();
    let main_check = json(
        root,
        &["diagnostics", "baseline", "check", "-s", ".", "--partition", "main", "--format", "json"],
    );
    assert!(main_check["added"].as_u64().unwrap() > 0);
    assert_eq!(main_check["removed"], 0);

    let main_update = json(
        root,
        &[
            "diagnostics",
            "baseline",
            "update",
            "-s",
            ".",
            "--partition",
            "main",
            "--format",
            "json",
        ],
    );
    assert_eq!(main_update["selected_partition"], "main");
    let after: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("baselines/manifest.json")).unwrap()).unwrap();
    let extension_after = after["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["partition_id"] == "extension:Ext")
        .unwrap()["file"]
        .as_str()
        .unwrap();
    assert_eq!(extension_after, extension_file);
    assert!(!run(root, &["diagnostics", "baseline", "check", "-s", "."]).status.success());
    assert!(run(root, &["diagnostics", "baseline", "check", "-s", ".", "--partition", "main"],)
        .status
        .success());

    fs::create_dir_all(root.join("src/cfe/NewExt")).unwrap();
    fs::write(root.join("src/cfe/NewExt/Configuration.xml"), "<Configuration/>").unwrap();
    fs::write(root.join("src/cfe/NewExt/New.bsl"), BROKEN).unwrap();
    let config = fs::read_to_string(root.join("bsl-analyzer.toml"))
        .unwrap()
        .replace(
            "extensions = [{ name = \"Ext\", path = \"src/cfe/Ext\", scope = \"InfoBase\" }]",
            "extensions = [{ name = \"Ext\", path = \"src/cfe/Ext\", scope = \"InfoBase\" }, { name = \"NewExt\", path = \"src/cfe/NewExt\", scope = \"InfoBase\" }]",
        );
    fs::write(root.join("bsl-analyzer.toml"), config).unwrap();
    let full_update = run(root, &["diagnostics", "baseline", "update", "-s", "."]);
    assert!(
        full_update.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&full_update.stdout),
        String::from_utf8_lossy(&full_update.stderr)
    );
    let updated: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("baselines/manifest.json")).unwrap()).unwrap();
    assert_eq!(updated["partitions"].as_array().unwrap().len(), 3);
}

#[test]
fn migration_preserves_v1_and_rejects_unknown_selector_before_publish() {
    let temp = setup(false);
    let root = temp.path();
    assert!(run(root, &["diagnostics", "baseline", "create", "-s", "."]).status.success());
    let legacy = fs::read(root.join("legacy.json")).unwrap();
    let legacy_count = serde_json::from_slice::<serde_json::Value>(&legacy).unwrap()["diagnostics"]
        .as_array()
        .unwrap()
        .len();
    let config = fs::read_to_string(root.join("bsl-analyzer.toml"))
        .unwrap()
        .replace("path = \"legacy.json\"", "directory = \"baselines\"");
    fs::remove_file(root.join("bsl-analyzer.toml")).unwrap();
    fs::write(root.join("bsl-analyzer.partitioned-test.toml"), config).unwrap();
    let migrated = json(
        root,
        &[
            "diagnostics",
            "baseline",
            "create",
            "-s",
            ".",
            "-c",
            "bsl-analyzer.partitioned-test.toml",
            "--from-v1",
            "legacy.json",
            "--format",
            "json",
        ],
    );
    assert_eq!(migrated["added"], legacy_count);
    assert!(migrated["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(root.join("legacy.json")).unwrap(), legacy);
    assert!(root.join("baselines/manifest.json").is_file());

    let bad = run(
        root,
        &[
            "diagnostics",
            "baseline",
            "check",
            "-s",
            ".",
            "-c",
            "bsl-analyzer.partitioned-test.toml",
            "--partition",
            "extension:Missing",
        ],
    );
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown diagnostics baseline partition"));
}

#[test]
fn documented_usage_covers_directory_selectors_migration_and_ci() {
    let diagnostics = include_str!("../../../docs/configuration/DIAGNOSTICS.md");
    let ci = include_str!("../../../docs/CI_REPORTERS.md");
    for text in [diagnostics, ci] {
        assert!(text.contains("directory"));
        assert!(text.contains("--partition"));
        assert!(text.contains("--from-v1"));
        assert!(text.contains("diagnostics baseline check -s ."));
    }
}
