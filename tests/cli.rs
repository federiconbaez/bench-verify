use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn write_claim(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("claim.toml");
    fs::write(&p, body).unwrap();
    p
}

#[test]
fn pass_on_constant_echo() {
    let dir = tempdir().unwrap();
    let claim = write_claim(
        dir.path(),
        r#"
name = "echo-pass"
command = "echo 'thrpt: 42.0 Melem/s'"
metric_regex = 'thrpt:\s*([0-9.]+)'
runs = 5
bootstrap_samples = 200

[threshold]
min = 40.0
"#,
    );
    Command::cargo_bin("bench-verify")
        .unwrap()
        .arg(&claim)
        .arg("-C")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"verdict\": \"PASS\""))
        .stdout(predicate::str::contains("\"median\": 42.0"));
}

#[test]
fn fail_when_below_threshold() {
    let dir = tempdir().unwrap();
    let claim = write_claim(
        dir.path(),
        r#"
name = "echo-fail"
command = "echo 'thrpt: 3.0 Melem/s'"
metric_regex = 'thrpt:\s*([0-9.]+)'
runs = 3
bootstrap_samples = 100

[threshold]
min = 10.0
"#,
    );
    Command::cargo_bin("bench-verify")
        .unwrap()
        .arg(&claim)
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"verdict\": \"FAIL\""));
}

#[test]
fn exits_2_on_bad_regex_match() {
    let dir = tempdir().unwrap();
    let claim = write_claim(
        dir.path(),
        r#"
name = "no-metric"
command = "echo hello"
metric_regex = 'thrpt:\s*([0-9.]+)'
runs = 1

[threshold]
min = 1.0
"#,
    );
    Command::cargo_bin("bench-verify")
        .unwrap()
        .arg(&claim)
        .assert()
        .failure()
        .code(2);
}

#[test]
fn max_threshold() {
    let dir = tempdir().unwrap();
    let claim = write_claim(
        dir.path(),
        r#"
name = "max-ok"
command = "printf 'latency_ms=%s\\n' 1.5"
metric_regex = 'latency_ms=([0-9.]+)'
runs = 4
bootstrap_samples = 100

[threshold]
max = 2.0
"#,
    );
    Command::cargo_bin("bench-verify")
        .unwrap()
        .arg(&claim)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"verdict\": \"PASS\""));
}
