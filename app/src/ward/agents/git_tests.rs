use std::path::Path;

use command::blocking::Command;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A bare `origin` at `<dir>/remotes/Owner/repo.git` with `main` and `feature/existing`.
fn remote(dir: &Path) -> String {
    let seed = dir.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "hello\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    git(&seed, &["checkout", "-q", "-b", "feature/existing"]);
    std::fs::write(seed.join("feature.txt"), "feature\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "feature"]);
    let bare = dir.join("remotes").join("Owner").join("repo.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    let output = Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .output()
        .unwrap();
    assert!(output.status.success());
    bare.to_string_lossy().into_owned()
}

#[test]
fn prepares_clone_and_worktrees() {
    if crate::ward::agents::machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = remote(dir.path());
    let root = dir.path().join("agents-work");
    let repo = launch::parse_repo(&url).unwrap();
    assert_eq!(repo.to_string(), "Owner/repo");

    let spec = WorktreeSpec {
        repo_url: &url,
        repo: &repo,
        base_branch: "main",
        branch: "feature/4623-new",
        session_id: "5f0e8a5b-0c3e-4d2f-9a64-3b1e4f6d7a20",
        search: &[],
    };
    let prepared = prepare_worktree(&root, &spec).unwrap();
    let worktree = prepared.path.clone();
    let clone = root.join("Owner").join("repo");
    assert_eq!(worktree, clone.join(".worktrees").join("5f0e8a5b"));
    assert!(!prepared.local_clone);
    assert_eq!(prepared.base.as_deref(), Some("main"));
    assert_eq!(prepared.base_note, None);
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/4623-new"
    );
    assert!(worktree.join("README.md").is_file());
    assert!(!worktree.join("feature.txt").exists());
    let exclude = std::fs::read_to_string(clone.join(".git").join("info").join("exclude")).unwrap();
    assert!(exclude.contains("/.agents/") && exclude.contains("/.worktrees/"));

    // Ward's files do not show up as changes.
    std::fs::create_dir_all(worktree.join(".agents")).unwrap();
    std::fs::write(worktree.join(".agents").join("prompt.md"), "p").unwrap();
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
    assert_eq!(git(&clone, &["status", "--porcelain"]), "");

    // The same session again reuses its worktree.
    assert_eq!(prepare_worktree(&root, &spec).unwrap().path, worktree);

    // A branch that exists on origin is checked out from there, after a fetch.
    let existing = WorktreeSpec {
        branch: "feature/existing",
        session_id: "a1b2c3d4-0000",
        ..spec.clone()
    };
    let second = prepare_worktree(&root, &existing).unwrap();
    assert!(second.path.join("feature.txt").is_file());
    assert_eq!(second.base, None);

    let bad = WorktreeSpec {
        branch: "bad..name",
        session_id: "b1b2c3d4",
        ..spec.clone()
    };
    assert!(prepare_worktree(&root, &bad).is_err());
    // A base origin does not have: the default branch of origin, with a note.
    let no_base = WorktreeSpec {
        base_branch: "develop",
        branch: "feature/other",
        session_id: "c1b2c3d4",
        ..spec
    };
    // (The bare origin was made with `feature/existing` checked out: that is its default.)
    let prepared = prepare_worktree(&root, &no_base).unwrap();
    assert_eq!(prepared.base.as_deref(), Some("feature/existing"));
    let note = prepared.base_note.unwrap();
    assert!(
        note.contains("develop") && note.contains("origin/feature/existing"),
        "{note}"
    );
}

#[test]
fn clone_failure_is_reported() {
    if crate::ward::agents::machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = dir.path().join("missing").join("Owner").join("none.git");
    let url = url.to_string_lossy().into_owned();
    let repo = launch::parse_repo(&url).unwrap();
    let err = prepare_worktree(
        &dir.path().join("root"),
        &WorktreeSpec {
            repo_url: &url,
            repo: &repo,
            base_branch: "main",
            branch: "feature/x",
            session_id: "5f0e8a5b",
            search: &[],
        },
    )
    .unwrap_err();
    assert!(err.starts_with("git clone"), "{err}");
}

/// A bare `origin` at `<dir>/remotes/<owner>/<name>.git` whose only branch, `default`, is its
/// default branch.
fn remote_with_default(dir: &Path, owner: &str, name: &str, default: &str) -> String {
    let seed = dir.join(format!("seed-{name}"));
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", default]);
    std::fs::write(seed.join("README.md"), "stable\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    let bare = dir.join("remotes").join(owner).join(format!("{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    let output = Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .output()
        .unwrap();
    assert!(output.status.success());
    bare.to_string_lossy().into_owned()
}

/// The owner's own clone of `url` at `path`.
fn clone_to(url: &str, path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let output = Command::new("git")
        .args(["clone", "-q", "--", url])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A folder that looks like a clone of `url` to the search: `.git/config` only.
fn fake_clone(path: &Path, url: &str) {
    let git_dir = path.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(
        git_dir.join("config"),
        format!(
            "[core]\n\trepositoryformatversion = 0\n[remote \"upstream\"]\n\turl = https://example.test/other/x.git\n[remote \"origin\"]\n\turl = {}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
            url.replace('\\', "\\\\")
        ),
    )
    .unwrap();
}

#[test]
fn remote_urls_are_compared_without_their_form() {
    let https = "https://github.com/HassleGames/game-server.git";
    for same in [
        "https://github.com/HassleGames/game-server",
        "https://github.com/hasslegames/Game-Server.git/",
        "https://user@github.com/HassleGames/game-server.git",
        "http://www.github.com/HassleGames/game-server",
        "git@github.com:HassleGames/game-server.git",
        "git@github.com:/hasslegames/game-server",
        "ssh://git@github.com/HassleGames/game-server.git",
        "ssh://git@github.com:22/HassleGames/game-server",
        "  https://github.com/HassleGames/game-server.git\n",
    ] {
        assert!(same_remote(https, same), "{same}");
    }
    for other in [
        "https://github.com/HassleGames/game-server-docs.git",
        "https://github.com/Other/game-server.git",
        "https://gitlab.com/HassleGames/game-server.git",
        "",
    ] {
        assert!(!same_remote(https, other), "{other}");
    }
    assert_eq!(
        remote_key(https).as_deref(),
        Some("github.com/hasslegames/game-server")
    );
    // Local paths: Windows separators and the drive letter, with or without `file://`.
    assert!(same_remote(
        r"C:\work\remotes\Owner\repo.git",
        "c:/work/remotes/owner/repo"
    ));
    assert!(same_remote(
        "file:///C:/work/remotes/Owner/repo.git",
        r"C:\work\remotes\Owner\repo.git"
    ));
    assert!(!same_remote(
        r"C:\work\remotes\Owner\repo.git",
        r"D:\work\remotes\Owner\repo.git"
    ));
}

#[test]
fn config_values_are_unescaped() {
    assert_eq!(
        config_value(r" C:\\work\\remotes\\Owner\\repo.git "),
        r"C:\work\remotes\Owner\repo.git"
    );
    assert_eq!(
        config_value(r#""C:/work/hassle online/x.git" # comment"#),
        "C:/work/hassle online/x.git"
    );
    assert_eq!(
        config_value("https://github.com/Owner/repo.git ; note"),
        "https://github.com/Owner/repo.git"
    );
    assert_eq!(config_value(r#""a;b\"c""#), r#"a;b"c"#);
}

#[test]
fn default_branch_of_ls_remote() {
    assert_eq!(
        parse_symref_head(
            "ref: refs/heads/stable\tHEAD\n0123456789abcdef0123456789abcdef01234567\tHEAD\n"
        )
        .as_deref(),
        Some("stable")
    );
    assert_eq!(parse_symref_head("0123\tHEAD\n"), None);
    assert_eq!(parse_symref_head(""), None);
}

#[test]
fn local_clones_are_found_by_origin() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let wanted = "https://github.com/HassleGames/game-server.git";
    // Depth 2, with a space in the folder name, like `C:/work/hassle online/radmir-crmp`.
    let crmp = work.join("hassle online").join("radmir-crmp");
    fake_clone(&crmp, wanted);
    fake_clone(
        &work.join("hassle online").join("docs"),
        "git@github.com:HassleGames/docs.git",
    );
    // Never searched: dependencies, hidden folders, other repositories inside a working copy
    // and what is deeper than the search goes.
    fake_clone(&work.join("app").join("node_modules").join("x"), wanted);
    fake_clone(&work.join(".cache").join("x"), wanted);
    fake_clone(
        &crmp.join("vendor").join("inner"),
        "https://github.com/Inner/inner.git",
    );
    fake_clone(
        &work.join("a").join("b").join("c").join("deep"),
        "https://github.com/Deep/deep.git",
    );

    let search = vec![work.clone()];
    let found = find_local_clone(&search, "git@github.com:hasslegames/Game-Server").unwrap();
    assert_eq!(found, crmp);
    assert_eq!(
        find_local_clone(&search, "https://github.com/Other/none.git"),
        None
    );
    assert_eq!(
        local_repos(&search),
        ["HassleGames/docs", "HassleGames/game-server"]
    );
    // An entry may be the clone itself; a missing entry is skipped.
    let direct = vec![dir.path().join("missing"), crmp.clone()];
    assert_eq!(find_local_clone(&direct, wanted), Some(crmp.clone()));
    assert_eq!(
        find_clones(&[crmp.clone(), work.clone()]).first(),
        Some(&crmp)
    );
    assert_eq!(
        find_clones(&[crmp.clone(), work])
            .iter()
            .filter(|path| **path == crmp)
            .count(),
        1
    );
    assert_eq!(find_local_clone(&[], wanted), None);
}

#[test]
fn worktree_from_the_owners_clone_leaves_it_as_it_is() {
    if crate::ward::agents::machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = remote(dir.path());
    let work = dir.path().join("work");
    let owner_clone = work.join("group").join("my-copy");
    clone_to(&url, &owner_clone);
    // The owner is on a branch of their own with changes not committed.
    git(&owner_clone, &["checkout", "-q", "-b", "owner/wip"]);
    std::fs::write(owner_clone.join("README.md"), "owner's edit\n").unwrap();
    std::fs::write(owner_clone.join("notes.txt"), "mine\n").unwrap();
    let head_before = git(&owner_clone, &["rev-parse", "HEAD"]);
    let status_before = git(&owner_clone, &["status", "--porcelain"]);
    // The base came to origin after the owner cloned: the worktree starts from the fresh one.
    let seed = dir.path().join("seed");
    git(&seed, &["checkout", "-q", "main"]);
    std::fs::write(seed.join("fresh.txt"), "fresh\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "fresh"]);
    git(&seed, &["push", "-q", &url, "main"]);

    let root = dir.path().join("agents-work");
    let repo = launch::parse_repo(&url).unwrap();
    let search = vec![work.clone()];
    let spec = WorktreeSpec {
        repo_url: &url,
        repo: &repo,
        base_branch: "main",
        branch: "feature/4640-local",
        session_id: "5f0e8a5b-0c3e-4d2f-9a64-3b1e4f6d7a20",
        search: &search,
    };
    let prepared = prepare_worktree(&root, &spec).unwrap();
    let worktree = root
        .join(".worktrees")
        .join("Owner")
        .join("repo")
        .join("5f0e8a5b");
    assert_eq!(prepared.path, worktree);
    assert_eq!(prepared.clone, owner_clone);
    assert!(prepared.local_clone);
    assert_eq!(prepared.base.as_deref(), Some("main"));
    assert_eq!(prepared.base_note, None);
    let summary = prepared.summary().unwrap();
    assert!(
        summary.contains("origin/main") && summary.contains("my-copy"),
        "{summary}"
    );
    // Nothing was cloned into the root.
    assert!(!root.join("Owner").join("repo").exists());

    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/4640-local"
    );
    assert!(worktree.join("fresh.txt").is_file());
    // The owner's working copy: same branch, same commit, same changes, no Ward folders.
    assert_eq!(
        git(&owner_clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "owner/wip"
    );
    assert_eq!(git(&owner_clone, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(&owner_clone, &["status", "--porcelain"]), status_before);
    assert!(!owner_clone.join(".worktrees").exists());
    assert_eq!(
        std::fs::read_to_string(owner_clone.join("README.md")).unwrap(),
        "owner's edit\n"
    );
    // Ward's files in the worktree are not changes, and the worktree belongs to the owner's
    // clone, so `git worktree remove` / `prune` there clean it up.
    std::fs::create_dir_all(worktree.join(".agents")).unwrap();
    std::fs::write(worktree.join(".agents").join("prompt.md"), "p").unwrap();
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
    let list = git(&owner_clone, &["worktree", "list", "--porcelain"]);
    assert!(list.contains("feature/4640-local"), "{list}");

    // The same session again reuses its worktree.
    assert_eq!(prepare_worktree(&root, &spec).unwrap().path, worktree);

    git(
        &owner_clone,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    assert!(!worktree.exists());
    assert_eq!(
        git(&owner_clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "owner/wip"
    );
}

#[test]
fn without_a_matching_local_clone_the_repository_is_cloned() {
    if crate::ward::agents::machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = remote(dir.path());
    let work = dir.path().join("work");
    fake_clone(&work.join("other"), "https://github.com/Other/other.git");
    let root = dir.path().join("agents-work");
    let repo = launch::parse_repo(&url).unwrap();
    let search = vec![work];
    let prepared = prepare_worktree(
        &root,
        &WorktreeSpec {
            repo_url: &url,
            repo: &repo,
            base_branch: "main",
            branch: "feature/x",
            session_id: "5f0e8a5b",
            search: &search,
        },
    )
    .unwrap();
    let clone = root.join("Owner").join("repo");
    assert!(!prepared.local_clone);
    assert_eq!(prepared.clone, clone);
    assert_eq!(prepared.path, clone.join(".worktrees").join("5f0e8a5b"));
}

#[test]
fn a_missing_base_falls_back_to_the_default_branch() {
    if crate::ward::agents::machine::which("git").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Like HassleGames/game-server: `stable` is the default branch, there is no `main`.
    let url = remote_with_default(dir.path(), "HassleGames", "game-server", "stable");
    let root = dir.path().join("agents-work");
    let repo = launch::parse_repo(&url).unwrap();
    let spec = WorktreeSpec {
        repo_url: &url,
        repo: &repo,
        base_branch: "main",
        branch: "feature/4640-fix",
        session_id: "a1b2c3d4",
        search: &[],
    };
    let prepared = prepare_worktree(&root, &spec).unwrap();
    assert_eq!(prepared.base.as_deref(), Some("stable"));
    let note = prepared.base_note.clone().unwrap();
    assert!(
        note.contains("main") && note.contains("origin/stable"),
        "{note}"
    );
    assert_eq!(prepared.summary(), Some(note));
    assert_eq!(
        git(&prepared.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/4640-fix"
    );
    assert_eq!(
        git(&prepared.path, &["rev-parse", "HEAD"]),
        git(&prepared.clone, &["rev-parse", "origin/stable"])
    );

    // No base at all, and no `origin/HEAD` in the clone: the remote says which is default.
    git(&prepared.clone, &["remote", "set-head", "origin", "-d"]);
    let no_base = WorktreeSpec {
        base_branch: "",
        branch: "feature/4640-other",
        session_id: "b1b2c3d4",
        ..spec.clone()
    };
    let prepared = prepare_worktree(&root, &no_base).unwrap();
    assert_eq!(prepared.base.as_deref(), Some("stable"));
    assert!(
        prepared
            .base_note
            .as_deref()
            .is_some_and(|note| note.contains("origin/stable")),
        "{:?}",
        prepared.base_note
    );

    // A base that exists is taken as it is, without a note.
    let stable = WorktreeSpec {
        base_branch: "stable",
        branch: "feature/4640-third",
        session_id: "c1b2c3d4",
        ..spec
    };
    let prepared = prepare_worktree(&root, &stable).unwrap();
    assert_eq!(prepared.base.as_deref(), Some("stable"));
    assert_eq!(prepared.base_note, None);
}
