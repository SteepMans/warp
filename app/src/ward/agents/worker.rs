//! The worker itself: four loops on a background executor, sharing [`Worker`].
//!
//! - **Control**: reads the settings, registers (again after a 401 or a changed key), sends a
//!   heartbeat every `heartbeatSec` with the load, the subscription usage, the session
//!   statuses, "Take tasks" and the state of the machine profile, backing off while the service
//!   is unreachable. Turning "Take tasks" off or on sends a heartbeat at once
//!   ([`Worker::heartbeat_now`]).
//! - **Inbox**: long-polls `GET /inbox`, acks or refuses assignments and queues commands.
//! - **Sessions**: moves every session through its phases (preparation, plan review, worktree,
//!   tab, agent session found, exit code), ships the transcript every
//!   [`Timing::journal_every`] and sends statuses. What could not be sent waits and goes out
//!   with the next tick. An agent in remote control mode does not end its process after its
//!   answer, so a session whose transcript stands still for [`IDLE_AFTER_ANSWER`] is read for
//!   its last turn: a closing answer ends the session as `review`, a question for the owner
//!   turns it `blocked` until somebody answers (see [`Worker::gone_quiet`]).
//! - **Profile**: applies the machine profile whenever its version changes, checks the proxy
//!   and reports both; with the key gone it takes Ward's own entries off the machine.
//!
//! The sessions outlive a restart of Ward. Every session that is still going has its state in
//! `<root>/.agents/<short sessionId>/session.json` ([`super::resume`]), and a start of Ward
//! reads those files before any loop begins ([`Worker::restore_sessions`]): an agent whose
//! process is still there (or which came back with the tab of an update relaunch) is picked up
//! where it was left, transcript position included, and one that did not survive ends with a
//! status that says so instead of looking like a session that never started. Quitting
//! therefore no longer reports the sessions as failed, see [`ShutdownReport`]. The session's
//! tab comes back with it: the owner's `message` is typed into the pane the session had, the
//! pane that runs its agent session, or the pane whose agent works in its directory, see
//! [`Worker::find_pane`].
//!
//! An assignment without a prompt (or without a repository or a branch) goes through the
//! preparation phase first: the orchestrator session in the vault writes `plan.json`, Ward
//! sends the plan to the service, waits for the orchestrator to end its own session (see
//! [`PREPARE_EXIT_WAIT`]) and only then, straight away or after the owner's `approve`, starts
//! the executor. Both sessions of one assignment share one journal, each part marked with its
//! role.
//!
//! Everything that needs the UI goes through [`Host`], so the loops run the same against a fake
//! host and a fake HTTP server in tests.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use instant::Instant;
use parking_lot::Mutex;
use sha2::Digest as _;

use super::about;
use super::admission::{self, MachineState};
use super::client::{Api, ApiError};
use super::contract::{
    self, AckRequest, AgentInfo, AppliedRequest, Assignment, Command, HeartbeatRequest, Limits,
    PartReport, Passport, ProfileBeat, ProfileBody, ProxyCheck, REASON_OWNER_QUESTION,
    REASON_PLAN_REJECTED, REASON_PREPARE_FAILED, REASON_USAGE_LIMIT, REASON_WARD_RESTARTED,
    RegisterRequest, Resources, SessionBeat, SessionStatus, StatusRequest, Tools, UsageReport,
};
use super::git::{self, WorktreeSpec};
use super::journal::Journal;
use super::launch::{self, RunnerKind};
use super::machine::{self, Machine};
use super::plan::{self, Plan};
use super::prepare;
use super::profile::{self, Vault};
use super::provider::{self, FoundSession, Homes, LastTurn, Provider, WardUsage};
use super::resume::{self, SavedAgent, SavedJournal, SavedPhase, SavedPrepare, SavedSession};
use super::stream;

/// What the worker needs from the settings. `Debug` leaves the key out.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    pub base_url: String,
    pub key: String,
    pub name: String,
    pub root: PathBuf,
    /// Where the owner's own clones are looked for before a repository is cloned into `root`
    /// (see [`git::find_clones`]).
    pub clone_search: Vec<PathBuf>,
    /// "Take tasks" is off: assignments are refused, running sessions go on.
    pub paused: bool,
}

impl std::fmt::Debug for WorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConfig")
            .field("base_url", &self.base_url)
            .field("key", &"<redacted>")
            .field("name", &self.name)
            .field("root", &self.root)
            .field("clone_search", &self.clone_search)
            .field("paused", &self.paused)
            .finish()
    }
}

impl WorkerConfig {
    /// Identifies the address, name and key a registration belongs to, without the key itself.
    pub fn fingerprint(&self) -> String {
        let mut hasher = sha2::Sha256::new();
        hasher.update(self.base_url.as_bytes());
        hasher.update([0]);
        hasher.update(self.name.as_bytes());
        hasher.update([0]);
        hasher.update(self.key.as_bytes());
        hex::encode(&hasher.finalize()[..16])
    }
}

/// The worker token of a registration. `Debug` leaves the token out.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Registration {
    pub fingerprint: String,
    pub worker_id: String,
    pub token: String,
    /// `passport.repos` sent with this registration: another set of clones on the machine
    /// registers again, because the service reads the clones from the passport only. `None`
    /// in a registration saved by an older Ward.
    #[serde(default)]
    pub repos: Option<Vec<String>>,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("fingerprint", &self.fingerprint)
            .field("worker_id", &self.worker_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// A tab to open for a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSpec {
    pub cwd: PathBuf,
    pub title: String,
    pub command: String,
    /// Shell by name (`pwsh`, `powershell`); `None` for the default shell.
    pub shell: Option<&'static str>,
}

/// How the tab of a session is looked for, in this order: the pane the session already had,
/// the pane whose agent runs the session's own agent session, and the pane whose agent works in
/// the session's directory. The first two name one tab exactly; the directory is the last
/// resort, for an agent whose session id Ward never found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneQuery {
    /// The pane UUID of the session's tab, which an update relaunch keeps.
    pub pane_uuid: Option<String>,
    /// The agent session the tab must be running.
    pub agent_session_id: Option<String>,
    /// The working directory the tab's agent must be in.
    pub cwd: Option<PathBuf>,
}

impl PaneQuery {
    /// Nothing to look a tab up by.
    pub fn is_empty(&self) -> bool {
        self.pane_uuid.is_none() && self.agent_session_id.is_none() && self.cwd.is_none()
    }
}

/// The UI side of the worker.
#[async_trait]
pub trait Host: Send + Sync {
    /// `None` while no key is configured: the worker idles.
    async fn config(&self) -> Option<WorkerConfig>;
    async fn load_registration(&self) -> Option<Registration>;
    async fn store_registration(&self, registration: Option<Registration>);
    async fn ward_usage(&self) -> WardUsage;
    /// Opens the tab and queues its command; returns the pane UUID.
    async fn open_tab(&self, tab: TabSpec) -> Result<Option<String>, String>;
    /// The pane of an open tab that matches `query`; `None` when no tab does.
    async fn find_pane(&self, query: PaneQuery) -> Option<String>;
    /// Types `text` into the agent of the pane and submits it.
    async fn send_text(&self, pane_uuid: &str, text: &str) -> Result<(), String>;
    /// Brings the tab of the pane forward and the window with it: the owner asked for the
    /// session on his screen.
    async fn focus_pane(&self, pane_uuid: &str) -> Result<(), String>;
    /// Starts keeping the recent output of the pane, so that a stream opened later starts with
    /// what is on the screen (see [`super::stream::Tap`]). Called on every tick: a pane kept
    /// already is left as it is.
    async fn tap_pane(&self, pane_uuid: &str);
    /// Streams the pane to `url` (a WebSocket of the service) with the worker `token` until the
    /// service or the tab closes it. A pane that is streaming already is not streamed twice.
    async fn stream_pane(&self, pane_uuid: &str, url: &str, token: &str) -> Result<(), String>;
    async fn publish(&self, snapshot: Snapshot);
}

/// Intervals of the loops; shorter in tests.
#[derive(Clone, Debug)]
pub struct Timing {
    pub tick: Duration,
    pub journal_every: Duration,
    pub config_every: Duration,
    pub inbox_wait_secs: u64,
    /// Without the agent session found by then, `running` is sent without its ids.
    pub session_lookup_grace: Duration,
    /// Neither an agent session nor a line of transcript by then: the agent never started and
    /// the session is reported as failed instead of staying `running` for good.
    pub session_start_grace: Duration,
    /// The runner is gone but wrote no exit code: the session ends after this.
    pub exit_grace: Duration,
    /// How long the orchestrator may take to end by itself after its plan was taken; see
    /// [`PREPARE_EXIT_WAIT`].
    pub prepare_exit_wait: Duration,
    /// How long the transcript of a launched agent may stand still before its last turn
    /// decides what became of the session; see [`IDLE_AFTER_ANSWER`].
    pub idle_after_answer: Duration,
    /// After a stop, the session ends after this even without an exit code.
    pub stop_grace: Duration,
    pub usage_every: Duration,
    /// How often the profile loop looks at the version the service reports.
    pub profile_every: Duration,
    /// How long a proxy check is taken as fresh.
    pub proxy_every: Duration,
    pub min_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(2),
            journal_every: Duration::from_secs(10),
            config_every: Duration::from_secs(5),
            inbox_wait_secs: 30,
            session_lookup_grace: Duration::from_secs(120),
            session_start_grace: Duration::from_secs(180),
            exit_grace: Duration::from_secs(15),
            prepare_exit_wait: PREPARE_EXIT_WAIT,
            idle_after_answer: IDLE_AFTER_ANSWER,
            stop_grace: Duration::from_secs(30),
            usage_every: Duration::from_secs(60),
            profile_every: Duration::from_secs(5),
            proxy_every: Duration::from_secs(5 * 60),
            min_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(5 * 60),
        }
    }
}

/// Doubling delay between failed attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    current: Option<Duration>,
}

impl Backoff {
    pub fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            current: None,
        }
    }

    /// The delay after one more failure.
    pub fn fail(&mut self) -> Duration {
        let next = match self.current {
            None => self.min,
            Some(current) => (current * 2).min(self.max),
        };
        self.current = Some(next);
        next
    }

    pub fn succeed(&mut self) {
        self.current = None;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Connection {
    /// No key configured.
    #[default]
    Disabled,
    Connecting,
    Online,
    /// The key was refused; nothing is retried until the settings change.
    KeyRejected(String),
    /// The service is unreachable or answered with an error; retried with backoff.
    Offline(String),
}

/// A session as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionView {
    pub session_id: String,
    pub title: String,
    pub task_number: Option<u64>,
    pub provider: String,
    pub status: SessionStatus,
    pub branch: String,
    pub worktree: Option<String>,
    pub remote_control_url: Option<String>,
    pub message: Option<String>,
    pub pane_uuid: Option<String>,
    /// The plan of the preparation phase, once the orchestrator wrote it.
    pub plan: Option<String>,
}

/// The machine profile as Ward applied it: for the panel, the heartbeat and the admission of
/// assignments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileStatus {
    /// The version Ward applied, not the one the service has now.
    pub version: u32,
    pub ok: bool,
    pub message: String,
    pub parts: Vec<PartReport>,
    /// The last check by `dnsProxy.checkUrl`; `None` while nothing was checked.
    pub proxy: Option<ProxyCheck>,
    /// The profile asks for a working proxy.
    pub proxy_required: bool,
    pub at: Option<DateTime<Utc>>,
}

/// What the panel shows about the worker.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub connection: Connection,
    pub worker_name: Option<String>,
    pub worker_id: Option<String>,
    pub limits: Limits,
    pub server_paused: bool,
    pub local_paused: bool,
    pub resources: Option<Resources>,
    pub agents: BTreeMap<String, AgentInfo>,
    pub usage: Vec<UsageReport>,
    pub sessions: Vec<SessionView>,
    pub last_heartbeat: Option<DateTime<Utc>>,
    /// The machine profile Ward applied; `None` while it applied none.
    pub profile: Option<ProfileStatus>,
    /// The version the service has now; `0` from a service that knows no profile.
    pub profile_version: u32,
    /// The vault of the profile: without it no assignment without a prompt is taken.
    pub vault: Option<String>,
}

/// Messages in session statuses; they end up in the card's comments.
const MESSAGE_RUNNING: &str = "Сессия запущена";
const MESSAGE_STOPPED: &str = "Остановлено владельцем";
const MESSAGE_TOO_LONG: &str = "Остановлено: превышено maxDurationMin";
const MESSAGE_NO_EXIT_CODE: &str = "Агент завершился без кода выхода (вкладку закрыли?)";
const MESSAGE_NO_SESSION_ID: &str = "Сессия запущена, id сессии агента пока не найден";
const MESSAGE_NOT_STARTED: &str =
    "Агент не начал работу: ждёт ответа в терминале (вопрос о доверии к папке?)";
const MESSAGE_USAGE_LIMIT: &str = "Агент упёрся в лимит подписки";
const MESSAGE_IDLE_ANSWER: &str = "Работа готова: агент выдал финальный ответ и ждёт вас. Сессия открыта: можно написать в неё или остановить";
const MESSAGE_OWNER_QUESTION: &str = "Агент задал вопрос и ждёт ответа владельца";
const MESSAGE_AGENT_ERROR: &str = "Агент закончил ход ошибкой API вместо ответа";
const MESSAGE_PREPARING: &str = "Подготовка: оркестратор собирает досье и промпт";
const MESSAGE_PREPARE_TOO_LONG: &str = "Подготовка не уложилась в prepareMaxDurationMin";
const MESSAGE_PREPARE_NO_PLAN: &str = "Оркестратор завершился, а plan.json не оставил";
const MESSAGE_PREPARE_CRASHED: &str = "Оркестратор упал, не оставив plan.json";
const MESSAGE_PLAN_REJECTED: &str = "План отклонён владельцем";
const MESSAGE_SHUTDOWN: &str =
    "Ward закрывается (обновление или выход), сессия продолжится после запуска";
const MESSAGE_RESUMED: &str = "Ward перезапустился, сессия подхвачена: процесс агента жив";
const MESSAGE_RESTORED: &str = "Сессия восстановлена владельцем: агент продолжает с того же места";
const MESSAGE_RESTORE_NO_WORKTREE: &str =
    "Восстановить не из чего: рабочей копии сессии на машине уже нет";
const MESSAGE_RESUMED_PREPARING: &str =
    "Ward перезапустился, подготовка подхвачена: оркестратор жив";
const MESSAGE_RESUMED_PLAN_REVIEW: &str = "Ward перезапустился, план по-прежнему ждёт согласования";
const MESSAGE_RESUMED_WORKTREE: &str =
    "Ward перезапустился во время развёртывания репозитория, worktree разворачивается заново";
const MESSAGE_WAITING_AFTER_RESTART: &str = "Ward перезапустился, ждём, вернётся ли процесс агента";
const MESSAGE_AGENT_GONE: &str =
    "Ward перезапустился, процесс агента не выжил: работа сессии прервана на середине";
const MESSAGE_PREPARE_AGENT_GONE: &str =
    "Ward перезапустился, процесс оркестратора не выжил, плана он не оставил";
const MESSAGE_NO_TAB: &str = "Сообщение не доставлено: открытой вкладки сессии не нашлось";

/// How much of the error of the API the message of a failed session keeps.
const MAX_AGENT_ERROR_CHARS: usize = 500;

/// Finished sessions the panel keeps.
const MAX_FINISHED_SESSIONS: usize = 20;

