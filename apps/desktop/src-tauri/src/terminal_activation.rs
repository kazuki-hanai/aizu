use std::{
    ffi::OsString,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use aizu_core::{TerminalActivation, TerminalApplication, TmuxActivation};
use thiserror::Error;

#[cfg(feature = "desktop-e2e")]
use std::sync::atomic::{AtomicUsize, Ordering};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(2);
const OPEN: &str = "/usr/bin/open";

#[cfg(feature = "desktop-e2e")]
static E2E_ACTIVATION_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
pub enum TerminalActivationError {
    #[error("the terminal activation target is invalid")]
    InvalidTarget,
    #[error("the terminal application could not be activated")]
    ApplicationUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandSpec {
    executable: PathBuf,
    arguments: Vec<OsString>,
}

pub fn activate(target: &TerminalActivation) -> Result<(), TerminalActivationError> {
    if !target.is_valid() {
        return Err(TerminalActivationError::InvalidTarget);
    }
    #[cfg(feature = "desktop-e2e")]
    {
        E2E_ACTIVATION_COUNT.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
    #[cfg(not(feature = "desktop-e2e"))]
    {
        activate_platform(target)
    }
}

#[cfg(not(feature = "desktop-e2e"))]
fn activate_platform(target: &TerminalActivation) -> Result<(), TerminalActivationError> {
    if let Some(tmux) = &target.tmux {
        let _ = tmux_command(tmux).is_some_and(|spec| run_bounded(&spec));
    }

    if let Some(exact) = exact_application_command(target)
        && run_bounded(&exact)
    {
        return Ok(());
    }
    let fallback = application_fallback(target.application);
    run_bounded(&fallback)
        .then_some(())
        .ok_or(TerminalActivationError::ApplicationUnavailable)
}

#[cfg(feature = "desktop-e2e")]
pub fn e2e_activation_count() -> usize {
    E2E_ACTIVATION_COUNT.load(Ordering::Acquire)
}

fn exact_application_command(target: &TerminalActivation) -> Option<CommandSpec> {
    match (target.application, target.application_session.as_deref()) {
        (TerminalApplication::Iterm2, Some(session)) => Some(CommandSpec {
            executable: OPEN.into(),
            arguments: vec![
                format!("iterm2:///reveal?sessionid={}", percent_encode(session)).into(),
            ],
        }),
        (TerminalApplication::WezTerm, Some(pane)) => {
            let executable = first_existing(&[
                "/Applications/WezTerm.app/Contents/MacOS/wezterm",
                "/opt/homebrew/bin/wezterm",
                "/usr/local/bin/wezterm",
            ])?;
            Some(CommandSpec {
                executable,
                arguments: ["cli", "activate-pane", "--pane-id", pane]
                    .into_iter()
                    .map(OsString::from)
                    .collect(),
            })
        }
        _ => None,
    }
}

fn application_fallback(application: TerminalApplication) -> CommandSpec {
    let bundle_identifier = match application {
        TerminalApplication::Iterm2 => "com.googlecode.iterm2",
        TerminalApplication::AppleTerminal => "com.apple.Terminal",
        TerminalApplication::WezTerm => "com.github.wez.wezterm",
        TerminalApplication::Ghostty => "com.mitchellh.ghostty",
        TerminalApplication::Warp => "dev.warp.Warp-Stable",
        TerminalApplication::Kitty => "net.kovidgoyal.kitty",
        TerminalApplication::VisualStudioCode => "com.microsoft.VSCode",
    };
    CommandSpec {
        executable: OPEN.into(),
        arguments: ["-b", bundle_identifier]
            .into_iter()
            .map(OsString::from)
            .collect(),
    }
}

fn tmux_command(target: &TmuxActivation) -> Option<CommandSpec> {
    let executable = first_existing(&[
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
        "/usr/bin/tmux",
    ])?;
    Some(CommandSpec {
        executable,
        arguments: [
            "-L",
            target.socket_name.as_str(),
            "select-window",
            "-t",
            target.pane_id.as_str(),
            ";",
            "select-pane",
            "-t",
            target.pane_id.as_str(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect(),
    })
}

fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

fn run_bounded(spec: &CommandSpec) -> bool {
    let Ok(mut child) = Command::new(&spec.executable)
        .args(&spec.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < ACTIVATION_TIMEOUT => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        path::Path,
    };

    use aizu_core::{TerminalActivation, TerminalApplication, TmuxActivation};

    use super::{application_fallback, exact_application_command, percent_encode, tmux_command};

    #[test]
    fn iterm_reveal_uses_a_fixed_url_command_and_encodes_the_session() {
        let spec = exact_application_command(&TerminalActivation {
            application: TerminalApplication::Iterm2,
            application_session: Some("w0t0p0:ABCD-EFGH".to_owned()),
            tmux: None,
        })
        .expect("iTerm exact command");

        assert_eq!(spec.executable, Path::new("/usr/bin/open"));
        assert_eq!(
            spec.arguments,
            [OsStr::new("iterm2:///reveal?sessionid=w0t0p0%3AABCD-EFGH")]
        );
        assert_eq!(percent_encode("a:b"), "a%3Ab");
    }

    #[test]
    fn tmux_selection_is_fixed_argv_without_a_shell() {
        let Some(spec) = tmux_command(&TmuxActivation {
            socket_name: "work".to_owned(),
            pane_id: "%17".to_owned(),
        }) else {
            return;
        };
        assert!(spec.executable.ends_with("tmux"));
        assert_eq!(
            spec.arguments,
            [
                "-L",
                "work",
                "select-window",
                "-t",
                "%17",
                ";",
                "select-pane",
                "-t",
                "%17"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn fallback_bundle_identifiers_are_not_event_controlled() {
        let terminal = application_fallback(TerminalApplication::AppleTerminal);
        assert_eq!(terminal.executable, Path::new("/usr/bin/open"));
        assert_eq!(
            terminal.arguments,
            ["-b", "com.apple.Terminal"].map(OsString::from)
        );
    }
}
