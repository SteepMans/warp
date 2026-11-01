//! The whole cycle against a fake service: registration, heartbeat, an accepted and a refused
//! assignment, a worktree from a local remote, a fake agent session, a message, the transcript
//! and the final status.
//!
//! Every test gives its fake service its own path prefix (`/agents/t-…`). The loops of a worker
//! never stop, and the fake servers come from one pool, so a worker of a finished test can get
//! the server of a test that starts later: with its own prefix it matches no mock there and
//! neither takes that test's assignment nor lands in its log.

use std::sync::atomic::{AtomicUsize, Ordering};

use command::blocking::Command as Process;
use mockito::{Matcher, Server, ServerGuard};

use super::*;

const GB: u64 = 1024 * 1024 * 1024;
const AGENT_SESSION_ID: &str = "0b9f4c2e-1111-4a2b-9c3d-5e6f7a8b9c0d";
const BRIDGE_ID: &str = "session_01Abc";

#[test]
fn backoff_doubles_up_to_the_cap() {
    let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(30));
    assert_eq!(backoff.fail(), Duration::from_secs(5));
    assert_eq!(backoff.fail(), Duration::from_secs(10));
    assert_eq!(backoff.fail(), Duration::from_secs(20));
    assert_eq!(backoff.fail(), Duration::from_secs(30));
    assert_eq!(backoff.fail(), Duration::from_secs(30));
    backoff.succeed();
    assert_eq!(backoff.fail(), Duration::from_secs(5));
}

#[test]
fn secrets_stay_out_of_debug() {
    let config = WorkerConfig {
        base_url: "https://example.test/agents".to_owned(),
        key: "agk_secret".to_owned(),
        name: "pc".to_owned(),
        root: PathBuf::from("root"),
        clone_search: Vec::new(),
        paused: false,
    };
    assert!(!format!("{config:?}").contains("agk_secret"));
    let registration = Registration {
        fingerprint: config.fingerprint(),
        worker_id: "w".to_owned(),
        token: "agw_secret".to_owned(),
        repos: None,
    };
    assert!(!format!("{registration:?}").contains("agw_secret"));
    let other_key = WorkerConfig {
        key: "agk_other".to_owned(),
        ..config.clone()
    };
    assert_ne!(config.fingerprint(), other_key.fingerprint());
    assert!(!config.fingerprint().contains("agk"));
}

struct FakeMachine;

impl Machine for FakeMachine {
    fn hardware(&self, _root: &Path) -> machine::Hardware {
        machine::Hardware {
            cpu_cores: 8,
            ram_total_bytes: 32 * GB,
            disk_total_bytes: 500 * GB,
        }
    }

    fn resources(&self, _root: &Path) -> Resources {
        Resources {
            cpu_percent: 5.0,
            ram_free_bytes: 16 * GB,
            disk_free_bytes: 200 * GB,
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        matches!(name, "git" | "claude")
    }

    fn tool_version(&self, name: &str) -> Option<String> {
        Some(format!("{name} 1.0"))
    }

    fn is_alive(&self, _pid: u32) -> bool {
        true
    }

    fn stop(&self, _pids: &[u32]) {}

    fn runner(&self) -> Option<(RunnerKind, Option<PathBuf>)> {
        Some((RunnerKind::Pwsh, None))
    }

    fn write_elevated(&self, path: &Path, contents: &str) -> Result<(), String> {
        std::fs::write(path, contents).map_err(|err| err.to_string())
    }
}

/// Plays the UI and the agent: opening the tab "starts" a Claude session in its directory.
/// Every tab gets its own pid and session id, so the orchestrator of the preparation phase and
/// the executor after it are told apart the way the real agent files tell them apart.
struct FakeHost {
    config: WorkerConfig,
    homes: Homes,
    /// The tab opens but no agent session appears: what a question left on the screen of the
    /// tab looks like from the outside.
    silent: bool,
    registration: Mutex<Option<Registration>>,
    tabs: Mutex<Vec<TabSpec>>,
    typed: Mutex<Vec<(String, String)>>,
    snapshot: Mutex<Snapshot>,
    /// What the orchestrator "writes" as its plan when its tab opens.
    plan: Mutex<Option<(PathBuf, String)>>,
}

impl FakeHost {
    fn new(config: WorkerConfig, homes: Homes) -> Self {
        Self {
            config,
            homes,
            silent: false,
            registration: Mutex::new(None),
            tabs: Mutex::new(Vec::new()),
            typed: Mutex::new(Vec::new()),
            snapshot: Mutex::new(Snapshot::default()),
            plan: Mutex::new(None),
        }
    }

    /// The tab opens and nothing else happens.
    fn silent(mut self) -> Self {
        self.silent = true;
        self
    }

    /// The agent session id of the tab opened as number `index`.
    fn agent_session_id(index: usize) -> String {
        if index == 0 {
            AGENT_SESSION_ID.to_owned()
        } else {
            format!("{}{index}", &AGENT_SESSION_ID[..AGENT_SESSION_ID.len() - 1])
        }
    }
}

#[async_trait]
impl Host for FakeHost {
    async fn config(&self) -> Option<WorkerConfig> {
        Some(self.config.clone())
    }

    async fn load_registration(&self) -> Option<Registration> {
        self.registration.lock().clone()
    }

    async fn store_registration(&self, registration: Option<Registration>) {
        *self.registration.lock() = registration;
    }

    async fn ward_usage(&self) -> WardUsage {
        WardUsage::default()
    }

    async fn open_tab(&self, tab: TabSpec) -> Result<Option<String>, String> {
        let index = {
            let mut tabs = self.tabs.lock();
            tabs.push(tab.clone());
            tabs.len() - 1
        };
        if self.silent {
            return Ok(Some(format!("pane-{}", index + 1)));
        }
        let session_id = Self::agent_session_id(index);
        let pid = 4242 + index as u32;
        let sessions = self.homes.claude_dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let session_file = serde_json::json!({
            "pid": pid,
            "sessionId": session_id,
            "cwd": tab.cwd.to_string_lossy(),
            "startedAt": now_ms(),
            "bridgeSessionId": BRIDGE_ID,
        });
        std::fs::write(
            sessions.join(format!("{pid}.json")),
            session_file.to_string(),
        )
        .unwrap();
        let project = self
            .homes
            .claude_dir
            .join("projects")
            .join(provider::claude::project_slug(&tab.cwd));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{session_id}.jsonl")),
            format!("{{\"type\":\"user\",\"n\":1,\"tab\":{index}}}\n{{\"type\":\"assistant\",\"n\":2}}\n"),
        )
        .unwrap();
        // The orchestrator leaves its plan behind, as the prompt tells it to.
        if let Some((path, text)) = self.plan.lock().take() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, text).unwrap();
        }
        Ok(Some(format!("pane-{}", index + 1)))
    }

    async fn send_text(&self, pane_uuid: &str, text: &str) -> Result<(), String> {
        self.typed
            .lock()
            .push((pane_uuid.to_owned(), text.to_owned()));
        Ok(())
    }

    async fn publish(&self, snapshot: Snapshot) {
        *self.snapshot.lock() = snapshot;
    }
}

