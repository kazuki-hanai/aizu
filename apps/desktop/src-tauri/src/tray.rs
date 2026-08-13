use std::collections::BTreeMap;

use tauri::{
    App, AppHandle, Emitter, Manager, Wry,
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{TrayIcon, TrayIconBuilder},
};

use crate::{
    model::{
        AgentKind, AppView, LanguagePreference, RunningAgentView, SourceKind, SourceStatus,
        SourceView, TrayState,
    },
    state::DesktopState,
};

const TRAY_NORMAL: &[u8] = include_bytes!("../icons/tray/tray-normal@2x.png");
const TRAY_ATTENTION: &[u8] = include_bytes!("../icons/tray/tray-attention@2x.png");
const TRAY_PAUSED: &[u8] = include_bytes!("../icons/tray/tray-paused@2x.png");
const TRAY_ERROR: &[u8] = include_bytes!("../icons/tray/tray-error@2x.png");
const MAX_TRAY_AGENT_ROWS: usize = 24;

pub struct TrayUi {
    tray: TrayIcon<Wry>,
}

impl TrayUi {
    pub fn sync_from_state(&self, app: &AppHandle<Wry>) -> tauri::Result<()> {
        let app = app.clone();
        let tray = self.tray.clone();
        app.clone().run_on_main_thread(move || {
            let view = match app.state::<DesktopState>().lock() {
                Ok(state) => state.view(),
                Err(_) => return,
            };
            let state = view.tray_state;
            let language = view.preferences.language;
            let Ok(menu) = build_menu(&app, &view) else {
                return;
            };
            let _ = tray.set_tooltip(Some(state.tooltip(language)));
            let _ = icon_for(state).and_then(|icon| tray.set_icon(Some(icon)));
            let _ = tray.set_menu(Some(menu));
        })
    }
}

pub fn setup(app: &App<Wry>, initial_view: &AppView) -> tauri::Result<TrayUi> {
    let initial_state = initial_view.tray_state;
    let language = initial_view.preferences.language;
    let menu = build_menu(app, initial_view)?;

    let tray = TrayIconBuilder::with_id("aizu")
        .icon(icon_for(initial_state)?)
        .icon_as_template(true)
        .tooltip(initial_state.tooltip(language))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu_event(app, &event))
        .build(app)?;

    Ok(TrayUi { tray })
}

fn build_menu<M: Manager<Wry>>(app: &M, view: &AppView) -> tauri::Result<Menu<Wry>> {
    let language = view.preferences.language;
    let status = MenuItem::with_id(
        app,
        "status",
        view.tray_state.tooltip(language),
        false,
        None::<&str>,
    )?;
    let agents = submenu(
        app,
        "agents",
        &format!(
            "{} ({})",
            label(language, "Agents", "エージェント"),
            view.running_agents.len()
        ),
        &agent_menu_lines(&view.running_agents, language),
    )?;
    let sources = submenu(
        app,
        "sources",
        &format!(
            "{} ({})",
            label(language, "Sources", "接続元"),
            view.sources.len()
        ),
        &source_menu_lines(&view.sources, language),
    )?;
    let test = MenuItem::with_id(
        app,
        "test-notification",
        label(language, "Test notification", "テスト通知"),
        true,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(
        app,
        "open",
        label(language, "Open Aizu", "Aizuを開く"),
        true,
        None::<&str>,
    )?;
    let paused = CheckMenuItem::with_id(
        app,
        "paused",
        label(language, "Mute notifications", "通知をミュート"),
        true,
        view.tray_state == TrayState::Paused,
        None::<&str>,
    )?;
    let reconnect = MenuItem::with_id(
        app,
        "reconnect",
        label(language, "Reconnect all", "すべて再接続"),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        label(language, "Quit Aizu", "Aizuを終了"),
        true,
        None::<&str>,
    )?;
    Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &agents,
            &sources,
            &PredefinedMenuItem::separator(app)?,
            &test,
            &open,
            &paused,
            &reconnect,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

fn submenu<M: Manager<Wry>>(
    app: &M,
    id: &str,
    title: &str,
    lines: &[String],
) -> tauri::Result<Submenu<Wry>> {
    let submenu = Submenu::with_id(app, id, title, true)?;
    for line in lines {
        let item = MenuItem::new(app, line, false, None::<&str>)?;
        submenu.append(&item)?;
    }
    Ok(submenu)
}

fn agent_menu_lines(
    running_agents: &[RunningAgentView],
    language: LanguagePreference,
) -> Vec<String> {
    let mut counts = BTreeMap::<(String, String), usize>::new();
    for agent in running_agents {
        let source = if agent.source_kind == SourceKind::Local {
            label(language, "This Mac", "このMac").to_owned()
        } else {
            bounded_menu_text(&agent.source_name)
        };
        *counts
            .entry((agent_name(agent.agent).to_owned(), source))
            .or_default() += 1;
    }
    if counts.is_empty() {
        return vec![label(language, "No running agents", "実行中のエージェントなし").to_owned()];
    }
    let total_groups = counts.len();
    let mut lines: Vec<_> = counts
        .into_iter()
        .take(MAX_TRAY_AGENT_ROWS)
        .map(|((agent, source), count)| {
            if count == 1 {
                format!("{agent} - {source}")
            } else {
                format!("{agent} x{count} - {source}")
            }
        })
        .collect();
    if total_groups > MAX_TRAY_AGENT_ROWS {
        lines.push(format!(
            "… {} {}",
            total_groups - MAX_TRAY_AGENT_ROWS,
            label(language, "more", "件を省略")
        ));
    }
    lines
}

const fn agent_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "Codex",
        AgentKind::ClaudeCode => "Claude Code",
    }
}

