use pathfinder_geometry::vector::vec2f;

use super::*;

/// An update is ready, the user has been away long enough and nothing runs.
fn free() -> Conditions {
    Conditions {
        enabled: true,
        ready_version: Some("v0.2026.11.06.10.00.oss_00".to_owned()),
        per_user_install: true,
        idle_for: IDLE_BEFORE_INSTALL + Duration::from_secs(1),
        sessions: Vec::new(),
        busy_panes: Vec::new(),
        unsaved_files: false,
        shared_sessions: 0,
    }
}

fn session(status: SessionStatus, turn: Option<AgentTurn>) -> WorkerSession {
    WorkerSession {
        name: "#4704".to_owned(),
        status,
        turn,
    }
}

fn agent_pane(turn: AgentTurn) -> BusyPane {
    BusyPane {
        tab: "warp".to_owned(),
        agent: Some(("Claude Code".to_owned(), turn)),
    }
}

#[test]
fn installs_when_nothing_holds_it() {
    assert_eq!(decide(&free()), Decision::Install);
}

#[test]
fn setting_off_never_installs() {
    let conditions = Conditions {
        enabled: false,
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::Off);
}

#[test]
fn nothing_to_install_without_a_ready_update() {
    let conditions = Conditions {
        ready_version: None,
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::NothingToInstall);
}

#[test]
fn user_active_waits() {
    let conditions = Conditions {
        idle_for: Duration::from_secs(14 * 60),
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::Wait(WaitReason::UserActive));
    assert_eq!(
        WaitReason::UserActive.text(),
        "Waiting: you are using Ward (installs after 15 min without input)"
    );
}

#[test]
fn exactly_the_idle_time_is_enough() {
    let conditions = Conditions {
        idle_for: IDLE_BEFORE_INSTALL,
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::Install);
}

#[test]
fn running_command_waits() {
    let conditions = Conditions {
        busy_panes: vec![BusyPane {
            tab: "cargo build".to_owned(),
            agent: None,
        }],
        ..free()
    };
    let decision = decide(&conditions);
    assert_eq!(
        decision,
        Decision::Wait(WaitReason::CommandRunning {
            tab: "cargo build".to_owned()
        })
    );
    let Decision::Wait(reason) = decision else {
        unreachable!()
    };
    assert_eq!(
        reason.text(),
        "Waiting: command running in tab \"cargo build\""
    );
}

#[test]
fn agent_in_the_middle_of_a_turn_waits() {
    let conditions = Conditions {
        busy_panes: vec![agent_pane(AgentTurn::Working)],
        ..free()
    };
    assert_eq!(
        decide(&conditions),
        Decision::Wait(WaitReason::AgentWorking {
            tab: "warp".to_owned(),
            agent: "Claude Code".to_owned(),
        })
    );
}

#[test]
fn agent_waiting_for_input_does_not_hold_the_install() {
    let conditions = Conditions {
        busy_panes: vec![agent_pane(AgentTurn::Waiting)],
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::Install);
}

#[test]
fn agent_without_plugin_status_counts_as_busy() {
    let conditions = Conditions {
        busy_panes: vec![BusyPane {
            tab: "codex".to_owned(),
            agent: Some(("Codex".to_owned(), AgentTurn::Unknown)),
        }],
        ..free()
    };
    assert_eq!(
        decide(&conditions),
        Decision::Wait(WaitReason::AgentStatusUnknown {
            tab: "codex".to_owned(),
            agent: "Codex".to_owned(),
        })
    );
}

#[test]
fn preparing_session_waits() {
    for status in [
        SessionStatus::Queued,
        SessionStatus::Assigned,
        SessionStatus::Preparing,
    ] {
        let conditions = Conditions {
            sessions: vec![session(status, Some(AgentTurn::Waiting))],
            ..free()
        };
        assert_eq!(
            decide(&conditions),
            Decision::Wait(WaitReason::SessionPreparing("#4704".to_owned())),
            "{status:?}"
        );
    }
}

#[test]
fn running_session_mid_turn_waits() {
    for turn in [None, Some(AgentTurn::Working), Some(AgentTurn::Unknown)] {
        let conditions = Conditions {
            sessions: vec![session(SessionStatus::Running, turn)],
            ..free()
        };
        let decision = decide(&conditions);
        assert_eq!(
            decision,
            Decision::Wait(WaitReason::SessionWorking("#4704".to_owned())),
            "{turn:?}"
        );
        let Decision::Wait(reason) = decision else {
            unreachable!()
        };
        assert_eq!(reason.text(), "Waiting: agent session #4704 is working");
    }
}

#[test]
fn running_session_between_turns_does_not_hold_the_install() {
    let conditions = Conditions {
        sessions: vec![session(SessionStatus::Running, Some(AgentTurn::Waiting))],
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::Install);
}

