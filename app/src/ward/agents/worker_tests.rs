//! The whole cycle against a fake service: registration, heartbeat, an accepted and a refused
//! assignment, a worktree from a local remote, a fake agent session, a message, the transcript
//! and the final status.

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
        paused: false,
    };
    assert!(!format!("{config:?}").contains("agk_secret"));
    let registration = Registration {
        fingerprint: config.fingerprint(),
        worker_id: "w".to_owned(),
        token: "agw_secret".to_owned(),
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
}

/// Plays the UI and the agent: opening the tab "starts" a Claude session in the worktree.
struct FakeHost {
    config: WorkerConfig,
    homes: Homes,
    registration: Mutex<Option<Registration>>,
    tabs: Mutex<Vec<TabSpec>>,
    typed: Mutex<Vec<(String, String)>>,
    snapshot: Mutex<Snapshot>,
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
        let sessions = self.homes.claude_dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let session_file = serde_json::json!({
            "pid": 4242,
            "sessionId": AGENT_SESSION_ID,
            "cwd": tab.cwd.to_string_lossy(),
            "startedAt": now_ms(),
            "bridgeSessionId": BRIDGE_ID,
        });
        std::fs::write(sessions.join("4242.json"), session_file.to_string()).unwrap();
        let project = self
            .homes
            .claude_dir
            .join("projects")
            .join(provider::claude::project_slug(&tab.cwd));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{AGENT_SESSION_ID}.jsonl")),
            "{\"type\":\"user\",\"n\":1}\n{\"type\":\"assistant\",\"n\":2}\n",
        )
        .unwrap();
        self.tabs.lock().push(tab);
        Ok(Some("pane-1".to_owned()))
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
    let registrations = Arc::new(AtomicUsize::new(0));
    {
        let registrations = registrations.clone();
        server
            .mock("POST", "/agents/api/v1/worker/register")
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
    let base = "/agents/api/v1/worker";
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

    let host = Arc::new(FakeHost {
        config: WorkerConfig {
            base_url: format!("{}/agents", server.url()),
            key: "agk_test".to_owned(),
            name: "test-pc".to_owned(),
            root: root.clone(),
            paused: false,
        },
        homes: homes.clone(),
        registration: Mutex::new(None),
        tabs: Mutex::new(Vec::new()),
        typed: Mutex::new(Vec::new()),
        snapshot: Mutex::new(Snapshot::default()),
    });
    let timing = Timing {
        tick: Duration::from_millis(100),
        journal_every: Duration::from_millis(100),
        config_every: Duration::from_millis(100),
        inbox_wait_secs: 0,
        session_lookup_grace: Duration::from_secs(30),
        exit_grace: Duration::from_secs(30),
        stop_grace: Duration::from_secs(30),
        usage_every: Duration::from_secs(60),
        min_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(1),
    };
    let worker = Worker::new(
        host.clone(),
        Arc::new(FakeMachine),
        http_client::Client::new_for_test(),
        homes.clone(),
        timing,
        "test".to_owned(),
    );
    let runner = worker.clone();
    std::thread::spawn(move || futures::executor::block_on(runner.run()));

    // Acks: the good assignment is taken, the empty prompt refused.
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
    assert_eq!(ack_of("s-empty")["reason"], "empty prompt");

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

    // The transcript goes out as whole lines with seq 1.
    wait_for("the journal", || {
        log.lock()
            .iter()
            .any(|(path, _)| path.contains("/s-good/journal"))
    });
    let (journal_path, journal_body) = log
        .lock()
        .iter()
        .find(|(path, _)| path.contains("/s-good/journal"))
        .cloned()
        .unwrap();
    assert!(journal_path.ends_with("?seq=1"), "{journal_path}");
    assert_eq!(
        journal_body,
        "{\"type\":\"user\",\"n\":1}\n{\"type\":\"assistant\",\"n\":2}\n"
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
