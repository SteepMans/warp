//! Chimney authentication, the same flow as chimney-mcp (`src/chimney.ts`): a service account
//! signs in with `POST /api/v1/auth/login` (email and password), the access token is renewed
//! with `POST /api/v1/auth/refresh` (refresh token) a minute before it expires, and a request
//! rejected with 401 renews the token once (refresh, then login) and is retried. Only one
//! login or refresh runs at a time; concurrent requests wait for it and reuse its token.
//!
//! A static access token, when configured, overrides the service account: it is used as is and
//! never renewed. Tokens live in memory only. Neither tokens nor the password are ever logged.

use std::future::Future;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::lock::Mutex as AsyncMutex;
use parking_lot::Mutex;
use serde::Deserialize;

/// How Ward signs in to Chimney.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A ready access token (settings or `CHIMNEY_ACCESS_TOKEN`), used as is.
    Token(String),
    /// The service account (settings or `CHIMNEY_EMAIL` / `CHIMNEY_PASSWORD`).
    Password { email: String, password: String },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(_) => f.write_str("Token(<redacted>)"),
            Self::Password { email, .. } => f
                .debug_struct("Password")
                .field("email", email)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

/// Body of the login and refresh responses (`TokenResponse` in chimney-mcp).
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default, rename = "requires2fa")]
    pub requires_2fa: Option<bool>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &self.access_token.as_ref().map(|_| "<redacted>"))
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_in", &self.expires_in)
            .field("requires_2fa", &self.requires_2fa)
            .finish()
    }
}

/// A failed login or refresh call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// Non-2xx status.
    Status(u16),
    Network,
    Parse,
}

/// The two auth endpoints; a fake one in tests.
#[async_trait]
pub trait AuthTransport: Send + Sync {
    async fn login(
        &self,
        base_url: &str,
        email: &str,
        password: &str,
    ) -> Result<TokenResponse, TransportError>;

    async fn refresh(
        &self,
        base_url: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse, TransportError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Neither a token nor an email and a password.
    NotConfigured,
    /// Chimney returned 401 for the static access token.
    TokenRejected,
    /// The login call returned this status (wrong email or password for 401).
    LoginRejected(u16),
    /// The service account has Telegram 2FA on: a password login cannot work.
    TwoFactorRequired,
    /// A 2xx login response without an access token.
    NoAccessToken,
    Network,
    Parse,
}

impl AuthError {
    pub fn message(&self) -> String {
        match self {
            Self::NotConfigured => "No Chimney credentials".to_owned(),
            Self::TokenRejected => "Chimney rejected the access token".to_owned(),
            Self::LoginRejected(401) => "Chimney rejected the email or password".to_owned(),
            Self::LoginRejected(status) => format!("Chimney login failed (HTTP {status})"),
            Self::TwoFactorRequired => {
                "The Chimney account has 2FA on; password login is impossible".to_owned()
            }
            Self::NoAccessToken => "Chimney login returned no access token".to_owned(),
            Self::Network => "Chimney is unreachable".to_owned(),
            Self::Parse => "Unexpected login response from Chimney".to_owned(),
        }
    }
}

/// What the settings page shows about the connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AuthStatus {
    /// No request has been made with the current credentials yet.
    #[default]
    Unknown,
    /// Requests use the static access token.
    Token,
    SignedIn { email: String },
    Failed(AuthError),
}

/// Default access token lifetime when the response does not say (chimney-mcp uses the same).
const DEFAULT_TTL_SECS: u64 = 900;
/// Renew this long before the token expires, so requests do not hit the boundary.
const RENEW_MARGIN_SECS: u64 = 60;
/// But keep a token for at least this long.
const MIN_LIFETIME_SECS: u64 = 30;

#[derive(Default)]
struct State {
    /// Base URL and credentials the tokens below belong to; other ones reset the state.
    key: Option<(String, Credentials)>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<Instant>,
}

impl State {
    fn reset_for(&mut self, base_url: &str, credentials: &Credentials) {
        let same = self
            .key
            .as_ref()
            .is_some_and(|(url, creds)| url == base_url && creds == credentials);
        if !same {
            *self = State {
                key: Some((base_url.to_owned(), credentials.clone())),
                ..Default::default()
            };
        }
    }

    fn apply(&mut self, response: TokenResponse, now: Instant) -> Result<String, AuthError> {
        let access_token = response
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or(AuthError::NoAccessToken)?;
        if let Some(refresh_token) = response.refresh_token.filter(|token| !token.is_empty()) {
            self.refresh_token = Some(refresh_token);
        }
        let ttl = response.expires_in.unwrap_or(DEFAULT_TTL_SECS);
        let lifetime = ttl.saturating_sub(RENEW_MARGIN_SECS).max(MIN_LIFETIME_SECS);
        self.expires_at = Some(now + Duration::from_secs(lifetime));
        self.access_token = Some(access_token.clone());
        Ok(access_token)
    }
}

/// Token state shared by all Chimney requests of the app.
#[derive(Default)]
pub struct Auth {
    /// Held for the whole login or refresh: this is what keeps auth single in-flight.
    state: AsyncMutex<State>,
    /// Snapshot for the UI, readable without awaiting.
    status: Mutex<AuthStatus>,
}

impl Auth {
    pub fn status(&self) -> AuthStatus {
        self.status.lock().clone()
    }

