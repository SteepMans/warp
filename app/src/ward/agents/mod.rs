//! Ward as a worker of the agents system (`agents-service`, contract note «Agents — контракт»).
//!
//! With a service address and a connection key in the settings (the "Agents worker" page) Ward
//! registers as a worker, sends heartbeats, takes assignments, clones the repository into its
//! root, creates a worktree per session, opens the agent (Claude Code or Codex) in its own tab
//! and reports the transcript and the statuses back. There is no separate daemon: the worker
//! lives as long as Ward runs. See [`worker`] for the loops.
//!
//! The sessions do outlive a restart of Ward, an update above all: the state of every session
//! still going is saved beside its files on disk ([`resume`]) and read again at the start of
//! the worker, which picks up an agent that survived and closes one that did not.
//!
//! An assignment may come without a prompt: Ward then first runs the orchestrator session in
//! the vault of the machine profile, which leaves the plan of the task, and only then the
//! executor ([`prepare`], [`plan`]). The machine profile itself (MCP servers, the agent's
//! settings, shared skills, the DNS proxy and the vault) is applied by [`profile`].
//!
//! Settings: address, worker name, root and "take tasks" in the private user preferences; the
//! key and the worker token in the platform secure storage, like the Chimney credentials. For
//! testing, `WARD_AGENTS_URL`, `WARD_AGENTS_KEY`, `WARD_AGENTS_NAME`, `WARD_AGENTS_ROOT` and
//! `WARD_AGENTS_LOCAL_REPOS` are used where nothing is saved.
//!
//! Before cloning, Ward looks for a clone of the same remote the owner already has: the clone
//! search setting lists clones and folders searched a few levels down. Such a clone is only
//! fetched; the session's worktree goes under the root, see [`git::prepare_worktree`].
//!
//! Neither the key nor the token is ever logged, and the contents of the machine profile are
//! not logged either: they carry the secrets of this machine.
//!
//! Reading and answering a session is not the worker's business but the owner's, and it is not
//! limited to this machine: the session screen ([`sessions`]) goes through the owner's own view
//! of the service ([`gateway`]), so the chat of a session running on another Ward is read and
//! written the same way as a local one.
//!
//! The tab of a session on another machine can be opened here as well: the card asks this Ward
//! on its loopback answer, and a tab of its own shows the stream of that session's tab, keys
//! going back ([`attach`]).

pub mod about;
pub mod admission;
pub mod attach;
pub mod client;
pub mod contract;
pub mod gateway;
pub mod git;
pub mod journal;
pub mod launch;
pub mod machine;
pub mod plan;
pub mod prepare;
pub mod presence;
pub mod profile;
pub mod provider;
pub mod resume;
pub mod sessions;
pub mod stream;
pub mod worker;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use warp_core::channel::ChannelState;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::{AppContext, Entity, ModelContext, ModelSpawner, SingletonEntity, ViewHandle};
use warpui_extras::secure_storage::AppContextExt as _;

use self::provider::{Homes, WardUsage};
use self::worker::{
    Host, PaneQuery, Registration, Snapshot, TabSpec, Timing, Worker, WorkerConfig,
};
use crate::pane_group::NewTerminalOptions;
use crate::terminal::TerminalView;
use crate::ward::uri_launch::{self, UriTab};
use crate::workspace::WorkspaceRegistry;

pub const DEFAULT_BASE_URL: &str = "https://chimney.steep-man.ru/agents";

pub const URL_ENV_VAR: &str = "WARD_AGENTS_URL";
pub const KEY_ENV_VAR: &str = "WARD_AGENTS_KEY";
pub const NAME_ENV_VAR: &str = "WARD_AGENTS_NAME";
pub const ROOT_ENV_VAR: &str = "WARD_AGENTS_ROOT";
/// Clones and folders to look for existing clones in, separated like `PATH` (`;` on Windows).
pub const LOCAL_REPOS_ENV_VAR: &str = "WARD_AGENTS_LOCAL_REPOS";

const URL_KEY: &str = "WardAgentsUrl";
const NAME_KEY: &str = "WardAgentsWorkerName";
const ROOT_KEY: &str = "WardAgentsRoot";
const LOCAL_REPOS_KEY: &str = "WardAgentsLocalRepos";
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

/// The folders where people usually keep their clones, those that exist: `~/work`, `~/src`,
/// `~/projects`, `~/repos`, `~/code`, `~/dev` and, on Windows, `<system drive>/work`.
fn default_clone_search() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for name in ["work", "src", "projects", "repos", "code", "dev"] {
            paths.push(home.join(name));
        }
    }
    if cfg!(windows) {
        let drive = environment_value("SystemDrive").unwrap_or_else(|| "C:".to_owned());
        paths.push(PathBuf::from(format!("{drive}/work")));
    }
    paths.retain(|path| path.is_dir());
    paths
}