/// How long the orchestrator gets to end by itself once its plan is taken. The plan is its last
/// action, so all that is left is its closing answer: seconds, not minutes. Ward ends the
/// process when the wait is over, because the executor of the same assignment waits for it; a
/// longer wait would only delay the executor for nothing.
const PREPARE_EXIT_WAIT: Duration = Duration::from_secs(20);

/// How long the transcript of a launched agent may stand still before Ward reads its last turn
/// and decides. An agent in remote control mode does not end its process after its answer: the
/// transcript standing still is the only sign Ward gets, so the wait has to outlast every quiet
/// stretch inside a turn — a long build, a test run, a wait for the model. Those stretches are
/// opened by a tool call the transcript already holds, so the last turn tells them from a
/// finished one; the wait is only there for the pauses between two lines of one turn. Five
/// minutes is far past any of them and far short of `maxDurationMin`, which used to be the only
/// way out of a session whose agent had long finished.
const IDLE_AFTER_ANSWER: Duration = Duration::from_secs(5 * 60);

/// Roles of the two sessions of one assignment, as the journal marks them.
const ROLE_ORCHESTRATOR: &str = "orchestrator";
const ROLE_EXECUTOR: &str = "executor";

/// Result of preparing the worktree, filled by the git thread.
type WorktreeSlot = Arc<Mutex<Option<Result<git::PreparedWorktree, String>>>>;

enum Phase {
    /// The orchestrator session of the preparation phase.
    Prepare {
        launched_at_ms: i64,
        at: Instant,
    },
    /// The plan is taken and the orchestrator ends by itself. The rule stays "two sessions one
    /// after the other, not at once": the executor starts only once the orchestrator is gone,
    /// or once Ward ended it because it did not go by itself.
    PrepareExit {
        /// The repository root of the settings, as it was when the plan was taken.
        root: PathBuf,
        repo: launch::RepoId,
        /// When the plan was taken.
        since: Instant,
    },
    /// The plan went to the service and waits for the owner's `approve`.
    PlanReview,
    /// The git thread clones the repository and makes the worktree.
    Worktree(WorktreeSlot),
    Launched {
        launched_at_ms: i64,
        at: Instant,
    },
    Done,
}

/// The preparation phase of a session: where the orchestrator works and what it must leave.
struct PrepareRun {
    /// `<root>/.agents/<first 8 characters of sessionId>`: the prompt, the runner and the plan.
    dir: PathBuf,
    plan_path: PathBuf,
    /// The vault the orchestrator session opens in.
    vault_dir: PathBuf,
    limit: Duration,
    /// Why the last `plan.json` could not be read, for the message of a failed preparation.
    last_error: Option<String>,
    /// The time the orchestrator stood waiting for the owner's answer: the limit measures work.
    waited: Duration,
    /// Since when it waits now, while its question is open.
    asked_at: Option<Instant>,
}

struct Session {
    assignment: Assignment,
    provider: &'static dyn Provider,
    phase: Phase,
    status: SessionStatus,
    worktree: Option<PathBuf>,
    /// Which base and which clone the worktree came from, for the `running` status.
    worktree_note: Option<String>,
    found: Option<FoundSession>,
    transcript: Option<PathBuf>,
    pane_uuid: Option<String>,
    runner_pid: Option<u32>,
    journal: Journal,
    last_journal_read: Option<Instant>,
    /// When the transcript of the agent running now last grew; the idle watch counts from here.
    transcript_grew_at: Option<Instant>,
    outbox: VecDeque<StatusRequest>,
    messages: VecDeque<String>,
    stop: Option<(&'static str, Option<Instant>)>,
    runner_gone_since: Option<Instant>,
    started: Instant,
    /// Unix ms of [`Session::started`], so a restart of Ward keeps counting `maxDurationMin`
    /// from when the session was taken.
    started_at_ms: i64,
    message: Option<String>,
    /// `reason` of the last status, for the state on disk.
    reason: Option<String>,
    /// The state already on disk, so an unchanged session is not written again; `None` while
    /// this run of Ward wrote no state file for the session, and a session that is over then
    /// has nothing to remove.
    saved_state: Option<String>,
    /// The session was picked up after a restart of Ward and its agent has not turned up yet:
    /// the watch of the session waits for it instead of watching a process that is gone.
    restarted: bool,
    /// The agent came back after a restart without the runner of its launch: `runner.pid`
    /// names a process that went with the old tab and is not read again.
    runner_left: bool,
    /// The preparation phase, when the assignment needed one.
    prepare: Option<PrepareRun>,
    /// The plan of the preparation phase, once it was read.
    plan: Option<Plan>,
    /// The plan still to send to the service.
    plan_outbox: Option<Plan>,
    /// The owner's answer in `plan-review`: `Ok` for `approve`, the reason for `reject`.
    approval: Option<Result<(), String>>,
    /// The owner asked for the session on his screen and it is not there yet.
    show: Option<ShowRequest>,
    /// The owner at another machine opened the session's window: the tab goes to the service.
    stream: bool,
    /// Whose transcript the journal is reading now.
    role: Option<&'static str>,
}

/// The owner pressed «Открыть в Варде» on the card.
#[derive(Clone, Copy, Debug)]
struct ShowRequest {
    /// He is at another machine: nothing comes forward here, the session only has to run.
    remote: bool,
}

impl Session {
    fn new(assignment: Assignment, provider: &'static dyn Provider, phase: Phase) -> Self {
        Self {
            assignment,
            provider,
            phase,
            status: SessionStatus::Assigned,
            worktree: None,
            worktree_note: None,
            found: None,
            transcript: None,
            pane_uuid: None,
            runner_pid: None,
            journal: Journal::default(),
            last_journal_read: None,
            transcript_grew_at: None,
            outbox: VecDeque::new(),
            messages: VecDeque::new(),
            stop: None,
            runner_gone_since: None,
            started: Instant::now(),
            started_at_ms: now_ms(),
            message: None,
            reason: None,
            saved_state: None,
            restarted: false,
            runner_left: false,
            prepare: None,
            plan: None,
            plan_outbox: None,
            approval: None,
            show: None,
            stream: false,
            role: None,
        }
    }

    /// Forgets the agent that just ran, so the next one of the same session is looked for from
    /// scratch.
    fn reset_agent(&mut self) {
        self.found = None;
        self.transcript = None;
        self.pane_uuid = None;
        self.runner_pid = None;
        self.runner_left = false;
        self.runner_gone_since = None;
        self.last_journal_read = None;
        self.transcript_grew_at = None;
    }

    /// Where the agent of this session works now: the vault for the orchestrator of the
    /// preparation phase, the worktree for the executor.
    fn agent_cwd(&self) -> Option<PathBuf> {
        match &self.phase {
            Phase::Prepare { .. } | Phase::PrepareExit { .. } => self
                .prepare
                .as_ref()
                .map(|prepare| prepare.vault_dir.clone()),
            _ => self.worktree.clone(),
        }
    }

    /// The directory the agent of this session opened in, whatever phase the session is in
    /// now: unlike [`Self::agent_cwd`] a session that is over keeps the answer, so its tab can
    /// be opened again.
    fn agent_dir(&self) -> Option<PathBuf> {
        self.agent_cwd().or_else(|| {
            self.prepare
                .as_ref()
                .map(|prepare| prepare.vault_dir.clone())
        })
    }

    /// Where Ward keeps the files of the run: `.agents` of the worktree for the executor, the
    /// session's own directory for the orchestrator of the preparation phase.
    fn agent_files(&self) -> Option<PathBuf> {
        let prepare = || {
            self.prepare
                .as_ref()
                .map(|prepare: &PrepareRun| prepare.dir.clone())
        };
        match &self.phase {
            Phase::Prepare { .. } | Phase::PrepareExit { .. } => prepare(),
            _ => match &self.worktree {
                Some(worktree) => Some(launch::agents_dir(worktree)),
                None => prepare(),
            },
        }
    }

    /// What the session's tab can be recognised by.
    fn pane_query(&self) -> PaneQuery {
        PaneQuery {
            pane_uuid: self.pane_uuid.clone(),
            agent_session_id: self
                .found
                .as_ref()
                .map(|found| found.agent_session_id.clone()),
            cwd: self.agent_cwd(),
        }
    }

    /// The processes of the agent that runs now.
    fn agent_pids(&self) -> Vec<u32> {
        match self.found.as_ref().and_then(|found| found.pid) {
            // The agent only: the runner then still writes the exit code.
            Some(pid) => vec![pid],
            None => self.runner_pid.into_iter().collect(),
        }
    }

    fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// Done and everything about it delivered.
    fn is_settled(&self) -> bool {
        self.is_done()
            && self.outbox.is_empty()
            && self.plan_outbox.is_none()
            && !self.journal.has_pending()
    }

    /// `text` with the base and the clone of the worktree, when there is something to say.
    fn with_worktree_note(&self, text: &str) -> String {
        match &self.worktree_note {
            Some(note) => format!("{text} ({note})"),
            None => text.to_owned(),
        }
    }

    fn status_request(&self, status: SessionStatus) -> StatusRequest {
        let mut request = StatusRequest::new(status);
        request.provider = Some(self.provider.id().to_owned());
        request.branch = Some(self.assignment.branch.clone()).filter(|branch| !branch.is_empty());
        request.worktree = self
            .worktree
            .as_ref()
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        if let Some(found) = &self.found {
            request.agent_session_id = Some(found.agent_session_id.clone());
            if self.provider.id() == provider::claude::ID {
                request.claude_session_id = Some(found.agent_session_id.clone());
            }
            request.remote_control_url = found.remote_control_url.clone();
        }
        // What the session does now, as its executor wrote it: the Agents screen shows it, and
        // it changes as the owner gives the session more tasks in the chat.
        if let Some(now) = self
            .worktree
            .as_deref()
            .and_then(|worktree| about::read(&launch::agents_dir(worktree)))
        {
            request.summary = now.summary;
            request.tags = now.tags;
        }
        request
    }

    /// Records a new status and queues it for the service.
    fn set_status(&mut self, request: StatusRequest) {
        self.status = request.status;
        self.message = request.message.clone();
        self.reason = request.reason.clone();
        self.outbox.push_back(request);
    }

    /// What the next start of Ward needs to pick this session up again.
    fn saved(&self) -> SavedSession {
        let (offset, next_seq) = self.journal.sent_position();
        let text = |path: &Path| path.to_string_lossy().into_owned();
        SavedSession {
            version: resume::FORMAT_VERSION,
            session_id: self.assignment.session_id.clone(),
            provider: self.provider.id().to_owned(),
            phase: match &self.phase {
                // The plan is read from `plan.json` again, so the wait for the orchestrator's
                // exit is the preparation phase once more.
                Phase::Prepare { .. } | Phase::PrepareExit { .. } => SavedPhase::Prepare,
                Phase::PlanReview => SavedPhase::PlanReview,
                Phase::Worktree(_) => SavedPhase::Worktree,
                Phase::Launched { .. } | Phase::Done => SavedPhase::Launched,
            },
            status: self.status,
            reason: self.reason.clone(),
            started_at_ms: self.started_at_ms,
            launched_at_ms: match &self.phase {
                Phase::Prepare { launched_at_ms, .. } | Phase::Launched { launched_at_ms, .. } => {
                    *launched_at_ms
                }
                _ => self.started_at_ms,
            },
            assignment: self.assignment.clone(),
            worktree: self.worktree.as_deref().map(text),
            worktree_note: self.worktree_note.clone(),
            runner_pid: self.runner_pid,
            agent: self.found.as_ref().map(|found| SavedAgent {
                agent_session_id: found.agent_session_id.clone(),
                pid: found.pid,
                remote_control_url: found.remote_control_url.clone(),
            }),
            transcript: self.transcript.as_deref().map(text),
            pane_uuid: self.pane_uuid.clone(),
            role: self.role.map(str::to_owned),
            journal: SavedJournal { offset, next_seq },
            prepare: self.prepare.as_ref().map(|prepare| SavedPrepare {
                dir: text(&prepare.dir),
                plan_path: text(&prepare.plan_path),
                vault_dir: text(&prepare.vault_dir),
                limit_secs: prepare.limit.as_secs(),
            }),
        }
    }

    fn view(&self) -> SessionView {
        SessionView {
            session_id: self.assignment.session_id.clone(),
            title: self.assignment.title.clone(),
            task_number: self
                .assignment
                .task_ref
                .task_number
                .or(Some(self.assignment.task_ref.task_id).filter(|id| *id > 0)),
            provider: self.provider.id().to_owned(),
            status: self.status,
            branch: self.assignment.branch.clone(),
            worktree: self
                .worktree
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            remote_control_url: self
                .found
                .as_ref()
                .and_then(|found| found.remote_control_url.clone()),
            message: self.message.clone(),
            pane_uuid: self.pane_uuid.clone(),
            plan: self.plan.as_ref().map(plan::summary),
        }
    }
}

/// Whether an assignment of a session Ward already knows is only delivered again: then it is
/// confirmed and nothing starts. The exception is a session that ended here and comes back with
/// `resume` — the owner runs it again (from the archive or after its tab was closed), so it
/// starts anew. Taking it out of the settled ones makes a repeated delivery of that new run a
/// duplicate again.
fn repeated_delivery(state: &mut State, session_id: &str, resume: bool) -> bool {
    if !state.known_ids.contains(session_id) {
        return false;
    }
    !(resume && state.settled_ids.remove(session_id))
}

/// The bookkeeping of runs at the end of a tick: a settled run of a session that runs again
/// leaves the panel, and the ids of the settled sessions left are returned.
fn settle_runs(sessions: &mut Vec<Session>) -> HashSet<String> {
    let running: HashSet<String> = sessions
        .iter()
        .filter(|session| !session.is_settled())
        .map(|session| session.assignment.session_id.clone())
        .collect();
    sessions.retain(|session| {
        !(session.is_settled() && running.contains(&session.assignment.session_id))
    });
    sessions
        .iter()
        .filter(|session| session.is_settled())
        .map(|session| session.assignment.session_id.clone())
        .collect()
}

#[derive(Default)]
struct State {
    config: Option<WorkerConfig>,
    registration: Option<Registration>,
    connection: Connection,
    limits: Limits,
    heartbeat_secs: u64,
    server_paused: bool,
    /// Sessions waiting for the sessions loop; it takes them out while it works on them.
    sessions: Vec<Session>,
    known_ids: HashSet<String>,
    /// Sessions that ended here and whose every report reached the service, as of the last
    /// tick: the owner may run one of them again (see [`repeated_delivery`]).
    settled_ids: HashSet<String>,
    commands: Vec<Command>,
    beats: Vec<SessionBeat>,
    views: Vec<SessionView>,
    resources: Option<Resources>,
    agents: BTreeMap<String, AgentInfo>,
    usage: Vec<UsageReport>,
    usage_at: Option<Instant>,
    last_heartbeat: Option<DateTime<Utc>>,
    rejected_fingerprint: Option<String>,
    /// The profile version the service has now; `0`: it knows no profile.
    profile_version: u32,
    /// The profile that came with the registration, so applying needs no extra request.
    profile_body: Option<ProfileBody>,
    /// What came of applying the profile.
    profile: Option<ProfileStatus>,
    /// `dnsProxy.checkUrl` of the profile Ward applied.
    proxy_url: Option<String>,
    profile_checked_at: Option<Instant>,
    /// The vault of the profile, for the preparation phase.
    vault: Option<Vault>,
    /// Ward's own entries were already taken off the machine after the key went away.
    managed_removed: bool,
}

/// What this machine does with an assignment it took.
struct Decision {
    provider: &'static dyn Provider,
    /// The repository root of the settings.
    root: PathBuf,
    /// `None` while the assignment names no repository: the orchestrator picks one.
    repo: Option<launch::RepoId>,
    /// The vault of the machine profile, when there is a usable one.
    vault: Option<Vault>,
}

/// Which shell runs the runner script of a tab.
fn shell_of(kind: RunnerKind) -> Option<&'static str> {
    match kind {
        RunnerKind::Pwsh if cfg!(windows) => Some("pwsh"),
        RunnerKind::Sh if cfg!(windows) => Some("powershell"),
        RunnerKind::Pwsh | RunnerKind::Sh => None,
    }
}

