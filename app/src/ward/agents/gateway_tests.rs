use super::*;

/// What the gateway answers with for the list of sessions, fields Ward does not read included.
const SESSIONS: &str = r#"[
  {
    "id": "7f1c0a3e-1111-4a2b-9c3d-5e6f7a8b9c0d",
    "taskRef": {"boardId": 5, "taskId": 5037, "taskNumber": 4652},
    "attempt": 1,
    "title": "  Окно сессии в Ward  ",
    "status": "running",
    "worker": "steep-pc",
    "provider": "claude",
    "branch": "feature/4652-ward-session-chat",
    "repo": "https://github.com/SteepMans/warp.git",
    "message": "Сессия запущена",
    "queueReason": null,
    "createdAt": "2026-11-02T10:00:00Z",
    "finishedAt": null
  },
  {
    "id": "8f1c0a3e-2222-4a2b-9c3d-5e6f7a8b9c0d",
    "taskRef": {"boardId": 5, "taskId": 5000},
    "title": "",
    "status": "review",
    "worker": null
  }
]"#;

#[test]
fn the_list_of_sessions_keeps_what_ward_reads() {
    let sessions = parse_sessions(SESSIONS).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].id, "7f1c0a3e-1111-4a2b-9c3d-5e6f7a8b9c0d");
    assert_eq!(sessions[0].worker.as_deref(), Some("steep-pc"));
    assert_eq!(sessions[0].provider.as_deref(), Some("claude"));
    assert_eq!(sessions[0].label(), "#4652 Окно сессии в Ward");
    assert!(sessions[0].is_live());

    // No task number: the card id names the session; an empty title has its own word.
    assert_eq!(sessions[1].label(), "#5000 без названия");
    // A session that is over has no worker to send a command to.
    assert!(!sessions[1].is_live());

    // A list from a service that says nothing Ward knows is still a list.
    assert_eq!(parse_sessions("[]").unwrap(), Vec::new());
    assert!(parse_sessions("{}").is_err());
}

#[test]
fn a_journal_chunk_is_cut_at_the_last_whole_line() {
    let (lines, bytes) = whole_lines("{\"a\":1}\n{\"b\":2}\n");
    assert_eq!(lines, "{\"a\":1}\n{\"b\":2}\n");
    assert_eq!(bytes, 16);

    // The chunk ends in the middle of a line: that line waits for the next offset.
    let (lines, bytes) = whole_lines("{\"a\":1}\n{\"b\":");
    assert_eq!(lines, "{\"a\":1}\n");
    assert_eq!(bytes, 8);

    // Nothing whole yet, so nothing is taken and the offset does not move.
    assert_eq!(whole_lines("{\"a\""), ("", 0));
    assert_eq!(whole_lines(""), ("", 0));

    // The count is in bytes, as the service counts them.
    let (lines, bytes) = whole_lines("{\"t\":\"привет\"}\n");
    assert_eq!(bytes, lines.len() as u64);
    assert_eq!(bytes, 21);
}

/// A journal that grew to tens of megabytes is not a failed read: its end is kept, cut at a
/// line boundary, and what was dropped is counted so the offset stays right.
#[test]
fn a_chunk_bigger_than_the_cap_keeps_its_end() {
    let line = "{\"type\":\"assistant\",\"n\":1}\n";
    let small = line.repeat(3);
    assert_eq!(
        cap(small.clone()),
        Chunk {
            text: small,
            skipped: 0
        }
    );

    // One line past the cap: the chunk starts at the next whole line, and `skipped` is exactly
    // the bytes in front of it.
    let times = MAX_CHUNK_BYTES / line.len() + 2;
    let big = line.repeat(times);
    let chunk = cap(big.clone());
    assert!(chunk.skipped > 0);
    assert!(chunk.text.len() <= MAX_CHUNK_BYTES, "{}", chunk.text.len());
    assert_eq!(chunk.skipped as usize + chunk.text.len(), big.len());
    assert!(chunk.text.starts_with(line), "cut at a line boundary");
    assert!(big.ends_with(&chunk.text));
}