type Log = Arc<Mutex<Vec<(String, String)>>>;

/// Records `(path and query, body)` of every request to `path` and answers with `respond`.
fn record(
    server: &mut ServerGuard,
    method: &str,
    path: &str,
    log: &Log,
    respond: impl Fn(&mockito::Request) -> Vec<u8> + Send + Sync + 'static,
) {
    let log = log.clone();
    server
        .mock(method, Matcher::Regex(format!("^{path}")))
        .match_header("authorization", "Bearer agw_token")
        .with_body_from_request(move |request| {
            let body = request
                .body()
                .map(|body| String::from_utf8_lossy(body).into_owned())
                .unwrap_or_default();
            log.lock().push((request.path_and_query().to_owned(), body));
            respond(request)
        })
        .create();
}

fn git(dir: &Path, args: &[&str]) {
    let output = Process::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
}

/// A bare remote at `<dir>/remotes/SteepMans/demo.git` with one commit on `main`.
fn remote(dir: &Path) -> String {
    let seed = dir.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "demo\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    let bare = dir.join("remotes").join("SteepMans").join("demo.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    let output = Process::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .output()
        .unwrap();
    assert!(output.status.success());
    bare.to_string_lossy().into_owned()
}

/// The machine profile is applied to temp files, never to the real home or `hosts` file.
fn profile_target(dir: &Path, homes: &Homes) -> profile::Target {
    profile::Target {
        claude_dir: homes.claude_dir.clone(),
        claude_json: homes.claude_json.clone(),
        hosts: dir.join("system").join("hosts"),
    }
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn statuses(log: &Log) -> Vec<serde_json::Value> {
    log.lock()
        .iter()
        .filter(|(path, _)| path.contains("/s-good/status"))
        .map(|(_, body)| serde_json::from_str(body).unwrap())
        .collect()
}

#[test]
fn full_cycle_against_a_fake_service() {
    if machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo_url = remote(dir.path());
    let root = dir.path().join("agents-work");
    let homes = Homes {
        claude_dir: dir.path().join("home").join(".claude"),
        claude_json: dir.path().join("home").join(".claude.json"),
        codex_dir: dir.path().join("home").join(".codex"),
    };
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    std::fs::write(
        &homes.claude_json,
        r#"{"oauthAccount":{"accountUuid":"u-1","organizationType":"claude_max"}}"#,
    )
    .unwrap();

    let mut server = Server::new();
    let log: Log = Arc::default();
    let base = "/agents/t-cycle/api/v1/worker";
    let registrations = Arc::new(AtomicUsize::new(0));
    {
        let registrations = registrations.clone();
        server
            .mock("POST", format!("{base}/register").as_str())
            .match_body(Matcher::PartialJson(serde_json::json!({
                "key": "agk_test",
                "name": "test-pc",
                "passport": {
                    "cpuCores": 8,
                    "tools": {"git": true, "claude": true, "codex": false},
                    "agents": {"claude": {"installed": true, "loggedIn": true}},
                },
            })))
            .with_body_from_request(move |_| {
                registrations.fetch_add(1, Ordering::SeqCst);
                br#"{"workerId":"w-1","token":"agw_token","heartbeatSec":1}"#.to_vec()
            })
            .create();
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/heartbeat"),
        &log,
        |_| b"{}".to_vec(),
    );
    let assignment = serde_json::json!({
        "sessionId": "s-good",
        "taskRef": {"boardId": 5, "taskId": 4700, "taskNumber": 4700},
        "attempt": 1,
        "title": "Demo task",
        "repo": repo_url,
        "baseBranch": "main",
        "branch": "feature/4700-demo",
        "prompt": "Do the demo task",
        "provider": "claude",
        "model": null,
        "maxDurationMin": 0,
    });
    let refused = serde_json::json!({
        "sessionId": "s-empty",
        "repo": "https://github.com/SteepMans/demo.git",
        "branch": "feature/x",
        "prompt": "  ",
    });
    let inbox_calls = Arc::new(AtomicUsize::new(0));
    {
        let inbox_calls = inbox_calls.clone();
        let first = serde_json::json!({"assignments": [assignment, refused]}).to_string();
        let second = serde_json::json!({
            "commands": [{"type": "message", "sessionId": "s-good", "text": "Also add tests"}],
        })
        .to_string();
        record(
            &mut server,
            "GET",
            &format!("{base}/inbox"),
            &log,
            move |_| {
                match inbox_calls.fetch_add(1, Ordering::SeqCst) {
                    0 => first.clone().into_bytes(),
                    1 => second.clone().into_bytes(),
                    // Not JSON: the worker backs off instead of polling in a tight loop.
                    _ => b"-".to_vec(),
                }
            },
        );
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/assignments/"),
        &log,
        |_| Vec::new(),
    );
    record(
        &mut server,
        "POST",
        &format!("{base}/sessions/"),
        &log,
        |_| Vec::new(),
    );

    let host = Arc::new(FakeHost::new(
        WorkerConfig {
            base_url: format!("{}/agents/t-cycle", server.url()),
            key: "agk_test".to_owned(),
            name: "test-pc".to_owned(),
            root: root.clone(),
            clone_search: Vec::new(),
            paused: false,
        },
        homes.clone(),
    ));
    let timing = Timing {
        tick: Duration::from_millis(100),
        journal_every: Duration::from_millis(100),
        config_every: Duration::from_millis(100),
        inbox_wait_secs: 0,
        session_lookup_grace: Duration::from_secs(30),
        session_start_grace: Duration::from_secs(300),
        exit_grace: Duration::from_secs(30),
        prepare_exit_wait: Duration::from_millis(200),
        stop_grace: Duration::from_secs(30),
        usage_every: Duration::from_secs(60),
        profile_every: Duration::from_millis(100),
        proxy_every: Duration::from_secs(60),
        min_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(1),
    };
    let worker = Worker::new(
        host.clone(),
        Arc::new(FakeMachine),
        http_client::Client::new_for_test(),
        homes.clone(),
        profile_target(dir.path(), &homes),
        timing,
        "test".to_owned(),
    );
    let runner = worker.clone();
    std::thread::spawn(move || futures::executor::block_on(runner.run()));

    // Acks: the good assignment is taken; the one without a prompt needs the preparation
    // phase, and this machine has no vault for it.
    wait_for("both acks", || {
        log.lock()
            .iter()
            .filter(|(path, _)| path.contains("/assignments/"))
            .count()
            >= 2
    });
    let acks: Vec<(String, serde_json::Value)> = log
        .lock()
        .iter()
        .filter(|(path, _)| path.contains("/assignments/"))
        .map(|(path, body)| (path.clone(), serde_json::from_str(body).unwrap()))
        .collect();
    let ack_of = |id: &str| {
        acks.iter()
            .find(|(path, _)| path.contains(&format!("/assignments/{id}/ack")))
            .map(|(_, body)| body.clone())
            .unwrap()
    };
    assert_eq!(ack_of("s-good")["accepted"], true);
    assert_eq!(ack_of("s-empty")["accepted"], false);
    assert_eq!(
        ack_of("s-empty")["reason"],
        "no vault on this worker for the preparation phase"
    );

    // The worktree, the prompt file and the tab.
    wait_for("running", || {
        statuses(&log)
            .iter()
            .any(|status| status["status"] == "running")
    });
    let worktree = root
        .join("SteepMans")
        .join("demo")
        .join(".worktrees")
        .join("s-good");
    let agents = worktree.join(".agents");
    assert_eq!(
        std::fs::read_to_string(agents.join("prompt.md")).unwrap(),
        "Do the demo task"
    );
    assert!(agents.join(RunnerKind::Pwsh.file_name()).is_file());
    {
        let tabs = host.tabs.lock();
        assert_eq!(tabs.len(), 1);
        assert!(super::provider::same_path(
            &tabs[0].cwd.to_string_lossy(),
            &worktree
        ));
        assert!(tabs[0].command.contains("run.ps1"), "{}", tabs[0].command);
    }
    let running = statuses(&log)
        .into_iter()
        .find(|status| status["status"] == "running")
        .unwrap();
    assert_eq!(running["provider"], "claude");
    assert_eq!(running["agentSessionId"], AGENT_SESSION_ID);
    assert_eq!(running["claudeSessionId"], AGENT_SESSION_ID);
    assert_eq!(
        running["remoteControlUrl"],
        format!("https://claude.ai/code/{BRIDGE_ID}")
    );
    assert_eq!(running["branch"], "feature/4700-demo");

    // The message command reaches the agent's pane.
    wait_for("the message", || !host.typed.lock().is_empty());
    assert_eq!(
        host.typed.lock()[0],
        ("pane-1".to_owned(), "Also add tests".to_owned())
    );

    // The journal opens with the line that marks the role and then the transcript, in whole
    // lines, `seq` from 1.
    wait_for("the journal", || {
        log.lock()
            .iter()
            .filter(|(path, _)| path.contains("/s-good/journal"))
            .count()
            >= 2
    });
    let chunks: Vec<(String, String)> = log
        .lock()
        .iter()
        .filter(|(path, _)| path.contains("/s-good/journal"))
        .cloned()
        .collect();
    assert!(chunks[0].0.ends_with("?seq=1"), "{}", chunks[0].0);
    assert!(
        chunks[0].1.contains("\"role\":\"executor\""),
        "{}",
        chunks[0].1
    );
    assert!(chunks[1].0.ends_with("?seq=2"), "{}", chunks[1].0);
    assert_eq!(
        chunks[1].1,
        "{\"type\":\"user\",\"n\":1,\"tab\":0}\n{\"type\":\"assistant\",\"n\":2}\n"
    );

    // The heartbeat lists the session.
    wait_for("a heartbeat with the session", || {
        log.lock().iter().any(|(path, body)| {
            path.ends_with("/heartbeat") && body.contains("\"sessionId\":\"s-good\"")
        })
    });

    // The agent exits with 0: review.
    std::fs::write(agents.join("exit-code"), "0\n").unwrap();
    wait_for("review", || {
        statuses(&log)
            .iter()
            .any(|status| status["status"] == "review")
    });
    let review = statuses(&log)
        .into_iter()
        .find(|status| status["status"] == "review")
        .unwrap();
    assert_eq!(review["exitCode"], 0);
    let order: Vec<String> = statuses(&log)
        .iter()
        .map(|status| status["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(order, ["running", "review"]);

    assert_eq!(registrations.load(Ordering::SeqCst), 1);
    let saved = host.registration.lock().clone().unwrap();
    assert_eq!(saved.worker_id, "w-1");
    assert_eq!(saved.fingerprint, host.config.fingerprint());
    wait_for("the panel", || {
        let snapshot = host.snapshot.lock();
        snapshot.connection == Connection::Online
            && snapshot
                .sessions
                .iter()
                .any(|session| session.status == SessionStatus::Review)
    });
}

/// An assignment without a prompt, a repository or a branch: the orchestrator session in the
/// vault, the plan, the owner's approval and only then the executor, plus the machine profile
/// of the same registration.
#[test]
fn the_preparation_phase_runs_before_the_executor() {
    if machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo_url = remote(dir.path());
    let root = dir.path().join("agents-work");
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let homes = Homes {
        claude_dir: dir.path().join("home").join(".claude"),
        claude_json: dir.path().join("home").join(".claude.json"),
        codex_dir: dir.path().join("home").join(".codex"),
    };
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    std::fs::write(
        &homes.claude_json,
        r#"{"oauthAccount":{"accountUuid":"u-1"},"mcpServers":{"grafana":{"url":"own"}}}"#,
    )
    .unwrap();

    let mut server = Server::new();
    let log: Log = Arc::default();
    let base = "/agents/t-prepare/api/v1/worker";
    let profile = serde_json::json!({
        "version": 2,
        "parts": {
            "dnsProxy": {"enabled": false, "mode": "off", "checkUrl": ""},
            "mcp": {"servers": {"chimney": {"url": "https://chimney.steep-man.ru/mcp"}}},
            "vault": {"enabled": true, "path": vault.to_string_lossy(), "update": "none"}
        }
    });
    {
        let body = serde_json::json!({
            "workerId": "w-1",
            "token": "agw_token",
            "heartbeatSec": 1,
            "profileVersion": 2,
            "profile": profile,
        })
        .to_string();
        server
            .mock("POST", format!("{base}/register").as_str())
            .with_body_from_request(move |_| body.clone().into_bytes())
            .create();
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/heartbeat"),
        &log,
        |_| br#"{"profileVersion":2}"#.to_vec(),
    );
    record(
        &mut server,
        "POST",
        &format!("{base}/profile/applied"),
        &log,
        |_| Vec::new(),
    );
    let assignment = serde_json::json!({
        "sessionId": "s-prep",
        "taskRef": {"boardId": 5, "taskId": 5024, "taskNumber": 4639},
        "attempt": 1,
        "title": "Подготовка задания",
        "repo": null,
        "baseBranch": null,
        "branch": null,
        "prompt": null,
        "provider": "claude",
        "model": null,
        "maxDurationMin": 0,
        "planApproval": true,
        "prepareMaxDurationMin": 30,
    });
    {
        let first = serde_json::json!({"assignments": [assignment]}).to_string();
        let approve = serde_json::json!({"commands": [{"type": "approve", "sessionId": "s-prep"}]})
            .to_string();
        let approved = Arc::new(Mutex::new(false));
        let seen = log.clone();
        record(
            &mut server,
            "GET",
            &format!("{base}/inbox"),
            &log,
            move |_| {
                let (profile_applied, accepted, plan_sent) = {
                    let log = seen.lock();
                    (
                        log.iter()
                            .any(|(path, _)| path.contains("/profile/applied")),
                        log.iter().any(|(path, body)| {
                            path.contains("/assignments/s-prep/ack")
                                && body.contains("\"accepted\":true")
                        }),
                        log.iter().any(|(path, _)| path.contains("/s-prep/plan")),
                    )
                };
                // The task waits for the machine profile: the vault of the preparation phase comes
                // with it. A body that is not JSON means "nothing yet", and the worker backs off
                // instead of polling in a tight loop.
                if !profile_applied {
                    return b"-".to_vec();
                }
                if !accepted {
                    return first.clone().into_bytes();
                }
                // The owner approves once the plan reached the service.
                let mut approved = approved.lock();
                if plan_sent && !*approved {
                    *approved = true;
                    return approve.clone().into_bytes();
                }
                b"-".to_vec()
            },
        );
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/assignments/"),
        &log,
        |_| Vec::new(),
    );
    record(
        &mut server,
        "POST",
        &format!("{base}/sessions/"),
        &log,
        |_| Vec::new(),
    );

    let host = Arc::new(FakeHost::new(
        WorkerConfig {
            base_url: format!("{}/agents/t-prepare", server.url()),
            key: "agk_test".to_owned(),
            name: "test-pc".to_owned(),
            root: root.clone(),
            clone_search: Vec::new(),
            paused: false,
        },
        homes.clone(),
    ));
    let plan = serde_json::json!({
        "repo": repo_url,
        "baseBranch": "main",
        "branch": "feature/4639-demo",
        "prompt": "Промпт из плана",
        "dossierPath": "11-Задачи/Активные/Steep-Brain/sb. Подготовка.md",
        "runId": "run-20261031-001",
        "provider": "claude",
        "model": null,
        "summary": "Досье собрано, ветка выбрана",
        "grouped": [4639],
    })
    .to_string();
    let prepare_dir = root.join(".agents").join("s-prep");
    *host.plan.lock() = Some((prepare_dir.join("plan.json"), plan));

    let timing = Timing {
        tick: Duration::from_millis(100),
        journal_every: Duration::from_millis(100),
        config_every: Duration::from_millis(100),
        inbox_wait_secs: 0,
        session_lookup_grace: Duration::from_secs(30),
        session_start_grace: Duration::from_secs(300),
        exit_grace: Duration::from_secs(30),
        prepare_exit_wait: Duration::from_millis(200),
        stop_grace: Duration::from_secs(30),
        usage_every: Duration::from_secs(60),
        profile_every: Duration::from_millis(100),
        proxy_every: Duration::from_secs(60),
        min_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(1),
    };
    let worker = Worker::new(
        host.clone(),
        Arc::new(FakeMachine),
        http_client::Client::new_for_test(),
        homes.clone(),
        profile_target(dir.path(), &homes),
        timing,
        "test".to_owned(),
    );
    let runner = worker.clone();
    std::thread::spawn(move || futures::executor::block_on(runner.run()));

    let statuses = |log: &Log| -> Vec<serde_json::Value> {
        log.lock()
            .iter()
            .filter(|(path, _)| path.contains("/s-prep/status"))
            .map(|(_, body)| serde_json::from_str(body).unwrap())
            .collect()
    };

    // The profile of the registration is applied before any task: the MCP server of the
    // profile is in place and the owner's own one is untouched.
    wait_for("the profile report", || {
        log.lock()
            .iter()
            .any(|(path, _)| path.contains("/profile/applied"))
    });
    let (_, report) = log
        .lock()
        .iter()
        .find(|(path, _)| path.contains("/profile/applied"))
        .cloned()
        .unwrap();
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["version"], 2);
    assert_eq!(report["ok"], true, "{report}");
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&homes.claude_json).unwrap()).unwrap();
    assert_eq!(
        claude_json["mcpServers"]["chimney"]["url"],
        "https://chimney.steep-man.ru/mcp"
    );
    assert_eq!(claude_json["mcpServers"]["grafana"]["url"], "own");

    // The assignment without a prompt is taken and the orchestrator opens in the vault.
    wait_for("the orchestrator tab", || !host.tabs.lock().is_empty());
    {
        let tabs = host.tabs.lock();
        assert!(
            provider::same_path(&tabs[0].cwd.to_string_lossy(), &vault),
            "{:?}",
            tabs[0].cwd
        );
    }
    let prompt = std::fs::read_to_string(prepare_dir.join("prompt.md")).unwrap();
    assert!(prompt.contains("#4639"), "{prompt}");
    assert!(prompt.contains("kanban:5/5024"));
    assert!(prompt.contains("brain-manager.md"));
    let plan_in_prompt = prepare_dir
        .join("plan.json")
        .to_string_lossy()
        .replace('\\', "/");
    assert!(prompt.contains(&plan_in_prompt), "{prompt}");
    wait_for("preparing", || {
        statuses(&log)
            .iter()
            .any(|status| status["status"] == "preparing")
    });

    // The plan reaches the service and the session waits for the owner.
    wait_for("the plan", || {
        log.lock()
            .iter()
            .any(|(path, _)| path.contains("/s-prep/plan"))
    });
    let (_, plan_body) = log
        .lock()
        .iter()
        .find(|(path, _)| path.contains("/s-prep/plan"))
        .cloned()
        .unwrap();
    let plan_body: serde_json::Value = serde_json::from_str(&plan_body).unwrap();
    assert_eq!(plan_body["branch"], "feature/4639-demo");
    assert_eq!(plan_body["prompt"], "Промпт из плана");
    assert_eq!(plan_body["runId"], "run-20261031-001");
    assert_eq!(plan_body["grouped"][0], 4639);
    wait_for("plan-review", || {
        statuses(&log)
            .iter()
            .any(|status| status["status"] == "plan-review")
    });

    // After `approve` the worktree of the plan is made and the executor starts with its prompt.
    wait_for("the executor tab", || host.tabs.lock().len() >= 2);
    let worktree = root
        .join("SteepMans")
        .join("demo")
        .join(".worktrees")
        .join("s-prep");
    wait_for("the prompt of the executor", || {
        worktree.join(".agents").join("prompt.md").is_file()
    });
    assert_eq!(
        std::fs::read_to_string(worktree.join(".agents").join("prompt.md")).unwrap(),
        "Промпт из плана"
    );
    wait_for("running", || {
        statuses(&log)
            .iter()
            .any(|status| status["status"] == "running")
    });
    let running = statuses(&log)
        .into_iter()
        .find(|status| status["status"] == "running")
        .unwrap();
    assert_eq!(running["branch"], "feature/4639-demo");
    assert_eq!(running["agentSessionId"], FakeHost::agent_session_id(1));

    // Both parts of the journal are marked with their role, in one numbering.
    wait_for("both roles in the journal", || {
        let log = log.lock();
        log.iter()
            .any(|(path, body)| path.contains("/s-prep/journal") && body.contains("orchestrator"))
            && log
                .iter()
                .any(|(path, body)| path.contains("/s-prep/journal") && body.contains("executor"))
    });
    let roles: Vec<String> = log
        .lock()
        .iter()
        .filter(|(path, _)| path.contains("/s-prep/journal"))
        .filter_map(|(_, body)| {
            serde_json::from_str::<serde_json::Value>(body.lines().next().unwrap_or_default())
                .ok()
                .and_then(|line| line["role"].as_str().map(str::to_owned))
        })
        .collect();
    assert_eq!(roles, ["orchestrator", "executor"]);

    // The heartbeat carries the state of the profile for the scheduler.
    wait_for("a heartbeat with the profile", || {
        log.lock()
            .iter()
            .any(|(path, body)| path.ends_with("/heartbeat") && body.contains("\"profile\""))
    });
    let (_, beat) = log
        .lock()
        .iter()
        .find(|(path, body)| path.ends_with("/heartbeat") && body.contains("\"profile\""))
        .cloned()
        .unwrap();
    let beat: serde_json::Value = serde_json::from_str(&beat).unwrap();
    assert_eq!(beat["profile"]["version"], 2);
    assert_eq!(beat["profile"]["ok"], true);

    // The executor finishes: the session ends as any other.
    std::fs::write(worktree.join(".agents").join("exit-code"), "0\n").unwrap();
    wait_for("review", || {
        statuses(&log)
            .iter()
            .any(|status| status["status"] == "review")
    });
    let order: Vec<String> = statuses(&log)
        .iter()
        .map(|status| status["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(order.first().map(String::as_str), Some("preparing"));
    assert!(order.contains(&"plan-review".to_owned()), "{order:?}");
    assert_eq!(order.last().map(String::as_str), Some("review"));

    // The panel shows the profile and the plan of the session.
    wait_for("the panel", || {
        let snapshot = host.snapshot.lock();
        snapshot
            .profile
            .as_ref()
            .is_some_and(|profile| profile.version == 2 && profile.ok)
            && snapshot
                .sessions
                .iter()
                .any(|session| session.plan.is_some())
    });
    let snapshot = host.snapshot.lock();
    assert_eq!(snapshot.profile_version, 2);
    assert!(snapshot.vault.is_some());
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.session_id == "s-prep")
        .unwrap();
    assert!(
        session
            .plan
            .as_deref()
            .is_some_and(|plan| plan.contains("Досье собрано")),
        "{:?}",
        session.plan
    );
}

/// The orchestrator left no plan: the session fails with `prepare_failed`, and no executor and
/// no worktree follow.
#[test]
fn a_preparation_without_a_plan_fails_with_its_own_reason() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents-work");
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let homes = Homes {
        claude_dir: dir.path().join("home").join(".claude"),
        claude_json: dir.path().join("home").join(".claude.json"),
        codex_dir: dir.path().join("home").join(".codex"),
    };
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    std::fs::write(
        &homes.claude_json,
        r#"{"oauthAccount":{"accountUuid":"u-1"}}"#,
    )
    .unwrap();

    let mut server = Server::new();
    let log: Log = Arc::default();
    let base = "/agents/t-noplan/api/v1/worker";
    {
        let body = serde_json::json!({
            "workerId": "w-1",
            "token": "agw_token",
            "heartbeatSec": 1,
            "profileVersion": 1,
            "profile": {
                "version": 1,
                "parts": {"vault": {"enabled": true, "path": vault.to_string_lossy(),
                                    "update": "none"}}
            },
        })
        .to_string();
        server
            .mock("POST", format!("{base}/register").as_str())
            .with_body_from_request(move |_| body.clone().into_bytes())
            .create();
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/heartbeat"),
        &log,
        |_| br#"{"profileVersion":1}"#.to_vec(),
    );
    record(
        &mut server,
        "POST",
        &format!("{base}/profile/applied"),
        &log,
        |_| Vec::new(),
    );
    {
        let first = serde_json::json!({"assignments": [{
            "sessionId": "s-noplan",
            "taskRef": {"boardId": 5, "taskId": 5024, "taskNumber": 4639},
            "title": "Без плана",
            "provider": "claude",
            "maxDurationMin": 0,
            "prepareMaxDurationMin": 0,
        }]})
        .to_string();
        let seen = log.clone();
        record(
            &mut server,
            "GET",
            &format!("{base}/inbox"),
            &log,
            move |_| {
                let (profile_applied, accepted) = {
                    let log = seen.lock();
                    (
                        log.iter()
                            .any(|(path, _)| path.contains("/profile/applied")),
                        log.iter().any(|(path, body)| {
                            path.contains("/assignments/s-noplan/ack")
                                && body.contains("\"accepted\":true")
                        }),
                    )
                };
                if !profile_applied || accepted {
                    return b"-".to_vec();
                }
                first.clone().into_bytes()
            },
        );
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/assignments/"),
        &log,
        |_| Vec::new(),
    );
    record(
        &mut server,
        "POST",
        &format!("{base}/sessions/"),
        &log,
        |_| Vec::new(),
    );

    let host = Arc::new(FakeHost::new(
        WorkerConfig {
            base_url: format!("{}/agents/t-noplan", server.url()),
            key: "agk_test".to_owned(),
            name: "test-pc".to_owned(),
            root: root.clone(),
            clone_search: Vec::new(),
            paused: false,
        },
        homes.clone(),
    ));
    // Nothing writes a plan, and the orchestrator's process is gone at once.
    let timing = Timing {
        tick: Duration::from_millis(100),
        journal_every: Duration::from_millis(100),
        config_every: Duration::from_millis(100),
        inbox_wait_secs: 0,
        session_lookup_grace: Duration::from_secs(30),
        session_start_grace: Duration::from_secs(300),
        exit_grace: Duration::from_millis(100),
        prepare_exit_wait: Duration::from_millis(200),
        stop_grace: Duration::from_millis(100),
        usage_every: Duration::from_secs(60),
        profile_every: Duration::from_millis(100),
        proxy_every: Duration::from_secs(60),
        min_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(1),
    };
    let worker = Worker::new(
        host.clone(),
        Arc::new(FakeMachine),
        http_client::Client::new_for_test(),
        homes.clone(),
        profile_target(dir.path(), &homes),
        timing,
        "test".to_owned(),
    );
    let runner = worker.clone();
    std::thread::spawn(move || futures::executor::block_on(runner.run()));

    // The orchestrator opened, wrote no plan and exited.
    let prepare_dir = root.join(".agents").join("s-noplan");
    wait_for("the orchestrator tab", || !host.tabs.lock().is_empty());
    std::fs::write(
        prepare_dir.join("exit-code"),
        "0
",
    )
    .unwrap();
    wait_for("failed", || {
        log.lock()
            .iter()
            .any(|(path, body)| path.contains("/s-noplan/status") && body.contains("\"failed\""))
    });
    let (_, failed) = log
        .lock()
        .iter()
        .find(|(path, body)| path.contains("/s-noplan/status") && body.contains("\"failed\""))
        .cloned()
        .unwrap();
    let failed: serde_json::Value = serde_json::from_str(&failed).unwrap();
    assert_eq!(failed["reason"], "prepare_failed");
    assert!(
        failed["message"]
            .as_str()
            .unwrap_or_default()
            .contains("plan.json"),
        "{failed}"
    );
    // No worktree was made: the executor never started.
    assert!(!root.join("SteepMans").exists());
}

