use super::*;

#[test]
fn splits_remote_control_session_names() {
    assert_eq!(
        split_session_name("hassle. payments-refund-webhook"),
        (
            Some("hassle".to_owned()),
            Some("payments-refund-webhook".to_owned())
        )
    );
    assert_eq!(
        split_session_name("just a task"),
        (None, Some("just a task".to_owned()))
    );
    assert_eq!(split_session_name("karandash."), (Some("karandash".to_owned()), None));
}

#[test]
fn directory_name_takes_the_last_component() {
    assert_eq!(
        directory_name("C:\\work\\other\\warp\\"),
        Some("warp".to_owned())
    );
    assert_eq!(directory_name("/home/me/hassle"), Some("hassle".to_owned()));
    assert_eq!(directory_name("C:\\"), None);
    assert_eq!(directory_name(""), None);
}

#[test]
fn labels_prefer_the_session_name() {
    let labels = agent_labels(
        Some("claude --remote-control 'hassle. payments-refund-webhook'"),
        Some("C:/work/hassle-backend"),
        Some("fix the webhook"),
        "Claude Code",
    );
    assert_eq!(labels.project.as_deref(), Some("hassle"));
    assert_eq!(labels.task, "payments-refund-webhook");
}

#[test]
fn labels_fall_back_to_directory_prompt_and_agent() {
    let labels = agent_labels(
        Some("claude"),
        Some("C:/work/karandash"),
        Some("  fix\n the   budget "),
        "Claude Code",
    );
    assert_eq!(labels.project.as_deref(), Some("karandash"));
    assert_eq!(labels.task, "fix the budget");

    let labels = agent_labels(None, None, None, "Codex");
    assert_eq!(labels.project, None);
    assert_eq!(labels.task, "Codex");
}

#[test]
fn clip_chars_adds_an_ellipsis() {
    assert_eq!(clip_chars("abcdef", 4), "abc\u{2026}");
    assert_eq!(clip_chars("abc", 4), "abc");
}
