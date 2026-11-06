use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_api::git::stage_and_commit;
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, add_origin_remote, commit_patch, project_repo};
use git2::Repository;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use rstest::rstest;
use serde_json::json;
use std::fs;
use std::io::Write;

/// Tests that the `pr` command handles additional included paths, e.g. `include-changes = ["crates/example-api"]`.
#[rstest]
fn test_pr_command_with_include_changes(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote.
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
        when.method(POST)
            .path("/repos/octocat/Hello-World/pulls")
            // NB: We expect the first package's commit message to be used in the title for a combined pull request.
            .json_body_includes(&json!({ "title": "chore: release v1.1.0" }).to_string());
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API.
    // - Having `example-cli` include changes from the `example-api` package;
    // - Having `example-cli` emit its changelog to the root of the repository.
    std::fs::remove_file(project_dir.child(".reflow.yaml")).unwrap(); // NB: Remove the config from the fixture.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } },
                "packages": [
                    {
                        "dir": "crates/example-cli",
                        "name": "example-cli",
                        "scope": "", // NB: The root dir `.` defaults to no scope, but this is a subdir, so clear it.
                        "changelog-path": "../../CHANGELOG.md",
                        "include-changes": ["crates/example-api"],
                        "include-name-in-tag": false, // NB: The first package defaults to false, but still set it.
                    },
                    {
                        "dir": "crates/example-api",
                        "name": "example-api",
                        "scope": "api",
                    },
                ],
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();
    repo.tag_lightweight("example-api-v1.0.0", commit.as_object(), false)
        .unwrap();

    // Create some changes in the packages and commit those changes.
    let lib_rs_path = project_dir.path().join("crates/example-api/src/lib.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

    // Create a change that affects both packages and commit that change.
    // NB: We expect the commit to be included *once* in each package's changelog.
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn multiply(a: i32, b: i32) -> i32 {{ a * b }}"
    )
    .unwrap();
    let main_rs_path = project_dir.path().join("crates/example-cli/src/main.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&main_rs_path).unwrap(),
        "\npub fn multiply(a: i32, b: i32) -> i32 {{ example_api::multiply(5, 9) }}"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat: add and consume a `multiply` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command for the changed package.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert();

    // Verify that a pull request was created for the package.
    // NB: We expect one combined pull request, since `separate_pull_requests` is false by default.
    list_mock.assert_calls(1);
    create_mock.assert_calls(1);
    let assert = assert.success();

    // Verify the Git repository state remains unchanged, and that the release branch was pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    let release_commit = remote_repo
        .find_reference("refs/heads/reflow--branches--main")
        .unwrap()
        .peel_to_commit()
        .unwrap();

    // Verify the output of the command as expected.
    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
        insta::assert_snapshot!("pr_command_with_include_changes_diff", commit_patch(&remote_repo, &release_commit));
    });
}

/// Tests that the `pr` command updates additional JSON, YAML, TOML, and Markdown files.
#[rstest]
fn test_pr_command_with_update_files(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote.
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
        when.method(POST)
            .path("/repos/octocat/Hello-World/pulls")
            .json_body_includes(&json!({ "title": "chore: release v1.1.0" }).to_string());
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API;
    // - Updating version fields in JSON, YAML, and TOML files, and a marked version in Markdown.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } },
                "packages": [
                    {
                        "dir": ".",
                        "update-files": [
                            { "path": "version.json", "field": "$.version", "value": "{{ version }}" },
                            { "path": "version.yaml", "field": "version", "value": "{{ version }}" },
                            { "path": "version.toml", "field": "version", "value": "{{ version }}" },
                            { "path": "README.md" },
                        ],
                    },
                ],
            })
            .to_string(),
        )
        .unwrap();

    // Create the additional files with the current version of the package.
    project_dir
        .child("version.json")
        .write_str("{\n  \"version\": \"1.0.0\"\n}\n")
        .unwrap();
    project_dir.child("version.yaml").write_str("version: 1.0.0\n").unwrap();
    project_dir
        .child("version.toml")
        .write_str("version = \"1.0.0\"\n")
        .unwrap();
    project_dir
        .child("README.md")
        .write_str("# example-rust\n\nVersion: v1.0.0 <!-- x-git-reflow-version -->\n")
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create a Git tag for the current version of the package.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();

    // Create some changes in the package and commit those changes.
    let main_rs_path = project_dir.path().join("src").join("main.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&main_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat: add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command for the changed package.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert();

    // Verify that a pull request was created for the package.
    list_mock.assert_calls(1);
    create_mock.assert_calls(1);
    let assert = assert.success();

    // Verify the Git repository state remains unchanged, and that the release branch was pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    let release_commit = remote_repo
        .find_reference("refs/heads/reflow--branches--main")
        .unwrap()
        .peel_to_commit()
        .unwrap();

    // Verify the output of the command as expected.
    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
        insta::assert_snapshot!("pr_command_with_update_files_diff", commit_patch(&remote_repo, &release_commit));
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