/// What the watch takes for an agent that never started: no session of its own, not a line of
/// transcript and the grace over.
#[test]
fn a_launch_is_silent_without_a_session_and_without_transcript() {
    let mut session = Session::new(
        Assignment::default(),
        provider::by_id("claude").unwrap(),
        Phase::Done,
    );
    let grace = Duration::from_secs(180);
    assert!(!is_silent(&session, Duration::from_secs(30), grace));
    assert!(is_silent(&session, grace, grace));
    // A line of the transcript is enough to tell a working agent from a waiting one.
    session.journal.ingest(b"{\"type\":\"user\"}\n");
    assert!(!is_silent(&session, grace, grace));
    let mut session = Session::new(
        Assignment::default(),
        provider::by_id("claude").unwrap(),
        Phase::Done,
    );
    session.found = Some(FoundSession {
        agent_session_id: AGENT_SESSION_ID.to_owned(),
        pid: Some(4242),
        remote_control_url: None,
        transcript: None,
    });
    assert!(!is_silent(&session, grace, grace));
}

/// A tab that opens and then nothing: the agent waits for an answer nobody gives. The session
/// ends as `failed` with a message that names it, and the folder trust Claude Code asks about
/// is in `~/.claude.json` before the tab starts.
#[test]
fn a_tab_that_never_starts_the_agent_ends_as_failed() {
    if machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo_url = remote(dir.path());
    let root = dir.path().join("agents-work");
    let homes = Homes {
        claude_dir: dir.path().join("home").join(".claude"),
        claude_json: dir.path().join("home").join(".claude.json"),
        codex_dir: dir.path().join("home").join(".codex"),
    };
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    std::fs::write(
        &homes.claude_json,
        r#"{"oauthAccount":{"accountUuid":"u-1"},"projects":{"C:/work/other/bot":{"hasTrustDialogAccepted":true}}}"#,
    )
    .unwrap();

    let mut server = Server::new();
    let log: Log = Arc::default();
    let base = "/agents/t-silent/api/v1/worker";
    server
        .mock("POST", format!("{base}/register").as_str())
        .with_body(r#"{"workerId":"w-1","token":"agw_token","heartbeatSec":1}"#)
        .create();
    record(
        &mut server,
        "POST",
        &format!("{base}/heartbeat"),
        &log,
        |_| b"{}".to_vec(),
    );
    let assignment = serde_json::json!({
        "sessionId": "s-silent",
        "taskRef": {"boardId": 5, "taskId": 5028, "taskNumber": 4643},
        "attempt": 1,
        "title": "Молчащая вкладка",
        "repo": repo_url,
        "baseBranch": "main",
        "branch": "fix/4643-demo",
        "prompt": "Сделай демо",
        "provider": "claude",
        "model": null,
        "maxDurationMin": 0,
    });
    {
        let first = serde_json::json!({"assignments": [assignment]}).to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        record(
            &mut server,
            "GET",
            &format!("{base}/inbox"),
            &log,
            move |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    first.clone().into_bytes()
                } else {
                    b"-".to_vec()
                }
            },
        );
    }
    record(
        &mut server,
        "POST",
        &format!("{base}/assignments/"),
        &log,
        |_| Vec::new(),
    );
    record(
        &mut server,
        "POST",
        &format!("{base}/sessions/"),
        &log,
        |_| Vec::new(),
    );

    let host = Arc::new(
        FakeHost::new(
            WorkerConfig {
                base_url: format!("{}/agents/t-silent", server.url()),
                key: "agk_test".to_owned(),
                name: "test-pc".to_owned(),
                root: root.clone(),
                clone_search: Vec::new(),
                paused: false,
            },
            homes.clone(),
        )
        .silent(),
    );
    let timing = Timing {
        tick: Duration::from_millis(100),
        journal_every: Duration::from_millis(100),
        config_every: Duration::from_millis(100),
        inbox_wait_secs: 0,
        session_lookup_grace: Duration::from_millis(100),
        session_start_grace: Duration::from_millis(300),
        exit_grace: Duration::from_secs(30),
        prepare_exit_wait: Duration::from_millis(200),
        stop_grace: Duration::from_secs(30),
        usage_every: Duration::from_secs(60),
        profile_every: Duration::from_millis(100),
        proxy_every: Duration::from_secs(60),
        min_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(1),
    };
    let worker = Worker::new(
        host.clone(),
        Arc::new(FakeMachine),
        http_client::Client::new_for_test(),
        homes.clone(),
        profile_target(dir.path(), &homes),
        timing,
        "test".to_owned(),
    );
    let runner = worker.clone();
    std::thread::spawn(move || futures::executor::block_on(runner.run()));

    wait_for("failed", || {
        log.lock()
            .iter()
            .any(|(path, body)| path.contains("/s-silent/status") && body.contains("\"failed\""))
    });
    let (_, failed) = log
        .lock()
        .iter()
        .find(|(path, body)| path.contains("/s-silent/status") && body.contains("\"failed\""))
        .cloned()
        .unwrap();
    let failed: serde_json::Value = serde_json::from_str(&failed).unwrap();
    assert!(
        failed["message"]
            .as_str()
            .unwrap_or_default()
            .contains("не начал работу"),
        "{failed}"
    );

    // The trust of the worktree was written before the tab, and the owner's own file is whole.
    let worktree = root
        .join("SteepMans")
        .join("demo")
        .join(".worktrees")
        .join("s-silent");
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&homes.claude_json).unwrap()).unwrap();
    assert_eq!(claude_json["oauthAccount"]["accountUuid"], "u-1");
    assert_eq!(
        claude_json["projects"]["C:/work/other/bot"]["hasTrustDialogAccepted"],
        true
    );
    for key in provider::claude::project_keys(&worktree) {
        assert_eq!(
            claude_json["projects"][&key]["hasTrustDialogAccepted"], true,
            "{key} missing in {claude_json}"
        );
    }
}

