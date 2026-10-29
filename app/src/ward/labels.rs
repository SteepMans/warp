//! Human names of a CLI agent for the overview board, the agents palette and the footer:
//! the project it works on and the task it was started for.

use super::project_groups::remote_control_session_name;

/// Longest task name kept; rows and cards clip it further.
const MAX_TASK_CHARS: usize = 80;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentLabels {
    /// `hassle` in `claude --remote-control 'hassle. payments-refund-webhook'`, else the name of
    /// the repo (or working) directory.
    pub project: Option<String>,
    /// `payments-refund-webhook` in the example above, else the user's latest prompt, else the
    /// agent's name.
    pub task: String,
}

/// Splits a remote-control session name `<project>. <task>` into its parts. Names without a
/// `<project>.` prefix are all task.
pub fn split_session_name(name: &str) -> (Option<String>, Option<String>) {
    let name = name.trim();
    match name.split_once('.') {
        Some((project, task))
            if !project.trim().is_empty() && !project.trim().contains(char::is_whitespace) =>
        {
            let task = task.trim();
            (
                Some(project.trim().to_owned()),
                (!task.is_empty()).then(|| task.to_owned()),
            )
        }
        _ => (None, (!name.is_empty()).then(|| name.to_owned())),
    }
}

/// Last component of a directory path, accepting both separators.
pub fn directory_name(path: &str) -> Option<String> {
    path.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.ends_with(':'))
        .map(str::to_owned)
}

/// Pure label derivation from what is known about the agent.
pub fn agent_labels(
    command: Option<&str>,
    directory: Option<&str>,
    latest_prompt: Option<&str>,
    agent_name: &str,
) -> AgentLabels {
    let (session_project, session_task) = command
        .and_then(remote_control_session_name)
        .map(|name| split_session_name(&name))
        .unwrap_or_default();
    let project = session_project.or_else(|| directory.and_then(directory_name));
    let task = session_task
        .or_else(|| {
            latest_prompt
                .map(|prompt| prompt.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|prompt| !prompt.is_empty())
        })
        .unwrap_or_else(|| agent_name.to_owned());
    AgentLabels {
        project,
        task: clip_chars(&task, MAX_TASK_CHARS),
    }
}

pub fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('\u{2026}');
    clipped
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
