use std::{fs, path::Path, process::Command};

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn one_semantic_run_classifies_main_and_extension_before_reporting() {
    let temp = tempfile::tempdir().unwrap();
    for root in ["src/cf", "src/cfe/Ext"] {
        fs::create_dir_all(temp.path().join(root)).unwrap();
        fs::write(temp.path().join(root).join("Configuration.xml"), "<Configuration/>").unwrap();
    }
    fs::write(temp.path().join("src/cf/Main.bsl"), "Процедура Тест(\n").unwrap();
    fs::write(temp.path().join("src/cfe/Ext/Ext.bsl"), "Процедура Тест(\n").unwrap();
    fs::write(
        temp.path().join("bsl-analyzer.toml"),
        r#"[source]
root = "src/cf"
extensions = [{ name = "Ext", path = "src/cfe/Ext", scope = "InfoBase" }]

[diagnostics.baseline]
directory = "baselines"
"#,
    )
    .unwrap();
    assert!(run(temp.path(), &["diagnostics", "baseline", "create", "-s", "."]).status.success());

    let reports = temp.path().join("reports");
    fs::create_dir(&reports).unwrap();
    let output = run(temp.path(), &["analyze", "-s", ".", "-r", "json", "-o", "reports", "-q"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(reports.join("bsl-json.json")).unwrap()).unwrap();
    assert_eq!(report["baseline"]["state"], "full");
    assert_eq!(report["baseline"]["new"], 0);
    assert!(report["baseline"]["known"].as_u64().unwrap() > 0);
    let partitions = report["baseline"]["partitions"].as_array().unwrap();
    assert_eq!(partitions.len(), 2);
    assert!(partitions.iter().any(|partition| partition["id"] == "main"));
    assert!(partitions.iter().any(|partition| partition["id"] == "extension:Ext"));
}
