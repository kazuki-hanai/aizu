#[cfg(unix)]
mod unix {
    use std::{
        fs,
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::{Arc, Mutex, mpsc},
        thread,
        time::Duration,
    };

    use aizu_core::{DesktopState, RemoteBridgeConsumer, SystemSshSource};
    use chrono::Utc;
    use tempfile::TempDir;

    const TIMEOUT: Duration = Duration::from_secs(15);

    fn consume_bridge(
        desktop: DesktopState,
        alias: &str,
        after: i64,
        expected_ingested: usize,
        expected_duplicates: usize,
        expected_cursor: i64,
    ) {
        let source = SystemSshSource::new("/usr/bin/ssh", alias).expect("valid SSH fixture alias");
        let spec = source.bridge_command(after).expect("valid bridge cursor");
        let mut child = Command::new(spec.program)
            .args(spec.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start system SSH bridge");
        let stdout = child.stdout.take().expect("bridge stdout");
        let child = Arc::new(Mutex::new(child));
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut consumer =
                RemoteBridgeConsumer::open(desktop, "ssh:ci", "CI SSH fixture", Utc::now())
                    .expect("open desktop bridge consumer");
            let mut ingested = 0usize;
            let mut duplicates = 0usize;
            for line in BufReader::new(stdout).lines() {
                let line = line.expect("read bridge frame");
                let mut frame = line.into_bytes();
                frame.push(b'\n');
                let report = consumer
                    .push_stdout(&frame, Utc::now())
                    .expect("consume bridge frame");
                ingested += report.ingested;
                duplicates += report.duplicates;
                if ingested >= expected_ingested && duplicates >= expected_duplicates {
                    let _ = sender.send((consumer.cursor(), ingested, duplicates));
                    return;
                }
            }
            let _ = sender.send((consumer.cursor(), ingested, duplicates));
        });

        let result = receiver.recv_timeout(TIMEOUT);
        if let Ok(mut child) = child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        reader.join().expect("join bridge reader");
        let (cursor, ingested, duplicates) = result.expect("bridge produced expected frames");
        assert_eq!(ingested, expected_ingested);
        assert_eq!(duplicates, expected_duplicates);
        assert_eq!(cursor, expected_cursor);
    }

    #[test]
    fn real_ssh_bridge_reconnects_from_cursor_and_deduplicates_replay() {
        let Ok(alias) = std::env::var("AIZU_REAL_SSH_ALIAS") else {
            return;
        };
        let phase = std::env::var("AIZU_REAL_SSH_PHASE").expect("fixture phase");
        let database = std::env::var("AIZU_REAL_SSH_DB").expect("fixture database path");
        let desktop = DesktopState::open(&database).expect("open isolated desktop database");

        match phase.as_str() {
            "initial" => {
                consume_bridge(desktop.clone(), &alias, 0, 1, 0, 1);
                assert_eq!(desktop.source("ssh:ci").unwrap().unwrap().cursor, 1);
                assert_eq!(desktop.recent_history(Some(10)).unwrap().len(), 1);
            }
            "resume" => {
                assert_eq!(desktop.source("ssh:ci").unwrap().unwrap().cursor, 1);
                consume_bridge(desktop.clone(), &alias, 1, 1, 0, 2);
                assert_eq!(desktop.source("ssh:ci").unwrap().unwrap().cursor, 2);
                assert_eq!(desktop.recent_history(Some(10)).unwrap().len(), 2);

                consume_bridge(desktop.clone(), &alias, 0, 0, 2, 2);
                assert_eq!(desktop.source("ssh:ci").unwrap().unwrap().cursor, 2);
                assert_eq!(desktop.recent_history(Some(10)).unwrap().len(), 2);
            }
            other => panic!("unknown real SSH fixture phase: {other}"),
        }

        let _ = fs::metadata(database).expect("durable desktop database remains available");
    }

    #[test]
    fn real_ssh_fixture_uses_an_isolated_database_when_enabled() {
        if std::env::var_os("AIZU_REAL_SSH_ALIAS").is_some() {
            return;
        }
        let temp = TempDir::new().unwrap();
        let state = DesktopState::open(temp.path().join("desktop.sqlite")).unwrap();
        assert!(state.recent_history(Some(1)).unwrap().is_empty());
    }
}

#[cfg(not(unix))]
#[test]
fn real_ssh_bridge_fixture_is_unix_only() {}
