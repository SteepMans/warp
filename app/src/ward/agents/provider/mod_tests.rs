use std::path::Path;

use super::*;

#[test]
fn providers_are_found_by_id() {
    assert_eq!(
        by_id("claude").map(|provider| provider.id()),
        Some("claude")
    );
    assert_eq!(by_id("codex").map(|provider| provider.id()), Some("codex"));
    assert!(by_id("gemini").is_none());
    let ids: Vec<&str> = all().iter().map(|provider| provider.id()).collect();
    assert_eq!(ids, ["claude", "codex"]);
}

#[test]
fn command_depends_on_provider_and_model() {
    let claude = by_id("claude")
        .unwrap()
        .command("4623 Встречи", Some("claude-opus-5-5"));
    assert_eq!(claude.program, "claude");
    assert_eq!(
        claude.args,
        [
            "--remote-control",
            "4623 Встречи",
            "--dangerously-skip-permissions",
            "--model",
            "claude-opus-5-5"
        ]
    );
    assert_eq!(claude.remove_env_prefixes, ["CLAUDE"]);
    assert_eq!(
        claude.set_env,
        [(
            "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE".to_owned(),
            "1".to_owned()
        )]
    );

    let claude_default = by_id("claude").unwrap().command("1 x", None);
    assert!(!claude_default.args.iter().any(|arg| arg == "--model"));

    let codex = by_id("codex")
        .unwrap()
        .command("4623 Встречи", Some("gpt-5.5"));
    assert_eq!(codex.program, "codex");
    assert_eq!(
        codex.args,
        [
            "--dangerously-bypass-approvals-and-sandbox",
            "-m",
            "gpt-5.5"
        ]
    );
    assert!(codex.remove_env_prefixes.contains(&"CLAUDE"));
    let codex_default = by_id("codex").unwrap().command("1 x", None);
    assert_eq!(
        codex_default.args,
        ["--dangerously-bypass-approvals-and-sandbox"]
    );
}

#[test]
fn same_path_ignores_slashes_and_case_on_windows() {
    assert!(same_path("/a/b/", Path::new("/a/b")));
    if cfg!(windows) {
        assert!(same_path(
            r"C:\Users\Steep\agents-work\x",
            Path::new("c:/users/steep/agents-work/x")
        ));
    } else {
        assert!(!same_path("/A/b", Path::new("/a/b")));
    }
}

/// What a line of the chat keeps of a long reply and of the arguments of a tool call.
#[test]
fn a_line_of_the_chat_is_cut_and_collapsed() {
    assert_eq!(cut("коротко", 10), "коротко");
    // The cut is counted in characters, so it never lands inside one.
    assert_eq!(cut("абвгд", 3), "абв…");
    assert_eq!(one_line("  две\n\tстроки  ", 40), "две строки");
    assert_eq!(one_line("абвгде", 3), "абв…");

    // The field that says what a tool did wins over the whole arguments object.
    let input = serde_json::json!({"command": "cargo  test\n--lib", "timeout": 600});
    assert_eq!(preview(&input), "cargo test --lib");
    // Nothing to single out: the arguments themselves, on one line.
    let other = serde_json::json!({"a": 1});
    assert_eq!(preview(&other), "{\"a\":1}");
    assert_eq!(preview(&serde_json::Value::Null), "");

    let said = tool_said("  Bash  ", Some(&input));
    assert_eq!(said.voice, Voice::Tool);
    assert_eq!(said.text, "Bash: cargo test --lib");
    assert_eq!(tool_said("Read", None).text, "Read");

    // A reply longer than one line of the chat may hold is cut, not dropped.
    let long = "а".repeat(MAX_SAID_CHARS + 5);
    let said = Said::new(Voice::Agent, &long);
    assert_eq!(said.text.chars().count(), MAX_SAID_CHARS + 1);
    assert!(said.text.ends_with('…'));
}

/// Ward's own markers of a journal are told from the replies of the agent, whichever provider
/// wrote the transcript around them.
#[test]
fn wards_own_markers_are_not_replies() {
    let journal = concat!(
        "{\"type\":\"ward.role\",\"role\":\"orchestrator\",\"sessionId\":\"s-1\"}\n",
        "{\"type\":\"ward.restart\",\"role\":\"executor\",\"sessionId\":\"s-1\"}\n",
        "{\"type\":\"ward.something-new\"}\n",
    );
    for provider in all() {
        let chat = chat(provider, journal);
        let lines: Vec<(Voice, &str)> = chat
            .iter()
            .map(|said| (said.voice, said.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (Voice::Ward, "Дальше транскрипт: orchestrator"),
                (Voice::Ward, "Ward перезапустился и подхватил сессию"),
            ],
            "{}",
            provider.id()
        );
    }
}
