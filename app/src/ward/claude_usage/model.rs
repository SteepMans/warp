//! Polls the Claude subscription usage off the main thread while the app runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::usage::{Backoff, POLL_INTERVAL, PollOutcome, Usage, parse_access_token, parse_usage};
use super::{credentials_path, is_enabled_by_user};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Beta header the OAuth endpoints require.
const OAUTH_BETA: &str = "oauth-2025-04-20";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Larger responses are not usage reports.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Larger files are not Claude Code credentials.
const MAX_CREDENTIALS_BYTES: u64 = 1024 * 1024;

/// A report this old is hidden rather than shown as current, e.g. after a long network outage.
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FetchError {
    /// No credentials file, or no token in it.
    NoCredentials,
    /// 401 or 403.
    Unauthorized,
    Status(u16),
    Network,
    Parse,
}

impl FetchError {
    fn outcome(self) -> PollOutcome {
        match self {
            Self::NoCredentials | Self::Unauthorized => PollOutcome::Unauthorized,
            Self::Status(status) => PollOutcome::from_status(status),
            Self::Network | Self::Parse => PollOutcome::Failed,
        }
    }

    fn message(self) -> String {
        match self {
            Self::NoCredentials => "no Claude Code OAuth credentials".to_owned(),
            Self::Unauthorized => "the OAuth token was rejected".to_owned(),
            Self::Status(status) => format!("HTTP {status}"),
            Self::Network => "network error".to_owned(),
            Self::Parse => "unexpected response".to_owned(),
        }
    }
}

pub enum ClaudeUsageEvent {
    /// A poll finished; the header chip should re-render.
    Updated,
}

pub struct ClaudeUsageModel {
    client: Arc<http_client::Client>,
    usage: Option<(Usage, Instant)>,
    backoff: Backoff,
    in_flight: bool,
}

impl Entity for ClaudeUsageModel {
    type Event = ClaudeUsageEvent;
}

impl SingletonEntity for ClaudeUsageModel {}

/// Registers the model and starts polling in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    let handle = ctx.add_singleton_model(|_| ClaudeUsageModel {
        // A plain client: no Warp headers, and the network log never sees the token.
        client: Arc::new(http_client::Client::new_external()),
        usage: None,
        backoff: Backoff::default(),
        in_flight: false,
    });
    handle.update(ctx, |model, ctx| model.poll(ctx));
}

/// The model, when registered (OSS desktop builds).
pub fn model(app: &AppContext) -> Option<&ClaudeUsageModel> {
    app.has_singleton_model::<ClaudeUsageModel>()
        .then(|| ClaudeUsageModel::as_ref(app))
}

/// Polls now, e.g. when the chip was just turned on; the regular schedule is kept.
pub fn refresh(app: &mut AppContext) {
    if !app.has_singleton_model::<ClaudeUsageModel>() {
        return;
    }
    let handle = ClaudeUsageModel::handle(app);
    handle.update(app, |model, ctx| {
        if !model.in_flight && is_enabled_by_user(ctx) {
            model.fetch(false, ctx);
        }
        ctx.emit(ClaudeUsageEvent::Updated);
        ctx.notify();
    });
}

impl ClaudeUsageModel {
    /// The last report, unless it is stale.
    pub fn usage(&self) -> Option<&Usage> {
        self.usage
            .as_ref()
            .filter(|(_, fetched_at)| fetched_at.elapsed() < STALE_AFTER)
            .map(|(usage, _)| usage)
    }

    fn schedule(&mut self, delay: Duration, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async move {
                Timer::after(delay).await;
            },
            |me, _, ctx| me.poll(ctx),
        );
    }

    fn poll(&mut self, ctx: &mut ModelContext<Self>) {
        if self.in_flight || !is_enabled_by_user(ctx) {
            self.schedule(POLL_INTERVAL, ctx);
            return;
        }
        self.fetch(true, ctx);
    }

    fn fetch(&mut self, reschedule: bool, ctx: &mut ModelContext<Self>) {
        self.in_flight = true;
        let client = self.client.clone();
        ctx.spawn(
            async move { fetch_usage(&client).await },
            move |me, result, ctx| {
                me.in_flight = false;
                let outcome = match result {
                    Ok(usage) => {
                        me.usage = Some((usage, Instant::now()));
                        PollOutcome::Fetched
                    }
                    Err(err) => {
                        log::info!("Ward: fetching Claude usage failed: {}", err.message());
                        let outcome = err.outcome();
                        // A transient failure keeps the last report until it goes stale.
                        if outcome == PollOutcome::Unauthorized {
                            me.usage = None;
                        }
                        outcome
                    }
                };
                let delay = me.backoff.next_delay(outcome);
                if reschedule {
                    me.schedule(delay, ctx);
                }
                ctx.emit(ClaudeUsageEvent::Updated);
                ctx.notify();
            },
        );
    }
}

/// Reads the token from the credentials file (fresh on every poll) and fetches the usage.
async fn fetch_usage(client: &http_client::Client) -> Result<Usage, FetchError> {
    let path = credentials_path().ok_or(FetchError::NoCredentials)?;
    let too_large = std::fs::metadata(&path)
        .map(|metadata| metadata.len() > MAX_CREDENTIALS_BYTES)
        .map_err(|_| FetchError::NoCredentials)?;
    if too_large {
        return Err(FetchError::NoCredentials);
    }
    let credentials = std::fs::read_to_string(&path).map_err(|_| FetchError::NoCredentials)?;
    let token = parse_access_token(&credentials).ok_or(FetchError::NoCredentials)?;
    drop(credentials);

    let response = client
        .get(USAGE_URL)
        .bearer_auth(&token)
        .header("anthropic-beta", OAUTH_BETA)
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|_| FetchError::Network)?;
    let status = response.status().as_u16();
    match PollOutcome::from_status(status) {
        PollOutcome::Fetched => {}
        PollOutcome::Unauthorized => return Err(FetchError::Unauthorized),
        PollOutcome::Failed => return Err(FetchError::Status(status)),
    }
    let body = response.text().await.map_err(|_| FetchError::Network)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(FetchError::Parse);
    }
    parse_usage(&body).ok_or(FetchError::Parse)
}
