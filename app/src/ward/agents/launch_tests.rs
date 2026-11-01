use std::path::Path;

use super::*;
use crate::ward::agents::provider;

fn repo(owner: &str, name: &str) -> RepoId {
    RepoId {
        owner: owner.to_owned(),
        name: name.to_owned(),
    }
}

#[test]
fn parses_repository_urls() {
    let expected = Some(repo("SteepMans", "brain-manager-bot"));
    assert_eq!(
        parse_repo("https://github.com/SteepMans/brain-manager-bot.git"),
        expected
    );
    assert_eq!(
        parse_repo("https://github.com/SteepMans/brain-manager-bot/"),
        expected
    );
    assert_eq!(
        parse_repo("git@github.com:SteepMans/brain-manager-bot.git"),
        expected
    );
    assert_eq!(
        parse_repo("github.com/SteepMans/brain-manager-bot"),
        expected
    );
    assert_eq!(
        parse_repo(r"C:\remotes\SteepMans\brain-manager-bot.git"),
        expected
    );
    assert_eq!(
        parse_repo("file:///tmp/remotes/SteepMans/brain-manager-bot.git"),
        expected
    );
    assert_eq!(
        parse_repo("https://github.com/brain-manager-bot"),
        Some(repo("github.com", "brain-manager-bot"))
    );
    assert_eq!(parse_repo("https://github.com/../x"), None);
    assert_eq!(parse_repo("https://github.com/a b/x"), None);
    assert_eq!(parse_repo("x"), None);
}

#[test]
fn allowed_repos_list() {
    let id = repo("SteepMans", "warp");
    assert!(is_repo_allowed(&id, ""));
    assert!(is_repo_allowed(&id, " steepmans/warp , a/b"));
    assert!(!is_repo_allowed(&id, "a/b"));
}

#[test]
fn session_paths() {
    let root = Path::new("/home/u/agents-work");
    let clone = clone_dir(root, &repo("SteepMans", "warp"));
    assert_eq!(clone, root.join("SteepMans").join("warp"));
    assert_eq!(
        worktree_dir(&clone, "5f0e8a5b-0c3e-4d2f-9a64-3b1e4f6d7a20"),
        Some(clone.join(".worktrees").join("5f0e8a5b"))
    );
    assert_eq!(worktree_dir(&clone, "../../etc"), None);
    assert_eq!(worktree_dir(&clone, ""), None);
    // A session on the owner's own clone keeps its worktree under the root.
    assert_eq!(
        external_worktree_dir(root, &repo("HassleGames", "game-server"), "5f0e8a5b-0c3e"),
        Some(
            root.join(".worktrees")
                .join("HassleGames")
                .join("game-server")
                .join("5f0e8a5b")
        )
    );
    assert_eq!(
        external_worktree_dir(root, &repo("HassleGames", "game-server"), "../x"),
        None
    );
    // The preparation phase keeps its files under the root: the orchestrator works in the
    // vault, and the repository may still be unknown.
    assert_eq!(
        prepare_dir(root, "5f0e8a5b-0c3e-4d2f-9a64-3b1e4f6d7a20"),
        Some(root.join(".agents").join("5f0e8a5b"))
    );
    assert_eq!(prepare_dir(root, "../../etc"), None);
}

#[test]
fn session_name_is_number_and_title() {
    let task = TaskRef {
        board_id: 5,
        task_id: 5008,
        task_number: Some(4623),
    };
    assert_eq!(
        session_name(&task, "Встречи:  followups\nв сводке"),
        "4623 Встречи: followups в сводке"
    );
    let task = TaskRef {
        task_number: None,
        ..task
    };
    assert_eq!(session_name(&task, ""), "5008");
}

#[test]
fn exclude_entries_are_added_once() {
    assert_eq!(
        exclude_with_entries("# git ls-files --others\n*.log"),
        Some("# git ls-files --others\n*.log\n/.agents/\n/.worktrees/\n".to_owned())
    );
    assert_eq!(exclude_with_entries("/.agents/\n/.worktrees/\n"), None);
    assert_eq!(
        exclude_with_entries("/.worktrees/\n"),
        Some("/.worktrees/\n/.agents/\n".to_owned())
    );
}

#[test]
fn quoting() {
    assert_eq!(pwsh_quote("it's"), "'it''s'");
    assert_eq!(sh_quote("it's"), r"'it'\''s'");
}

#[test]
fn pwsh_runner_clears_env_and_writes_the_exit_code() {
    let command = provider::by_id("claude")
        .unwrap()
        .command("4623 O'Neil", Some("claude-opus-5-5"));
    let script = runner_script(
        RunnerKind::Pwsh,
        Path::new("C:/w/t/.agents"),
        Path::new("C:/w/t"),
        &command,
    );
    let expected = [
        "Set-Content -LiteralPath 'C:/w/t/.agents/runner.pid' -Value $PID -NoNewline",
        "$_.Name.StartsWith('CLAUDE', [StringComparison]::OrdinalIgnoreCase)",
        "Set-Item -LiteralPath 'Env:CLAUDE_CODE_FORCE_SESSION_PERSISTENCE' -Value '1'",
        "Set-Location -LiteralPath 'C:/w/t'",
        "$prompt = Get-Content -Raw -Encoding UTF8 -LiteralPath 'C:/w/t/.agents/prompt.md'",
        "& 'claude' '--remote-control' '4623 O''Neil' '--dangerously-skip-permissions' '--model' 'claude-opus-5-5' -- $prompt",
        "Move-Item -LiteralPath 'C:/w/t/.agents/exit-code.tmp' -Destination 'C:/w/t/.agents/exit-code' -Force",
        "exit $code",
    ];
    for line in expected {
        assert!(script.contains(line), "missing {line:?} in\n{script}");
    }
    // The variables are cleared before they are set, and both before the agent starts.
    let clear = script.find("StartsWith('CLAUDE'").unwrap();
    let set = script.find("Set-Item").unwrap();
    let start = script.find("& 'claude'").unwrap();
    assert!(clear < set && set < start);
}

