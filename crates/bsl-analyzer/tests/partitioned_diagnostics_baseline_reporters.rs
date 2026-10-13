use std::{fs, path::Path, process::Command};

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bsl-analyzer-app"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn setup() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("src/cf")).unwrap();
    fs::create_dir_all(temp.path().join("src/cfe/Ext")).unwrap();
    for root in ["src/cf", "src/cfe/Ext"] {
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
    temp
}

#[test]
fn partitioned_baseline_reporters_keep_their_existing_containers() {
    let temp = setup();
    fs::create_dir(temp.path().join("reports")).unwrap();
    let output = run(
        temp.path(),
        &["analyze", "-s", ".", "-r", "console,json,sarif,junit", "-o", "reports"],
    );
    assert!(output.status.success());
    let console = String::from_utf8(output.stdout).unwrap();
    assert!(console.contains("main: full"));
    assert!(console.contains("extension:Ext: full"));

    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("reports/bsl-json.json")).unwrap())
            .unwrap();
    assert_eq!(json["baseline"]["partitions"].as_array().unwrap().len(), 2);
    let sarif: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("reports/bsl-analyzer.sarif")).unwrap())
            .unwrap();
    assert_eq!(
        sarif["runs"][0]["properties"]["baseline"]["partitions"].as_array().unwrap().len(),
        2
    );
    assert!(sarif["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|result| result.get("baselineState").is_none()));
    // JUnit carries no baseline summary: `<properties>` is only valid inside a
    // `<testsuite>`, and a document with one under `<testsuites>` fails schema
    // validation in the tools this format exists for. SARIF and JSONL carry it.
    let junit = fs::read_to_string(temp.path().join("reports/bsl-analyzer.junit.xml")).unwrap();
    assert!(!junit.contains("<properties>"));

    let jsonl = run(temp.path(), &["analyze", "-s", ".", "--format", "jsonl"]);
    assert!(jsonl.status.success());
    let done: serde_json::Value = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .find(|event: &serde_json::Value| event["type"] == "done")
        .unwrap();
    assert_eq!(done["baseline"]["partitions"].as_array().unwrap().len(), 2);
}

#[test]
fn codequality_shape_and_fingerprints_survive_partitioning() {
    let temp = setup();
    fs::write(temp.path().join("src/cf/New.bsl"), "Процедура Новая(\n").unwrap();
    fs::create_dir(temp.path().join("reports")).unwrap();
    let output =
        run(temp.path(), &["analyze", "-s", ".", "-r", "codequality", "-o", "reports", "-q"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(temp.path().join("reports/gl-code-quality-report.json")).unwrap(),
    )
    .unwrap();
    let findings = value.as_array().expect("Code Quality root remains an array");
    assert!(!findings.is_empty());
    assert!(findings.iter().all(|finding| {
        finding.get("baseline").is_none()
            && finding["fingerprint"].as_str().is_some_and(|value| value.len() == 64)
    }));
}
