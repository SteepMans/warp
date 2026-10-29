use super::*;

const CARD: CardRef = CardRef {
    board_id: 5,
    task_id: 1287,
};

#[test]
fn bindings_round_trip_through_json() {
    let mut map = BindingMap::default();
    assert!(map.bind("0a1b", CARD));
    assert!(!map.bind("0a1b", CARD));
    assert!(map.bind("ff00", CardRef { board_id: 5, task_id: 1 }));
    let decoded = decode(&encode(&map).expect("serializes"));
    assert_eq!(decoded, map);
    assert_eq!(decoded.get("0a1b"), Some(CARD));
}

#[test]
fn unbind_removes_only_the_pane() {
    let mut map = BindingMap::default();
    map.bind("a", CARD);
    map.bind("b", CARD);
    assert!(map.unbind("a"));
    assert!(!map.unbind("a"));
    assert_eq!(map.get("a"), None);
    assert_eq!(map.get("b"), Some(CARD));
}

#[test]
fn decode_tolerates_bad_or_old_data() {
    assert!(decode("").is_empty());
    assert!(decode("{\"something\": 1}").is_empty());
    assert_eq!(
        decode(r#"{"panes": {"ab": {"board_id": 5, "task_id": 1287}}}"#).get("ab"),
        Some(CARD)
    );
}

#[test]
fn uuid_hex_is_lowercase_and_padded() {
    assert_eq!(uuid_hex(&[0x0a, 0xff, 0x00]), "0aff00");
}

#[test]
fn parses_kanban_markers() {
    assert_eq!(
        parse_marker("claude --remote-control 'hassle. refund kanban:5/1287'"),
        Some(CARD)
    );
    assert_eq!(parse_marker("kanban:5/1287"), Some(CARD));
    assert_eq!(
        parse_marker("kanban:x kanban:12/34."),
        Some(CardRef {
            board_id: 12,
            task_id: 34
        })
    );
    assert_eq!(parse_marker("kanban:5/"), None);
    assert_eq!(parse_marker("kanban:/12"), None);
    assert_eq!(parse_marker("no marker"), None);
}
