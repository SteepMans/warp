//! The tabs Ward reopens after it relaunched itself to install an update.
//!
//! Upstream session restore is gated on `general.restore_session`, a setting the user owns and
//! which is off in some profiles. With it off nothing is ever snapshotted, so the relaunch that
//! installs an update comes back with one empty window and every tab is gone, agent tabs
//! included. Ward therefore keeps its own one-shot handoff, written right before terminating
//! for an update and independent of that setting: the windows, their tabs, and per tab the
//! working directory, the custom title, the shell and the command that resumes its CLI agent
//! session.
//!
//! The file is `<state dir>/ward_session_handoff.json`
//! (`%LOCALAPPDATA%\warp\WarpOss\data` on Windows). Every start reads it and deletes it, and
//! reopens its tabs only when nothing else restored the session: with `general.restore_session`
//! on, upstream restore has already brought the windows back and the handoff is dropped. A
//! handoff older than [`MAX_AGE_MS`] belongs to an update that never came back and is ignored.
//!
//! The resume command is run in the reopened tab, exactly as upstream session restore runs it.
//! It comes from Ward's own state directory, which only the user and administrators can write,
//! the same trust the session database already has; unlike a `new_tab` URI it cannot be
//! triggered by a web page, so there is no token check here.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use warp_core::channel::ChannelState;
use warpui::{AppContext, SingletonEntity as _, WindowId};

use crate::app_state::{
    AppState, LeafContents, PaneNodeSnapshot, TabSnapshot, TerminalPaneSnapshot, WindowSnapshot,
    get_app_state,
};
use crate::autoupdate::RelaunchModel;
use crate::pane_group::NewTerminalOptions;
use crate::terminal::ShellLaunchData;
use crate::ward::uri_launch::{self, UriTab};
use crate::workspace::Workspace;

const FILE_NAME: &str = "ward_session_handoff.json";

/// Bumped whenever the fields below change meaning; a file of another version is ignored.
const FORMAT_VERSION: u32 = 1;

/// A handoff this old belongs to an update that never relaunched Ward, so its tabs are not
/// reopened. One day covers an installer that waited for the user before finishing.
const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// What Ward saved before relaunching for an update.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Handoff {
    pub version: u32,
    /// Unix time of the write, in milliseconds.
    pub written_at_ms: u64,
    /// Version Ward ran before the relaunch; for the log only.
    pub from_version: Option<String>,
    pub windows: Vec<HandoffWindow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoffWindow {
    pub active_tab_index: usize,
    pub tabs: Vec<HandoffTab>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoffTab {
    /// Title the user gave the tab; without one the reopened tab names itself after its
    /// directory, as a new tab does.
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// The tab's shell as [`uri_launch::find_shell`] looks it up again: `pwsh`, `bash`,
    /// `wsl:Ubuntu`. Without one the tab opens with the user's default shell.
    pub shell: Option<String>,
    /// Command that resumes the tab's CLI agent session (`claude ... --resume <id>`).
    pub resume_command: Option<String>,
}

/// What a start should do with the handoff file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to reopen: no handoff, an unusable one, or the session is already back.
    Skip,
    /// Reopen these windows and their tabs.
    Open(Vec<HandoffWindow>),
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as u64)
        .unwrap_or_default()
}

/// Where the handoff file lives.
pub fn path() -> PathBuf {
    warp_core::paths::state_dir().join(FILE_NAME)
}

/// The terminal pane whose directory and agent session stand for the whole tab: the focused
/// one, otherwise the first one in the layout. A tab without a terminal pane (settings, a
/// notebook) has none.
fn tab_terminal(root: &PaneNodeSnapshot) -> Option<&TerminalPaneSnapshot> {
    let mut first = None;
    let mut focused = None;
    visit_terminals(root, &mut first, &mut focused);
    focused.or(first)
}

fn visit_terminals<'a>(
    node: &'a PaneNodeSnapshot,
    first: &mut Option<&'a TerminalPaneSnapshot>,
    focused: &mut Option<&'a TerminalPaneSnapshot>,
) {
    match node {
        PaneNodeSnapshot::Branch(branch) => {
            for (_, child) in &branch.children {
                visit_terminals(child, first, focused);
            }
        }
        PaneNodeSnapshot::Leaf(leaf) => {
            let LeafContents::Terminal(terminal) = &leaf.contents else {
                return;
            };
            if first.is_none() {
                *first = Some(terminal);
            }
            if leaf.is_focused && focused.is_none() {
                *focused = Some(terminal);
            }
        }
    }
}

/// Name under which [`uri_launch::find_shell`] can look the pane's shell up again.
fn shell_name(launch_data: Option<&ShellLaunchData>) -> Option<String> {
    match launch_data? {
        ShellLaunchData::Executable {
            executable_path, ..
        }
        | ShellLaunchData::MSYS2 {
            executable_path, ..
        } => Some(executable_path.file_stem()?.to_string_lossy().into_owned()),
        ShellLaunchData::WSL { distro } => Some(format!("wsl:{distro}")),
        // Not a shell that can be found by name.
        ShellLaunchData::DockerSandbox { .. } => None,
    }
}

fn tab_handoff(tab: &TabSnapshot) -> HandoffTab {
    let terminal = tab_terminal(&tab.root);
    HandoffTab {
        title: tab.custom_title.clone(),
        cwd: terminal.and_then(|terminal| terminal.cwd.clone()),
        shell: terminal.and_then(|terminal| shell_name(terminal.shell_launch_data.as_ref())),
        resume_command: terminal.and_then(|terminal| terminal.cli_agent_resume_command.clone()),
    }
}

fn window_handoff(window: &WindowSnapshot) -> HandoffWindow {
    HandoffWindow {
        active_tab_index: window.active_tab_index,
        tabs: window.tabs.iter().map(tab_handoff).collect(),
    }
}

