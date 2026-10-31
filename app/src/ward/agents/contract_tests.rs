use std::collections::BTreeMap;

use serde_json::json;

use super::*;

#[test]
fn register_request_uses_contract_field_names() {
    let mut agents = BTreeMap::new();
    agents.insert(
        "claude".to_owned(),
        AgentInfo {
            installed: true,
            version: Some("2.1.316 (Claude Code)".to_owned()),
            logged_in: true,
            plan: Some("max".to_owned()),
        },
    );
    let request = RegisterRequest {
        key: "agk_x".to_owned(),
        name: "steep-pc".to_owned(),
        os: "windows".to_owned(),
        arch: "x86_64".to_owned(),
        ward_version: "v0.2026.10.31.12.00.oss_00".to_owned(),
        passport: Passport {
            cpu_cores: 16,
            ram_total_bytes: 32,
            disk_total_bytes: 64,
            tools: Tools {
                git: true,
                codex: true,
                ..Default::default()
            },
            agents,
            repos: vec!["SteepMans/brain-manager-bot".to_owned()],
        },
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "key": "agk_x",
            "name": "steep-pc",
            "os": "windows",
            "arch": "x86_64",
            "wardVersion": "v0.2026.10.31.12.00.oss_00",
            "passport": {
                "cpuCores": 16,
                "ramTotalBytes": 32,
                "diskTotalBytes": 64,
                "tools": {
                    "git": true, "claude": false, "codex": true, "node": false,
                    "java": false, "cargo": false, "dotnet": false
                },
                "agents": {
                    "claude": {
                        "installed": true,
                        "version": "2.1.316 (Claude Code)",
                        "loggedIn": true,
                        "plan": "max"
                    }
                },
                "repos": ["SteepMans/brain-manager-bot"]
            }
        })
    );
}

#[test]
fn register_response_defaults_and_redacts_token() {
    let response = parse_register(r#"{"workerId":"w1","token":"agw_secret"}"#).unwrap();
    assert_eq!(response.worker_id, "w1");
    assert_eq!(response.heartbeat_sec, DEFAULT_HEARTBEAT_SECS);
    assert_eq!(response.limits, Limits::default());
    assert!(!format!("{response:?}").contains("agw_secret"));

    let response = parse_register(
        r#"{"workerId":"w1","token":"t","heartbeatSec":5,"limits":{"maxSessions":3,"providers":"codex"},"extra":1}"#,
    )
    .unwrap();
    assert_eq!(response.heartbeat_sec, 5);
    assert_eq!(response.limits.max_sessions, 3);
    assert_eq!(response.limits.providers, "codex");
    assert_eq!(response.limits.usage_max_percent, 90.0);
}

#[test]
fn default_limits_follow_the_contract() {
    let limits = Limits::default();
    assert_eq!(limits.max_sessions, 2);
    assert_eq!(limits.cpu_percent, 80.0);
    assert_eq!(limits.ram_reserve_gb, 4.0);
    assert_eq!(limits.disk_min_free_gb, 20.0);
    assert_eq!(limits.providers, "claude,codex");
    assert_eq!(limits.usage_max_percent, 90.0);
    assert!(limits.require_proxy);
    assert!(!limits.paused);
}

#[test]
fn heartbeat_request_shape() {
    let request = HeartbeatRequest {
        resources: Resources {
            cpu_percent: 12.5,
            ram_free_bytes: 1,
            disk_free_bytes: 2,
        },
        usage: vec![UsageReport {
            provider: "codex".to_owned(),
            window: "primary".to_owned(),
            window_minutes: Some(300),
            used_percent: None,
            resets_at: Some("2026-10-31T18:00:00Z".to_owned()),
            observed_at: None,
            source: "rollout".to_owned(),
        }],
        sessions: vec![SessionBeat {
            session_id: "s1".to_owned(),
            status: SessionStatus::Running,
        }],
        profile: None,
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "resources": {"cpuPercent": 12.5, "ramFreeBytes": 1, "diskFreeBytes": 2},
            "usage": [{
                "provider": "codex", "window": "primary", "windowMinutes": 300,
                "usedPercent": null, "resetsAt": "2026-10-31T18:00:00Z", "observedAt": null,
                "source": "rollout"
            }],
            "sessions": [{"sessionId": "s1", "status": "running"}]
        })
    );
}

