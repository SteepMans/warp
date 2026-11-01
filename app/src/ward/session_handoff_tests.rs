use super::*;
use crate::app_state::{BranchSnapshot, LeafSnapshot, PaneFlex, SplitDirection};
use crate::tab::SelectedTabColor;
use crate::terminal::shell::ShellType;

fn terminal_leaf(cwd: Option<&str>, resume: Option<&str>, focused: bool) -> PaneNodeSnapshot {
    PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: focused,
        custom_vertical_tabs_title: None,
        contents: LeafContents::Terminal(TerminalPaneSnapshot {
            uuid: Vec::new(),
            cwd: cwd.map(str::to_owned),
            shell_launch_data: None,
            is_active: focused,
            is_read_only: false,
            input_config: None,
            llm_model_override: None,
            active_profile_id: None,
            conversation_ids_to_restore: Vec::new(),
            active_conversation_id: None,
            cli_agent_resume_command: resume.map(str::to_owned),
        }),
    })
}

/// A leaf that is not a terminal, so it carries neither a directory nor an agent session.
fn editor_leaf() -> PaneNodeSnapshot {
    PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: true,
        custom_vertical_tabs_title: None,
        contents: LeafContents::CustomRouterEditor,
    })
}

fn branch(children: Vec<PaneNodeSnapshot>) -> PaneNodeSnapshot {
    PaneNodeSnapshot::Branch(BranchSnapshot {
        direction: SplitDirection::Vertical,
        children: children
            .into_iter()
            .map(|child| (PaneFlex(1.), child))
            .collect(),
    })
}

fn tab(title: Option<&str>, root: PaneNodeSnapshot) -> TabSnapshot {
    TabSnapshot {
        custom_title: title.map(str::to_owned),
        root,
        default_directory_color: None,
        selected_color: SelectedTabColor::default(),
        left_panel: None,
        right_panel: None,
        group_id: None,
        pinned: false,
    }
}

fn window(tabs: Vec<TabSnapshot>, active_tab_index: usize, quake_mode: bool) -> WindowSnapshot {
    WindowSnapshot {
        tabs,
        active_tab_index,
        team_uid: None,
        bounds: None,
        fullscreen_state: Default::default(),
        quake_mode,
        universal_search_width: None,
        warp_ai_width: None,
        voltron_width: None,
        warp_drive_index_width: None,
        left_panel_open: false,
        vertical_tabs_panel_open: false,
        left_panel_width: None,
        right_panel_width: None,
        agent_management_filters: None,
        tab_groups: Vec::new(),
    }
}

fn app_state(windows: Vec<WindowSnapshot>) -> AppState {
    AppState {
        windows,
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Vec::new(),
    }
}

fn handoff_tab(cwd: &str) -> HandoffTab {
    HandoffTab {
        title: None,
        cwd: Some(cwd.to_owned()),
        shell: None,
        resume_command: None,
    }
}

fn saved(windows: Vec<HandoffWindow>, written_at_ms: u64) -> Handoff {
    Handoff {
        version: FORMAT_VERSION,
        written_at_ms,
        from_version: Some("v1".to_owned()),
        windows,
    }
}

#[test]
fn the_focused_pane_stands_for_a_split_tab() {
    let root = branch(vec![
        terminal_leaf(Some("/first"), None, false),
        terminal_leaf(Some("/focused"), Some("claude --resume abc"), true),
    ]);
    let terminal = tab_terminal(&root).expect("the tab has terminals");
    assert_eq!(terminal.cwd.as_deref(), Some("/focused"));
    assert_eq!(
        terminal.cli_agent_resume_command.as_deref(),
        Some("claude --resume abc")
    );
}

#[test]
fn without_a_focused_pane_the_first_one_stands_for_the_tab() {
    let root = branch(vec![
        terminal_leaf(Some("/first"), None, false),
        terminal_leaf(Some("/second"), None, false),
    ]);
    let terminal = tab_terminal(&root).expect("the tab has terminals");
    assert_eq!(terminal.cwd.as_deref(), Some("/first"));
}

#[test]
fn a_tab_without_a_terminal_has_nothing_to_hand_off() {
    assert!(tab_terminal(&editor_leaf()).is_none());
    assert!(tab_terminal(&branch(vec![editor_leaf()])).is_none());
}

#[test]
fn the_handoff_keeps_titles_directories_and_resume_commands() {
    let state = app_state(vec![window(
        vec![
            tab(
                Some("psn. brain-manager-bot"),
                terminal_leaf(
                    Some("C:\\work\\other\\brain-manager-bot"),
                    Some("claude --resume abc"),
                    true,
                ),
            ),
            tab(None, terminal_leaf(Some("C:\\work\\hassle"), None, false)),
        ],
        1,
        false,
    )]);

    let handoff = from_app_state(&state, 1_000, Some("v1".to_owned()));

    assert_eq!(handoff.version, FORMAT_VERSION);
    assert_eq!(handoff.written_at_ms, 1_000);
    assert_eq!(handoff.windows.len(), 1);
    assert_eq!(handoff.windows[0].active_tab_index, 1);
    assert_eq!(
        handoff.windows[0].tabs,
        vec![
            HandoffTab {
                title: Some("psn. brain-manager-bot".to_owned()),
                cwd: Some("C:\\work\\other\\brain-manager-bot".to_owned()),
                shell: None,
                resume_command: Some("claude --resume abc".to_owned()),
            },
            HandoffTab {
                title: None,
                cwd: Some("C:\\work\\hassle".to_owned()),
                shell: None,
                resume_command: None,
            },
        ]
    );
}