/// Pids of the orchestrator of the tests below: the agent itself and the runner around it.
const ORCHESTRATOR_PID: u32 = 4343;
const RUNNER_PID: u32 = 4242;

/// A machine whose processes can be declared gone and which remembers what Ward asked it to
/// stop; everything else is [`FakeMachine`].
struct WatchedMachine {
    alive: Mutex<bool>,
    stopped: Mutex<Vec<u32>>,
}

impl Default for WatchedMachine {
    fn default() -> Self {
        Self {
            alive: Mutex::new(true),
            stopped: Mutex::new(Vec::new()),
        }
    }
}

impl Machine for WatchedMachine {
    fn hardware(&self, root: &Path) -> machine::Hardware {
        FakeMachine.hardware(root)
    }

    fn resources(&self, root: &Path) -> Resources {
        FakeMachine.resources(root)
    }

    fn has_tool(&self, name: &str) -> bool {
        FakeMachine.has_tool(name)
    }

    fn tool_version(&self, name: &str) -> Option<String> {
        FakeMachine.tool_version(name)
    }

    fn is_alive(&self, _pid: u32) -> bool {
        *self.alive.lock()
    }

    fn stop(&self, pids: &[u32]) {
        self.stopped.lock().extend_from_slice(pids);
    }

    fn runner(&self) -> Option<(RunnerKind, Option<PathBuf>)> {
        FakeMachine.runner()
    }

