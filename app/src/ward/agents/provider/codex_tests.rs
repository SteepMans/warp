use chrono::TimeZone as _;

use super::*;

/// A `token_count` line as Codex CLI 0.148 writes it (`~/.codex/sessions/.../rollout-*.jsonl`).
const TOKEN_COUNT: &str = r#"{"timestamp":"2026-08-18T05:10:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":13.0,"window_minutes":10080,"resets_at":1787550558},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"individual_limit":null,"spend_control_reached":null,"plan_type":"plus","rate_limit_reached_type":null}}}"#;

const SESSION_META: &str = r#"{"timestamp":"2026-08-18T04:59:11.328Z","ordinal":0,"type":"session_meta","payload":{"session_id":"01a01337-8da8","id":"01a01337-8da8","timestamp":"2026-08-18T04:59:11Z","cwd":"C:\\agents-work\\o\\r\\.worktrees\\5f0e8a5b","originator":"codex_cli_rs","cli_version":"0.148.0"}}"#;

/// The last `token_count` of a machine where Codex has not run since 20.08 (`info` left out):
/// 94% of the weekly window, which reset on 24.08 at 05:49:18 UTC.
const STALE_TOKEN_COUNT: &str = r#"{"timestamp":"2026-08-20T09:17:24.591Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":94.0,"window_minutes":10080,"resets_at":1787550558},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"individual_limit":null,"spend_control_reached":null,"plan_type":null,"rate_limit_reached_type":null}}}"#;

/// Before the reset of [`TOKEN_COUNT`] and [`STALE_TOKEN_COUNT`].
fn before_reset() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

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
    let reports = usage_reports(Some(&limits), before_reset());
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

    let unknown = usage_reports(None, before_reset());
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].used_percent, None);
    assert_eq!(unknown[0].source, "none");
}

#[test]
fn a_window_past_its_reset_is_reported_empty() {
    let limits = parse_rate_limits(STALE_TOKEN_COUNT).unwrap();
    let reports = usage_reports(Some(&limits), before_reset());
    assert_eq!(reports[0].used_percent, Some(94.0));
    assert_eq!(
        reports[0].resets_at.as_deref(),
        Some("2026-08-24T05:49:18Z")
    );

    // Months later Codex has not run again: the window started over at its reset, and what is
    // used since is unknown until the next Codex turn, so is the next reset.
    let now = Utc.with_ymd_and_hms(2026, 11, 5, 6, 48, 0).unwrap();
    let reports = usage_reports(Some(&limits), now);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].window, "primary");
    assert_eq!(reports[0].window_minutes, Some(10080));
    assert_eq!(reports[0].used_percent, Some(0.0));
    assert_eq!(reports[0].resets_at, None);
    assert_eq!(
        reports[0].observed_at.as_deref(),
        Some("2026-08-24T05:49:18Z")
    );
    assert_eq!(reports[0].source, "rollout");

    // The moment of the reset already belongs to the new window.
    let at_reset = Utc.timestamp_opt(1787550558, 0).unwrap();
    assert_eq!(
        usage_reports(Some(&limits), at_reset)[0].used_percent,
        Some(0.0)
    );
}

#[test]
fn only_the_window_that_started_over_is_emptied() {
    // The 5-hour window reset an hour ago, the weekly one still runs.
    let line = TOKEN_COUNT.replace(
        r#""primary":{"used_percent":13.0,"window_minutes":10080,"resets_at":1787550558},"secondary":null"#,
        r#""primary":{"used_percent":97.0,"window_minutes":300,"resets_at":1787500000},"secondary":{"used_percent":60.0,"window_minutes":10080,"resets_at":1787550558}"#,
    );
    let limits = parse_rate_limits(&line).unwrap();
    let now = Utc.timestamp_opt(1787500000 + 3600, 0).unwrap();
    let reports = usage_reports(Some(&limits), now);
    assert_eq!(reports[0].window, "primary");
    assert_eq!(reports[0].used_percent, Some(0.0));
    assert_eq!(reports[0].resets_at, None);
    assert_eq!(reports[1].window, "secondary");
    assert_eq!(reports[1].used_percent, Some(60.0));
    assert_eq!(
        reports[1].resets_at.as_deref(),
        Some("2026-08-24T05:49:18Z")
    );
    assert_eq!(
        reports[1].observed_at.as_deref(),
        Some("2026-08-18T05:10:00Z")
    );
}

