use chrono::TimeZone;

use super::*;

fn at(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, hour, 0, 0).unwrap()
}

fn announcement(id: &str) -> Announcement {
    Announcement {
        id: id.to_owned(),
        title: format!("Title {id}"),
        body: String::new(),
        level: Level::Info,
        created_at: None,
        expires_at: None,
        min_version: None,
        max_version: None,
        url: None,
    }
}

fn bounded(min: Option<&str>, max: Option<&str>) -> Announcement {
    Announcement {
        min_version: min.map(str::to_owned),
        max_version: max.map(str::to_owned),
        ..announcement("a")
    }
}

const BUILD: &str = "v0.2026.10.29.12.00.oss_00";

#[test]
fn parses_the_object_form() {
    let json = r#"{"schema":1,"announcements":[{
        "id":"3f0c2a9e","title":"Короткий заголовок","body":"Текст","level":"warning",
        "created_at":"2026-10-30T10:00:00Z","expires_at":null,"min_version":null,
        "max_version":"0.2026.10.29","url":"https://example.com/a","extra":{"x":1}
    }]}"#;
    let parsed = parse_feed(json).unwrap();
    assert_eq!(
        parsed,
        vec![Announcement {
            id: "3f0c2a9e".to_owned(),
            title: "Короткий заголовок".to_owned(),
            body: "Текст".to_owned(),
            level: Level::Warning,
            created_at: Some(at(30, 10)),
            expires_at: None,
            min_version: None,
            max_version: Some("0.2026.10.29".to_owned()),
            url: Some("https://example.com/a".to_owned()),
        }]
    );
}

