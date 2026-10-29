//! Anonymous usage metrics of the OSS build, pushed to the fork's own Pushgateway.
//!
//! `PUT https://chimney.steep-man.ru/ward-metrics/metrics/job/ward/instance/<installation_id>`
//! with HTTP basic auth (`ward` / the token baked in from `WARD_METRICS_TOKEN` at compile time)
//! and a Prometheus text body (see [`body`]). Builds without the token (local and dev builds)
//! never collect or push anything. The user can turn pushing off on the Privacy settings page.
//!
//! Pushes happen shortly after startup, once the previous run has been classified, then every
//! [`backoff::PUSH_INTERVAL`] (backing off on failures, stopping on 401/403 until restart), and
//! once more on a clean exit with `ward_app_running 0` and no agent session series. Counters
//! are cumulative over the installation's lifetime and persisted in [`state`].

pub mod backoff;
pub mod body;
pub mod state;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use warp_core::channel::ChannelState;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use self::backoff::{Backoff, PushOutcome};
use self::body::{AgentLabel, MetricsSnapshot, SessionState};
use self::state::{MetricsState, RestoredAgent, StartupFacts, StatePaths};
use crate::app_state::{LeafContents, PaneNodeSnapshot};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::ward::agent_state::{self, StatusKind};

/// Pushgateway group URL without the instance (the installation id).
const PUSH_URL_PREFIX: &str =
    "https://chimney.steep-man.ru/ward-metrics/metrics/job/ward/instance/";

const BASIC_AUTH_USER: &str = "ward";

/// Baked in by CI; absent in local builds, which disables metrics entirely.
const TOKEN: Option<&str> = option_env!("WARD_METRICS_TOKEN");

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The final push on exit never holds up quitting for longer than this.
const FINAL_PUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Lets the restored windows come up first, so the first push already sees their sessions.
const FIRST_PUSH_DELAY: Duration = Duration::from_secs(3);

/// Private user preferences key of the Privacy page toggle; metrics are on unless it is "false".
const ENABLED_PREFERENCE_KEY: &str = "WardMetricsEnabled";

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

/// Panics of this run; the panic hook must not take the [`RUNTIME`] lock.
static PANICS_THIS_RUN: AtomicU64 = AtomicU64::new(0);

/// Set by the Windows autoupdate code when an installer log from an update attempt was found.
static UPDATE_LOG_FOUND: AtomicBool = AtomicBool::new(false);

/// Set after a 401/403: the token is not accepted, so nothing is pushed until restart.
static REJECTED: AtomicBool = AtomicBool::new(false);

fn token() -> Option<&'static str> {
    TOKEN.map(str::trim).filter(|token| !token.is_empty())
}

/// Whether this build collects metrics at all.
pub fn is_available() -> bool {
    crate::features::is_fork_oss() && token().is_some() && !cfg!(target_family = "wasm")
}

/// Whether the user allows pushing metrics (Privacy settings page). On by default.
pub fn is_enabled_by_user(app: &AppContext) -> bool {
    app.private_user_preferences()
        .read_value(ENABLED_PREFERENCE_KEY)
        .ok()
        .flatten()
        .is_none_or(|value| value != "false")
}

pub fn set_enabled_by_user(enabled: bool, app: &AppContext) {
    if let Err(err) = app
        .private_user_preferences()
        .write_value(ENABLED_PREFERENCE_KEY, enabled.to_string())
    {
        log::warn!("Failed to save the Ward metrics setting: {err:?}");
    }
}

/// Called by the Windows autoupdate code when it finds the installer log of an update
/// attempt; together with an unchanged version it means the update failed to install.
pub fn note_update_log_found() {
    UPDATE_LOG_FOUND.store(true, Ordering::SeqCst);
}

struct Runtime {
    paths: StatePaths,
    state: MetricsState,
    version: String,
    process_start_time_seconds: u64,
}

impl Runtime {
    fn push_url(&self) -> String {
        format!("{PUSH_URL_PREFIX}{}", self.state.installation_id)
    }