#[test]
fn sessions_waiting_for_the_owner_do_not_hold_the_install() {
    for status in [
        SessionStatus::Review,
        SessionStatus::Blocked,
        SessionStatus::PlanReview,
    ] {
        for turn in [None, Some(AgentTurn::Waiting), Some(AgentTurn::Unknown)] {
            let conditions = Conditions {
                sessions: vec![session(status, turn)],
                ..free()
            };
            assert_eq!(
                decide(&conditions),
                Decision::Install,
                "{status:?} {turn:?}"
            );
        }
    }
}

#[test]
fn session_waiting_for_the_owner_whose_agent_works_again_waits() {
    let conditions = Conditions {
        sessions: vec![session(SessionStatus::Review, Some(AgentTurn::Working))],
        ..free()
    };
    assert_eq!(
        decide(&conditions),
        Decision::Wait(WaitReason::SessionWorking("#4704".to_owned()))
    );
}

#[test]
fn finished_sessions_do_not_hold_the_install() {
    let conditions = Conditions {
        sessions: vec![
            session(SessionStatus::Failed, None),
            session(SessionStatus::Cancelled, None),
            session(SessionStatus::Lost, None),
            session(SessionStatus::Complete, None),
        ],
        ..free()
    };
    assert_eq!(decide(&conditions), Decision::Install);
}

#[test]
fn install_for_all_users_waits() {
    let conditions = Conditions {
        per_user_install: false,
        ..free()
    };
    assert_eq!(
        decide(&conditions),
        Decision::Wait(WaitReason::NeedsAdministrator)
    );
}

#[test]
fn unsaved_files_and_shared_sessions_wait() {
    let unsaved = Conditions {
        unsaved_files: true,
        ..free()
    };
    assert_eq!(decide(&unsaved), Decision::Wait(WaitReason::UnsavedFiles));
    let shared = Conditions {
        shared_sessions: 1,
        ..free()
    };
    assert_eq!(decide(&shared), Decision::Wait(WaitReason::SharedSession));
}

#[test]
fn lasting_reasons_come_before_the_users_activity() {
    let conditions = Conditions {
        idle_for: Duration::ZERO,
        busy_panes: vec![agent_pane(AgentTurn::Working)],
        sessions: vec![session(SessionStatus::Preparing, None)],
        ..free()
    };
    assert_eq!(
        decide(&conditions),
        Decision::Wait(WaitReason::SessionPreparing("#4704".to_owned()))
    );
}

#[test]
fn plugin_status_gives_the_turn() {
    assert_eq!(
        turn_of(&CLIAgentSessionStatus::InProgress, true),
        AgentTurn::Working
    );
    for status in [
        CLIAgentSessionStatus::Success,
        CLIAgentSessionStatus::Cancelled,
        CLIAgentSessionStatus::Failed {
            error_type: None,
            message: None,
        },
        CLIAgentSessionStatus::Blocked {
            message: None,
            source: crate::terminal::cli_agent_sessions::BlockedSource::PermissionRequest,
        },
    ] {
        assert_eq!(turn_of(&status, true), AgentTurn::Waiting, "{status:?}");
    }
    assert_eq!(
        turn_of(&CLIAgentSessionStatus::Success, false),
        AgentTurn::Unknown
    );
    assert_eq!(
        turn_of(&CLIAgentSessionStatus::InProgress, false),
        AgentTurn::Unknown
    );
}

#[test]
fn session_names() {
    assert_eq!(session_name(Some(4704), "Fix", "abcdef1234"), "#4704");
    assert_eq!(session_name(None, " Fix it ", "abcdef1234"), "\"Fix it\"");
    assert_eq!(session_name(None, " ", "abcdef1234"), "abcdef12");
}

#[test]
fn install_dir_comparison_ignores_case_and_separators() {
    assert!(same_install_dir(
        r"C:\Users\me\AppData\Local\Programs\WarpOss\",
        r"c:\users\me\appdata\local\programs\warposs"
    ));
    assert!(same_install_dir(
        r"C:\Programs\WarpOss",
        r"\\?\C:\Programs\WarpOss"
    ));
    assert!(same_install_dir(
        "C:/Programs/WarpOss/",
        r"C:\Programs\WarpOss"
    ));
    assert!(!same_install_dir(
        r"C:\Program Files\WarpOss\",
        r"C:\Users\me\AppData\Local\Programs\WarpOss"
    ));
    assert!(!same_install_dir("", r"C:\"));
}

#[test]
fn only_made_up_mouse_moves_are_not_input() {
    let moved = |is_synthetic| Event::MouseMoved {
        position: vec2f(1., 1.),
        cmd: false,
        shift: false,
        is_synthetic,
    };
    assert!(is_user_input(&moved(false)));
    assert!(!is_user_input(&moved(true)));
    assert!(is_user_input(&Event::TypedCharacters {
        chars: "a".to_owned()
    }));
}
