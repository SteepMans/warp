//! Chimney, the owner's kanban, in Ward: tabs bound to cards show the card's column, agents can
//! be started from a card. Read-only use of the REST API: Ward never moves or edits a card.
//!
//! Configuration: base URL and board id in the private user preferences, the access token in
//! the platform secure storage (or `CHIMNEY_ACCESS_TOKEN` in the environment). The token is
//! never logged.

pub mod api;
pub mod auth;
pub mod bindings;
pub mod chip;
pub mod model;
pub mod picker;
pub mod repos;
pub mod slug;

use warp_core::user_preferences::GetUserPreferences as _;
use warpui::AppContext;
use warpui_extras::secure_storage::AppContextExt as _;

pub const DEFAULT_BASE_URL: &str = "https://chimney.steep-man.ru";
pub const DEFAULT_BOARD_ID: u64 = 5;

/// Environment fallback for the access token, as in chimney-mcp.
pub const TOKEN_ENV_VAR: &str = "CHIMNEY_ACCESS_TOKEN";

const BASE_URL_KEY: &str = "WardChimneyBaseUrl";
const BOARD_ID_KEY: &str = "WardChimneyBoardId";
/// Secure storage key of the access token.
const TOKEN_KEY: &str = "WardChimneyAccessToken";

/// Trims and validates a base URL: `http(s)://host[/path]` without a trailing slash.
pub fn normalize_base_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    (!rest.is_empty() && !rest.contains(char::is_whitespace)).then(|| url.to_owned())
}

fn read_preference(key: &str, app: &AppContext) -> Option<String> {
    app.private_user_preferences()
        .read_value(key)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn write_preference(key: &str, value: String, app: &AppContext) {
    if let Err(err) = app.private_user_preferences().write_value(key, value) {
        log::warn!("Failed to save Ward Chimney setting {key}: {err:?}");
    }
}

pub fn base_url(app: &AppContext) -> String {
    read_preference(BASE_URL_KEY, app)
        .and_then(|url| normalize_base_url(&url))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned())
}

/// Saves the base URL. Returns false (and saves nothing) for an invalid URL.
pub fn set_base_url(url: &str, app: &AppContext) -> bool {
    match normalize_base_url(url) {
        Some(url) => {
            write_preference(BASE_URL_KEY, url, app);
            true
        }
        None => false,
    }
}

/// The board pickers list cards from.
pub fn board_id(app: &AppContext) -> u64 {
    read_preference(BOARD_ID_KEY, app)
        .and_then(|id| id.parse().ok())
        .filter(|id| *id > 0)
        .unwrap_or(DEFAULT_BOARD_ID)
}

pub fn set_board_id(board_id: u64, app: &AppContext) {
    write_preference(BOARD_ID_KEY, board_id.to_string(), app);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenSource {
    SecureStorage,
    Environment,
    Missing,
}

fn stored_token(app: &AppContext) -> Option<String> {
    app.secure_storage()
        .read_value(TOKEN_KEY)
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

fn environment_token() -> Option<String> {
    std::env::var(TOKEN_ENV_VAR)
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

/// The access token: the one saved in Ward's settings, else the environment variable.
pub fn access_token(app: &AppContext) -> Option<String> {
    stored_token(app).or_else(environment_token)
}

pub fn token_source(app: &AppContext) -> TokenSource {
    if stored_token(app).is_some() {
        TokenSource::SecureStorage
    } else if environment_token().is_some() {
        TokenSource::Environment
    } else {
        TokenSource::Missing
    }
}

/// Saves the access token in the secure storage; an empty token removes it.
pub fn set_access_token(token: &str, app: &AppContext) {
    let token = token.trim();
    let result = if token.is_empty() {
        app.secure_storage().remove_value(TOKEN_KEY)
    } else {
        app.secure_storage()
            .write_value_with_owner_only_fallback(TOKEN_KEY, token)
    };
    if let Err(err) = result {
        // The error never contains the token itself.
        log::warn!("Failed to update the Ward Chimney token in secure storage: {err}");
    }
}

/// Everything a request needs, captured on the main thread.
#[derive(Clone)]
pub struct RequestConfig {
    pub base_url: String,
    pub token: String,
}

impl std::fmt::Debug for RequestConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestConfig")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish()
    }
}

pub fn request_config(app: &AppContext) -> Option<RequestConfig> {
    Some(RequestConfig {
        base_url: base_url(app),
        token: access_token(app)?,
    })
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