fn source_menu_lines(sources: &[SourceView], language: LanguagePreference) -> Vec<String> {
    sources
        .iter()
        .take(33)
        .map(|source| {
            let name = if source.kind == SourceKind::Local {
                label(language, "This Mac", "このMac").to_owned()
            } else {
                bounded_menu_text(&source.name)
            };
            format!("{name} - {}", source_status(source.status, language))
        })
        .collect()
}

fn source_status(status: SourceStatus, language: LanguagePreference) -> &'static str {
    match (status, language.prefers_japanese()) {
        (SourceStatus::Connected, true) => "接続済み",
        (SourceStatus::Reconnecting, true) => "再接続中",
        (SourceStatus::Error, true) => "エラー",
        (SourceStatus::Disabled, true) => "無効",
        (SourceStatus::Connected, false) => "Connected",
        (SourceStatus::Reconnecting, false) => "Reconnecting",
        (SourceStatus::Error, false) => "Error",
        (SourceStatus::Disabled, false) => "Disabled",
    }
}

fn bounded_menu_text(value: &str) -> String {
    let mut text: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(60)
        .collect();
    if value
        .chars()
        .filter(|character| !character.is_control())
        .count()
        > 60
    {
        text.push_str("...");
    }
    text
}

fn label<'a>(language: LanguagePreference, english: &'a str, japanese: &'a str) -> &'a str {
    if language.prefers_japanese() {
        japanese
    } else {
        english
    }
}

fn icon_for(state: TrayState) -> tauri::Result<Image<'static>> {
    let bytes = match state {
        TrayState::Normal => TRAY_NORMAL,
        TrayState::Attention => TRAY_ATTENTION,
        TrayState::Paused => TRAY_PAUSED,
        TrayState::Error => TRAY_ERROR,
    };
    Image::from_bytes(bytes)
}