    fn render(
        &self,
        running: bool,
        agent_sessions: Option<&BTreeMap<(AgentLabel, SessionState), u64>>,
    ) -> String {
        body::render(&MetricsSnapshot {
            version: &self.version,
            channel: channel(),
            arch: arch(),
            running,
            process_start_time_seconds: self.process_start_time_seconds,
            state: &self.state,
            panics_this_run: PANICS_THIS_RUN.load(Ordering::SeqCst),
            agent_sessions,
        })
    }

    fn save(&self) -> bool {
        match state::save(&self.paths.state, &self.state) {
            Ok(()) => true,
            Err(err) => {
                log::warn!("Failed to save Ward metrics state: {err}");
                false
            }
        }
    }
}

fn runtime() -> MutexGuard<'static, Option<Runtime>> {
    RUNTIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Master builds are the stable channel of the fork.
fn channel() -> &'static str {
    if ChannelState::is_release_bundle() {
        "stable"
    } else {
        "dev"
    }
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// Classifies the previous run, counts this launch and starts pushing. Call once at startup,
/// after the autoupdate installer log has been checked.
pub fn init(ctx: &mut AppContext) {
    if !is_available() {
        return;
    }
    let version = ChannelState::app_version().unwrap_or("unknown").to_owned();
    let paths = StatePaths::in_dir(&warp_core::paths::state_dir());
    let mut metrics_state = state::load(&paths.state);
    let marker_panics = state::read_panic_marker(&paths.panic_marker);
    let previous_run = metrics_state.begin_run(
        &StartupFacts {
            current_version: &version,
            marker_panics,
            update_log_found: UPDATE_LOG_FOUND.load(Ordering::SeqCst),
        },
        || uuid::Uuid::new_v4().to_string(),
    );
    log::info!("Ward metrics: previous run {previous_run:?}");

    let process_start_time_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let runtime_state = Runtime {
        paths: paths.clone(),
        state: metrics_state,
        version,
        process_start_time_seconds,
    };
    // Only drop the marker once its panics are part of the saved counters.
    if runtime_state.save() && marker_panics > 0 {
        state::remove_panic_marker(&paths.panic_marker);
    }
    *runtime() = Some(runtime_state);

    install_panic_hook(paths.panic_marker);

    ctx.add_singleton_model(|ctx| {
        let mut pusher = MetricsPusher::new();
        pusher.schedule(FIRST_PUSH_DELAY, ctx);
        pusher
    });
}

fn install_panic_hook(marker: PathBuf) {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let panics = PANICS_THIS_RUN.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = state::write_panic_marker(&marker, panics);
        previous_hook(info);
    }));
}

/// Counts the panes of a tab restored after a restart, by the CLI agent they resume.
pub fn record_restored_panes(root: &PaneNodeSnapshot) {
    if !is_available() {
        return;
    }
    let mut agents = Vec::new();
    collect_restored_agents(root, &mut agents);
    if agents.is_empty() {
        return;
    }
    if let Some(runtime) = runtime().as_mut() {
        for agent in agents {
            runtime.state.restored_sessions.increment(agent);
        }
        runtime.save();
    }
}

fn collect_restored_agents(node: &PaneNodeSnapshot, agents: &mut Vec<RestoredAgent>) {
    match node {
        PaneNodeSnapshot::Branch(branch) => {
            for (_, child) in &branch.children {
                collect_restored_agents(child, agents);
            }
        }
        PaneNodeSnapshot::Leaf(leaf) => {
            if let LeafContents::Terminal(terminal) = &leaf.contents {
                agents.push(RestoredAgent::of_resume_command(
                    terminal.cli_agent_resume_command.as_deref(),
                ));
            }
        }
    }
}

/// Live CLI agent sessions by agent and coarse state.
fn agent_sessions(app: &AppContext) -> BTreeMap<(AgentLabel, SessionState), u64> {
    let mut sessions = BTreeMap::new();
    for (terminal_view_id, session) in CLIAgentSessionsModel::as_ref(app).sessions() {
        let agent = match session.agent {
            CLIAgent::Claude => AgentLabel::Claude,
            CLIAgent::Codex => AgentLabel::Codex,
            _ => AgentLabel::Other,
        };
        let state = match agent_state::agent_snapshot(*terminal_view_id, app) {
            Some(snapshot) => SessionState::of_agent_state(snapshot.state),
            // Sessions without plugin status only have the coarse status.
            None => SessionState::of_status_kind(StatusKind::of(&session.status)),
        };
        *sessions.entry((agent, state)).or_insert(0) += 1;
    }
    sessions
}

