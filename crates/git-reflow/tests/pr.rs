use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_api::git::stage_and_commit;
use git_reflow_fixtures::{add_origin_remote, project_repo};
use git2::Repository;
use httpmock::prelude::*;
use rstest::rstest;
use serde_json::json;
use std::fs;

/// Tests that the `pr` command makes no changes when there are no changes to release.
#[rstest]
fn test_pr_command_noop_when_no_changes(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    // NB: We don't expect any API calls to be made, so don't add any mocks.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "github": { "host": server.base_url() } } } }).to_string())
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    for tag in ["v1.0.0", "example-api-v1.0.0", "example-cli-v1.0.0"] {
        repo.tag_lightweight(tag, commit.as_object(), false).unwrap();
    }

    // Run the `pr` command and check its JSON output.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert()
        .success()
        .stdout("[]\n");

    // Verify the Git repository state remains unchanged, and that the release branches do not exist.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(repo.find_reference("refs/heads/reflow--branches--main").is_err());
    assert!(remote_repo.find_reference("refs/heads/reflow--branches--main").is_err());
}

/// Tests that an empty release plan short-circuits and prints an empty array.
#[rstest]
fn test_pr_command_with_empty_plan(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository.
    let (project_dir, _) = project_repo;

    // Write an empty plan for the command to load.
    let plan_path = project_dir.path().join("plan.json");
    fs::write(&plan_path, "[]").unwrap();

    // Run the `pr` command and check its JSON output.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .args(["pr", "--target-branch", "main", "--plan"])
        .arg(&plan_path)
        .assert()
        .success()
        .stdout("[]\n");
}
