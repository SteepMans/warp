//! The plan of the preparation phase: what the orchestrator leaves in `plan.json` and how it
//! fills the blanks of an assignment.
//!
//! The fields are those of `POST /sessions/{id}/plan` in the contract note «Agents —
//! контракт». The file is written by an agent, so it is parsed leniently: `null` counts as
//! absent, numbers may come as strings, unknown fields are ignored. What the assignment
//! already said is never overridden — the orchestrator only fills what was missing.

use serde::{Deserialize, Serialize};

use super::contract::Assignment;

/// The machine readable result of the preparation phase, in the session's work directory.
pub const PLAN_FILE: &str = "plan.json";

/// Largest `plan.json` Ward reads.
pub const MAX_PLAN_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Plan {
    #[serde(deserialize_with = "string_or_empty")]
    pub repo: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub base_branch: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub branch: String,
    #[serde(deserialize_with = "string_or_empty")]
    pub prompt: String,
    /// The dossier in the vault, relative to its root.
    #[serde(deserialize_with = "string_or_empty")]
    pub dossier_path: String,
    /// `run-ГГГГММДД-NNN` of the playbook.
    #[serde(deserialize_with = "string_or_empty")]
    pub run_id: String,
    /// The agent the orchestrator had in mind. The service picks the agent itself, by the
    /// subscription limits of the machine, and ignores this; so does Ward, which keeps the one
    /// chosen when the assignment was taken. The field travels for the card and the dossier.
    #[serde(deserialize_with = "string_or_empty")]
    pub provider: String,
    pub model: Option<String>,
    /// Two or three lines for the card.
    #[serde(deserialize_with = "string_or_empty")]
    pub summary: String,
    /// Cards the orchestrator decided to do in one session.
    #[serde(deserialize_with = "lenient_numbers")]
    pub grouped: Vec<u64>,
}

fn string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .unwrap_or_default()
        .trim()
        .to_owned())
}

/// Card numbers, whether the agent wrote them as numbers or as strings; anything else is
/// skipped instead of losing the whole plan.
fn lenient_numbers<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Option::<Vec<serde_json::Value>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(values
        .into_iter()
        .filter_map(|value| match value {
            serde_json::Value::Number(number) => number.as_u64(),
            serde_json::Value::String(text) => text.trim().parse().ok(),
            _ => None,
        })
        .collect())
}

/// Reads the plan out of the file's text. The error is the message for the owner.
pub fn parse(text: &str) -> Result<Plan, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("plan.json пустой".to_owned());
    }
    // An agent may wrap the object in a fenced block; take the object itself.
    let text = unfence(text);
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|err| format!("plan.json не разбирается как JSON: {err}"))?;
    if !value.is_object() {
        return Err("plan.json должен быть объектом JSON".to_owned());
    }
    serde_json::from_value(value).map_err(|err| format!("plan.json не по контракту: {err}"))
}

/// The JSON object inside a ```json fence, or the text as it is.
fn unfence(text: &str) -> &str {
    let Some(start) = text.find('{') else {
        return text;
    };
    let Some(end) = text.rfind('}') else {
        return text;
    };
    if start <= end {
        &text[start..=end]
    } else {
        text
    }
}

/// Fills the assignment from the plan: what the service already said stays as it is, what it
/// left out comes from the plan. The error says what is still missing.
pub fn apply(plan: &Plan, assignment: &mut Assignment) -> Result<(), String> {
    fill(&mut assignment.repo, &plan.repo);
    fill(&mut assignment.base_branch, &plan.base_branch);
    fill(&mut assignment.branch, &plan.branch);
    fill(&mut assignment.prompt, &plan.prompt);
    if assignment
        .model
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        assignment.model = plan
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned);
    }
    let mut missing = Vec::new();
    if assignment.repo.trim().is_empty() {
        missing.push("репозиторий");
    }
    if assignment.branch.trim().is_empty() {
        missing.push("ветка");
    }
    if assignment.prompt.trim().is_empty() {
        missing.push("промпт");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("в плане нет: {}", missing.join(", ")))
    }
}

fn fill(target: &mut String, value: &str) {
    if target.trim().is_empty() {
        *target = value.trim().to_owned();
    }
}

/// One or two lines about the plan for the panel and the card.
pub fn summary(plan: &Plan) -> String {
    let mut text = plan.summary.trim().to_owned();
    if text.is_empty() {
        text = format!("{} → {}", plan.repo, plan.branch);
    }
    if !plan.dossier_path.is_empty() {
        text.push_str(&format!(" · досье {}", plan.dossier_path));
    }
    if !plan.run_id.is_empty() {
        text.push_str(&format!(" · {}", plan.run_id));
    }
    text
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
