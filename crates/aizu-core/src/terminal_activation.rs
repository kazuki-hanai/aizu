use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::NormalizedEvent;

/// Reserved metadata key populated by the trusted Aizu CLI process.
pub const TERMINAL_ACTIVATION_METADATA_KEY: &str = "aizu_terminal_activation";

const MAX_SESSION_ID_CHARS: usize = 200;
const MAX_TMUX_LABEL_CHARS: usize = 64;

/// A supported terminal application that Aizu may activate with a fixed adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalApplication {
    Iterm2,
    AppleTerminal,
    WezTerm,
    Ghostty,
    Warp,
    Kitty,
    VisualStudioCode,
}

/// A tmux pane on a named server in the current user's standard socket directory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TmuxActivation {
    pub socket_name: String,
    pub pane_id: String,
}

/// Privacy-safe instructions for returning to the terminal that emitted an event.
///
/// Values are identifiers only. Absolute paths, commands, environment values, and
/// arbitrary application bundle identifiers are deliberately not represented.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalActivation {
    pub application: TerminalApplication,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxActivation>,
}

impl TerminalActivation {
    /// Captures an activation target from a small allowlist of inherited variables.
    #[must_use]
    pub fn capture(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let (application, application_session) = capture_application(&get)?;
        let tmux = capture_tmux(&get);
        let activation = Self {
            application,
            application_session,
            tmux,
        };
        activation.is_valid().then_some(activation)
    }

    /// Parses a stored metadata value and rejects values outside the fixed contract.
    #[must_use]
    pub fn from_metadata(value: &Value) -> Option<Self> {
        let activation = serde_json::from_value::<Self>(value.clone()).ok()?;
        activation.is_valid().then_some(activation)
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        let session_valid = match (self.application, self.application_session.as_deref()) {
            (TerminalApplication::Iterm2, Some(session)) => valid_iterm_session(session),
            (TerminalApplication::WezTerm, Some(pane)) => valid_numeric_id(pane),
            (
                TerminalApplication::AppleTerminal
                | TerminalApplication::Ghostty
                | TerminalApplication::Warp
                | TerminalApplication::Kitty
                | TerminalApplication::VisualStudioCode
                | TerminalApplication::Iterm2
                | TerminalApplication::WezTerm,
                None,
            ) => true,
            _ => false,
        };
        session_valid && self.tmux.as_ref().is_none_or(valid_tmux)
    }
}

/// Removes receiver-local activation data before SSH output or durable remote ingest.
pub fn remove_terminal_activation_metadata(event: &mut NormalizedEvent) {
    if let Some(metadata) = event.metadata.as_mut() {
        metadata.remove(TERMINAL_ACTIVATION_METADATA_KEY);
        if metadata.is_empty() {
            event.metadata = None;
        }
    }
}

fn capture_application(
    get: &impl Fn(&str) -> Option<String>,
) -> Option<(TerminalApplication, Option<String>)> {
    if let Some(session) = get("ITERM_SESSION_ID").filter(|value| valid_iterm_session(value)) {
        return Some((TerminalApplication::Iterm2, Some(session)));
    }
    if let Some(pane) = get("WEZTERM_PANE").filter(|value| valid_numeric_id(value)) {
        return Some((TerminalApplication::WezTerm, Some(pane)));
    }

    let term_program = get("TERM_PROGRAM").unwrap_or_default();
    let application = match term_program.as_str() {
        "Apple_Terminal" => TerminalApplication::AppleTerminal,
        "iTerm.app" => TerminalApplication::Iterm2,
        "WezTerm" => TerminalApplication::WezTerm,
        "ghostty" | "Ghostty" => TerminalApplication::Ghostty,
        "WarpTerminal" | "Warp" => TerminalApplication::Warp,
        "kitty" => TerminalApplication::Kitty,
        "vscode" => TerminalApplication::VisualStudioCode,
        _ if get("TERM_SESSION_ID").is_some() => TerminalApplication::AppleTerminal,
        _ if get("KITTY_WINDOW_ID").is_some() => TerminalApplication::Kitty,
        _ => return None,
    };
    Some((application, None))
}

fn capture_tmux(get: &impl Fn(&str) -> Option<String>) -> Option<TmuxActivation> {
    let pane_id = get("TMUX_PANE").filter(|value| valid_tmux_pane(value))?;
    let socket = get("TMUX")?;
    let socket_path = Path::new(socket.split(',').next()?);
    let socket_name = socket_path.file_name()?.to_str()?.to_owned();
    let owner_directory = socket_path.parent()?.file_name()?.to_str()?;
    if !owner_directory
        .strip_prefix("tmux-")
        .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|byte| byte.is_ascii_digit()))
        || !valid_tmux_socket_name(&socket_name)
    {
        return None;
    }
    Some(TmuxActivation {
        socket_name,
        pane_id,
    })
}