    fn write_elevated(&self, path: &Path, contents: &str) -> Result<(), String> {
        FakeMachine.write_elevated(path, contents)
    }
}

/// A worker with no service and no loops behind it: the tests of the preparation phase move a
/// session by hand with [`Worker::advance`].
fn offline_worker(dir: &Path, machine: Arc<dyn Machine>, timing: Timing) -> Arc<Worker> {
    let homes = Homes {
        claude_dir: dir.join("home").join(".claude"),
        claude_json: dir.join("home").join(".claude.json"),
        codex_dir: dir.join("home").join(".codex"),
    };
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let config = WorkerConfig {
        base_url: "http://127.0.0.1:1/agents".to_owned(),
        key: "agk_test".to_owned(),
        name: "test-pc".to_owned(),
        root: dir.join("agents-work"),
        clone_search: Vec::new(),
        paused: false,
    };
    let host = Arc::new(FakeHost::new(config.clone(), homes.clone()));
    let worker = Worker::new(
        host,
        machine,
        http_client::Client::new_for_test(),
        homes.clone(),
        profile_target(dir, &homes),
        timing,
        "test".to_owned(),
    );
    worker.state.lock().config = Some(config);
    worker
}

/// A session in the preparation phase: the orchestrator's directory, the agent session Ward
/// found and the transcript it writes.
fn preparing_session(root: &Path, transcript: &Path, plan_approval: bool) -> Session {
    let assignment = Assignment {
        session_id: "s-exit".to_owned(),
        title: "Выход оркестратора".to_owned(),
        plan_approval,
        ..Assignment::default()
    };
    let dir = launch::prepare_dir(root, &assignment.session_id).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = Session::new(
        assignment,
        provider::by_id("claude").unwrap(),
        Phase::Prepare {
            launched_at_ms: now_ms(),
            at: Instant::now(),
        },
    );
    session.prepare = Some(PrepareRun {
        plan_path: dir.join(plan::PLAN_FILE),
        dir,
        vault_dir: root.join("vault"),
        limit: Duration::from_secs(600),
        last_error: None,
    });
    session.status = SessionStatus::Preparing;
    session.runner_pid = Some(RUNNER_PID);
    session.found = Some(FoundSession {
        agent_session_id: AGENT_SESSION_ID.to_owned(),
        pid: Some(ORCHESTRATOR_PID),
        remote_control_url: None,
        transcript: None,
    });
    session.transcript = Some(transcript.to_path_buf());
    session
}