/// Whether a launch never got going: `grace` passed and the agent left neither a session of
/// its own nor a line of transcript. An agent that asks something on the screen of its tab
/// looks exactly like this, and nobody is there to answer it.
fn is_silent(session: &Session, elapsed: Duration, grace: Duration) -> bool {
    session.found.is_none() && !session.journal.has_lines() && elapsed >= grace
}

/// The line that marks whose transcript follows in the journal of a session.
fn role_marker(role: &str, session_id: &str) -> String {
    serde_json::json!({
        "type": "ward.role",
        "role": role,
        "sessionId": session_id,
        "at": provider::rfc3339(Utc::now()),
    })
    .to_string()
}

/// The line that marks in the journal that Ward restarted and took the session over.
fn restart_marker(role: &str, session_id: &str, agent_session_id: Option<&str>) -> String {
    serde_json::json!({
        "type": "ward.restart",
        "role": role,
        "sessionId": session_id,
        "agentSessionId": agent_session_id,
        "at": provider::rfc3339(Utc::now()),
    })
    .to_string()
}

/// The moment `elapsed` ago, for a timer that was already running before Ward restarted; the
/// present when the clock cannot go that far back.
fn instant_ago(elapsed: Duration) -> Instant {
    let now = Instant::now();
    now.checked_sub(elapsed).unwrap_or(now)
}

/// How long ago `at_ms` (Unix ms) was; zero for a moment in the future, a clock that moved or a
/// state file of a session that has no such moment.
fn elapsed_since_ms(at_ms: i64) -> Duration {
    let elapsed = now_ms().saturating_sub(at_ms);
    if at_ms <= 0 || elapsed <= 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(elapsed as u64)
}

/// The last statuses to send when Ward quits.
pub struct ShutdownReport {
    pub base_url: String,
    pub token: String,
    /// `(sessionId, status)`.
    pub sessions: Vec<(String, StatusRequest)>,
}

pub struct Worker {
    host: Arc<dyn Host>,
    machine: Arc<dyn Machine>,
    client: http_client::Client,
    homes: Homes,
    /// The files the machine profile is applied to; temp directories in tests.
    profile_target: profile::Target,
    timing: Timing,
    ward_version: String,
    state: Mutex<State>,
    register_lock: futures::lock::Mutex<()>,
    /// Asks the control loop for a heartbeat before its interval is over; see
    /// [`Worker::heartbeat_now`]. One request waiting is enough: the heartbeat it brings reads
    /// the settings as they are by then.
    beat_requests: async_channel::Sender<()>,
    beat_requested: async_channel::Receiver<()>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

async fn sleep(duration: Duration) {
    warpui::r#async::Timer::after(duration).await;
}

impl Worker {
    pub fn new(
        host: Arc<dyn Host>,
        machine: Arc<dyn Machine>,
        client: http_client::Client,
        homes: Homes,
        profile_target: profile::Target,
        timing: Timing,
        ward_version: String,
    ) -> Arc<Self> {
        let (beat_requests, beat_requested) = async_channel::bounded(1);
        Arc::new(Self {
            host,
            machine,
            client,
            homes,
            profile_target,
            timing,
            ward_version,
            state: Mutex::new(State {
                heartbeat_secs: super::contract::DEFAULT_HEARTBEAT_SECS,
                ..Default::default()
            }),
            register_lock: futures::lock::Mutex::new(()),
            beat_requests,
            beat_requested,
        })
    }

    /// Sends a heartbeat now instead of at the end of the interval: "Take tasks" was turned off
    /// or on. The service schedules by `localPaused` of the heartbeat, so turned off, the
    /// machine is not offered tasks it would only refuse, and turned on, it gets the waiting
    /// ones at once. The settings are read again before that heartbeat, which also lets
    /// admission see the change right away.
    pub fn heartbeat_now(&self) {
        // Full: a heartbeat is already asked for, and it reads the settings when it goes out.
        let _ = self.beat_requests.try_send(());
    }

    /// Waits `delay` before the next heartbeat, or less when [`Worker::heartbeat_now`] asks
    /// for one.
    async fn wait_for_beat(&self, delay: Duration) {
        let timer = std::pin::pin!(sleep(delay));
        let requested = std::pin::pin!(self.beat_requested.recv());
        futures::future::select(timer, requested).await;
    }

    /// Picks up the sessions of the run before and then runs the four loops; never returns.
    /// Restoring comes first on purpose: the inbox must not take an assignment of a session
    /// this Ward is about to own again.
    pub async fn run(self: Arc<Self>) {
        self.restore_sessions().await;
        futures::join!(
            self.clone().control_loop(),
            self.clone().inbox_loop(),
            self.clone().sessions_loop(),
            self.clone().profile_loop()
        );
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock();
        Snapshot {
            connection: state.connection.clone(),
            worker_name: state.config.as_ref().map(|config| config.name.clone()),
            worker_id: state
                .registration
                .as_ref()
                .map(|registration| registration.worker_id.clone()),
            limits: state.limits.clone(),
            server_paused: state.server_paused,
            local_paused: state.config.as_ref().is_some_and(|config| config.paused),
            resources: state.resources.clone(),
            agents: state.agents.clone(),
            usage: state.usage.clone(),
            sessions: state.views.clone(),
            last_heartbeat: state.last_heartbeat,
            profile: state.profile.clone(),
            profile_version: state.profile_version,
            vault: state
                .vault
                .as_ref()
                .filter(|vault| !vault.path.trim().is_empty())
                .map(Vault::root),
        }
    }

    async fn publish(&self) {
        let snapshot = self.snapshot();
        self.host.publish(snapshot).await;
    }

    /// The service address and worker token, when registered.
    fn credentials(&self) -> Option<(String, String)> {
        let state = self.state.lock();
        let config = state.config.as_ref()?;
        let registration = state.registration.as_ref()?;
        Some((config.base_url.clone(), registration.token.clone()))
    }

    /// Saves the state of every session still going and says what to tell the service, for the
    /// moment Ward quits — an update relaunch above all. The sessions are not reported as
    /// failed any more: the next start picks them up, so what goes out is the status they are
    /// in with a message that says Ward is closing. A session whose agent did not survive is
    /// closed by that next start, which can tell, and not here, which cannot.
    pub fn shutdown_report(&self) -> Option<ShutdownReport> {
        let root = {
            let state = self.state.lock();
            state.config.as_ref().map(|config| config.root.clone())
        };
        if let Some(root) = root {
            let mut state = self.state.lock();
            for session in state
                .sessions
                .iter_mut()
                .filter(|session| !session.is_done())
            {
                save_session_state(session, &root);
            }
        }
        let (base_url, token) = self.credentials()?;
        let state = self.state.lock();
        let reports: Vec<(String, StatusRequest)> = state
            .sessions
            .iter()
            .filter(|session| !session.is_done() && is_reportable(session.status))
            .map(|session| {
                let mut request = session.status_request(session.status);
                request.reason = session.reason.clone();
                request.message = Some(MESSAGE_SHUTDOWN.to_owned());
                (session.assignment.session_id.clone(), request)
            })
            .collect();
        (!reports.is_empty()).then_some(ShutdownReport {
            base_url,
            token,
            sessions: reports,
        })
    }

    /// Forgets `token` after a 401, unless a new registration already replaced it.
    async fn invalidate(&self, token: &str) {
        let forget = {
            let mut state = self.state.lock();
            let forget = state
                .registration
                .as_ref()
                .is_some_and(|registration| registration.token == token);
            if forget {
                state.registration = None;
                state.connection = Connection::Connecting;
            }
            forget
        };
        if forget {
            log::info!("Ward agents: the worker token was rejected; registering again");
            self.host.store_registration(None).await;
        }
    }

    async fn refresh_config(&self) -> Option<WorkerConfig> {
        let config = self.host.config().await;
        let changed = {
            let mut state = self.state.lock();
            let old = state.config.as_ref().map(WorkerConfig::fingerprint);
            let new = config.as_ref().map(WorkerConfig::fingerprint);
            state.config = config.clone();
            if old != new {
                state.registration = None;
                state.rejected_fingerprint = None;
                state.connection = if config.is_some() {
                    Connection::Connecting
                } else {
                    Connection::Disabled
                };
            }
            old != new
        };
        if changed {
            self.publish().await;
        }
        config
    }

    /// `owner/repo` of the clones in the root and of the owner's own clones.
    fn repos(config: &WorkerConfig) -> Vec<String> {
        let mut repos = machine::scan_repos(&config.root);
        repos.extend(git::local_repos(&config.clone_search));
        repos.sort_by_key(|repo| repo.to_ascii_lowercase());
        repos.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        repos
    }

    fn passport(&self, config: &WorkerConfig, repos: Vec<String>) -> Passport {
        let root = &config.root;
        let hardware = self.machine.hardware(root);
        let tools = Tools {
            git: self.machine.has_tool("git"),
            claude: self.machine.has_tool("claude"),
            codex: self.machine.has_tool("codex"),
            node: self.machine.has_tool("node"),
            java: self.machine.has_tool("java"),
            cargo: self.machine.has_tool("cargo"),
            dotnet: self.machine.has_tool("dotnet"),
        };
        let agents = provider::all()
            .into_iter()
            .map(|provider| {
                let installed = self.machine.has_tool(provider.binary());
                let account = provider.account(&self.homes);
                let info = AgentInfo {
                    installed,
                    version: installed
                        .then(|| self.machine.tool_version(provider.binary()))
                        .flatten(),
                    logged_in: account.logged_in,
                    plan: account.plan,
                };
                (provider.id().to_owned(), info)
            })
            .collect();
        Passport {
            cpu_cores: hardware.cpu_cores,
            ram_total_bytes: hardware.ram_total_bytes,
            disk_total_bytes: hardware.disk_total_bytes,
            tools,
            agents,
            repos,
        }
    }

    /// Registers unless a registration for the current settings exists (in memory or saved).
    async fn ensure_registered(&self, config: &WorkerConfig) -> Result<(), ApiError> {
        let _guard = self.register_lock.lock().await;
        let fingerprint = config.fingerprint();
        if self.state.lock().registration.is_some() {
            return Ok(());
        }
        let repos = Self::repos(config);
        // The installed agents are read on every start, saved registration or not: admission
        // checks them, and an empty list refuses every assignment as "not installed".
        let passport = self.passport(config, repos.clone());
        self.state.lock().agents = passport.agents.clone();
        if let Some(saved) = self.host.load_registration().await
            && saved.fingerprint == fingerprint
            && saved.repos.as_ref() == Some(&repos)
        {
            let mut state = self.state.lock();
            state.registration = Some(saved);
            return Ok(());
        }
        let request = RegisterRequest {
            key: config.key.clone(),
            name: config.name.clone(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            ward_version: self.ward_version.clone(),
            passport,
        };
        let api = Api {
            client: &self.client,
            base_url: &config.base_url,
        };
        let response = api.register(&request).await?;
        let registration = Registration {
            fingerprint,
            worker_id: response.worker_id.clone(),
            token: response.token.clone(),
            repos: Some(request.passport.repos.clone()),
        };
        {
            let mut state = self.state.lock();
            state.registration = Some(registration.clone());
            state.limits = response.limits.clone();
            state.heartbeat_secs = response.heartbeat_sec.max(1);
            state.profile_version = response.profile_version;
            state.profile_body = response.profile.clone();
            state.managed_removed = false;
        }
        log::info!("Ward agents: registered as worker {}", response.worker_id);
        self.host.store_registration(Some(registration)).await;
        Ok(())
    }

    /// Subscription usage, recomputed every [`Timing::usage_every`].
    async fn usage(&self) -> Vec<UsageReport> {
        {
            let state = self.state.lock();
            if state
                .usage_at
                .is_some_and(|at| at.elapsed() < self.timing.usage_every)
            {
                return state.usage.clone();
            }
        }
        let ward = self.host.ward_usage().await;
        let now = Utc::now();
        let usage: Vec<UsageReport> = provider::all()
            .into_iter()
            .filter(|provider| {
                self.state
                    .lock()
                    .agents
                    .get(provider.id())
                    .is_none_or(|info| info.installed)
            })
            .flat_map(|provider| provider.usage(&self.homes, &ward, now))
            .collect();
        let mut state = self.state.lock();
        state.usage = usage.clone();
        state.usage_at = Some(Instant::now());
        usage
    }

    async fn heartbeat(&self, config: &WorkerConfig) -> Result<(), ApiError> {
        let Some((base_url, token)) = self.credentials() else {
            return Ok(());
        };
        let resources = self.machine.resources(&config.root);
        let usage = self.usage().await;
        let (sessions, profile) = {
            let state = self.state.lock();
            let profile = state.profile.as_ref().map(|status| ProfileBeat {
                version: status.version,
                ok: status.ok,
                proxy_check: status.proxy.clone().unwrap_or_default(),
            });
            (state.beats.clone(), profile)
        };
        let request = HeartbeatRequest {
            resources: resources.clone(),
            usage,
            sessions,
            local_paused: config.paused,
            profile,
        };
        let api = Api {
            client: &self.client,
            base_url: &base_url,
        };
        match api.heartbeat(&token, &request).await {
            Ok(response) => {
                let mut state = self.state.lock();
                if let Some(limits) = response.limits {
                    state.limits = limits;
                }
                state.server_paused = response.paused;
                state.commands.extend(response.commands);
                state.resources = Some(resources);
                state.last_heartbeat = Some(Utc::now());
                state.connection = Connection::Online;
                if let Some(version) = response.profile_version {
                    state.profile_version = version;
                }
                Ok(())
            }
            Err(ApiError::Unauthorized) => {
                self.invalidate(&token).await;
                Err(ApiError::Unauthorized)
            }
            Err(err) => {
                self.state.lock().resources = Some(resources);
                Err(err)
            }
        }
    }

    async fn control_loop(self: Arc<Self>) {
        let mut backoff = Backoff::new(self.timing.min_backoff, self.timing.max_backoff);
        loop {
            let Some(config) = self.refresh_config().await else {
                sleep(self.timing.config_every).await;
                continue;
            };
            let fingerprint = config.fingerprint();
            if self.state.lock().rejected_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                sleep(self.timing.config_every).await;
                continue;
            }
            if let Err(err) = self.ensure_registered(&config).await {
                match err {
                    ApiError::Unauthorized | ApiError::Forbidden => {
                        log::warn!("Ward agents: the connection key was rejected");
                        let mut state = self.state.lock();
                        state.rejected_fingerprint = Some(fingerprint);
                        state.connection = Connection::KeyRejected(err.message());
                    }
                    err => {
                        log::info!("Ward agents: registration failed: {}", err.message());
                        self.state.lock().connection = Connection::Offline(err.message());
                        self.publish().await;
                        sleep(backoff.fail().min(self.timing.max_backoff)).await;
                        continue;
                    }
                }
                self.publish().await;
                continue;
            }
            let delay = match self.heartbeat(&config).await {
                Ok(()) => {
                    backoff.succeed();
                    Duration::from_secs(self.state.lock().heartbeat_secs)
                }
                // Register again right away.
                Err(ApiError::Unauthorized) => Duration::ZERO,
                Err(err) => {
                    log::info!("Ward agents: heartbeat failed: {}", err.message());
                    self.state.lock().connection = Connection::Offline(err.message());
                    backoff.fail().min(Duration::from_secs(
                        self.state.lock().heartbeat_secs.max(1) * 8,
                    ))
                }
            };
            self.publish().await;
            if !delay.is_zero() {
                self.wait_for_beat(delay).await;
            }
        }
    }

