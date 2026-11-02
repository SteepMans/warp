use super::*;

fn open_of(provider: &str) -> Open {
    Open {
        summary: Summary {
            id: "s-1".to_owned(),
            provider: Some(provider.to_owned()),
            ..Summary::default()
        },
        offset: 0,
        chat: VecDeque::new(),
        error: None,
        sending: false,
        send_error: None,
        loaded: false,
    }
}

/// A chunk the service sent whole, nothing dropped from its beginning.
fn chunk(text: &str) -> Chunk {
    Chunk {
        text: text.to_owned(),
        skipped: 0,
    }
}

fn lines(open: &Open) -> Vec<String> {
    open.chat()
        .map(|said| format!("{}: {}", said.voice.label(), said.text))
        .collect()
}

/// The chat of a Claude session out of its journal: the owner, the agent, the tools it called
/// and Ward's own markers, with the entries that are no reply left out. Every chunk moves the
/// offset by exactly the bytes of the whole lines it brought.
#[test]
fn the_chat_grows_chunk_by_chunk_without_rereading() {
    let mut open = open_of("claude");
    let first = concat!(
        "{\"type\":\"ward.role\",\"role\":\"executor\",\"sessionId\":\"s-1\"}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"сделай окно сессии\"}}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Смотрю код\"},",
        "{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"cargo test\"}}]}}\n",
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\"}]}}\n",
    );
    open.absorb(&chunk(first));
    assert_eq!(open.offset, first.len() as u64);
    assert_eq!(
        lines(&open),
        [
            "Ward: Дальше транскрипт: executor",
            "Владелец: сделай окно сессии",
            "Агент: Смотрю код",
            "Инструмент: Bash: cargo test",
        ]
    );

    // The next chunk ends in the middle of a line: that line is not read and the offset stops
    // in front of it, so the next request brings it back whole.
    let answer = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Готово\"}]}}\n";
    let half = format!("{answer}{{\"type\":\"assi");
    open.absorb(&chunk(&half));
    assert_eq!(open.offset, (first.len() + answer.len()) as u64);
    assert_eq!(lines(&open).last().unwrap(), "Агент: Готово");

    // Nothing whole at all: nothing is read and nothing moves.
    let offset = open.offset;
    open.absorb(&chunk("{\"type\":\"assi"));
    assert_eq!(open.offset, offset);
    assert_eq!(lines(&open).len(), 5);
}

/// A Codex session reads the same way out of its own rollout events.
#[test]
fn the_chat_of_a_codex_session_reads_its_rollout() {
    let mut open = open_of("codex");
    open.absorb(&chunk(concat!(
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"собери\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_reasoning\",\"text\":\"думаю\"}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",",
        "\"arguments\":\"{\\\"command\\\":\\\"cargo build\\\"}\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Собрано\"}}\n",
    )));
    assert_eq!(
        lines(&open),
        [
            "Владелец: собери",
            "Инструмент: shell: cargo build",
            "Агент: Собрано",
        ]
    );
}

/// A session without a provider still reads: the first provider Ward knows is tried, and a
/// line it makes nothing of is skipped instead of breaking the chat.
#[test]
fn a_session_without_a_provider_and_broken_lines_do_not_break_the_chat() {
    let mut open = Open {
        summary: Summary::default(),
        ..open_of("claude")
    };
    open.absorb(&chunk(concat!(
        "not json\n",
        "{\"type\":\"summary\",\"summary\":\"итог\"}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Есть\"}]}}\n",
    )));
    assert_eq!(lines(&open), ["Агент: Есть"]);
}

/// The chat keeps the newest lines only: a session that ran for hours does not grow for ever.
#[test]
fn the_chat_keeps_the_newest_lines() {
    let mut open = open_of("claude");
    let mut journal = String::new();
    for index in 0..MAX_CHAT_LINES + 10 {
        journal.push_str(&format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ход {index}\"}}]}}}}\n"
        ));
    }
    open.absorb(&chunk(&journal));
    let lines = lines(&open);
    assert_eq!(lines.len(), MAX_CHAT_LINES);
    assert_eq!(lines[0], "Агент: ход 10");
    assert_eq!(
        lines[MAX_CHAT_LINES - 1],
        format!("Агент: ход {}", MAX_CHAT_LINES + 9)
    );
}

/// A chunk whose beginning the gateway layer had to drop still moves the offset by the bytes
/// nobody read, so the next request asks for the tail and not for them again.
#[test]
fn a_chunk_that_dropped_its_beginning_still_moves_the_offset() {
    let mut open = open_of("claude");
    let answer = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Хвост\"}]}}\n";
    open.absorb(&Chunk {
        text: answer.to_owned(),
        skipped: 10_000,
    });
    assert_eq!(open.offset, 10_000 + answer.len() as u64);
    assert_eq!(lines(&open), ["Агент: Хвост"]);
}
