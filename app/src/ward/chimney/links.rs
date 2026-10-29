//! Links into the Chimney web app, built like chimney-mcp's `src/links.ts`: the board is
//! `{appUrl}#/boards/{board}`, a card is `{appUrl}#/boards/{board}?task={task}` when the
//! frontend supports task deep links (`CHIMNEY_TASK_DEEPLINKS=true`), else the board.

use super::bindings::CardRef;

pub const DEFAULT_APP_URL: &str = "https://chimney.steep-man.ru/chimney/";

/// Environment overrides, the same variables chimney-mcp reads.
pub const APP_URL_ENV_VAR: &str = "CHIMNEY_APP_URL";
pub const TASK_DEEPLINKS_ENV_VAR: &str = "CHIMNEY_TASK_DEEPLINKS";

/// `http(s)://host[/path]/` with exactly one trailing slash, or `None` for anything else.
pub fn normalize_app_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    (!rest.is_empty() && !rest.contains(char::is_whitespace)).then(|| format!("{url}/"))
}

pub fn board_url(app_url: &str, board_id: u64) -> String {
    format!("{app_url}#/boards/{board_id}")
}

pub fn card_url(app_url: &str, card: CardRef, deep_links: bool) -> String {
    if deep_links {
        format!("{app_url}#/boards/{}?task={}", card.board_id, card.task_id)
    } else {
        board_url(app_url, card.board_id)
    }
}

/// The web app URL: `CHIMNEY_APP_URL` when valid, else the default.
pub fn app_url() -> String {
    std::env::var(APP_URL_ENV_VAR)
        .ok()
        .and_then(|url| normalize_app_url(&url))
        .unwrap_or_else(|| DEFAULT_APP_URL.to_owned())
}

/// Whether `CHIMNEY_TASK_DEEPLINKS` is `true`, as in chimney-mcp (off by default).
pub fn task_deep_links() -> bool {
    std::env::var(TASK_DEEPLINKS_ENV_VAR).is_ok_and(|value| value.trim() == "true")
}

/// Where "Open card in browser" goes for a bound card.
pub fn browser_url(card: CardRef) -> String {
    card_url(&app_url(), card, task_deep_links())
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
