//! Ward as a worker of the agents system (`agents-service`, contract note «Agents — контракт»).
//!
//! With a service address and a connection key in the settings (Third party CLI agents page,
//! "Agents worker") Ward registers as a worker, sends heartbeats, takes assignments, clones the
//! repository into its root, creates a worktree per session, opens the agent (Claude Code or
//! Codex) in its own tab and reports the transcript and the statuses back. There is no separate
//! daemon: the worker lives as long as Ward runs. See [`worker`] for the loops.
//!
//! Settings: address, worker name, root and "take tasks" in the private user preferences; the
//! key and the worker token in the platform secure storage, like the Chimney credentials. For
//! testing, `WARD_AGENTS_URL`, `WARD_AGENTS_KEY`, `WARD_AGENTS_NAME` and `WARD_AGENTS_ROOT` are
//! used where nothing is saved. Neither the key nor the token is ever logged.

pub mod admission;
pub mod client;
pub mod contract;
pub mod git;
pub mod journal;
pub mod launch;
pub mod machine;
pub mod provider;
pub mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use warp_core::channel::ChannelState;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::{AppContext, Entity, ModelSpawner, SingletonEntity};
use warpui_extras::secure_storage::AppContextExt as _;

use self::provider::{Homes, WardUsage};
use self::worker::{Host, Registration, Snapshot, TabSpec, Timing, Worker, WorkerConfig};
use crate::pane_group::NewTerminalOptions;
use crate::ward::uri_launch::{self, UriTab};
use crate::workspace::WorkspaceRegistry;

pub const DEFAULT_BASE_URL: &str = "https://chimney.steep-man.ru/agents";

pub const URL_ENV_VAR: &str = "WARD_AGENTS_URL";
pub const KEY_ENV_VAR: &str = "WARD_AGENTS_KEY";
pub const NAME_ENV_VAR: &str = "WARD_AGENTS_NAME";
pub const ROOT_ENV_VAR: &str = "WARD_AGENTS_ROOT";

const URL_KEY: &str = "WardAgentsUrl";
const NAME_KEY: &str = "WardAgentsWorkerName";
const ROOT_KEY: &str = "WardAgentsRoot";
const PAUSED_KEY: &str = "WardAgentsPaused";
/// Secure storage keys.
const KEY_KEY: &str = "WardAgentsKey";
const REGISTRATION_KEY: &str = "WardAgentsRegistration";

/// Where a setting's value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Saved,
    Environment,
    Default,
}

