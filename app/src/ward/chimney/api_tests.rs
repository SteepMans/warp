use super::*;

/// Shaped like chimney-mcp's `BoardFull` (src/chimney.ts), extra fields included.
const BOARD_JSON: &str = r##"{
  "id": 5,
  "title": "Personal board",
  "description": "",
  "color": "#61BD4F",
  "columns": [
    {"id": 26, "title": "backlog", "columnType": "DEFAULT", "order": 0, "taskIds": [1201]},
    {"id": 27, "title": "sprint", "columnType": "BACKLOG", "order": 1, "taskIds": [1202]},
    {"id": 28, "title": "active task", "columnType": "ACTIVE", "order": 2, "taskIds": [1203]},
    {"id": 29, "title": "In progress", "columnType": "PROGRESS", "order": 3, "taskIds": [1287, 1204]},
    {"id": 30, "title": "Done", "columnType": "DONE", "order": 4, "taskIds": [1205]},
    {"id": 34, "title": "Complete sprint", "columnType": "DONE_SPRINT", "order": 5, "taskIds": [1206]}
  ],
  "labels": [
    {"id": 70, "name": "hassle online", "color": "#0079BF"},
    {"id": 121, "name": "chimney", "color": "#C377E0"}
  ],
  "members": [{"id": 115, "userId": 116, "name": "MCP Bot", "role": "EDITOR"}],
  "tasks": [
    {"id": 1201, "number": 11, "title": "Backlog idea", "description": null, "columnId": 26,
     "order": 0, "labelIds": [], "memberIds": [], "comments": [], "checklist": [],
     "attachments": [], "timerSeconds": 0, "timerRunning": false, "timerStartedAt": null,
     "dueDate": null, "scheduledAt": null, "scheduledEndAt": null, "storyPoints": null,
     "archived": false, "completedAt": null, "createdAt": "2026-10-01T10:00:00Z",
     "updatedAt": "2026-10-01T10:00:00Z"},
    {"id": 1202, "number": 12, "title": "Sprint task", "columnId": 27, "order": 0,
     "labelIds": [121], "archived": false},
    {"id": 1203, "number": 13, "title": "  Backlog stats endpoint ", "columnId": 28,
     "order": 0, "labelIds": [121, 70], "archived": false},
    {"id": 1287, "number": 87, "title": "Payments refund webhook", "columnId": 29,
     "order": 1, "labelIds": [70], "archived": false},
    {"id": 1204, "number": 14, "title": "Auth token rotation", "columnId": 29, "order": 0,
     "labelIds": null, "archived": false},
    {"id": 1205, "number": 15, "title": "Finished", "columnId": 30, "order": 0,
     "labelIds": [], "archived": false},
    {"id": 1206, "number": 16, "title": "Old sprint", "columnId": 34, "order": 0,
     "labelIds": [], "archived": false},
    {"id": 1207, "number": 17, "title": "Archived", "columnId": 28, "order": 1,
     "labelIds": [], "archived": true}
  ],
  "templates": [],
  "currentSpaceMemberId": 115,
  "nextTaskNumber": 18,
  "autoMoveDoneToSprint": true,
  "weeklyRestHours": 60
}"##;

fn board() -> Board {
    parse_board(BOARD_JSON).expect("fixture parses")
}

#[test]
fn parses_the_board_shape() {
    let board = board();
    assert_eq!(board.id, 5);
    assert_eq!(board.columns.len(), 6);
    assert_eq!(board.labels[0].name, "hassle online");
    assert_eq!(board.tasks.len(), 8);
    assert_eq!(board.tasks[3].column_id, Some(29));
    assert_eq!(board.tasks[4].label_ids, None);
}

#[test]
fn parses_minimal_and_null_fields() {
    let board = parse_board(r#"{"id": 7, "columns": [{"id": 1, "title": "x", "columnType": null}], "tasks": [{"id": 2}]}"#)
        .expect("minimal board parses");
    assert_eq!(board.id, 7);
    assert_eq!(ColumnKind::of(board.columns[0].column_type.as_deref()), ColumnKind::Backlog);
    assert_eq!(card_status(&board, 2).map(|status| status.column), Some(String::new()));
    assert!(parse_board("not json").is_err());
}

#[test]
fn card_status_reads_column_and_archive() {
    let board = board();
    let status = card_status(&board, 1287).expect("card exists");
    assert_eq!(status.column, "In progress");
    assert_eq!(status.column_kind, ColumnKind::Progress);
    assert!(!status.archived);
    assert!(card_status(&board, 1207).expect("archived card").archived);
    assert_eq!(card_status(&board, 9999), None);
}

#[test]
fn card_statuses_index_matches_card_status() {
    let board = board();
    let statuses = card_statuses(&board);
    assert_eq!(statuses.len(), board.tasks.len());
    for task in &board.tasks {
        assert_eq!(statuses.get(&task.id).cloned(), card_status(&board, task.id));
    }
    assert_eq!(statuses.get(&9999), None);
}

#[test]
fn picker_puts_active_work_first_and_skips_archive() {
    let board = board();
    let ids: Vec<u64> = picker_cards(&board, false)
        .iter()
        .map(|card| card.task_id)
        .collect();
    assert_eq!(ids, vec![1203, 1204, 1287, 1202, 1201]);
    let with_done: Vec<u64> = picker_cards(&board, true)
        .iter()
        .map(|card| card.task_id)
        .collect();
    assert_eq!(with_done, vec![1203, 1204, 1287, 1202, 1201, 1205]);
    let card = &picker_cards(&board, false)[0];
    assert_eq!(card.title, "Backlog stats endpoint");
    assert_eq!(card.labels, vec!["hassle online".to_owned(), "chimney".to_owned()]);
}

#[test]
fn chip_text_shows_ids_and_column() {
    let board = board();
    assert_eq!(chip_text(5, 1287, card_status(&board, 1287).as_ref()), "5/1287 In progress");
    assert_eq!(chip_text(5, 1207, card_status(&board, 1207).as_ref()), "5/1207 archived");
    assert_eq!(chip_text(5, 1, None), "5/1");
}
