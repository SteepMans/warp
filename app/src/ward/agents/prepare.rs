//! The preparation phase: the orchestrator session that collects the dossier and writes the
//! plan, and the prompt Ward gives it.
//!
//! An assignment does not have to come with a prompt. Without one (or without a repository or a
//! branch) Ward first runs an orchestrator in the vault: an ordinary agent with the vault and
//! the Chimney MCP, which works by the brain-manager playbook, and whose result is
//! `plan.json` in the session's preparation directory. Ward writes the prompt for it itself
//! from the card, the vault paths of the profile and the fields of the plan, so the
//! orchestrator needs nothing but the vault to start.
//!
//! The orchestrator writes no code and touches no working repository: its work is the vault,
//! the card and the prompt. The plan is its last action, after which it ends its own session;
//! Ward waits for that exit (and ends the process itself only if it does not come) and starts
//! the executor after it, so the two sessions run one after the other.

use std::path::Path;

use super::contract::{Assignment, TaskRef};
use super::plan::Plan;
use super::profile::Vault;

/// `prepareMaxDurationMin` when the assignment does not say: a setting of the service
/// (`AGENTS_PREPARE_MAX_MIN`), so Ward only falls back for an older service.
pub const DEFAULT_MAX_DURATION_MIN: u64 = 30;

/// Whether an assignment needs the preparation phase: Ward cannot start an executor without a
/// prompt, a repository or a branch.
pub fn is_needed(assignment: &Assignment) -> bool {
    assignment.prompt.trim().is_empty()
        || assignment.repo.trim().is_empty()
        || assignment.branch.trim().is_empty()
}

/// How long the preparation may take. The limit belongs to the service and comes with the
/// assignment; Ward invents none of its own. The count runs while there is no plan: waiting for
/// the owner's approval afterwards does not end the session.
pub fn max_duration_min(assignment: &Assignment) -> u64 {
    assignment
        .prepare_max_duration_min
        .filter(|minutes| *minutes > 0)
        .unwrap_or(DEFAULT_MAX_DURATION_MIN)
}

/// The Remote Control name of the orchestrator session, next to the executor's own name.
pub fn session_name(task_ref: &TaskRef, title: &str) -> String {
    format!(
        "{} (подготовка)",
        super::launch::session_name(task_ref, title)
    )
}

/// The card as the playbook wants it written: a link for the owner and a marker for the
/// integration.
pub fn card_link(task_ref: &TaskRef) -> String {
    let board = task_ref.board_id;
    let task = task_ref.task_id;
    format!(
        "[Дымоход #{task}](https://chimney.steep-man.ru/chimney/#/boards/{board}?task={task}) \
         -> kanban:{board}/{task}"
    )
}

/// What the orchestrator is told about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptContext<'a> {
    pub assignment: &'a Assignment,
    /// The worker's name, as the journal of the dossier wants the device.
    pub worker_name: &'a str,
    pub vault: &'a Vault,
    /// Where `plan.json` goes, absolute, with forward slashes.
    pub plan_path: &'a Path,
    pub max_duration_min: u64,
}