#[test]
fn parses_a_bare_array() {
    let parsed = parse_feed(r#"[{"id":"a","title":"A"},{"id":"b","title":"B"}]"#).unwrap();
    let ids: Vec<_> = parsed.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(parsed[0].body, "");
    assert_eq!(parsed[0].level, Level::Info);
}

#[test]
fn rejects_bodies_that_are_not_a_feed() {
    assert_eq!(parse_feed("not json"), None);
    assert_eq!(parse_feed(r#"{"schema":1}"#), None);
    assert_eq!(parse_feed(r#"{"announcements":{}}"#), None);
    assert_eq!(parse_feed("42"), None);
    let huge = format!("[{}]", " ".repeat(MAX_BODY_BYTES));
    assert_eq!(parse_feed(&huge), None);
    assert_eq!(parse_feed(r#"{"announcements":[]}"#), Some(vec![]));
}

#[test]
fn skips_malformed_items_and_keeps_the_rest() {
    let json = r#"[
        "text",
        {"title":"no id"},
        {"id":"","title":"empty id"},
        {"id":7,"title":"numeric id"},
        {"id":"no-title"},
        {"id":"bad-body","title":"T","body":{"x":1}},
        {"id":"bad-expiry","title":"T","expires_at":"tomorrow"},
        {"id":"good","title":"T","created_at":"yesterday"}
    ]"#;
    let parsed = parse_feed(json).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "good");
    assert_eq!(parsed[0].created_at, None);
}

#[test]
fn unknown_or_missing_level_is_info() {
    let json = r#"[
        {"id":"a","title":"T","level":"urgent"},
        {"id":"b","title":"T","level":5},
        {"id":"c","title":"T","level":"CRITICAL"},
        {"id":"d","title":"T","level":"info"}
    ]"#;
    let levels: Vec<_> = parse_feed(json)
        .unwrap()
        .into_iter()
        .map(|item| item.level)
        .collect();
    assert_eq!(
        levels,
        [Level::Info, Level::Info, Level::Critical, Level::Info]
    );
}

#[test]
fn keeps_only_web_links() {
    let json = r#"[
        {"id":"a","title":"T","url":"https://example.com"},
        {"id":"b","title":"T","url":"javascript:alert(1)"},
        {"id":"c","title":"T","url":"file:///C:/x"},
        {"id":"d","title":"T","url":"https://exa mple.com"},
        {"id":"e","title":"T","url":5}
    ]"#;
    let urls: Vec<_> = parse_feed(json)
        .unwrap()
        .into_iter()
        .map(|item| item.url)
        .collect();
    assert_eq!(
        urls,
        [
            Some("https://example.com".to_owned()),
            None,
            None,
            None,
            None
        ]
    );
}

#[test]
fn caps_items_and_long_text() {
    let items: Vec<_> = (0..MAX_ITEMS + 10)
        .map(|index| format!(r#"{{"id":"{index}","title":"T"}}"#))
        .collect();
    let parsed = parse_feed(&format!("[{}]", items.join(","))).unwrap();
    assert_eq!(parsed.len(), MAX_ITEMS);

    let long_title = "я".repeat(500);
    let parsed = parse_feed(&format!(r#"[{{"id":"a","title":"{long_title}"}}]"#)).unwrap();
    assert_eq!(parsed[0].title.chars().count(), 201);
    assert!(parsed[0].title.ends_with('…'));
}

#[test]
fn version_numbers_are_the_digit_runs() {
    assert_eq!(version_numbers(BUILD), vec![0, 2026, 10, 29, 12, 0, 0]);
    assert_eq!(version_numbers("0.2026.10.29"), vec![0, 2026, 10, 29]);
    assert_eq!(version_numbers("dev"), Vec::<u64>::new());
    assert_eq!(version_numbers(""), Vec::<u64>::new());
}

#[test]
fn expired_announcements_are_hidden() {
    let version = version_numbers(BUILD);
    let expiring = Announcement {
        expires_at: Some(at(30, 12)),
        ..announcement("a")
    };
    assert!(expiring.is_visible(at(30, 11), &version));
    assert!(!expiring.is_visible(at(30, 12), &version));
    assert!(!expiring.is_visible(at(31, 0), &version));
    assert!(announcement("b").is_visible(at(31, 0), &version));
}

#[test]
fn min_version_is_a_prefix_bound() {
    let now = at(30, 0);
    let build = |version: &str| version_numbers(version);
    let from_oct_29 = bounded(Some("0.2026.10.29"), None);
    assert!(from_oct_29.is_visible(now, &build("v0.2026.10.29.00.00.oss_00")));
    assert!(from_oct_29.is_visible(now, &build(BUILD)));
    assert!(from_oct_29.is_visible(now, &build("v0.2026.11.01.08.00.oss_00")));
    assert!(!from_oct_29.is_visible(now, &build("v0.2026.10.28.23.59.oss_00")));
    assert!(!from_oct_29.is_visible(now, &build("v0.2025.12.31.23.59.oss_00")));
}

#[test]
fn max_version_includes_every_build_of_the_bound() {
    let now = at(30, 0);
    let build = |version: &str| version_numbers(version);
    let until_oct_29 = bounded(None, Some("0.2026.10.29"));
    assert!(until_oct_29.is_visible(now, &build("v0.2026.10.29.23.59.oss_09")));
    assert!(until_oct_29.is_visible(now, &build("v0.2026.10.01.00.00.oss_00")));
    assert!(!until_oct_29.is_visible(now, &build("v0.2026.10.30.00.00.oss_00")));

    let one_build = bounded(Some(BUILD), Some(BUILD));
    assert!(one_build.is_visible(now, &build(BUILD)));
    assert!(!one_build.is_visible(now, &build("v0.2026.10.29.12.00.oss_01")));
}

#[test]
fn bound_longer_than_the_version_pads_with_zeros() {
    let now = at(30, 0);
    let bound = bounded(Some("0.2026.10.29.0.0"), None);
    assert!(bound.is_visible(now, &version_numbers("0.2026.10.29")));
    let bound = bounded(Some("0.2026.10.29.0.1"), None);
    assert!(!bound.is_visible(now, &version_numbers("0.2026.10.29")));
}

#[test]
fn builds_without_a_version_see_only_unbounded_announcements() {
    let now = at(30, 0);
    let unknown = version_numbers("dev");
    assert!(announcement("a").is_visible(now, &unknown));
    assert!(!bounded(Some("0.2026.10.29"), None).is_visible(now, &unknown));
    assert!(!bounded(None, Some("0.2026.10.29")).is_visible(now, &unknown));
}

#[test]
fn bounds_without_digits_match_nothing() {
    let now = at(30, 0);
    let version = version_numbers(BUILD);
    assert!(!bounded(Some("latest"), None).is_visible(now, &version));
    assert!(!bounded(None, Some("latest")).is_visible(now, &version));
}

#[test]
fn select_new_skips_shown_hidden_and_duplicate_announcements() {
    let version = version_numbers(BUILD);
    let mut shown = ShownIds::default();
    shown.insert("shown");
    let feed = vec![
        announcement("shown"),
        Announcement {
            expires_at: Some(at(29, 0)),
            ..announcement("expired")
        },
        Announcement {
            min_version: Some("0.2027".to_owned()),
            ..announcement("future")
        },
        announcement("new"),
        announcement("new"),
    ];
    let new = select_new(feed, &shown, at(30, 0), &version);
    let ids: Vec<_> = new.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["new"]);
}

#[test]
fn select_new_orders_oldest_first_and_caps_the_count() {
    let version = version_numbers(BUILD);
    let dated = |id: &str, day: u32| Announcement {
        created_at: Some(at(day, 0)),
        ..announcement(id)
    };
    let feed = vec![
        announcement("undated"),
        dated("d28", 28),
        dated("d26", 26),
        dated("d27", 27),
        dated("d25", 25),
    ];
    let new = select_new(feed, &ShownIds::default(), at(30, 0), &version);
    let ids: Vec<_> = new.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["d25", "d26", "d27"]);
    assert_eq!(new.len(), MAX_SHOWN_PER_POLL);
}