    async fn inbox_loop(self: Arc<Self>) {
        let mut backoff = Backoff::new(self.timing.tick, Duration::from_secs(60));
        loop {
            let Some((base_url, token)) = self.credentials() else {
                sleep(self.timing.tick).await;
                continue;
            };
            let api = Api {
                client: &self.client,
                base_url: &base_url,
            };
            match api.inbox(&token, self.timing.inbox_wait_secs).await {
                Ok(inbox) => {
                    backoff.succeed();
                    self.state.lock().commands.extend(inbox.commands);
                    for assignment in inbox.assignments {
                        self.take_assignment(&api, &token, assignment).await;
                    }
                }
                Err(ApiError::Unauthorized) => {
                    self.invalidate(&token).await;
                    sleep(self.timing.tick).await;
                }
                Err(err) => {
                    log::info!("Ward agents: inbox failed: {}", err.message());
                    sleep(backoff.fail()).await;
                }
            }
        }
    }

    /// Acks or refuses an assignment; an accepted one starts its preparation phase, or its
    /// worktree when the assignment already says everything the executor needs.
    async fn take_assignment(&self, api: &Api<'_>, token: &str, assignment: Assignment) {
        let session_id = assignment.session_id.clone();
        if session_id.is_empty() {
            return;
        }
        let duplicate = repeated_delivery(
            &mut self.state.lock(),
            &session_id,
            assignment.resume.is_some(),
        );
        if duplicate {
            // A repeated delivery: confirm again, start nothing new.
            let _ = api.ack(token, &session_id, &AckRequest::accept()).await;
            return;
        }
        let decision = self.decide(&assignment);
        let ack = match &decision {
            Ok(_) => AckRequest::accept(),
            Err(reason) => AckRequest::reject(reason.clone()),
        };
        match api.ack(token, &session_id, &ack).await {
            Ok(()) => {}
            Err(err) => {
                // Without an ack the service gives the task to someone else in 2 minutes.
                log::info!("Ward agents: ack failed: {}", err.message());
                if err == ApiError::Unauthorized {
                    self.invalidate(token).await;
                }
                return;
            }
        }
        let decision = match decision {
            Ok(decision) => decision,
            Err(reason) => {
                log::info!("Ward agents: refused an assignment: {reason}");
                return;
            }
        };
        let provider = decision.provider;
        if let Some(resume) = assignment.resume.clone() {
            log::info!(
                "Ward agents: took session {session_id} ({}, resumed by the owner)",
                provider.id()
            );
            self.start_resumed(assignment, provider, resume).await;
            return;
        }
        if prepare::runs(&assignment, decision.vault.is_some()) {
            let Some(vault) = decision.vault.clone() else {
                log::warn!("Ward agents: no vault for the preparation of {session_id}");
                return;
            };
            log::info!(
                "Ward agents: took session {session_id} ({}, preparing in the vault)",
                provider.id()
            );
            self.start_preparation(assignment, provider, decision.root, vault)
                .await;
            return;
        }
        let Some(repo) = decision.repo else {
            return;
        };
        log::info!(
            "Ward agents: took session {session_id} ({}, {repo})",
            provider.id()
        );
        // The phase is set right away by `start_worktree`.
        let mut session = Session::new(assignment, provider, Phase::Done);
        self.start_worktree(&mut session, &decision.root, &repo);
        let mut state = self.state.lock();
        state.known_ids.insert(session_id);
        state.sessions.push(session);
    }

    /// A session the owner brought back from the archive: the worktree is on the machine and
    /// the agent session is known, so nothing is cloned and no prompt is written — the agent
    /// opens in its own tab with the resume flag and goes on from where it stopped. Its
    /// transcript is shipped as a new part of the journal: Ward does not know what of it the
    /// service already has.
    async fn start_resumed(
        &self,
        assignment: Assignment,
        provider: &'static dyn Provider,
        resume: contract::Resume,
    ) {
        let session_id = assignment.session_id.clone();
        let worktree = PathBuf::from(&resume.worktree);
        let mut session = Session::new(assignment, provider, Phase::Done);
        if resume.agent_session_id.is_empty() || !worktree.is_dir() {
            let mut request = session.status_request(SessionStatus::Failed);
            request.message = Some(MESSAGE_RESTORE_NO_WORKTREE.to_owned());
            finish(&mut session, request);
        } else {
            session.worktree = Some(worktree.clone());
            // The agent session is known from the service: the transcript is found by its id,
            // and nothing has to be guessed from the time of the launch.
            session.found = Some(FoundSession {
                agent_session_id: resume.agent_session_id.clone(),
                pid: None,
                remote_control_url: None,
                transcript: None,
            });
            match self
                .launch(&session, &worktree, Some(&resume.agent_session_id))
                .await
            {
                Ok(pane_uuid) => {
                    session.pane_uuid = pane_uuid;
                    session.phase = Phase::Launched {
                        launched_at_ms: now_ms(),
                        at: Instant::now(),
                    };
                    let mut request = session.status_request(SessionStatus::Running);
                    request.message = Some(MESSAGE_RESTORED.to_owned());
                    session.set_status(request);
                }
                Err(err) => {
                    log::warn!("Ward agents: could not restore the session: {err}");
                    let mut request = session.status_request(SessionStatus::Failed);
                    request.message = Some(format!("Не удалось открыть вкладку: {err}"));
                    finish(&mut session, request);
                }
            }
        }
        let mut state = self.state.lock();
        state.known_ids.insert(session_id);
        state.sessions.push(session);
    }

    /// Puts the session on its worktree phase and starts the git thread for it.
    fn start_worktree(&self, session: &mut Session, root: &Path, repo: &launch::RepoId) {
        let slot: WorktreeSlot = Arc::default();
        let thread_slot = slot.clone();
        let root = root.to_path_buf();
        let repo = repo.clone();
        let repo_url = session.assignment.repo.clone();
        let base_branch = session.assignment.base_branch.clone();
        let branch = session.assignment.branch.clone();
        let session_id = session.assignment.session_id.clone();
        let search = self
            .state
            .lock()
            .config
            .as_ref()
            .map(|config| config.clone_search.clone())
            .unwrap_or_default();
        let spawned = std::thread::Builder::new()
            .name("ward-agents-git".to_owned())
            .spawn(move || {
                let result = git::prepare_worktree(
                    &root,
                    &WorktreeSpec {
                        repo_url: &repo_url,
                        repo: &repo,
                        base_branch: &base_branch,
                        branch: &branch,
                        session_id: &session_id,
                        search: &search,
                    },
                );
                *thread_slot.lock() = Some(result);
            });
        if let Err(err) = spawned {
            *slot.lock() = Some(Err(format!("could not start git: {err}")));
        }
        session.phase = Phase::Worktree(slot);
    }

    /// Opens the orchestrator session in the vault: the prompt Ward wrote, the runner and the
    /// `preparing` status. Nothing of Ward is written inside the vault; the files of the phase
    /// live under the repository root.
    async fn start_preparation(
        &self,
        assignment: Assignment,
        provider: &'static dyn Provider,
        root: PathBuf,
        vault: Vault,
    ) {
        let session_id = assignment.session_id.clone();
        let mut session = Session::new(assignment, provider, Phase::Done);
        let Some(dir) = launch::prepare_dir(&root, &session_id) else {
            let mut request = session.status_request(SessionStatus::Failed);
            request.reason = Some(REASON_PREPARE_FAILED.to_owned());
            request.message =
                Some("Не удалось развернуть подготовку: странный id сессии".to_owned());
            finish(&mut session, request);
            let mut state = self.state.lock();
            state.known_ids.insert(session_id);
            state.sessions.push(session);
            return;
        };
        let limit =
            Duration::from_secs(prepare::max_duration_min(&session.assignment).saturating_mul(60));
        session.prepare = Some(PrepareRun {
            plan_path: dir.join(plan::PLAN_FILE),
            dir,
            vault_dir: vault.dir(),
            limit,
            last_error: None,
            waited: Duration::ZERO,
            asked_at: None,
        });
        match self.launch_orchestrator(&mut session, &vault).await {
            Ok(pane_uuid) => {
                session.pane_uuid = pane_uuid;
                session.phase = Phase::Prepare {
                    launched_at_ms: now_ms(),
                    at: Instant::now(),
                };
                let mut request = session.status_request(SessionStatus::Preparing);
                request.message = Some(MESSAGE_PREPARING.to_owned());
                session.set_status(request);
            }
            Err(err) => {
                log::warn!("Ward agents: could not start the orchestrator: {err}");
                let mut request = session.status_request(SessionStatus::Failed);
                request.reason = Some(REASON_PREPARE_FAILED.to_owned());
                request.message = Some(format!("Не удалось запустить оркестратора: {err}"));
                finish(&mut session, request);
            }
        }
        let mut state = self.state.lock();
        state.known_ids.insert(session_id);
        state.sessions.push(session);
    }

    /// Writes the orchestrator's prompt and runner and opens its tab in the vault.
    async fn launch_orchestrator(
        &self,
        session: &mut Session,
        vault: &Vault,
    ) -> Result<Option<String>, String> {
        let Some(prepare) = session.prepare.as_ref() else {
            return Err("no preparation phase".to_owned());
        };
        let (kind, bash) = self
            .machine
            .runner()
            .ok_or_else(|| "no pwsh or Git Bash".to_owned())?;
        if !vault.is_ready() {
            return Err(format!("волта нет в {}", vault.root()));
        }
        let dir = prepare.dir.clone();
        let plan_path = prepare.plan_path.clone();
        let vault_dir = prepare.vault_dir.clone();
        let limit_min = prepare.limit.as_secs() / 60;
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let worker_name = self
            .state
            .lock()
            .config
            .as_ref()
            .map(|config| config.name.clone())
            .unwrap_or_default();
        let prompt = prepare::orchestrator_prompt(&prepare::PromptContext {
            assignment: &session.assignment,
            worker_name: &worker_name,
            vault,
            plan_path: &plan_path,
            max_duration_min: limit_min,
            provider: session.provider.id(),
        });
        std::fs::write(dir.join(launch::PROMPT_FILE), prompt).map_err(|err| err.to_string())?;
        let _ = std::fs::remove_file(dir.join(launch::EXIT_CODE_FILE));
        let _ = std::fs::remove_file(dir.join(launch::RUNNER_PID_FILE));
        let _ = std::fs::remove_file(&plan_path);
        // The orchestrator may be another agent than the executor, and its model comes from
        // the profile: judgement work, not the machine's default.
        let orchestrator = provider::by_id(vault.provider.trim()).unwrap_or(session.provider);
        let model = provider::launch_model(orchestrator, vault.model.as_deref());
        let name = prepare::session_name(&session.assignment.task_ref, &session.assignment.title);
        let command = orchestrator.command(&name, model, None);
        let script = launch::runner_script(kind, &dir, &vault_dir, &command, true);
        let runner = dir.join(kind.file_name());
        std::fs::write(&runner, script).map_err(|err| err.to_string())?;
        self.prepare_workspace(orchestrator, &vault_dir);
        self.host
            .open_tab(TabSpec {
                cwd: vault_dir,
                title: name,
                command: launch::tab_command(kind, &runner, bash.as_deref()),
                shell: shell_of(kind),
            })
            .await
    }

    /// What this machine does with an assignment, or the reason it takes none.
    fn decide(&self, assignment: &Assignment) -> Result<Decision, String> {
        let repo = if assignment.repo.trim().is_empty() {
            None
        } else {
            Some(
                launch::parse_repo(&assignment.repo)
                    .ok_or_else(|| "unsupported repository URL".to_owned())?,
            )
        };
        if self.machine.runner().is_none() {
            return Err("no pwsh or Git Bash to run the agent".to_owned());
        }
        let state = self.state.lock();
        let config = state
            .config
            .clone()
            .ok_or_else(|| "worker is not configured".to_owned())?;
        let vault = state.vault.clone().filter(Vault::is_ready);
        // The proxy of the machine profile: the scheduler decided on a heartbeat that may be
        // stale, so the freshest check is looked at again here. A part that failed is not
        // refused on this side: the service already filters on the report of the whole
        // profile, and a broken `skills` part should not stop the machine from working.
        if state.limits.require_proxy {
            match &state.profile {
                Some(status)
                    if status.proxy_required
                        && !status.proxy.as_ref().is_some_and(|proxy| proxy.ok) =>
                {
                    return Err("the proxy check does not pass on this worker".to_owned());
                }
                Some(_) => {}
                None if state.profile_version > 0 => {
                    return Err("the machine profile is not applied yet".to_owned());
                }
                None => {}
            }
        }
        drop(state);
        if prepare::is_needed(assignment) && vault.is_none() {
            return Err("no vault on this worker for the preparation phase".to_owned());
        }
        let resources = self.machine.resources(&config.root);
        let usage = self.state.lock().usage.clone();
        let state = self.state.lock();
        let active_sessions = state
            .sessions
            .iter()
            // A session that answered waits for the owner with an idle agent: it keeps its tab,
            // but not the slot, or two finished sessions would block the machine for good.
            .filter(|session| !session.is_done() && session.status != SessionStatus::Review)
            .count()
            + state
                .beats
                .iter()
                .filter(|beat| {
                    !state
                        .sessions
                        .iter()
                        .any(|session| session.assignment.session_id == beat.session_id)
                        && matches!(
                            beat.status,
                            SessionStatus::Assigned
                                | SessionStatus::Preparing
                                | SessionStatus::PlanReview
                                | SessionStatus::Running
                                | SessionStatus::Blocked
                        )
                })
                .count();
        let machine_state = MachineState {
            limits: &state.limits,
            paused: state.server_paused || config.paused,
            active_sessions,
            resources: &resources,
            agents: &state.agents,
            usage: &usage,
        };
        admission::check_slot(&machine_state, repo.as_ref())?;
        let provider = admission::choose_provider(&assignment.provider, &machine_state)?;
        Ok(Decision {
            provider,
            root: config.root.clone(),
            repo,
            vault,
        })
    }

    /// Picks up the sessions the run before Ward restarted left behind. The state files under
    /// the root are the whole list: the service hands an assignment out once, so a session it
    /// has already given to this worker never comes through the inbox again. What the service
    /// does do is take a session it had given up on (`lost`, after the heartbeat stopped) back
    /// to `running` as soon as the heartbeat names it again, which the restored session does.
    async fn restore_sessions(&self) {
        let Some(config) = self.refresh_config().await else {
            return;
        };
        let saved = resume::scan(&config.root);
        if saved.is_empty() {
            return;
        }
        log::info!(
            "Ward agents: {} session(s) of the run before Ward restarted are being picked up",
            saved.len()
        );
        let mut restored = Vec::new();
        for state in saved {
            let session_id = state.session_id.clone();
            let known = self.state.lock().known_ids.contains(&session_id);
            if known {
                continue;
            }
            match self.restore(state, &config.root) {
                Some(session) => restored.push(session),
                // Nothing of this session can be used any more; its state goes.
                None => {
                    if let Some(path) = resume::path(&config.root, &session_id) {
                        resume::remove(&path);
                    }
                }
            }
        }
        if restored.is_empty() {
            return;
        }
        {
            let mut state = self.state.lock();
            for session in &restored {
                state
                    .known_ids
                    .insert(session.assignment.session_id.clone());
            }
            state.sessions.extend(restored);
        }
        self.publish().await;
    }

