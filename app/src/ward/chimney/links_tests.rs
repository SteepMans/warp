use super::*;

const CARD: CardRef = CardRef {
    board_id: 5,
    task_id: 1287,
};

#[test]
fn app_url_gets_one_trailing_slash() {
    assert_eq!(
        normalize_app_url(" https://chimney.steep-man.ru/chimney "),
        Some(DEFAULT_APP_URL.to_owned())
    );
    assert_eq!(
        normalize_app_url("http://localhost:5173//"),
        Some("http://localhost:5173/".to_owned())
    );
    assert_eq!(normalize_app_url("chimney.steep-man.ru"), None);
    assert_eq!(normalize_app_url("https://"), None);
}

#[test]
fn card_url_without_deep_links_is_the_board() {
    assert_eq!(
        card_url(DEFAULT_APP_URL, CARD, false),
        "https://chimney.steep-man.ru/chimney/#/boards/5"
    );
}

#[test]
fn card_url_with_deep_links_opens_the_task() {
    assert_eq!(
        card_url(DEFAULT_APP_URL, CARD, true),
        "https://chimney.steep-man.ru/chimney/#/boards/5?task=1287"
    );
}
