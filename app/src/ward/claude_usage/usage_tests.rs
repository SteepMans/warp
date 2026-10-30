use chrono::FixedOffset;

use super::*;

fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn usage(five_hour: Option<f64>, seven_day: Option<f64>) -> Usage {
    Usage {
        five_hour: UsageWindow {
            utilization: five_hour,
            resets_at: None,
        },
        seven_day: UsageWindow {
            utilization: seven_day,
            resets_at: None,
        },
    }
}

#[test]
fn reads_the_access_token() {
    let json = r#"{"claudeAiOauth":{"accessToken":" sk-ant-oat01-abc ","refreshToken":"r",
        "expiresAt":1761840000000,"scopes":["user:inference"]},"other":1}"#;
    assert_eq!(parse_access_token(json).as_deref(), Some("sk-ant-oat01-abc"));
}

#[test]
fn missing_or_empty_token_is_none() {
    assert_eq!(parse_access_token("{}"), None);
    assert_eq!(parse_access_token(r#"{"claudeAiOauth":{}}"#), None);
    assert_eq!(
        parse_access_token(r#"{"claudeAiOauth":{"accessToken":"  "}}"#),
        None
    );
    assert_eq!(
        parse_access_token(r#"{"claudeAiOauth":{"accessToken":null}}"#),
        None
    );
    assert_eq!(parse_access_token("not json"), None);
}

#[test]
fn parses_a_full_response() {
    let json = r#"{
        "five_hour": {"utilization": 2.0, "resets_at": "2026-10-30T18:00:00.512345+00:00"},
        "seven_day": {"utilization": 60, "resets_at": "2026-11-03T10:00:00+00:00"},
        "seven_day_opus": {"utilization": null, "resets_at": null},
        "extra_usage": {"is_enabled": false}
    }"#;
    let usage = parse_usage(json).unwrap();
    assert_eq!(usage.five_hour.utilization, Some(2.));
    assert_eq!(
        usage.five_hour.resets_at,
        Some(utc("2026-10-30T18:00:00.512345+00:00"))
    );
    assert_eq!(usage.seven_day.utilization, Some(60.));
    assert_eq!(
        usage.seven_day.resets_at,
        Some(utc("2026-11-03T10:00:00+00:00"))
    );
}

#[test]
fn parsing_is_lenient() {
    let usage = parse_usage(
        r#"{"five_hour": {"utilization": null, "resets_at": null},
            "seven_day": {"utilization": "42.5%", "resets_at": 1761840000}}"#,
    )
    .unwrap();
    assert_eq!(usage.five_hour, UsageWindow::default());
    assert_eq!(usage.seven_day.utilization, Some(42.5));
    assert_eq!(
        usage.seven_day.resets_at,
        Utc.timestamp_opt(1_761_840_000, 0).single()
    );

    let usage = parse_usage(r#"{"five_hour": {"utilization": 5, "resets_at": "soon"}}"#).unwrap();
    assert_eq!(usage.five_hour.utilization, Some(5.));
    assert_eq!(usage.five_hour.resets_at, None);
    assert_eq!(usage.seven_day, UsageWindow::default());

    let usage = parse_usage(r#"{"five_hour": {"utilization": -3}, "seven_day": 7}"#).unwrap();
    assert_eq!(usage.five_hour.utilization, Some(0.));
    assert_eq!(usage.seven_day, UsageWindow::default());
}

#[test]
fn response_without_utilization_is_none() {
    assert_eq!(parse_usage("{}"), None);
    assert_eq!(parse_usage("[]"), None);
    assert_eq!(parse_usage("garbage"), None);
    assert_eq!(
        parse_usage(r#"{"five_hour": null, "seven_day": {"utilization": null}}"#),
        None
    );
    assert_eq!(
        parse_usage(r#"{"error": {"type": "rate_limit_error"}}"#),
        None
    );
}

#[test]
fn chip_text_shows_both_windows() {
    assert_eq!(
        chip_text(&usage(Some(2.), Some(60.))).as_deref(),
        Some("5h 2% \u{b7} 7d 60%")
    );
    assert_eq!(
        chip_text(&usage(Some(12.6), Some(0.4))).as_deref(),
        Some("5h 13% \u{b7} 7d 0%")
    );
    assert_eq!(chip_text(&usage(None, Some(60.))).as_deref(), Some("7d 60%"));
    assert_eq!(chip_text(&usage(Some(100.), None)).as_deref(), Some("5h 100%"));
    assert_eq!(chip_text(&usage(None, None)), None);
}

#[test]
fn levels_follow_the_thresholds() {
    assert_eq!(UsageLevel::of(0.), UsageLevel::Normal);
    assert_eq!(UsageLevel::of(49.9), UsageLevel::Normal);
    assert_eq!(UsageLevel::of(50.), UsageLevel::Warning);
    assert_eq!(UsageLevel::of(80.), UsageLevel::Warning);
    assert_eq!(UsageLevel::of(80.1), UsageLevel::Critical);
    assert_eq!(UsageLevel::of(130.), UsageLevel::Critical);
}

#[test]
fn chip_level_is_the_busiest_window() {
    assert_eq!(usage_level(&usage(Some(2.), Some(60.))), UsageLevel::Warning);
    assert_eq!(usage_level(&usage(Some(95.), Some(10.))), UsageLevel::Critical);
    assert_eq!(usage_level(&usage(Some(5.), None)), UsageLevel::Normal);
    assert_eq!(usage_level(&usage(None, None)), UsageLevel::Normal);
}

#[test]
fn resets_are_shown_in_local_time() {
    let moscow = FixedOffset::east_opt(3 * 3600).unwrap();
    let now = utc("2026-10-30T09:00:00Z");
    assert_eq!(
        format_reset(utc("2026-10-30T15:00:00Z"), now, &moscow),
        "today 18:00"
    );
    // 22:30 UTC is already the next day in Moscow.
    assert_eq!(
        format_reset(utc("2026-10-30T22:30:00Z"), now, &moscow),
        "Sat 31 Oct 01:30"
    );
    assert_eq!(
        format_reset(utc("2026-11-03T07:00:00Z"), now, &moscow),
        "Tue 03 Nov 10:00"
    );
}

#[test]
fn tooltip_lists_windows_with_resets() {
    let utc_tz = Utc;
    let now = utc("2026-10-30T09:00:00Z");
    let usage = Usage {
        five_hour: UsageWindow {
            utilization: Some(2.),
            resets_at: Some(utc("2026-10-30T14:00:00Z")),
        },
        seven_day: UsageWindow {
            utilization: Some(60.),
            resets_at: None,
        },
    };
    assert_eq!(
        tooltip_text(&usage, now, &utc_tz),
        "5-hour limit: 2% used, resets today 14:00\n7-day limit: 60% used"
    );
}

#[test]
fn statuses_map_to_outcomes() {
    assert_eq!(PollOutcome::from_status(200), PollOutcome::Fetched);
    assert_eq!(PollOutcome::from_status(401), PollOutcome::Unauthorized);
    assert_eq!(PollOutcome::from_status(403), PollOutcome::Unauthorized);
    assert_eq!(PollOutcome::from_status(429), PollOutcome::Failed);
    assert_eq!(PollOutcome::from_status(500), PollOutcome::Failed);
    assert_eq!(PollOutcome::from_status(404), PollOutcome::Failed);
}

#[test]
fn backoff_doubles_on_failures_and_resets() {
    let mut backoff = Backoff::default();
    assert_eq!(backoff.next_delay(PollOutcome::Fetched), POLL_INTERVAL);
    assert_eq!(
        backoff.next_delay(PollOutcome::Failed),
        Duration::from_secs(10 * 60)
    );
    assert_eq!(
        backoff.next_delay(PollOutcome::Failed),
        Duration::from_secs(20 * 60)
    );
    assert_eq!(backoff.next_delay(PollOutcome::Failed), MAX_INTERVAL);
    assert_eq!(backoff.next_delay(PollOutcome::Failed), MAX_INTERVAL);
    assert_eq!(backoff.next_delay(PollOutcome::Unauthorized), POLL_INTERVAL);
    backoff.next_delay(PollOutcome::Failed);
    assert_eq!(backoff.next_delay(PollOutcome::Fetched), POLL_INTERVAL);
}