    /// One session of the run before: everything the state file holds, then the phase it was
    /// in decides whether the agent is picked up or the session is closed. `None` for a state
    /// file this Ward can make nothing of.
    fn restore(&self, saved: SavedSession, root: &Path) -> Option<Session> {
        let provider = provider::by_id(&saved.provider)?;
        // The phase is set below, by the restoring of that very phase.
        let mut session = Session::new(saved.assignment.clone(), provider, Phase::Done);
        session.started_at_ms = saved.started_at_ms;
        session.started = instant_ago(elapsed_since_ms(saved.started_at_ms));
        session.status = saved.status;
        session.reason = saved.reason.clone();
        session.worktree = saved.worktree.as_deref().map(PathBuf::from);
        session.worktree_note = saved.worktree_note.clone();
        session.runner_pid = saved.runner_pid;
        session.found = saved.agent.as_ref().map(|agent| FoundSession {
            agent_session_id: agent.agent_session_id.clone(),
            pid: agent.pid,
            remote_control_url: agent.remote_control_url.clone(),
            transcript: None,
        });
        session.transcript = saved.transcript.as_deref().map(PathBuf::from);
        session.pane_uuid = saved.pane_uuid.clone();
        session.role = match saved.role.as_deref() {
            Some(ROLE_ORCHESTRATOR) => Some(ROLE_ORCHESTRATOR),
            Some(ROLE_EXECUTOR) => Some(ROLE_EXECUTOR),
            _ => None,
        };
        session
            .journal
            .resume(saved.journal.offset, saved.journal.next_seq);
        // Only what the service has not taken is sent again, but what the session is read from
        // is the end of the transcript, so the tail is filled from the file.
        if let Some(path) = session.transcript.clone()
            && let Err(err) = session.journal.prime_tail(&path)
        {
            log::debug!("Ward agents: the transcript of a session is not readable yet: {err}");
        }
        session.prepare = saved.prepare.as_ref().map(|prepare| PrepareRun {
            dir: PathBuf::from(&prepare.dir),
            plan_path: PathBuf::from(&prepare.plan_path),
            vault_dir: PathBuf::from(&prepare.vault_dir),
            limit: Duration::from_secs(prepare.limit_secs),
            last_error: None,
            waited: Duration::ZERO,
            asked_at: None,
        });
        // There is a state file for this session; the tick that finds the session over removes
        // it, whether this run ever wrote it or not.
        session.saved_state = Some(String::new());
        let launched_at = instant_ago(elapsed_since_ms(saved.launched_at_ms));
        match saved.phase {
            SavedPhase::Worktree => self.restore_worktree(&mut session, root),
            SavedPhase::Prepare => self.restore_prepare(&mut session, &saved, launched_at),
            SavedPhase::PlanReview => self.restore_plan_review(&mut session),
            SavedPhase::Launched => self.restore_launched(&mut session, &saved, launched_at),
        }
        Some(session)
    }

    /// The worktree was not ready when Ward went: no agent had started, so the repository is
    /// deployed again, which is what the git thread would have done anyway.
    fn restore_worktree(&self, session: &mut Session, root: &Path) {
        let Some(repo) = launch::parse_repo(&session.assignment.repo) else {
            let mut request = session.status_request(SessionStatus::Failed);
            request.reason = Some(REASON_WARD_RESTARTED.to_owned());
            request.message = Some(format!(
                "{MESSAGE_AGENT_GONE}: репозиторий назначения не разобрать"
            ));
            finish(session, request);
            return;
        };
        log::info!(
            "Ward agents: session {} deploys its worktree again after the restart",
            session.assignment.session_id
        );
        session.message = Some(MESSAGE_RESUMED_WORKTREE.to_owned());
        session.reset_agent();
        self.start_worktree(session, root, &repo);
    }

    /// The orchestrator of the preparation phase. Alive (or back with the tab of an update
    /// relaunch): the phase goes on. Gone, but its `plan.json` is there: the next tick takes
    /// the plan, so the preparation counts as done. Gone with nothing: the preparation ends
    /// saying what happened, not blaming the orchestrator for a plan it never got to write.
    fn restore_prepare(&self, session: &mut Session, saved: &SavedSession, launched_at: Instant) {
        let Some((plan_path, vault_dir)) = session
            .prepare
            .as_ref()
            .map(|prepare| (prepare.plan_path.clone(), prepare.vault_dir.clone()))
        else {
            let mut request = session.status_request(SessionStatus::Failed);
            request.reason = Some(REASON_PREPARE_FAILED.to_owned());
            request.message = Some(MESSAGE_PREPARE_AGENT_GONE.to_owned());
            finish(session, request);
            return;
        };
        let alive = self.revive(session, &vault_dir, saved.launched_at_ms, ROLE_ORCHESTRATOR);
        let has_plan = read_plan_file(&plan_path).is_some();
        session.phase = Phase::Prepare {
            launched_at_ms: saved.launched_at_ms,
            // Without the orchestrator the wait for it starts now, not when it was launched.
            at: if alive || has_plan {
                launched_at
            } else {
                Instant::now()
            },
        };
        // The plan on disk settles the phase on the next tick, whether the orchestrator is
        // there or not; without either, the orchestrator is waited for.
        session.restarted = !alive && !has_plan;
        let message = match (alive, has_plan) {
            (true, _) => MESSAGE_RESUMED_PREPARING,
            (false, true) => MESSAGE_PREPARING,
            (false, false) => MESSAGE_WAITING_AFTER_RESTART,
        };
        let mut request = session.status_request(SessionStatus::Preparing);
        request.message = Some(message.to_owned());
        session.set_status(request);
    }

    /// The plan was ready and the owner had not answered: nothing of the agent is running, the
    /// session only waits again. The plan is read from `plan.json` once more, so the panel
    /// shows it and the service gets it again — a repeat of the same plan is idempotent.
    fn restore_plan_review(&self, session: &mut Session) {
        let plan = session
            .prepare
            .as_ref()
            .and_then(|prepare| read_plan_file(&prepare.plan_path))
            .and_then(|text| plan::parse(&text).ok());
        if let Some(plan) = plan {
            session.plan_outbox = Some(plan.clone());
            session.plan = Some(plan);
        }
        session.phase = Phase::PlanReview;
        session.reset_agent();
        let mut request = session.status_request(SessionStatus::PlanReview);
        request.message = Some(MESSAGE_RESUMED_PLAN_REVIEW.to_owned());
        session.set_status(request);
    }

    /// The executor: picked up while its process is still there, closed honestly when it is
    /// not. An exit code the runner managed to leave is the end of the session, whatever Ward
    /// was doing at the time; without one the restart is what ended it, and the message says
    /// that instead of "never started" or a `review` nobody earned.
    fn restore_launched(&self, session: &mut Session, saved: &SavedSession, launched_at: Instant) {
        let Some(worktree) = session.worktree.clone() else {
            self.fail_after_restart(session, MESSAGE_AGENT_GONE);
            return;
        };
        let agents = launch::agents_dir(&worktree);
        if session.runner_pid.is_none() {
            session.runner_pid = read_runner_pid(&agents);
        }
        if self.revive(session, &worktree, saved.launched_at_ms, ROLE_EXECUTOR) {
            session.phase = Phase::Launched {
                launched_at_ms: saved.launched_at_ms,
                at: launched_at,
            };
            // A session that was waiting for the owner goes on waiting; anything else is
            // running again.
            let blocked = saved.status == SessionStatus::Blocked;
            let mut request = session.status_request(if blocked {
                SessionStatus::Blocked
            } else {
                SessionStatus::Running
            });
            request.reason = saved.reason.clone().filter(|_| blocked);
            request.message = Some(session.with_worktree_note(MESSAGE_RESUMED));
            log::info!(
                "Ward agents: session {} is picked up after the restart of Ward",
                session.assignment.session_id
            );
            session.set_status(request);
            return;
        }
        if let Some(code) = read_exit_code(&agents) {
            log::info!(
                "Ward agents: session {} ended with {code} while Ward was away",
                session.assignment.session_id
            );
            self.end(session, Some(code));
            return;
        }
        // The agent is not there yet, which does not mean it is gone: an update relaunch
        // reopens the session's tab with a resume command, and the agent needs a moment more
        // than Ward to come up. The watch of the session waits it out ([`Worker::watch`]) and
        // only then gives the session up.
        session.phase = Phase::Launched {
            launched_at_ms: saved.launched_at_ms,
            at: Instant::now(),
        };
        session.restarted = true;
        let mut request = session.status_request(session.status);
        request.reason = session.reason.clone();
        request.message = Some(MESSAGE_WAITING_AFTER_RESTART.to_owned());
        if !is_reportable(request.status) {
            request.status = SessionStatus::Running;
            request.reason = None;
        }
        session.set_status(request);
    }

    /// A session picked up after a restart of Ward whose agent has not turned up: it is looked
    /// for once more every tick, because an update relaunch brings it back with the tab, and
    /// the grace of a launch is how long that may take. Returns whether the session is over.
    fn wait_for_agent(
        &self,
        session: &mut Session,
        cwd: &Path,
        launched_at_ms: i64,
        launched_at: Instant,
        role: &'static str,
    ) -> bool {
        if self.revive(session, cwd, launched_at_ms, role) {
            session.restarted = false;
            let status = if role == ROLE_EXECUTOR {
                SessionStatus::Running
            } else {
                SessionStatus::Preparing
            };
            let mut request = session.status_request(status);
            request.message = Some(session.with_worktree_note(if role == ROLE_EXECUTOR {
                MESSAGE_RESUMED
            } else {
                MESSAGE_RESUMED_PREPARING
            }));
            session.set_status(request);
            return false;
        }
        if launched_at.elapsed() < self.timing.session_start_grace {
            return false;
        }
        if role == ROLE_EXECUTOR {
            self.fail_after_restart(session, MESSAGE_AGENT_GONE);
        } else {
            self.fail_prepare_after_restart(session);
        }
        true
    }

    /// Closes a session whose agent did not survive the restart of Ward.
    fn fail_after_restart(&self, session: &mut Session, message: &str) {
        log::info!(
            "Ward agents: session {} did not survive the restart of Ward",
            session.assignment.session_id
        );
        self.stop_agent(session);
        self.read_journal(session);
        let mut request = session.status_request(SessionStatus::Failed);
        request.reason = Some(REASON_WARD_RESTARTED.to_owned());
        request.message = Some(message.to_owned());
        finish(session, request);
    }

    /// The same for the orchestrator of the preparation phase, which left no plan: a failed
    /// preparation, named for what it was.
    fn fail_prepare_after_restart(&self, session: &mut Session) {
        let code = session
            .prepare
            .as_ref()
            .and_then(|prepare| read_exit_code(&prepare.dir));
        log::info!(
            "Ward agents: the orchestrator of {} did not survive the restart of Ward",
            session.assignment.session_id
        );
        self.stop_agent(session);
        self.read_journal(session);
        let mut request = session.status_request(SessionStatus::Failed);
        request.reason = Some(REASON_PREPARE_FAILED.to_owned());
        request.message = Some(match code {
            Some(code) => format!("{MESSAGE_PREPARE_AGENT_GONE} (код {code})"),
            None => MESSAGE_PREPARE_AGENT_GONE.to_owned(),
        });
        finish(session, request);
    }

    /// Whether the agent of a saved launch is still there, and what it is now. A process of
    /// that launch which is still alive is taken as it is. Otherwise the agent's own files are
    /// read once more: an update relaunch reopens the session's tab with a resume command, so
    /// the agent may be back under another process — with the same transcript when it resumed
    /// its session, with another one when it started a new one.
    ///
    /// Either way the journal gets a line saying Ward restarted here, and the chunks go on
    /// being numbered from where the service stopped taking them.
    fn revive(
        &self,
        session: &mut Session,
        cwd: &Path,
        launched_at_ms: i64,
        role: &'static str,
    ) -> bool {
        let pids = session.agent_pids();
        let mut alive = pids.iter().any(|pid| self.machine.is_alive(*pid));
        // The same process means the same transcript, read on from the position the service
        // confirmed.
        let mut same_transcript = alive;
        if !alive
            && let Some(found) = session
                .provider
                .find_session(&self.homes, cwd, launched_at_ms)
            && found.pid.is_none_or(|pid| self.machine.is_alive(pid))
        {
            same_transcript = session
                .found
                .as_ref()
                .is_some_and(|old| old.agent_session_id == found.agent_session_id);
            log::info!(
                "Ward agents: session {} found the agent session {} again after the restart",
                session.assignment.session_id,
                found.agent_session_id
            );
            // The tab of an update relaunch runs the agent with its own resume command: the
            // runner of the launch went with the old tab and leaves no exit code. From now on
            // the agent's own process says whether the session still runs, or nothing does
            // when its pid is not known, but never the runner that is gone.
            session.runner_pid = found.pid;
            session.runner_left = true;
            session.runner_gone_since = None;
            session.found = Some(found);
            alive = true;
        }
        if !alive {
            return false;
        }
        let agent = session
            .found
            .as_ref()
            .map(|found| found.agent_session_id.clone());
        let marker = restart_marker(role, &session.assignment.session_id, agent.as_deref());
        if same_transcript && session.transcript.is_some() {
            session.journal.mark(&marker);
        } else {
            // Another transcript, or none found yet: its own part of the journal, read from
            // the beginning of the file.
            session.transcript = None;
            session.journal.start_part(&marker);
            session.role = Some(role);
        }
        // The idle watch counts from the restart: the agent may have said its last word while
        // Ward was away.
        session.transcript_grew_at = Some(Instant::now());
        true
    }

    async fn sessions_loop(self: Arc<Self>) {
        loop {
            let (mut sessions, commands) = {
                let mut state = self.state.lock();
                (
                    std::mem::take(&mut state.sessions),
                    std::mem::take(&mut state.commands),
                )
            };
            for command in commands {
                apply_command(&mut sessions, command);
            }
            let root = self
                .state
                .lock()
                .config
                .as_ref()
                .map(|config| config.root.clone());
            for session in &mut sessions {
                self.advance(session).await;
                self.show_session(session).await;
                self.stream_session(session).await;
                if let Some(pane) = &session.pane_uuid {
                    self.host.tap_pane(pane).await;
                }
                self.deliver(session).await;
                // After the delivery: what is saved is the journal position the service has
                // confirmed, so a crash of Ward costs at most this tick.
                if let Some(root) = &root {
                    save_session_state(session, root);
                }
            }
            {
                let mut state = self.state.lock();
                sessions.append(&mut state.sessions);
                state.settled_ids = settle_runs(&mut sessions);
                // Settled sessions beyond the panel's history are dropped.
                let finished = sessions
                    .iter()
                    .filter(|session| session.is_settled())
                    .count();
                let mut drop_count = finished.saturating_sub(MAX_FINISHED_SESSIONS);
                sessions.retain(|session| {
                    if drop_count > 0 && session.is_settled() {
                        drop_count -= 1;
                        false
                    } else {
                        true
                    }
                });
                state.beats = sessions
                    .iter()
                    .filter(|session| !session.is_settled())
                    .map(|session| SessionBeat {
                        session_id: session.assignment.session_id.clone(),
                        status: session.status,
                    })
                    .collect();
                state.views = sessions.iter().rev().map(Session::view).collect();
                state.sessions = sessions;
            }
            self.publish().await;
            sleep(self.timing.tick).await;
        }
    }

