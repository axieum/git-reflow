use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, add_origin_remote, project_repo};
use git2::Repository;
use httpmock::prelude::*;
use rstest::rstest;
use serde_json::json;
use std::fs;
use std::io::Write;

/// Tests that the `pr` command writes changes, pushes a release branch, and outputs the created pull request.
#[rstest]
fn test_pr_command_creates_a_pull_request(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();
    let list_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/repos/octocat/Hello-World/pulls")
            .query_param("head", "octocat:reflow--branches--main")
            .query_param("base", "main");
        then.status(200).header("content-type", "application/json").body("[]");
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST).path("/repos/octocat/Hello-World/pulls");
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({"git": {"provider": {"github": {"host": server.base_url()}}}}).to_string())
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, ["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    for tag in ["v1.0.0", "example-api-v1.0.0", "example-cli-v1.0.0"] {
        repo.tag_lightweight(tag, commit.as_object(), false).unwrap();
    }

    // Create some changes in the packages and commit those changes.
    let lib_rs_path = project_dir.path().join("crates/example-api/src/lib.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    git_reflow_api::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command for the changed package.
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert();

    // Verify the Git repository state remains unchanged, and that the release branch was pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(remote_repo.find_reference("refs/heads/reflow--branches--main").is_ok());

    // Verify that a pull request was created for the release branch.
    // NB: We expect one combined pull request, since `separate_pull_requests` is false by default.
    list_mock.assert_calls(1);
    create_mock.assert_calls(1);

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `pr` command writes changes, pushes multiple release branches, and outputs the created pull requests.
#[rstest]
fn test_pr_command_creates_separate_pull_requests(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();
    let server_mocks = vec![
        server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/pulls")
                .query_param("head", "octocat:reflow--branches--main--example-rust-workspace")
                .query_param("base", "main");
            then.status(200).header("content-type", "application/json").body("[]");
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/pulls")
                .body_includes("example-rust-workspace");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
                ));
        }),
        server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/pulls")
                .query_param("head", "octocat:reflow--branches--main--example-api")
                .query_param("base", "main");
            then.status(200).header("content-type", "application/json").body("[]");
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/pulls")
                .body_includes("example-api");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
                ));
        }),
    ];

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Enabling separate pull requests for each package.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "github": { "host": server.base_url() } },
                    "separate-pull-requests": true,
                },
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, ["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    for tag in ["v1.0.0", "example-api-v1.0.0", "example-cli-v1.0.0"] {
        repo.tag_lightweight(tag, commit.as_object(), false).unwrap();
    }

    // Create some changes in the packages and commit those changes.
    let lib_rs_path = project_dir.path().join("crates/example-api/src/lib.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    git_reflow_api::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command for the changed packages.
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert()
        .success();

    // Verify the Git repository state remains unchanged, and that the release branches were pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    for branch in [
        "reflow--branches--main--example-rust-workspace",
        "reflow--branches--main--example-api",
    ] {
        assert!(remote_repo.find_reference(&format!("refs/heads/{}", branch)).is_ok());
    }

    // Verify that multiple pull requests were created for each release branch.
    // NB: We expect two pull requests, since `separate_pull_requests` was enabled.
    for mock in server_mocks {
        mock.assert_calls(1);
    }

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

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
        .write_str(&json!({"git": {"provider": {"github": {"host": server.base_url()}}}}).to_string())
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, ["."], "chore: initial commit").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    for tag in ["v1.0.0", "example-api-v1.0.0", "example-cli-v1.0.0"] {
        repo.tag_lightweight(tag, commit.as_object(), false).unwrap();
    }

    // Run the `pr` command and check its JSON output.
    Command::cargo_bin(env!("CARGO_PKG_NAME"))
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

/// Tests that the `pr` command with `--dry-run` reports on the activity that would happen without taking action.
#[rstest]
fn test_pr_command_with_dry_run(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    // NB: We don't expect any API calls to be made, so don't add any mocks.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({"git": {"provider": {"github": {"host": server.base_url()}}}}).to_string())
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, ["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    for tag in ["v1.0.0", "example-api-v1.0.0", "example-cli-v1.0.0"] {
        repo.tag_lightweight(tag, commit.as_object(), false).unwrap();
    }

    // Create some changes in the packages and commit those changes.
    let lib_rs_path = project_dir.path().join("crates/example-api/src/lib.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    git_reflow_api::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command with `--dry-run` for the changed package.
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .arg("--dry-run")
        .assert();

    // Verify the Git repository state remains unchanged, and that the release branches do not exist.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(repo.find_reference("refs/heads/reflow--branches--main").is_err());
    assert!(remote_repo.find_reference("refs/heads/reflow--branches--main").is_err());

    // Verify that a pull request was reported on for the release branch.
    // NB: We expect one combined pull request, since `separate_pull_requests` is false by default.

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that an empty release plan short-circuits and prints an empty array.
#[rstest]
fn test_pr_command_with_empty_plan(#[with("example-python-uv-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository.
    let (project_dir, _) = project_repo;

    // Write an empty plan for the command to load.
    let plan_path = project_dir.path().join("plan.json");
    fs::write(&plan_path, "[]").unwrap();

    // Run the `pr` command and check its JSON output.
    Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .args(["pr", "--target-branch", "main", "--plan"])
        .arg(&plan_path)
        .assert()
        .success()
        .stdout("[]\n");
}
