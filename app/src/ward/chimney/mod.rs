//! Chimney, the owner's kanban, in Ward: tabs bound to cards show the card's column, agents can
//! be started from a card. Read-only use of the REST API: Ward never moves or edits a card.
//!
//! Configuration: base URL and board id in the private user preferences; the service account
//! email and password (or `CHIMNEY_EMAIL` / `CHIMNEY_PASSWORD` in the environment) in the
//! platform secure storage, see [`auth`] for the sign-in flow. A static access token (secure
//! storage or `CHIMNEY_ACCESS_TOKEN`) overrides the account. Secrets are never logged.

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

use self::auth::Credentials;

pub const DEFAULT_BASE_URL: &str = "https://chimney.steep-man.ru";
pub const DEFAULT_BOARD_ID: u64 = 5;

/// Environment fallback for the access token, as in chimney-mcp.
pub const TOKEN_ENV_VAR: &str = "CHIMNEY_ACCESS_TOKEN";

const BASE_URL_KEY: &str = "WardChimneyBaseUrl";
const BOARD_ID_KEY: &str = "WardChimneyBoardId";
/// Environment fallbacks for the service account, as in chimney-mcp.
pub const EMAIL_ENV_VAR: &str = "CHIMNEY_EMAIL";
pub const PASSWORD_ENV_VAR: &str = "CHIMNEY_PASSWORD";

/// Secure storage keys of the access token and the service account.
const TOKEN_KEY: &str = "WardChimneyAccessToken";
const EMAIL_KEY: &str = "WardChimneyEmail";
const PASSWORD_KEY: &str = "WardChimneyPassword";

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

/// A secret from the secure storage. Never logged.
fn secure_value(key: &str, app: &AppContext) -> Option<String> {
    app.secure_storage()
        .read_value(key)
        .ok()
        .map(|value| trim_secret(key, &value))
        .filter(|value| !value.is_empty())
}

fn environment_value(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|value| trim_secret(var, &value))
        .filter(|value| !value.is_empty())
}

/// Passwords keep inner and edge spaces; only line endings (a `\r` from a Windows pipe) go.
/// Everything else is trimmed.
fn trim_secret(key: &str, value: &str) -> String {
    if key == PASSWORD_KEY || key == PASSWORD_ENV_VAR {
        value.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        value.trim().to_owned()
    }
}

fn source_of(key: &str, var: &str, app: &AppContext) -> TokenSource {
    if secure_value(key, app).is_some() {
        TokenSource::SecureStorage
    } else if environment_value(var).is_some() {
        TokenSource::Environment
    } else {
        TokenSource::Missing
    }
}

/// The access token: the one saved in Ward's settings, else the environment variable.
pub fn access_token(app: &AppContext) -> Option<String> {
    secure_value(TOKEN_KEY, app).or_else(|| environment_value(TOKEN_ENV_VAR))
}

pub fn token_source(app: &AppContext) -> TokenSource {
    source_of(TOKEN_KEY, TOKEN_ENV_VAR, app)
}

/// The service account email: saved in Ward's settings, else `CHIMNEY_EMAIL`.
pub fn email(app: &AppContext) -> Option<String> {
    secure_value(EMAIL_KEY, app).or_else(|| environment_value(EMAIL_ENV_VAR))
}

pub fn email_source(app: &AppContext) -> TokenSource {
    source_of(EMAIL_KEY, EMAIL_ENV_VAR, app)
}

pub fn password_source(app: &AppContext) -> TokenSource {
    source_of(PASSWORD_KEY, PASSWORD_ENV_VAR, app)
}

/// How requests sign in: a static access token overrides the service account; each value
/// comes from the settings, else from the environment.
pub fn credentials(app: &AppContext) -> Option<Credentials> {
    if let Some(token) = access_token(app) {
        return Some(Credentials::Token(token));
    }
    let email = email(app)?;
    let password =
        secure_value(PASSWORD_KEY, app).or_else(|| environment_value(PASSWORD_ENV_VAR))?;
    Some(Credentials::Password { email, password })
}

/// Saves a secret in the secure storage; an empty value removes it.
fn set_secure_value(key: &str, value: &str, app: &AppContext) {
    let value = trim_secret(key, value);
    let result = if value.is_empty() {
        app.secure_storage().remove_value(key)
    } else {
        app.secure_storage()
            .write_value_with_owner_only_fallback(key, &value)
    };
    if let Err(err) = result {
        // The error never contains the value itself.
        log::warn!("Failed to update {key} in secure storage: {err}");
    }
}

/// Saves the access token in the secure storage; an empty token removes it.
pub fn set_access_token(token: &str, app: &AppContext) {
    set_secure_value(TOKEN_KEY, token, app);
}

/// Saves the service account email; an empty one removes it.
pub fn set_email(email: &str, app: &AppContext) {
    set_secure_value(EMAIL_KEY, email, app);
}

/// Saves the service account password; an empty one removes it.
pub fn set_password(password: &str, app: &AppContext) {
    set_secure_value(PASSWORD_KEY, password, app);
}

/// Everything a request needs, captured on the main thread.
#[derive(Clone)]
pub struct RequestConfig {
    pub base_url: String,
    pub credentials: Credentials,
}

impl std::fmt::Debug for RequestConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestConfig")
            .field("base_url", &self.base_url)
            .field("credentials", &self.credentials)
            .finish()
    }
}

pub fn request_config(app: &AppContext) -> Option<RequestConfig> {
    Some(RequestConfig {
        base_url: base_url(app),
        credentials: credentials(app)?,
    })
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