/// Final push on a clean exit: records the clean exit and reports the app as stopped.
pub fn on_will_terminate(app: &mut AppContext) {
    let Some((url, body)) = runtime().as_mut().map(|runtime| {
        runtime.state.end_run();
        runtime.save();
        (runtime.push_url(), runtime.render(false, None))
    }) else {
        return;
    };
    if REJECTED.load(Ordering::SeqCst) || !is_enabled_by_user(app) {
        return;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ward-metrics-final-push".to_owned())
        .spawn(move || {
            let client = http_client::Client::new_external();
            let outcome =
                futures::executor::block_on(send(&client, &url, body, FINAL_PUSH_TIMEOUT));
            let _ = sender.send(outcome);
        });
    if spawned.is_ok() {
        // Gives up waiting after the timeout; the thread dies with the process.
        let _ = receiver.recv_timeout(FINAL_PUSH_TIMEOUT);
    }
}

async fn send(
    client: &http_client::Client,
    url: &str,
    body: String,
    timeout: Duration,
) -> PushOutcome {
    let Some(token) = token() else {
        return PushOutcome::Rejected;
    };
    if body.len() >= body::MAX_BODY_BYTES {
        log::warn!("Ward metrics body is too large ({} bytes)", body.len());
        return PushOutcome::Unexpected;
    }
    let result = client
        .put(url)
        .basic_auth(BASIC_AUTH_USER, Some(token))
        .header("Content-Type", "text/plain; version=0.0.4")
        .timeout(timeout)
        .body(body)
        .send()
        .await;
    match result {
        Ok(response) => {
            let status = response.status();
            let outcome = PushOutcome::from_status(status.as_u16());
            if outcome != PushOutcome::Accepted {
                log::info!("Ward metrics push returned {status}");
            }
            outcome
        }
        Err(err) => {
            log::info!("Ward metrics push failed: {err}");
            PushOutcome::Retryable
        }
    }
}

/// Pushes the metrics periodically. Registered by [`init`] only when metrics are available.
pub struct MetricsPusher {
    client: Arc<http_client::Client>,
    backoff: Backoff,
}

impl Entity for MetricsPusher {
    type Event = ();
}

impl SingletonEntity for MetricsPusher {}

impl MetricsPusher {
    fn new() -> Self {
        Self {
            // A plain client: no Warp headers, and the network log never sees the token.
            client: Arc::new(http_client::Client::new_external()),
            backoff: Backoff::default(),
        }
    }

    fn schedule(&mut self, delay: Duration, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async move {
                Timer::after(delay).await;
            },
            |me, _, ctx| me.push(ctx),
        );
    }

    fn push(&mut self, ctx: &mut ModelContext<Self>) {
        if REJECTED.load(Ordering::SeqCst) {
            return;
        }
        if !is_enabled_by_user(ctx) {
            // Check again later, in case the user turns metrics back on.
            self.schedule(backoff::PUSH_INTERVAL, ctx);
            return;
        }
        let sessions = agent_sessions(ctx);
        let Some((url, body)) = runtime().as_ref().map(|runtime| {
            runtime.save();
            (runtime.push_url(), runtime.render(true, Some(&sessions)))
        }) else {
            return;
        };
        let client = self.client.clone();
        ctx.spawn(
            async move { send(&client, &url, body, REQUEST_TIMEOUT).await },
            |me, outcome, ctx| match me.backoff.next_delay(outcome) {
                Some(delay) => me.schedule(delay, ctx),
                None => {
                    log::warn!("Ward metrics token was rejected; not pushing until restart");
                    REJECTED.store(true, Ordering::SeqCst);
                }
            },
        );
    }
}
