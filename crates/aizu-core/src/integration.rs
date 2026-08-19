use std::path::Path;

use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::AgentKind;

const HOOK_TIMEOUT_SECONDS: u8 = 5;

/// Builds a first-party user hook configuration without reading or modifying
/// the agent's configuration files.
///
/// The desktop installer must merge this value with structured JSON after an
/// explicit user action. Existing hooks must never be replaced wholesale.
pub fn hook_configuration(agent: AgentKind, aizu_path: &Path) -> Result<Value, IntegrationError> {
    let executable = validate_executable_path(aizu_path)?;
    Ok(match agent {
        AgentKind::Codex => codex_configuration(&executable),
        AgentKind::ClaudeCode => claude_code_configuration(&executable),
    })
}

/// Merges Aizu's required hooks into an existing agent JSON configuration.
///
/// Existing top-level values, event groups, and non-Aizu handlers are retained. Reapplying the
/// merge is idempotent and never replaces an event's complete hook array.
pub fn merge_hook_configuration(
    agent: AgentKind,
    existing: &Value,
    aizu_path: &Path,
) -> Result<Value, IntegrationError> {
    validate_executable_path(aizu_path)?;
    let mut merged = existing
        .as_object()
        .cloned()
        .ok_or(IntegrationError::ConfigurationRootMustBeObject)?;
    let generated = hook_configuration(agent, aizu_path)?;
    let expected_hooks = generated
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or(IntegrationError::GeneratedConfigurationInvalid)?;
    let actual_hooks = merged
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(IntegrationError::HooksMustBeObject)?;

    for (event, expected_groups) in expected_hooks {
        let expected_groups = expected_groups
            .as_array()
            .ok_or(IntegrationError::GeneratedConfigurationInvalid)?;
        let actual_groups = actual_hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or(IntegrationError::HookEventMustBeArray)?;
        remove_generated_aizu_handlers(actual_groups, agent, event);
        for expected_group in expected_groups {
            let expected_handlers: Vec<_> = hook_handlers(expected_group).collect();
            let already_configured = expected_handlers.iter().all(|expected| {
                actual_groups
                    .iter()
                    .flat_map(hook_handlers)
                    .any(|actual| actual == *expected)
            });
            if !already_configured {
                actual_groups.push(expected_group.clone());
            }
        }
    }
    Ok(Value::Object(merged))
}

fn remove_generated_aizu_handlers(groups: &mut Vec<Value>, agent: AgentKind, event: &str) {
    for group in groups.iter_mut() {
        if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            handlers.retain(|handler| !is_generated_aizu_handler(handler, agent, event));
        }
    }
    groups.retain(|group| hook_handlers(group).next().is_some());
}

fn is_generated_aizu_handler(handler: &Value, agent: AgentKind, event: &str) -> bool {
    let Some(handler) = handler.as_object() else {
        return false;
    };
    if handler.get("type").and_then(Value::as_str) != Some("command")
        || handler.get("timeout").and_then(Value::as_u64) != Some(u64::from(HOOK_TIMEOUT_SECONDS))
    {
        return false;
    }

    let agent_name = match agent {
        AgentKind::Codex => "codex",
        AgentKind::ClaudeCode => "claude-code",
    };
    let Some(command) = handler.get("command").and_then(Value::as_str) else {
        return false;
    };
    if let Some(executable) =
        command.strip_suffix(&format!(" hook --agent {agent_name} --event {event}"))
        && canonical_quoted_aizu_executable(executable)
    {
        return true;
    }

    is_aizu_executable_path(command)
        && handler.get("args") == Some(&json!(["hook", "--agent", agent_name, "--event", event]))
}

fn canonical_quoted_aizu_executable(value: &str) -> bool {
    let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    else {
        return false;
    };
    let executable = inner.replace("'\"'\"'", "'");
    shell_quote(&executable) == value && is_aizu_executable_path(&executable)
}

fn is_aizu_executable_path(value: &str) -> bool {
    !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
        && Path::new(value)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "aizu" || name.eq_ignore_ascii_case("aizu.exe"))
}

fn hook_handlers(group: &Value) -> impl Iterator<Item = &Value> {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn validate_executable_path(path: &Path) -> Result<String, IntegrationError> {
    if !path.is_absolute() {
        return Err(IntegrationError::ExecutableMustBeAbsolute);
    }
    let value = path
        .to_str()
        .ok_or(IntegrationError::ExecutablePathNotUtf8)?;
    if value.chars().any(char::is_control) {
        return Err(IntegrationError::ExecutablePathContainsControl);
    }
    Ok(value.to_owned())
}

fn codex_configuration(executable: &str) -> Value {
    let command = |event: &str| {
        format!(
            "{} hook --agent codex --event {event}",
            shell_quote(executable)
        )
    };
    json!({
        "description": "Aizu lifecycle notifications",
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": command("Stop"),
                    "timeout": HOOK_TIMEOUT_SECONDS
                }]
            }],
            "PermissionRequest": [{
                "hooks": [{
                    "type": "command",
                    "command": command("PermissionRequest"),
                    "timeout": HOOK_TIMEOUT_SECONDS
                }]
            }]
        }
    })
}

