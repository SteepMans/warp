use std::path::PathBuf;

use super::*;

fn vault() -> Vault {
    Vault {
        enabled: true,
        path: "C:\\work\\other\\steep-vault-brain".to_owned(),
        ..Default::default()
    }
}

fn assignment() -> Assignment {
    Assignment {
        session_id: "5f0e8a5b-0c3e-4d2f-9a64-3b1e4f6d7a20".to_owned(),
        task_ref: TaskRef {
            board_id: 5,
            task_id: 5024,
            task_number: Some(4639),
        },
        attempt: 1,
        title: "Agents. Подготовка задания".to_owned(),
        max_duration_min: 240,
        ..Default::default()
    }
}

#[test]
fn preparation_is_needed_without_a_prompt_a_repository_or_a_branch() {
    let full = Assignment {
        repo: "https://github.com/SteepMans/warp.git".to_owned(),
        branch: "feature/4639-x".to_owned(),
        prompt: "сделай".to_owned(),
        ..assignment()
    };
    assert!(!is_needed(&full));
    assert!(is_needed(&assignment()));
    assert!(is_needed(&Assignment {
        prompt: "   ".to_owned(),
        ..full.clone()
    }));
    assert!(is_needed(&Assignment {
        repo: String::new(),
        ..full.clone()
    }));
    assert!(is_needed(&Assignment {
        branch: String::new(),
        ..full
    }));
}

#[test]
fn the_time_limit_comes_from_the_assignment() {
    // An older service says nothing, and zero is not a limit anyone meant.
    assert_eq!(max_duration_min(&assignment()), DEFAULT_MAX_DURATION_MIN);
    assert_eq!(
        max_duration_min(&Assignment {
            prepare_max_duration_min: Some(0),
            ..assignment()
        }),
        DEFAULT_MAX_DURATION_MIN
    );
    // The limit is a setting of the service and travels in the assignment.
    assert_eq!(
        max_duration_min(&Assignment {
            prepare_max_duration_min: Some(45),
            ..assignment()
        }),
        45
    );
}

#[test]
fn the_orchestrator_prompt_carries_the_card_the_playbook_and_the_plan() {
    let vault = vault();
    let plan_path = PathBuf::from("C:\\Users\\steep\\agents-work\\.agents\\5f0e8a5b\\plan.json");
    let prompt = orchestrator_prompt(&PromptContext {
        assignment: &assignment(),
        worker_name: "steep-pc",
        vault: &vault,
        plan_path: &plan_path,
        max_duration_min: 30,
    });

    assert!(prompt.contains("steep-pc"), "{prompt}");
    assert!(prompt.contains("#4639"));
    assert!(prompt.contains("Agents. Подготовка задания"));
    assert!(prompt.contains("Доска: 5, карточка 5024"));
    assert!(prompt.contains("kanban:5/5024"));
    assert!(prompt.contains("https://chimney.steep-man.ru/chimney/#/boards/5?task=5024"));
    // The playbook, the prompt template, the context playbook and the dossier directory.
    assert!(
        prompt.contains("C:/work/other/steep-vault-brain/_meta/playbooks/brain-manager.md"),
        "{prompt}"
    );
    assert!(prompt.contains("C:/work/other/steep-vault-brain/_templates/task-prompt.md"));
    assert!(prompt.contains("C:/work/other/steep-vault-brain/_meta/playbooks/task-context.md"));
    assert!(prompt.contains("C:/work/other/steep-vault-brain/11-Задачи/Активные/<Проект>/"));
    // The plan: its path with forward slashes and every field of the contract.
    assert!(
        prompt.contains("C:/Users/steep/agents-work/.agents/5f0e8a5b/plan.json"),
        "{prompt}"
    );
    for field in [
        "\"repo\"",
        "\"baseBranch\"",
        "\"branch\"",
        "\"prompt\"",
        "\"dossierPath\"",
        "\"runId\"",
        "\"provider\"",
        "\"model\"",
        "\"summary\"",
        "\"grouped\"",
    ] {
        assert!(prompt.contains(field), "{field} is missing from {prompt}");
    }
    assert!(prompt.contains("feature/4639-<короткий-слаг>"));
    // Nothing is given, so the orchestrator decides everything.
    assert!(prompt.contains("определяешь ты"));
    // The boundaries.
    assert!(prompt.contains("Не писать и не менять код"));
    assert!(prompt.contains("Ничего не пушить"));
    assert!(prompt.contains("prepare_failed"));
    assert!(prompt.contains("30 минут"));
    assert!(prompt.contains("Секреты"));
}

#[test]
fn what_the_assignment_fixed_is_in_the_prompt_as_given() {
    let vault = vault();
    let assignment = Assignment {
        repo: "https://github.com/SteepMans/warp.git".to_owned(),
        base_branch: "master".to_owned(),
        prompt: "Готовый промпт от менеджера".to_owned(),
        provider: "auto".to_owned(),
        model: Some("claude-opus-5-5".to_owned()),
        ..assignment()
    };
    let prompt = orchestrator_prompt(&PromptContext {
        assignment: &assignment,
        worker_name: "steep-pc",
        vault: &vault,
        plan_path: &PathBuf::from("/tmp/plan.json"),
        max_duration_min: 15,
    });
    assert!(prompt.contains("- репозиторий: https://github.com/SteepMans/warp.git"));
    assert!(prompt.contains("- базовая ветка: master"));
    assert!(prompt.contains("- модель: claude-opus-5-5"));
    assert!(prompt.contains("промпт исполнителя уже есть"));
    assert!(prompt.contains("Заданное не переопределять"));
    assert!(!prompt.contains("определяешь ты"));
    // The example of the plan repeats what is given, and `auto` is not a provider for it.
    assert!(prompt.contains("\"repo\": \"https://github.com/SteepMans/warp.git\""));
    assert!(prompt.contains("\"baseBranch\": \"master\""));
    assert!(prompt.contains("\"prompt\": \"Готовый промпт от менеджера\""));
    assert!(!prompt.contains("\"provider\": \"auto\""), "{prompt}");
    assert!(prompt.contains("\"provider\": \"claude\""));
    assert!(prompt.contains("15 минут"));
}

#[test]
fn the_prompt_follows_the_vault_paths_of_the_profile() {
    let vault = Vault {
        enabled: true,
        path: "D:/vault".to_owned(),
        playbook: "playbooks/manager.md".to_owned(),
        prompt_template: "templates/prompt.md".to_owned(),
        context_playbook: "playbooks/context.md".to_owned(),
        dossier_dir: "tasks/active".to_owned(),
        ..Default::default()
    };
    let prompt = orchestrator_prompt(&PromptContext {
        assignment: &assignment(),
        worker_name: "laptop",
        vault: &vault,
        plan_path: &PathBuf::from("D:/work/.agents/s/plan.json"),
        max_duration_min: 30,
    });
    assert!(prompt.contains("D:/vault/playbooks/manager.md"));
    assert!(prompt.contains("D:/vault/templates/prompt.md"));
    assert!(prompt.contains("D:/vault/playbooks/context.md"));
    assert!(prompt.contains("D:/vault/tasks/active/<Проект>/"));
    assert!(
        prompt.contains("Волт на этой машине: D:/vault."),
        "{prompt}"
    );
}

#[test]
fn the_orchestrator_session_has_its_own_name() {
    let task_ref = TaskRef {
        board_id: 5,
        task_id: 5024,
        task_number: Some(4639),
    };
    assert_eq!(
        session_name(&task_ref, "Подготовка задания"),
        "4639 Подготовка задания (подготовка)"
    );
}
