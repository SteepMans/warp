use super::*;

fn observation(keys: &[&str], group: Option<(TabGroupId, Option<&str>)>) -> TabObservation {
    TabObservation {
        keys: keys.iter().map(|key| key.to_string()).collect(),
        group_id: group.map(|(id, _)| id),
        group_name: group.and_then(|(_, name)| name.map(str::to_owned)),
    }
}

fn map_with(entries: &[(&str, &str)]) -> ProjectGroupMap {
    let mut map = ProjectGroupMap::default();
    for (key, group) in entries {
        map.learn(&[key.to_string()], group);
    }
    map
}

#[test]
fn path_key_normalizes_separators_case_and_trailing_slash() {
    assert_eq!(
        path_key(r"C:\Work\Hassle\"),
        Some("c:/work/hassle".to_owned())
    );
    assert_eq!(path_key("/home/me/repo/"), Some("/home/me/repo".to_owned()));
    assert_eq!(path_key("  "), None);
}

#[test]
fn project_keys_put_remote_control_name_first() {
    assert_eq!(
        project_keys(Some("Hassle"), Some(r"C:\work\hassle")),
        vec!["rc:hassle".to_owned(), "c:/work/hassle".to_owned()]
    );
    assert_eq!(
        project_keys(None, Some(r"C:\work\hassle")),
        vec!["c:/work/hassle".to_owned()]
    );
    assert!(project_keys(Some(" "), None).is_empty());
}

#[test]
fn remote_control_project_reads_quoted_session_name() {
    assert_eq!(
        remote_control_project("claude --remote-control 'hassle. payments-refund-webhook'"),
        Some("hassle".to_owned())
    );
    assert_eq!(
        remote_control_project(r#"claude --model opus --remote-control "karandash. budget""#),
        Some("karandash".to_owned())
    );
    assert_eq!(
        remote_control_project("claude --remote-control=chimney.grafana --resume abc"),
        Some("chimney".to_owned())
    );
    assert_eq!(
        remote_control_project("claude --remote-control-session-name-prefix 'brain-manager.'"),
        Some("brain-manager".to_owned())
    );
    // PowerShell escapes a single quote inside single quotes by doubling it.
    assert_eq!(
        remote_control_project("claude --remote-control 'o''neil. task'"),
        Some("o'neil".to_owned())
    );
}

#[test]
fn remote_control_session_name_is_unquoted() {
    assert_eq!(
        remote_control_session_name("claude --remote-control 'hassle. payments-refund-webhook'"),
        Some("hassle. payments-refund-webhook".to_owned())
    );
    assert_eq!(remote_control_session_name("claude --remote-control"), None);
}

#[test]
fn remote_control_project_needs_a_project_prefix() {
    assert_eq!(
        remote_control_project("claude --remote-control 'just a task'"),
        None
    );
    assert_eq!(
        remote_control_project("claude --remote-control --resume abc"),
        None
    );
    assert_eq!(remote_control_project("claude --resume abc"), None);
    assert_eq!(
        remote_control_project("claude --remote-control 'two words. task'"),
        None
    );
}

#[test]
fn lookup_uses_first_mapped_key() {
    let map = map_with(&[("c:/work/hassle", "hassle"), ("rc:other", "other")]);
    assert_eq!(
        map.lookup(&["rc:missing".to_owned(), "c:/work/hassle".to_owned()]),
        Some("hassle")
    );
    assert_eq!(
        map.lookup(&["rc:other".to_owned(), "c:/work/hassle".to_owned()]),
        Some("other")
    );
    assert_eq!(map.lookup(&["c:/nowhere".to_owned()]), None);
}

#[test]
fn rename_group_updates_all_keys() {
    let mut map = map_with(&[("a", "Old"), ("b", "old"), ("c", "Other")]);
    assert!(map.rename_group("old", "New"));
    assert_eq!(map.lookup(&["a".to_owned()]), Some("New"));
    assert_eq!(map.lookup(&["b".to_owned()]), Some("New"));
    assert_eq!(map.lookup(&["c".to_owned()]), Some("Other"));
    assert!(!map.rename_group("old", "New"));
}

#[test]
fn new_tab_with_mapped_project_moves_to_group() {
    let map = map_with(&[("c:/work/hassle", "hassle")]);
    let before = observation(&[], None);
    let after = observation(&["c:/work/hassle"], None);
    assert_eq!(
        decide(Some(&before), &after, &map),
        GroupDecision::MoveTo("hassle".to_owned())
    );
    // Restored or brand-new tabs are seen with their keys right away, too.
    assert_eq!(
        decide(None, &after, &map),
        GroupDecision::MoveTo("hassle".to_owned())
    );
}

#[test]
fn tab_already_in_mapped_group_only_fills_in_keys() {
    let group = TabGroupId::new();
    let map = map_with(&[("c:/work/hassle", "Hassle")]);
    let after = observation(
        &["rc:hassle", "c:/work/hassle"],
        Some((group, Some("hassle"))),
    );
    assert_eq!(
        decide(None, &after, &map),
        GroupDecision::Learn("hassle".to_owned())
    );
}

#[test]
fn unmapped_project_in_named_group_is_learned_and_ungrouped_is_ignored() {
    let group = TabGroupId::new();
    let map = ProjectGroupMap::default();
    assert_eq!(
        decide(
            None,
            &observation(&["c:/work/karandash"], Some((group, Some("karandash")))),
            &map
        ),
        GroupDecision::Learn("karandash".to_owned())
    );
    assert_eq!(
        decide(None, &observation(&["c:/work/karandash"], None), &map),
        GroupDecision::Nothing
    );
}

#[test]
fn moving_a_tab_into_another_group_relearns() {
    let (old_group, new_group) = (TabGroupId::new(), TabGroupId::new());
    let map = map_with(&[("c:/work/chimney", "infra")]);
    let before = observation(&["c:/work/chimney"], Some((old_group, Some("infra"))));
    let after = observation(&["c:/work/chimney"], Some((new_group, Some("chimney"))));
    assert_eq!(
        decide(Some(&before), &after, &map),
        GroupDecision::Learn("chimney".to_owned())
    );
}

#[test]
fn naming_a_new_group_learns_and_renaming_follows() {
    let group = TabGroupId::new();
    let map = ProjectGroupMap::default();
    let unnamed = observation(&["c:/work/hassle"], Some((group, None)));
    let named = observation(&["c:/work/hassle"], Some((group, Some("hassle"))));
    let renamed = observation(&["c:/work/hassle"], Some((group, Some("Hassle core"))));
    assert_eq!(
        decide(Some(&unnamed), &named, &map),
        GroupDecision::Learn("hassle".to_owned())
    );
    assert_eq!(
        decide(Some(&named), &renamed, &map),
        GroupDecision::Renamed {
            from: "hassle".to_owned(),
            to: "Hassle core".to_owned()
        }
    );
}

#[test]
fn leaving_a_group_or_no_change_does_nothing() {
    let group = TabGroupId::new();
    let map = map_with(&[("c:/work/hassle", "hassle")]);
    let grouped = observation(&["c:/work/hassle"], Some((group, Some("hassle"))));
    let ungrouped = observation(&["c:/work/hassle"], None);
    assert_eq!(
        decide(Some(&grouped), &ungrouped, &map),
        GroupDecision::Nothing
    );
    assert_eq!(
        decide(Some(&grouped), &grouped, &map),
        GroupDecision::Nothing
    );
    assert_eq!(
        decide(None, &observation(&[], None), &map),
        GroupDecision::Nothing
    );
}

#[test]
fn cwd_moving_into_another_mapped_repo_moves_the_tab() {
    let group = TabGroupId::new();
    let map = map_with(&[("c:/work/hassle", "hassle"), ("c:/work/chimney", "chimney")]);
    let before = observation(&["c:/work/hassle"], Some((group, Some("hassle"))));
    let after = observation(&["c:/work/chimney"], Some((group, Some("hassle"))));
    assert_eq!(
        decide(Some(&before), &after, &map),
        GroupDecision::MoveTo("chimney".to_owned())
    );
}

#[test]
fn auto_group_color_is_stable_and_never_an_attention_color() {
    assert_eq!(auto_group_color("hassle"), auto_group_color(" Hassle "));
    for name in ["hassle", "karandash", "chimney", "brain-manager", "warp"] {
        assert!(!matches!(
            auto_group_color(name),
            AnsiColorIdentifier::Red | AnsiColorIdentifier::Yellow
        ));
    }
}

#[test]
fn map_round_trips_through_json() {
    let map = map_with(&[("c:/work/hassle", "hassle"), ("rc:chimney", "chimney")]);
    let json = serde_json::to_string(&map).unwrap();
    assert_eq!(serde_json::from_str::<ProjectGroupMap>(&json).unwrap(), map);
}

#[test]
fn paths_for_project_follow_groups_and_remote_control_names() {
    let mut map = ProjectGroupMap::default();
    map.learn(
        &["rc:hassle".to_owned(), "c:/work/hassle-backend".to_owned()],
        "Hassle",
    );
    map.learn(&["c:/work/hassle-admin".to_owned()], "hassle");
    map.learn(&["c:/work/karandash".to_owned()], "Karandash");
    assert_eq!(
        map.paths_for_project("HASSLE"),
        vec!["c:/work/hassle-admin".to_owned(), "c:/work/hassle-backend".to_owned()]
    );
    assert_eq!(
        map.paths_for_project("karandash"),
        vec!["c:/work/karandash".to_owned()]
    );
    assert!(map.paths_for_project("chimney").is_empty());
    assert!(map.paths_for_project(" ").is_empty());
}

fn settings_leaf() -> crate::app_state::PaneNodeSnapshot {
    use crate::app_state::{LeafContents, LeafSnapshot, PaneNodeSnapshot, SettingsPaneSnapshot};
    PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: true,
        custom_vertical_tabs_title: None,
        contents: LeafContents::Settings(SettingsPaneSnapshot::Local {
            current_page: Default::default(),
            search_query: None,
        }),
    })
}

#[test]
fn only_tabs_with_a_terminal_join_a_tab_group() {
    use crate::app_state::{BranchSnapshot, PaneFlex, SplitDirection};
    use crate::pane_group::PanesLayout;

    assert!(layout_joins_tab_group(&PanesLayout::default()));
    assert!(!layout_joins_tab_group(&PanesLayout::Snapshot(Box::new(
        settings_leaf()
    ))));
    let split = crate::app_state::PaneNodeSnapshot::Branch(BranchSnapshot {
        direction: SplitDirection::Vertical,
        children: vec![
            (PaneFlex(1.), settings_leaf()),
            (PaneFlex(1.), settings_leaf()),
        ],
    });
    assert!(!layout_joins_tab_group(&PanesLayout::Snapshot(Box::new(
        split
    ))));
    assert!(!layout_joins_tab_group(&PanesLayout::AmbientAgent));
}
