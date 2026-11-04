use super::*;

#[test]
fn the_file_is_read_with_its_summary_and_tags() {
    let about = parse(
        r##"{"summary": "  Поиск по сессиям на экране агентов  ",
            "tags": [" Chimney ", "#agents", "chimney", 42, "", "Hassle   Online"],
            "unknown": true}"##,
    )
    .unwrap();
    assert_eq!(
        about.summary.as_deref(),
        Some("Поиск по сессиям на экране агентов")
    );
    assert_eq!(about.tags, vec!["chimney", "agents", "hassle online"]);
}

#[test]
fn an_agent_may_write_the_tags_as_one_string_or_wrap_the_object() {
    let about = parse("Готово:\n```json\n{\"tags\": \"ward, agents\"}\n```").unwrap();
    assert_eq!(about.summary, None);
    assert_eq!(about.tags, vec!["ward", "agents"]);
}

#[test]
fn a_file_with_nothing_usable_says_nothing() {
    assert_eq!(parse(""), None);
    assert_eq!(parse("не JSON"), None);
    assert_eq!(parse(r#"{"summary": "   ", "tags": []}"#), None);
    assert_eq!(parse(r#"{"summary": 7, "tags": {"a": 1}}"#), None);
    assert_eq!(parse("} {"), None);
}

#[test]
fn the_summary_and_the_tags_are_kept_short() {
    let long = "я".repeat(2000);
    let about = parse(&format!(r#"{{"summary": "{long}"}}"#)).unwrap();
    assert_eq!(about.summary.unwrap().chars().count(), MAX_SUMMARY_CHARS);
    let many = serde_json::json!(["a", "b", "c", "d", "e", "f", "g", "h"]);
    assert_eq!(tags(Some(&many)).len(), MAX_TAGS);
    assert!(tags(None).is_empty());
}

#[test]
fn the_file_is_read_from_the_agents_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read(dir.path()), None);
    std::fs::write(
        dir.path().join(ABOUT_FILE),
        r#"{"summary": "Теги сессий", "tags": ["ward"]}"#,
    )
    .unwrap();
    assert_eq!(
        read(dir.path()),
        Some(About {
            summary: Some("Теги сессий".to_owned()),
            tags: vec!["ward".to_owned()],
        })
    );
    // A file too big for a note about the session is not read at all.
    std::fs::write(
        dir.path().join(ABOUT_FILE),
        "x".repeat(MAX_ABOUT_BYTES as usize + 1),
    )
    .unwrap();
    assert_eq!(read(dir.path()), None);
}

#[test]
fn the_prompt_of_the_executor_ends_with_the_note_about_the_file() {
    let path = Path::new("C:\\work\\demo\\.worktrees\\s1\\.agents\\about.json");
    let prompt = with_instruction("Сделай задачу\n\n", path);
    assert!(
        prompt.starts_with("Сделай задачу\n\n## Для экрана агентов"),
        "{prompt}"
    );
    assert!(
        prompt.contains("C:/work/demo/.worktrees/s1/.agents/about.json"),
        "{prompt}"
    );
    assert!(prompt.contains("\"summary\""));
    assert!(prompt.contains("\"tags\""));
    // The example in the note is the object Ward reads back.
    let start = prompt.find("```json\n").unwrap() + "```json\n".len();
    let end = start + prompt[start..].find("\n```").unwrap();
    let example = parse(&prompt[start..end]).unwrap();
    assert_eq!(example.tags.len(), 2);
}
