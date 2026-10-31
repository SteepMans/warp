use super::*;

fn blank() -> Assignment {
    Assignment {
        session_id: "s1".to_owned(),
        title: "Подготовка задания".to_owned(),
        ..Default::default()
    }
}

#[test]
fn a_full_plan_fills_an_empty_assignment() {
    let plan = parse(
        r#"{
            "repo": "https://github.com/SteepMans/warp.git",
            "baseBranch": "master",
            "branch": "feature/4639-agents-prepare",
            "prompt": "Собрать клиентскую половину подготовки",
            "dossierPath": "11-Задачи/Активные/Steep-Brain/sb. Подготовка-задания.md",
            "runId": "run-20261031-001",
            "provider": "claude",
            "model": "claude-opus-5-5",
            "summary": "Ward поднимает оркестратора",
            "grouped": [4639, "4627", null],
            "unknown": {"a": 1}
        }"#,
    )
    .unwrap();
    assert_eq!(plan.grouped, vec![4639, 4627]);
    assert_eq!(plan.run_id, "run-20261031-001");

    let mut assignment = blank();
    apply(&plan, &mut assignment).unwrap();
    assert_eq!(assignment.repo, "https://github.com/SteepMans/warp.git");
    assert_eq!(assignment.base_branch, "master");
    assert_eq!(assignment.branch, "feature/4639-agents-prepare");
    assert_eq!(assignment.prompt, "Собрать клиентскую половину подготовки");
    assert_eq!(assignment.model.as_deref(), Some("claude-opus-5-5"));
    assert!(summary(&plan).contains("Ward поднимает оркестратора"));
    assert!(summary(&plan).contains("run-20261031-001"));
}

#[test]
fn the_assignment_wins_over_the_plan() {
    let plan = parse(
        r#"{"repo":"https://github.com/SteepMans/other.git","baseBranch":"main",
            "branch":"feature/plan","prompt":"промпт плана","provider":"codex",
            "model":"claude-sonnet-5-5"}"#,
    )
    .unwrap();
    let mut assignment = Assignment {
        repo: "https://github.com/SteepMans/warp.git".to_owned(),
        branch: "feature/4639-given".to_owned(),
        prompt: "промпт назначения".to_owned(),
        provider: "claude".to_owned(),
        model: Some("claude-opus-5-5".to_owned()),
        ..blank()
    };
    apply(&plan, &mut assignment).unwrap();
    assert_eq!(assignment.repo, "https://github.com/SteepMans/warp.git");
    assert_eq!(assignment.branch, "feature/4639-given");
    assert_eq!(assignment.prompt, "промпт назначения");
    assert_eq!(assignment.model.as_deref(), Some("claude-opus-5-5"));
    // The base branch was not in the assignment, so it comes from the plan.
    assert_eq!(assignment.base_branch, "main");
    // The agent of the plan is not Ward's business: the service picks it.
    assert_eq!(assignment.provider, "claude");
}

#[test]
fn the_agent_of_the_plan_does_not_change_the_assignment() {
    let plan = parse(r#"{"provider":"codex","model":"claude-opus-5-5"}"#).unwrap();
    let mut assignment = Assignment {
        provider: "auto".to_owned(),
        ..blank()
    };
    apply(&plan, &mut assignment).unwrap_err();
    // The service chose `auto` and picks the agent itself; the model of the plan is taken.
    assert_eq!(assignment.provider, "auto");
    assert_eq!(assignment.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(plan.provider, "codex");
}

#[test]
fn an_incomplete_plan_says_what_is_missing() {
    let plan = parse(r#"{"repo":"https://github.com/SteepMans/warp.git","prompt":null}"#).unwrap();
    let mut assignment = blank();
    let err = apply(&plan, &mut assignment).unwrap_err();
    assert_eq!(err, "в плане нет: ветка, промпт");

    let plan = parse("{}").unwrap();
    let mut assignment = blank();
    assert_eq!(
        apply(&plan, &mut assignment).unwrap_err(),
        "в плане нет: репозиторий, ветка, промпт"
    );

    // The prompt came with the assignment: only the repository and the branch are wanted.
    let mut assignment = Assignment {
        prompt: "промпт назначения".to_owned(),
        ..blank()
    };
    assert_eq!(
        apply(&plan, &mut assignment).unwrap_err(),
        "в плане нет: репозиторий, ветка"
    );
}

#[test]
fn a_broken_plan_is_an_error_not_a_panic() {
    assert_eq!(parse("   ").unwrap_err(), "plan.json пустой");
    assert!(
        parse("{\"repo\": ")
            .unwrap_err()
            .starts_with("plan.json не разбирается как JSON"),
        "a truncated object is reported"
    );
    assert_eq!(
        parse("[1,2]").unwrap_err(),
        "plan.json должен быть объектом JSON"
    );
    assert!(
        parse(r#"{"repo": 42}"#)
            .unwrap_err()
            .starts_with("plan.json не по контракту")
    );
}

#[test]
fn a_fenced_plan_is_still_read() {
    let plan = parse("```json\n{\"branch\":\"feature/x\"}\n```").unwrap();
    assert_eq!(plan.branch, "feature/x");
    let plan = parse("Готово, вот план:\n{\"branch\":\"feature/y\"}\nкак просили").unwrap();
    assert_eq!(plan.branch, "feature/y");
}

#[test]
fn the_plan_goes_out_in_contract_field_names() {
    let plan = Plan {
        repo: "https://github.com/SteepMans/warp.git".to_owned(),
        base_branch: "master".to_owned(),
        branch: "feature/4639-agents-prepare".to_owned(),
        prompt: "промпт".to_owned(),
        dossier_path: "11-Задачи/Активные/x.md".to_owned(),
        run_id: "run-20261031-001".to_owned(),
        provider: "claude".to_owned(),
        model: None,
        summary: "итог".to_owned(),
        grouped: vec![4639],
    };
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        serde_json::json!({
            "repo": "https://github.com/SteepMans/warp.git",
            "baseBranch": "master",
            "branch": "feature/4639-agents-prepare",
            "prompt": "промпт",
            "dossierPath": "11-Задачи/Активные/x.md",
            "runId": "run-20261031-001",
            "provider": "claude",
            "model": null,
            "summary": "итог",
            "grouped": [4639]
        })
    );
}
