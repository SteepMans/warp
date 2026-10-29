use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn resume(args: &[&str], shell_type: Option<ShellType>) -> String {
    let argv = argv(args);
    let args = claude_args_from_argv(&argv).expect("claude executable in argv");
    build_resume_command(args, "5da63d55-1deb-4bf3-ae66-a25a95d2a1b6", shell_type)
}

#[test]
fn keeps_launch_flags_and_drops_prompt() {
    assert_eq!(
        resume(
            &[
                r"C:\Users\me\.local\bin\claude.exe",
                "--remote-control",
                "hassle. dup-currency",
                "--dangerously-skip-permissions",
                "--model",
                "claude-opus-5-5",
                "Fix the bug\nwith a long prompt",
            ],
            Some(ShellType::PowerShell),
        ),
        "claude --remote-control 'hassle. dup-currency' --dangerously-skip-permissions \
         --model claude-opus-5-5 --resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"
    );
}

#[test]
fn stops_at_double_dash() {
    assert_eq!(
        resume(
            &[
                "/usr/bin/claude",
                "--remote-control",
                "it's mine",
                "--",
                "--model looks like a flag but is the prompt",
            ],
            Some(ShellType::Bash),
        ),
        r"claude --remote-control 'it'\''s mine' --resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"
    );
}

#[test]
fn replaces_previous_resume_and_continue() {
    assert_eq!(
        resume(
            &[
                "claude",
                "--resume",
                "0022af3c-f160-428b-8225-ba464c02cd76",
                "-c",
                "--permission-mode=plan",
                "--session-id",
                "abc",
            ],
            None,
        ),
        "claude --permission-mode plan --resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"
    );
}

#[test]
fn remote_control_without_name() {
    assert_eq!(
        resume(&["claude", "--remote-control", "--verbose"], None),
        "claude --remote-control --verbose --resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"
    );
}

#[test]
fn skips_node_interpreter() {
    assert_eq!(
        resume(
            &[
                "node",
                "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                "--model",
                "sonnet",
            ],
            None,
        ),
        "claude --model sonnet --resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"
    );
}

#[test]
fn reads_session_id_from_resume_command() {
    assert_eq!(
        session_id_from_resume_command(
            "claude --model sonnet --resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"
        ),
        Some("5da63d55-1deb-4bf3-ae66-a25a95d2a1b6")
    );
    assert_eq!(
        session_id_from_resume_command("codex resume 5da63d55-1deb-4bf3-ae66-a25a95d2a1b6"),
        None
    );
}