#[test]
fn sh_runner_for_codex() {
    let command = provider::by_id("codex").unwrap().command("1 x", None);
    let script = runner_script(
        RunnerKind::Sh,
        Path::new("/w/t/.agents"),
        Path::new("/w/t"),
        &command,
    );
    assert!(script.starts_with("#!/bin/sh\n"));
    assert!(script.contains("echo $$ > '/w/t/.agents/runner.pid'"));
    assert!(
        script
            .contains(r"for k in $(env | sed -n 's/^\(CLAUDE[A-Za-z0-9_]*\)=.*/\1/p'); do unset ")
    );
    assert!(script.contains("cd '/w/t' || exit 1"));
    assert!(script.contains("prompt=$(cat '/w/t/.agents/prompt.md')"));
    assert!(script.contains("'codex' '--dangerously-bypass-approvals-and-sandbox' -- \"$prompt\""));
    assert!(script.contains("mv -f '/w/t/.agents/exit-code.tmp' '/w/t/.agents/exit-code'"));
}

#[test]
fn tab_command_per_runner() {
    assert_eq!(
        tab_command(RunnerKind::Pwsh, Path::new(r"C:\w\t\.agents\run.ps1"), None),
        r#"pwsh -NoProfile -ExecutionPolicy Bypass -File "C:/w/t/.agents/run.ps1""#
    );
    assert_eq!(
        tab_command(
            RunnerKind::Sh,
            Path::new("C:/w/t/.agents/run.sh"),
            Some(Path::new(r"C:\Program Files\Git\bin\bash.exe"))
        ),
        r#"& "C:/Program Files/Git/bin/bash.exe" "C:/w/t/.agents/run.sh""#
    );
    assert_eq!(
        tab_command(RunnerKind::Sh, Path::new("/w/t/.agents/run.sh"), None),
        r#"sh "/w/t/.agents/run.sh""#
    );
}

#[test]
fn exit_code_file() {
    assert_eq!(parse_exit_code("0"), Some(0));
    assert_eq!(parse_exit_code(" 1\r\n"), Some(1));
    assert_eq!(parse_exit_code("-1073741510"), Some(-1073741510));
    assert_eq!(parse_exit_code(""), None);
}

/// Runs the pwsh runner for real with a fake agent and checks what the agent got.
#[cfg(windows)]
#[test]
fn pwsh_runner_end_to_end() {
    if crate::ward::agents::machine::which("pwsh").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path();
    let agents = agents_dir(worktree);
    std::fs::create_dir_all(&agents).unwrap();
    // The fake agent reports its argv and environment, then exits with 3.
    let agent = agents.join("fake-agent.ps1");
    std::fs::write(
        &agent,
        "Set-Content -LiteralPath (Join-Path $PSScriptRoot 'seen.txt') -Encoding UTF8 -Value (($args -join '|') + \"`n\" + [string]$env:CLAUDE_CODE_CHILD_SESSION + '|' + [string]$env:CLAUDE_CODE_FORCE_SESSION_PERSISTENCE)\nexit 3\n",
    )
    .unwrap();
    std::fs::write(agents.join(PROMPT_FILE), "-first line\nsecond line").unwrap();
    let command = provider::AgentCommand {
        program: agent.to_string_lossy().into_owned(),
        args: vec!["--flag".to_owned()],
        remove_env_prefixes: vec!["CLAUDE"],
        set_env: vec![(
            "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE".to_owned(),
            "1".to_owned(),
        )],
    };
    let runner = agents.join(RunnerKind::Pwsh.file_name());
    std::fs::write(
        &runner,
        runner_script(RunnerKind::Pwsh, &agents, worktree, &command),
    )
    .unwrap();
    let status = command::blocking::Command::new("pwsh")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&runner)
        .env("CLAUDE_CODE_CHILD_SESSION", "1")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3));
    let exit_code = std::fs::read_to_string(agents.join(EXIT_CODE_FILE)).unwrap();
    assert_eq!(parse_exit_code(&exit_code), Some(3));
    let seen = std::fs::read_to_string(agents.join("seen.txt")).unwrap();
    let seen = seen.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    // A script called through `&` keeps `--` among its arguments; native agents get it as is.
    assert!(seen.contains("-first line\nsecond line"), "{seen}");
    assert!(seen.starts_with("--flag|"), "{seen}");
    assert!(seen.trim_end().ends_with("|1"), "{seen}");
    assert!(
        !seen.contains("\n1|"),
        "CLAUDE_CODE_CHILD_SESSION leaked: {seen}"
    );
    assert!(agents.join(RUNNER_PID_FILE).is_file());
}
