use chrono::FixedOffset;

use super::*;

fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn window(used_percent: f64, window_minutes: Option<i64>, resets_at: Option<&str>) -> RateWindow {
    RateWindow {
        used_percent,
        window_minutes,
        resets_at: resets_at.map(utc),
    }
}

const TOKEN_COUNT: &str = r#"{"timestamp":"2026-08-20T09:17:24.591Z","ordinal":10123,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1},"model_context_window":258400},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":94.0,"window_minutes":10080,"resets_at":1787550558},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":null}}}"#;

#[test]
fn parses_a_real_token_count_line() {
    let usage = latest_usage(TOKEN_COUNT, false).unwrap();
    assert_eq!(usage.updated_at, utc("2026-08-20T09:17:24.591Z"));
    assert_eq!(
        usage.windows,
        vec![RateWindow {
            used_percent: 94.,
            window_minutes: Some(10080),
            resets_at: Utc.timestamp_opt(1_787_550_558, 0).single(),
        }]
    );
}

#[test]
fn both_windows_are_sorted_shortest_first() {
    let line = r#"{"timestamp":"2026-10-30T10:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":30,"window_minutes":10080,"resets_at":1793000000},"secondary":{"used_percent":10.4,"window_minutes":300,"resets_at":1792000000}}}}"#;
    let usage = latest_usage(line, false).unwrap();
    assert_eq!(usage.windows.len(), 2);
    assert_eq!(usage.windows[0].window_minutes, Some(300));
    assert_eq!(usage.windows[1].window_minutes, Some(10080));
    assert_eq!(chip_text(&usage).as_deref(), Some("5h 10% \u{b7} 7d 30%"));
}

#[test]
fn resets_in_seconds_counts_from_the_line_time() {
    let line = r#"{"timestamp":"2026-10-30T10:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":5,"window_minutes":300,"resets_in_seconds":3600}}}}"#;
    let usage = latest_usage(line, false).unwrap();
    assert_eq!(
        usage.windows[0].resets_at,
        Some(utc("2026-10-30T11:00:00Z"))
    );
}

#[test]
fn the_last_line_with_rate_limits_wins() {
    let older = TOKEN_COUNT.replace("94.0", "90.0");
    let without_limits = r#"{"timestamp":"2026-08-20T09:18:00Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}"#;
    let message = r#"{"timestamp":"2026-08-20T09:19:00Z","type":"response_item","payload":{"type":"message","content":"rate_limits"}}"#;
    let text = format!("{older}\n{TOKEN_COUNT}\n{without_limits}\n{message}\n");
    let usage = latest_usage(&text, false).unwrap();
    assert_eq!(usage.windows[0].used_percent, 94.);
}

#[test]
fn the_codex_bucket_beats_other_buckets() {
    let other = TOKEN_COUNT
        .replace("\"codex\"", "\"codex_other\"")
        .replace("94.0", "3.0");
    let text = format!("{TOKEN_COUNT}\n{other}");
    assert_eq!(
        latest_usage(&text, false).unwrap().windows[0].used_percent,
        94.
    );
    // Only another bucket: shown as a fallback.
    assert_eq!(
        latest_usage(&other, false).unwrap().windows[0].used_percent,
        3.
    );
}

#[test]
fn a_cut_first_line_is_skipped() {
    let cut = &TOKEN_COUNT[40..];
    assert_eq!(latest_usage(cut, true), None);
    let text = format!("{cut}\n{TOKEN_COUNT}");
    assert!(latest_usage(&text, true).is_some());
}

#[test]
fn garbage_is_none() {
    assert_eq!(latest_usage("", false), None);
    assert_eq!(latest_usage("not json \"rate_limits\"", false), None);
    let no_percent = r#"{"timestamp":"2026-10-30T10:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"window_minutes":300},"secondary":null}}}"#;
    assert_eq!(latest_usage(no_percent, false), None);
}

