use std::path::Path;

use chrono::TimeZone as _;

use super::*;
use crate::ward::claude_usage::usage::{Usage, UsageWindow};

#[test]
fn session_file_is_parsed() {
    let file = parse_session_file(
        r#"{"pid":38392,"sessionId":"85612177-ea68","cwd":"C:\\work\\other\\brain-manager-bot","startedAt":1793386592989,"status":"busy","bridgeSessionId":"session_01DL"}"#,
    )
    .unwrap();
    assert_eq!(file.pid, 38392);
    assert_eq!(file.session_id, "85612177-ea68");
    assert_eq!(file.started_at, 1793386592989);
    assert_eq!(file.bridge_session_id.as_deref(), Some("session_01DL"));
    assert_eq!(
        remote_control_url(file.bridge_session_id.as_deref()).as_deref(),
        Some("https://claude.ai/code/session_01DL")
    );
    assert_eq!(remote_control_url(None), None);
    assert_eq!(remote_control_url(Some(" ")), None);
    assert!(parse_session_file(r#"{"pid":1}"#).is_none());
    assert!(parse_session_file("not json").is_none());
}

#[test]
fn picks_the_newest_session_of_the_worktree_since_launch() {
    let worktree = if cfg!(windows) {
        Path::new(r"C:\w\repo\.worktrees\5f0e8a5b")
    } else {
        Path::new("/w/repo/.worktrees/5f0e8a5b")
    };
    let cwd = worktree.to_string_lossy().into_owned();
    let file = |id: &str, cwd: &str, started_at: i64| SessionFile {
        pid: 1,
        session_id: id.to_owned(),
        cwd: cwd.to_owned(),
        started_at,
        bridge_session_id: None,
    };
    let files = [
        file("old", &cwd, 1_000),
        file("other-dir", "/elsewhere", 100_000),
        file("new", &cwd, 90_000),
        file("newer", &cwd, 95_000),
    ];
    assert_eq!(
        pick_session(&files, worktree, 92_000).map(|file| file.session_id.as_str()),
        Some("newer")
    );
    assert!(pick_session(&files, worktree, 200_000).is_none());
}

#[test]
fn project_slug_replaces_everything_but_letters_and_digits() {
    assert_eq!(
        project_slug(Path::new(r"C:\work\hassle online")),
        "C--work-hassle-online"
    );
    assert_eq!(
        project_slug(Path::new(
            "C:/agents-work/SteepMans/repo/.worktrees/5f0e8a5b"
        )),
        "C--agents-work-SteepMans-repo--worktrees-5f0e8a5b"
    );
}

#[test]
fn account_comes_from_oauth_account_or_api_key() {
    let logged_in = parse_account(
        Some(
            r#"{"oauthAccount":{"accountUuid":"u1","organizationType":"claude_max","planDisplayName":"Ultra"}}"#,
        ),
        false,
    );
    assert_eq!(
        logged_in,
        Account {
            logged_in: true,
            plan: Some("max".to_owned())
        }
    );
    let display_name = parse_account(
        Some(r#"{"oauthAccount":{"accountUuid":"u1","planDisplayName":"Pro"}}"#),
        false,
    );
    assert_eq!(display_name.plan.as_deref(), Some("pro"));
    assert_eq!(
        parse_account(Some(r#"{"projects":{}}"#), true),
        Account {
            logged_in: true,
            plan: Some("api".to_owned())
        }
    );
    assert_eq!(parse_account(Some("{}"), false), Account::default());
    assert_eq!(parse_account(None, false), Account::default());
}

#[test]
fn usage_comes_from_the_header_chip_report() {
    let now = Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap();
    let usage = Usage {
        five_hour: UsageWindow {
            utilization: Some(12.0),
            resets_at: Some(Utc.with_ymd_and_hms(2026, 10, 31, 15, 0, 0).unwrap()),
        },
        seven_day: UsageWindow {
            utilization: Some(40.5),
            resets_at: None,
        },
    };
    let reports = usage_reports(Some(&usage), now);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].window, "5h");
    assert_eq!(reports[0].window_minutes, Some(300));
    assert_eq!(reports[0].used_percent, Some(12.0));
    assert_eq!(
        reports[0].resets_at.as_deref(),
        Some("2026-10-31T15:00:00Z")
    );
    assert_eq!(
        reports[0].observed_at.as_deref(),
        Some("2026-10-31T12:00:00Z")
    );
    assert_eq!(reports[0].source, "oauth_usage");
    assert_eq!(reports[1].window, "week");
    assert_eq!(reports[1].used_percent, Some(40.5));

    let unknown = usage_reports(None, now);
    assert_eq!(unknown.len(), 2);
    assert!(unknown.iter().all(|report| report.used_percent.is_none()));
    assert!(unknown.iter().all(|report| report.source == "none"));
}

#[test]
fn limit_hit_from_rate_limit_api_error() {
    let tail = concat!(
        r#"{"type":"user","message":{"content":"go"}}"#,
        "\n",
        r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","quotaLimits":{"rateLimitType":"five_hour","resetsAt":1793466000},"message":{"content":[{"type":"text","text":"You've hit your limit"}]}}"#,
        "\n",
        r#"{"type":"system","subtype":"x"}"#,
        "\n",
    );
    assert_eq!(
        parse_limit_hit(tail),
        Some(LimitHit {
            resets_at: Utc.timestamp_opt(1793466000, 0).single()
        })
    );
}

#[test]
fn limit_hit_from_text_and_not_from_other_errors() {
    let text = r#"{"type":"assistant","isApiErrorMessage":true,"error":"unknown","message":{"content":[{"type":"text","text":"You've hit your limit · resets 5pm"}]}}"#;
    assert_eq!(parse_limit_hit(text), Some(LimitHit { resets_at: None }));

    let auth = r#"{"type":"assistant","isApiErrorMessage":true,"error":"authentication_failed","message":{"content":[{"type":"text","text":"Please run /login"}]}}"#;
    assert_eq!(parse_limit_hit(auth), None);

    // Transient rate limiting without quota limits is not the subscription limit.
    let transient = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"content":[{"type":"text","text":"Overloaded"}]}}"#;
    assert_eq!(parse_limit_hit(transient), None);

    // The agent went on after the limit: no refusal.
    let recovered = concat!(
        r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","quotaLimits":{"resetsAt":1}}"#,
        "\n",
        r#"{"type":"user","message":{"content":"again"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#,
        "\n",
    );
    assert_eq!(parse_limit_hit(recovered), None);
    assert_eq!(parse_limit_hit(""), None);
}

#[test]
fn finds_session_and_transcript_on_disk() {
    let home = tempfile::tempdir().unwrap();
    let homes = Homes {
        claude_dir: home.path().join(".claude"),
        claude_json: home.path().join(".claude.json"),
        codex_dir: home.path().join(".codex"),
    };
    let worktree = home.path().join("work").join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let sessions = homes.claude_dir.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let file = serde_json::json!({
        "pid": 4242,
        "sessionId": "abc-123",
        "cwd": worktree.to_string_lossy(),
        "startedAt": 10_000,
        "bridgeSessionId": "session_01X"
    });
    std::fs::write(sessions.join("4242.json"), file.to_string()).unwrap();
    std::fs::write(sessions.join("4242.abcdef.key"), "ignored").unwrap();

    let found = Claude.find_session(&homes, &worktree, 9_000).unwrap();
    assert_eq!(found.agent_session_id, "abc-123");
    assert_eq!(found.pid, Some(4242));
    assert_eq!(
        found.remote_control_url.as_deref(),
        Some("https://claude.ai/code/session_01X")
    );
    assert!(Claude.transcript(&homes, &worktree, &found).is_none());

    let project = homes
        .claude_dir
        .join("projects")
        .join(project_slug(&worktree));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("abc-123.jsonl"), "{}\n").unwrap();
    assert_eq!(
        Claude.transcript(&homes, &worktree, &found),
        Some(project.join("abc-123.jsonl"))
    );
}

