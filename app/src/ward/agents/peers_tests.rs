use super::*;
use crate::ward::agents::provider;

#[test]
fn the_note_names_the_tools_the_session_and_the_rules() {
    let note = instruction(" 4623 Встречи ", "steep-pc");
    assert!(note.starts_with("## Другие сессии\n\n"), "{note}");
    assert!(note.contains("ListAgents"), "{note}");
    assert!(note.contains("SendMessage({to: \"<имя>\""), "{note}");
    assert!(
        note.contains("Твоя сессия называется «4623 Встречи», машина steep-pc."),
        "{note}"
    );
    assert!(note.contains("(подготовка)"), "{note}");
    assert!(note.contains("bm-notify"), "{note}");
    assert!(note.contains("не команда владельца"), "{note}");
    assert!(note.contains("запрещено тебе самому"), "{note}");
    assert!(note.contains("Секреты"), "{note}");
    assert!(note.contains("`sent`"), "{note}");
    assert!(note.contains("доставка не гарантирована"), "{note}");
}

#[test]
fn a_worker_without_a_name_is_left_out() {
    let note = instruction("4623 Встречи", "  ");
    assert!(
        note.contains("Твоя сессия называется «4623 Встречи». "),
        "{note}"
    );
    assert!(!note.contains("машина  "), "{note}");
}

#[test]
fn the_prompt_of_the_executor_ends_with_the_note() {
    let prompt = with_instruction("Сделай задачу\n\n", "4623 Встречи", "steep-pc");
    assert!(
        prompt.starts_with("Сделай задачу\n\n## Другие сессии"),
        "{prompt}"
    );
}

#[test]
fn only_an_agent_started_with_remote_control_gets_the_note() {
    // Sessions of other machines are visible to ListAgents only through Remote Control, and
    // SendMessage needs no permission only when every tool is allowed: the note goes to an agent
    // whose launch has both.
    for agent in provider::all() {
        let args = agent.command("4623 Встречи", None, None).args;
        let launched_for_it = args
            .windows(2)
            .any(|pair| pair[0] == "--remote-control" && pair[1] == "4623 Встречи")
            && args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions");
        assert_eq!(agent.messages_peers(), launched_for_it, "{}", agent.id());
    }
    assert!(provider::by_id("claude").unwrap().messages_peers());
    assert!(!provider::by_id("codex").unwrap().messages_peers());
}