    /// Moves a session one step on.
    async fn advance(&self, session: &mut Session) {
        match &session.phase {
            Phase::Done => {}
            Phase::Prepare { launched_at_ms, at } => {
                let (launched_at_ms, launched_at) = (*launched_at_ms, *at);
                self.watch_preparation(session, launched_at_ms, launched_at)
                    .await;
            }
            Phase::PrepareExit { root, repo, since } => {
                let (root, repo, since) = (root.clone(), repo.clone(), *since);
                self.watch_prepare_exit(session, &root, &repo, since);
            }
            Phase::PlanReview => self.watch_plan_review(session).await,
            Phase::Worktree(slot) => {
                let result = slot.lock().take();
                match result {
                    None => {}
                    Some(Err(err)) => {
                        log::warn!("Ward agents: preparing the worktree failed: {err}");
                        let mut request = session.status_request(SessionStatus::Failed);
                        request.message = Some(format!("Не удалось развернуть репозиторий: {err}"));
                        finish(session, request);
                    }
                    Some(Ok(prepared)) => {
                        let worktree = prepared.path.clone();
                        session.worktree_note = prepared.summary();
                        log::info!(
                            "Ward agents: session {} works in {} ({})",
                            session.assignment.session_id,
                            worktree.display(),
                            session
                                .worktree_note
                                .as_deref()
                                .unwrap_or("существующая ветка")
                        );
                        session.worktree = Some(worktree.clone());
                        if let Some((reason, _)) = session.stop {
                            let mut request = session.status_request(SessionStatus::Failed);
                            request.message = Some(reason.to_owned());
                            finish(session, request);
                            return;
                        }
                        match self.launch(session, &worktree, None).await {
                            Ok(pane_uuid) => {
                                session.pane_uuid = pane_uuid;
                                session.phase = Phase::Launched {
                                    launched_at_ms: now_ms(),
                                    at: Instant::now(),
                                };
                            }
                            Err(err) => {
                                log::warn!("Ward agents: could not start the session: {err}");
                                let mut request = session.status_request(SessionStatus::Failed);
                                request.message =
                                    Some(format!("Не удалось открыть вкладку: {err}"));
                                finish(session, request);
                            }
                        }
                    }
                }
            }
            Phase::Launched { launched_at_ms, at } => {
                let (launched_at_ms, launched_at) = (*launched_at_ms, *at);
                self.watch(session, launched_at_ms, launched_at).await;
            }
        }
    }

    /// Lets the agent start in `cwd` without a question on the screen of its tab (Claude Code's
    /// folder trust). A failure only costs the session the answer it needs, so the launch goes
    /// on and the watch of the session reports a tab that stays silent.
    fn prepare_workspace(&self, provider: &'static dyn Provider, cwd: &Path) {
        if let Err(err) = provider.prepare_workspace(&self.homes, cwd) {
            log::warn!(
                "Ward agents: could not record the trust of {} for {}: {err}",
                cwd.display(),
                provider.id()
            );
        }
    }

    /// Writes the session files and opens its tab. `resume`: the agent session to continue
    /// instead of a new one — then the prompt is not written, the agent has it in its history.
    /// The prompt ends with Ward's note on `about.json` ([`about`]), which keeps the description
    /// of the session on the Agents screen current.
    async fn launch(
        &self,
        session: &Session,
        worktree: &Path,
        resume: Option<&str>,
    ) -> Result<Option<String>, String> {
        let (kind, bash) = self
            .machine
            .runner()
            .ok_or_else(|| "no pwsh or Git Bash".to_owned())?;
        let agents = launch::agents_dir(worktree);
        std::fs::create_dir_all(&agents).map_err(|err| err.to_string())?;
        if resume.is_none() {
            let prompt = about::with_instruction(
                &session.assignment.prompt,
                &agents.join(about::ABOUT_FILE),
            );
            std::fs::write(agents.join(launch::PROMPT_FILE), prompt)
                .map_err(|err| err.to_string())?;
        }
        let _ = std::fs::remove_file(agents.join(launch::EXIT_CODE_FILE));
        let _ = std::fs::remove_file(agents.join(launch::RUNNER_PID_FILE));
        let name = launch::session_name(&session.assignment.task_ref, &session.assignment.title);
        // The model may come from the plan, and the orchestrator may have meant another agent.
        let model = provider::launch_model(session.provider, session.assignment.model.as_deref());
        let command = session.provider.command(&name, model, resume);
        let script = launch::runner_script(kind, &agents, worktree, &command, resume.is_none());
        let runner = agents.join(kind.file_name());
        std::fs::write(&runner, script).map_err(|err| err.to_string())?;
        self.prepare_workspace(session.provider, worktree);
        self.host
            .open_tab(TabSpec {
                cwd: worktree.to_path_buf(),
                title: name,
                command: launch::tab_command(kind, &runner, bash.as_deref()),
                shell: shell_of(kind),
            })
            .await
    }

    /// The orchestrator session of the preparation phase: the same tracking as an executor
    /// plus the plan it must leave, the time limit and the owner's `stop`.
    async fn watch_preparation(
        &self,
        session: &mut Session,
        launched_at_ms: i64,
        launched_at: Instant,
    ) {
        let Some(prepare) = session.prepare.as_ref() else {
            return;
        };
        let (dir, plan_path, vault_dir, limit) = (
            prepare.dir.clone(),
            prepare.plan_path.clone(),
            prepare.vault_dir.clone(),
            prepare.limit,
        );
        self.track_agent(
            session,
            &dir,
            &vault_dir,
            launched_at_ms,
            launched_at,
            ROLE_ORCHESTRATOR,
        );
        self.send_messages(session).await;
        if self.journal_due(session) {
            self.read_journal(session);
        }

        // The plan may be half written when Ward looks: a broken file is not fatal while the
        // orchestrator is still going.
        if let Some(text) = read_plan_file(&plan_path) {
            match plan::parse(&text) {
                Ok(plan) => {
                    self.accept_plan(session, plan).await;
                    return;
                }
                Err(err) => {
                    if let Some(prepare) = session.prepare.as_mut() {
                        prepare.last_error = Some(err);
                    }
                }
            }
        }

        if let Some((reason, _)) = session.stop {
            self.stop_agent(session);
            let mut request = session.status_request(SessionStatus::Failed);
            request.reason = Some(REASON_PREPARE_FAILED.to_owned());
            request.message = Some(reason.to_owned());
            self.read_journal(session);
            finish(session, request);
            return;
        }
        // Picked up after a restart of Ward without its orchestrator: it is waited for, and
        // nothing is concluded from a process that is not there yet.
        if session.restarted {
            self.wait_for_agent(
                session,
                &vault_dir,
                launched_at_ms,
                launched_at,
                ROLE_ORCHESTRATOR,
            );
            return;
        }
        if is_silent(
            session,
            launched_at.elapsed(),
            self.timing.session_start_grace,
        ) {
            self.fail_preparation(session, MESSAGE_NOT_STARTED);
            return;
        }
        // A question to the owner stops the clock of the preparation until the answer comes.
        let waiting = session.status == SessionStatus::Blocked;
        let waited = session.prepare.as_mut().map_or(Duration::ZERO, |prepare| {
            match (waiting, prepare.asked_at) {
                (true, None) => prepare.asked_at = Some(Instant::now()),
                (false, Some(at)) => {
                    prepare.waited += at.elapsed();
                    prepare.asked_at = None;
                }
                _ => {}
            }
            prepare.waited
        });
        if !waiting && launched_at.elapsed().saturating_sub(waited) >= limit {
            self.fail_preparation(session, MESSAGE_PREPARE_TOO_LONG);
            return;
        }
        // The orchestrator ended its turn without a plan and is still there: its last words are
        // questions for the owner, written as text or with the question tool alike. A turn that
        // ended on an error of the API asks nothing: the preparation failed.
        if self.gone_quiet(session) {
            self.read_journal(session);
            if self.gone_quiet(session) && session.status != SessionStatus::Blocked {
                match session.provider.last_turn(&session.journal.tail()) {
                    LastTurn::Working => {}
                    LastTurn::Failed(error) => {
                        self.fail_preparation(session, &agent_error_message(&error));
                        return;
                    }
                    LastTurn::Answered | LastTurn::Asked => {
                        self.ask_owner(session);
                        if let Some(prepare) = session.prepare.as_mut() {
                            prepare.asked_at = Some(Instant::now());
                        }
                    }
                }
            }
        }
        let gone = session
            .runner_pid
            .is_some_and(|pid| !self.machine.is_alive(pid))
            || dir.join(launch::EXIT_CODE_FILE).is_file();
        if gone {
            let since = *session.runner_gone_since.get_or_insert_with(Instant::now);
            // The plan may land a moment after the agent's own process is gone.
            if since.elapsed() >= self.timing.exit_grace {
                // Ward ends the orchestrator only after its plan is taken, so a nonzero code
                // here is the orchestrator's own failure and is named as one.
                let reason = match read_exit_code(&dir) {
                    Some(code) if code != 0 => format!("{MESSAGE_PREPARE_CRASHED} (код {code})"),
                    _ => MESSAGE_PREPARE_NO_PLAN.to_owned(),
                };
                self.fail_preparation(session, &reason);
            }
        } else {
            session.runner_gone_since = None;
        }
    }

    /// The preparation did not end with a usable plan.
    fn fail_preparation(&self, session: &mut Session, reason: &str) {
        self.stop_agent(session);
        self.read_journal(session);
        let detail = session
            .prepare
            .as_ref()
            .and_then(|prepare| prepare.last_error.clone());
        let mut request = session.status_request(SessionStatus::Failed);
        request.reason = Some(REASON_PREPARE_FAILED.to_owned());
        request.message = Some(match detail {
            Some(detail) => format!("{reason}: {detail}"),
            None => reason.to_owned(),
        });
        log::warn!(
            "Ward agents: preparation of {} failed: {reason}",
            session.assignment.session_id
        );
        finish(session, request);
    }

    /// The plan is there: it fills the assignment, goes to the service, and the session waits
    /// for the orchestrator to end itself ([`Self::watch_prepare_exit`]), because the plan was
    /// its last action.
    async fn accept_plan(&self, session: &mut Session, plan: Plan) {
        if let Err(err) = plan::apply(&plan, &mut session.assignment) {
            if let Some(prepare) = session.prepare.as_mut() {
                prepare.last_error = Some(err);
            }
            self.fail_preparation(session, "План неполный");
            return;
        }
        // The agent of the plan is not acted on: the service picks it by the subscription
        // limits of the machine, and the choice was made when the assignment was taken. Only
        // the model of the plan is taken, in `plan::apply`, and it reaches the agent only when
        // it is a model of that agent ([`provider::launch_model`]).
        let repo = match launch::parse_repo(&session.assignment.repo) {
            Some(repo) => repo,
            None => {
                if let Some(prepare) = session.prepare.as_mut() {
                    prepare.last_error = Some(format!(
                        "репозиторий {} не разобрать",
                        session.assignment.repo
                    ));
                }
                self.fail_preparation(session, "План не годится");
                return;
            }
        };
        let (allowed_repos, root) = {
            let state = self.state.lock();
            (
                state.limits.allowed_repos.clone(),
                state.config.as_ref().map(|config| config.root.clone()),
            )
        };
        if !launch::is_repo_allowed(&repo, &allowed_repos) {
            if let Some(prepare) = session.prepare.as_mut() {
                prepare.last_error = Some(format!("репозиторий {repo} не разрешён на этой машине"));
            }
            self.fail_preparation(session, "План не годится");
            return;
        }
        let Some(root) = root else {
            self.fail_preparation(session, "Воркер больше не настроен");
            return;
        };
        log::info!(
            "Ward agents: session {} has a plan ({repo}, {})",
            session.assignment.session_id,
            session.assignment.branch
        );
        session.plan_outbox = Some(plan.clone());
        session.plan = Some(plan);
        // The plan was the orchestrator's last action, so it is left to end by itself: the
        // next phase waits for its exit before anything of the executor starts.
        session.phase = Phase::PrepareExit {
            root,
            repo,
            since: Instant::now(),
        };
    }

    /// The plan is taken and the orchestrator finishes its own session: Ward waits
    /// [`Timing::prepare_exit_wait`] for its exit, so its closing answer lands in the
    /// transcript, reads the journal to the end and only then starts the executor. An
    /// orchestrator that does not go by itself is ended by Ward, as before; that exit code is
    /// Ward's doing and says nothing about the preparation, which the plan already settled.
    fn watch_prepare_exit(
        &self,
        session: &mut Session,
        root: &Path,
        repo: &launch::RepoId,
        since: Instant,
    ) {
        let dir = session.prepare.as_ref().map(|prepare| prepare.dir.clone());
        let gone = match dir.as_deref() {
            Some(dir) => self.orchestrator_gone(session, dir),
            None => false,
        };
        // A stop from the owner is not worth waiting out: the phase after this one answers it.
        if !gone && session.stop.is_none() && since.elapsed() < self.timing.prepare_exit_wait {
            if self.journal_due(session) {
                self.read_journal(session);
            }
            return;
        }
        if gone {
            // Its own code, whatever it is: the preparation was settled by the plan.
            match dir.as_deref().and_then(read_exit_code) {
                Some(code) => log::info!(
                    "Ward agents: the orchestrator of {} ended by itself with {code}",
                    session.assignment.session_id
                ),
                None => log::info!(
                    "Ward agents: the orchestrator of {} ended by itself",
                    session.assignment.session_id
                ),
            }
        } else {
            log::info!(
                "Ward agents: the orchestrator of {} did not end by itself after its plan, \
                 Ward ends it",
                session.assignment.session_id
            );
            self.stop_agent(session);
        }
        // The last lines of the orchestrator belong to the journal either way, so the panel
        // shows its closing answer instead of a sentence cut in half.
        self.read_journal(session);
        if session.assignment.plan_approval {
            session.phase = Phase::PlanReview;
            let mut request = session.status_request(SessionStatus::PlanReview);
            request.message = Some(format!(
                "План готов и ждёт согласования: {}",
                session.plan.as_ref().map(plan::summary).unwrap_or_default()
            ));
            session.set_status(request);
            return;
        }
        session.reset_agent();
        self.start_worktree(session, root, repo);
    }

    /// Whether the orchestrator's own processes are over: the runner left its exit code, or
    /// neither the agent nor the runner is alive any more.
    fn orchestrator_gone(&self, session: &Session, dir: &Path) -> bool {
        if dir.join(launch::EXIT_CODE_FILE).is_file() {
            return true;
        }
        let pids = session.agent_pids();
        !pids.is_empty() && pids.iter().all(|pid| !self.machine.is_alive(*pid))
    }

    /// Waiting in `plan-review` for `approve` or `reject`.
    async fn watch_plan_review(&self, session: &mut Session) {
        if self.journal_due(session) {
            self.read_journal(session);
        }
        match session.approval.take() {
            Some(Ok(())) => {
                let root = self
                    .state
                    .lock()
                    .config
                    .as_ref()
                    .map(|config| config.root.clone());
                let repo = launch::parse_repo(&session.assignment.repo);
                match (root, repo) {
                    (Some(root), Some(repo)) => {
                        log::info!(
                            "Ward agents: the owner approved the plan of {}",
                            session.assignment.session_id
                        );
                        session.reset_agent();
                        self.start_worktree(session, &root, &repo);
                    }
                    _ => self.fail_preparation(session, "План согласован, но запустить нечего"),
                }
            }
            Some(Err(reason)) => {
                let mut request = session.status_request(SessionStatus::Failed);
                request.reason = Some(REASON_PLAN_REJECTED.to_owned());
                request.message = Some(if reason.trim().is_empty() {
                    MESSAGE_PLAN_REJECTED.to_owned()
                } else {
                    format!("{MESSAGE_PLAN_REJECTED}: {reason}")
                });
                finish(session, request);
            }
            None => {
                if let Some((reason, _)) = session.stop {
                    let mut request = session.status_request(SessionStatus::Failed);
                    request.message = Some(reason.to_owned());
                    finish(session, request);
                    return;
                }
                let limit = Duration::from_secs(session.assignment.max_duration_min * 60);
                if session.assignment.max_duration_min > 0 && session.started.elapsed() >= limit {
                    let mut request = session.status_request(SessionStatus::Failed);
                    request.message = Some(MESSAGE_TOO_LONG.to_owned());
                    finish(session, request);
                }
            }
        }
    }

