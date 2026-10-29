use super::*;

const MINUTE: Duration = Duration::from_secs(60);

fn agent(id: usize, state: AgentState, minutes: u64, project: &str, task: &str) -> PaletteAgent {
    PaletteAgent {
        terminal_view_id: EntityId::from_usize(id),
        state,
        in_state: minutes * MINUTE,
        project: Some(project.to_owned()),
        task: task.to_owned(),
        detail: None,
    }
}

fn ids(agents: &[PaletteAgent]) -> Vec<EntityId> {
    agents.iter().map(|agent| agent.terminal_view_id).collect()
}

fn expected(ids: &[usize]) -> Vec<EntityId> {
    ids.iter().copied().map(EntityId::from_usize).collect()
}

#[test]
fn sorts_by_urgency_then_waiting_time() {
    let agents = vec![
        agent(1, AgentState::RunningTool, 3, "hassle", "planka-sync"),
        agent(2, AgentState::WaitingAnswer, 14, "hassle", "payments-refund-webhook"),
        agent(3, AgentState::Error, 6, "brain-manager", "telegram-notify-retry"),
        agent(4, AgentState::WaitingPermission, 1, "karandash", "budget-llm-limits"),
        agent(5, AgentState::WaitingAnswer, 30, "chimney", "backlog-stats"),
        agent(6, AgentState::Done, 2, "hassle", "auth-token-rotation"),
    ];
    assert_eq!(ids(&filter_and_sort(agents, "")), expected(&[4, 5, 2, 3, 1, 6]));
}

#[test]
fn fuzzy_search_covers_project_task_and_question() {
    let mut question = agent(1, AgentState::WaitingAnswer, 5, "hassle", "refund-webhook");
    question.detail = Some("Apply migration 0042 to stage?".to_owned());
    let agents = vec![
        question,
        agent(2, AgentState::RunningTool, 3, "karandash", "budget-llm-limits"),
        agent(3, AgentState::Done, 3, "payments", "cleanup"),
    ];
    assert_eq!(ids(&filter_and_sort(agents.clone(), "migr")), expected(&[1]));
    assert_eq!(ids(&filter_and_sort(agents.clone(), "budget")), expected(&[2]));
    assert_eq!(ids(&filter_and_sort(agents.clone(), "pay")), expected(&[3]));
    assert!(filter_and_sort(agents, "zzzz").is_empty());
}

#[test]
fn section_labels_follow_columns() {
    assert_eq!(section_label(AgentState::WaitingPermission), "Waiting for you");
    assert_eq!(section_label(AgentState::Error), "Errors");
    assert_eq!(section_label(AgentState::Thinking), "Running");
    assert_eq!(section_label(AgentState::Idle), "Done & idle");
}