/// The prompt of the orchestrator session: the card, the order of the playbook, what the
/// assignment already decided, the shape of `plan.json` and the boundaries.
pub fn orchestrator_prompt(ctx: &PromptContext<'_>) -> String {
    let assignment = ctx.assignment;
    let vault = ctx.vault;
    let number = assignment
        .task_ref
        .task_number
        .unwrap_or(assignment.task_ref.task_id);
    let title = assignment.title.trim();
    let plan_path = forward_slashes(ctx.plan_path);
    let mut text = String::new();

    text.push_str("# Подготовка задания для исполнителя\n\n");
    text.push_str(&format!(
        "Ты агент-оркестратор на машине {}. Задача пришла из сервиса агентов без готового \
         задания: твоя работа — собрать контекст в волте и подготовить задание исполнителю. \
         Код ты не пишешь и рабочие репозитории не трогаешь.\n\n",
        ctx.worker_name
    ));

    text.push_str("## Карточка\n\n");
    text.push_str(&format!("- Номер: #{number}\n"));
    if !title.is_empty() {
        text.push_str(&format!("- Название: {title}\n"));
    }
    text.push_str(&format!(
        "- Доска: {}, карточка {}\n",
        assignment.task_ref.board_id, assignment.task_ref.task_id
    ));
    text.push_str(&format!("- Ссылка: {}\n", card_link(&assignment.task_ref)));
    text.push_str(&format!("- Попытка: {}\n\n", assignment.attempt.max(1)));

    text.push_str("## Порядок\n\n");
    text.push_str(&format!(
        "Работай по плейбуку {}: прочитай его целиком и следуй ему. Коротко: прочитать \
         карточку и связанные заметки, решить, нужно ли досье, завести или обновить его в \
         {}/<Проект>/, проверить, не стоит ли рядом ещё несколько таких же мелких задач того \
         же репозитория (группировка), выбрать репозиторий, базовую ветку и имя ветки по \
         соглашению самого репозитория (`git branch -a`), собрать промпт по шаблону {} и \
         выбрать модель по правилу шаблона. Блок «Контекст» карточки — по {}.\n\n",
        vault.inside(vault.playbook()),
        vault.inside(vault.dossier_dir()),
        vault.inside(vault.prompt_template()),
        vault.inside(vault.context_playbook()),
    ));
    text.push_str(&format!(
        "Волт на этой машине: {}. Личные проекты идут автономно: промпт владельцу на \
         согласование не показываешь, развилки решаешь сам и записываешь решение в досье.\n\n",
        vault.root()
    ));

    text.push_str("## Что уже задано\n\n");
    let mut given = Vec::new();
    if !assignment.repo.trim().is_empty() {
        given.push(format!("- репозиторий: {}", assignment.repo.trim()));
    }
    if !assignment.base_branch.trim().is_empty() {
        given.push(format!(
            "- базовая ветка: {}",
            assignment.base_branch.trim()
        ));
    }
    if !assignment.branch.trim().is_empty() {
        given.push(format!("- ветка: {}", assignment.branch.trim()));
    }
    if !assignment.prompt.trim().is_empty() {
        given.push(
            "- промпт исполнителя уже есть: его не переписывай, определи только репозиторий и \
             ветку, досье не выдумывай"
                .to_owned(),
        );
    }
    if let Some(model) = assignment
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        given.push(format!("- модель: {model}"));
    }
    if given.is_empty() {
        text.push_str(
            "Ничего: репозиторий, базовую ветку, ветку, промпт и модель определяешь ты.\n\n",
        );
    } else {
        text.push_str(&given.join("\n"));
        text.push_str(
            "\n\nЗаданное не переопределять: в плане повтори его как есть или \
                       оставь поле пустым.\n\n",
        );
    }

    text.push_str("## Результат\n\n");
    text.push_str(&format!(
        "Последним действием запиши файл {plan_path} — один объект JSON, без текста вокруг:\n\n"
    ));
    text.push_str(&plan_example(assignment, number));
    text.push_str(
        "\n- `repo`, `branch` и `prompt` обязательны: без них подготовка считается неудачной.\n\
         - `prompt` — полный промпт исполнителю по шаблону, вместе с паспортом запуска.\n\
         - `provider` — `claude` или `codex`; `model` — id модели или `null`.\n\
         - `dossierPath` — путь досье внутри волта, `runId` — `run-ГГГГММДД-NNN`.\n\
         - `summary` — две-три строки для карточки.\n\
         - `grouped` — номера карточек, которые ты решил вести одной сессией.\n\
         - Пути в JSON — с прямыми слэшами.\n\
         - Файл — последнее действие: записал, коротко ответил — и заверши сессию сам. Ward \
           дождётся твоего выхода и только потом запустит исполнителя.\n\n",
    );

    text.push_str("## Границы\n\n");
    text.push_str(&format!(
        "- Не писать и не менять код; клоны, worktree и ветки исполнителя не трогать.\n\
         - Ничего не пушить и не открывать pull request.\n\
         - Волт и карточка — можно: досье, журнал, блок «Контекст», комментарий о запуске.\n\
         - Секреты в план, волт и карточку не писать.\n\
         - Эмодзи не использовать.\n\
         - Уложиться в {} минут: иначе Ward отметит сессию как неудачную подготовку \
           (`prepare_failed`), а карточка останется в работе.\n",
        ctx.max_duration_min
    ));
    text
}

/// The shape of `plan.json` in the prompt, with the values the assignment already fixed.
fn plan_example(assignment: &Assignment, number: u64) -> String {
    let field = |value: &str, placeholder: &str| {
        let value = value.trim();
        if value.is_empty() {
            placeholder.to_owned()
        } else {
            value.to_owned()
        }
    };
    let plan = Plan {
        repo: field(&assignment.repo, "https://github.com/<owner>/<repo>.git"),
        base_branch: field(&assignment.base_branch, "<база, например main>"),
        branch: field(
            &assignment.branch,
            &format!("feature/{number}-<короткий-слаг>"),
        ),
        prompt: field(&assignment.prompt, "<полный промпт исполнителю>"),
        dossier_path: "11-Задачи/Активные/<Проект>/<префикс>. <Задача>.md".to_owned(),
        run_id: "run-ГГГГММДД-NNN".to_owned(),
        provider: field(&assignment.provider, "claude"),
        model: Some(
            assignment
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .unwrap_or("claude-sonnet-5-5")
                .to_owned(),
        ),
        summary: "<две-три строки для карточки>".to_owned(),
        grouped: vec![number],
    };
    let mut text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    // `auto` is the service's word for "choose yourself", not a provider the plan may name.
    text = text.replace("\"provider\": \"auto\"", "\"provider\": \"claude\"");
    text.push('\n');
    text
}

fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