    /// Finds the agent session of a launch, its transcript and the runner's pid; shared by the
    /// orchestrator and the executor.
    fn track_agent(
        &self,
        session: &mut Session,
        files_dir: &Path,
        cwd: &Path,
        launched_at_ms: i64,
        launched_at: Instant,
        role: &'static str,
    ) {
        if session.runner_pid.is_none() && !session.runner_left {
            session.runner_pid = read_runner_pid(files_dir);
        }
        if session.found.is_none()
            && let Some(found) = session
                .provider
                .find_session(&self.homes, cwd, launched_at_ms)
        {
            log::info!(
                "Ward agents: session {} runs agent session {} ({role})",
                session.assignment.session_id,
                found.agent_session_id
            );
            session.found = Some(found);
            if role == ROLE_EXECUTOR {
                let mut request = session.status_request(SessionStatus::Running);
                request.message = Some(session.with_worktree_note(MESSAGE_RUNNING));
                session.set_status(request);
            } else {
                // The orchestrator's own ids reach the card with the `preparing` status.
                let mut request = session.status_request(SessionStatus::Preparing);
                request.message = Some(MESSAGE_PREPARING.to_owned());
                session.set_status(request);
            }
        }
        if role == ROLE_EXECUTOR
            && session.found.is_none()
            && matches!(
                session.status,
                SessionStatus::Assigned | SessionStatus::Preparing | SessionStatus::PlanReview
            )
            && launched_at.elapsed() >= self.timing.session_lookup_grace
        {
            let mut request = session.status_request(SessionStatus::Running);
            request.message = Some(session.with_worktree_note(MESSAGE_NO_SESSION_ID));
            session.set_status(request);
        }
        if session.transcript.is_none() {
            let path = session
                .found
                .as_ref()
                .and_then(|found| session.provider.transcript(&self.homes, cwd, found));
            if let Some(path) = path {
                // One journal for both sessions of the assignment, each part marked with its
                // role.
                if session.role != Some(role) {
                    session
                        .journal
                        .start_part(&role_marker(role, &session.assignment.session_id));
                    session.role = Some(role);
                }
                session.transcript = Some(path);
            }
        }
    }

    /// Binds the session to its tab again and returns that tab's pane: the pane the session
    /// already had (an update relaunch reopens the tab under the same UUID), the pane whose
    /// agent runs this very agent session, or the pane whose agent works in the session's
    /// directory. `None` when no open tab is the session's, and the pane the session knew is
    /// then forgotten rather than kept as a tab that is not there.
    async fn find_pane(&self, session: &mut Session) -> Option<String> {
        let query = session.pane_query();
        if query.is_empty() {
            return None;
        }
        let found = self.host.find_pane(query).await;
        if found != session.pane_uuid {
            match &found {
                Some(pane) => log::info!(
                    "Ward agents: session {} found its tab again ({pane})",
                    session.assignment.session_id
                ),
                None => log::info!(
                    "Ward agents: session {} has no open tab of its own",
                    session.assignment.session_id
                ),
            }
            session.pane_uuid = found.clone();
        }
        found
    }

    /// Puts the session in front of the owner. Its own tab comes forward; a session whose tab
    /// is gone gets the agent back in a new tab, resumed where it stopped and without the task
    /// prompt again. From another machine (`remote`) nothing comes forward here: the session
    /// only has to run, and the agent found again reports its remote control link.
    async fn show_session(&self, session: &mut Session) {
        let Some(show) = session.show.take() else {
            return;
        };
        let (pane, reopened) = self.pane_or_reopen(session).await;
        // The resumed agent is another process with another remote control link: the next tick
        // looks it up again and reports it.
        if reopened && show.remote && !session.is_done() {
            session.found = None;
        }
        if show.remote {
            return;
        }
        let Some(pane) = pane else { return };
        if let Err(err) = self.host.focus_pane(&pane).await {
            log::warn!("Ward agents: could not bring the session forward: {err}");
        }
    }

    /// The tab of the session, opened again when it is gone; `true` when it had to be opened.
    async fn pane_or_reopen(&self, session: &mut Session) -> (Option<String>, bool) {
        if let Some(pane) = self.find_pane(session).await {
            return (Some(pane), false);
        }
        match self.reopen(session).await {
            Ok(opened) => {
                log::info!(
                    "Ward agents: session {} opened again for the owner",
                    session.assignment.session_id
                );
                session.pane_uuid = opened.clone();
                (opened, true)
            }
            Err(err) => {
                log::warn!(
                    "Ward agents: could not open session {} for the owner: {err}",
                    session.assignment.session_id
                );
                (None, false)
            }
        }
    }

    /// Sends the session's tab to the owner at another machine, see [`super::stream`].
    async fn stream_session(&self, session: &mut Session) {
        if !std::mem::take(&mut session.stream) {
            return;
        }
        let (pane, reopened) = self.pane_or_reopen(session).await;
        if reopened && !session.is_done() {
            session.found = None;
        }
        let Some(pane) = pane else { return };
        let Some((base_url, token)) = self.credentials() else {
            log::warn!("Ward agents: no registration to stream the session with");
            return;
        };
        let Some(url) = stream::worker_url(&base_url, &session.assignment.session_id) else {
            log::warn!("Ward agents: no stream address for {base_url}");
            return;
        };
        self.host.tap_pane(&pane).await;
        if let Err(err) = self.host.stream_pane(&pane, &url, &token).await {
            log::warn!("Ward agents: could not stream the session: {err}");
        }
    }

    /// Opens the tab of a session that lost it, resuming the agent that ran in it. The prompt
    /// and the plan of the run stay as they are; only the runner is written again.
    async fn reopen(&self, session: &Session) -> Result<Option<String>, String> {
        let agent = session
            .found
            .as_ref()
            .map(|found| found.agent_session_id.clone())
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "the session has no agent to resume".to_owned())?;
        let cwd = session
            .agent_dir()
            .ok_or_else(|| "the session has no directory of its own".to_owned())?;
        let files = session
            .agent_files()
            .ok_or_else(|| "the session has no files of its own".to_owned())?;
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let (kind, bash) = self
            .machine
            .runner()
            .ok_or_else(|| "no pwsh or Git Bash".to_owned())?;
        std::fs::create_dir_all(&files).map_err(|err| err.to_string())?;
        let _ = std::fs::remove_file(files.join(launch::EXIT_CODE_FILE));
        let _ = std::fs::remove_file(files.join(launch::RUNNER_PID_FILE));
        let name = launch::session_name(&session.assignment.task_ref, &session.assignment.title);
        let model = provider::launch_model(session.provider, session.assignment.model.as_deref());
        let command = session.provider.command(&name, model, Some(&agent));
        let script = launch::runner_script(kind, &files, &cwd, &command, false);
        let runner = files.join(kind.file_name());
        std::fs::write(&runner, script).map_err(|err| err.to_string())?;
        self.prepare_workspace(session.provider, &cwd);
        self.host
            .open_tab(TabSpec {
                cwd,
                title: name,
                command: launch::tab_command(kind, &runner, bash.as_deref()),
                shell: shell_of(kind),
            })
            .await
    }

    /// Types the owner's messages into the tab of the agent running now. The tab is looked for
    /// before every message ([`Self::find_pane`]): a session picked up after a restart of Ward
    /// has no tab of this run, and the tab an update relaunch reopened, or the one the agent
    /// still runs in, is its tab again. Without any such tab the card says the message was not
    /// delivered instead of swallowing it.
    async fn send_messages(&self, session: &mut Session) {
        while let Some(text) = session.messages.pop_front() {
            let pane = self.find_pane(session).await;
            let delivered = match &pane {
                Some(pane) => match self.host.send_text(pane, &text).await {
                    Ok(()) => true,
                    Err(err) => {
                        log::warn!("Ward agents: could not type a message into the session: {err}");
                        false
                    }
                },
                None => {
                    log::warn!("Ward agents: a message for a session without a tab");
                    false
                }
            };
            if delivered {
                continue;
            }
            let status = session.status;
            if is_reportable(status) {
                let mut request = session.status_request(status);
                request.reason = session.reason.clone();
                request.message = Some(MESSAGE_NO_TAB.to_owned());
                session.set_status(request);
            }
        }
    }

    fn journal_due(&self, session: &Session) -> bool {
        session
            .last_journal_read
            .is_none_or(|at| at.elapsed() >= self.timing.journal_every)
    }

    /// Ends the processes of the agent running now.
    fn stop_agent(&self, session: &mut Session) {
        let pids = session.agent_pids();
        if !pids.is_empty() {
            self.machine.stop(&pids);
        }
    }

    /// Whether the transcript of the agent running now has stood still for
    /// [`Timing::idle_after_answer`]. A launch that never wrote a line is not quiet but silent,
    /// and [`is_silent`] answers for that one.
    fn gone_quiet(&self, session: &Session) -> bool {
        session.journal.has_lines()
            && session
                .transcript_grew_at
                .is_some_and(|at| at.elapsed() >= self.timing.idle_after_answer)
    }

    /// The agent gave its closing answer and went quiet without ending its process. The work
    /// looks done, but the session is not over: only the owner ends it. The agent and its tab
    /// stay as they are, so a `message` command still reaches the tab, the agent can go on by
    /// itself (a background job it was waiting for), and the next lines of the transcript put
    /// the session back to `running` (see [`Self::read_journal`]). The slot of the machine is
    /// free meanwhile: a session waiting for the owner does not hold one.
    fn answered(&self, session: &mut Session) {
        if session.status == SessionStatus::Review {
            return;
        }
        log::info!(
            "Ward agents: session {} answered and waits for the owner",
            session.assignment.session_id
        );
        let mut request = session.status_request(SessionStatus::Review);
        request.message = Some(MESSAGE_IDLE_ANSWER.to_owned());
        session.set_status(request);
    }

    /// The agent ended its turn with a question for the owner: the session is not over, it
    /// calls the owner. It stays where it is, so a `message` command still reaches the tab, and
    /// the next lines of the transcript put it back to `running` (see [`Self::read_journal`]).
    fn ask_owner(&self, session: &mut Session) {
        if session.status == SessionStatus::Blocked {
            return;
        }
        log::info!(
            "Ward agents: session {} waits for an answer of the owner",
            session.assignment.session_id
        );
        let mut request = session.status_request(SessionStatus::Blocked);
        request.reason = Some(REASON_OWNER_QUESTION.to_owned());
        request.message = Some(MESSAGE_OWNER_QUESTION.to_owned());
        session.set_status(request);
    }

    /// The agent ended its turn on an error of its API and waits at an empty prompt: there is
    /// no answer to review, so the session fails with the error in its message, and its agent
    /// is ended, as for a session that never started. A refusal for the subscription limit is
    /// left to [`Self::read_journal`], which has made the session `blocked` until the reset.
    /// `true`: the session is over.
    fn turn_failed(&self, session: &mut Session, error: &str) -> bool {
        if session
            .provider
            .limit_hit(&session.journal.tail())
            .is_some()
        {
            return false;
        }
        log::warn!(
            "Ward agents: session {} ended its turn on an error of the API: {error}",
            session.assignment.session_id
        );
        self.stop_agent(session);
        let mut request = session.status_request(SessionStatus::Failed);
        request.message = Some(agent_error_message(error));
        finish(session, request);
        true
    }

    /// A launched session: finds the agent session, ships the transcript, handles stop and
    /// message commands and notices the exit — the exit code, the runner that went, a stop that
    /// ran out of grace, or a transcript that stood still after the agent's last turn.
    async fn watch(&self, session: &mut Session, launched_at_ms: i64, launched_at: Instant) {
        let Some(worktree) = session.worktree.clone() else {
            return;
        };
        let agents = launch::agents_dir(&worktree);
        self.track_agent(
            session,
            &agents,
            &worktree,
            launched_at_ms,
            launched_at,
            ROLE_EXECUTOR,
        );

        self.send_messages(session).await;

        // Picked up after a restart of Ward without its agent: it is waited for, because the
        // tab of an update relaunch brings it back a moment later.
        if session.restarted {
            // The owner does not have to wait for that: a stop ends the session now.
            if let Some((reason, _)) = session.stop {
                self.stop_agent(session);
                self.read_journal(session);
                let mut request = session.status_request(SessionStatus::Failed);
                request.message = Some(reason.to_owned());
                finish(session, request);
                return;
            }
            if self.wait_for_agent(
                session,
                &worktree,
                launched_at_ms,
                launched_at,
                ROLE_EXECUTOR,
            ) {
                return;
            }
            if session.restarted {
                if self.journal_due(session) {
                    self.read_journal(session);
                }
                return;
            }
        }

        let max_duration = Duration::from_secs(session.assignment.max_duration_min * 60);
        // The limit measures work, not waiting: a session that answered or asked the owner
        // stands still until the owner comes, and ending it by the clock loses that answer.
        let waits_for_owner = matches!(
            session.status,
            SessionStatus::Review | SessionStatus::Blocked
        );
        if session.stop.is_none()
            && !waits_for_owner
            && session.assignment.max_duration_min > 0
            && session.started.elapsed() >= max_duration
        {
            session.stop = Some((MESSAGE_TOO_LONG, None));
        }
        if let Some((reason, None)) = session.stop {
            log::info!(
                "Ward agents: stopping session {}",
                session.assignment.session_id
            );
            self.stop_agent(session);
            session.stop = Some((reason, Some(Instant::now())));
        }

        if self.journal_due(session) {
            self.read_journal(session);
        }

        if let Some(code) = read_exit_code(&agents) {
            self.end(session, Some(code));
            return;
        }
        if session.stop.is_none()
            && is_silent(
                session,
                launched_at.elapsed(),
                self.timing.session_start_grace,
            )
        {
            log::warn!(
                "Ward agents: session {} never started",
                session.assignment.session_id
            );
            self.stop_agent(session);
            let mut request = session.status_request(SessionStatus::Failed);
            request.message = Some(MESSAGE_NOT_STARTED.to_owned());
            finish(session, request);
            return;
        }
        if session.stop.is_none() && self.gone_quiet(session) {
            // The transcript is read to the end before anything is concluded from it: an answer
            // that landed between two ticks counts, and the agent's last word reaches the
            // service whichever way the session goes.
            self.read_journal(session);
            let turn = session.provider.last_turn(&session.journal.tail());
            if self.gone_quiet(session) {
                match turn {
                    LastTurn::Answered => self.answered(session),
                    LastTurn::Asked => self.ask_owner(session),
                    LastTurn::Failed(error) => {
                        if self.turn_failed(session, &error) {
                            return;
                        }
                    }
                    LastTurn::Working => {}
                }
            }
        }
        let runner_gone = session
            .runner_pid
            .is_some_and(|pid| !self.machine.is_alive(pid));
        if runner_gone {
            let since = *session.runner_gone_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= self.timing.exit_grace {
                self.end(session, None);
                return;
            }
        } else {
            session.runner_gone_since = None;
        }
        if let Some((_, Some(since))) = session.stop
            && since.elapsed() >= self.timing.stop_grace
        {
            self.end(session, None);
        }
    }

