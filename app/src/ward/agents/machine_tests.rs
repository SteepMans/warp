use super::*;

#[test]
fn scans_owner_repo_clones() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("SteepMans").join("warp").join(".git")).unwrap();
    std::fs::create_dir_all(root.join("SteepMans").join("chimney").join(".git")).unwrap();
    std::fs::create_dir_all(root.join("SteepMans").join("not-a-repo")).unwrap();
    std::fs::write(root.join("stray.txt"), "x").unwrap();
    assert_eq!(scan_repos(root), ["SteepMans/chimney", "SteepMans/warp"]);
    assert!(scan_repos(&root.join("missing")).is_empty());
}

#[test]
fn finds_tools_in_path() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("codex.cmd"), "").unwrap();
    std::fs::write(bin.join("claude"), "").unwrap();
    let path = std::env::join_paths([dir.path().join("empty"), bin.clone()]).unwrap();
    let windows_ext = [".exe".to_owned(), ".cmd".to_owned()];
    assert_eq!(
        find_in_path("codex", &path, &windows_ext),
        Some(bin.join("codex.cmd"))
    );
    assert_eq!(find_in_path("claude", &path, &windows_ext), None);
    assert_eq!(find_in_path("claude", &path, &[]), Some(bin.join("claude")));
    assert_eq!(find_in_path("git", &path, &[]), None);
}

#[test]
fn real_machine_reports_something() {
    let machine = RealMachine::default();
    let dir = tempfile::tempdir().unwrap();
    let hardware = machine.hardware(dir.path());
    assert!(hardware.cpu_cores > 0);
    assert!(hardware.ram_total_bytes > 0);
    let resources = machine.resources(dir.path());
    assert!(resources.ram_free_bytes > 0);
    assert!(machine.is_alive(std::process::id()));
    assert!(machine.runner().is_some() || cfg!(windows));
}
