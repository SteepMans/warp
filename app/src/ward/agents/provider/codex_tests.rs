use chrono::TimeZone as _;

use super::*;

/// A `token_count` line as Codex CLI 0.148 writes it (`~/.codex/sessions/.../rollout-*.jsonl`).
const TOKEN_COUNT: &str = r#"{"timestamp":"2026-08-18T05:10:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":13.0,"window_minutes":10080,"resets_at":1787550558},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"individual_limit":null,"spend_control_reached":null,"plan_type":"plus","rate_limit_reached_type":null}}}"#;

const SESSION_META: &str = r#"{"timestamp":"2026-08-18T04:59:11.328Z","ordinal":0,"type":"session_meta","payload":{"session_id":"01a01337-8da8","id":"01a01337-8da8","timestamp":"2026-08-18T04:59:11Z","cwd":"C:\\agents-work\\o\\r\\.worktrees\\5f0e8a5b","originator":"codex_cli_rs","cli_version":"0.148.0"}}"#;

#[test]
fn parses_rate_limits_of_token_count() {
    let limits = parse_rate_limits(TOKEN_COUNT).unwrap();
    assert_eq!(
        limits.primary,
        Some(Window {
            used_percent: Some(13.0),
            window_minutes: Some(10080),
            resets_at: Utc.timestamp_opt(1787550558, 0).single(),
        })
    );
    assert_eq!(limits.secondary, None);
    assert_eq!(limits.plan_type.as_deref(), Some("plus"));
    assert_eq!(limits.reached_type, None);
    assert_eq!(
        limits.observed_at,
        Some(Utc.with_ymd_and_hms(2026, 8, 18, 5, 10, 0).unwrap())
    );
    assert!(parse_rate_limits(SESSION_META).is_none());
    assert!(
        parse_rate_limits(
            r#"{"type":"event_msg","payload":{"type":"agent_message","text":"\"rate_limits\""}}"#
        )
        .is_none()
    );
}

#[test]
fn latest_rate_limits_wins() {
    let newer = TOKEN_COUNT.replace("13.0", "55.0");
    let text = format!("{TOKEN_COUNT}\n{newer}\n{SESSION_META}\n");
    let limits = latest_rate_limits(&text).unwrap();
    assert_eq!(limits.primary.unwrap().used_percent, Some(55.0));
}

#[test]
fn usage_reports_name_primary_and_secondary() {
    let with_secondary = TOKEN_COUNT.replace(
        r#""secondary":null"#,
        r#""secondary":{"used_percent":80.5,"window_minutes":300,"resets_at":1787500000}"#,
    );
    let limits = parse_rate_limits(&with_secondary).unwrap();
    let reports = usage_reports(Some(&limits));
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].provider, "codex");
    assert_eq!(reports[0].window, "primary");
    assert_eq!(reports[0].window_minutes, Some(10080));
    assert_eq!(reports[0].used_percent, Some(13.0));
    assert_eq!(reports[0].source, "rollout");
    assert_eq!(
        reports[0].observed_at.as_deref(),
        Some("2026-08-18T05:10:00Z")
    );
    assert_eq!(reports[1].window, "secondary");
    assert_eq!(reports[1].used_percent, Some(80.5));

    let unknown = usage_reports(None);
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].used_percent, None);
    assert_eq!(unknown[0].source, "none");
}

#[test]
fn limit_hit_from_error_event_or_reached_type() {
    let started = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
    let error = r#"{"type":"event_msg","payload":{"type":"error","message":"You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), or try again at 5:00 PM."}}"#;
    let full = TOKEN_COUNT.replace("13.0", "100.0");
    let tail = format!("{started}\n{full}\n{error}\n");
    assert_eq!(
        parse_limit_hit(&tail),
        Some(LimitHit {
            resets_at: Utc.timestamp_opt(1787550558, 0).single()
        })
    );

    let reached = TOKEN_COUNT.replace(
        r#""rate_limit_reached_type":null"#,
        r#""rate_limit_reached_type":"primary""#,
    );
    let tail = format!("{started}\n{reached}\n");
    assert_eq!(parse_limit_hit(&tail), Some(LimitHit { resets_at: None }));

    // A refusal before the last turn started does not count.
    let tail = format!("{error}\n{started}\n{TOKEN_COUNT}\n");
    assert_eq!(parse_limit_hit(&tail), None);
    assert_eq!(
        parse_limit_hit(&format!("{started}\n{TOKEN_COUNT}\n")),
        None
    );
}

#[test]
fn account_skips_token_values() {
    let chatgpt = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"x","access_token":"y","refresh_token":"z","account_id":"a"},"last_refresh":"2026-10-01T00:00:00Z"}"#;
    assert_eq!(
        parse_account(Some(chatgpt), Some("plus".to_owned())),
        Account {
            logged_in: true,
            plan: Some("plus".to_owned())
        }
    );
    let api_key = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-x","tokens":null}"#;
    assert_eq!(
        parse_account(Some(api_key), Some("plus".to_owned())),
        Account {
            logged_in: true,
            plan: Some("api".to_owned())
        }
    );
    let logged_out = r#"{"OPENAI_API_KEY":null,"tokens":null}"#;
    assert_eq!(
        parse_account(Some(logged_out), Some("plus".to_owned())),
        Account::default()
    );
    assert_eq!(parse_account(None, None), Account::default());
}

#[test]
fn session_meta_gives_id_and_cwd() {
    assert_eq!(
        parse_session_meta(SESSION_META),
        Some((
            "01a01337-8da8".to_owned(),
            r"C:\agents-work\o\r\.worktrees\5f0e8a5b".to_owned()
        ))
    );
    assert!(parse_session_meta(TOKEN_COUNT).is_none());
}

#[test]
fn finds_the_rollout_of_the_worktree() {
    let home = tempfile::tempdir().unwrap();
    let homes = Homes {
        claude_dir: home.path().join(".claude"),
        claude_json: home.path().join(".claude.json"),
        codex_dir: home.path().join(".codex"),
    };
    let worktree = home.path().join("wt");
    let day = homes
        .codex_dir
        .join("sessions")
        .join("2026")
        .join("10")
        .join("31");
    std::fs::create_dir_all(&day).unwrap();
    let meta = |id: &str, cwd: &std::path::Path| {
        serde_json::json!({
            "type": "session_meta",
            "payload": {"id": id, "cwd": cwd.to_string_lossy()}
        })
        .to_string()
    };
    std::fs::write(
        day.join("rollout-2026-10-31T10-00-00-other.jsonl"),
        meta("other", &home.path().join("elsewhere")) + "\n",
    )
    .unwrap();
    let rollout = day.join("rollout-2026-10-31T10-00-01-mine.jsonl");
    std::fs::write(
        &rollout,
        meta("mine", &worktree) + "\n" + TOKEN_COUNT + "\n",
    )
    .unwrap();

    let found = Codex.find_session(&homes, &worktree, 0).unwrap();
    assert_eq!(found.agent_session_id, "mine");
    assert_eq!(found.pid, None);
    assert_eq!(found.remote_control_url, None);
    assert_eq!(Codex.transcript(&homes, &worktree, &found), Some(rollout));

    let usage = Codex.usage(&homes, &WardUsage::default(), Utc::now());
    assert_eq!(usage[0].used_percent, Some(13.0));
    std::fs::write(
        homes.codex_dir.join("auth.json"),
        r#"{"auth_mode":"chatgpt","tokens":{"access_token":"y"}}"#,
    )
    .unwrap();
    assert_eq!(Codex.account(&homes).plan.as_deref(), Some("plus"));
}
