//! Ids of the announcements already shown, kept in private user preferences as a JSON list so a
//! restart does not show them again.

use std::collections::VecDeque;

/// The oldest ids are forgotten past this many; the server stops serving old announcements long
/// before that.
pub const MAX_IDS: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShownIds {
    /// Oldest first.
    ids: VecDeque<String>,
}

impl ShownIds {
    /// A malformed or missing list starts empty; entries that are not strings are dropped.
    pub fn from_json(json: Option<&str>) -> Self {
        let mut shown = Self::default();
        let Some(values) =
            json.and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(json).ok())
        else {
            return shown;
        };
        for id in values.iter().filter_map(serde_json::Value::as_str) {
            shown.insert(id);
        }
        shown
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.ids).unwrap_or_else(|_| "[]".to_owned())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|shown| shown == id)
    }

    /// Records `id`, forgetting the oldest ids past [`MAX_IDS`]. Returns whether it was new.
    pub fn insert(&mut self, id: &str) -> bool {
        if id.is_empty() || self.contains(id) {
            return false;
        }
        self.ids.push_back(id.to_owned());
        while self.ids.len() > MAX_IDS {
            self.ids.pop_front();
        }
        true
    }
}

#[cfg(test)]
#[path = "shown_tests.rs"]
mod tests;
