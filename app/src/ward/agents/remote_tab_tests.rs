use super::*;
use warp_core::ui::theme::AnsiColorIdentifier;

const TASK: &str = "#4726 Реверс. Попробовать добавить салюты";
const BRANCH: &str = "feature/4726-fireworks-sync";

fn status(text: &str, warning: bool) -> ViewerStatus {
    ViewerStatus {
        text: text.to_owned(),
        warning,
    }
}

#[test]
fn the_first_command_of_a_remote_tab_is_the_viewer() {
    let view = EntityId::new();
    let other = EntityId::new();
    register(view, RemoteTab::new("steep-pc", "#4726 Задача", ""));
    assert!(is_remote(view));
    assert!(!is_remote(other));
    assert!(!claim_block(other, "s-1"));
    assert!(claim_block(view, "s-1"));
    // A command typed after the viewer is over is the user's own.
    assert!(!claim_block(view, "s-2"));
    assert_eq!(of_view(view).unwrap().block.as_deref(), Some("s-1"));
}

#[test]
fn the_tab_is_live_while_the_viewer_block_runs() {
    let mut tab = RemoteTab::new("steep-pc", "#4726 Задача", "");
    assert_eq!(tab.phase("s-0", false), Phase::Starting);
    tab.block = Some("s-1".to_owned());
    assert_eq!(tab.phase("s-1", false), Phase::Live);
    assert_eq!(tab.phase("s-1", true), Phase::Ended);
    assert_eq!(tab.phase("s-2", false), Phase::Ended);
}

#[test]
fn the_header_names_the_machine_and_the_task() {
    let tab = RemoteTab::new("steep-pc", "#4726 Задача", BRANCH);
    assert_eq!(
        tab.header(Phase::Live),
        "Удалённое управление: steep-pc · #4726 Задача"
    );
    assert_eq!(RemoteTab::hint(Phase::Live), Some("Ctrl+] — отключиться"));
    assert_eq!(
        tab.header(Phase::Ended),
        "Отключено от steep-pc · #4726 Задача"
    );
    assert_eq!(RemoteTab::hint(Phase::Ended), None);

    let unnamed = RemoteTab::new("", "Сессия агента", "");
    assert_eq!(
        unnamed.header(Phase::Starting),
        "Удалённое управление: другая машина · Сессия агента"
    );
}

/// The name of the tab is the task alone, however long (the row clips it at its end); the
/// machine and the task in full are in the hover.
#[test]
fn the_hover_names_the_machine_and_the_task_in_full() {
    let tab = RemoteTab::new("steep-pc", TASK, BRANCH);
    assert_eq!(tab.task, TASK);
    assert_eq!(
        tab.full_title(),
        "steep-pc · #4726 Реверс. Попробовать добавить салюты"
    );
    assert_eq!(
        tab.tooltip(),
        "Удалённое управление: steep-pc · #4726 Реверс. Попробовать добавить салюты"
    );

    let long = format!("#4726 {}", "очень длинное название ".repeat(20));
    let tab = RemoteTab::new("steep-pc", long.as_str(), BRANCH);
    assert_eq!(tab.task, long);
    assert_eq!(tab.full_title(), format!("steep-pc · {long}"));
    // The line under the name does not carry the task at all.
    assert_eq!(tab.row_label(Link::Live), format!("steep-pc · {BRANCH}"));

    let unnamed = RemoteTab::new("", TASK, BRANCH);
    assert_eq!(
        unnamed.full_title(),
        "другая машина · #4726 Реверс. Попробовать добавить салюты"
    );
}

/// The line under the name: the machine and the branch; without a branch, the machine and the
/// state of the stream; without a machine, the branch or the state alone.
#[test]
fn the_row_names_the_machine_and_the_branch_or_the_state() {
    let tab = RemoteTab::new("steep-pc", TASK, BRANCH);
    for link in [
        Link::Connecting,
        Link::Live,
        Link::Reconnecting,
        Link::Ended,
    ] {
        assert_eq!(
            tab.row_label(link),
            "steep-pc · feature/4726-fireworks-sync"
        );
    }

    let no_branch = RemoteTab::new("steep-pc", TASK, "");
    assert_eq!(no_branch.row_label(Link::Live), "steep-pc · подключено");
    assert_eq!(
        no_branch.row_label(Link::Connecting),
        "steep-pc · подключение…"
    );
    assert_eq!(no_branch.row_label(Link::Waiting), "steep-pc · ждём Ward");
    assert_eq!(no_branch.row_label(Link::Legacy), "steep-pc · старый Ward");
    assert_eq!(
        no_branch.row_label(Link::Reconnecting),
        "steep-pc · переподключение…"
    );
    assert_eq!(no_branch.row_label(Link::Ended), "steep-pc · отключено");

    let no_machine = RemoteTab::new("", TASK, BRANCH);
    assert_eq!(no_machine.row_label(Link::Live), BRANCH);

    let nothing = RemoteTab::new("", TASK, "");
    assert_eq!(nothing.row_label(Link::Live), "подключено");
    assert_eq!(nothing.row_label(Link::Ended), "отключено");
}