/// Splits a list of paths: `;` (and `:` outside Windows, as in `PATH`) or new lines.
pub fn parse_path_list(value: &str) -> Vec<PathBuf> {
    value
        .lines()
        .flat_map(std::env::split_paths)
        .map(|path| path.to_string_lossy().trim().to_owned())
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(shellexpand::tilde(&path).into_owned()))
        .collect()
}

/// Where existing clones are looked for: saved, else `WARD_AGENTS_LOCAL_REPOS`, else the
/// usual folders of [`default_clone_search`].
pub fn clone_search(app: &AppContext) -> (Vec<PathBuf>, Source) {
    read_preference(LOCAL_REPOS_KEY, app)
        .map(|value| (parse_path_list(&value), Source::Saved))
        .or_else(|| {
            environment_value(LOCAL_REPOS_ENV_VAR)
                .map(|value| (parse_path_list(&value), Source::Environment))
        })
        .unwrap_or_else(|| (default_clone_search(), Source::Default))
}

pub fn set_clone_search(value: &str, app: &AppContext) {
    write_preference(LOCAL_REPOS_KEY, value, app);
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
        clone_search: clone_search(app).0,
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
    /// What the card in the browser on this computer is told: see [`presence`].
    presence: presence::Shared,
    /// The recent output of the sessions' tabs, by pane: see [`stream`].
    taps: Taps,
    /// The panes being streamed to the service now.
    streams: Arc<parking_lot::Mutex<HashSet<String>>>,
}

type Taps = Arc<parking_lot::Mutex<HashMap<String, stream::SharedTap>>>;

/// The terminal of the pane `pane_uuid`, in whatever window it is.
fn terminal_view_of(pane_uuid: &str, ctx: &AppContext) -> Option<ViewHandle<TerminalView>> {
    for (_, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        for pane_group in workspace.as_ref(ctx).tab_views() {
            let pane_group = pane_group.as_ref(ctx);
            let found = pane_group.terminal_views(ctx).into_iter().find(|view| {
                pane_group
                    .terminal_pane_session_uuid(view.id(), ctx)
                    .is_some_and(|uuid| {
                        crate::ward::chimney::bindings::uuid_hex(&uuid) == pane_uuid
                    })
            });
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

/// Columns and rows of the pane's terminal; `None` when the tab is gone.
fn pane_size_of(pane_uuid: &str, ctx: &AppContext) -> Option<(usize, usize)> {
    let view = terminal_view_of(pane_uuid, ctx)?;
    let size = view.as_ref(ctx).size_info();
    Some((size.columns(), size.rows()))
}

/// Writes keys into the pane as they come, without the paste markers of [`send_text_to_pane`]:
/// the viewer of a stream types, he does not paste.
fn write_bytes_to_pane(
    pane_uuid: &str,
    bytes: Vec<u8>,
    ctx: &mut AppContext,
) -> Result<(), String> {
    let view =
        terminal_view_of(pane_uuid, ctx).ok_or_else(|| "the session's tab is closed".to_owned())?;
    if view.update(ctx, |view, ctx| view.write_user_bytes_to_pty(bytes, ctx)) {
        Ok(())
    } else {
        Err("the tab does not take input now".to_owned())
    }
}

/// Starts keeping the output of the pane: a background task feeds every read of its PTY into
/// the pane's [`stream::Tap`] until the tab closes.
fn start_tap(pane: String, taps: Taps, ctx: &mut ModelContext<AgentsModel>) {
    if taps.lock().contains_key(&pane) {
        return;
    }
    let Some(reads) = terminal_view_of(&pane, ctx).and_then(|view| view.as_ref(ctx).pty_reads(ctx))
    else {
        return;
    };
    let tap: stream::SharedTap = Arc::default();
    taps.lock().insert(pane.clone(), tap.clone());
    let mut receiver = reads.activate_cloned();
    ctx.background_executor()
        .spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(bytes) => tap.lock().feed(bytes),
                    Err(async_broadcast::RecvError::Overflowed(_)) => continue,
                    Err(async_broadcast::RecvError::Closed) => break,
                }
            }
            let mut taps = taps.lock();
            if taps.get(&pane).is_some_and(|kept| Arc::ptr_eq(kept, &tap)) {
                taps.remove(&pane);
            }
        })
        .detach();
}