fn read_preference(key: &str, app: &AppContext) -> Option<String> {
    app.private_user_preferences()
        .read_value(key)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn write_preference(key: &str, value: &str, app: &AppContext) {
    let value = value.trim();
    let result = if value.is_empty() {
        app.private_user_preferences().remove_value(key)
    } else {
        app.private_user_preferences()
            .write_value(key, value.to_owned())
    };
    if let Err(err) = result {
        log::warn!("Failed to save Ward agents setting {key}: {err:?}");
    }
}

fn environment_value(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn secure_value(key: &str, app: &AppContext) -> Option<String> {
    app.secure_storage()
        .read_value(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn set_secure_value(key: &str, value: &str, app: &AppContext) {
    let value = value.trim();
    let result = if value.is_empty() {
        app.secure_storage().remove_value(key)
    } else {
        app.secure_storage()
            .write_value_with_owner_only_fallback(key, value)
    };
    if let Err(err) = result {
        // The error never contains the value itself.
        log::warn!("Failed to update {key} in secure storage: {err}");
    }
}

/// The service address: saved, else `WARD_AGENTS_URL`, else the default.
pub fn base_url(app: &AppContext) -> (String, Source) {
    if let Some(url) =
        read_preference(URL_KEY, app).and_then(|url| client::normalize_base_url(&url))
    {
        return (url, Source::Saved);
    }
    if let Some(url) =
        environment_value(URL_ENV_VAR).and_then(|url| client::normalize_base_url(&url))
    {
        return (url, Source::Environment);
    }
    (DEFAULT_BASE_URL.to_owned(), Source::Default)
}

/// Saves the address; an empty one goes back to the default. False for an invalid URL.
pub fn set_base_url(url: &str, app: &AppContext) -> bool {
    if url.trim().is_empty() {
        write_preference(URL_KEY, "", app);
        return true;
    }
    match client::normalize_base_url(url) {
        Some(url) => {
            write_preference(URL_KEY, &url, app);
            true
        }
        None => false,
    }
}

/// The connection key: saved, else `WARD_AGENTS_KEY`.
fn key(app: &AppContext) -> Option<(String, Source)> {
    secure_value(KEY_KEY, app)
        .map(|key| (key, Source::Saved))
        .or_else(|| environment_value(KEY_ENV_VAR).map(|key| (key, Source::Environment)))
}

pub fn key_source(app: &AppContext) -> Option<Source> {
    key(app).map(|(_, source)| source)
}

/// Saves the key; an empty one removes it. The saved worker token goes with the old key.
pub fn set_key(key: &str, app: &AppContext) {
    set_secure_value(KEY_KEY, key, app);
    set_secure_value(REGISTRATION_KEY, "", app);
}

fn host_name() -> String {
    sysinfo::System::host_name()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "ward".to_owned())
}

/// The worker name: saved, else `WARD_AGENTS_NAME`, else the host name.
pub fn worker_name(app: &AppContext) -> (String, Source) {
    read_preference(NAME_KEY, app)
        .map(|name| (name, Source::Saved))
        .or_else(|| environment_value(NAME_ENV_VAR).map(|name| (name, Source::Environment)))
        .unwrap_or_else(|| (host_name(), Source::Default))
}

pub fn set_worker_name(name: &str, app: &AppContext) {
    write_preference(NAME_KEY, name, app);
}

fn default_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("agents-work")
}

/// The repository root: saved, else `WARD_AGENTS_ROOT`, else `~/agents-work`.
pub fn root(app: &AppContext) -> (PathBuf, Source) {
    let expand = |path: String| PathBuf::from(shellexpand::tilde(&path).into_owned());
    read_preference(ROOT_KEY, app)
        .map(|path| (expand(path), Source::Saved))
        .or_else(|| environment_value(ROOT_ENV_VAR).map(|path| (expand(path), Source::Environment)))
        .unwrap_or_else(|| (default_root(), Source::Default))
}

pub fn set_root(path: &str, app: &AppContext) {
    write_preference(ROOT_KEY, path, app);
}

/// "Take tasks" is off.
pub fn is_paused(app: &AppContext) -> bool {
    read_preference(PAUSED_KEY, app).is_some_and(|value| value == "true")
}

pub fn set_paused(paused: bool, app: &AppContext) {
    write_preference(PAUSED_KEY, if paused { "true" } else { "" }, app);
}

/// Everything the worker needs, or `None` without a key.
pub fn config(app: &AppContext) -> Option<WorkerConfig> {
    let (key, _) = key(app)?;
    Some(WorkerConfig {
        base_url: base_url(app).0,
        key,
        name: worker_name(app).0,
        root: root(app).0,
        paused: is_paused(app),
    })
}

fn load_registration(app: &AppContext) -> Option<Registration> {
    serde_json::from_str(&secure_value(REGISTRATION_KEY, app)?).ok()
}

fn store_registration(registration: Option<&Registration>, app: &AppContext) {
    let json = registration
        .and_then(|registration| serde_json::to_string(registration).ok())
        .unwrap_or_default();
    set_secure_value(REGISTRATION_KEY, &json, app);
}

pub enum AgentsEvent {
    /// The worker state changed; see [`AgentsModel::snapshot`].
    Updated,
}

/// The worker of this Ward and the latest state of it for the UI.
pub struct AgentsModel {
    worker: Option<Arc<Worker>>,
    snapshot: Snapshot,
}

impl Entity for AgentsModel {
    type Event = AgentsEvent;
}

impl SingletonEntity for AgentsModel {}

impl AgentsModel {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

pub fn model(app: &AppContext) -> Option<&AgentsModel> {
    app.has_singleton_model::<AgentsModel>()
        .then(|| AgentsModel::as_ref(app))
}

/// Opens tabs, types into them and reads the settings on the main thread for the worker.
struct UiHost {
    spawner: ModelSpawner<AgentsModel>,
}

/// Bracketed paste markers: the agent's input box takes the text as one paste, newlines
/// included, and the carriage return after it submits.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

fn send_text_to_pane(pane_uuid: &str, text: &str, ctx: &mut AppContext) -> Result<(), String> {
    let mut found = None;
    for (_, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        for pane_group in workspace.as_ref(ctx).tab_views() {
            let pane_group = pane_group.as_ref(ctx);
            found = pane_group.terminal_views(ctx).into_iter().find(|view| {
                pane_group
                    .terminal_pane_session_uuid(view.id(), ctx)
                    .is_some_and(|uuid| {
                        crate::ward::chimney::bindings::uuid_hex(&uuid) == pane_uuid
                    })
            });
            if found.is_some() {
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let view = found.ok_or_else(|| "the session's tab is closed".to_owned())?;
    let mut bytes = PASTE_START.to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(PASTE_END);
    bytes.push(b'\r');
    if view.update(ctx, |view, ctx| view.write_user_bytes_to_pty(bytes, ctx)) {
        Ok(())
    } else {
        Err("the tab does not take input now".to_owned())
    }
}

fn open_tab(spec: TabSpec, ctx: &mut AppContext) -> Result<Option<String>, String> {
    if !spec.cwd.is_dir() {
        return Err("the worktree is not a directory".to_owned());
    }
    let shell = spec
        .shell
        .and_then(|name| uri_launch::find_shell(name, ctx));
    let mut env_vars = crate::ward::claude_launch::env_vars_for_command("claude", ctx);
    // Also for the runner's own shell, in case the agent is started from it by hand.
    env_vars.insert("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE".into(), "1".into());
    let tab = UriTab {
        options: NewTerminalOptions {
            shell,
            initial_directory: Some(spec.cwd),
            env_vars,
            ..Default::default()
        },
        title: Some(spec.title),
        command: Some(spec.command),
        ack: None,
    };
    let window_id = ctx.windows().active_window().or_else(|| {
        WorkspaceRegistry::as_ref(ctx)
            .all_workspaces(ctx)
            .first()
            .map(|(window_id, _)| *window_id)
    });
    let (_, opened) = uri_launch::open_tab_in_window(window_id, tab, ctx);
    opened
        .map(|opened| opened.pane_uuid)
        .ok_or_else(|| "could not open the tab".to_owned())
}

#[async_trait]
impl Host for UiHost {
    async fn config(&self) -> Option<WorkerConfig> {
        self.spawner
            .spawn(|_, ctx| config(ctx))
            .await
            .ok()
            .flatten()
    }

    async fn load_registration(&self) -> Option<Registration> {
        self.spawner
            .spawn(|_, ctx| load_registration(ctx))
            .await
            .ok()
            .flatten()
    }

    async fn store_registration(&self, registration: Option<Registration>) {
        let _ = self
            .spawner
            .spawn(move |_, ctx| store_registration(registration.as_ref(), ctx))
            .await;
    }

    async fn ward_usage(&self) -> WardUsage {
        self.spawner
            .spawn(|_, ctx| WardUsage {
                claude: crate::ward::claude_usage::model::model(ctx)
                    .and_then(|model| model.usage().cloned()),
            })
            .await
            .unwrap_or_default()
    }

    async fn open_tab(&self, tab: TabSpec) -> Result<Option<String>, String> {
        self.spawner
            .spawn(move |_, ctx| open_tab(tab, ctx))
            .await
            .map_err(|_| "Ward is shutting down".to_owned())?
    }

    async fn send_text(&self, pane_uuid: &str, text: &str) -> Result<(), String> {
        let pane_uuid = pane_uuid.to_owned();
        let text = text.to_owned();
        self.spawner
            .spawn(move |_, ctx| send_text_to_pane(&pane_uuid, &text, ctx))
            .await
            .map_err(|_| "Ward is shutting down".to_owned())?
    }

    async fn publish(&self, snapshot: Snapshot) {
        let _ = self
            .spawner
            .spawn(move |me, ctx| {
                if me.snapshot != snapshot {
                    me.snapshot = snapshot;
                    ctx.emit(AgentsEvent::Updated);
                    ctx.notify();
                }
            })
            .await;
    }
}

/// Registers the model and starts the worker loops in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    let Some(homes) = Homes::of_user() else {
        log::warn!("Ward agents: no home directory; the worker is off");
        return;
    };
    ctx.add_singleton_model(|ctx| {
        let host = Arc::new(UiHost {
            spawner: ctx.spawner(),
        });
        let worker = Worker::new(
            host,
            Arc::new(machine::RealMachine::default()),
            http_client::Client::new_external(),
            homes,
            Timing::default(),
            ChannelState::app_version().unwrap_or("unknown").to_owned(),
        );
        ctx.spawn(worker.clone().run(), |_, _, _| {});
        AgentsModel {
            worker: Some(worker),
            snapshot: Snapshot::default(),
        }
    });
}

/// How long quitting waits for the last statuses.
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Tells the service that the sessions still going end with Ward.
pub fn on_will_terminate(app: &mut AppContext) {
    let Some(report) = model(app)
        .and_then(|model| model.worker.as_ref())
        .and_then(|worker| worker.shutdown_report())
    else {
        return;
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ward-agents-shutdown".to_owned())
        .spawn(move || {
            let worker::ShutdownReport {
                base_url,
                token,
                sessions,
            } = report;
            let client = http_client::Client::new_external();
            let api = client::Api {
                client: &client,
                base_url: &base_url,
            };
            futures::executor::block_on(async {
                for (session_id, status) in &sessions {
                    let _ = api.status(&token, session_id, status).await;
                }
            });
            let _ = sender.send(());
        });
    if spawned.is_ok() {
        let _ = receiver.recv_timeout(SHUTDOWN_TIMEOUT);
    }
}
