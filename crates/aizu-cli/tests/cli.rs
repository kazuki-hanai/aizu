use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use aizu_core::{RetentionPolicy, Spool, StatePaths};
use assert_cmd::Command as AssertCommand;
use chrono::Utc;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

fn aizu() -> AssertCommand {
    AssertCommand::new(assert_cmd::cargo::cargo_bin!("aizu"))
}

#[test]
fn emit_doctor_and_bridge_round_trip() {
    let directory = TempDir::new().unwrap();

    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "--display-name",
            "test-host",
            "emit",
            "task.completed",
            "--title",
            "Done",
            "--outcome",
            "succeeded",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let emitted: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(emitted["sequence"], 1);
    assert_eq!(emitted["event"]["kind"], "task.completed");
    assert_eq!(emitted["event"]["outcome"], "succeeded");
    assert_eq!(emitted["event"]["source"]["display_name"], "test-host");

    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .unwrap();
    let doctor: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doctor["healthy"], true);
    assert_eq!(doctor["event_count"], 1);
    assert_eq!(doctor["journal_mode"], "wal");

    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "bridge",
            "--protocol",
            "1",
            "--after",
            "0",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let frames: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["type"], "hello");
    assert_eq!(frames[1]["type"], "event");
    assert_eq!(frames[1]["sequence"], 1);
}

#[test]
fn bridge_rejects_protocol_mismatch_before_hello() {
    let directory = TempDir::new().unwrap();
    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "bridge",
            "--protocol",
            "999",
            "--after",
            "0",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let lines: Vec<_> = output.stdout.split(|byte| *byte == b'\n').collect();
    let frame: Value = serde_json::from_slice(lines[0]).unwrap();
    assert_eq!(frame["type"], "error");
    assert_eq!(frame["code"], "incompatible_protocol");
    assert!(!directory.path().exists() || directory.path().read_dir().unwrap().next().is_none());
}

#[test]
fn bridge_reports_cursor_ahead_after_hello() {
    let directory = TempDir::new().unwrap();
    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "bridge",
            "--protocol",
            "1",
            "--after",
            "1",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let frames: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(frames[0]["type"], "hello");
    assert_eq!(frames[1]["type"], "error");
    assert_eq!(frames[1]["code"], "cursor_ahead");
}

#[test]
fn stdin_json_ignores_spoofed_identity_fields() {
    let directory = TempDir::new().unwrap();
    let payload = serde_json::json!({
        "kind": "agent.question",
        "title": "Need input",
        "id": "attacker",
        "schema_version": 999,
        "source": {"source_id": "attacker"}
    })
    .to_string();
    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "emit",
            "--stdin-json",
            "--json",
        ])
        .write_stdin(payload)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["event"]["schema_version"], 1);
    assert_ne!(value["event"]["id"], "attacker");
    assert_ne!(value["event"]["source"]["source_id"], "attacker");
}

#[test]
fn stdin_json_rejects_duplicate_keys_without_persisting() {
    let directory = TempDir::new().unwrap();
    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "emit",
            "--stdin-json",
        ])
        .write_stdin(r#"{"kind":"agent.question","title":"one","title":"two"}"#)
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate JSON object key"));

    let report = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["event_count"], 0);
}

#[test]
fn emit_rejects_invalid_inputs_without_persisting() {
    let directory = TempDir::new().unwrap();
    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "emit",
            "agent.question",
            "--title",
            "Question",
            "--outcome",
            "succeeded",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "agent.question must not include an outcome",
        ));

    let doctor = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["event_count"], 0);
    assert_eq!(report["latest_sequence"], 0);
}

#[test]
fn hook_is_best_effort_unless_strict() {
    let directory = TempDir::new().unwrap();
    let unsafe_state_path = directory.path().join("state");
    #[cfg(unix)]
    std::os::unix::fs::symlink(directory.path().join("target"), &unsafe_state_path).unwrap();
    #[cfg(not(unix))]
    fs::write(&unsafe_state_path, "not a directory").unwrap();

    aizu()
        .args([
            "--state-dir",
            unsafe_state_path.to_str().unwrap(),
            "hook",
            "--agent",
            "generic",
            "--event",
            "question",
        ])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("was not persisted"));

    aizu()
        .args([
            "--state-dir",
            unsafe_state_path.to_str().unwrap(),
            "hook",
            "--agent",
            "generic",
            "--event",
            "question",
            "--strict",
        ])
        .write_stdin("{}")
        .assert()
        .failure();
}