/// Everything one stream of a tab needs, see [`run_stream`].
struct StreamJob {
    spawner: ModelSpawner<AgentsModel>,
    tap: stream::SharedTap,
    pane: String,
    url: String,
    token: String,
    streams: Arc<parking_lot::Mutex<HashSet<String>>>,
}

/// What goes out to the service, in order.
enum Out {
    Bytes(Arc<Vec<u8>>),
    Text(String),
}

async fn pane_size(spawner: &ModelSpawner<AgentsModel>, pane: &str) -> Option<(usize, usize)> {
    let pane = pane.to_owned();
    spawner
        .spawn(move |_, ctx| pane_size_of(&pane, ctx))
        .await
        .ok()
        .flatten()
}

/// One stream of a tab, from the connection to the end; the pane can be streamed again after.
async fn run_stream(job: StreamJob) {
    let pane = job.pane.clone();
    let streams = job.streams.clone();
    match stream_tab(job).await {
        Ok(()) => log::info!("Ward agents: the stream of a session is over"),
        Err(err) => log::warn!("Ward agents: the stream of a session broke: {err:#}"),
    }
    streams.lock().remove(&pane);
}

/// Connects to the service and streams the tab: its size, the recent output, then every new
/// read; the keys of the viewers go into the tab. Over when the service closes the socket, the
/// socket breaks or the tab is gone.
async fn stream_tab(job: StreamJob) -> anyhow::Result<()> {
    use futures::{SinkExt as _, StreamExt as _};
    use websocket::WebsocketMessage as _;

    let StreamJob {
        spawner,
        tap,
        pane,
        url,
        token,
        ..
    } = job;
    let socket = websocket::WebSocket::connect_with_headers(
        &url,
        None::<&str>,
        vec![("Authorization", format!("Bearer {token}"))],
    )
    .await?;
    let (mut sink, mut incoming) = socket.split().await;
    let size = pane_size(&spawner, &pane).await;
    if let Some((cols, rows)) = size {
        sink.send(websocket::Message::new_text(stream::size_message(
            cols, rows,
        )))
        .await?;
    }
    let (snapshot, output) = tap.lock().subscribe();
    for chunk in snapshot.chunks(stream::CHUNK_BYTES) {
        sink.send(websocket::Message::new_binary(chunk.to_vec()))
            .await?;
    }

    let (out_tx, out_rx) = async_channel::unbounded::<Out>();
    let forward = {
        let out_tx = out_tx.clone();
        async move {
            while let Ok(bytes) = output.recv().await {
                if out_tx.send(Out::Bytes(bytes)).await.is_err() {
                    break;
                }
            }
        }
    };
    let watch = {
        let spawner = spawner.clone();
        let pane = pane.clone();
        async move {
            let mut last = size;
            let mut ticks: u32 = 0;
            loop {
                async_io::Timer::after(stream::SIZE_CHECK).await;
                ticks = ticks.wrapping_add(1);
                // The tab is gone: the stream is over.
                let Some(now) = pane_size(&spawner, &pane).await else {
                    break;
                };
                if last != Some(now) {
                    last = Some(now);
                    let text = stream::size_message(now.0, now.1);
                    if out_tx.send(Out::Text(text)).await.is_err() {
                        break;
                    }
                }
                if ticks.is_multiple_of(stream::PING_EVERY)
                    && out_tx
                        .send(Out::Text(stream::ping_message()))
                        .await
                        .is_err()
                {
                    break;
                }
            }
        }
    };
    let write = async move {
        while let Ok(item) = out_rx.recv().await {
            let message = match item {
                Out::Bytes(bytes) => websocket::Message::new_binary(bytes.to_vec()),
                Out::Text(text) => websocket::Message::new_text(text),
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    };
    let read = {
        let spawner = spawner.clone();
        let pane = pane.clone();
        async move {
            while let Some(Ok(message)) = incoming.next().await {
                let Some(bytes) = message.binary() else {
                    continue;
                };
                let bytes = bytes.to_vec();
                let pane = pane.clone();
                match spawner
                    .spawn(move |_, ctx| write_bytes_to_pane(&pane, bytes, ctx))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => log::debug!("Ward agents: a key of the stream was lost: {err}"),
                    Err(_) => break,
                }
            }
        }
    };
    let output_side = futures::future::join(
        write,
        futures::future::select(Box::pin(forward), Box::pin(watch)),
    );
    futures::future::select(Box::pin(read), Box::pin(output_side)).await;
    Ok(())
}

/// Bracketed paste markers: the agent's input box takes the text as one paste, newlines
/// included, and the carriage return after it submits.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The pane of the open tab that belongs to a session, see [`PaneQuery`]. The pane the session
/// already had wins outright: an update relaunch reopens the tab under the same persistent
/// UUID, so that is the session's own tab. Otherwise the agent each tab runs is read off the
/// pane (`claude_resume_info`, the same detection the tab row and the Chimney bindings use): a
/// tab running this very agent session is the session's tab whatever pane it sits in, and a tab
/// whose agent works in the session's directory is the last resort, for a session whose agent
/// id Ward never found.
fn find_pane_of_session(query: &PaneQuery, ctx: &AppContext) -> Option<String> {
    let mut by_agent_session = None;
    let mut by_cwd = None;
    for (_, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        for pane_group in workspace.as_ref(ctx).tab_views() {
            let pane_group = pane_group.as_ref(ctx);
            for view in pane_group.terminal_views(ctx) {
                let Some(uuid) = pane_group.terminal_pane_session_uuid(view.id(), ctx) else {
                    continue;
                };
                let uuid = crate::ward::chimney::bindings::uuid_hex(&uuid);
                if query.pane_uuid.as_deref() == Some(uuid.as_str()) {
                    return Some(uuid);
                }
                let Some(info) = view.as_ref(ctx).claude_resume_info() else {
                    continue;
                };
                if by_agent_session.is_none()
                    && query.agent_session_id.as_deref() == Some(info.session_id.as_str())
                {
                    by_agent_session = Some(uuid.clone());
                }
                if by_cwd.is_none()
                    && let (Some(cwd), Some(dir)) = (query.cwd.as_deref(), info.cwd.as_deref())
                    && provider::same_path(dir, cwd)
                {
                    by_cwd = Some(uuid.clone());
                }
            }
        }
    }
    by_agent_session.or(by_cwd)
}

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

/// Brings the tab of `pane_uuid` forward: its window to the front of the screen, its tab in
/// front of the others and the pane itself focused, so the owner can type into the agent at
/// once.
fn focus_pane_of(pane_uuid: &str, ctx: &mut AppContext) -> Result<(), String> {
    let mut target = None;
    'search: for (window_id, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        for pane_group in workspace.as_ref(ctx).tab_views() {
            let group = pane_group.as_ref(ctx);
            for view in group.terminal_views(ctx) {
                let Some(uuid) = group.terminal_pane_session_uuid(view.id(), ctx) else {
                    continue;
                };
                if crate::ward::chimney::bindings::uuid_hex(&uuid) != pane_uuid {
                    continue;
                }
                let Some(pane_id) = group.find_pane_id_for_terminal_view(view.id(), ctx) else {
                    continue;
                };
                target = Some((window_id, workspace.clone(), pane_group.clone(), pane_id));
                break 'search;
            }
        }
    }
    let Some((window_id, workspace, pane_group, pane_id)) = target else {
        return Err("the session's tab is closed".to_owned());
    };
    let group_id = pane_group.id();
    workspace.update(ctx, |workspace, ctx| {
        workspace.activate_tab_by_pane_group_id(group_id, ctx);
    });
    pane_group.update(ctx, |group, ctx| {
        group.reveal_and_focus_pane(pane_id, ctx);
    });
    ctx.windows().show_window_and_focus_app(window_id);
    Ok(())
}