#[test]
fn heartbeat_request_carries_the_profile_state() {
    let request = HeartbeatRequest {
        resources: Resources::default(),
        usage: Vec::new(),
        sessions: Vec::new(),
        profile: Some(ProfileBeat {
            version: 7,
            ok: true,
            proxy_check: ProxyCheck {
                ok: false,
                message: "нет ответа".to_owned(),
            },
        }),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap()["profile"],
        json!({"version": 7, "ok": true, "proxyCheck": {"ok": false, "message": "нет ответа"}})
    );
}

#[test]
fn register_response_carries_the_profile() {
    let response = parse_register(
        r#"{"workerId":"w1","token":"t","profileVersion":4,
            "profile":{"version":4,"parts":{"mcp":{"servers":{"chimney":{"url":"u"}}}}}}"#,
    )
    .unwrap();
    assert_eq!(response.profile_version, 4);
    let profile = response.profile.clone().unwrap();
    assert_eq!(profile.version, 4);
    assert_eq!(profile.parts.keys().collect::<Vec<_>>(), vec!["mcp"]);
    // Neither the parts (they hold the substituted secrets) nor the token are in the log.
    let text = format!("{response:?}");
    assert!(!text.contains("chimney"), "{text}");
    assert!(!text.contains("servers"), "{text}");

    let empty = parse_register(r#"{"workerId":"w1","token":"t"}"#).unwrap();
    assert_eq!(empty.profile_version, 0);
    assert_eq!(empty.profile, None);
}

#[test]
fn profile_body_and_applied_report_shapes() {
    let body =
        parse_profile(r#"{"version":9,"parts":{"vault":{"path":"C:/vault"}},"extra":1}"#).unwrap();
    assert_eq!(body.version, 9);
    assert_eq!(body.parts["vault"]["path"], "C:/vault");
    assert_eq!(parse_profile(" ").unwrap(), ProfileBody::default());

    let report = AppliedRequest {
        version: 9,
        ok: false,
        message: "прокси без прав администратора".to_owned(),
        parts: vec![PartReport {
            name: "dnsProxy".to_owned(),
            ok: false,
            message: "нет прав".to_owned(),
        }],
        proxy_check: ProxyCheck {
            ok: false,
            message: "HTTP 503".to_owned(),
        },
    };
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        json!({
            "version": 9,
            "ok": false,
            "message": "прокси без прав администратора",
            "parts": [{"name": "dnsProxy", "ok": false, "message": "нет прав"}],
            "proxyCheck": {"ok": false, "message": "HTTP 503"}
        })
    );
}

#[test]
fn statuses_of_the_preparation_phase() {
    assert_eq!(SessionStatus::Preparing.as_str(), "preparing");
    assert_eq!(SessionStatus::PlanReview.as_str(), "plan-review");
    assert_eq!(
        serde_json::to_value(StatusRequest::new(SessionStatus::PlanReview)).unwrap()["status"],
        "plan-review"
    );
    assert_eq!(
        serde_json::from_str::<SessionStatus>("\"preparing\"").unwrap(),
        SessionStatus::Preparing
    );
}

#[test]
fn heartbeat_response_is_lenient() {
    let response = parse_heartbeat(
        r#"{"paused":true,"commands":[{"type":"stop","sessionId":"s1"},{"type":"reboot"},{"type":"message","sessionId":"s2","text":"hi"}]}"#,
    )
    .unwrap();
    assert!(response.paused);
    assert_eq!(response.limits, None);
    assert_eq!(
        response.commands,
        vec![
            Command::Stop {
                session_id: "s1".to_owned()
            },
            Command::Message {
                session_id: "s2".to_owned(),
                text: "hi".to_owned()
            },
        ]
    );
    assert_eq!(parse_heartbeat("").unwrap(), HeartbeatResponse::default());
}

