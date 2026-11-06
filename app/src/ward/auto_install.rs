//! Ward installs a downloaded update by itself, the same way "Install and restart" does (the
//! tabs come back after the restart, see [`super::session_handoff`]), but only when that
//! disturbs nobody:
//!
//! - the user is away: no input in any window of Ward for [`IDLE_BEFORE_INSTALL`];
//! - no tab runs a command (a shell at its prompt is fine);
//! - no CLI agent is in the middle of a turn, neither in a plain tab nor in a session of the
//!   agents worker; an agent that finished its turn and waits for input is fine, its session
//!   comes back with `--resume`;
//! - nothing the quit warning would ask about is left: unsaved files, a shared session;
//! - the installer needs no administrator rights (a per-user install).
//!
//! What the conditions are made of is gathered from the app ([`gather`]); whether they hold is
//! decided by one pure function, [`decide`]. The check runs every [`CHECK_EVERY`]; a condition
//! that does not hold only means waiting for the next check, never a forced install.
//!
//! Whether an agent works comes from the same signals Ward's tab rows and the agents worker use:
//! the status the agent's plugin reports for its tab (`CLIAgentSessionsModel`, Claude Code and
//! Codex with the Warp plugin: in progress between the prompt and the end of the turn) and the
//! status of a worker session (`running` / `review` / `blocked`, from its transcript). An agent
//! without plugin status in a plain tab cannot be read, so its tab counts as busy.
//!
//! Before the restart Ward turns "Take tasks" off (the local pause the heartbeat sends as
//! `localPaused`), so the service gives this machine no task while it closes, waits
//! [`PAUSE_SETTLE`] for the heartbeat and checks everything again. The next start turns "Take
//! tasks" back on ([`restore_task_pause`]); a pause the user had set stays.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use warp_core::execution_mode::AppExecutionMode;
use warp_core::user_preferences::GetUserPreferences as _;
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, EntityId, Event, ModelContext, SingletonEntity};

use crate::autoupdate::{self, AutoupdateStage, AutoupdateState};
use crate::code::editor_management::{CodeEditorStatus, CodeEditorSummary};
use crate::features::FeatureFlag;
use crate::session_management::CommandContext;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};
use crate::ward::agents;
use crate::ward::agents::contract::SessionStatus;
use crate::workspace::WorkspaceRegistry;

/// Without input in any window of Ward for this long the user is taken to be away.
pub const IDLE_BEFORE_INSTALL: Duration = Duration::from_secs(15 * 60);

/// How often the conditions are checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(60);

/// Between turning "Take tasks" off and the restart: the heartbeat with the pause goes out at
/// once, and an assignment that came in just before has time to show up as a session.
pub const PAUSE_SETTLE: Duration = Duration::from_secs(15);

/// Private user preferences key of the setting; on unless it is "false".
const ENABLED_KEY: &str = "WardAutoInstallUpdates";

/// Set while "Take tasks" is off because of an automatic install, so the next start turns it
/// back on. Not set when the user had it off already.
const PAUSED_FOR_UPDATE_KEY: &str = "WardAutoInstallPausedTasks";

/// Where the CLI agent of a tab is in its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentTurn {
    /// Between the prompt and the end of the turn.
    Working,
    /// The turn is over: the agent waits for a prompt, an answer or a permission.
    Waiting,
    /// No status from the agent's plugin, so Ward cannot tell.
    Unknown,
}

/// A terminal pane with a block running in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusyPane {
    /// The title of the pane's tab.
    pub tab: String,
    /// The CLI agent the block runs and its turn; `None` for any other command.
    pub agent: Option<(String, AgentTurn)>,
}

/// A session of the agents worker of this Ward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerSession {
    /// How the session is named to the user: `#4704`, else its title.
    pub name: String,
    pub status: SessionStatus,
    /// The turn of the agent in the session's tab; `None` when the tab is not open or Ward
    /// tracks no agent in it.
    pub turn: Option<AgentTurn>,
}

/// Everything the decision looks at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conditions {
    /// "Install updates automatically" is on.
    pub enabled: bool,
    /// The version downloaded, checked and ready to install, if any.
    pub ready_version: Option<String>,
    /// The installer can update this copy without administrator rights.
    pub per_user_install: bool,
    /// Since the last input in any window of Ward.
    pub idle_for: Duration,
    pub sessions: Vec<WorkerSession>,
    /// Panes with a running block, except the tabs of worker sessions: their sessions decide.
    pub busy_panes: Vec<BusyPane>,
    /// An editor or a code review has changes that are not saved.
    pub unsaved_files: bool,
    /// Sessions shared with others right now.
    pub shared_sessions: usize,
}