/// What the orchestrator writes into `plan.json`.
fn demo_plan() -> String {
    serde_json::json!({
        "repo": "https://github.com/SteepMans/demo.git",
        "baseBranch": "main",
        "branch": "feature/4650-demo",
        "prompt": "Промпт из плана",
        "summary": "План готов",
    })
    .to_string()
}

/// The transcript of the orchestrator with its closing answer added.
const ANSWERED: &str =
    "{\"type\":\"user\",\"n\":1}\n{\"type\":\"assistant\",\"text\":\"план записан\"}\n";

/// The plan is taken and the orchestrator ends its own session: Ward never touches its
/// process, its closing answer still reaches the journal, and nothing of the executor starts
/// while it is alive.
#[test]
fn an_orchestrator_that_ends_by_itself_is_not_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents-work");
    let transcript = dir.path().join("orchestrator.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\",\"n\":1}\n").unwrap();
    let machine = Arc::new(WatchedMachine::default());
    let timing = Timing {
        prepare_exit_wait: Duration::from_secs(30),
        ..Timing::default()
    };
    let worker = offline_worker(dir.path(), machine.clone(), timing);
    let mut session = preparing_session(&root, &transcript, true);
    let prepare_dir = session.prepare.as_ref().unwrap().dir.clone();
    std::fs::write(prepare_dir.join(plan::PLAN_FILE), demo_plan()).unwrap();

    // The plan is read, but the orchestrator is still running: it is left to finish.
    futures::executor::block_on(worker.advance(&mut session));
    assert!(matches!(session.phase, Phase::PrepareExit { .. }));
    assert_eq!(session.assignment.branch, "feature/4650-demo");
    assert!(session.plan.is_some());
    futures::executor::block_on(worker.advance(&mut session));
    assert!(
        matches!(session.phase, Phase::PrepareExit { .. }),
        "the executor must not start while the orchestrator is alive"
    );
    assert_eq!(session.status, SessionStatus::Preparing);
    assert!(session.worktree.is_none());
    assert!(machine.stopped.lock().is_empty());

    // It answers and exits by itself; the exit code is then its own and no doing of Ward's.
    std::fs::write(&transcript, ANSWERED).unwrap();
    *machine.alive.lock() = false;
    std::fs::write(prepare_dir.join(launch::EXIT_CODE_FILE), "0").unwrap();
    futures::executor::block_on(worker.advance(&mut session));
    assert!(matches!(session.phase, Phase::PlanReview));
    assert_eq!(session.status, SessionStatus::PlanReview);
    assert!(
        machine.stopped.lock().is_empty(),
        "an orchestrator that already went needs no stopping"
    );
    // The journal was read to the end: the last message is there, not a sentence cut in half.
    assert!(
        session.journal.tail().contains("план записан"),
        "{}",
        session.journal.tail()
    );
}

