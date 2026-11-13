use std::path::Path;

use serde_json::json;

use super::*;

const ROOT: &str = "01a09473-8407-7391-a64f-dc3385603d60";
const GUARDIAN: &str = "01a1d412-f5f5-7e50-b215-aed4851f118e";
const NESTED: &str = "01a1e000-0000-7000-8000-000000000001";

fn line(payload: serde_json::Value) -> String {
    json!({"timestamp": "2026-11-13T10:00:00.000Z", "type": "session_meta", "payload": payload})
        .to_string()
}

fn write_rollout(sessions: &Path, day: &str, id: &str, payload: serde_json::Value) {
    let dir = sessions.join(day);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-2026-11-13T10-00-00-{id}.jsonl")),
        format!("{}\n{{\"type\":\"event_msg\"}}\n", line(payload)),
    )
    .unwrap();
}

fn root_payload() -> serde_json::Value {
    json!({
        "id": ROOT, "session_id": ROOT, "cwd": "C:\\work\\special-order",
        "source": "cli", "thread_source": "user",
    })
}

#[test]
fn session_the_user_started_is_root() {
    assert_eq!(thread_origin(&root_payload()), ThreadOrigin::Root);
    // Older Codex writes neither `session_id` nor `thread_source`.
    assert_eq!(
        thread_origin(&json!({"id": ROOT, "cwd": "C:\\repo"})),
        ThreadOrigin::Root
    );
    // A fork is a session of its own.
    assert_eq!(
        thread_origin(&json!({
            "id": ROOT, "session_id": ROOT, "forked_from_id": GUARDIAN,
            "source": "cli", "thread_source": "user",
        })),
        ThreadOrigin::Root
    );
}

#[test]
fn sub_agent_names_its_parent() {
    // As Codex writes a spawned sub-agent.
    let payload = json!({
        "session_id": ROOT, "id": NESTED, "forked_from_id": ROOT, "parent_thread_id": ROOT,
        "source": {"subagent": {"thread_spawn": {"parent_thread_id": ROOT, "depth": 1}}},
        "thread_source": "subagent",
    });
    assert_eq!(
        thread_origin(&payload),
        ThreadOrigin::Child {
            parent: Some(ROOT.to_owned())
        }
    );
}

#[test]
fn guardian_review_thread_is_a_child() {
    // Only the session it belongs to.
    assert_eq!(
        thread_origin(&json!({"id": GUARDIAN, "session_id": ROOT, "source": "cli"})),
        ThreadOrigin::Child {
            parent: Some(ROOT.to_owned())
        }
    );
    // Only the source.
    assert_eq!(
        thread_origin(&json!({"id": GUARDIAN, "source": {"subagent": "guardian_review"}})),
        ThreadOrigin::Child { parent: None }
    );
    assert_eq!(
        thread_origin(&json!({"id": GUARDIAN, "source": "guardian"})),
        ThreadOrigin::Child { parent: None }
    );
    // Only the thread source.
    assert_eq!(
        thread_origin(&json!({"id": GUARDIAN, "thread_source": "guardian"})),
        ThreadOrigin::Child { parent: None }
    );
}

#[test]
fn reads_the_payload_of_the_first_line() {
    let payload = parse_session_meta(&line(root_payload())).unwrap();
    assert_eq!(thread_id(&payload).as_deref(), Some(ROOT));
    assert_eq!(
        parse_session_meta("{\"type\":\"event_msg\",\"payload\":{}}"),
        None
    );
    assert_eq!(parse_session_meta("not json"), None);
}

#[test]
fn root_is_found_through_the_chain_of_parents() {
    let sessions = tempfile::tempdir().unwrap();
    write_rollout(sessions.path(), "2026/11/12", ROOT, root_payload());
    write_rollout(
        sessions.path(),
        "2026/11/13",
        GUARDIAN,
        json!({"id": GUARDIAN, "parent_thread_id": ROOT, "thread_source": "guardian"}),
    );
    write_rollout(
        sessions.path(),
        "2026/11/13",
        NESTED,
        json!({"id": NESTED, "parent_thread_id": GUARDIAN, "thread_source": "subagent"}),
    );

    assert_eq!(root_session_id(sessions.path(), NESTED), ROOT);
    assert_eq!(root_session_id(sessions.path(), GUARDIAN), ROOT);
    assert_eq!(root_session_id(sessions.path(), ROOT), ROOT);
    // A thread without a rollout is taken as it is.
    assert_eq!(
        root_session_id(sessions.path(), "01a1ffff-0000-7000-8000-000000000000"),
        "01a1ffff-0000-7000-8000-000000000000"
    );
}

