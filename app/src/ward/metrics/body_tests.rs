use super::*;
use crate::ward::metrics::state::{CrashCounts, RestoredCounts, UpdateCounts};

fn state() -> MetricsState {
    MetricsState {
        installation_id: "id".to_owned(),
        launches: 7,
        crashes: CrashCounts {
            panic: 1,
            unclean_exit: 2,
        },
        update_installs: UpdateCounts {
            success: 3,
            failure: 0,
        },
        restored_sessions: RestoredCounts {
            claude: 4,
            codex: 0,
            other: 0,
            none: 5,
        },
        last_seen_version: Some("v1".to_owned()),
        run_in_progress: true,
    }
}

fn snapshot<'a>(
    state: &'a MetricsState,
    sessions: Option<&'a BTreeMap<(AgentLabel, SessionState), u64>>,
) -> MetricsSnapshot<'a> {
    MetricsSnapshot {
        version: "v0.2026.10.29.12.00.oss_00",
        channel: "stable",
        arch: "x86_64",
        running: true,
        process_start_time_seconds: 1_761_739_200,
        state,
        panics_this_run: 0,
        agent_sessions: sessions,
    }
}

#[test]
fn renders_every_series_with_type_lines() {
    let state = state();
    let mut sessions = BTreeMap::new();
    sessions.insert((AgentLabel::Claude, SessionState::Working), 2);
    sessions.insert((AgentLabel::Codex, SessionState::Waiting), 1);
    let body = render(&snapshot(&state, Some(&sessions)));
    let expected = "\
# TYPE ward_build_info gauge
ward_build_info{version=\"v0.2026.10.29.12.00.oss_00\",channel=\"stable\",os=\"windows\",arch=\"x86_64\"} 1
# TYPE ward_app_running gauge
ward_app_running 1
# TYPE ward_process_start_time_seconds gauge
ward_process_start_time_seconds 1761739200
# TYPE ward_app_launches_total counter
ward_app_launches_total 7
# TYPE ward_crashes_total counter
ward_crashes_total{kind=\"panic\"} 1
ward_crashes_total{kind=\"unclean_exit\"} 2
# TYPE ward_update_installs_total counter
ward_update_installs_total{result=\"success\"} 3
ward_update_installs_total{result=\"failure\"} 0
# TYPE ward_restored_sessions_total counter
ward_restored_sessions_total{agent=\"claude\"} 4
ward_restored_sessions_total{agent=\"codex\"} 0
ward_restored_sessions_total{agent=\"other\"} 0
ward_restored_sessions_total{agent=\"none\"} 5
# TYPE ward_agent_sessions gauge
ward_agent_sessions{agent=\"claude\",state=\"working\"} 2
ward_agent_sessions{agent=\"codex\",state=\"waiting\"} 1
";
    assert_eq!(body, expected);
}

#[test]
fn fresh_install_sends_zero_counters() {
    let state = MetricsState::default();
    let body = render(&snapshot(&state, Some(&BTreeMap::new())));
    for line in [
        "ward_app_launches_total 0\n",
        "ward_crashes_total{kind=\"panic\"} 0\n",
        "ward_crashes_total{kind=\"unclean_exit\"} 0\n",
        "ward_update_installs_total{result=\"success\"} 0\n",
        "ward_update_installs_total{result=\"failure\"} 0\n",
        "ward_restored_sessions_total{agent=\"claude\"} 0\n",
        "ward_restored_sessions_total{agent=\"codex\"} 0\n",
        "ward_restored_sessions_total{agent=\"other\"} 0\n",
        "ward_restored_sessions_total{agent=\"none\"} 0\n",
    ] {
        assert!(body.contains(line), "missing {line:?} in\n{body}");
    }
}

#[test]
fn final_push_reports_not_running_and_no_agent_sessions() {
    let state = state();
    let body = render(&MetricsSnapshot {
        running: false,
        ..snapshot(&state, None)
    });
    assert!(body.contains("ward_app_running 0\n"));
    assert!(!body.contains("ward_agent_sessions"));
}

#[test]
fn zero_agent_session_combinations_are_omitted() {
    let state = state();
    let mut sessions = BTreeMap::new();
    sessions.insert((AgentLabel::Other, SessionState::Idle), 0);
    let body = render(&snapshot(&state, Some(&sessions)));
    assert!(body.contains("# TYPE ward_agent_sessions gauge\n"));
    assert!(!body.contains("ward_agent_sessions{"));
}

#[test]
fn panics_of_the_current_run_are_added() {
    let state = state();
    let body = render(&MetricsSnapshot {
        panics_this_run: 2,
        ..snapshot(&state, None)
    });
    assert!(body.contains("ward_crashes_total{kind=\"panic\"} 3\n"));
}

#[test]
fn label_values_are_escaped() {
    let state = state();
    let body = render(&MetricsSnapshot {
        version: "v\"1\\\n",
        ..snapshot(&state, None)
    });
    assert!(body.contains("version=\"v\\\"1\\\\\\n\""), "{body}");
}

#[test]
fn body_has_no_job_or_instance_labels_and_fits_the_limit() {
    let state = state();
    let mut sessions = BTreeMap::new();
    for agent in [AgentLabel::Claude, AgentLabel::Codex, AgentLabel::Other] {
        for session_state in [SessionState::Working, SessionState::Waiting, SessionState::Idle] {
            sessions.insert((agent, session_state), 1_000);
        }
    }
    let body = render(&snapshot(&state, Some(&sessions)));
    assert!(!body.contains("job="));
    assert!(!body.contains("instance="));
    assert!(body.len() < MAX_BODY_BYTES);
}

#[test]
fn agent_states_map_to_session_states() {
    use crate::ward::agent_state::{AgentState, StatusKind};

    assert_eq!(SessionState::of_agent_state(AgentState::RunningTool), SessionState::Working);
    assert_eq!(SessionState::of_agent_state(AgentState::Thinking), SessionState::Working);
    assert_eq!(
        SessionState::of_agent_state(AgentState::WaitingPermission),
        SessionState::Waiting
    );
    assert_eq!(SessionState::of_agent_state(AgentState::WaitingAnswer), SessionState::Waiting);
    for idle in [AgentState::Error, AgentState::DoneUnseen, AgentState::Done, AgentState::Idle] {
        assert_eq!(SessionState::of_agent_state(idle), SessionState::Idle);
    }
    assert_eq!(SessionState::of_status_kind(StatusKind::InProgress), SessionState::Working);
    assert_eq!(SessionState::of_status_kind(StatusKind::Question), SessionState::Waiting);
    assert_eq!(SessionState::of_status_kind(StatusKind::Failed), SessionState::Idle);
}
