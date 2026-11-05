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
use super::provider::{claude, codex, model_family};

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

/// Whether this machine runs the preparation. With a vault it always does: what the owner
/// wrote is a brief for the orchestrator, which checks it against the repository and the vault
/// and writes the executor's prompt from it, not a finished prompt to hand on as it is. Without
/// a vault only an assignment that cannot start an executor as it is goes through it.
pub fn runs(assignment: &Assignment, has_vault: bool) -> bool {
    has_vault || is_needed(assignment)
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
    /// The agent of the executor (`claude`, `codex`), picked when the assignment was taken: the
    /// plan does not change it, so the model of the plan has to be one of its own.
    pub provider: &'a str,
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
        "Ты агент-оркестратор на машине {}. Задача пришла из сервиса агентов: твоя работа — \
         собрать контекст в волте и подготовить задание исполнителю. Код ты не пишешь и рабочие \
         репозитории не меняешь, читать их можно.\n\n",
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
         же репозитория (группировка), выбрать репозиторий, базовую ветку и имя ветки, \
         собрать промпт по шаблону {} и выбрать модель исполнителю (раздел «Исполнитель» \
         ниже). Блок «Контекст» карточки — по {}.\n\n",
        vault.inside(vault.playbook()),
        vault.inside(vault.dossier_dir()),
        vault.inside(vault.prompt_template()),
        vault.inside(vault.context_playbook()),
    ));
    text.push_str(&format!(
        "Имя ветки: `<тип>/{number}-<тема>`. Тема — суть задачи коротко по-английски, 2-4 \
         слова в kebab-case, а не пересказ заголовка карточки. Транслитерировать русские слова \
         в имени ветки нельзя: не `feature/{number}-uvedomlenie-o-restarte`, а \
         `feature/{number}-restart-tech-channel-notify`. Тип — по соглашению самого \
         репозитория, если оно видно в `git branch -a` (`feature/`, `fix/` и т.п.); иначе \
         `feature/` для фич и `fix/` для багов.\n\n"
    ));
    text.push_str(&format!(
        "Волт на этой машине: {}. Промпт владельцу на согласование не показываешь. Развилки, \
         от которых суть работы не меняется, решаешь сам и записываешь решение в досье.\n\n",
        vault.root()
    ));
    text.push_str(
        "Если от ответа владельца зависит, что и как делать исполнителю (постановка допускает \
         разные прочтения, не хватает данных, которых нет ни в карточке, ни в волте, ни в \
         репозитории), спроси его: все вопросы одним сообщением, коротко и по пунктам, и \
         закончи ход, не записывая план. Владелец ответит в чате этой сессии; время ожидания \
         ответа в лимит подготовки не входит. После ответа продолжай с того же места.\n\n",
    );
    if !assignment.prompt.trim().is_empty() {
        text.push_str("## Постановка владельца\n\n");
        text.push_str(
            "Владелец написал задачу сам. Это постановка, а не готовый промпт: разбери её, \
             сверь с карточкой, досье и репозиторием (есть ли упомянутые ветки, пути, файлы, \
             команды) и напиши по ней полный промпт исполнителю по шаблону — с паспортом \
             запуска, путями и проверками. Всё существенное из постановки сохрани; копировать \
             её в `prompt` как есть нельзя.\n\n",
        );
        text.push_str("```text\n");
        text.push_str(assignment.prompt.trim());
        text.push_str("\n```\n\n");
    }

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
    if let Some(model) = assignment
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        given.push(format!("- модель: {model}"));
    }
    if given.is_empty() {
        text.push_str("Ничего: репозиторий, базовую ветку, ветку и модель определяешь ты.\n\n");
    } else {
        text.push_str(&given.join("\n"));
        text.push_str(
            "\n\nЗаданное не переопределять: в плане повтори его как есть. Исключение — \
             базовая ветка, которой нет в репозитории: укажи существующую и напиши об этом в \
             `summary`.\n\n",
        );
    }

    text.push_str(&executor_section(ctx.provider));

    text.push_str("## Результат\n\n");
    text.push_str(&format!(
        "Последним действием запиши файл {plan_path} — один объект JSON, без текста вокруг:\n\n"
    ));
    text.push_str(&plan_example(assignment, ctx.provider, number));
    text.push_str(
        "\n- `repo`, `branch` и `prompt` обязательны: без них подготовка считается неудачной.\n\
         - `prompt` — полный промпт исполнителю по шаблону, вместе с паспортом запуска.\n\
         - `provider` — агент исполнителя; `model` — id модели этого агента или `null`.\n\
         - `dossierPath` — путь досье внутри волта, `runId` — `run-ГГГГММДД-NNN`.\n\
         - `summary` — одна-две строки: о чём эта сессия, для экрана агентов и карточки.\n\
         - `tags` — одно-четыре коротких тега в нижнем регистре: проект (метка карточки или \
           папка проекта в волте), репозиторий, модуль. По ним владелец ищет и фильтрует \
           сессии на экране агентов.\n\
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

/// The model of the executor the example of the plan shows when the assignment fixed none.
const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5-5";

/// Who runs the executor and which model it can take. The rule of the prompt template picks
/// Claude models only, and Codex refuses every one of them (`400 model_not_found`) and does
/// nothing, so for Codex the orchestrator is told to leave the model to the agent.
fn executor_section(provider: &str) -> String {
    let mut text = String::from("## Исполнитель\n\n");
    text.push_str(&format!(
        "Исполнителя запустит агент `{provider}`: его выбрал сервис по лимитам подписок \
         машины, и план его не меняет. В `provider` плана — `{provider}`. "
    ));
    text.push_str(match provider {
        claude::ID => {
            "Модель — модель Claude (`claude-*`) по правилу шаблона или `null` (модель агента \
             по умолчанию)."
        }
        codex::ID => {
            "Правило шаблона о моделях Claude к нему не относится: модель `claude-*` Codex не \
             запустит и ничего не сделает. Если модель не задана выше, в `model` — `null`, \
             Codex возьмёт свою модель по умолчанию."
        }
        _ => "Если модель не задана выше, в `model` — `null`: агент возьмёт свою модель по умолчанию.",
    });
    text.push_str(
        " Модель другого агента не предлагать. В паспорте запуска `ai` и `model` — те же, что \
         в плане.\n\n",
    );
    text
}

/// The model the example of the plan shows: the one the assignment fixed when it is the
/// executor's, the default of the template for Claude, none for any other agent.
fn example_model(assignment: &Assignment, provider: &str) -> Option<String> {
    let given = assignment
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty() && model_family(model) == provider);
    match given {
        Some(model) => Some(model.to_owned()),
        None if provider == claude::ID => Some(DEFAULT_CLAUDE_MODEL.to_owned()),
        None => None,
    }
}

/// The shape of `plan.json` in the prompt, with the values the assignment already fixed and
/// the executor's agent.
fn plan_example(assignment: &Assignment, provider: &str, number: u64) -> String {
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
            &format!("feature/{number}-<тема-по-английски>"),
        ),
        prompt: "<полный промпт исполнителю>".to_owned(),
        dossier_path: "11-Задачи/Активные/<Проект>/<префикс>. <Задача>.md".to_owned(),
        run_id: "run-ГГГГММДД-NNN".to_owned(),
        provider: provider.to_owned(),
        model: example_model(assignment, provider),
        summary: "<одна-две строки: о чём сессия>".to_owned(),
        tags: vec!["<проект>".to_owned(), "<модуль>".to_owned()],
        grouped: vec![number],
    };
    let mut text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    text.push('\n');
    text
}

fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
