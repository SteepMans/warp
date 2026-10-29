use std::time::{Duration, SystemTime};

use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn keeps_flags_and_drops_prompt() {
    let launch = parse_launch(&argv(&[
        "codex",
        "-m",
        "gpt-5.5",
        "--dangerously-bypass-approvals-and-sandbox",
        "-c",
        "model_reasoning_effort=high",
        "fix the tests",
    ]))
    .unwrap();
    assert_eq!(launch.resumed_id, None);
    assert_eq!(
        build_resume_command(
            &launch.preserved,
            "01a0198b-ac42-7602-91d9-db473d42b664",
            None
        ),
        "codex resume -m gpt-5.5 --dangerously-bypass-approvals-and-sandbox -c model_reasoning_effort=high 01a0198b-ac42-7602-91d9-db473d42b664"
    );
}

#[test]
fn reads_id_of_resumed_session() {
    let launch = parse_launch(&argv(&[
        "C:\\Codex\\bin\\codex.exe",
        "resume",
        "--model=gpt-5.5",
        "01a0198b-ac42-7602-91d9-db473d42b664",
    ]))
    .unwrap();
    assert_eq!(
        launch.resumed_id.as_deref(),
        Some("01a0198b-ac42-7602-91d9-db473d42b664")
    );
    assert_eq!(launch.preserved, argv(&["--model", "gpt-5.5"]));
}

#[test]
fn drops_cd_and_image() {
    let launch = parse_launch(&argv(&[
        "codex", "-C", "D:\\repo", "-i", "shot.png", "--search",
    ]))
    .unwrap();
    assert_eq!(launch.preserved, argv(&["--search"]));
}

#[test]
fn non_interactive_runs_are_not_resumable() {
    assert_eq!(parse_launch(&argv(&["codex", "exec", "do it"])), None);
    assert_eq!(parse_launch(&argv(&["codex", "login"])), None);
}

#[test]
fn quotes_for_powershell() {
    assert_eq!(
        build_resume_command(&argv(&["-c", "a='b c'"]), "id", Some(ShellType::PowerShell)),
        "codex resume -c 'a=''b c''' id"
    );
}

fn write_rollout(dir: &Path, name: &str, id: &str, cwd: &str, thread_source: &str) {
    let line = serde_json::json!({
        "timestamp": "2026-10-29T10:00:00.000Z",
        "type": "session_meta",
        "payload": {"id": id, "cwd": cwd, "thread_source": thread_source},
    });
    std::fs::write(dir.join(name), format!("{line}\n{{\"type\":\"event\"}}\n")).unwrap();
}

#[test]
fn picks_latest_top_level_session_in_cwd() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join(day_key(SystemTime::now()));
    std::fs::create_dir_all(&day).unwrap();
    let old_day = root.path().join("2020/01/01");
    std::fs::create_dir_all(&old_day).unwrap();

    write_rollout(&old_day, "rollout-old.jsonl", "old", "C:\\repo", "user");
    write_rollout(&day, "rollout-a.jsonl", "a", "C:\\repo", "user");
    std::thread::sleep(Duration::from_millis(50));
    write_rollout(
        &day,
        "rollout-other.jsonl",
        "other",
        "C:\\elsewhere",
        "user",
    );
    write_rollout(&day, "rollout-sub.jsonl", "sub", "C:\\repo", "subagent");

    let started = SystemTime::now() - Duration::from_secs(60);
    let found = latest_session(root.path(), started, Some(Path::new("c:/repo/"))).unwrap();
    assert_eq!(found, ("a".to_owned(), Some("C:\\repo".to_owned())));
}

#[test]
fn ignores_rollouts_older_than_process() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join(day_key(SystemTime::now()));
    std::fs::create_dir_all(&day).unwrap();
    write_rollout(&day, "rollout-a.jsonl", "a", "C:\\repo", "user");

    let started = SystemTime::now() + Duration::from_secs(60);
    assert_eq!(latest_session(root.path(), started, None), None);
}
