//! End-to-end tests for the `scanbench` binary: output shape and exit codes.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn scanbench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_scanbench"))
        .args(args)
        .output()
        .expect("run scanbench")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8 stderr")
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).expect("json report")
}

#[test]
fn the_fixture_run_passes_and_reports_json() {
    let output = scanbench(&["--fixtures", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let report = json(&output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(
        report["backend"],
        if cfg!(target_os = "macos") {
            "platform"
        } else {
            "reference"
        },
        "the fixture run must exercise the shipped scanner on macOS"
    );
    assert_eq!(report["fixture"]["ok"], true);
    assert!(report["files"].as_u64().expect("files") > 0);
    assert!(report["dirs"].as_u64().expect("dirs") > 0);
    assert!(report["allocated_bytes"].as_u64().expect("allocated") > 0);
    // The fixture tree always contains an unreadable directory on Unix; the deny
    // ACL is skipped on Windows, so a Windows scan is complete.
    #[cfg(unix)]
    assert_eq!(report["status"], "partial");
    #[cfg(not(unix))]
    assert_eq!(report["status"], "complete");
    assert!(report["db"].as_str().expect("db").ends_with("scanbench.db"));
}

#[test]
fn the_text_report_has_the_plan_labels_in_order() {
    let output = scanbench(&["--fixtures"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let labels: Vec<String> = stdout(&output)
        .lines()
        .map(|line| {
            line.chars()
                .take(16)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();

    assert_eq!(
        labels,
        [
            "path",
            "backend",
            "files",
            "dirs",
            "allocated",
            "logical",
            "skipped",
            "elapsed",
            "peak rss",
            "fixture",
        ]
    );
}

#[test]
fn a_file_root_is_counted_as_one_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("note.txt");
    fs::write(&file, b"hello").expect("write");

    let output = scanbench(&[file.to_str().expect("utf-8 path"), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let report = json(&output);
    assert_eq!(report["files"], 1);
    assert_eq!(report["dirs"], 0);
    assert_eq!(report["entries"], 1);
    assert_eq!(report["logical_bytes"], 5);
    assert_eq!(report["status"], "complete");
}

#[test]
fn budget_passes_on_a_small_fixture_scan() {
    let output = scanbench(&["--fixtures", "--budget"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("budget          ok"));
}

#[test]
fn a_missing_path_is_a_scan_error() {
    let missing = Path::new("/definitely/not/here/scanbench");
    let output = scanbench(&[missing.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("scanbench: cannot open"));
    assert!(output.stdout.is_empty());
}

#[test]
fn a_path_and_fixtures_together_are_refused() {
    let output = scanbench(&[".", "--fixtures"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!stderr(&output).is_empty());
}