/// Reduces the app state to what Ward reopens. The quake window is left out: it is hidden and
/// cannot be recreated as a normal window on Windows.
pub fn from_app_state(app_state: &AppState, now_ms: u64, from_version: Option<String>) -> Handoff {
    Handoff {
        version: FORMAT_VERSION,
        written_at_ms: now_ms,
        from_version,
        windows: app_state
            .windows
            .iter()
            .filter(|window| !window.quake_mode)
            .map(window_handoff)
            .collect(),
    }
}

/// Decides whether the handoff's tabs are reopened. `restored_windows` is how many windows
/// already exist: upstream session restore brought the session back by itself, so the handoff
/// would duplicate it.
pub fn plan(handoff: Option<Handoff>, restored_windows: usize, now_ms: u64) -> Plan {
    let Some(handoff) = handoff else {
        return Plan::Skip;
    };
    if restored_windows > 0 {
        return Plan::Skip;
    }
    if handoff.version != FORMAT_VERSION {
        return Plan::Skip;
    }
    if now_ms.saturating_sub(handoff.written_at_ms) > MAX_AGE_MS {
        return Plan::Skip;
    }
    let windows: Vec<HandoffWindow> = handoff
        .windows
        .into_iter()
        .filter(|window| !window.tabs.is_empty())
        .map(|mut window| {
            window.active_tab_index = window.active_tab_index.min(window.tabs.len() - 1);
            window
        })
        .collect();
    if windows.is_empty() {
        Plan::Skip
    } else {
        Plan::Open(windows)
    }
}

/// Writes the handoff through a temporary file, so a kill mid-write leaves no half a file.
pub fn write(path: &Path, handoff: &Handoff) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let serialized = serde_json::to_string(handoff).map_err(io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serialized)?;
    fs::rename(&temp, path)
}

/// Reads the handoff; a missing or unreadable file is no handoff.
pub fn read(path: &Path) -> Option<Handoff> {
    match fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(handoff) => Some(handoff),
            Err(err) => {
                log::warn!("Ward: ignoring an unreadable session handoff: {err}");
                None
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                log::warn!("Ward: could not read the session handoff: {err}");
            }
            None
        }
    }
}

/// Drops the handoff file: it is good for one start only.
pub fn remove(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("Ward: could not remove the session handoff: {err}"),
    }
}

/// Saves the open tabs when Ward terminates to install an update, so the next start can reopen
/// them. Call first in `on_will_terminate`, while the windows are still around.
pub fn on_will_terminate(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    // Only an update relaunch brings Ward back by itself; a plain quit leaves session
    // restoration to the user's `general.restore_session`. Launch modes without the autoupdate
    // models (the CLI) have no relaunch to prepare for.
    if !ctx.has_singleton_model::<RelaunchModel>()
        || !RelaunchModel::as_ref(ctx).is_relaunch_requested()
    {
        return;
    }
    let handoff = from_app_state(
        &get_app_state(ctx),
        now_ms(),
        ChannelState::app_version().map(str::to_owned),
    );
    let tabs: usize = handoff.windows.iter().map(|window| window.tabs.len()).sum();
    match write(&path(), &handoff) {
        Ok(()) => log::info!(
            "Ward: saved {tabs} tabs in {} windows for the update relaunch",
            handoff.windows.len()
        ),
        Err(err) => log::warn!("Ward: could not save the tabs of the update relaunch: {err}"),
    }
}

/// Reopens the tabs saved before an update relaunch. Call once at startup, right after
/// upstream session restore and before the fallback empty window is opened.
pub fn restore_after_update(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    let path = path();
    let handoff = read(&path);
    // Good for one start: a handoff not used now is not used later either.
    remove(&path);
    let Plan::Open(windows) = plan(handoff, ctx.window_ids().count(), now_ms()) else {
        return;
    };
    let tabs: usize = windows.iter().map(|window| window.tabs.len()).sum();
    log::info!(
        "Ward: reopening {tabs} tabs in {} windows saved before the update",
        windows.len()
    );
    for window in windows {
        open_window(window, ctx);
    }
}

fn open_window(window: HandoffWindow, ctx: &mut AppContext) {
    let mut window_id: Option<WindowId> = None;
    for tab in &window.tabs {
        let tab = uri_tab(tab, ctx);
        let (opened_in, _) = uri_launch::open_tab_in_window(window_id, tab, ctx);
        window_id = Some(opened_in);
    }
    let Some(window_id) = window_id else {
        return;
    };
    let workspace = ctx
        .views_of_type::<Workspace>(window_id)
        .and_then(|workspaces| workspaces.into_iter().next());
    if let Some(workspace) = workspace {
        workspace.update(ctx, |workspace, ctx| {
            workspace.ward_activate_restored_tab(window.active_tab_index, ctx);
        });
    }
}

fn uri_tab(tab: &HandoffTab, ctx: &AppContext) -> UriTab {
    let shell = tab
        .shell
        .as_deref()
        .and_then(|name| uri_launch::find_shell(name, ctx));
    let env_vars = tab
        .resume_command
        .as_deref()
        .map(|command| crate::ward::claude_launch::env_vars_for_command(command, ctx))
        .unwrap_or_default();
    // A directory that is gone (a removed agent worktree) would fail the shell, so the tab
    // opens in the default directory instead.
    let initial_directory = tab
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .filter(|directory| directory.is_dir());
    UriTab {
        options: NewTerminalOptions {
            shell,
            initial_directory,
            env_vars,
            ..Default::default()
        },
        title: tab.title.clone(),
        command: tab.resume_command.clone(),
        ack: None,
    }
}

#[cfg(test)]
#[path = "session_handoff_tests.rs"]
mod tests;