#[test]
fn project_keys_use_forward_slashes_and_the_resolved_path() {
    let keys = project_keys(Path::new(
        r"C:\w\agents-work\SteepMans\bot\.worktrees\86d19c06",
    ));
    assert_eq!(
        keys[0],
        "C:/w/agents-work/SteepMans/bot/.worktrees/86d19c06"
    );
    assert!(!keys.iter().any(|key| key.contains('\\')), "{keys:?}");
    // An existing directory is also recorded as the file system spells it, which is what Claude
    // Code's own process reports as its working directory.
    let dir = tempfile::tempdir().unwrap();
    let keys = project_keys(dir.path());
    assert!(keys.contains(&forward_slashes(dir.path())), "{keys:?}");
    assert!(keys.len() <= 2, "{keys:?}");
}

#[test]
fn trust_is_added_without_touching_the_other_projects() {
    let mut root = serde_json::json!({
        "numStartups": 961,
        "projects": {
            "C:/work/other/bot": {"allowedTools": [], "hasTrustDialogAccepted": true},
        },
    });
    let keys = vec!["C:/w/wt".to_owned()];
    assert_eq!(with_trusted_folders(&mut root, &keys), Ok(true));
    assert_eq!(root["numStartups"], 961);
    assert_eq!(
        root["projects"]["C:/work/other/bot"]["hasTrustDialogAccepted"],
        true
    );
    let added = &root["projects"]["C:/w/wt"];
    assert_eq!(added["hasTrustDialogAccepted"], true);
    // The warning about CLAUDE.md includes from outside counts as shown, so it asks nothing,
    // and the includes stay unapproved.
    assert_eq!(added["hasClaudeMdExternalIncludesWarningShown"], true);
    assert_eq!(added["hasClaudeMdExternalIncludesApproved"], false);
    assert!(added["allowedTools"].is_array());
    assert!(added["mcpServers"].is_object());
    // Nothing left to do the second time.
    assert_eq!(with_trusted_folders(&mut root, &keys), Ok(false));
}

