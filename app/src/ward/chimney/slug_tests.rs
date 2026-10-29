use super::*;

#[test]
fn slugifies_latin_titles() {
    assert_eq!(slugify("Payments: refund webhook!", 30), "payments-refund-webhook");
    assert_eq!(slugify("  API  v2 / auth ", 30), "api-v2-auth");
}

#[test]
fn transliterates_cyrillic() {
    assert_eq!(
        slugify("Чеклист followups встреч", 30),
        "cheklist-followups-vstrech"
    );
    assert_eq!(slugify("Ёлка и щи", 30), "elka-i-shchi");
}

#[test]
fn cuts_at_word_boundaries() {
    let slug = slugify("Grafana folder migration for the monitoring stack", 30);
    assert!(slug.len() <= 30);
    assert_eq!(slug, "grafana-folder-migration-for");
    assert_eq!(slugify("abcdefghijklmnopqrstuvwxyz0123456789", 30).len(), 30);
}

#[test]
fn falls_back_to_the_card_id() {
    assert_eq!(task_slug("任务 ++", 1287), "card-1287");
    assert_eq!(task_slug("Backlog stats", 1), "backlog-stats");
}

#[test]
fn builds_the_claude_command() {
    assert_eq!(
        claude_command("hassle-online", "refund-webhook"),
        "claude --remote-control 'hassle-online. refund-webhook'"
    );
}