#[test]
fn windows_past_their_reset_are_hidden() {
    let usage = CodexUsage {
        windows: vec![
            window(10., Some(300), Some("2026-10-30T12:00:00Z")),
            window(30., Some(10080), Some("2026-11-03T12:00:00Z")),
        ],
        updated_at: utc("2026-10-30T09:00:00Z"),
    };
    let now = utc("2026-10-30T11:00:00Z");
    assert_eq!(usage.current(now).unwrap().windows.len(), 2);

    let later = utc("2026-10-30T13:00:00Z");
    let current = usage.current(later).unwrap();
    assert_eq!(chip_text(&current).as_deref(), Some("7d 30%"));

    assert_eq!(usage.current(utc("2026-11-04T00:00:00Z")), None);
}

#[test]
fn windows_without_a_reset_expire_after_one_window() {
    let usage = CodexUsage {
        windows: vec![window(10., Some(300), None), window(1., None, None)],
        updated_at: utc("2026-10-30T09:00:00Z"),
    };
    let current = usage.current(utc("2026-10-30T13:59:00Z")).unwrap();
    assert_eq!(current.windows, vec![window(10., Some(300), None)]);
    assert_eq!(usage.current(utc("2026-10-30T14:01:00Z")), None);
}

#[test]
fn labels_use_the_window_length() {
    assert_eq!(window_label(Some(300)), "5h");
    assert_eq!(window_label(Some(10080)), "7d");
    assert_eq!(window_label(Some(1440)), "1d");
    assert_eq!(window_label(Some(90)), "90m");
    assert_eq!(window_label(None), "limit");
}

#[test]
fn level_is_the_busiest_window() {
    let usage = CodexUsage {
        windows: vec![window(10., Some(300), None), window(85., Some(10080), None)],
        updated_at: utc("2026-10-30T09:00:00Z"),
    };
    assert_eq!(usage_level(&usage), UsageLevel::Critical);
    let usage = CodexUsage {
        windows: vec![window(50., Some(300), None)],
        updated_at: utc("2026-10-30T09:00:00Z"),
    };
    assert_eq!(usage_level(&usage), UsageLevel::Warning);
}

#[test]
fn tooltip_has_resets_and_the_update_time() {
    let usage = CodexUsage {
        windows: vec![
            window(10., Some(300), Some("2026-10-30T15:00:00Z")),
            window(30., Some(10080), Some("2026-11-03T07:00:00Z")),
        ],
        updated_at: utc("2026-10-30T09:48:00Z"),
    };
    let tz = FixedOffset::east_opt(3 * 3600).unwrap();
    assert_eq!(
        tooltip_text(&usage, utc("2026-10-30T10:00:00Z"), &tz),
        "5-hour limit: 10% used, resets today 18:00\n\
         7-day limit: 30% used, resets Tue 03 Nov 10:00\n\
         Updated 12 min ago (last Codex turn)"
    );
}

#[test]
fn ages_are_rounded_down() {
    let now = utc("2026-10-30T10:00:00Z");
    assert_eq!(format_age(utc("2026-10-30T09:59:30Z"), now), "just now");
    assert_eq!(format_age(utc("2026-10-30T07:30:00Z"), now), "2 h ago");
    assert_eq!(format_age(utc("2026-10-27T10:00:00Z"), now), "3 d ago");
    // Clock skew: a report from the future is "just now".
    assert_eq!(format_age(utc("2026-10-30T10:05:00Z"), now), "just now");
}

#[test]
fn reads_the_newest_rollout_tail() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join("2026").join("10").join("30");
    std::fs::create_dir_all(&day).unwrap();
    let older_day = dir.path().join("2026").join("10").join("29");
    std::fs::create_dir_all(&older_day).unwrap();
    std::fs::write(older_day.join("notes.txt"), "x").unwrap();
    let path = day.join("rollout-2026-10-30T09-00-00-abc.jsonl");
    // Enough filler for the first tail read to miss the rate limits line.
    let filler = format!("{{\"payload\":\"{}\"}}\n", "x".repeat(300 * 1024));
    std::fs::write(&path, format!("{TOKEN_COUNT}\n{filler}")).unwrap();

    let rollouts = recent_rollouts(dir.path());
    assert_eq!(rollouts.len(), 1);
    assert_eq!(rollouts[0].0, path);
    let usage = read_latest_usage(&path).unwrap();
    assert_eq!(usage.windows[0].used_percent, 94.);

    assert!(recent_rollouts(&dir.path().join("missing")).is_empty());
}