/// Opens the tab of a session on another machine, asked for by the card: see [`attach`].
fn open_attach_tab(tab: attach::Attach, ctx: &mut AppContext) -> Result<(), String> {
    let script = attach::script_path();
    attach::write_script(&script).map_err(|err| format!("could not write the viewer: {err}"))?;
    let attach::Attach { url, title } = tab;
    let mut env_vars: HashMap<std::ffi::OsString, std::ffi::OsString> = HashMap::new();
    env_vars.insert(attach::URL_ENV_VAR.into(), url.into());
    env_vars.insert(attach::TITLE_ENV_VAR.into(), title.clone().into());
    let tab = UriTab {
        options: NewTerminalOptions {
            shell: uri_launch::find_shell("powershell", ctx),
            env_vars,
            ..Default::default()
        },
        title: Some(title),
        command: Some(attach::tab_command(&script)),
        ack: None,
    };
    let window_id = ctx.windows().active_window().or_else(|| {
        WorkspaceRegistry::as_ref(ctx)
            .all_workspaces(ctx)
            .first()
            .map(|(window_id, _)| *window_id)
    });
    let (window_id, opened) = uri_launch::open_tab_in_window(window_id, tab, ctx);
    opened.ok_or_else(|| "could not open the tab".to_owned())?;
    // The owner clicked in the browser: Ward comes forward with the new tab.
    ctx.windows().show_window_and_focus_app(window_id);
    Ok(())
}

