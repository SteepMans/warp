//! The worker itself: three loops on a background executor, sharing [`Worker`].
//!
//! - **Control**: reads the settings, registers (again after a 401 or a changed key), sends a
//!   heartbeat every `heartbeatSec` with the load, the subscription usage and the session
//!   statuses, backing off while the service is unreachable.
//! - **Inbox**: long-polls `GET /inbox`, acks or refuses assignments and queues commands.
//! - **Sessions**: moves every session through its phases (worktree, tab, agent session found,
//!   exit code), ships the transcript every [`Timing::journal_every`] and sends statuses. What
//!   could not be sent waits and goes out with the next tick.
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

use super::admission::{self, MachineState};
use super::client::{Api, ApiError};
use super::contract::{
    AckRequest, AgentInfo, Assignment, Command, HeartbeatRequest, Limits, Passport,
    REASON_USAGE_LIMIT, RegisterRequest, Resources, SessionBeat, SessionStatus, StatusRequest,
    Tools, UsageReport,
};
use super::git::{self, WorktreeSpec};
use super::journal::Journal;
use super::launch::{self, RunnerKind};
use super::machine::{self, Machine};
use super::provider::{self, FoundSession, Homes, Provider, WardUsage};

/// What the worker needs from the settings. `Debug` leaves the key out.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    pub base_url: String,
    pub key: String,
    pub name: String,
    pub root: PathBuf,
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
    /// Types `text` into the agent of the pane and submits it.
    async fn send_text(&self, pane_uuid: &str, text: &str) -> Result<(), String>;
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
    /// The runner is gone but wrote no exit code: the session ends after this.
    pub exit_grace: Duration,
    /// After a stop, the session ends after this even without an exit code.
    pub stop_grace: Duration,
    pub usage_every: Duration,
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
            exit_grace: Duration::from_secs(15),
            stop_grace: Duration::from_secs(30),
            usage_every: Duration::from_secs(60),
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
}

/// Messages in session statuses; they end up in the card's comments.
const MESSAGE_RUNNING: &str = "Сессия запущена";
const MESSAGE_STOPPED: &str = "Остановлено владельцем";
const MESSAGE_TOO_LONG: &str = "Остановлено: превышено maxDurationMin";
const MESSAGE_NO_EXIT_CODE: &str = "Агент завершился без кода выхода (вкладку закрыли?)";
const MESSAGE_NO_SESSION_ID: &str = "Сессия запущена, id сессии агента пока не найден";
const MESSAGE_USAGE_LIMIT: &str = "Агент упёрся в лимит подписки";

/// Finished sessions the panel keeps.
const MAX_FINISHED_SESSIONS: usize = 20;

/// Result of preparing the worktree, filled by the git thread.
type PrepareSlot = Arc<Mutex<Option<Result<PathBuf, String>>>>;

enum Phase {
    Preparing(PrepareSlot),
    Launched { launched_at_ms: i64, at: Instant },
    Done,
}

struct Session {
    assignment: Assignment,
    provider: &'static dyn Provider,
    phase: Phase,
    status: SessionStatus,
    worktree: Option<PathBuf>,
    found: Option<FoundSession>,
    transcript: Option<PathBuf>,
    pane_uuid: Option<String>,
    runner_pid: Option<u32>,
    journal: Journal,
    last_journal_read: Option<Instant>,
    outbox: VecDeque<StatusRequest>,
    messages: VecDeque<String>,
    stop: Option<(&'static str, Option<Instant>)>,
    runner_gone_since: Option<Instant>,
    started: Instant,
    message: Option<String>,
}

impl Session {
    fn new(assignment: Assignment, provider: &'static dyn Provider, slot: PrepareSlot) -> Self {
        Self {
            assignment,
            provider,
            phase: Phase::Preparing(slot),
            status: SessionStatus::Assigned,
            worktree: None,
            found: None,
            transcript: None,
            pane_uuid: None,
            runner_pid: None,
            journal: Journal::default(),
            last_journal_read: None,
            outbox: VecDeque::new(),
            messages: VecDeque::new(),
            stop: None,
            runner_gone_since: None,
            started: Instant::now(),
            message: None,
        }
    }

    fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// Done and everything about it delivered.
    fn is_settled(&self) -> bool {
        self.is_done() && self.outbox.is_empty() && !self.journal.has_pending()
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
        request
    }

    /// Records a new status and queues it for the service.
    fn set_status(&mut self, request: StatusRequest) {
        self.status = request.status;
        self.message = request.message.clone();
        self.outbox.push_back(request);
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
        }
    }
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
    commands: Vec<Command>,
    beats: Vec<SessionBeat>,
    views: Vec<SessionView>,
    resources: Option<Resources>,
    agents: BTreeMap<String, AgentInfo>,
    usage: Vec<UsageReport>,
    usage_at: Option<Instant>,
    last_heartbeat: Option<DateTime<Utc>>,
    rejected_fingerprint: Option<String>,
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
    timing: Timing,
    ward_version: String,
    state: Mutex<State>,
    register_lock: futures::lock::Mutex<()>,
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
        timing: Timing,
        ward_version: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            machine,
            client,
            homes,
            timing,
            ward_version,
            state: Mutex::new(State {
                heartbeat_secs: super::contract::DEFAULT_HEARTBEAT_SECS,
                ..Default::default()
            }),
            register_lock: futures::lock::Mutex::new(()),
        })
    }

    /// Runs the three loops; never returns.
    pub async fn run(self: Arc<Self>) {
        futures::join!(
            self.clone().control_loop(),
            self.clone().inbox_loop(),
            self.clone().sessions_loop()
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

    /// Statuses for the sessions still going, for a last report when Ward quits.
    pub fn shutdown_report(&self) -> Option<ShutdownReport> {
        let (base_url, token) = self.credentials()?;
        let state = self.state.lock();
        let reports: Vec<(String, StatusRequest)> = state
            .sessions
            .iter()
            .filter(|session| !session.is_done())
            .map(|session| {
                let mut request = session.status_request(SessionStatus::Failed);
                request.message = Some("Ward закрыт, сессия прервана".to_owned());
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

    fn passport(&self, root: &Path) -> Passport {
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
            repos: machine::scan_repos(root),
        }
    }

    /// Registers unless a registration for the current settings exists (in memory or saved).
    async fn ensure_registered(&self, config: &WorkerConfig) -> Result<(), ApiError> {
        let _guard = self.register_lock.lock().await;
        let fingerprint = config.fingerprint();
        if self.state.lock().registration.is_some() {
            return Ok(());
        }
        if let Some(saved) = self.host.load_registration().await
            && saved.fingerprint == fingerprint
        {
            let mut state = self.state.lock();
            state.registration = Some(saved);
            return Ok(());
        }
        let passport = self.passport(&config.root);
        self.state.lock().agents = passport.agents.clone();
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
        };
        {
            let mut state = self.state.lock();
            state.registration = Some(registration.clone());
            state.limits = response.limits.clone();
            state.heartbeat_secs = response.heartbeat_sec.max(1);
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
        let sessions = self.state.lock().beats.clone();
        let request = HeartbeatRequest {
            resources: resources.clone(),
            usage,
            sessions,
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
                sleep(delay).await;
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

    /// Acks or refuses an assignment; an accepted one starts preparing its worktree.
    async fn take_assignment(&self, api: &Api<'_>, token: &str, assignment: Assignment) {
        let session_id = assignment.session_id.clone();
        if session_id.is_empty() {
            return;
        }
        let duplicate = self.state.lock().known_ids.contains(&session_id);
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
        let (provider, root, repo) = match decision {
            Ok(decision) => decision,
            Err(reason) => {
                log::info!("Ward agents: refused an assignment: {reason}");
                return;
            }
        };
        log::info!(
            "Ward agents: took session {session_id} ({}, {repo})",
            provider.id()
        );
        let slot: PrepareSlot = Arc::default();
        let thread_slot = slot.clone();
        let repo_url = assignment.repo.clone();
        let base_branch = assignment.base_branch.clone();
        let branch = assignment.branch.clone();
        let thread_session_id = session_id.clone();
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
                        session_id: &thread_session_id,
                    },
                );
                *thread_slot.lock() = Some(result);
            });
        if let Err(err) = spawned {
            *slot.lock() = Some(Err(format!("could not start git: {err}")));
        }
        let mut state = self.state.lock();
        state.known_ids.insert(session_id);
        state
            .sessions
            .push(Session::new(assignment, provider, slot));
    }

    /// The provider, root and repository of an assignment this machine takes, or the reason
    /// it does not.
    fn decide(
        &self,
        assignment: &Assignment,
    ) -> Result<(&'static dyn Provider, PathBuf, launch::RepoId), String> {
        let repo = launch::parse_repo(&assignment.repo)
            .ok_or_else(|| "unsupported repository URL".to_owned())?;
        if assignment.prompt.trim().is_empty() {
            return Err("empty prompt".to_owned());
        }
        if self.machine.runner().is_none() {
            return Err("no pwsh or Git Bash to run the agent".to_owned());
        }
        let state = self.state.lock();
        let config = state
            .config
            .clone()
            .ok_or_else(|| "worker is not configured".to_owned())?;
        drop(state);
        let resources = self.machine.resources(&config.root);
        let usage = self.state.lock().usage.clone();
        let state = self.state.lock();
        let active_sessions = state
            .sessions
            .iter()
            .filter(|session| !session.is_done())
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
        admission::check_slot(&machine_state, &repo)?;
        let provider = admission::choose_provider(&assignment.provider, &machine_state)?;
        Ok((provider, config.root.clone(), repo))
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
            for session in &mut sessions {
                self.advance(session).await;
                self.deliver(session).await;
            }
            {
                let mut state = self.state.lock();
                sessions.append(&mut state.sessions);
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
            Phase::Preparing(slot) => {
                let result = slot.lock().take();
                match result {
                    None => {}
                    Some(Err(err)) => {
                        log::warn!("Ward agents: preparing the worktree failed: {err}");
                        let mut request = session.status_request(SessionStatus::Failed);
                        request.message = Some(format!("Не удалось развернуть репозиторий: {err}"));
                        finish(session, request);
                    }
                    Some(Ok(worktree)) => {
                        session.worktree = Some(worktree.clone());
                        if let Some((reason, _)) = session.stop {
                            let mut request = session.status_request(SessionStatus::Failed);
                            request.message = Some(reason.to_owned());
                            finish(session, request);
                            return;
                        }
                        match self.launch(session, &worktree).await {
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

    /// Writes the session files and opens its tab.
    async fn launch(&self, session: &Session, worktree: &Path) -> Result<Option<String>, String> {
        let (kind, bash) = self
            .machine
            .runner()
            .ok_or_else(|| "no pwsh or Git Bash".to_owned())?;
        let agents = launch::agents_dir(worktree);
        std::fs::create_dir_all(&agents).map_err(|err| err.to_string())?;
        std::fs::write(agents.join(launch::PROMPT_FILE), &session.assignment.prompt)
            .map_err(|err| err.to_string())?;
        let _ = std::fs::remove_file(agents.join(launch::EXIT_CODE_FILE));
        let _ = std::fs::remove_file(agents.join(launch::RUNNER_PID_FILE));
        let name = launch::session_name(&session.assignment.task_ref, &session.assignment.title);
        let model = session
            .assignment
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty());
        let command = session.provider.command(&name, model);
        let script = launch::runner_script(kind, worktree, &command);
        let runner = agents.join(kind.file_name());
        std::fs::write(&runner, script).map_err(|err| err.to_string())?;
        let shell = match kind {
            RunnerKind::Pwsh if cfg!(windows) => Some("pwsh"),
            RunnerKind::Sh if cfg!(windows) => Some("powershell"),
            RunnerKind::Pwsh | RunnerKind::Sh => None,
        };
        self.host
            .open_tab(TabSpec {
                cwd: worktree.to_path_buf(),
                title: name,
                command: launch::tab_command(kind, &runner, bash.as_deref()),
                shell,
            })
            .await
    }

    /// A launched session: finds the agent session, ships the transcript, handles stop and
    /// message commands and notices the exit.
    async fn watch(&self, session: &mut Session, launched_at_ms: i64, launched_at: Instant) {
        let Some(worktree) = session.worktree.clone() else {
            return;
        };
        let agents = launch::agents_dir(&worktree);
        if session.runner_pid.is_none() {
            session.runner_pid = std::fs::read_to_string(agents.join(launch::RUNNER_PID_FILE))
                .ok()
                .and_then(|text| text.trim().parse().ok());
        }

        if session.found.is_none()
            && let Some(found) =
                session
                    .provider
                    .find_session(&self.homes, &worktree, launched_at_ms)
        {
            log::info!(
                "Ward agents: session {} runs agent session {}",
                session.assignment.session_id,
                found.agent_session_id
            );
            session.found = Some(found);
            let mut request = session.status_request(SessionStatus::Running);
            request.message = Some(MESSAGE_RUNNING.to_owned());
            session.set_status(request);
        }
        if session.found.is_none()
            && session.status == SessionStatus::Assigned
            && launched_at.elapsed() >= self.timing.session_lookup_grace
        {
            let mut request = session.status_request(SessionStatus::Running);
            request.message = Some(MESSAGE_NO_SESSION_ID.to_owned());
            session.set_status(request);
        }
        if session.transcript.is_none()
            && let Some(found) = &session.found
        {
            session.transcript = session.provider.transcript(&self.homes, &worktree, found);
        }

        while let Some(text) = session.messages.pop_front() {
            match &session.pane_uuid {
                Some(pane) => {
                    if let Err(err) = self.host.send_text(pane, &text).await {
                        log::warn!("Ward agents: could not type a message into the session: {err}");
                    }
                }
                None => log::warn!("Ward agents: a message for a session without a tab"),
            }
        }

        let max_duration = Duration::from_secs(session.assignment.max_duration_min * 60);
        if session.stop.is_none()
            && session.assignment.max_duration_min > 0
            && session.started.elapsed() >= max_duration
        {
            session.stop = Some((MESSAGE_TOO_LONG, None));
        }
        if let Some((reason, None)) = session.stop {
            let mut pids: Vec<u32> = Vec::new();
            match session.found.as_ref().and_then(|found| found.pid) {
                // The agent only: the runner then still writes the exit code.
                Some(pid) => pids.push(pid),
                None => pids.extend(session.runner_pid),
            }
            log::info!(
                "Ward agents: stopping session {}",
                session.assignment.session_id
            );
            self.machine.stop(&pids);
            session.stop = Some((reason, Some(Instant::now())));
        }

        let journal_due = session
            .last_journal_read
            .is_none_or(|at| at.elapsed() >= self.timing.journal_every);
        if journal_due {
            self.read_journal(session);
        }

        let exit_code = std::fs::read_to_string(agents.join(launch::EXIT_CODE_FILE))
            .ok()
            .and_then(|text| launch::parse_exit_code(&text));
        if let Some(code) = exit_code {
            self.end(session, Some(code));
            return;
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
            (SessionStatus::Blocked, None) => {
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

    /// Sends the transcript chunks, then the statuses, in order; stops at the first failure.
    async fn deliver(&self, session: &mut Session) {
        if session.outbox.is_empty() && !session.journal.has_pending() {
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
                    return;
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
                    return;
                }
            }
        }
    }
}

fn finish(session: &mut Session, request: StatusRequest) {
    session.set_status(request);
    session.phase = Phase::Done;
}

fn apply_command(sessions: &mut [Session], command: Command) {
    let session_id = match &command {
        Command::Stop { session_id } | Command::Message { session_id, .. } => session_id,
    };
    let Some(session) = sessions
        .iter_mut()
        .find(|session| &session.assignment.session_id == session_id)
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
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