/// The state of the stream for the row: from where the viewer is and from the title it gives
/// its console, in the words of `attach.ps1`.
#[test]
fn the_state_of_the_stream_follows_the_viewer() {
    assert_eq!(Link::of(Phase::Starting, None), Link::Connecting);
    assert_eq!(
        Link::of(Phase::Starting, Some(&status("подключено", false))),
        Link::Connecting
    );
    assert_eq!(Link::of(Phase::Live, None), Link::Connecting);
    assert_eq!(
        Link::of(Phase::Ended, Some(&status("подключено", false))),
        Link::Ended
    );
    let cases = [
        ("подключаемся к steep-pc", false, Link::Connecting),
        (
            "ждём Ward на steep-pc (сессия запускается, агент ещё не стартовал)",
            false,
            Link::Waiting,
        ),
        (
            "ждём Ward на машине сессии (сессия запускается, агент ещё не стартовал)",
            false,
            Link::Waiting,
        ),
        ("подключено", false, Link::Live),
        (
            "на steep-pc Ward старой версии: экран может задваиваться, обновите Ward там",
            true,
            Link::Legacy,
        ),
        (
            "Ward на steep-pc отключился, переподключаемся",
            true,
            Link::Reconnecting,
        ),
        ("трансляция закончилась", false, Link::Ended),
        // Words of a newer viewer this Ward does not know yet: still connecting, or trouble.
        ("что-то новое", false, Link::Connecting),
        ("что-то тревожное", true, Link::Reconnecting),
    ];
    for (text, warning, link) in cases {
        assert_eq!(
            Link::of(Phase::Live, Some(&status(text, warning))),
            link,
            "{text}"
        );
    }
}

/// The group of remote tabs loses the color an older Ward gave it, and keeps one the user
/// picked.
#[test]
fn the_group_of_remote_tabs_has_no_fill_of_its_own() {
    let auto = SelectedTabColor::Color(project_groups::auto_group_color(GROUP_NAME));
    assert_eq!(group_color(auto), SelectedTabColor::Cleared);
    assert_eq!(
        group_color(SelectedTabColor::Cleared),
        SelectedTabColor::Cleared
    );
    assert_eq!(
        group_color(SelectedTabColor::Unset),
        SelectedTabColor::Unset
    );
    let picked = [
        AnsiColorIdentifier::Blue,
        AnsiColorIdentifier::Cyan,
        AnsiColorIdentifier::Green,
        AnsiColorIdentifier::Magenta,
    ]
    .into_iter()
    .map(SelectedTabColor::Color)
    .find(|color| *color != auto)
    .unwrap();
    assert_eq!(group_color(picked), picked);
}

/// The viewer titles its console with the state of the stream; the header reads it from there.
#[test]
fn the_state_of_the_stream_is_read_off_the_title_of_the_viewer() {
    assert_eq!(
        viewer_status("Ward: подключено"),
        Some(status("подключено", false))
    );
    assert_eq!(
        viewer_status("Ward! на steep-pc Ward старой версии: экран может задваиваться"),
        Some(status(
            "на steep-pc Ward старой версии: экран может задваиваться",
            true
        ))
    );
    // The titles of the shell and of the agent are not a state of the stream.
    for title in [
        "",
        "Windows PowerShell",
        "✳ Claude Code",
        "Ward:подключено",
        "Ward:   ",
        "ward: подключено",
    ] {
        assert_eq!(viewer_status(title), None, "{title}");
    }
    let long = format!("Ward: {}\u{1b}[31m", "я".repeat(500));
    let status = viewer_status(&long).unwrap();
    assert_eq!(status.text.chars().count(), MAX_STATUS_CHARS);
    assert!(!status.text.contains('\u{1b}'));
}

#[test]
fn closed_tabs_are_forgotten() {
    let kept = EntityId::new();
    let closed = EntityId::new();
    register(kept, RemoteTab::new("steep-pc", "#1", ""));
    register(closed, RemoteTab::new("steep-pc", "#2", ""));
    retain(|view| view == kept);
    assert!(is_remote(kept));
    assert!(!is_remote(closed));
}
