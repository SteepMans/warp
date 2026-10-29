//! Learns which tab group a project belongs to and puts new tabs of that project into it.
//!
//! A tab's project keys are the git repo root of its terminal (falling back to the working
//! directory) and, for `claude --remote-control '<project>. <task>'` sessions, the `<project>`
//! prefix of the session name. Placing a tab with known keys into a named group, by any means
//! (menu, drag, new group plus rename), records key -> group name; the mapping is persisted in
//! the private user preferences. When a tab's keys first become known (a new or restored tab
//! finished bootstrapping, an agent session was detected, the shell moved into another repo) and
//! a mapping exists, the tab is moved into that group, recreating the group if it is gone.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use warp_core::ui::theme::AnsiColorIdentifier;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::{AppContext, EntityId};

use crate::workspace::tab_group::TabGroupId;

/// Private user preferences key holding the JSON-encoded [`ProjectGroupMap`].
const PREFERENCES_KEY: &str = "WardProjectGroups";

/// Prefix of keys derived from a remote-control session name, so they never collide with paths.
const REMOTE_CONTROL_KEY_PREFIX: &str = "rc:";

/// Normalizes a directory into a project key: forward slashes, no trailing slash, lowercase
/// (Windows paths are case-insensitive).
pub fn path_key(path: &str) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    (!normalized.is_empty()).then(|| normalized.to_lowercase())
}

/// Project keys of a tab, most specific first.
pub fn project_keys(remote_control_project: Option<&str>, directory: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(project) = remote_control_project
        .map(str::trim)
        .filter(|project| !project.is_empty())
    {
        keys.push(format!(
            "{REMOTE_CONTROL_KEY_PREFIX}{}",
            project.to_lowercase()
        ));
    }
    if let Some(key) = directory.and_then(path_key) {
        keys.push(key);
    }
    keys
}

/// Extracts `<project>` from a `--remote-control '<project>. <task>'` (or
/// `--remote-control-session-name-prefix '<project>.'`) argument of a Claude Code command line.
/// Session names without a `<project>.` prefix yield `None`.
pub fn remote_control_project(command: &str) -> Option<String> {
    let name = remote_control_session_name(command)?;
    let (project, _) = name.split_once('.')?;
    let project = project.trim();
    (!project.is_empty() && !project.contains(char::is_whitespace)).then(|| project.to_owned())
}

/// The session name given to `--remote-control` (or its prefix flag), unquoted.
pub fn remote_control_session_name(command: &str) -> Option<String> {
    let args = split_command_line(command);
    let mut value = None;
    for (index, arg) in args.iter().enumerate() {
        let flag_value = match arg.split_once('=') {
            Some((flag, inline)) => is_session_name_flag(flag).then(|| inline.to_owned()),
            None if is_session_name_flag(arg) => args
                .get(index + 1)
                .filter(|next| !next.starts_with('-'))
                .cloned(),
            None => None,
        };
        if flag_value.is_some() {
            value = flag_value;
            break;
        }
    }
    value
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn is_session_name_flag(arg: &str) -> bool {
    arg == "--remote-control" || arg == "--remote-control-session-name-prefix"
}

/// Splits a command line into arguments, honouring single and double quotes (including the
/// doubled `''` escape PowerShell uses inside single quotes).
fn split_command_line(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut has_current = false;
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => {
                if q == '\'' && chars.peek() == Some(&'\'') {
                    chars.next();
                    current.push('\'');
                } else {
                    quote = None;
                }
            }
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                has_current = true;
            }
            None if c.is_whitespace() => {
                if has_current {
                    args.push(std::mem::take(&mut current));
                    has_current = false;
                }
            }
            None => {
                current.push(c);
                has_current = true;
            }
        }
    }
    if has_current {
        args.push(current);
    }
    args
}

/// Group names match case-insensitively, so "Hassle" and "hassle" are the same group.
pub fn same_group_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Color for a group Ward creates itself: stable per name, never red or yellow (those mean
/// "needs attention" in the tab rows).
pub fn auto_group_color(name: &str) -> AnsiColorIdentifier {
    const COLORS: [AnsiColorIdentifier; 4] = [
        AnsiColorIdentifier::Blue,
        AnsiColorIdentifier::Magenta,
        AnsiColorIdentifier::Cyan,
        AnsiColorIdentifier::Green,
    ];
    // FNV-1a: stable across builds, unlike `DefaultHasher`.
    let hash = name
        .trim()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    COLORS[(hash % COLORS.len() as u64) as usize]
}

/// Persisted project key -> group name mapping.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGroupMap {
    groups: BTreeMap<String, String>,
}

impl ProjectGroupMap {
    /// Group of the first key that has one.
    pub fn lookup(&self, keys: &[String]) -> Option<&str> {
        keys.iter()
            .find_map(|key| self.groups.get(key))
            .map(String::as_str)
    }

