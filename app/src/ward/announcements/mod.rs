//! Announcements from the Ward maintainers, broadcast to every installation.
//!
//! `GET https://chimney.steep-man.ru/api/v1/ward/announcements` is public: no auth and no
//! secrets on the client. The feed is polled shortly after startup and then every
//! [`backoff::POLL_INTERVAL`], backing off on failures. Announcements that are current and meant
//! for this build's version (see [`feed`]) and were not shown yet are offered to the workspace
//! of the active window, which shows them and reports them back as shown. Shown ids are kept in
//! private user preferences (see [`shown`]), so a restart does not show them again.

pub mod backoff;
pub mod feed;
pub mod shown;

use std::sync::Arc;
use std::time::Duration;

use warp_core::channel::ChannelState;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use self::backoff::{Backoff, PollOutcome};
use self::feed::Announcement;
use self::shown::ShownIds;

const FEED_URL: &str = "https://chimney.steep-man.ru/api/v1/ward/announcements";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Lets the restored windows come up first, so there is a workspace to show the toasts in.
const FIRST_POLL_DELAY: Duration = Duration::from_secs(5);

/// Private user preferences key of the JSON list of shown announcement ids.
const SHOWN_PREFERENCE_KEY: &str = "WardShownAnnouncements";

pub enum AnnouncementsEvent {
    /// New announcements are waiting to be shown; see [`AnnouncementsModel::pending`].
    Pending,
}

pub struct AnnouncementsModel {
    client: Arc<http_client::Client>,
    backoff: Backoff,
    pending: Vec<Announcement>,
}

impl Entity for AnnouncementsModel {
    type Event = AnnouncementsEvent;
}

impl SingletonEntity for AnnouncementsModel {}

/// Registers the model and starts polling in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    ctx.add_singleton_model(|ctx| {
        let mut model = AnnouncementsModel {
            client: Arc::new(http_client::Client::new_external()),
            backoff: Backoff::default(),
            pending: Vec::new(),
        };
        model.schedule(FIRST_POLL_DELAY, ctx);
        model
    });
}

fn load_shown(app: &AppContext) -> ShownIds {
    let json = app
        .private_user_preferences()
        .read_value(SHOWN_PREFERENCE_KEY)
        .ok()
        .flatten();
    ShownIds::from_json(json.as_deref())
}

impl AnnouncementsModel {
    /// Announcements of the last poll that were not shown yet, oldest first.
    pub fn pending(&self) -> &[Announcement] {
        &self.pending
    }

    /// Records announcements as shown, so they are neither offered again nor shown after a
    /// restart.
    pub fn mark_shown(&mut self, ids: &[String], ctx: &mut ModelContext<Self>) {
        self.pending
            .retain(|announcement| !ids.contains(&announcement.id));
        let mut shown = load_shown(ctx);
        let mut changed = false;
        for id in ids {
            changed |= shown.insert(id);
        }
        if !changed {
            return;
        }
        if let Err(err) = ctx
            .private_user_preferences()
            .write_value(SHOWN_PREFERENCE_KEY, shown.to_json())
        {
            log::warn!("Failed to save shown Ward announcements: {err:?}");
        }
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
        let client = self.client.clone();
        ctx.spawn(
            async move { fetch_feed(&client).await },
            |me, result, ctx| {
                let outcome = match result {
                    Ok(announcements) => {
                        let version =
                            feed::version_numbers(ChannelState::app_version().unwrap_or_default());
                        me.pending = feed::select_new(
                            announcements,
                            &load_shown(ctx),
                            chrono::Utc::now(),
                            &version,
                        );
                        if !me.pending.is_empty() {
                            ctx.emit(AnnouncementsEvent::Pending);
                        }
                        PollOutcome::Fetched
                    }
                    Err(outcome) => outcome,
                };
                let delay = me.backoff.next_delay(outcome);
                me.schedule(delay, ctx);
            },
        );
    }
}

async fn fetch_feed(client: &http_client::Client) -> Result<Vec<Announcement>, PollOutcome> {
    let response = client
        .get(FEED_URL)
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|err| {
            log::info!("Ward announcements: request failed: {err}");
            PollOutcome::Retryable
        })?;
    let status = response.status().as_u16();
    let outcome = PollOutcome::from_status(status);
    if outcome != PollOutcome::Fetched {
        log::info!("Ward announcements: HTTP {status}");
        return Err(outcome);
    }
    let declared_length = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared_length.is_some_and(|length| length > feed::MAX_BODY_BYTES) {
        log::info!("Ward announcements: response is too large");
        return Err(PollOutcome::Unexpected);
    }
    let body = response.text().await.map_err(|err| {
        log::info!("Ward announcements: reading the response failed: {err}");
        PollOutcome::Retryable
    })?;
    feed::parse_feed(&body).ok_or_else(|| {
        log::info!("Ward announcements: the response is not a feed");
        PollOutcome::Unexpected
    })
}
