//! The Chimney card picker: a command palette mode listing the cards of the configured board.
//! What the picked card is for (binding a tab or starting an agent) is kept here while the
//! palette is open.

use std::cell::RefCell;

use fuzzy_match::match_indices_case_insensitive;

use super::api::CardChoice;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickPurpose {
    /// Bind the terminal pane with this persistent uuid (hex) to the card.
    Bind { pane_uuid: String },
    /// Start a CLI agent for the card in a new tab.
    StartAgent,
}

impl PickPurpose {
    /// A tab may be bound to a finished card; starting work on one makes no sense.
    pub fn include_done(&self) -> bool {
        matches!(self, PickPurpose::Bind { .. })
    }
}

thread_local! {
    static PENDING: RefCell<Option<PickPurpose>> = const { RefCell::new(None) };
}

pub fn set_pending(purpose: PickPurpose) {
    PENDING.with(|pending| *pending.borrow_mut() = Some(purpose));
}

pub fn pending() -> Option<PickPurpose> {
    PENDING.with(|pending| pending.borrow().clone())
}

pub fn take_pending() -> Option<PickPurpose> {
    PENDING.with(|pending| pending.borrow_mut().take())
}

/// Best fuzzy score of the query over the card's title, labels and column; the card id (with or
/// without `#`) matches exactly and ranks first. An empty query matches everything.
pub fn card_match_score(card: &CardChoice, query: &str) -> Option<i64> {
    let query = query.trim();
    if query.is_empty() {
        return Some(0);
    }
    if query.trim_start_matches('#') == card.task_id.to_string() {
        return Some(i64::MAX);
    }
    std::iter::once(card.title.as_str())
        .chain(card.labels.iter().map(String::as_str))
        .chain(std::iter::once(card.column.as_str()))
        .filter_map(|field| match_indices_case_insensitive(field, query))
        .map(|result| result.score)
        .max()
}

/// The cards matching `query`: board order (work in progress first) for an empty query, best
/// match first otherwise; equal matches keep the board order.
pub fn filter_cards(cards: Vec<CardChoice>, query: &str) -> Vec<CardChoice> {
    let mut scored: Vec<(i64, CardChoice)> = cards
        .into_iter()
        .filter_map(|card| card_match_score(&card, query).map(|score| (score, card)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, card)| card).collect()
}

#[cfg(test)]
#[path = "picker_tests.rs"]
mod tests;
