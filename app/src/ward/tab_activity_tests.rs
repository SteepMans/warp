use std::time::Duration;

use super::*;

#[test]
fn working_states_animate() {
    assert_eq!(
        TabActivity::of(AgentState::Thinking),
        Some(TabActivity::Working)
    );
    assert_eq!(
        TabActivity::of(AgentState::RunningTool),
        Some(TabActivity::Working)
    );
}

#[test]
fn waiting_and_failed_states_need_the_user() {
    for state in [
        AgentState::WaitingPermission,
        AgentState::WaitingAnswer,
        AgentState::Error,
    ] {
        assert_eq!(
            TabActivity::of(state),
            Some(TabActivity::NeedsUser),
            "{state:?}"
        );
    }
}

#[test]
fn finished_and_idle_states_add_no_mark() {
    // A finished run that was not seen keeps the unread dot instead.
    for state in [AgentState::DoneUnseen, AgentState::Done, AgentState::Idle] {
        assert_eq!(TabActivity::of(state), None, "{state:?}");
    }
}

#[test]
fn working_frame_walks_the_dots_and_loops() {
    assert_eq!(working_frame_at(Duration::ZERO), (0, WORKING_FRAME));
    assert_eq!(working_frame_at(WORKING_FRAME), (1, WORKING_FRAME));
    assert_eq!(working_frame_at(WORKING_FRAME * 2), (2, WORKING_FRAME));
    assert_eq!(
        working_frame_at(WORKING_FRAME * WORKING_DOTS as u32),
        (0, WORKING_FRAME)
    );
}

#[test]
fn working_frame_waits_only_for_the_rest_of_the_frame() {
    let elapsed = WORKING_FRAME + Duration::from_millis(150);
    assert_eq!(
        working_frame_at(elapsed),
        (1, WORKING_FRAME - Duration::from_millis(150))
    );
}

#[test]
fn working_frame_handles_long_runs() {
    // Hours into a run the phase still comes out right.
    let elapsed = WORKING_FRAME * (WORKING_DOTS as u32 * 10_000 + 2);
    assert_eq!(working_frame_at(elapsed).0, 2);
}