fn handle_menu_event(app: &AppHandle<Wry>, event: &tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "open" => show_main_window(app),
        "test-notification" => {
            if let Ok(mut state) = app.state::<DesktopState>().lock() {
                let _ = state.send_test_notification();
            }
        }
        "paused" => toggle_pause(app),
        "reconnect" => reconnect_all(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

fn reconnect_all(app: &AppHandle<Wry>) {
    let result = app
        .state::<DesktopState>()
        .lock()
        .and_then(|mut state| state.reconnect_all_remote_sources());

    if let Ok(view) = result {
        if let Some(tray) = app.try_state::<TrayUi>() {
            let _ = tray.sync_from_state(app);
        }
        let _ = app.emit("aizu://view-changed", view);
    }
}

pub fn show_main_window(app: &AppHandle<Wry>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn toggle_pause(app: &AppHandle<Wry>) {
    let result = app.state::<DesktopState>().lock().and_then(|mut state| {
        let paused = !state.view().paused;
        state.set_paused(paused)
    });

    if let Ok(view) = result {
        if let Some(tray) = app.try_state::<TrayUi>() {
            let _ = tray.sync_from_state(app);
        }
        let _ = app.emit("aizu://view-changed", view);
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{
        AgentKind, LanguagePreference, RunningAgentView, SourceKind, SourceStatus, SourceView,
    };

    use super::{MAX_TRAY_AGENT_ROWS, agent_menu_lines, bounded_menu_text, source_menu_lines};

    #[test]
    fn agent_menu_groups_instances_by_agent_and_source() {
        let agents = vec![
            RunningAgentView {
                agent: AgentKind::Codex,
                label: "Codex".to_owned(),
                source_id: "local".to_owned(),
                source_name: "This Mac".to_owned(),
                source_kind: SourceKind::Local,
            },
            RunningAgentView {
                agent: AgentKind::Codex,
                label: "Codex".to_owned(),
                source_id: "local".to_owned(),
                source_name: "This Mac".to_owned(),
                source_kind: SourceKind::Local,
            },
            RunningAgentView {
                agent: AgentKind::ClaudeCode,
                label: "Claude Code".to_owned(),
                source_id: "ssh:mini-pc".to_owned(),
                source_name: "Mini PC".to_owned(),
                source_kind: SourceKind::RemoteSsh,
            },
        ];

        assert_eq!(
            agent_menu_lines(&agents, LanguagePreference::English),
            vec!["Claude Code - Mini PC", "Codex x2 - This Mac"]
        );
        assert_eq!(
            agent_menu_lines(&agents[..2], LanguagePreference::Japanese),
            vec!["Codex x2 - このMac"]
        );
    }

    #[test]
    fn source_menu_shows_only_local_labels_and_safe_states() {
        let sources = vec![
            SourceView {
                id: "local".to_owned(),
                name: "This Mac".to_owned(),
                kind: SourceKind::Local,
                status: SourceStatus::Connected,
                detail: "private diagnostic must not appear".to_owned(),
                last_event_at: None,
                action_required: None,
            },
            SourceView {
                id: "ssh:mini-pc".to_owned(),
                name: "Mini PC".to_owned(),
                kind: SourceKind::RemoteSsh,
                status: SourceStatus::Reconnecting,
                detail: "raw SSH error must not appear".to_owned(),
                last_event_at: None,
                action_required: None,
            },
        ];

        assert_eq!(
            source_menu_lines(&sources, LanguagePreference::Japanese),
            vec!["このMac - 接続済み", "Mini PC - 再接続中"]
        );
    }

    #[test]
    fn menu_text_strips_controls_and_is_bounded() {
        let value = format!("A\n{}", "b".repeat(80));
        let output = bounded_menu_text(&value);

        assert!(!output.contains('\n'));
        assert_eq!(output.chars().count(), 63);
        assert!(output.ends_with("..."));
    }

    #[test]
    fn agent_menu_has_a_global_row_cap_and_accurate_overflow() {
        let agents: Vec<_> = (0..32)
            .flat_map(|source| {
                [AgentKind::Codex, AgentKind::ClaudeCode].map(|agent| RunningAgentView {
                    agent,
                    label: format!("untrusted instance {source}"),
                    source_id: format!("ssh:source-{source}"),
                    source_name: format!("Source {source}"),
                    source_kind: SourceKind::RemoteSsh,
                })
            })
            .collect();

        let lines = agent_menu_lines(&agents, LanguagePreference::English);
        assert_eq!(lines.len(), MAX_TRAY_AGENT_ROWS + 1);
        assert_eq!(lines.last().map(String::as_str), Some("… 40 more"));
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("untrusted instance"))
        );
    }
}
