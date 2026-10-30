//! Re-reads the Codex rate limits from the newest rollout off the main thread while the app
//! runs. Local files only, so the poll is frequent; an unchanged rollout is not read again.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::rollout::{CodexUsage, MAX_ROLLOUTS_TRIED, read_latest_usage, recent_rollouts};
use super::{is_enabled_by_user, sessions_dir};

const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The rollout the last report came from, as of the last read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RolloutStamp {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

enum ReadResult {
    /// The rollout the report came from has not changed since.
    Unchanged,
    Read(RolloutStamp, CodexUsage),
    /// No rollout with rate limits (Codex not used, or signed in with an API key).
    Missing,
}

fn read_usage(previous: Option<RolloutStamp>) -> ReadResult {
    let Some(sessions_dir) = sessions_dir() else {
        return ReadResult::Missing;
    };
    for (path, modified, len) in recent_rollouts(&sessions_dir)
        .into_iter()
        .take(MAX_ROLLOUTS_TRIED)
    {
        let stamp = RolloutStamp {
            path,
            modified,
            len,
        };
        if previous.as_ref() == Some(&stamp) {
            return ReadResult::Unchanged;
        }
        if let Some(usage) = read_latest_usage(&stamp.path) {
            return ReadResult::Read(stamp, usage);
        }
    }
    ReadResult::Missing
}

pub enum CodexUsageEvent {
    /// A read finished; the header chip should re-render.
    Updated,
}

pub struct CodexUsageModel {
    usage: Option<(RolloutStamp, CodexUsage)>,
    in_flight: bool,
}

impl Entity for CodexUsageModel {
    type Event = CodexUsageEvent;
}

impl SingletonEntity for CodexUsageModel {}

/// Registers the model and starts polling in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    let handle = ctx.add_singleton_model(|_| CodexUsageModel {
        usage: None,
        in_flight: false,
    });
    handle.update(ctx, |model, ctx| model.poll(ctx));
}

/// The model, when registered (OSS desktop builds).
pub fn model(app: &AppContext) -> Option<&CodexUsageModel> {
    app.has_singleton_model::<CodexUsageModel>()
        .then(|| CodexUsageModel::as_ref(app))
}

/// Reads now, e.g. when the chip was just turned on; the regular schedule is kept.
pub fn refresh(app: &mut AppContext) {
    if !app.has_singleton_model::<CodexUsageModel>() {
        return;
    }
    let handle = CodexUsageModel::handle(app);
    handle.update(app, |model, ctx| {
        if !model.in_flight && is_enabled_by_user(ctx) {
            model.read(false, ctx);
        }
        ctx.emit(CodexUsageEvent::Updated);
        ctx.notify();
    });
}

impl CodexUsageModel {
    /// The last report, as of `now`: windows past their reset are left out.
    pub fn usage(&self, now: chrono::DateTime<chrono::Utc>) -> Option<CodexUsage> {
        self.usage.as_ref()?.1.current(now)
    }

    fn schedule(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async move {
                Timer::after(POLL_INTERVAL).await;
            },
            |me, _, ctx| me.poll(ctx),
        );
    }

    fn poll(&mut self, ctx: &mut ModelContext<Self>) {
        if self.in_flight || !is_enabled_by_user(ctx) {
            self.schedule(ctx);
            return;
        }
        self.read(true, ctx);
    }

    fn read(&mut self, reschedule: bool, ctx: &mut ModelContext<Self>) {
        self.in_flight = true;
        let previous = self.usage.as_ref().map(|(stamp, _)| stamp.clone());
        ctx.spawn(
            async move { read_usage(previous) },
            move |me, result, ctx| {
                me.in_flight = false;
                match result {
                    ReadResult::Unchanged => {}
                    ReadResult::Read(stamp, usage) => me.usage = Some((stamp, usage)),
                    ReadResult::Missing => me.usage = None,
                }
                if reschedule {
                    me.schedule(ctx);
                }
                // Re-rendered after every read, changed or not: windows expire and the age in
                // the tooltip moves on while the rollout stays the same.
                ctx.emit(CodexUsageEvent::Updated);
                ctx.notify();
            },
        );
    }
}
