use super::*;

#[test]
fn the_first_command_of_a_remote_tab_is_the_viewer() {
    let view = EntityId::new();
    let other = EntityId::new();
    register(view, RemoteTab::new("steep-pc", "#4726 Задача"));
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
    let mut tab = RemoteTab::new("steep-pc", "#4726 Задача");
    assert_eq!(tab.phase("s-0", false), Phase::Starting);
    tab.block = Some("s-1".to_owned());
    assert_eq!(tab.phase("s-1", false), Phase::Live);
    assert_eq!(tab.phase("s-1", true), Phase::Ended);
    assert_eq!(tab.phase("s-2", false), Phase::Ended);
}

#[test]
fn the_header_names_the_machine_and_the_task() {
    let tab = RemoteTab::new("steep-pc", "#4726 Задача");
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
    assert_eq!(
        tab.row_label(Phase::Starting),
        "Удалённое управление · steep-pc"
    );
    assert_eq!(tab.row_label(Phase::Ended), "Отключено · steep-pc");

    let unnamed = RemoteTab::new("", "Сессия агента");
    assert_eq!(
        unnamed.header(Phase::Starting),
        "Удалённое управление: другая машина · Сессия агента"
    );
}

/// The viewer titles its console with the state of the stream; the header reads it from there.
#[test]
fn the_state_of_the_stream_is_read_off_the_title_of_the_viewer() {
    assert_eq!(
        viewer_status("Ward: подключено"),
        Some(ViewerStatus {
            text: "подключено".to_owned(),
            warning: false,
        })
    );
    assert_eq!(
        viewer_status("Ward! на steep-pc Ward старой версии: экран может задваиваться"),
        Some(ViewerStatus {
            text: "на steep-pc Ward старой версии: экран может задваиваться".to_owned(),
            warning: true,
        })
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
    register(kept, RemoteTab::new("steep-pc", "#1"));
    register(closed, RemoteTab::new("steep-pc", "#2"));
    retain(|view| view == kept);
    assert!(is_remote(kept));
    assert!(!is_remote(closed));
}
