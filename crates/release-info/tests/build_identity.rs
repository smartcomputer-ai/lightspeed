use std::{fs, path::Path, process::Command};

fn run(directory: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(directory)
        .env_remove("LIGHTSPEED_GIT_SHA")
        .env_remove("LIGHTSPEED_RELEASE_VERSION")
        .env_remove("LIGHTSPEED_ENVD_TARGETS")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("execute fixture command");
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit(directory: &Path, message: &str) -> String {
    run(directory, "git", &["add", "."]);
    run(
        directory,
        "git",
        &[
            "-c",
            "user.name=Build Test",
            "-c",
            "user.email=build@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            message,
        ],
    );
    run(directory, "git", &["rev-parse", "HEAD"])
}

fn built_sha(directory: &Path) -> String {
    run(directory, env!("CARGO"), &["run", "--quiet", "--offline"])
}

#[test]
fn cached_build_identity_tracks_branch_commits_packed_refs_and_worktrees() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"build-identity-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
    )
    .unwrap();
    fs::write(repo.join("build.rs"), include_str!("../build.rs")).unwrap();
    fs::write(
        repo.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"LIGHTSPEED_BUILD_GIT_SHA\")); }\n",
    )
    .unwrap();
    fs::write(repo.join(".gitignore"), "/target\n/Cargo.lock\n").unwrap();
    run(&repo, "git", &["init", "-q", "-b", "main"]);

    let first = commit(&repo, "first");
    assert_eq!(built_sha(&repo), first);
    let second = commit(&repo, "advance branch without source changes");
    assert_ne!(first, second);
    assert_eq!(built_sha(&repo), second);

    run(&repo, "git", &["pack-refs", "--all", "--prune"]);
    assert_eq!(built_sha(&repo), second);
    let third = commit(&repo, "advance a packed branch");
    assert_eq!(built_sha(&repo), third);

    run(&repo, "git", &["checkout", "-q", "--detach", &first]);
    assert_eq!(built_sha(&repo), first);

    let worktree = root.path().join("worktree");
    run(
        &repo,
        "git",
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    assert_eq!(built_sha(&worktree), first);
    let linked = commit(&worktree, "advance linked worktree");
    assert_eq!(built_sha(&worktree), linked);
}