#[test]
fn bridge_spool_failure_is_a_single_protocol_frame() {
    let directory = TempDir::new().unwrap();
    let state_path = directory.path().join("state");
    #[cfg(unix)]
    std::os::unix::fs::symlink(directory.path().join("target"), &state_path).unwrap();
    #[cfg(not(unix))]
    fs::write(&state_path, "not a directory").unwrap();

    let output = aizu()
        .args([
            "--state-dir",
            state_path.to_str().unwrap(),
            "bridge",
            "--protocol",
            "1",
            "--after",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let frames: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["type"], "error");
    assert_eq!(frames[0]["code"], "spool_unavailable");
    assert!(!output.stderr.is_empty());
}

#[test]
fn version_does_not_create_a_spool() {
    let directory = TempDir::new().unwrap();
    let state_path = directory.path().join("unused");
    let output = aizu()
        .args([
            "--state-dir",
            state_path.to_str().unwrap(),
            "version",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["protocol"], 1);
    assert_eq!(report["event_schema"], 1);
    assert!(!state_path.exists());
}

#[test]
fn identity_regeneration_requires_explicit_discard_and_creates_backup() {
    let directory = TempDir::new().unwrap();
    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "emit",
            "agent.question",
            "--title",
            "Question",
        ])
        .assert()
        .success();

    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "identity",
            "regenerate",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("events remain"));

    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "identity",
            "regenerate",
            "--discard-events",
        ])
        .assert()
        .failure();

    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "identity",
            "regenerate",
            "--discard-events",
            "--yes",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["discarded_events"], 1);
    let backup = result["backup_path"].as_str().unwrap();
    assert!(fs::metadata(backup).unwrap().len() > 0);
}

#[test]
fn follow_stream_delivers_event_added_after_startup() {
    let directory = TempDir::new().unwrap();
    let binary = assert_cmd::cargo::cargo_bin!("aizu");
    let mut child = Command::new(binary)
        .env("AIZU_TEST_HEARTBEAT_MS", "50")
        .env("AIZU_TEST_POLL_MS", "10")
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "--display-name",
            "remote",
            "bridge",
            "--protocol",
            "1",
            "--after",
            "0",
            "--follow",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let hello: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(hello["type"], "hello");

    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "--display-name",
            "remote",
            "emit",
            "agent.question",
            "--title",
            "Question",
        ])
        .assert()
        .success();

    let event = (0..20)
        .find_map(|_| {
            let frame: Value = serde_json::from_str(&lines.next()?.ok()?).ok()?;
            match frame["type"].as_str() {
                Some("event") => Some(frame),
                Some("heartbeat") => None,
                other => panic!("unexpected bridge frame before event: {other:?}"),
            }
        })
        .expect("event should arrive within 20 bridge frames");
    assert_eq!(event["type"], "event");
    assert_eq!(event["event"]["title"], "Question");
    drop(lines);
    thread::sleep(Duration::from_millis(50));
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
}

#[test]
fn bridge_reports_pruned_ranges_before_retained_events() {
    let directory = TempDir::new().unwrap();
    for index in 0..3 {
        aizu()
            .args([
                "--state-dir",
                directory.path().to_str().unwrap(),
                "emit",
                "agent.question",
                "--title",
                &format!("Question {index}"),
            ])
            .assert()
            .success();
    }
    let spool = Spool::open(StatePaths::new(directory.path())).unwrap();
    spool
        .maintain(
            Utc::now(),
            RetentionPolicy {
                max_events: 1,
                ..RetentionPolicy::default()
            },
        )
        .unwrap();

    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "bridge",
            "--protocol",
            "1",
            "--after",
            "0",
        ])
        .output()
        .unwrap();
    let frames: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["hello", "gap", "event"]
    );
    assert_eq!(frames[1]["requested_after"], 0);
    assert_eq!(frames[1]["lost_through_sequence"], 2);
    assert_eq!(frames[1]["oldest_sequence"], 3);
    assert_eq!(frames[2]["sequence"], 3);
}

#[test]
fn bridge_reports_gap_when_every_event_was_pruned() {
    let directory = TempDir::new().unwrap();
    aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "emit",
            "agent.question",
            "--title",
            "Question",
        ])
        .assert()
        .success();
    let spool = Spool::open(StatePaths::new(directory.path())).unwrap();
    spool
        .maintain(
            Utc::now(),
            RetentionPolicy {
                max_events: 0,
                ..RetentionPolicy::default()
            },
        )
        .unwrap();

    let output = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "bridge",
            "--protocol",
            "1",
            "--after",
            "0",
        ])
        .output()
        .unwrap();
    let frames: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["oldest_sequence"], Value::Null);
    assert_eq!(frames[0]["latest_sequence"], 1);
    assert_eq!(frames[1]["type"], "gap");
    assert_eq!(frames[1]["oldest_sequence"], Value::Null);
    assert_eq!(frames[1]["lost_through_sequence"], 1);
}

#[test]
fn concurrent_process_emit_allocates_every_sequence_once() {
    let directory = TempDir::new().unwrap();
    let binary = assert_cmd::cargo::cargo_bin!("aizu");
    let mut children = Vec::new();
    for index in 0..20 {
        children.push(
            Command::new(binary)
                .args([
                    "--state-dir",
                    directory.path().to_str().unwrap(),
                    "--display-name",
                    "process-test",
                    "emit",
                    "agent.question",
                    "--title",
                    &format!("Question {index}"),
                    "--json",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }

    let mut sequences = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        sequences.push(value["sequence"].as_i64().unwrap());
    }
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=20).collect::<Vec<_>>());

    let report = aizu()
        .args([
            "--state-dir",
            directory.path().to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["event_count"], 20);
    assert_eq!(report["latest_sequence"], 20);
}
