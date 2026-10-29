use super::*;

fn labels(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn remembered_directory_wins() {
    let mut map = RepoMap::default();
    assert!(map.remember("Hassle Online", "C:/work/hassle"));
    assert!(!map.remember("hassle online", "C:/work/hassle"));
    let choice = choose_repo(&labels(&["MUST", "hassle online"]), &map, |_| {
        vec!["c:/other".to_owned()]
    });
    assert_eq!(
        choice,
        Some(RepoChoice {
            label: "hassle online".to_owned(),
            path: "C:/work/hassle".to_owned()
        })
    );
}

#[test]
fn learned_directory_is_used_only_when_unambiguous() {
    let map = RepoMap::default();
    let learned = |label: &str| match label {
        "chimney" => vec!["c:/work/chimney".to_owned()],
        "hassle" => vec!["c:/work/a".to_owned(), "c:/work/b".to_owned()],
        _ => Vec::new(),
    };
    assert_eq!(
        choose_repo(&labels(&["research", "chimney"]), &map, learned),
        Some(RepoChoice {
            label: "chimney".to_owned(),
            path: "c:/work/chimney".to_owned()
        })
    );
    assert_eq!(choose_repo(&labels(&["hassle"]), &map, learned), None);
    assert_eq!(choose_repo(&[], &map, learned), None);
}

#[test]
fn session_project_has_no_whitespace() {
    assert_eq!(session_project(Some("hassle online"), "C:/x"), "hassle-online");
    assert_eq!(session_project(None, "C:/work/Brain Manager"), "brain-manager");
    assert_eq!(session_project(Some("!!"), "C:\\"), UNLABELED);
}

#[test]
fn repo_map_round_trips_through_json() {
    let mut map = RepoMap::default();
    map.remember("chimney", "C:/work/chimney");
    let decoded: RepoMap =
        serde_json::from_str(&serde_json::to_string(&map).expect("serializes")).expect("parses");
    assert_eq!(decoded, map);
}