/// The plan is taken and the orchestrator hangs: Ward waits for it, ends it when the wait is
/// over and goes on with the session.
#[test]
fn an_orchestrator_that_hangs_after_the_plan_is_ended_by_ward() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents-work");
    let transcript = dir.path().join("orchestrator.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\",\"n\":1}\n").unwrap();
    let machine = Arc::new(WatchedMachine::default());
    let timing = Timing {
        prepare_exit_wait: Duration::from_millis(100),
        ..Timing::default()
    };
    let worker = offline_worker(dir.path(), machine.clone(), timing);
    let mut session = preparing_session(&root, &transcript, true);
    let prepare_dir = session.prepare.as_ref().unwrap().dir.clone();
    std::fs::write(prepare_dir.join(plan::PLAN_FILE), demo_plan()).unwrap();

    futures::executor::block_on(worker.advance(&mut session));
    assert!(matches!(session.phase, Phase::PrepareExit { .. }));
    assert!(machine.stopped.lock().is_empty());

    // Nothing comes of the wait: the process of the orchestrator is ended, and its last lines
    // are read all the same.
    std::fs::write(&transcript, ANSWERED).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    futures::executor::block_on(worker.advance(&mut session));
    assert_eq!(*machine.stopped.lock(), vec![ORCHESTRATOR_PID]);
    assert!(matches!(session.phase, Phase::PlanReview));
    assert_eq!(session.status, SessionStatus::PlanReview);
    assert!(
        session.journal.tail().contains("план записан"),
        "{}",
        session.journal.tail()
    );
}

