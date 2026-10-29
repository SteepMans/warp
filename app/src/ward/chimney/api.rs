//! Chimney REST shapes Ward reads (`GET /api/v1/task-boards/{id}`) and the pure helpers on top:
//! card status for chips and card lists for pickers. Field names follow the chimney-mcp types;
//! everything Ward does not need is ignored, everything optional has a default.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub columns: Vec<BoardColumn>,
    #[serde(default)]
    pub labels: Vec<BoardLabel>,
    #[serde(default)]
    pub tasks: Vec<BoardTask>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardColumn {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    /// `DEFAULT | BACKLOG | ACTIVE | PROGRESS | DONE | DONE_SPRINT`.
    #[serde(default)]
    pub column_type: Option<String>,
    #[serde(default)]
    pub order: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardLabel {
    pub id: u64,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardTask {
    pub id: u64,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub column_id: Option<u64>,
    #[serde(default)]
    pub order: Option<f64>,
    #[serde(default)]
    pub label_ids: Option<Vec<u64>>,
    #[serde(default)]
    pub archived: Option<bool>,
}

pub fn parse_board(json: &str) -> Result<Board, serde_json::Error> {
    serde_json::from_str(json)
}

/// How far along a column is, from the point of view of picking work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColumnKind {
    Active,
    Progress,
    Sprint,
    Backlog,
    Done,
    DoneSprint,
}

impl ColumnKind {
    pub fn of(column_type: Option<&str>) -> Self {
        match column_type.map(str::to_ascii_uppercase).as_deref() {
            Some("ACTIVE") => Self::Active,
            Some("PROGRESS") => Self::Progress,
            Some("BACKLOG") => Self::Sprint,
            Some("DONE") => Self::Done,
            Some("DONE_SPRINT") => Self::DoneSprint,
            _ => Self::Backlog,
        }
    }

    pub fn is_done(self) -> bool {
        matches!(self, Self::Done | Self::DoneSprint)
    }
}

/// What a bound tab shows of its card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardStatus {
    pub title: String,
    pub column: String,
    pub column_kind: ColumnKind,
    pub archived: bool,
}

/// One card's status by scanning the board; the reference [`card_statuses`] is tested against.
#[cfg(test)]
pub fn card_status(board: &Board, task_id: u64) -> Option<CardStatus> {
    let task = board.tasks.iter().find(|task| task.id == task_id)?;
    let column = task
        .column_id
        .and_then(|id| board.columns.iter().find(|column| column.id == id));
    Some(task_status(task, column))
}

fn task_status(task: &BoardTask, column: Option<&BoardColumn>) -> CardStatus {
    CardStatus {
        title: task.title.trim().to_owned(),
        column: column.map(|column| column.title.trim().to_owned()).unwrap_or_default(),
        column_kind: ColumnKind::of(column.and_then(|column| column.column_type.as_deref())),
        archived: task.archived.unwrap_or(false),
    }
}

/// [`card_status`] of every card, built once per fetched board so chips do not scan the board
/// on each render. The first card wins when ids repeat, as in [`card_status`].
pub fn card_statuses(board: &Board) -> HashMap<u64, CardStatus> {
    let mut columns: HashMap<u64, &BoardColumn> = HashMap::new();
    for column in &board.columns {
        columns.entry(column.id).or_insert(column);
    }
    let mut statuses = HashMap::with_capacity(board.tasks.len());
    for task in &board.tasks {
        statuses.entry(task.id).or_insert_with(|| {
            let column = task.column_id.and_then(|id| columns.get(&id).copied());
            task_status(task, column)
        });
    }
    statuses
}

/// A card offered by a picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardChoice {
    pub board_id: u64,
    pub task_id: u64,
    pub title: String,
    pub column: String,
    pub column_kind: ColumnKind,
    /// Names of the card's labels, in board order; Ward treats them as project names.
    pub labels: Vec<String>,
}

/// Cards for a picker: work in progress first (active, in progress, sprint, backlog), then,
/// when `include_done`, finished ones. Archived cards and closed sprints are left out.
pub fn picker_cards(board: &Board, include_done: bool) -> Vec<CardChoice> {
    let mut cards: Vec<(ColumnKind, f64, f64, CardChoice)> = board
        .tasks
        .iter()
        .filter(|task| !task.archived.unwrap_or(false))
        .filter_map(|task| {
            let column = task
                .column_id
                .and_then(|id| board.columns.iter().find(|column| column.id == id))?;
            let kind = ColumnKind::of(column.column_type.as_deref());
            if kind == ColumnKind::DoneSprint || (kind.is_done() && !include_done) {
                return None;
            }
            let label_ids = task.label_ids.clone().unwrap_or_default();
            let labels = board
                .labels
                .iter()
                .filter(|label| label_ids.contains(&label.id))
                .map(|label| label.name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect();
            Some((
                kind,
                column.order.unwrap_or_default(),
                task.order.unwrap_or_default(),
                CardChoice {
                    board_id: board.id,
                    task_id: task.id,
                    title: task.title.trim().to_owned(),
                    column: column.title.trim().to_owned(),
                    column_kind: kind,
                    labels,
                },
            ))
        })
        .collect();
    cards.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
            .then(a.3.task_id.cmp(&b.3.task_id))
    });
    cards.into_iter().map(|(_, _, _, card)| card).collect()
}

/// `5/1287 In progress`: the chip of a bound tab. Without a fetched status only the ids show.
pub fn chip_text(board_id: u64, task_id: u64, status: Option<&CardStatus>) -> String {
    match status {
        Some(status) if status.archived => format!("{board_id}/{task_id} archived"),
        Some(status) if !status.column.is_empty() => {
            format!("{board_id}/{task_id} {}", status.column)
        }
        _ => format!("{board_id}/{task_id}"),
    }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
