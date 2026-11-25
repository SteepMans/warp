use super::*;

fn no_flicker() -> HashMap<OsString, OsString> {
    HashMap::from([(OsString::from("CLAUDE_CODE_NO_FLICKER"), OsString::from("1"))])
}

#[test]
fn recognizes_claude_commands() {
    for command in [
        "claude",
        "claude --continue",
        "  claude --remote-control 'warp. task-1287'",
        "claude --model haiku --dangerously-skip-permissions --resume d068312d-1",
        "claude.exe --resume abc",
        "Claude.CMD",
        r"C:\Users\me\.local\bin\claude.exe --continue",
        "/home/me/.local/bin/claude -c",
        r#""C:\Program Files\Claude\claude.exe" --resume abc"#,
        "'/opt/my tools/claude' --continue",
    ] {
        assert!(is_claude_command(command), "{command}");
    }
}

#[test]
fn other_commands_are_not_claude() {
    for command in [
        "",
        "   ",
        "codex",
        "codex resume 0199a",
        "claude-monitor",
        "claudette",
        "echo claude",
        "npx claude",
        "gemini --resume latest",
    ] {
        assert!(!is_claude_command(command), "{command}");
    }
}

#[test]
fn env_is_set_only_for_claude_when_enabled() {
    assert_eq!(launch_env_vars("claude", true), no_flicker());
    assert_eq!(launch_env_vars("claude --continue", true), no_flicker());
    assert_eq!(
        launch_env_vars("claude --model haiku --resume 'd068312d-1'", true),
        no_flicker()
    );
    assert!(launch_env_vars("claude --continue", false).is_empty());
    assert!(launch_env_vars("codex", true).is_empty());
    assert!(launch_env_vars("codex resume 0199a", true).is_empty());
}

fn resume_session(env_vars: &HashMap<OsString, OsString>) -> Option<&str> {
    env_vars
        .get(&OsString::from(RESUME_SESSION_ENV_VAR))
        .and_then(|value| value.to_str())
}

#[test]
fn reads_the_session_id_of_a_resume_command() {
    assert_eq!(
        resumed_session_id(
            "claude --remote-control 'warp. task 1' --model haiku --resume d068312d-aaaa-4bbb-8ccc-000000000001"
        ),
        Some("d068312d-aaaa-4bbb-8ccc-000000000001")
    );
    assert_eq!(
        resumed_session_id("claude --resume 'd068312d-aaaa-4bbb-8ccc-000000000001'"),
        Some("d068312d-aaaa-4bbb-8ccc-000000000001")
    );
    assert_eq!(
        resumed_session_id("codex resume --model gpt-5 019a2b3c-1111-7222-8333-444455556666"),
        Some("019a2b3c-1111-7222-8333-444455556666")
    );
    assert_eq!(resumed_session_id("claude"), None);
    assert_eq!(resumed_session_id("claude --continue"), None);
    assert_eq!(resumed_session_id("codex"), None);
}

/// Several tabs in one project directory, each with a conversation of its own: every reopened
/// tab carries its own session id, never another tab's.
#[test]
fn every_reopened_tab_of_one_project_carries_its_own_session() {
    let tabs = [
        "claude --remote-control 'psn. bot' --resume 11111111-aaaa-4bbb-8ccc-000000000001",
        "claude --resume 22222222-aaaa-4bbb-8ccc-000000000002",
        "claude --model haiku --resume 33333333-aaaa-4bbb-8ccc-000000000003",
    ];
    let sessions: Vec<Option<String>> = tabs
        .iter()
        .map(|command| resume_session(&resume_env_vars(command, false)).map(str::to_owned))
        .collect();
    assert_eq!(
        sessions,
        vec![
            Some("11111111-aaaa-4bbb-8ccc-000000000001".to_owned()),
            Some("22222222-aaaa-4bbb-8ccc-000000000002".to_owned()),
            Some("33333333-aaaa-4bbb-8ccc-000000000003".to_owned()),
        ]
    );
}

#[test]
fn resume_env_keeps_the_launch_env() {
    let env_vars = resume_env_vars("claude --resume 11111111-aaaa-4bbb-8ccc-000000000001", true);
    assert_eq!(
        env_vars.get(&OsString::from(NO_FLICKER_ENV_VAR)),
        Some(&OsString::from("1"))
    );
    assert_eq!(
        resume_session(&env_vars),
        Some("11111111-aaaa-4bbb-8ccc-000000000001")
    );
    let codex = resume_env_vars("codex resume 019a2b3c-1111-7222-8333-444455556666", true);
    assert!(codex.get(&OsString::from(NO_FLICKER_ENV_VAR)).is_none());
    assert_eq!(
        resume_session(&codex),
        Some("019a2b3c-1111-7222-8333-444455556666")
    );
}

#[test]
fn a_command_without_a_session_sets_no_resume_variable() {
    assert_eq!(resume_session(&resume_env_vars("claude", false)), None);
    assert_eq!(
        resume_session(&resume_env_vars("claude --continue", true)),
        None
    );
    assert_eq!(resume_session(&resume_env_vars("codex", false)), None);
}