#[test]
fn trust_of_an_existing_entry_keeps_its_fields() {
    let mut root = serde_json::json!({
        "projects": {
            "C:/w/wt": {"allowedTools": ["Bash"], "hasTrustDialogAccepted": false, "lastCost": 1.5},
        },
    });
    let keys = vec!["C:/w/wt".to_owned()];
    assert_eq!(with_trusted_folders(&mut root, &keys), Ok(true));
    let entry = &root["projects"]["C:/w/wt"];
    assert_eq!(entry["hasTrustDialogAccepted"], true);
    assert_eq!(entry["allowedTools"][0], "Bash");
    assert_eq!(entry["lastCost"], 1.5);
}

#[test]
fn a_file_of_another_shape_is_refused() {
    let mut array = serde_json::json!([1, 2]);
    assert!(with_trusted_folders(&mut array, &["C:/w/wt".to_owned()]).is_err());
    let mut odd = serde_json::json!({"projects": 1});
    assert!(with_trusted_folders(&mut odd, &["C:/w/wt".to_owned()]).is_err());
}

#[test]
fn trust_file_is_written_whole_and_a_broken_one_is_left_alone() {
    let home = tempfile::tempdir().unwrap();
    let claude_json = home.path().join(".claude.json");
    let worktree = home.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();

    // No file yet: one is written with the entry alone.
    trust_folder(&claude_json, &worktree).unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let keys = project_keys(&worktree);
    for key in &keys {
        assert_eq!(written["projects"][key]["hasTrustDialogAccepted"], true);
    }

    // The owner's own fields and projects survive the next launch.
    std::fs::write(
        &claude_json,
        r#"{"oauthAccount":{"accountUuid":"u-1"},"projects":{"C:/work/other/bot":{"hasTrustDialogAccepted":true,"lastCost":2.25}}}"#,
    )
    .unwrap();
    trust_folder(&claude_json, &worktree).unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert_eq!(written["oauthAccount"]["accountUuid"], "u-1");
    assert_eq!(written["projects"]["C:/work/other/bot"]["lastCost"], 2.25);
    assert_eq!(
        written["projects"][&keys[0]]["hasTrustDialogAccepted"],
        true
    );
    assert!(!claude_json.with_extension("ward-tmp").exists());

    // A file that is not JSON is the owner's to fix: Ward refuses and writes nothing.
    std::fs::write(&claude_json, "{ broken").unwrap();
    assert!(trust_folder(&claude_json, &worktree).is_err());
    assert_eq!(std::fs::read_to_string(&claude_json).unwrap(), "{ broken");
}

/// One `assistant` line of a transcript with the content blocks given.
fn assistant(blocks: &str) -> String {
    format!(r#"{{"type":"assistant","message":{{"content":[{blocks}]}}}}"#)
}

#[test]
fn the_last_turn_tells_an_answer_from_a_tool_call_and_from_a_question() {
    let answered = assistant(r#"{"type":"text","text":"Готово, PR открыт"}"#);
    assert_eq!(parse_last_turn(&answered), LastTurn::Answered);
    // Thinking before the text does not hide the answer.
    let thinking = assistant(
        r#"{"type":"thinking","thinking":"..."},{"type":"text","text":"Готово, PR открыт"}"#,
    );
    assert_eq!(parse_last_turn(&thinking), LastTurn::Answered);
    // System, summary and snapshot lines after the answer do not hide it either.
    let meta = format!(
        "{answered}\n{}\n{}\n",
        r#"{"type":"summary","summary":"итог"}"#, r#"{"type":"system","content":"hook"}"#
    );
    assert_eq!(parse_last_turn(&meta), LastTurn::Answered);

    // A tool call waits for its result: the turn goes on.
    let tool = assistant(r#"{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}"#);
    assert_eq!(parse_last_turn(&tool), LastTurn::Working);
    // Text and then a tool call in one turn: the newest block decides.
    let both = assistant(
        r#"{"type":"text","text":"Сейчас проверю"},{"type":"tool_use","name":"Bash","input":{}}"#,
    );
    assert_eq!(parse_last_turn(&both), LastTurn::Working);
    // The result came, the agent still owes the answer to it.
    let result = format!(
        "{answered}\n{tool}\n{}\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#
    );
    assert_eq!(parse_last_turn(&result), LastTurn::Working);

    // The question tools wait for the owner, not for a tool.
    for name in ASK_TOOLS {
        let asked = assistant(&format!(
            r#"{{"type":"tool_use","name":"{name}","input":{{}}}}"#
        ));
        assert_eq!(parse_last_turn(&asked), LastTurn::Asked, "{name}");
    }

    // An API error in place of an answer is no answer, and a transcript that says nothing
    // keeps the session going.
    let error = r#"{"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"Overloaded"}]}}"#;
    assert_eq!(parse_last_turn(error), LastTurn::Working);
    assert_eq!(parse_last_turn(""), LastTurn::Working);
    assert_eq!(parse_last_turn("not json\n"), LastTurn::Working);
}
