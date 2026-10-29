//! Which repository an agent started from a Chimney card runs in. Card labels are project
//! names; the label -> directory mapping is remembered in the private user preferences each
//! time the user picks a directory, and otherwise guessed from what project grouping learned
//! about the user's tabs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;

use super::slug::slugify;
use crate::ward::labels::directory_name;
use crate::ward::project_groups;

const PREFERENCES_KEY: &str = "WardChimneyRepos";

/// Longest project part of a session name.
const MAX_PROJECT_CHARS: usize = 24;

/// Label used for cards without labels.
pub const UNLABELED: &str = "task";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMap {
    /// Lowercased label -> directory.
    #[serde(default)]
    projects: BTreeMap<String, String>,
}

impl RepoMap {
    /// The first label with a remembered directory.
    pub fn lookup<'a>(&self, labels: &'a [String]) -> Option<(&'a str, &str)> {
        labels.iter().find_map(|label| {
            self.projects
                .get(&label.trim().to_lowercase())
                .map(|path| (label.as_str(), path.as_str()))
        })
    }

    /// Returns whether anything changed.
    pub fn remember(&mut self, label: &str, path: &str) -> bool {
        let label = label.trim().to_lowercase();
        let path = path.trim();
        if label.is_empty() || path.is_empty() {
            return false;
        }
        self.projects.insert(label, path.to_owned()).as_deref() != Some(path)
    }
}

/// Where to run the agent for a card, decided without asking the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoChoice {
    /// The label the directory belongs to.
    pub label: String,
    pub path: String,
}

/// Pure choice: a remembered directory for any label wins, then a single directory learned for
/// a label (several candidates are ambiguous, so the user is asked).
pub fn choose_repo(
    labels: &[String],
    remembered: &RepoMap,
    learned: impl Fn(&str) -> Vec<String>,
) -> Option<RepoChoice> {
    if let Some((label, path)) = remembered.lookup(labels) {
        return Some(RepoChoice {
            label: label.to_owned(),
            path: path.to_owned(),
        });
    }
    labels.iter().find_map(|label| match learned(label).as_slice() {
        [path] => Some(RepoChoice {
            label: label.clone(),
            path: path.clone(),
        }),
        _ => None,
    })
}

/// The `<project>` of the session name: the label as a slug, else the directory name, else
/// [`UNLABELED`]. Never contains whitespace, so project grouping recognizes it.
pub fn session_project(label: Option<&str>, path: &str) -> String {
    label
        .map(|label| slugify(label, MAX_PROJECT_CHARS))
        .filter(|project| !project.is_empty())
        .or_else(|| {
            directory_name(path)
                .map(|name| slugify(&name, MAX_PROJECT_CHARS))
                .filter(|project| !project.is_empty())
        })
        .unwrap_or_else(|| UNLABELED.to_owned())
}

pub fn load(app: &AppContext) -> RepoMap {
    app.private_user_preferences()
        .read_value(PREFERENCES_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn remember(label: &str, path: &str, app: &AppContext) {
    let mut map = load(app);
    if !map.remember(label, path) {
        return;
    }
    match serde_json::to_string(&map) {
        Ok(serialized) => {
            if let Err(err) = app
                .private_user_preferences()
                .write_value(PREFERENCES_KEY, serialized)
            {
                log::warn!("Failed to save Ward Chimney repositories: {err:?}");
            }
        }
        Err(err) => log::warn!("Failed to serialize Ward Chimney repositories: {err:?}"),
    }
}

/// [`choose_repo`] against the saved mapping and the project groups.
pub fn repo_for_labels(labels: &[String], app: &AppContext) -> Option<RepoChoice> {
    choose_repo(labels, &load(app), |label| {
        project_groups::known_paths_for_project(label, app)
    })
}

#[cfg(test)]
#[path = "repos_tests.rs"]
mod tests;