fn valid_iterm_session(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_SESSION_ID_CHARS
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-'))
}

fn valid_numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_tmux_pane(value: &str) -> bool {
    value.strip_prefix('%').is_some_and(valid_numeric_id)
}

fn valid_tmux_socket_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_TMUX_LABEL_CHARS
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_tmux(value: &TmuxActivation) -> bool {
    valid_tmux_socket_name(&value.socket_name) && valid_tmux_pane(&value.pane_id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{
        TERMINAL_ACTIVATION_METADATA_KEY, TerminalActivation, TerminalApplication, TmuxActivation,
        remove_terminal_activation_metadata,
    };

    fn capture(values: &[(&str, &str)]) -> Option<TerminalActivation> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        TerminalActivation::capture(|key| values.get(key).cloned())
    }

    #[test]
    fn captures_iterm_and_standard_tmux_identifiers_without_socket_path() {
        let activation = capture(&[
            ("TERM_PROGRAM", "tmux"),
            ("ITERM_SESSION_ID", "w0t1p0:0123-ABCD"),
            ("TMUX_PANE", "%17"),
            ("TMUX", "/private/tmp/tmux-501/work,123,0"),
        ])
        .expect("activation should be captured");

        assert_eq!(activation.application, TerminalApplication::Iterm2);
        assert_eq!(
            activation.application_session.as_deref(),
            Some("w0t1p0:0123-ABCD")
        );
        assert_eq!(
            activation.tmux,
            Some(TmuxActivation {
                socket_name: "work".to_owned(),
                pane_id: "%17".to_owned(),
            })
        );
        let serialized = serde_json::to_string(&activation).expect("activation JSON");
        assert!(!serialized.contains("/private/tmp"));
    }

    #[test]
    fn captures_wezterm_and_known_terminal_fallbacks() {
        let wezterm = capture(&[("WEZTERM_PANE", "42")]).expect("WezTerm pane");
        assert_eq!(wezterm.application, TerminalApplication::WezTerm);
        assert_eq!(wezterm.application_session.as_deref(), Some("42"));

        let terminal = capture(&[("TERM_PROGRAM", "Apple_Terminal")]).expect("Terminal app");
        assert_eq!(terminal.application, TerminalApplication::AppleTerminal);
        assert!(terminal.application_session.is_none());
    }

    #[test]
    fn rejects_paths_commands_and_unknown_metadata_fields() {
        assert!(capture(&[("ITERM_SESSION_ID", "../../Applications/Bad.app")]).is_none());
        assert!(capture(&[("WEZTERM_PANE", "1;open -a Calculator")]).is_none());
        assert!(
            capture(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TMUX_PANE", "%1"),
                ("TMUX", "/private/tmp/tmux-501/..,1,0"),
            ])
            .expect("iTerm fallback remains")
            .tmux
            .is_none()
        );
        assert!(
            capture(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TMUX_PANE", "%1"),
                ("TMUX", "/Users/alice/custom.sock,1,0"),
            ])
            .expect("iTerm fallback remains")
            .tmux
            .is_none()
        );
        assert!(
            TerminalActivation::from_metadata(&json!({
                "application": "iterm2",
                "applicationSession": "w0t0p0:ABC",
                "command": "rm -rf /"
            }))
            .is_none()
        );
    }

    #[test]
    fn removing_activation_preserves_unrelated_metadata() {
        let mut event = crate::EmitRequest {
            kind: Some(crate::EventKind::TaskCompleted),
            title: Some("Done".to_owned()),
            metadata: Some(serde_json::json!({
                "aizu_adapter": "codex-v1",
                TERMINAL_ACTIVATION_METADATA_KEY: {
                    "application": "iterm2",
                    "application_session": "w0t0p0:ABCD"
                }
            })),
            ..crate::EmitRequest::default()
        }
        .normalize(
            uuid::Uuid::parse_str("7a4881c7-c667-47dc-b544-f98a46ab17ca").expect("UUID"),
            "local".to_owned(),
            None,
        )
        .expect("event");

        remove_terminal_activation_metadata(&mut event);

        assert_eq!(
            event
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("aizu_adapter")),
            Some(&serde_json::json!("codex-v1"))
        );
        assert!(
            event
                .metadata
                .as_ref()
                .is_none_or(|metadata| !metadata.contains_key(TERMINAL_ACTIVATION_METADATA_KEY))
        );
    }
}
