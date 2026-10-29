use super::*;

fn group(module: &str, items: &[&str]) -> ChangelogGroup {
    ChangelogGroup {
        module: module.to_owned(),
        items: items.iter().map(|item| item.to_string()).collect(),
    }
}

#[test]
fn parses_published_changelog_with_optional_fields_missing() {
    let changelog: WardChangelog = serde_json::from_str(
        r#"{"version":"v0.2026.10.29.12.00.oss_00","groups":[{"module":"Tabs","items":["a"]}]}"#,
    )
    .unwrap();
    assert_eq!(changelog.version, "v0.2026.10.29.12.00.oss_00");
    assert_eq!(changelog.previous_version, None);
    assert_eq!(changelog.groups, vec![group("Tabs", &["a"])]);
}

#[test]
fn parses_changelog_written_by_ci() {
    let changelog: WardChangelog = serde_json::from_str(
        r#"{
            "version": "v0.2026.10.29.12.00.oss_00",
            "previous_version": "v0.2026.10.28.09.30.oss_00",
            "commit": "abc",
            "previous_commit": null,
            "date": "2026-10-29T12:00:00Z",
            "groups": [{"module": "Вкладки", "items": ["фильтр по статусу агента"]}]
        }"#,
    )
    .unwrap();
    assert_eq!(changelog.commit.as_deref(), Some("abc"));
    assert_eq!(changelog.previous_commit, None);
    assert_eq!(changelog.groups[0].module, "Вкладки");
}

#[test]
fn markdown_lists_items_under_bold_module_lines() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        groups: vec![
            group("Tabs", &["agent filter", "group counters"]),
            group("CI", &["license cache"]),
        ],
        ..Default::default()
    };
    let markdown = render_markdown(&changelog, "v1");
    let expected_tail = "**Tabs**\n* agent filter\n* group counters\n\n**CI**\n* license cache\n";
    assert!(markdown.ends_with(expected_tail), "{markdown}");
    assert!(markdown.starts_with("**What's new in "), "{markdown}");
    assert!(markdown.contains(" v1**\n\n"), "{markdown}");
}

#[test]
fn markdown_escapes_emphasis_and_code_characters() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        groups: vec![group("Metrics", &["ward_app_running and `code` *x*"])],
        ..Default::default()
    };
    let markdown = render_markdown(&changelog, "v1");
    assert!(
        markdown.contains("* ward\\_app\\_running and \\`code\\` \\*x\\*\n"),
        "{markdown}"
    );
}

#[test]
fn markdown_drops_empty_items_and_names_unnamed_groups() {
    let changelog = WardChangelog {
        version: String::new(),
        groups: vec![group("", &["x", "  "]), group("Empty", &[" "])],
        ..Default::default()
    };
    let markdown = render_markdown(&changelog, "v2");
    assert!(markdown.contains(" v2**"), "{markdown}");
    assert!(markdown.contains("**Other**\n* x\n"), "{markdown}");
    assert!(!markdown.contains("Empty"), "{markdown}");
}

#[test]
fn markdown_without_changes_says_so() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        ..Default::default()
    };
    assert!(render_markdown(&changelog, "v1").ends_with("* No notable changes this release\n"));
}

#[test]
fn warp_changelog_has_a_single_new_features_section() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        date: Some("2026-10-29T12:00:00Z".to_owned()),
        groups: vec![group("Tabs", &["a"])],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert_eq!(converted.markdown_sections.len(), 1);
    assert_eq!(
        converted.markdown_sections[0].title,
        ChangelogHeader::NewFeatures.to_string()
    );
    assert_eq!(converted.date.to_rfc3339(), "2026-10-29T12:00:00+00:00");
    assert!(converted.sections.is_empty());
    assert!(converted.image_url.is_none());
}

#[test]
fn invalid_date_falls_back_to_now() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        date: Some("yesterday".to_owned()),
        ..Default::default()
    };
    let before = Utc::now().fixed_offset();
    let converted = to_warp_changelog(&changelog, "v1");
    assert!(converted.date >= before - chrono::Duration::seconds(1));
}
