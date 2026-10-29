//! Tab to Chimney card bindings. A binding belongs to a terminal pane and is keyed by the pane's
//! persistent UUID (the one session restoration reuses), so it survives restarts. The map lives
//! in the private user preferences; Ward only ever reads cards, never changes them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::{AppContext, EntityId};

/// Private user preferences key holding the JSON-encoded [`BindingMap`].
const PREFERENCES_KEY: &str = "WardChimneyBindings";

/// A Chimney card: board id and task id, as in the `kanban:<board>/<task>` marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CardRef {
    pub board_id: u64,
    pub task_id: u64,
}

/// Persisted pane UUID (hex) -> card mapping.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingMap {
    #[serde(default)]
    panes: BTreeMap<String, CardRef>,
}

impl BindingMap {
    pub fn get(&self, pane_uuid: &str) -> Option<CardRef> {
        self.panes.get(pane_uuid).copied()
    }

    /// Returns whether anything changed.
    pub fn bind(&mut self, pane_uuid: &str, card: CardRef) -> bool {
        self.panes.insert(pane_uuid.to_owned(), card) != Some(card)
    }

    /// Returns whether anything changed.
    pub fn unbind(&mut self, pane_uuid: &str) -> bool {
        self.panes.remove(pane_uuid).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }
}

pub fn encode(map: &BindingMap) -> Result<String, serde_json::Error> {
    serde_json::to_string(map)
}

/// Unreadable stored data yields an empty map rather than an error: bindings are a convenience.
pub fn decode(raw: &str) -> BindingMap {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Lowercase hex of a pane UUID.
pub fn uuid_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Finds a `kanban:<board>/<task>` marker, e.g. in a remote-control session name.
pub fn parse_marker(text: &str) -> Option<CardRef> {
    let mut rest = text;
    while let Some(index) = rest.find("kanban:") {
        rest = &rest[index + "kanban:".len()..];
        let board: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let after_board = &rest[board.len()..];
        if !board.is_empty()
            && let Some(after_slash) = after_board.strip_prefix('/')
        {
            let task: String = after_slash.chars().take_while(char::is_ascii_digit).collect();
            if let (Ok(board_id), Ok(task_id)) = (board.parse(), task.parse()) {
                return Some(CardRef { board_id, task_id });
            }
        }
    }
    None
}

#[derive(Default)]
struct Store {
    map: Option<BindingMap>,
    /// Live terminal views and their pane UUIDs, refreshed whenever the app state is saved.
    views: HashMap<EntityId, String>,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

fn with_map<R>(app: &AppContext, f: impl FnOnce(&mut BindingMap, &HashMap<EntityId, String>) -> R) -> R {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        let store = &mut *store;
        let map = store.map.get_or_insert_with(|| load(app));
        f(map, &store.views)
    })
}

fn load(app: &AppContext) -> BindingMap {
    app.private_user_preferences()
        .read_value(PREFERENCES_KEY)
        .ok()
        .flatten()
        .map(|raw| decode(&raw))
        .unwrap_or_default()
}

fn save(map: &BindingMap, app: &AppContext) {
    match encode(map) {
        Ok(serialized) => {
            if let Err(err) = app
                .private_user_preferences()
                .write_value(PREFERENCES_KEY, serialized)
            {
                log::warn!("Failed to save Ward Chimney bindings: {err:?}");
            }
        }
        Err(err) => log::warn!("Failed to serialize Ward Chimney bindings: {err:?}"),
    }
}

/// Replaces the set of live terminal views (terminal view id -> pane UUID hex).
pub fn set_live_views(views: HashMap<EntityId, String>) {
    STORE.with(|store| store.borrow_mut().views = views);
}

/// Adds one live terminal view, for panes created since the last refresh.
pub fn register_view(terminal_view_id: EntityId, pane_uuid: String) {
    STORE.with(|store| {
        store.borrow_mut().views.insert(terminal_view_id, pane_uuid);
    });
}

pub fn pane_uuid_of(terminal_view_id: EntityId) -> Option<String> {
    STORE.with(|store| store.borrow().views.get(&terminal_view_id).cloned())
}

pub fn card_of_view(terminal_view_id: EntityId, app: &AppContext) -> Option<CardRef> {
    with_map(app, |map, views| {
        views
            .get(&terminal_view_id)
            .and_then(|pane_uuid| map.get(pane_uuid))
    })
}

pub fn card_of_pane(pane_uuid: &str, app: &AppContext) -> Option<CardRef> {
    with_map(app, |map, _| map.get(pane_uuid))
}

pub fn bind(pane_uuid: &str, card: CardRef, app: &AppContext) {
    with_map(app, |map, _| {
        if map.bind(pane_uuid, card) {
            save(map, app);
        }
    });
}

pub fn unbind(pane_uuid: &str, app: &AppContext) {
    with_map(app, |map, _| {
        if map.unbind(pane_uuid) {
            save(map, app);
        }
    });
}

/// Boards of the cards bound to live panes; polling runs only while this is not empty.
pub fn live_boards(app: &AppContext) -> BTreeSet<u64> {
    with_map(app, |map, views| {
        views
            .values()
            .filter_map(|pane_uuid| map.get(pane_uuid))
            .map(|card| card.board_id)
            .collect()
    })
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;