/// Why the update is not installed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitReason {
    /// Installed for all users: the installer would ask for administrator rights.
    NeedsAdministrator,
    /// A worker session is being set up: the worktree is deployed or the orchestrator works.
    SessionPreparing(String),
    /// The agent of a worker session is in the middle of a turn.
    SessionWorking(String),
    AgentWorking {
        tab: String,
        agent: String,
    },
    /// An agent runs in the tab and gives no status.
    AgentStatusUnknown {
        tab: String,
        agent: String,
    },
    CommandRunning {
        tab: String,
    },
    UnsavedFiles,
    SharedSession,
    /// There was input in Ward less than [`IDLE_BEFORE_INSTALL`] ago.
    UserActive,
}

impl WaitReason {
    /// The line shown under the status on Settings → Updates.
    pub fn text(&self) -> String {
        match self {
            WaitReason::NeedsAdministrator => {
                "Waiting: Ward is installed for all users and its installer needs administrator \
                 rights, use Install and restart"
                    .to_owned()
            }
            WaitReason::SessionPreparing(session) => {
                format!("Waiting: agent session {session} is being prepared")
            }
            WaitReason::SessionWorking(session) => {
                format!("Waiting: agent session {session} is working")
            }
            WaitReason::AgentWorking { tab, agent } => {
                format!("Waiting: {agent} is working in tab \"{tab}\"")
            }
            WaitReason::AgentStatusUnknown { tab, agent } => format!(
                "Waiting: {agent} runs in tab \"{tab}\" and does not report whether it is working"
            ),
            WaitReason::CommandRunning { tab } => {
                format!("Waiting: command running in tab \"{tab}\"")
            }
            WaitReason::UnsavedFiles => "Waiting: there are unsaved file changes".to_owned(),
            WaitReason::SharedSession => "Waiting: a session is being shared".to_owned(),
            WaitReason::UserActive => format!(
                "Waiting: you are using Ward (installs after {} min without input)",
                IDLE_BEFORE_INSTALL.as_secs() / 60
            ),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Decision {
    /// The setting is off.
    Off,
    /// No downloaded update waits to be installed.
    #[default]
    NothingToInstall,
    Wait(WaitReason),
    Install,
}

/// Whether the ready update is installed now. The order only picks the reason shown: lasting
/// ones (a session, a command) come before the user's own activity, which the user sees anyway
/// while looking at the page.
pub fn decide(conditions: &Conditions) -> Decision {
    if !conditions.enabled {
        return Decision::Off;
    }
    if conditions.ready_version.is_none() {
        return Decision::NothingToInstall;
    }
    if !conditions.per_user_install {
        return Decision::Wait(WaitReason::NeedsAdministrator);
    }
    if let Some(reason) = conditions.sessions.iter().find_map(session_wait) {
        return Decision::Wait(reason);
    }
    if let Some(reason) = conditions.busy_panes.iter().find_map(pane_wait) {
        return Decision::Wait(reason);
    }
    if conditions.unsaved_files {
        return Decision::Wait(WaitReason::UnsavedFiles);
    }
    if conditions.shared_sessions > 0 {
        return Decision::Wait(WaitReason::SharedSession);
    }
    if conditions.idle_for < IDLE_BEFORE_INSTALL {
        return Decision::Wait(WaitReason::UserActive);
    }
    Decision::Install
}

/// A worker session holds the install while it is set up and while its agent works. `running`
/// is taken as working unless the session's tab says the turn is over: the worker itself only
/// turns a quiet session `review` minutes after the end of the turn. A session waiting for the
/// owner holds it only if its tab shows the agent working again (the owner just wrote to it).
fn session_wait(session: &WorkerSession) -> Option<WaitReason> {
    let working = match session.status {
        SessionStatus::Queued | SessionStatus::Assigned | SessionStatus::Preparing => {
            return Some(WaitReason::SessionPreparing(session.name.clone()));
        }
        SessionStatus::Running => session.turn != Some(AgentTurn::Waiting),
        SessionStatus::PlanReview | SessionStatus::Blocked | SessionStatus::Review => {
            session.turn == Some(AgentTurn::Working)
        }
        SessionStatus::Failed
        | SessionStatus::Cancelled
        | SessionStatus::Lost
        | SessionStatus::Complete => false,
    };
    working.then(|| WaitReason::SessionWorking(session.name.clone()))
}

fn pane_wait(pane: &BusyPane) -> Option<WaitReason> {
    let tab = pane.tab.clone();
    match &pane.agent {
        None => Some(WaitReason::CommandRunning { tab }),
        Some((agent, AgentTurn::Working)) => Some(WaitReason::AgentWorking {
            tab,
            agent: agent.clone(),
        }),
        Some((agent, AgentTurn::Unknown)) => Some(WaitReason::AgentStatusUnknown {
            tab,
            agent: agent.clone(),
        }),
        Some((_, AgentTurn::Waiting)) => None,
    }
}

/// The turn as the agent's plugin reported it: in progress from the prompt to the end of the
/// turn, anything else (done, cancelled, failed, blocked on the user) waits for input.
pub fn turn_of(status: &CLIAgentSessionStatus, has_plugin_status: bool) -> AgentTurn {
    if !has_plugin_status {
        return AgentTurn::Unknown;
    }
    match status {
        CLIAgentSessionStatus::InProgress => AgentTurn::Working,
        CLIAgentSessionStatus::Success
        | CLIAgentSessionStatus::Failed { .. }
        | CLIAgentSessionStatus::Blocked { .. }
        | CLIAgentSessionStatus::Cancelled => AgentTurn::Waiting,
    }
}

/// How a worker session is named in the waiting line.
pub fn session_name(task_number: Option<u64>, title: &str, session_id: &str) -> String {
    if let Some(number) = task_number {
        return format!("#{number}");
    }
    let title = title.trim();
    if !title.is_empty() {
        return format!("\"{title}\"");
    }
    session_id.chars().take(8).collect()
}

/// Whether the install location from the registry is the directory Ward runs from. Windows
/// paths: case and the trailing separator do not matter.
pub fn same_install_dir(location: &str, install_dir: &str) -> bool {
    fn normalize(path: &str) -> String {
        let path = path.trim();
        let path = path.strip_prefix(r"\\?\").unwrap_or(path);
        path.replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    }
    let location = normalize(location);
    !location.is_empty() && location == normalize(install_dir)
}

/// Whether the event is the user's: everything but the mouse moves Ward makes up itself.
pub fn is_user_input(event: &Event) -> bool {
    !matches!(
        event,
        Event::MouseMoved {
            is_synthetic: true,
            ..
        }
    )
}

/// Unix time in milliseconds of the last input in any window of Ward.
static LAST_INPUT_MS: AtomicI64 = AtomicI64::new(0);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Notes the user's input; called for every event before it is dispatched.
pub fn note_event(event: &Event) {
    if is_user_input(event) {
        LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);
    }
}

fn idle_for() -> Duration {
    let since = now_ms().saturating_sub(LAST_INPUT_MS.load(Ordering::Relaxed));
    Duration::from_millis(since.max(0) as u64)
}

/// "Install updates automatically", on by default.
pub fn is_enabled(app: &AppContext) -> bool {
    app.private_user_preferences()
        .read_value(ENABLED_KEY)
        .ok()
        .flatten()
        .is_none_or(|value| value.trim() != "false")
}

pub fn set_enabled(enabled: bool, app: &mut AppContext) {
    if let Err(err) = app
        .private_user_preferences()
        .write_value(ENABLED_KEY, enabled.to_string())
    {
        log::warn!("Failed to save the Ward automatic update setting: {err:?}");
    }
    refresh(app);
}

/// The turn of the CLI agent Ward tracks in the terminal, with the agent's name.
fn agent_in(terminal_view_id: EntityId, app: &AppContext) -> Option<(String, AgentTurn)> {
    if !app.has_singleton_model::<CLIAgentSessionsModel>() {
        return None;
    }
    let session = CLIAgentSessionsModel::as_ref(app).session(terminal_view_id)?;
    Some((
        session.agent.display_name().to_owned(),
        turn_of(&session.status, session.supports_rich_status()),
    ))
}

fn has_unsaved_files(app: &AppContext) -> bool {
    let mut editors: Vec<CodeEditorStatus> = CodeEditorStatus::all_editors(app).collect();
    for window_id in app.window_ids() {
        editors.extend(CodeEditorStatus::code_review_views_in_window(
            window_id, app,
        ));
    }
    !CodeEditorSummary::new(&editors).unsaved_changes.is_empty()
}

/// Reads the conditions off the app. Panes and sessions are only looked at when there is an
/// update to install.
fn gather(app: &AppContext) -> Conditions {
    let ready_version = match AutoupdateState::as_ref(app).stage() {
        AutoupdateStage::UpdateReady { new_version, .. } => Some(new_version.version.clone()),
        _ => None,
    };
    let mut conditions = Conditions {
        enabled: is_enabled(app),
        ready_version,
        per_user_install: false,
        idle_for: idle_for(),
        sessions: Vec::new(),
        busy_panes: Vec::new(),
        unsaved_files: false,
        shared_sessions: 0,
    };
    if !conditions.enabled || conditions.ready_version.is_none() {
        return conditions;
    }
    conditions.per_user_install = autoupdate::can_install_unattended();

    let views = agents::model(app)
        .map(|model| model.snapshot().sessions.clone())
        .unwrap_or_default();
    let mut session_panes: HashMap<String, usize> = HashMap::new();
    for (index, view) in views.iter().enumerate() {
        if let Some(pane) = &view.pane_uuid {
            session_panes.insert(pane.clone(), index);
        }
        conditions.sessions.push(WorkerSession {
            name: session_name(view.task_number, &view.title, &view.session_id),
            status: view.status,
            turn: None,
        });
    }

    let workspaces = if app.has_singleton_model::<WorkspaceRegistry>() {
        WorkspaceRegistry::as_ref(app).all_workspaces(app)
    } else {
        Vec::new()
    };
    for (_, workspace) in workspaces {
        for pane_group in workspace.as_ref(app).tab_views() {
            let group = pane_group.as_ref(app);
            for view in group.terminal_views(app) {
                let agent = agent_in(view.id(), app);
                let session = group
                    .terminal_pane_session_uuid(view.id(), app)
                    .map(|uuid| crate::ward::chimney::bindings::uuid_hex(&uuid))
                    .and_then(|uuid| session_panes.get(&uuid).copied());
                if let Some(index) = session {
                    conditions.sessions[index].turn = agent.map(|(_, turn)| turn);
                    continue;
                }
                let running = matches!(
                    view.as_ref(app).session_command_context(app),
                    CommandContext::RunningCommand { .. } | CommandContext::RunningAIBlock { .. }
                );
                if running {
                    conditions.busy_panes.push(BusyPane {
                        tab: group.display_title(app),
                        agent,
                    });
                }
            }
        }
    }

    conditions.unsaved_files = has_unsaved_files(app);
    conditions.shared_sessions = crate::session_management::num_shared_sessions(app);
    conditions
}

/// Turns "Take tasks" off for the restart, unless it is off already or this Ward is no
/// worker. Returns whether it did.
fn pause_tasks(app: &AppContext) -> bool {
    if agents::config(app).is_none() || agents::is_paused(app) {
        return false;
    }
    // The mark goes first: whatever happens next, the next start turns "Take tasks" back on.
    if let Err(err) = app
        .private_user_preferences()
        .write_value(PAUSED_FOR_UPDATE_KEY, "true".to_owned())
    {
        log::warn!("Ward: could not note the pause of the automatic update, not pausing: {err:?}");
        return false;
    }
    agents::set_paused(true, app);
    log::info!("Ward: \"Take tasks\" is off for the automatic update");
    true
}

/// Turns "Take tasks" back on after a pause of [`pause_tasks`].
fn resume_tasks(app: &AppContext) {
    if let Err(err) = app
        .private_user_preferences()
        .remove_value(PAUSED_FOR_UPDATE_KEY)
    {
        log::warn!("Ward: could not clear the pause of the automatic update: {err:?}");
    }
    if agents::is_paused(app) {
        agents::set_paused(false, app);
    }
    log::info!("Ward: \"Take tasks\" is on again after the automatic update");
}

/// Turns "Take tasks" back on when the run before turned it off to install an update. Call at
/// startup before the agents worker starts.
pub fn restore_task_pause(app: &AppContext) {
    if !crate::features::is_fork_oss() {
        return;
    }
    let paused_for_update = app
        .private_user_preferences()
        .read_value(PAUSED_FOR_UPDATE_KEY)
        .ok()
        .flatten()
        .is_some_and(|value| value == "true");
    if paused_for_update {
        resume_tasks(app);
    }
}

/// The restart under way: "Take tasks" is off (by Ward when `paused_tasks`) and the conditions
/// get checked once more after [`PAUSE_SETTLE`].
#[derive(Clone, Copy, Debug)]
struct Restart {
    paused_tasks: bool,
}

/// Checks the conditions every [`CHECK_EVERY`] and installs the update when they hold.
pub struct AutoInstallModel {
    decision: Decision,
    restart: Option<Restart>,
}

impl Entity for AutoInstallModel {
    type Event = ();
}

impl SingletonEntity for AutoInstallModel {}

impl AutoInstallModel {
    fn new(ctx: &mut ModelContext<Self>) -> Self {
        // An update that becomes ready shows its waiting line at once; it is installed by a
        // check, not here.
        ctx.observe(&AutoupdateState::handle(ctx), |me, _, ctx| me.refresh(ctx));
        let mut model = Self {
            decision: Decision::default(),
            restart: None,
        };
        model.schedule_check(ctx);
        model
    }

    pub fn decision(&self) -> &Decision {
        &self.decision
    }

    /// The pause before the restart has started.
    pub fn is_restarting(&self) -> bool {
        self.restart.is_some()
    }

    fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        let decision = decide(&gather(ctx));
        if decision != self.decision {
            match &decision {
                Decision::Wait(reason) => {
                    log::info!("Ward: automatic update install: {}", reason.text())
                }
                Decision::Install => {
                    log::info!("Ward: automatic update install: nobody is disturbed")
                }
                Decision::Off | Decision::NothingToInstall => {}
            }
            self.decision = decision;
            ctx.notify();
        }
    }

    fn schedule_check(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(
            async {
                Timer::after(CHECK_EVERY).await;
            },
            |me, _, ctx| {
                me.check(ctx);
                me.schedule_check(ctx);
            },
        );
    }

    fn check(&mut self, ctx: &mut ModelContext<Self>) {
        self.refresh(ctx);
        if self.decision != Decision::Install || self.restart.is_some() {
            return;
        }
        let paused_tasks = pause_tasks(ctx);
        self.restart = Some(Restart { paused_tasks });
        ctx.notify();
        ctx.spawn(
            async {
                Timer::after(PAUSE_SETTLE).await;
            },
            |me, _, ctx| me.finish_restart(ctx),
        );
    }

    fn finish_restart(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(restart) = self.restart.take() else {
            return;
        };
        self.refresh(ctx);
        if self.decision == Decision::Install {
            log::info!("Ward: installing the update by itself and restarting");
            if autoupdate::initiate_unattended_relaunch_for_update(ctx) {
                // Ward is closing; the next start turns "Take tasks" back on.
                self.restart = Some(restart);
                ctx.notify();
                return;
            }
        }
        log::info!("Ward: the automatic update install is called off, conditions changed");
        if restart.paused_tasks {
            resume_tasks(ctx);
        }
        ctx.notify();
    }
}

pub fn model(app: &AppContext) -> Option<&AutoInstallModel> {
    app.has_singleton_model::<AutoInstallModel>()
        .then(|| AutoInstallModel::as_ref(app))
}

/// Checks the conditions again for the page, without installing anything.
pub fn refresh(app: &mut AppContext) {
    if app.has_singleton_model::<AutoInstallModel>() {
        AutoInstallModel::handle(app).update(app, |model, ctx| model.refresh(ctx));
    }
}

/// The line about the automatic install for Settings → Updates; `None` when there is nothing
/// to say (setting off, no update waiting).
pub fn status_text(app: &AppContext) -> Option<String> {
    let model = model(app)?;
    if model.is_restarting() {
        return Some(
            "Installing automatically: new agent tasks are paused, Ward restarts in a moment"
                .to_owned(),
        );
    }
    match model.decision() {
        Decision::Off | Decision::NothingToInstall => None,
        Decision::Wait(reason) => Some(reason.text()),
        Decision::Install => Some("Nobody is using Ward: installs at the next check".to_owned()),
    }
}

/// Starts the checks in the Windows OSS build with updates on. Call after the autoupdate
/// models are registered.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || !cfg!(windows) {
        return;
    }
    if !FeatureFlag::Autoupdate.is_enabled()
        || !AppExecutionMode::as_ref(ctx).can_autoupdate()
        || !ctx.has_singleton_model::<AutoupdateState>()
    {
        return;
    }
    // The start counts as input: right after it the user may well be there.
    LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);
    ctx.add_singleton_model(AutoInstallModel::new);
}

#[cfg(test)]
#[path = "auto_install_tests.rs"]
mod tests;
