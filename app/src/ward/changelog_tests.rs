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
fn warp_changelog_without_notes_has_a_single_new_section() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        date: Some("2026-10-29T12:00:00Z".to_owned()),
        groups: vec![group("Tabs", &["a"])],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert_eq!(converted.markdown_sections.len(), 1);
    assert_eq!(converted.markdown_sections[0].title, NEW_SECTION_TITLE);
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

fn note(section: &str, title: &str, text: &str, image_url: Option<&str>) -> ChangelogNote {
    ChangelogNote {
        section: section.to_owned(),
        title: title.to_owned(),
        text: text.to_owned(),
        image_url: image_url.map(ToOwned::to_owned),
    }
}

#[test]
fn old_changelog_has_no_notes_or_image() {
    let changelog: WardChangelog = serde_json::from_str(
        r#"{"version":"v1","date":"2026-10-29T12:00:00Z","groups":[{"module":"Tabs","items":["a"]}]}"#,
    )
    .unwrap();
    assert!(changelog.notes.is_empty());
    assert_eq!(changelog.image_url, None);
}

#[test]
fn parses_changelog_with_notes() {
    let changelog: WardChangelog = serde_json::from_str(
        r#"{
            "version": "v1",
            "image_url": "https://example.com/stable/v1/images/a.png",
            "notes": [
                {"section": "new", "title": "Обзор агентов", "text": "Ctrl+Alt+W.", "image_url": "https://example.com/stable/v1/images/a.png"},
                {"section": "fixed", "title": "Без картинки", "text": "Текст.", "image_url": null}
            ],
            "groups": []
        }"#,
    )
    .unwrap();
    assert_eq!(
        changelog.notes,
        vec![
            note(
                "new",
                "Обзор агентов",
                "Ctrl+Alt+W.",
                Some("https://example.com/stable/v1/images/a.png")
            ),
            note("fixed", "Без картинки", "Текст.", None),
        ]
    );
    assert_eq!(
        changelog.image_url.as_deref(),
        Some("https://example.com/stable/v1/images/a.png")
    );
}

#[test]
fn notes_become_sections_in_fixed_order_without_empty_ones() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        notes: vec![
            note("fixed", "Исправлено", "Больше не падает.", None),
            note("new", "Первое", "Текст один.", None),
            note("new", "Второе", "Текст два.", None),
        ],
        groups: vec![group("Tabs", &["ignored when there are notes"])],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    let titles: Vec<&str> = converted
        .markdown_sections
        .iter()
        .map(|section| section.title.as_str())
        .collect();
    assert_eq!(titles, vec![NEW_SECTION_TITLE, FIXED_SECTION_TITLE]);
    assert_eq!(
        converted.markdown_sections[0].markdown,
        "**Первое**\nТекст один.\n\n**Второе**\nТекст два.\n"
    );
    assert_eq!(
        converted.markdown_sections[1].markdown,
        "**Исправлено**\nБольше не падает.\n"
    );
    assert!(converted.image_url.is_none());
}

#[test]
fn unknown_section_counts_as_improvement() {
    let changelog = WardChangelog {
        notes: vec![
            note("Improved", "a", "b", None),
            note("other", "c", "d", None),
        ],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert_eq!(converted.markdown_sections.len(), 1);
    assert_eq!(converted.markdown_sections[0].title, IMPROVED_SECTION_TITLE);
}

#[test]
fn first_note_picture_is_shown_and_others_are_linked() {
    let changelog = WardChangelog {
        notes: vec![
            note("improved", "Без картинки", "a", None),
            note(
                "improved",
                "С картинкой",
                "b",
                Some("https://x.test/images/one.png"),
            ),
            note(
                "fixed",
                "Вторая",
                "c",
                Some("https://x.test/images/two.png"),
            ),
        ],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert_eq!(
        converted.image_url.as_deref(),
        Some("https://x.test/images/one.png")
    );
    let improved = &converted.markdown_sections[0].markdown;
    assert!(!improved.contains("one.png"), "{improved}");
    let fixed = &converted.markdown_sections[1].markdown;
    assert!(
        fixed.ends_with("[Посмотреть картинку](https://x.test/images/two.png)\n"),
        "{fixed}"
    );
}

#[test]
fn top_level_picture_wins_and_unsafe_urls_are_dropped() {
    let changelog = WardChangelog {
        image_url: Some("https://x.test/images/top.png".to_owned()),
        notes: vec![
            note("new", "a", "b", Some("javascript:alert(1)")),
            note("new", "c", "d", Some("https://x.test/a b.png")),
        ],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert_eq!(
        converted.image_url.as_deref(),
        Some("https://x.test/images/top.png")
    );
    assert!(!converted.markdown_sections[0].markdown.contains("]("));
}

#[test]
fn note_titles_are_escaped_and_empty_notes_skipped() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        notes: vec![
            note("new", "Флаг *x*", "Текст с `claude`.", None),
            note("new", " ", " ", None),
        ],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert_eq!(
        converted.markdown_sections[0].markdown,
        "**Флаг \\*x\\***\nТекст с `claude`.\n"
    );
}

#[test]
fn only_empty_notes_fall_back_to_groups() {
    let changelog = WardChangelog {
        version: "v1".to_owned(),
        notes: vec![note("new", "", "", None)],
        groups: vec![group("Tabs", &["a"])],
        ..Default::default()
    };
    let converted = to_warp_changelog(&changelog, "v1");
    assert!(
        converted.markdown_sections[0]
            .markdown
            .contains("**Tabs**\n* a\n")
    );
}