    /// Maps every key to `group`. Returns whether anything changed.
    pub fn learn(&mut self, keys: &[String], group: &str) -> bool {
        let mut changed = false;
        for key in keys {
            if self.groups.get(key).map(String::as_str) != Some(group) {
                self.groups.insert(key.clone(), group.to_owned());
                changed = true;
            }
        }
        changed
    }

    /// Follows a group rename. Returns whether anything changed.
    pub fn rename_group(&mut self, from: &str, to: &str) -> bool {
        let mut changed = false;
        for group in self.groups.values_mut() {
            if same_group_name(group, from) && group != to {
                *group = to.to_owned();
                changed = true;
            }
        }
        changed
    }
}

/// What Ward last saw of a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabObservation {
    pub keys: Vec<String>,
    pub group_id: Option<TabGroupId>,
    /// `None` for ungrouped tabs and for groups without a name.
    pub group_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupDecision {
    Nothing,
    /// Record the tab's keys under this group.
    Learn(String),
    /// The tab's group was renamed: follow the rename, then record the tab's keys.
    Renamed {
        from: String,
        to: String,
    },
    /// Put the tab into this group.
    MoveTo(String),
}

/// Pure decision for one tab, given what was seen of it last time.
pub fn decide(
    previous: Option<&TabObservation>,
    current: &TabObservation,
    map: &ProjectGroupMap,
) -> GroupDecision {
    if current.keys.is_empty() {
        return GroupDecision::Nothing;
    }
    let keys_changed = previous.is_none_or(|previous| previous.keys != current.keys);
    if keys_changed {
        return match (map.lookup(&current.keys), current.group_name.as_deref()) {
            // The project is known: keep the tab in its group, filling in any new keys.
            (Some(target), Some(name)) if same_group_name(target, name) => {
                GroupDecision::Learn(name.to_owned())
            }
            (Some(target), _) => GroupDecision::MoveTo(target.to_owned()),
            // First time this project shows up inside a named group: remember it.
            (None, Some(name)) => GroupDecision::Learn(name.to_owned()),
            (None, None) => GroupDecision::Nothing,
        };
    }
    let Some(previous) = previous else {
        return GroupDecision::Nothing;
    };
    match (
        previous.group_id == current.group_id,
        previous.group_name.as_deref(),
        current.group_name.as_deref(),
    ) {
        (true, Some(from), Some(to)) if from != to => GroupDecision::Renamed {
            from: from.to_owned(),
            to: to.to_owned(),
        },
        // Named, or moved into another named group by the user.
        (_, from, Some(to)) if from != Some(to) || previous.group_id != current.group_id => {
            GroupDecision::Learn(to.to_owned())
        }
        // Taking a tab out of its group does not forget the project's group.
        _ => GroupDecision::Nothing,
    }
}

#[derive(Default)]
struct Store {
    map: Option<ProjectGroupMap>,
    observed: HashMap<EntityId, TabObservation>,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

fn load_map(app: &AppContext) -> ProjectGroupMap {
    app.private_user_preferences()
        .read_value(PREFERENCES_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_map(map: &ProjectGroupMap, app: &AppContext) {
    match serde_json::to_string(map) {
        Ok(serialized) => {
            if let Err(err) = app
                .private_user_preferences()
                .write_value(PREFERENCES_KEY, serialized)
            {
                log::warn!("Failed to save Ward project groups: {err:?}");
            }
        }
        Err(err) => log::warn!("Failed to serialize Ward project groups: {err:?}"),
    }
}

/// Observes one tab (keyed by its pane group) and returns the group it should be moved to, if
/// any. Learned mappings are persisted immediately.
pub fn observe_tab(
    pane_group_id: EntityId,
    observation: TabObservation,
    app: &AppContext,
) -> Option<String> {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        let store = &mut *store;
        let map = store.map.get_or_insert_with(|| load_map(app));
        let decision = decide(store.observed.get(&pane_group_id), &observation, map);
        let mut changed = false;
        let mut move_to = None;
        match decision {
            GroupDecision::Nothing => {}
            GroupDecision::Learn(group) => changed = map.learn(&observation.keys, &group),
            GroupDecision::Renamed { from, to } => {
                changed = map.rename_group(&from, &to);
                changed |= map.learn(&observation.keys, &to);
            }
            GroupDecision::MoveTo(group) => move_to = Some(group),
        }
        if changed {
            save_map(map, app);
        }
        store.observed.insert(pane_group_id, observation);
        move_to
    })
}

/// Drops observations of tabs that no longer exist.
pub fn retain_tabs(mut is_alive: impl FnMut(EntityId) -> bool) {
    STORE.with(|store| store.borrow_mut().observed.retain(|id, _| is_alive(*id)));
}

#[cfg(test)]
#[path = "project_groups_tests.rs"]
mod tests;
