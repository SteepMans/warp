use super::*;

#[test]
fn missing_or_malformed_list_starts_empty() {
    assert_eq!(ShownIds::from_json(None), ShownIds::default());
    assert_eq!(ShownIds::from_json(Some("not json")), ShownIds::default());
    assert_eq!(ShownIds::from_json(Some(r#"{"a":1}"#)), ShownIds::default());
}

#[test]
fn round_trips_through_json() {
    let mut shown = ShownIds::default();
    shown.insert("a");
    shown.insert("b");
    let json = shown.to_json();
    assert_eq!(json, r#"["a","b"]"#);
    assert_eq!(ShownIds::from_json(Some(&json)), shown);
}

#[test]
fn non_string_entries_and_duplicates_are_dropped_on_load() {
    let shown = ShownIds::from_json(Some(r#"["a", 5, null, "a", "", "b"]"#));
    assert_eq!(shown.to_json(), r#"["a","b"]"#);
}

#[test]
fn insert_dedupes() {
    let mut shown = ShownIds::default();
    assert!(shown.insert("a"));
    assert!(!shown.insert("a"));
    assert!(!shown.insert(""));
    assert!(shown.contains("a"));
    assert!(!shown.contains("b"));
    assert_eq!(shown.to_json(), r#"["a"]"#);
}

#[test]
fn oldest_ids_are_forgotten_past_the_limit() {
    let mut shown = ShownIds::default();
    for index in 0..MAX_IDS + 5 {
        shown.insert(&format!("id-{index}"));
    }
    for index in 0..5 {
        assert!(!shown.contains(&format!("id-{index}")));
    }
    assert!(shown.contains("id-5"));
    assert!(shown.contains(&format!("id-{}", MAX_IDS + 4)));
    let reloaded = ShownIds::from_json(Some(&shown.to_json()));
    assert_eq!(reloaded, shown);
}