    fn set_status(&self, status: AuthStatus) {
        *self.status.lock() = status;
    }

    /// A usable access token: the static one, the cached one while fresh, else a refreshed
    /// one, else a new login.
    pub async fn token(
        &self,
        base_url: &str,
        credentials: &Credentials,
        transport: &dyn AuthTransport,
        now: &(dyn Fn() -> Instant + Sync),
    ) -> Result<String, AuthError> {
        let (email, password) = match credentials {
            Credentials::Token(token) => return Ok(token.clone()),
            Credentials::Password { email, password } => (email, password),
        };
        let mut state = self.state.lock().await;
        state.reset_for(base_url, credentials);
        if let (Some(token), Some(expires_at)) = (&state.access_token, state.expires_at)
            && now() < expires_at
        {
            return Ok(token.clone());
        }
        state.access_token = None;

        if let Some(refresh_token) = state.refresh_token.clone() {
            match transport.refresh(base_url, &refresh_token).await {
                Ok(response) => {
                    if let Ok(token) = state.apply(response, now()) {
                        return Ok(token);
                    }
                    state.refresh_token = None;
                }
                // The refresh token expired or was revoked: sign in again.
                Err(TransportError::Status(_)) | Err(TransportError::Parse) => {
                    state.refresh_token = None;
                }
                Err(TransportError::Network) => return Err(AuthError::Network),
            }
        }

        let response = transport
            .login(base_url, email, password)
            .await
            .map_err(|err| match err {
                TransportError::Status(status) => AuthError::LoginRejected(status),
                TransportError::Network => AuthError::Network,
                TransportError::Parse => AuthError::Parse,
            })?;
        if response.requires_2fa == Some(true) {
            return Err(AuthError::TwoFactorRequired);
        }
        state.apply(response, now())
    }

    /// Forgets `token` after Chimney rejected it, unless another request already replaced it.
    /// The refresh token is kept, so the next [`Self::token`] tries a refresh first.
    pub async fn invalidate(&self, token: &str) {
        let mut state = self.state.lock().await;
        if state.access_token.as_deref() == Some(token) {
            state.access_token = None;
            state.expires_at = None;
        }
    }

    /// Runs `send` with a token. A 401 with the service account renews the token and runs
    /// `send` once more; a 401 with the static token is final.
    pub async fn send_authorized<T, E, F, Fut>(
        &self,
        base_url: &str,
        credentials: &Credentials,
        transport: &dyn AuthTransport,
        now: &(dyn Fn() -> Instant + Sync),
        mut send: F,
    ) -> Result<T, E>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: AuthorizedError,
    {
        let mut retried = false;
        loop {
            let token = match self.token(base_url, credentials, transport, now).await {
                Ok(token) => token,
                Err(err) => {
                    self.set_status(AuthStatus::Failed(err));
                    return Err(E::from_auth(err));
                }
            };
            let result = send(token.clone()).await;
            match &result {
                Err(err) if err.is_unauthorized() => match credentials {
                    Credentials::Token(_) => {
                        self.set_status(AuthStatus::Failed(AuthError::TokenRejected));
                    }
                    Credentials::Password { .. } if !retried => {
                        self.invalidate(&token).await;
                        retried = true;
                        continue;
                    }
                    Credentials::Password { .. } => {
                        self.set_status(AuthStatus::Failed(AuthError::LoginRejected(401)));
                    }
                },
                _ => self.set_status(match credentials {
                    Credentials::Token(_) => AuthStatus::Token,
                    Credentials::Password { email, .. } => AuthStatus::SignedIn {
                        email: email.clone(),
                    },
                }),
            }
            return result;
        }
    }

    /// Records that no credentials are configured.
    pub fn set_not_configured(&self) {
        self.set_status(AuthStatus::Failed(AuthError::NotConfigured));
    }
}

/// Errors of authorized requests: they can carry an [`AuthError`] and tell a 401 apart.
pub trait AuthorizedError {
    fn from_auth(err: AuthError) -> Self;
    fn is_unauthorized(&self) -> bool;
}

/// Login and refresh over HTTP. The JSON bodies are built by hand so no request hook ever
/// sees a serialized payload with the password.
pub struct HttpAuthTransport<'a> {
    pub client: &'a http_client::Client,
    pub timeout: Duration,
}

impl HttpAuthTransport<'_> {
    async fn post(&self, url: String, body: String) -> Result<TokenResponse, TransportError> {
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|_| TransportError::Network)?;
        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            return Err(TransportError::Status(status));
        }
        let text = response.text().await.map_err(|_| TransportError::Network)?;
        serde_json::from_str(&text).map_err(|_| TransportError::Parse)
    }
}

#[async_trait]
impl AuthTransport for HttpAuthTransport<'_> {
    async fn login(
        &self,
        base_url: &str,
        email: &str,
        password: &str,
    ) -> Result<TokenResponse, TransportError> {
        let body = serde_json::json!({ "email": email, "password": password }).to_string();
        self.post(format!("{base_url}/api/v1/auth/login"), body).await
    }

    async fn refresh(
        &self,
        base_url: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse, TransportError> {
        let body = serde_json::json!({ "refreshToken": refresh_token }).to_string();
        self.post(format!("{base_url}/api/v1/auth/refresh"), body).await
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
