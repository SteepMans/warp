use super::*;

fn facts(version: &str) -> StartupFacts<'_> {
    StartupFacts {
        current_version: version,
        marker_panics: 0,
        update_log_found: false,
    }
}

fn id() -> String {
    "0b8e4f7c-6a1d-4c39-9f3e-2d5b7a1c9e40".to_owned()
}

fn temp_paths() -> (tempfile::TempDir, StatePaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::in_dir(dir.path());
    (dir, paths)
}

#[test]
fn first_run_creates_the_id_and_counts_the_launch() {
    let mut state = MetricsState::default();
    let previous = state.begin_run(&facts("v1"), id);
    assert_eq!(previous, PreviousRun::FirstRun);
    assert_eq!(state.installation_id, id());
    assert_eq!(state.launches, 1);
    assert!(state.run_in_progress);
    assert_eq!(state.last_seen_version.as_deref(), Some("v1"));
    assert_eq!(state.crashes, CrashCounts::default());
    assert_eq!(state.update_installs, UpdateCounts::default());
}

#[test]
fn existing_id_is_kept() {
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    state.end_run();
    state.begin_run(&facts("v1"), || panic!("id must not be recreated"));
    assert_eq!(state.installation_id, id());
    assert_eq!(state.launches, 2);
}

#[test]
fn clean_exit_is_not_a_crash() {
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    state.end_run();
    assert_eq!(state.begin_run(&facts("v1"), id), PreviousRun::Clean);
    assert_eq!(state.crashes, CrashCounts::default());
}

#[test]
fn run_without_clean_exit_is_an_unclean_exit() {
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    assert_eq!(state.begin_run(&facts("v1"), id), PreviousRun::Unclean);
    assert_eq!(state.crashes.unclean_exit, 1);
    assert_eq!(state.crashes.panic, 0);
}

#[test]
fn recorded_panic_is_not_also_an_unclean_exit() {
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    let previous = state.begin_run(
        &StartupFacts {
            marker_panics: 2,
            ..facts("v1")
        },
        id,
    );
    assert_eq!(previous, PreviousRun::Panicked);
    assert_eq!(state.crashes.panic, 2);
    assert_eq!(state.crashes.unclean_exit, 0);
}

#[test]
fn version_change_counts_a_successful_update() {
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    state.end_run();
    state.begin_run(&facts("v2"), id);
    assert_eq!(state.update_installs.success, 1);
    assert_eq!(state.update_installs.failure, 0);
    assert_eq!(state.last_seen_version.as_deref(), Some("v2"));
}

#[test]
fn installer_log_without_version_change_counts_a_failed_update() {
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    state.end_run();
    state.begin_run(
        &StartupFacts {
            update_log_found: true,
            ..facts("v1")
        },
        id,
    );
    assert_eq!(state.update_installs, UpdateCounts { success: 0, failure: 1 });
}

#[test]
fn first_run_is_not_an_update() {
    let mut state = MetricsState::default();
    state.begin_run(
        &StartupFacts {
            update_log_found: true,
            ..facts("v1")
        },
        id,
    );
    assert_eq!(state.update_installs, UpdateCounts::default());
}

#[test]
fn restored_counts_by_agent() {
    let mut counts = RestoredCounts::default();
    counts.increment(RestoredAgent::Claude);
    counts.increment(RestoredAgent::Claude);
    counts.increment(RestoredAgent::None);
    assert_eq!(counts.get(RestoredAgent::Claude), 2);
    assert_eq!(counts.get(RestoredAgent::Codex), 0);
    assert_eq!(counts.get(RestoredAgent::None), 1);
}

#[test]
fn resume_commands_map_to_agents() {
    assert_eq!(RestoredAgent::of_resume_command(None), RestoredAgent::None);
    assert_eq!(
        RestoredAgent::of_resume_command(Some("claude --resume 'abc'")),
        RestoredAgent::Claude
    );
    assert_eq!(
        RestoredAgent::of_resume_command(Some("codex resume 'abc'")),
        RestoredAgent::Codex
    );
    assert_eq!(
        RestoredAgent::of_resume_command(Some(r"& 'C:\Tools\Codex.exe' resume x")),
        RestoredAgent::Codex
    );
    assert_eq!(
        RestoredAgent::of_resume_command(Some("gemini --resume x")),
        RestoredAgent::Other
    );
}

#[test]
fn state_round_trips_through_the_file() {
    let (_dir, paths) = temp_paths();
    let mut state = MetricsState::default();
    state.begin_run(&facts("v1"), id);
    state.restored_sessions.increment(RestoredAgent::Codex);
    save(&paths.state, &state).unwrap();
    assert_eq!(load(&paths.state), state);
}

#[test]
fn missing_or_corrupt_state_starts_from_scratch() {
    let (_dir, paths) = temp_paths();
    assert_eq!(load(&paths.state), MetricsState::default());
    std::fs::write(&paths.state, "{not json").unwrap();
    assert_eq!(load(&paths.state), MetricsState::default());
}

#[test]
fn state_from_an_older_build_keeps_known_fields() {
    let (_dir, paths) = temp_paths();
    std::fs::write(
        &paths.state,
        r#"{"installation_id":"x","launches":5,"crashes":{"panic":1}}"#,
    )
    .unwrap();
    let state = load(&paths.state);
    assert_eq!(state.installation_id, "x");
    assert_eq!(state.launches, 5);
    assert_eq!(state.crashes, CrashCounts { panic: 1, unclean_exit: 0 });
    assert_eq!(state.restored_sessions, RestoredCounts::default());
}

#[test]
fn panic_marker_round_trip() {
    let (_dir, paths) = temp_paths();
    assert_eq!(read_panic_marker(&paths.panic_marker), 0);
    write_panic_marker(&paths.panic_marker, 3).unwrap();
    assert_eq!(read_panic_marker(&paths.panic_marker), 3);
    std::fs::write(&paths.panic_marker, "garbage").unwrap();
    assert_eq!(read_panic_marker(&paths.panic_marker), 1);
    remove_panic_marker(&paths.panic_marker);
    assert_eq!(read_panic_marker(&paths.panic_marker), 0);
    // Removing a missing marker is fine.
    remove_panic_marker(&paths.panic_marker);
}
