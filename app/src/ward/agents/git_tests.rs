use command::blocking::Command;
use std::path::Path;

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
    };
    let worktree = prepare_worktree(&root, &spec).unwrap();
    let clone = root.join("Owner").join("repo");
    assert_eq!(worktree, clone.join(".worktrees").join("5f0e8a5b"));
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
    assert_eq!(prepare_worktree(&root, &spec).unwrap(), worktree);

    // A branch that exists on origin is checked out from there, after a fetch.
    let existing = WorktreeSpec {
        branch: "feature/existing",
        session_id: "a1b2c3d4-0000",
        ..spec.clone()
    };
    let second = prepare_worktree(&root, &existing).unwrap();
    assert!(second.join("feature.txt").is_file());

    let bad = WorktreeSpec {
        branch: "bad..name",
        session_id: "b1b2c3d4",
        ..spec.clone()
    };
    assert!(prepare_worktree(&root, &bad).is_err());
    let no_base = WorktreeSpec {
        base_branch: "develop",
        branch: "feature/other",
        session_id: "c1b2c3d4",
        ..spec
    };
    let err = prepare_worktree(&root, &no_base).unwrap_err();
    assert!(err.contains("develop"), "{err}");
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
        },
    )
    .unwrap_err();
    assert!(err.starts_with("git clone"), "{err}");
}