#[test]
fn the_quake_window_is_not_handed_off() {
    let quake = vec![tab(None, terminal_leaf(Some("/quake"), None, true))];
    let normal = vec![tab(None, terminal_leaf(Some("/normal"), None, true))];
    let state = app_state(vec![window(quake, 0, true), window(normal, 0, false)]);

    let handoff = from_app_state(&state, 0, None);

    assert_eq!(handoff.windows.len(), 1);
    assert_eq!(handoff.windows[0].tabs[0].cwd.as_deref(), Some("/normal"));
}

#[test]
fn a_shell_is_handed_off_under_a_name_that_can_be_looked_up() {
    assert_eq!(
        shell_name(Some(&ShellLaunchData::Executable {
            executable_path: PathBuf::from("C:\\Users\\steep\\pwsh.exe"),
            shell_type: ShellType::PowerShell,
        }))
        .as_deref(),
        Some("pwsh")
    );
    assert_eq!(
        shell_name(Some(&ShellLaunchData::WSL {
            distro: "Ubuntu".to_owned(),
        }))
        .as_deref(),
        Some("wsl:Ubuntu")
    );
    assert_eq!(
        shell_name(Some(&ShellLaunchData::DockerSandbox {
            sbx_path: PathBuf::from("/usr/bin/sbx"),
            base_image: None,
        })),
        None
    );
    assert_eq!(shell_name(None), None);
}

#[test]
fn tabs_are_reopened_when_nothing_else_restored_the_session() {
    let handoff = saved(
        vec![HandoffWindow {
            active_tab_index: 1,
            tabs: vec![handoff_tab("/first"), handoff_tab("/second")],
        }],
        1_000,
    );

    let plan = plan(Some(handoff.clone()), 0, 2_000);

    assert_eq!(plan, Plan::Open(handoff.windows));
}

#[test]
fn restored_windows_win_over_the_handoff() {
    let handoff = saved(
        vec![HandoffWindow {
            active_tab_index: 0,
            tabs: vec![handoff_tab("/first")],
        }],
        1_000,
    );

    assert_eq!(plan(Some(handoff), 1, 2_000), Plan::Skip);
}

#[test]
fn nothing_is_reopened_without_a_handoff() {
    assert_eq!(plan(None, 0, 2_000), Plan::Skip);
}

#[test]
fn a_handoff_of_an_update_that_never_came_back_is_ignored() {
    let handoff = saved(
        vec![HandoffWindow {
            active_tab_index: 0,
            tabs: vec![handoff_tab("/first")],
        }],
        1_000,
    );

    assert_eq!(
        plan(Some(handoff.clone()), 0, 1_000 + MAX_AGE_MS),
        Plan::Open(handoff.windows.clone())
    );
    assert_eq!(plan(Some(handoff), 0, 1_000 + MAX_AGE_MS + 1), Plan::Skip);
}

#[test]
fn a_handoff_of_another_format_is_ignored() {
    let mut handoff = saved(
        vec![HandoffWindow {
            active_tab_index: 0,
            tabs: vec![handoff_tab("/first")],
        }],
        1_000,
    );
    handoff.version = FORMAT_VERSION + 1;

    assert_eq!(plan(Some(handoff), 0, 2_000), Plan::Skip);
}

#[test]
fn windows_without_tabs_are_dropped() {
    let handoff = saved(
        vec![
            HandoffWindow {
                active_tab_index: 0,
                tabs: Vec::new(),
            },
            HandoffWindow {
                active_tab_index: 0,
                tabs: vec![handoff_tab("/first")],
            },
        ],
        1_000,
    );

    let Plan::Open(windows) = plan(Some(handoff), 0, 2_000) else {
        panic!("the second window should be reopened");
    };
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].tabs, vec![handoff_tab("/first")]);

    let empty = saved(
        vec![HandoffWindow {
            active_tab_index: 0,
            tabs: Vec::new(),
        }],
        1_000,
    );
    assert_eq!(plan(Some(empty), 0, 2_000), Plan::Skip);
}

#[test]
fn an_active_tab_index_past_the_end_is_clamped() {
    let handoff = saved(
        vec![HandoffWindow {
            active_tab_index: 7,
            tabs: vec![handoff_tab("/first"), handoff_tab("/second")],
        }],
        1_000,
    );

    let Plan::Open(windows) = plan(Some(handoff), 0, 2_000) else {
        panic!("the window should be reopened");
    };
    assert_eq!(windows[0].active_tab_index, 1);
}

#[test]
fn the_handoff_survives_a_write_and_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join(FILE_NAME);
    let handoff = saved(
        vec![HandoffWindow {
            active_tab_index: 0,
            tabs: vec![HandoffTab {
                title: Some("psn".to_owned()),
                cwd: Some("C:\\work".to_owned()),
                shell: Some("pwsh".to_owned()),
                resume_command: Some("claude --resume abc".to_owned()),
            }],
        }],
        1_000,
    );

    write(&path, &handoff).expect("the handoff should be written");
    assert_eq!(read(&path), Some(handoff));
}

#[test]
fn a_missing_or_broken_handoff_reads_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);

    assert_eq!(read(&path), None);

    fs::write(&path, "{ not json").unwrap();
    assert_eq!(read(&path), None);
}

#[test]
fn removing_the_handoff_twice_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    fs::write(&path, "{}").unwrap();

    remove(&path);
    assert!(!path.exists());
    remove(&path);
}
