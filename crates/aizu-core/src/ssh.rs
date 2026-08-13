use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::PROTOCOL_VERSION;

const MAX_HOST_ALIAS_SCALARS: usize = 255;
const MAX_PREFLIGHT_OUTPUT_BYTES: usize = 128 * 1024;
const REMOTE_CLI: &str = "$HOME/.local/bin/aizu";
const RECONNECT_SECONDS: [u64; 6] = [1, 2, 5, 10, 30, 60];

/// A shell-free system SSH invocation assembled from trusted fixed options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshCommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Builder for the system SSH commands used by a remote Aizu source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemSshSource {
    executable: PathBuf,
    host_alias: String,
}

impl SystemSshSource {
    /// Creates a source backed by the platform's explicit system SSH path.
    pub fn new(
        executable: impl Into<PathBuf>,
        host_alias: impl Into<String>,
    ) -> Result<Self, SshConfigurationError> {
        let executable = executable.into();
        if !executable.is_absolute() {
            return Err(SshConfigurationError::ExecutableMustBeAbsolute);
        }

        let host_alias = host_alias.into();
        validate_host_alias(&host_alias)?;
        Ok(Self {
            executable,
            host_alias,
        })
    }

    /// Returns an `ssh -G` invocation used to inspect effective config safely.
    #[must_use]
    pub fn preflight_command(&self) -> SshCommandSpec {
        SshCommandSpec {
            program: self.executable.clone(),
            args: vec!["-G".to_owned(), self.host_alias.clone()],
        }
    }