#[test]
fn a_window_without_reset_time_starts_over_one_window_after_the_report() {
    let line = TOKEN_COUNT.replace(
        r#""window_minutes":10080,"resets_at":1787550558"#,
        r#""window_minutes":300"#,
    );
    let limits = parse_rate_limits(&line).unwrap();
    // Reported at 05:10; a 5-hour window cannot outlast 10:10.
    let still = Utc.with_ymd_and_hms(2026, 8, 18, 10, 9, 0).unwrap();
    assert_eq!(
        usage_reports(Some(&limits), still)[0].used_percent,
        Some(13.0)
    );
    let over = Utc.with_ymd_and_hms(2026, 8, 18, 10, 10, 0).unwrap();
    let reports = usage_reports(Some(&limits), over);
    assert_eq!(reports[0].used_percent, Some(0.0));
    assert_eq!(reports[0].resets_at, None);
    assert_eq!(reports[0].observed_at, None);

    // Neither a reset time nor a window length: nothing to tell an old report by.
    let bare = TOKEN_COUNT.replace(
        r#""window_minutes":10080,"resets_at":1787550558"#,
        r#""window_minutes":null"#,
    );
    let limits = parse_rate_limits(&bare).unwrap();
    let much_later = Utc.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap();
    assert_eq!(
        usage_reports(Some(&limits), much_later)[0].used_percent,
        Some(13.0)
    );
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

/// Codex 0.147 and later end a refused turn with the error on `task_complete`, the refusal for
/// the limit included; any other error there is no limit.
#[test]
fn limit_hit_from_the_error_of_task_complete() {
    let started = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
    let full = TOKEN_COUNT.replace("13.0", "100.0");
    let by_message = r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":null,"error":{"message":"You've hit your usage limit. Try again at 5:00 PM.","codex_error_info":"other"}}}"#;
    assert_eq!(
        parse_limit_hit(&format!("{started}\n{full}\n{by_message}\n")),
        Some(LimitHit {
            resets_at: Utc.timestamp_opt(1787550558, 0).single()
        })
    );
    let by_kind = r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":null,"error":{"message":"","codex_error_info":"usage_limit_exceeded"}}}"#;
    assert_eq!(
        parse_limit_hit(&format!("{started}\n{by_kind}\n")),
        Some(LimitHit { resets_at: None })
    );
    assert_eq!(
        parse_limit_hit(&format!("{started}\n{MODEL_REFUSED}\n")),
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

    let usage = Codex.usage(&homes, &WardUsage::default(), before_reset());
    assert_eq!(usage[0].used_percent, Some(13.0));
    // The same rollout read after the reset of its window.
    let after_reset = Utc.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap();
    let usage = Codex.usage(&homes, &WardUsage::default(), after_reset);
    assert_eq!(usage[0].used_percent, Some(0.0));
    assert_eq!(usage[0].resets_at, None);
    std::fs::write(
        homes.codex_dir.join("auth.json"),
        r#"{"auth_mode":"chatgpt","tokens":{"access_token":"y"}}"#,
    )
    .unwrap();
    assert_eq!(Codex.account(&homes).plan.as_deref(), Some("plus"));
}

#[test]
fn the_last_turn_of_a_rollout_ends_with_the_task() {
    let started = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
    let call = r#"{"type":"response_item","payload":{"type":"function_call","name":"shell"}}"#;
    let answer = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Готово"}}"#;
    let complete = r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#;
    assert_eq!(parse_last_turn(started), LastTurn::Working);
    assert_eq!(
        parse_last_turn(&format!("{started}\n{call}\n")),
        LastTurn::Working
    );
    assert_eq!(
        parse_last_turn(&format!("{started}\n{call}\n{answer}\n{complete}\n")),
        LastTurn::Answered
    );
    // A token count after the turn does not hide its end.
    assert_eq!(
        parse_last_turn(&format!("{complete}\n{TOKEN_COUNT}\n")),
        LastTurn::Answered
    );
    // A new turn after the answer: the agent owes a new one.
    assert_eq!(
        parse_last_turn(&format!("{complete}\n{started}\n")),
        LastTurn::Working
    );
    assert_eq!(parse_last_turn(""), LastTurn::Working);
}

/// The `task_complete` of a turn Codex CLI 0.148 could not even start: the model of the plan
/// was a Claude one, and the API's answer is kept in the error as it came.
const MODEL_REFUSED: &str = r#"{"timestamp":"2026-11-05T09:37:05.571Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"01a1ab11-74cc","last_agent_message":null,"error":{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'claude-sonnet-5-5' model is not supported when using Codex with a ChatGPT account.\",\"code\":\"model_not_found\"}}","codex_error_info":"other"},"started_at":1793871410,"completed_at":1793871425,"duration_ms":15183}}"#;

/// A turn that ended on an error of the API is no answer, whichever way Codex wrote it down.
#[test]
fn a_turn_that_ended_on_an_error_is_no_answer() {
    let started = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
    assert_eq!(
        parse_last_turn(&format!("{started}\n{MODEL_REFUSED}\n{TOKEN_COUNT}\n")),
        LastTurn::Failed(
            "The 'claude-sonnet-5-5' model is not supported when using Codex with a ChatGPT \
             account. (model_not_found)"
                .to_owned()
        )
    );
    // An error that is no answer of the API stays as it is.
    let dropped = r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":null,"error":{"message":"stream disconnected before completion","codex_error_info":"other"}}}"#;
    assert_eq!(
        parse_last_turn(&format!("{started}\n{dropped}\n")),
        LastTurn::Failed("stream disconnected before completion".to_owned())
    );

    // Codex before 0.147: an `error` event right before a `task_complete` without the last
    // message of the agent.
    let error = r#"{"type":"event_msg","payload":{"type":"error","message":"unexpected status 400 Bad Request"}}"#;
    let complete =
        r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":null}}"#;
    assert_eq!(
        parse_last_turn(&format!("{started}\n{error}\n{TOKEN_COUNT}\n{complete}\n")),
        LastTurn::Failed("unexpected status 400 Bad Request".to_owned())
    );

    // An error the turn went on after is not where it ended.
    let call = r#"{"type":"response_item","payload":{"type":"function_call","name":"shell"}}"#;
    let answer = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Готово"}}"#;
    assert_eq!(
        parse_last_turn(&format!(
            "{started}\n{error}\n{call}\n{answer}\n{complete}\n"
        )),
        LastTurn::Answered
    );
    assert_eq!(
        parse_last_turn(&format!("{started}\n{error}\n{call}\n")),
        LastTurn::Working
    );
    let answered =
        r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Готово"}}"#;
    assert_eq!(
        parse_last_turn(&format!("{started}\n{error}\n{answered}\n")),
        LastTurn::Answered
    );
    // A turn of tools that ended without a message or an error is done all the same.
    assert_eq!(
        parse_last_turn(&format!("{started}\n{call}\n{complete}\n")),
        LastTurn::Answered
    );
}

#[test]
fn an_error_of_the_api_reads_as_its_message() {
    assert_eq!(
        readable_error(
            r#"{"type":"error","status":400,"error":{"message":"Bad model","code":"model_not_found"}}"#
        ),
        "Bad model (model_not_found)"
    );
    assert_eq!(readable_error(r#"{"message":"Overloaded"}"#), "Overloaded");
    assert_eq!(readable_error(r#"{"status":500}"#), r#"{"status":500}"#);
    assert_eq!(readable_error("  plain text  "), "plain text");
    assert_eq!(readable_error(""), "");
}

/// The error a turn ended on is Ward's line of the chat, in either shape Codex wrote it.
#[test]
fn the_error_of_a_turn_is_a_line_of_wards_in_the_chat() {
    let said = |line: &str| parse_said(&serde_json::from_str(line).unwrap());
    assert_eq!(
        said(MODEL_REFUSED),
        [Said::new(
            Voice::Ward,
            "Ошибка агента: The 'claude-sonnet-5-5' model is not supported when using Codex \
             with a ChatGPT account. (model_not_found)"
        )]
    );
    assert_eq!(
        said(r#"{"type":"event_msg","payload":{"type":"error","message":"stream disconnected"}}"#),
        [Said::new(Voice::Ward, "Ошибка агента: stream disconnected")]
    );
    // A turn that ended well says nothing of its own.
    assert!(
        said(r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Готово"}}"#)
            .is_empty()
    );
}

#[test]
fn child_threads_are_not_the_session() {
    let root = r#"{"type":"session_meta","payload":{"id":"01a09473-8407-7391-a64f-dc3385603d60","session_id":"01a09473-8407-7391-a64f-dc3385603d60","cwd":"C:\w","source":"cli","thread_source":"user"}}"#;
    let guardian = r#"{"type":"session_meta","payload":{"id":"01a1d412-f5f5-7e50-b215-aed4851f118e","session_id":"01a09473-8407-7391-a64f-dc3385603d60","cwd":"C:\w","source":{"subagent":"guardian"}}}"#;
    let sub_agent = r#"{"type":"session_meta","payload":{"id":"01a1e000-0000-7000-8000-000000000001","parent_thread_id":"01a09473-8407-7391-a64f-dc3385603d60","cwd":"C:\w","thread_source":"subagent"}}"#;
    assert!(!is_child_thread(root));
    assert!(is_child_thread(guardian));
    assert!(is_child_thread(sub_agent));
    assert!(!is_child_thread(TOKEN_COUNT));
}
