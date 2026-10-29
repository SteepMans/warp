use super::*;
use crate::ward::chimney::api::ColumnKind;

fn card(task_id: u64, title: &str, labels: &[&str], column: &str) -> CardChoice {
    CardChoice {
        board_id: 5,
        task_id,
        title: title.to_owned(),
        column: column.to_owned(),
        column_kind: ColumnKind::Active,
        labels: labels.iter().map(|label| (*label).to_owned()).collect(),
    }
}

fn ids(cards: &[CardChoice]) -> Vec<u64> {
    cards.iter().map(|card| card.task_id).collect()
}

#[test]
fn empty_query_keeps_board_order() {
    let cards = vec![
        card(3, "Fix login", &["hassle"], "active task"),
        card(1, "Write docs", &["chimney"], "sprint"),
    ];
    assert_eq!(ids(&filter_cards(cards, "  ")), vec![3, 1]);
}

#[test]
fn query_matches_title_label_and_column() {
    let cards = vec![
        card(1, "Fix login", &["hassle"], "active task"),
        card(2, "Write docs", &["chimney"], "sprint"),
        card(3, "Budget import", &[], "In progress"),
    ];
    assert_eq!(ids(&filter_cards(cards.clone(), "login")), vec![1]);
    assert_eq!(ids(&filter_cards(cards.clone(), "chimney")), vec![2]);
    assert_eq!(ids(&filter_cards(cards.clone(), "progress")), vec![3]);
    assert!(filter_cards(cards, "zzzz").is_empty());
}

#[test]
fn card_id_ranks_first() {
    let cards = vec![
        card(12, "Card 1287 follow-up", &[], "sprint"),
        card(1287, "Something else", &[], "sprint"),
    ];
    assert_eq!(ids(&filter_cards(cards.clone(), "#1287"))[0], 1287);
    assert_eq!(ids(&filter_cards(cards, "1287"))[0], 1287);
}

#[test]
fn purpose_decides_done_cards() {
    assert!(
        PickPurpose::Bind {
            pane_uuid: "ab".to_owned()
        }
        .include_done()
    );
    assert!(!PickPurpose::StartAgent.include_done());
}

#[test]
fn pending_purpose_is_taken_once() {
    set_pending(PickPurpose::StartAgent);
    assert_eq!(pending(), Some(PickPurpose::StartAgent));
    assert_eq!(take_pending(), Some(PickPurpose::StartAgent));
    assert_eq!(take_pending(), None);
}