    /// Returns the fixed bridge invocation for the given durable cursor.
    pub fn bridge_command(&self, after: i64) -> Result<SshCommandSpec, SshConfigurationError> {
        if after < 0 {
            return Err(SshConfigurationError::InvalidCursor(after));
        }

        let remote_command = format!(
            "exec \"{REMOTE_CLI}\" bridge --protocol {PROTOCOL_VERSION} --after {after} --follow"
        );
        let args = [
            "-T",
            "-n",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "PermitLocalCommand=no",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain([self.host_alias.clone(), remote_command])
        .collect();

        Ok(SshCommandSpec {
            program: self.executable.clone(),
            args,
        })
    }

    #[must_use]
    pub fn host_alias(&self) -> &str {
        &self.host_alias
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Returns a bounded reconnect delay with stable per-source jitter.
    #[must_use]
    pub fn reconnect_delay(&self, attempt: usize) -> Duration {
        let index = attempt.min(RECONNECT_SECONDS.len() - 1);
        let base_millis = RECONNECT_SECONDS[index] * 1_000;
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in self.host_alias.bytes().chain(attempt.to_le_bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A stable -20%..=20% spread prevents registered sources reconnecting
        // in lockstep while keeping tests and diagnostics deterministic.
        let jitter_permille = i64::try_from(hash % 401).expect("bounded remainder") - 200;
        let adjusted =
            i64::try_from(base_millis).expect("bounded delay") * (1_000 + jitter_permille) / 1_000;
        Duration::from_millis(u64::try_from(adjusted).expect("positive jittered delay"))
    }
}

/// Validates the bounded, local SSH config alias accepted by the UI.
pub fn validate_host_alias(alias: &str) -> Result<(), SshConfigurationError> {
    let length = alias.chars().count();
    if length == 0 {
        return Err(SshConfigurationError::EmptyHostAlias);
    }
    if length > MAX_HOST_ALIAS_SCALARS {
        return Err(SshConfigurationError::HostAliasTooLong {
            actual: length,
            maximum: MAX_HOST_ALIAS_SCALARS,
        });
    }
    if alias.starts_with('-') {
        return Err(SshConfigurationError::OptionLikeHostAlias);
    }
    if alias.chars().any(char::is_whitespace) {
        return Err(SshConfigurationError::WhitespaceInHostAlias);
    }
    if alias.chars().any(char::is_control) {
        return Err(SshConfigurationError::ControlCharacterInHostAlias);
    }
    Ok(())
}

/// Checks effective `ssh -G` output for config that conflicts with Aizu's
/// fixed remote bridge command.
pub fn validate_preflight_output(output: &[u8]) -> Result<(), SshConfigurationError> {
    if output.len() > MAX_PREFLIGHT_OUTPUT_BYTES {
        return Err(SshConfigurationError::PreflightOutputTooLarge {
            actual: output.len(),
            maximum: MAX_PREFLIGHT_OUTPUT_BYTES,
        });
    }
    let output =
        std::str::from_utf8(output).map_err(|_| SshConfigurationError::PreflightOutputNotUtf8)?;
    for line in output.lines() {
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if key.eq_ignore_ascii_case("remotecommand") && !value.trim().eq_ignore_ascii_case("none") {
            return Err(SshConfigurationError::RemoteCommandConflict);
        }
    }
    Ok(())
}

/// Stable categories shown by diagnostics without exposing raw SSH stderr.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshFailureCategory {
    RetryableNetwork,
    AuthenticationRequired,
    HostVerificationFailed,
    MissingRemoteCli,
    ConfigurationConflict,
    RemoteFailure,
}

impl SshFailureCategory {
    #[must_use]
    pub const fn retry_automatically(self) -> bool {
        matches!(self, Self::RetryableNetwork)
    }
}

/// Categorizes bounded SSH diagnostics; callers must not persist the raw text.
#[must_use]
pub fn classify_ssh_failure(stderr: &[u8]) -> SshFailureCategory {
    let bounded = &stderr[..stderr.len().min(MAX_PREFLIGHT_OUTPUT_BYTES)];
    let text = String::from_utf8_lossy(bounded).to_ascii_lowercase();

    if text.contains("remotecommand") && text.contains("already") {
        return SshFailureCategory::ConfigurationConflict;
    }
    if text.contains("remote host identification has changed")
        || text.contains("host key verification failed")
        || text.contains("no matching host key")
    {
        return SshFailureCategory::HostVerificationFailed;
    }
    if text.contains("permission denied")
        || text.contains("no supported authentication methods")
        || text.contains("sign_and_send_pubkey")
    {
        return SshFailureCategory::AuthenticationRequired;
    }
    if text.contains("aizu: not found")
        || text.contains("aizu: command not found")
        || (text.contains(".local/bin/aizu") && text.contains("no such file"))
    {
        return SshFailureCategory::MissingRemoteCli;
    }
    if [
        "connection refused",
        "connection reset",
        "connection closed",
        "operation timed out",
        "connection timed out",
        "no route to host",
        "network is unreachable",
        "could not resolve hostname",
        "temporary failure in name resolution",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        return SshFailureCategory::RetryableNetwork;
    }
    SshFailureCategory::RemoteFailure
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SshConfigurationError {
    #[error("SSH executable path must be absolute")]
    ExecutableMustBeAbsolute,
    #[error("SSH host alias must not be empty")]
    EmptyHostAlias,
    #[error("SSH host alias must not begin with '-'")]
    OptionLikeHostAlias,
    #[error("SSH host alias must not contain whitespace")]
    WhitespaceInHostAlias,
    #[error("SSH host alias must not contain control characters")]
    ControlCharacterInHostAlias,
    #[error("SSH host alias is {actual} characters; maximum is {maximum}")]
    HostAliasTooLong { actual: usize, maximum: usize },
    #[error("SSH bridge cursor must be non-negative, got {0}")]
    InvalidCursor(i64),
    #[error("SSH config has a RemoteCommand that conflicts with the Aizu bridge")]
    RemoteCommandConflict,
    #[error("SSH preflight output is not UTF-8")]
    PreflightOutputNotUtf8,
    #[error("SSH preflight output is {actual} bytes; maximum is {maximum}")]
    PreflightOutputTooLarge { actual: usize, maximum: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(alias: &str) -> SystemSshSource {
        SystemSshSource::new("/usr/bin/ssh", alias).expect("valid source")
    }

    #[test]
    fn bridge_command_uses_fixed_security_options_and_remote_command() {
        let command = source("build-server").bridge_command(42).expect("command");
        assert_eq!(command.program, PathBuf::from("/usr/bin/ssh"));
        assert_eq!(command.args[0..2], ["-T", "-n"]);
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-o", "BatchMode=yes"])
        );
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["-o", "StrictHostKeyChecking=yes"])
        );
        assert_eq!(command.args[command.args.len() - 2], "build-server");
        assert_eq!(
            command.args.last().expect("remote command"),
            "exec \"$HOME/.local/bin/aizu\" bridge --protocol 1 --after 42 --follow"
        );
    }

    #[test]
    fn aliases_cannot_be_reinterpreted_as_options_or_shell_input() {
        for alias in ["-Fattack", "host name", "host\nname", "host\0name"] {
            assert!(SystemSshSource::new("/usr/bin/ssh", alias).is_err());
        }
        assert!(SystemSshSource::new("ssh", "host").is_err());
        assert!(SystemSshSource::new("/usr/bin/ssh", "user@host.example").is_ok());
    }

    #[test]
    fn cursor_is_numeric_and_non_negative() {
        assert_eq!(
            source("host").bridge_command(-1),
            Err(SshConfigurationError::InvalidCursor(-1))
        );
        assert!(source("host").bridge_command(i64::MAX).is_ok());
    }

    #[test]
    fn reconnect_delay_is_jittered_and_capped() {
        let source = source("host-a");
        for (attempt, base_seconds) in RECONNECT_SECONDS.into_iter().enumerate() {
            let delay = source.reconnect_delay(attempt);
            assert!(delay >= Duration::from_millis(base_seconds * 800));
            assert!(delay <= Duration::from_millis(base_seconds * 1_200));
        }
        assert_eq!(source.reconnect_delay(99), source.reconnect_delay(99));
        let capped = source.reconnect_delay(99);
        assert!(capped >= Duration::from_secs(48));
        assert!(capped <= Duration::from_secs(72));
    }

    #[test]
    fn preflight_rejects_only_an_effective_remote_command() {
        validate_preflight_output(b"hostname host.example\nremotecommand none\n")
            .expect("no conflict");
        assert_eq!(
            validate_preflight_output(b"remotecommand exec something\n"),
            Err(SshConfigurationError::RemoteCommandConflict)
        );
        assert_eq!(
            validate_preflight_output(&[0xff]),
            Err(SshConfigurationError::PreflightOutputNotUtf8)
        );
    }

    #[test]
    fn ssh_failures_are_stably_classified_without_returning_diagnostics() {
        let cases = [
            (
                b"ssh: connect to host x: Connection refused".as_slice(),
                SshFailureCategory::RetryableNetwork,
            ),
            (
                b"user@x: Permission denied (publickey).".as_slice(),
                SshFailureCategory::AuthenticationRequired,
            ),
            (
                b"WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!".as_slice(),
                SshFailureCategory::HostVerificationFailed,
            ),
            (
                b"sh: /home/private/.local/bin/aizu: No such file or directory".as_slice(),
                SshFailureCategory::MissingRemoteCli,
            ),
            (
                b"Cannot execute command-line and remote command.".as_slice(),
                SshFailureCategory::RemoteFailure,
            ),
        ];
        for (stderr, expected) in cases {
            assert_eq!(classify_ssh_failure(stderr), expected);
        }
        assert!(SshFailureCategory::RetryableNetwork.retry_automatically());
        assert!(!SshFailureCategory::AuthenticationRequired.retry_automatically());
    }
}
