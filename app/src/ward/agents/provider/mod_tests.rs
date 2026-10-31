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
