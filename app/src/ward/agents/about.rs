//! What a session is about, for the Agents screen: one or two lines and the project tags.
//!
//! The orchestrator gives the first ones with its plan ([`super::plan::Plan`]). A session often
//! goes on past its card — the owner writes the next task into its chat — so the executor keeps
//! them current itself: Ward adds a short note to its prompt, the agent rewrites
//! `<worktree>/.agents/about.json` when it takes up or finishes a task, and Ward sends what the
//! file says with the next status of the session. That costs no extra call of the model: the
//! agent writes the file in the turn it does anyway. The file is written by an agent, so it is
//! read leniently, and the service cleans the values up once more.

use std::path::Path;

/// The executor's own account of the session, in `.agents` of its worktree.
pub const ABOUT_FILE: &str = "about.json";

/// Largest `about.json` Ward reads.
pub const MAX_ABOUT_BYTES: u64 = 16 * 1024;

/// The service keeps no more than this of the summary either.
const MAX_SUMMARY_CHARS: usize = 500;

/// Tags Ward sends at most.
const MAX_TAGS: usize = 6;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct About {
    /// One or two lines: what the session does now.
    pub summary: Option<String>,
    /// Project tags, lowercase, without repeats.
    pub tags: Vec<String>,
}

/// Reads the object out of the file's text; `None` when it says nothing usable.
pub fn parse(text: &str) -> Option<About> {
    let text = text.trim();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let summary = value
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(|summary| {
            summary
                .trim()
                .chars()
                .take(MAX_SUMMARY_CHARS)
                .collect::<String>()
        })
        .map(|summary| summary.trim().to_owned())
        .filter(|summary| !summary.is_empty());
    let tags = tags(value.get("tags"));
    if summary.is_none() && tags.is_empty() {
        return None;
    }
    Some(About { summary, tags })
}

/// Tags as an agent writes them: an array of strings or one string with commas. Anything else
/// in the array is skipped instead of losing the rest.
pub fn tags(value: Option<&serde_json::Value>) -> Vec<String> {
    let raw: Vec<String> = match value {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(serde_json::Value::String(text)) => text.split(',').map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag
            .trim()
            .trim_start_matches('#')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !tag.is_empty() && !out.contains(&tag) && out.len() < MAX_TAGS {
            out.push(tag);
        }
    }
    out
}

/// What `about.json` in `agents_dir` says now; `None` without a usable file.
pub fn read(agents_dir: &Path) -> Option<About> {
    let path = agents_dir.join(ABOUT_FILE);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ABOUT_BYTES {
        return None;
    }
    parse(&std::fs::read_to_string(&path).ok()?)
}

/// The note Ward adds to the end of the executor's prompt. `path` is where `about.json` goes.
pub fn instruction(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    format!(
        "## Для экрана агентов\n\n\
         Сессия может вести несколько задач: владелец дописывает новые в чат. Когда берёшься за \
         задачу и когда заканчиваешь её, перезапиши файл {path} одним объектом JSON:\n\n\
         ```json\n\
         {{\"summary\": \"<одна-две строки: чем сессия занята сейчас>\", \"tags\": [\"<проект>\", \"<модуль>\"]}}\n\
         ```\n\n\
         Теги — одно-четыре коротких слова в нижнем регистре: проект, репозиторий, модуль. Файл \
         лежит вне репозитория и в коммиты не попадает; владельцу о нём отдельно не пиши.\n"
    )
}

/// The executor's prompt with [`instruction`] after it.
pub fn with_instruction(prompt: &str, path: &Path) -> String {
    format!("{}\n\n{}", prompt.trim_end(), instruction(path))
}

#[cfg(test)]
#[path = "about_tests.rs"]
mod tests;
