use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use directories::BaseDirs;
use serde::Deserialize;
use thiserror::Error;

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_VERSION_OUTPUT_BYTES: u64 = 16 * 1_024;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CliDiagnostic {
    Missing,
    Installed { version: String },
    VersionMismatch { version: Option<String> },
}

#[derive(Debug, Error)]
pub enum CliDiagnosticError {
    #[error("home directory is unavailable")]
    HomeUnavailable,
    #[error("CLI metadata could not be read: {0}")]
    Metadata(io::Error),
    #[error("CLI path is not a regular file")]
    NotRegularFile,
    #[error("CLI could not be started: {0}")]
    Spawn(io::Error),
    #[error("CLI version check timed out")]
    Timeout,
    #[error("CLI stdout reader failed")]
    Reader,
    #[error("CLI version output exceeded 16 KiB")]
    OversizedOutput,
    #[error("CLI version command failed")]
    CommandFailed,
    #[error("CLI version output was invalid")]
    InvalidOutput,
}

#[derive(Deserialize)]
struct VersionReport {
    application: String,
    protocol: u16,
    event_schema: u16,
    database_schema: u16,
    sqlite: String,
}

const PROTOCOL_VERSION: u16 = 1;
const EVENT_SCHEMA_VERSION: u16 = 1;
const DATABASE_SCHEMA_VERSION: u16 = 1;

pub fn inspect(expected_version: &str) -> Result<CliDiagnostic, CliDiagnosticError> {
    let path = cli_path()?;
    inspect_path(&path, expected_version)
}

pub fn inspect_path(
    path: &std::path::Path,
    expected_version: &str,
) -> Result<CliDiagnostic, CliDiagnosticError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(CliDiagnostic::Missing),
        Err(error) => return Err(CliDiagnosticError::Metadata(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(CliDiagnosticError::NotRegularFile);
    }

    let mut child = Command::new(path)
        .args(["version", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(CliDiagnosticError::Spawn)?;
    let stdout = child.stdout.take().ok_or(CliDiagnosticError::Reader)?;
    let reader = thread::Builder::new()
        .name("aizu-cli-version-reader".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_VERSION_OUTPUT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })
        .map_err(CliDiagnosticError::Spawn)?;

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(CliDiagnosticError::Spawn)? {
            break status;
        }
        if started.elapsed() >= VERSION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(CliDiagnosticError::Timeout);
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = reader
        .join()
        .map_err(|_| CliDiagnosticError::Reader)?
        .map_err(CliDiagnosticError::Spawn)?;
    if bytes.len() > usize::try_from(MAX_VERSION_OUTPUT_BYTES).unwrap_or(usize::MAX) {
        return Err(CliDiagnosticError::OversizedOutput);
    }
    if !status.success() {
        return Err(CliDiagnosticError::CommandFailed);
    }
    let report = parse_version_report(&bytes)?;
    Ok(diagnostic_for_report(report, expected_version))
}

fn diagnostic_for_report(report: VersionReport, expected_version: &str) -> CliDiagnostic {
    if report.application == expected_version
        && report.protocol == PROTOCOL_VERSION
        && report.event_schema == EVENT_SCHEMA_VERSION
        && report.database_schema == DATABASE_SCHEMA_VERSION
    {
        CliDiagnostic::Installed {
            version: report.application,
        }
    } else {
        CliDiagnostic::VersionMismatch {
            version: Some(report.application),
        }
    }
}

fn parse_version_report(bytes: &[u8]) -> Result<VersionReport, CliDiagnosticError> {
    let report: VersionReport =
        serde_json::from_slice(bytes).map_err(|_| CliDiagnosticError::InvalidOutput)?;
    let valid_application = is_version_identifier(&report.application);
    let valid_sqlite = !report.sqlite.is_empty()
        && report
            .sqlite
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.');
    if !valid_application || !valid_sqlite {
        return Err(CliDiagnosticError::InvalidOutput);
    }
    Ok(report)
}

fn is_version_identifier(value: &str) -> bool {
    let core = value
        .split_once(['-', '+'])
        .map_or(value, |(core, _suffix)| core);
    let mut components = core.split('.');
    let valid_core = (0..3).all(|_| {
        components.next().is_some_and(|component| {
            !component.is_empty() && component.chars().all(|c| c.is_ascii_digit())
        })
    }) && components.next().is_none();
    valid_core
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+')
        })
}

fn cli_path() -> Result<PathBuf, CliDiagnosticError> {
    let base = BaseDirs::new().ok_or(CliDiagnosticError::HomeUnavailable)?;
    Ok(base.home_dir().join(".local/bin/aizu"))
}

#[cfg(test)]
mod tests {
    use super::{CliDiagnostic, diagnostic_for_report, parse_version_report};

    #[test]
    fn diagnostic_states_do_not_expose_process_output() {
        assert_eq!(CliDiagnostic::Missing, CliDiagnostic::Missing);
        assert_eq!(
            CliDiagnostic::Installed {
                version: "0.1.0".to_owned(),
            },
            CliDiagnostic::Installed {
                version: "0.1.0".to_owned(),
            }
        );
    }

    #[test]
    fn managed_cli_identity_requires_the_complete_version_contract() {
        assert!(parse_version_report(br#"{"application":"0.1.0"}"#).is_err());
        assert!(
            parse_version_report(
                br#"{"application":"tool","protocol":1,"event_schema":1,"database_schema":1,"sqlite":"3.53.2"}"#,
            )
            .is_err()
        );
        assert!(
            parse_version_report(
                br#"{"application":"0.1.0","protocol":2,"event_schema":1,"database_schema":1,"sqlite":"3.53.2"}"#,
            )
            .is_ok()
        );
        assert!(
            parse_version_report(
                br#"{"application":"0.1.0","protocol":1,"event_schema":1,"database_schema":1,"sqlite":"3.53.2"}"#,
            )
            .is_ok()
        );
        assert!(
            parse_version_report(
                br#"{"application":"0.1.0-dev.1","protocol":1,"event_schema":1,"database_schema":1,"sqlite":"3.53.2"}"#,
            )
            .is_ok()
        );
        let older = parse_version_report(
            br#"{"application":"0.0.9","protocol":0,"event_schema":0,"database_schema":0,"sqlite":"3.45.0"}"#,
        )
        .expect("older Aizu reports retain their managed identity");
        assert_eq!(
            diagnostic_for_report(older, "0.1.0"),
            CliDiagnostic::VersionMismatch {
                version: Some("0.0.9".to_owned()),
            }
        );
    }
}
