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