#[test]
fn heartbeat_response_carries_the_profile_version() {
    let response = parse_heartbeat(r#"{"paused":false,"profileVersion":12}"#).unwrap();
    assert_eq!(response.profile_version, Some(12));
    // A service that knows no profile yet says nothing about it.
    assert_eq!(parse_heartbeat("{}").unwrap().profile_version, None);
}

#[test]
fn approve_and_reject_commands_are_parsed() {
    let response = parse_heartbeat(
        r#"{"commands":[{"type":"approve","sessionId":"s1"},
            {"type":"reject","sessionId":"s2","reason":"не та ветка"},
            {"type":"reject","sessionId":"s3"}]}"#,
    )
    .unwrap();
    assert_eq!(
        response.commands,
        vec![
            Command::Approve {
                session_id: "s1".to_owned()
            },
            Command::Reject {
                session_id: "s2".to_owned(),
                reason: "не та ветка".to_owned()
            },
            Command::Reject {
                session_id: "s3".to_owned(),
                reason: String::new()
            },
        ]
    );
    assert_eq!(response.commands[1].session_id(), "s2");
}

#[test]
fn inbox_parses_assignments_with_providers() {
    let inbox = parse_inbox(
        r#"{"assignments":[{
            "sessionId":"5f0e8a5b-0c3e-4d2f-9a64-3b1e4f6d7a20",
            "taskRef":{"boardId":5,"taskId":5008,"taskNumber":4623},
            "attempt":1,"title":"Встречи","repo":"https://github.com/SteepMans/brain-manager-bot.git",
            "baseBranch":"main","branch":"feature/4623-x","prompt":"p",
            "provider":"codex","model":null,"maxDurationMin":240
        },{"sessionId":"s2","model":"claude-opus-5-5"}],"commands":null}"#,
    )
    .unwrap();
    assert_eq!(inbox.assignments.len(), 2);
    let first = &inbox.assignments[0];
    assert_eq!(first.task_ref.task_number, Some(4623));
    assert_eq!(first.provider, "codex");
    assert_eq!(first.model, None);
    assert_eq!(first.max_duration_min, 240);
    let second = &inbox.assignments[1];
    assert_eq!(second.provider, "");
    assert_eq!(second.model.as_deref(), Some("claude-opus-5-5"));
    assert!(inbox.commands.is_empty());
    assert_eq!(parse_inbox("  ").unwrap(), Inbox::default());
}

#[test]
fn an_assignment_without_a_prompt_and_a_repository_is_parsed() {
    let inbox = parse_inbox(
        r#"{"assignments":[{
            "sessionId":"s1","taskRef":{"boardId":5,"taskId":5024,"taskNumber":4639},
            "attempt":1,"title":"Подготовка задания","repo":null,"baseBranch":null,
            "branch":null,"prompt":null,"provider":"auto","model":null,
            "maxDurationMin":240,"planApproval":true,"prepareMaxDurationMin":45
        }]}"#,
    )
    .unwrap();
    let assignment = &inbox.assignments[0];
    assert_eq!(assignment.repo, "");
    assert_eq!(assignment.base_branch, "");
    assert_eq!(assignment.branch, "");
    assert_eq!(assignment.prompt, "");
    assert_eq!(assignment.title, "Подготовка задания");
    assert!(assignment.plan_approval);
    assert_eq!(assignment.prepare_max_duration_min, Some(45));

    // Nothing but the session id: still an assignment, and it needs the preparation phase.
    let inbox = parse_inbox(r#"{"assignments":[{"sessionId":"s2"}]}"#).unwrap();
    assert_eq!(inbox.assignments[0].session_id, "s2");
    assert!(!inbox.assignments[0].plan_approval);
    assert_eq!(inbox.assignments[0].prepare_max_duration_min, None);
}

#[test]
fn status_request_sends_every_field() {
    let mut request = StatusRequest::new(SessionStatus::Blocked);
    request.provider = Some("claude".to_owned());
    request.reason = Some(REASON_USAGE_LIMIT.to_owned());
    request.resets_at = Some("2026-10-31T20:00:00Z".to_owned());
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "status": "blocked", "provider": "claude", "branch": null, "worktree": null,
            "agentSessionId": null, "claudeSessionId": null, "remoteControlUrl": null,
            "prUrl": null, "message": null, "exitCode": null, "reason": "usage_limit",
            "resetsAt": "2026-10-31T20:00:00Z"
        })
    );
}

#[test]
fn ack_bodies() {
    assert_eq!(
        serde_json::to_value(AckRequest::accept()).unwrap(),
        json!({"accepted": true})
    );
    assert_eq!(
        serde_json::to_value(AckRequest::reject("no free slot")).unwrap(),
        json!({"accepted": false, "reason": "no free slot"})
    );
}