    /// Reads new transcript lines and moves between `running` and `blocked` when the agent
    /// hits or leaves its subscription limit.
    fn read_journal(&self, session: &mut Session) {
        session.last_journal_read = Some(Instant::now());
        let Some(path) = session.transcript.clone() else {
            return;
        };
        let added = match session.journal.read_from(&path) {
            Ok(added) => added,
            Err(err) => {
                log::debug!("Ward agents: transcript not readable yet: {err}");
                false
            }
        };
        if added {
            session.transcript_grew_at = Some(Instant::now());
        }
        if !added || session.found.is_none() {
            return;
        }
        let hit = session.provider.limit_hit(&session.journal.tail());
        match (session.status, hit) {
            (SessionStatus::Running, Some(hit)) => {
                let mut request = session.status_request(SessionStatus::Blocked);
                request.reason = Some(REASON_USAGE_LIMIT.to_owned());
                request.resets_at = hit.resets_at.map(provider::rfc3339);
                request.message = Some(MESSAGE_USAGE_LIMIT.to_owned());
                session.set_status(request);
            }
            // An orchestrator that got its answer goes back to preparing: the service takes
            // the plan only from there.
            (SessionStatus::Blocked | SessionStatus::Review, None)
                if matches!(session.phase, Phase::Prepare { .. }) =>
            {
                let mut request = session.status_request(SessionStatus::Preparing);
                request.message = Some(MESSAGE_PREPARING.to_owned());
                session.set_status(request);
            }
            (SessionStatus::Blocked | SessionStatus::Review, None) => {
                let mut request = session.status_request(SessionStatus::Running);
                request.message = Some(MESSAGE_RUNNING.to_owned());
                session.set_status(request);
            }
            _ => {}
        }
    }

    /// The agent exited (`code`) or vanished (`None`): the final status.
    fn end(&self, session: &mut Session, code: Option<i32>) {
        self.read_journal(session);
        let mut request = if let Some((reason, _)) = session.stop {
            let mut request = session.status_request(SessionStatus::Failed);
            request.message = Some(reason.to_owned());
            request
        } else if code == Some(0) {
            session.status_request(SessionStatus::Review)
        } else if let Some(hit) = session.provider.limit_hit(&session.journal.tail()) {
            let mut request = session.status_request(SessionStatus::Blocked);
            request.reason = Some(REASON_USAGE_LIMIT.to_owned());
            request.resets_at = hit.resets_at.map(provider::rfc3339);
            request.message = Some(MESSAGE_USAGE_LIMIT.to_owned());
            request
        } else {
            let mut request = session.status_request(SessionStatus::Failed);
            if code.is_none() {
                request.message = Some(MESSAGE_NO_EXIT_CODE.to_owned());
            }
            request
        };
        request.exit_code = code;
        log::info!(
            "Ward agents: session {} ended with {:?} ({})",
            session.assignment.session_id,
            code,
            request.status.as_str()
        );
        finish(session, request);
    }

    /// Sends the transcript chunks, then the plan, then the statuses, in order; stops at the
    /// first failure. A request the service refuses outright (an older service that does not
    /// know this status or the plan) is dropped instead of holding back everything behind it.
    async fn deliver(&self, session: &mut Session) {
        if session.outbox.is_empty()
            && session.plan_outbox.is_none()
            && !session.journal.has_pending()
        {
            return;
        }
        let Some((base_url, token)) = self.credentials() else {
            return;
        };
        let api = Api {
            client: &self.client,
            base_url: &base_url,
        };
        let session_id = session.assignment.session_id.clone();
        while let Some(chunk) = session.journal.next_pending() {
            let seq = chunk.seq;
            match api
                .journal(&token, &session_id, seq, chunk.bytes.clone())
                .await
            {
                Ok(()) => session.journal.mark_sent(seq),
                Err(err) => {
                    log::info!(
                        "Ward agents: journal chunk {seq} not sent: {}",
                        err.message()
                    );
                    if err == ApiError::Unauthorized {
                        self.invalidate(&token).await;
                    }
                    if err.is_refused() {
                        session.journal.mark_sent(seq);
                        continue;
                    }
                    return;
                }
            }
        }
        // The plan goes before the status that follows it: the service wants the plan of a
        // session it still sees as preparing.
        if let Some(plan) = session.plan_outbox.clone() {
            match api.plan(&token, &session_id, &plan).await {
                Ok(()) => session.plan_outbox = None,
                Err(err) => {
                    log::info!("Ward agents: plan not sent: {}", err.message());
                    if err == ApiError::Unauthorized {
                        self.invalidate(&token).await;
                    }
                    if err.is_refused() {
                        session.plan_outbox = None;
                    } else {
                        return;
                    }
                }
            }
        }
        while let Some(request) = session.outbox.front() {
            match api.status(&token, &session_id, request).await {
                Ok(()) => {
                    session.outbox.pop_front();
                }
                Err(err) => {
                    log::info!("Ward agents: status not sent: {}", err.message());
                    if err == ApiError::Unauthorized {
                        self.invalidate(&token).await;
                    }
                    if err.is_refused() {
                        session.outbox.pop_front();
                        continue;
                    }
                    return;
                }
            }
        }
    }

    /// Applies the machine profile whenever the version the service reports differs from the
    /// one Ward applied, checks the proxy and reports both. Without a key it takes Ward's own
    /// entries off the machine.
    async fn profile_loop(self: Arc<Self>) {
        loop {
            sleep(self.timing.profile_every).await;
            let (configured, connection) = {
                let state = self.state.lock();
                (state.config.is_some(), state.connection.clone())
            };
            if !configured || matches!(connection, Connection::KeyRejected(_)) {
                self.forget_profile().await;
                continue;
            }
            let Some((base_url, token)) = self.credentials() else {
                continue;
            };
            let (current, applied, body) = {
                let state = self.state.lock();
                (
                    state.profile_version,
                    state.profile.as_ref().map(|status| status.version),
                    state.profile_body.clone(),
                )
            };
            if current == 0 {
                // The service knows no machine profile: nothing to apply.
                continue;
            }
            if applied == Some(current) {
                // A version that failed is not retried by itself: the DNS proxy asks for
                // administrator rights once, and a prompt every few minutes would be worse
                // than the machine staying "without a proxy" until the owner edits the
                // profile or restarts Ward.
                self.refresh_proxy().await;
                continue;
            }
            let api = Api {
                client: &self.client,
                base_url: &base_url,
            };
            let body = match body.filter(|body| body.version == current) {
                Some(body) => body,
                None => match api.profile(&token).await {
                    Ok(body) => body,
                    Err(err) => {
                        log::info!("Ward agents: profile not taken: {}", err.message());
                        if err == ApiError::Unauthorized {
                            self.invalidate(&token).await;
                        }
                        continue;
                    }
                },
            };
            let version = body.version;
            log::info!("Ward agents: applying the machine profile version {version}");
            let outcome = self.apply_profile(body).await;
            let proxy = self.check_proxy_of(&outcome).await;
            let status = ProfileStatus {
                version,
                ok: outcome.ok,
                message: outcome.message.clone(),
                parts: outcome.parts.clone(),
                proxy: Some(proxy.clone()),
                proxy_required: outcome.proxy_required,
                at: Some(Utc::now()),
            };
            {
                let mut state = self.state.lock();
                state.profile = Some(status);
                state.profile_body = None;
                state.vault = outcome.vault.clone();
                state.proxy_url = outcome.check_url.clone();
                state.profile_checked_at = Some(Instant::now());
            }
            self.publish().await;
            let report = AppliedRequest {
                version,
                ok: outcome.ok,
                message: outcome.message,
                parts: outcome.parts,
                proxy_check: proxy,
            };
            match api.profile_applied(&token, &report).await {
                Ok(()) => {}
                Err(err) => {
                    log::info!("Ward agents: profile report not sent: {}", err.message());
                    if err == ApiError::Unauthorized {
                        self.invalidate(&token).await;
                    }
                }
            }
        }
    }

    /// Applies the profile off the loops: files, git and possibly one elevation request.
    async fn apply_profile(&self, body: ProfileBody) -> profile::Outcome {
        let version = body.version;
        let target = self.profile_target.clone();
        let machine = self.machine.clone();
        let slot: Arc<Mutex<Option<profile::Outcome>>> = Arc::default();
        let thread_slot = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("ward-agents-profile".to_owned())
            .spawn(move || {
                *thread_slot.lock() = Some(profile::apply(&body, &target, machine.as_ref()));
            });
        if let Err(err) = spawned {
            return profile::Outcome {
                version,
                ok: false,
                message: format!("не удалось применить профиль: {err}"),
                ..Default::default()
            };
        }
        loop {
            if let Some(outcome) = slot.lock().take() {
                return outcome;
            }
            sleep(self.timing.tick).await;
        }
    }

    /// The proxy check after applying: by `checkUrl` when the profile wants a proxy.
    async fn check_proxy_of(&self, outcome: &profile::Outcome) -> ProxyCheck {
        match (&outcome.check_url, outcome.proxy_required) {
            (Some(url), _) => self.check_proxy(url).await,
            (None, true) => ProxyCheck {
                ok: false,
                message: "в части dnsProxy нет checkUrl: проверить нечем".to_owned(),
            },
            (None, false) => ProxyCheck {
                ok: true,
                message: "прокси не требуется".to_owned(),
            },
        }
    }

    /// One request through whatever the machine resolves and routes now.
    async fn check_proxy(&self, url: &str) -> ProxyCheck {
        let request = self
            .client
            .get(url.to_owned())
            .timeout(Duration::from_secs(10));
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                ProxyCheck {
                    ok: status < 500,
                    message: format!("HTTP {status}"),
                }
            }
            Err(err) => ProxyCheck {
                ok: false,
                message: err.without_url().to_string(),
            },
        }
    }

    /// Checks the proxy again when the last check got old: the heartbeat is what the scheduler
    /// looks at.
    async fn refresh_proxy(&self) {
        let (due, url) = {
            let state = self.state.lock();
            let required = state
                .profile
                .as_ref()
                .is_some_and(|status| status.proxy_required);
            let stale = state
                .profile_checked_at
                .is_none_or(|at| at.elapsed() >= self.timing.proxy_every);
            (required && stale, state.proxy_url.clone())
        };
        let Some(url) = url.filter(|_| due) else {
            return;
        };
        let proxy = self.check_proxy(&url).await;
        {
            let mut state = self.state.lock();
            if let Some(status) = state.profile.as_mut() {
                status.proxy = Some(proxy);
                status.at = Some(Utc::now());
            }
            state.profile_checked_at = Some(Instant::now());
        }
        self.publish().await;
    }

    /// The key is gone or was refused: Ward's own MCP servers, settings, skills and `hosts`
    /// block come off the machine, once.
    async fn forget_profile(&self) {
        {
            let state = self.state.lock();
            if state.managed_removed {
                return;
            }
        }
        let target = self.profile_target.clone();
        if !profile::has_managed(&target) {
            self.state.lock().managed_removed = true;
            return;
        }
        log::info!("Ward agents: no key any more, taking Ward's own settings off the machine");
        let machine = self.machine.clone();
        let slot: Arc<Mutex<Option<profile::Outcome>>> = Arc::default();
        let thread_slot = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("ward-agents-profile-off".to_owned())
            .spawn(move || {
                *thread_slot.lock() = Some(profile::remove_managed(&target, machine.as_ref()));
            });
        if spawned.is_err() {
            return;
        }
        let outcome = loop {
            if let Some(outcome) = slot.lock().take() {
                break outcome;
            }
            sleep(self.timing.tick).await;
        };
        if !outcome.ok {
            log::warn!("Ward agents: not everything came off: {}", outcome.message);
        }
        {
            let mut state = self.state.lock();
            state.managed_removed = true;
            state.profile = None;
            state.profile_body = None;
            state.vault = None;
        }
        self.publish().await;
    }
}

fn finish(session: &mut Session, request: StatusRequest) {
    session.set_status(request);
    session.phase = Phase::Done;
}

/// The message of a session whose agent ended its turn on an error of the API, with the error.
fn agent_error_message(error: &str) -> String {
    let error = provider::one_line(error, MAX_AGENT_ERROR_CHARS);
    if error.is_empty() {
        MESSAGE_AGENT_ERROR.to_owned()
    } else {
        format!("{MESSAGE_AGENT_ERROR}: {error}")
    }
}

/// Whether the service takes this status from a worker (`FROM_WORKER` of the contract): a
/// session that has not started yet has nothing to report.
fn is_reportable(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Preparing
            | SessionStatus::PlanReview
            | SessionStatus::Running
            | SessionStatus::Blocked
    )
}

/// Saves the state of a session still going, and drops the state of one that is over, so the
/// next start of Ward finds exactly the sessions it has to pick up. An unchanged session is
/// not written again.
fn save_session_state(session: &mut Session, root: &Path) {
    let Some(path) = resume::path(root, &session.assignment.session_id) else {
        return;
    };
    if session.is_done() {
        if session.saved_state.take().is_some() {
            resume::remove(&path);
        }
        return;
    }
    let state = session.saved();
    let Ok(text) = serde_json::to_string(&state) else {
        return;
    };
    if session.saved_state.as_deref() == Some(text.as_str()) {
        return;
    }
    match resume::write(&path, &state) {
        Ok(()) => session.saved_state = Some(text),
        Err(err) => log::warn!(
            "Ward agents: could not save the state of session {}: {err}",
            session.assignment.session_id
        ),
    }
}

/// The pid the runner left in `dir`, when it is there and readable.
fn read_runner_pid(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join(launch::RUNNER_PID_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The exit code the runner left in `dir`, when it is there and readable.
fn read_exit_code(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(dir.join(launch::EXIT_CODE_FILE))
        .ok()
        .and_then(|text| launch::parse_exit_code(&text))
}

/// The text of `plan.json`, when it is there and not absurdly large.
fn read_plan_file(path: &Path) -> Option<String> {
    let size = std::fs::metadata(path).ok()?.len();
    if size == 0 || size > plan::MAX_PLAN_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn apply_command(sessions: &mut [Session], command: Command) {
    let session_id = command.session_id().to_owned();
    let Some(session) = sessions
        .iter_mut()
        .find(|session| session.assignment.session_id == session_id)
    else {
        log::info!("Ward agents: a command for an unknown session");
        return;
    };
    match command {
        Command::Stop { .. } => {
            if session.stop.is_none() && !session.is_done() {
                session.stop = Some((MESSAGE_STOPPED, None));
            }
        }
        Command::Message { text, .. } => {
            if !text.trim().is_empty() && !session.is_done() {
                session.messages.push_back(text);
            }
        }
        // The plan reaches the service while the orchestrator is still ending, so the owner's
        // answer may come before the session asks for it: it is kept for `plan-review`.
        Command::Approve { .. } => {
            if matches!(session.phase, Phase::PlanReview | Phase::PrepareExit { .. }) {
                session.approval = Some(Ok(()));
            } else {
                log::info!("Ward agents: approve for a session that waits for nothing");
            }
        }
        Command::Reject { reason, .. } => {
            if matches!(session.phase, Phase::PlanReview | Phase::PrepareExit { .. }) {
                session.approval = Some(Err(reason));
            } else {
                log::info!("Ward agents: reject for a session that waits for nothing");
            }
        }
        // Also for a session that is over: its tab is what the owner goes on talking in.
        Command::Show { remote, .. } => session.show = Some(ShowRequest { remote }),
        Command::Stream { .. } => session.stream = true,
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