/// The orchestrator fell over before it wrote a plan: that is a failed preparation, and the
/// code it died with is named in the message.
#[test]
fn an_orchestrator_that_crashes_before_the_plan_fails_the_preparation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agents-work");
    let transcript = dir.path().join("orchestrator.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\",\"n\":1}\n").unwrap();
    let machine = Arc::new(WatchedMachine::default());
    let timing = Timing {
        exit_grace: Duration::from_millis(100),
        ..Timing::default()
    };
    let worker = offline_worker(dir.path(), machine.clone(), timing);
    let mut session = preparing_session(&root, &transcript, false);
    let prepare_dir = session.prepare.as_ref().unwrap().dir.clone();
    std::fs::write(prepare_dir.join(launch::EXIT_CODE_FILE), "2\n").unwrap();

    // The plan may still land right after the exit, so the failure waits out `exit_grace`.
    futures::executor::block_on(worker.advance(&mut session));
    assert!(!session.is_done());
    std::thread::sleep(Duration::from_millis(150));
    futures::executor::block_on(worker.advance(&mut session));
    assert!(session.is_done());
    assert_eq!(session.status, SessionStatus::Failed);
    let message = session.message.clone().unwrap_or_default();
    assert!(message.contains("plan.json"), "{message}");
    assert!(message.contains("код 2"), "{message}");
    let failed = session
        .outbox
        .iter()
        .find(|request| request.status == SessionStatus::Failed)
        .unwrap();
    assert_eq!(failed.reason.as_deref(), Some(REASON_PREPARE_FAILED));
    assert!(session.worktree.is_none());
}