/// Opens the tabs the card asks for, one after another, until Ward quits.
async fn open_attach_tabs(
    spawner: ModelSpawner<AgentsModel>,
    tabs: async_channel::Receiver<attach::Attach>,
) {
    while let Ok(tab) = tabs.recv().await {
        match spawner.spawn(move |_, ctx| open_attach_tab(tab, ctx)).await {
            Ok(Ok(())) => log::info!("Ward agents: opened the tab of a session on another machine"),
            Ok(Err(err)) => {
                log::warn!(
                    "Ward agents: could not open the tab of a session on another machine: {err}"
                )
            }
            Err(_) => break,
        }
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
        let config = self
            .spawner
            .spawn(|_, ctx| config(ctx))
            .await
            .ok()
            .flatten();
        let now = config
            .as_ref()
            .and_then(|config| presence::presence_of(&config.name, &config.base_url));
        if let Ok(mut shared) = self.presence.write() {
            *shared = now;
        }
        config
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

    async fn find_pane(&self, query: PaneQuery) -> Option<String> {
        self.spawner
            .spawn(move |_, ctx| find_pane_of_session(&query, ctx))
            .await
            .ok()
            .flatten()
    }

    async fn send_text(&self, pane_uuid: &str, text: &str) -> Result<(), String> {
        let pane_uuid = pane_uuid.to_owned();
        let text = text.to_owned();
        self.spawner
            .spawn(move |_, ctx| send_text_to_pane(&pane_uuid, &text, ctx))
            .await
            .map_err(|_| "Ward is shutting down".to_owned())?
    }

    async fn focus_pane(&self, pane_uuid: &str) -> Result<(), String> {
        let pane_uuid = pane_uuid.to_owned();
        self.spawner
            .spawn(move |_, ctx| focus_pane_of(&pane_uuid, ctx))
            .await
            .map_err(|_| "Ward is shutting down".to_owned())?
    }

    async fn tap_pane(&self, pane_uuid: &str) {
        if self.taps.lock().contains_key(pane_uuid) {
            return;
        }
        let pane = pane_uuid.to_owned();
        let taps = self.taps.clone();
        let _ = self
            .spawner
            .spawn(move |_, ctx| start_tap(pane, taps, ctx))
            .await;
    }

    async fn stream_pane(&self, pane_uuid: &str, url: &str, token: &str) -> Result<(), String> {
        self.tap_pane(pane_uuid).await;
        let tap = self
            .taps
            .lock()
            .get(pane_uuid)
            .cloned()
            .ok_or_else(|| "the session's tab is closed".to_owned())?;
        if !self.streams.lock().insert(pane_uuid.to_owned()) {
            // Streaming already: the service shares one stream between its viewers.
            return Ok(());
        }
        let job = StreamJob {
            spawner: self.spawner.clone(),
            tap,
            pane: pane_uuid.to_owned(),
            url: url.to_owned(),
            token: token.to_owned(),
            streams: self.streams.clone(),
        };
        let spawned = self
            .spawner
            .spawn(move |_, ctx| ctx.background_executor().spawn(run_stream(job)).detach())
            .await;
        if spawned.is_err() {
            self.streams.lock().remove(pane_uuid);
            return Err("Ward is shutting down".to_owned());
        }
        Ok(())
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

/// Requests to open a tab not yet taken: the card asks once per click.
const ATTACH_QUEUE: usize = 4;

/// Registers the model and starts the worker loops in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    // The session screen is the owner's view of every machine's sessions, not of this
    // worker's, so it is registered whether this Ward takes tasks or not.
    sessions::init(ctx);
    let Some(homes) = Homes::of_user() else {
        log::warn!("Ward agents: no home directory; the worker is off");
        return;
    };
    let shared: presence::Shared = Arc::default();
    let (attaches, attach_tabs) = async_channel::bounded(ATTACH_QUEUE);
    presence::start(shared.clone(), attaches);
    ctx.add_singleton_model(|ctx| {
        let spawner = ctx.spawner();
        ctx.background_executor()
            .spawn(open_attach_tabs(spawner, attach_tabs))
            .detach();
        let host = Arc::new(UiHost {
            spawner: ctx.spawner(),
            presence: shared,
            taps: Arc::default(),
            streams: Arc::default(),
        });
        let profile_target = profile::Target::of_machine(&homes);
        let worker = Worker::new(
            host,
            Arc::new(machine::RealMachine::default()),
            http_client::Client::new_external(),
            homes,
            profile_target,
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

/// Saves the state of the sessions still going, so the next start of Ward picks them up, and
/// tells the service they are not over. See [`worker::Worker::shutdown_report`].
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