#[test]
fn a_loop_of_parents_ends() {
    let sessions = tempfile::tempdir().unwrap();
    write_rollout(
        sessions.path(),
        "2026/11/13",
        GUARDIAN,
        json!({"id": GUARDIAN, "parent_thread_id": NESTED}),
    );
    write_rollout(
        sessions.path(),
        "2026/11/13",
        NESTED,
        json!({"id": NESTED, "parent_thread_id": GUARDIAN}),
    );
    let root = root_session_id(sessions.path(), GUARDIAN);
    assert!(root == GUARDIAN || root == NESTED);
}

#[test]
fn saved_resume_of_a_child_turns_into_the_root() {
    let sessions = tempfile::tempdir().unwrap();
    write_rollout(sessions.path(), "2026/11/12", ROOT, root_payload());
    write_rollout(
        sessions.path(),
        "2026/11/13",
        GUARDIAN,
        json!({"id": GUARDIAN, "session_id": ROOT, "source": {"subagent": "guardian"}}),
    );

    assert_eq!(
        resume_root_command(
            sessions.path(),
            &format!("codex resume -m gpt-5.5 --search {GUARDIAN}")
        ),
        Some(format!("codex resume -m gpt-5.5 --search {ROOT}"))
    );
    // The root itself and other commands stay as they are.
    assert_eq!(
        resume_root_command(sessions.path(), &format!("codex resume {ROOT}")),
        None
    );
    assert_eq!(
        resume_root_command(sessions.path(), &format!("claude --resume {GUARDIAN}")),
        None
    );
}

#[test]
fn resumed_id_of_codex_commands_only() {
    assert_eq!(
        resumed_id(&format!("codex resume -c 'a=b' {GUARDIAN}")),
        Some(GUARDIAN)
    );
    assert_eq!(
        resumed_id(&format!(r"C:\tools\codex.exe resume '{GUARDIAN}'")),
        Some(GUARDIAN)
    );
    assert_eq!(resumed_id("codex resume --last"), None);
    assert_eq!(resumed_id(&format!("codex fork {GUARDIAN}")), None);
    assert_eq!(resumed_id(&format!("claude --resume {GUARDIAN}")), None);
}

const ERROR: &str = "Error: Cannot resume a Guardian review thread. Resume its parent session \
                     instead: codex resume 01a09473-8407-7391-a64f-dc3385603d60";

#[test]
fn parent_is_read_from_the_error() {
    assert_eq!(parent_from_resume_error(ERROR).as_deref(), Some(ROOT));
    // The terminal wrapped the line in the middle of the id.
    let wrapped = "Error: Cannot resume a Guardian review thread. Resume its parent ses\nsion \
                   instead: codex resume 01a09473-8407-7391-a6\n4f-dc3385603d60\n";
    assert_eq!(parent_from_resume_error(wrapped).as_deref(), Some(ROOT));
    assert_eq!(parent_from_resume_error("Error: no such session"), None);
    assert_eq!(
        parent_from_resume_error("Resume its parent session instead: codex resume 01a0"),
        None
    );
}

#[test]
fn failed_resume_is_retried_with_the_parent() {
    assert_eq!(
        retry_after_failed_resume(&format!("codex resume -m gpt-5.5 {GUARDIAN}"), ERROR),
        Some(format!("codex resume -m gpt-5.5 {ROOT}"))
    );
    // Not a resume of Codex, another failure, or the parent itself failed: nothing to retry.
    assert_eq!(retry_after_failed_resume("ls", ERROR), None);
    assert_eq!(
        retry_after_failed_resume(&format!("codex resume {GUARDIAN}"), "Error: quota"),
        None
    );
    assert_eq!(
        retry_after_failed_resume(&format!("codex resume {ROOT}"), ERROR),
        None
    );
}

#[test]
fn session_ids_are_uuids() {
    assert!(looks_like_session_id(ROOT));
    assert!(!looks_like_session_id(
        "01a09473840773917a64fdc3385603d60aaa"
    ));
    assert!(!looks_like_session_id("--dangerously"));
}