fn claude_code_configuration(executable: &str) -> Value {
    let handler = |event: &str| {
        json!({
            "type": "command",
            "command": format!(
                "{} hook --agent claude-code --event {event}",
                shell_quote(executable)
            ),
            "timeout": HOOK_TIMEOUT_SECONDS
        })
    };
    json!({
        "hooks": {
            "Stop": [{"hooks": [handler("Stop")]}],
            "StopFailure": [{"hooks": [handler("StopFailure")]}],
            "PermissionRequest": [{"hooks": [handler("PermissionRequest")]}]
        }
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("the Aizu hook executable path must be absolute")]
    ExecutableMustBeAbsolute,
    #[error("the Aizu hook executable path must be valid UTF-8")]
    ExecutablePathNotUtf8,
    #[error("the Aizu hook executable path contains a control character")]
    ExecutablePathContainsControl,
    #[error("the existing agent configuration root must be a JSON object")]
    ConfigurationRootMustBeObject,
    #[error("the existing agent hooks value must be a JSON object")]
    HooksMustBeObject,
    #[error("an existing agent hook event value must be a JSON array")]
    HookEventMustBeArray,
    #[error("the generated agent hook configuration is invalid")]
    GeneratedConfigurationInvalid,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn codex_configures_completion_and_permission_as_bounded_sync_hooks() {
        let config = hook_configuration(AgentKind::Codex, Path::new("/Users/me/.local/bin/aizu"))
            .expect("configuration");
        assert_eq!(config["hooks"]["Stop"][0]["hooks"][0]["timeout"], 5);
        assert_eq!(
            config["hooks"]["PermissionRequest"][0]["hooks"][0].get("async"),
            None
        );
        let serialized = config.to_string();
        assert!(serialized.contains("--agent codex --event Stop"));
        assert!(!serialized.contains("StopFailure"));
    }

    #[test]
    fn claude_code_uses_compatible_shell_commands_for_all_supported_hooks() {
        let config = hook_configuration(AgentKind::ClaudeCode, Path::new("/opt/aizu bin/aizu"))
            .expect("configuration");
        assert_eq!(
            config["hooks"]["StopFailure"][0]["hooks"][0]["command"],
            "'/opt/aizu bin/aizu' hook --agent claude-code --event StopFailure"
        );
        assert_eq!(
            config["hooks"]["PermissionRequest"][0]["hooks"][0].get("args"),
            None
        );
        assert_eq!(
            config["hooks"]["PermissionRequest"][0]["hooks"][0].get("async"),
            None
        );
    }

    #[test]
    fn codex_shell_command_quotes_apostrophes_and_spaces() {
        let config = hook_configuration(AgentKind::Codex, Path::new("/Users/O'Brien/Aizu CLI"))
            .expect("configuration");
        assert_eq!(
            config["hooks"]["Stop"][0]["hooks"][0]["command"],
            "'/Users/O'\"'\"'Brien/Aizu CLI' hook --agent codex --event Stop"
        );
    }

    #[test]
    fn relative_and_control_character_paths_are_rejected() {
        assert!(matches!(
            hook_configuration(AgentKind::Codex, Path::new("aizu")),
            Err(IntegrationError::ExecutableMustBeAbsolute)
        ));
        assert!(matches!(
            hook_configuration(AgentKind::ClaudeCode, Path::new("/tmp/aizu\nother")),
            Err(IntegrationError::ExecutablePathContainsControl)
        ));
    }

    #[test]
    fn merge_preserves_existing_hooks_and_is_idempotent() {
        let existing = json!({
            "theme": "dark",
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "other-tool"}]}],
                "SessionStart": [{"hooks": [{"type": "command", "command": "startup"}]}]
            }
        });
        let path = Path::new("/Users/me/.local/bin/aizu");
        let merged =
            merge_hook_configuration(AgentKind::ClaudeCode, &existing, path).expect("merge hooks");
        let repeated =
            merge_hook_configuration(AgentKind::ClaudeCode, &merged, path).expect("repeat merge");

        assert_eq!(merged, repeated);
        assert_eq!(merged["theme"], "dark");
        assert_eq!(
            merged["hooks"]["SessionStart"],
            existing["hooks"]["SessionStart"]
        );
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(merged["hooks"]["PermissionRequest"].is_array());
        assert!(merged["hooks"]["StopFailure"].is_array());
    }

    #[test]
    fn merge_migrates_old_aizu_handlers_without_touching_other_hooks() {
        let path = Path::new("/Users/me/.local/bin/aizu");
        let old_claude = json!({
            "hooks": {
                "Stop": [{"hooks": [
                    {
                        "type": "command",
                        "command": "/Users/me/.local/bin/aizu",
                        "args": ["hook", "--agent", "claude-code", "--event", "Stop"],
                        "timeout": 5,
                        "async": true
                    },
                    {"type": "command", "command": "other-tool"}
                ]}]
            }
        });
        let claude = merge_hook_configuration(AgentKind::ClaudeCode, &old_claude, path)
            .expect("migrate Claude hooks");
        let claude_handlers: Vec<_> = claude["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(hook_handlers)
            .collect();
        assert_eq!(claude_handlers.len(), 2);
        assert!(
            claude_handlers
                .iter()
                .any(|handler| handler["command"] == "other-tool")
        );
        assert!(claude_handlers.iter().any(|handler| {
            handler["command"]
                == "'/Users/me/.local/bin/aizu' hook --agent claude-code --event Stop"
                && handler.get("args").is_none()
                && handler.get("async").is_none()
        }));

        let old_full_command_claude = json!({
            "hooks": {
                "Stop": [{"hooks": [{
                    "type": "command",
                    "command": "'/Users/me/.local/bin/aizu' hook --agent claude-code --event Stop",
                    "timeout": 5,
                    "async": true
                }]}]
            }
        });
        let migrated_full_command =
            merge_hook_configuration(AgentKind::ClaudeCode, &old_full_command_claude, path)
                .expect("migrate full-command Claude hook");
        let migrated_handlers: Vec<_> = migrated_full_command["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(hook_handlers)
            .collect();
        assert_eq!(migrated_handlers.len(), 1);
        assert!(migrated_handlers[0].get("async").is_none());

        let old_codex = json!({
            "hooks": {
                "Stop": [{"hooks": [{
                    "type": "command",
                    "command": "'/Users/me/.local/bin/aizu' hook --agent codex --event Stop",
                    "timeout": 5,
                    "async": true
                }]}]
            }
        });
        let codex = merge_hook_configuration(AgentKind::Codex, &old_codex, path)
            .expect("migrate Codex hooks");
        let codex_handlers: Vec<_> = codex["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(hook_handlers)
            .collect();
        assert_eq!(codex_handlers.len(), 1);
        assert!(codex_handlers[0].get("async").is_none());
        assert_eq!(
            merge_hook_configuration(AgentKind::Codex, &codex, path).expect("repeat migration"),
            codex
        );
    }

    #[test]
    fn merge_replaces_generated_handlers_from_other_aizu_install_paths() {
        let managed_path = Path::new("/Users/me/.local/bin/aizu");
        let existing = json!({
            "hooks": {
                "Stop": [
                    {"hooks": [{
                        "type": "command",
                        "command": "'/Users/me/project/target/debug/aizu' hook --agent codex --event Stop",
                        "timeout": 5
                    }]},
                    {"hooks": [{
                        "type": "command",
                        "command": "'/Applications/Aizu.app/Contents/Resources/bin/aizu' hook --agent codex --event Stop",
                        "timeout": 5
                    }]},
                    {"hooks": [{
                        "type": "command",
                        "command": "'/Users/me/.local/bin/aizu' hook --agent codex --event Stop",
                        "timeout": 5
                    }]},
                    {"hooks": [{
                        "type": "command",
                        "command": "other-tool",
                        "timeout": 5
                    }]},
                    {"hooks": [{
                        "type": "command",
                        "command": "'/Users/me/.local/bin/not-aizu' hook --agent codex --event Stop",
                        "timeout": 5
                    }]}
                ]
            }
        });

        let merged = merge_hook_configuration(AgentKind::Codex, &existing, managed_path)
            .expect("normalize Aizu handlers");
        let handlers: Vec<_> = merged["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(hook_handlers)
            .collect();

        assert_eq!(handlers.len(), 3);
        assert!(handlers.iter().any(|handler| {
            handler["command"] == "'/Users/me/.local/bin/aizu' hook --agent codex --event Stop"
        }));
        assert!(
            handlers
                .iter()
                .any(|handler| handler["command"] == "other-tool")
        );
        assert!(handlers.iter().any(|handler| {
            handler["command"] == "'/Users/me/.local/bin/not-aizu' hook --agent codex --event Stop"
        }));
    }

    #[test]
    fn merge_rejects_incompatible_existing_hook_shapes() {
        assert!(matches!(
            merge_hook_configuration(
                AgentKind::Codex,
                &json!({"hooks": []}),
                Path::new("/opt/aizu")
            ),
            Err(IntegrationError::HooksMustBeObject)
        ));
    }
}
