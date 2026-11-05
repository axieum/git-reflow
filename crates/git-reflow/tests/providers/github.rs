use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_api::git::stage_and_commit;
use git_reflow_api::provider::{PullRequestPackage, build_pull_request_body};
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, add_origin_remote, project_repo};
use git2::Repository;
use httpmock::prelude::*;
use rstest::rstest;
use serde_json::json;
use std::fs;
use std::io::Write;

/// Tests that the `pr` command writes changes, pushes a release branch, and outputs the created pull request
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_pr_command(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
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
                "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } }
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
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
    assert!(remote_repo.find_reference("refs/heads/reflow--branches--main").is_ok());

    // Verify the output of the command as expected.
    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `pr` command writes changes, pushes multiple release branches, and outputs the created pull requests
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
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
                    "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
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
                .body_includes("reflow--branches--main--example-api");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
                ));
        }),
        server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/pulls")
                .query_param("head", "octocat:reflow--branches--main--example-cli")
                .query_param("base", "main");
            then.status(200).header("content-type", "application/json").body("[]");
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/pulls")
                .body_includes("reflow--branches--main--example-cli");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
                ));
        }),
    ];

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API;
    // - Enabling separate pull requests for each package.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "github": { "host": server.base_url(), "commit-via-api": false } },
                    "separate-pull-requests": true,
                },
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command for the changed packages.
    let assert = Command::cargo_bin("git-reflow")
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
        "reflow--branches--main--example-cli",
    ] {
        assert!(remote_repo.find_reference(&format!("refs/heads/{}", branch)).is_ok());
    }

    // Verify that multiple pull requests were created for each release branch.
    // NB: We expect three pull requests, including the dependent CLI package.
    for mock in server_mocks {
        mock.assert_calls(1);
    }

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `pr` command pushes a release branch, writes changes via the GitHub API, and outputs the created
/// pull request for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_pr_command_commits_via_github_api(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub pull request API.
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
                "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Enabling commits via the GitHub API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": true } } }
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Mock a GitHub API commit against the original branch head with the release file changes.
    let api_commit_id = "0123456789abcdef0123456789abcdef01234567";
    let commit_mock = server.mock(|when, then| {
        when.method(POST).path("/graphql").json_body_includes(
            json!({
                "query": "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
                "variables": {
                    "input": {
                        "branch": {
                            "repositoryNameWithOwner": "octocat/Hello-World",
                            "branchName": "reflow--branches--main",
                        },
                        "expectedHeadOid": head.to_string(),
                        "message": { "headline": "chore: release v1.1.0", "body": null },
                        "fileChanges": {
                            "additions": [
                                { "path": "CHANGELOG.md" },
                                { "path": "Cargo.lock" },
                                { "path": "crates/example-api/CHANGELOG.md" },
                                { "path": "crates/example-api/Cargo.toml" },
                                { "path": "crates/example-cli/CHANGELOG.md" },
                                { "path": "crates/example-cli/Cargo.toml" },
                            ],
                            "deletions": [],
                        },
                    },
                },
            })
                .to_string(),
        );
        then.status(200).json_body(json!({
            "data": { "createCommitOnBranch": { "commit": { "oid": api_commit_id } } },
        }));
    });

    // Run the `pr` command for the changed packages.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert()
        .success();
    assert!(assert.get_output().stdout.len() > 3, "expected non-empty JSON output");

    // Verify that the original branch, commit and clean working directory were restored.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    // Verify that the release changes were committed via the API and one combined pull request was created.
    commit_mock.assert_calls(1);
    list_mock.assert_calls(1);
    create_mock.assert_calls(1);

    // Verify that the release branch was pushed without creating a local release commit.
    // NB: The mocked API does not update the remote branch with its returned commit.
    assert_eq!(repo.refname_to_id("refs/heads/reflow--branches--main").unwrap(), head);
    assert_eq!(
        remote_repo.refname_to_id("refs/heads/reflow--branches--main").unwrap(),
        head,
    );
}

/// Tests that the `pr` command makes no changes or pull requests when the remote is already up-to-date
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_pr_command_noop_when_remote_is_up_to_date(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API for the initial pull request.
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
                "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } }
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command once to publish the release branch and create its pull request.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert()
        .success();
    assert!(assert.get_output().stdout.len() > 3, "expected non-empty JSON output");
    let remote_commit_id = remote_repo.refname_to_id("refs/heads/reflow--branches--main").unwrap();
    assert_ne!(remote_commit_id, head);

    // Run the `pr` command again and check that its JSON output contains no pull requests.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("pr")
        .assert()
        .success()
        .stdout("[]\n");

    // Verify that the original branch, commit and clean working directory were restored.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    // Verify that the remote release branch is unchanged and no new local release commit was created.
    assert_eq!(
        remote_repo.refname_to_id("refs/heads/reflow--branches--main").unwrap(),
        remote_commit_id,
    );
    assert_eq!(repo.refname_to_id("refs/heads/reflow--branches--main").unwrap(), head);

    // Verify that only the initial run made API requests, with no pull request updates on the second run.
    list_mock.assert_calls(1);
    create_mock.assert_calls(1);
}

/// Tests that the `pr` command with `--dry-run` reports on the activity that would happen without taking action
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_pr_command_with_dry_run(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
    #[values(false, true)] commit_via_api: bool,
) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    // NB: We don't expect any API calls to be made, so don't add any mocks.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Optionally enabling commits via the GitHub API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": commit_via_api } } }
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command with `--dry-run` for the changed package.
    let assert = Command::cargo_bin("git-reflow")
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

    // Verify that a pull request was _reported_ on for the release branch.
    // NB: We expect one combined pull request, since `separate_pull_requests` is false by default.

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::allow_duplicates! {
            insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
        }
    });
}

/// Tests that the `release` command creates Git tags, releases, and outputs a list of released packages
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_release_command(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } }
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let head = stage_and_commit(&repo, &["."], "chore: release v0.3.0").unwrap();

    // Prepare the packages included in the release pull request.
    let packages = vec![
        PullRequestPackage {
            name: "example-rust-workspace".to_string(),
            version: "0.3.0".parse().unwrap(),
            changelog_md: Some(
                "## [0.3.0] - 2026-09-26\n\n### Features\n\n- Add a `subtract` function and print subtractions"
                    .to_string(),
            ),
        },
        PullRequestPackage {
            name: "example-api".to_string(),
            version: "0.2.0".parse().unwrap(),
            changelog_md: Some("## [0.2.0] - 2026-09-26\n\n### Features\n\n- Add a `subtract` function".to_string()),
        },
        PullRequestPackage {
            name: "example-cli".to_string(),
            version: "0.3.0".parse().unwrap(),
            changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions".to_string()),
        },
    ];
    let expected_releases = [
        // The expected tag name, release title, and whether the package should be marked as the latest release.
        ("v0.3.0", "v0.3.0", true),
        ("example-api-v0.2.0", "example-api: v0.2.0", false),
        ("example-cli-v0.3.0", "example-cli: v0.3.0", false),
    ];

    // Mock the release pull request associated with the current commit and the creation of its releases.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main");
        body[0]["body"] = json!(build_pull_request_body(packages.clone()).unwrap());

        when.method(GET)
            .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let mut server_mocks = Vec::new();
    for (pkg, (tag_name, title, make_latest)) in packages.iter().zip(expected_releases) {
        server_mocks.push(server.mock(|when, then| {
            when.method(GET)
                .path(format!("/repos/octocat/Hello-World/releases/tags/{tag_name}"));
            then.status(404)
                .header("content-type", "application/json")
                .body(r#"{"message":"Not Found"}"#);
        }));
        server_mocks.push(server.mock(|when, then| {
            let mut body: serde_json::Value = serde_json::from_str(include_str!(
                "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ))
            .unwrap();
            body["html_url"] = json!(format!("https://github.com/octocat/Hello-World/releases/{tag_name}"));

            when.method(POST)
                .path("/repos/octocat/Hello-World/releases")
                .json_body(json!({
                    "tag_name": tag_name,
                    "target_commitish": head.to_string(),
                    "name": title,
                    "body": pkg.changelog_md,
                    "make_latest": if make_latest { "true" } else { "false" },
                }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(body);
        }));
    }

    // Run the `release` command for the current commit.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .assert();

    // Verify that a release was created for each package.
    commit_pulls_mock.assert_calls(1);
    for mock in server_mocks {
        mock.assert_calls(1);
    }
    let assert = assert.success();

    // Verify the Git repository state remains unchanged, and that all release tags were pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    for tag_name in [
        "v0.3.0",
        "v0.3",
        "v0",
        "example-api-v0.2.0",
        "example-api-v0.2",
        "example-api-v0",
        "example-cli-v0.3.0",
        "example-cli-v0.3",
        "example-cli-v0",
    ] {
        let reference = format!("refs/tags/{tag_name}");
        assert_eq!(
            repo.find_reference(&reference).unwrap().peel_to_commit().unwrap().id(),
            head,
        );
        assert_eq!(
            remote_repo
                .find_reference(&reference)
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id(),
            head,
        );
    }

    // Verify the output of the command as expected.
    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `release` command creates Git tags via the GitHub API, and outputs a list of released packages
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_release_command_tags_via_api(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be checked.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, _remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Enabling commits via the GitHub API (default behaviour);
    // - Enabling the creation of additional major and minor Git tags (default behaviour).
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "github": { "host": server.base_url(), "commit-via-api": true } },
                    "tag-major-and-minor": true, // NB: Explicitly enable this, despite it defaulting to `true`.
                },
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let head = stage_and_commit(&repo, ["."], "chore: release v0.3.0").unwrap();

    // Prepare the packages included in the release pull request.
    let packages = vec![
        PullRequestPackage {
            name: "example-rust-workspace".to_string(),
            version: "0.3.0".parse().unwrap(),
            changelog_md: Some(
                "## [0.3.0] - 2026-09-26\n\n### Features\n\n- Add a `subtract` function and print subtractions"
                    .to_string(),
            ),
        },
        PullRequestPackage {
            name: "example-api".to_string(),
            version: "0.2.0".parse().unwrap(),
            changelog_md: Some("## [0.2.0] - 2026-09-26\n\n### Features\n\n- Add a `subtract` function".to_string()),
        },
        PullRequestPackage {
            name: "example-cli".to_string(),
            version: "0.3.0".parse().unwrap(),
            changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions".to_string()),
        },
    ];
    let expected_releases = [
        // The expected tag names, release title, and whether the package should be marked as the latest release.
        (["v0.3.0", "v0.3", "v0"], "v0.3.0", true),
        (
            ["example-api-v0.2.0", "example-api-v0.2", "example-api-v0"],
            "example-api: v0.2.0",
            false,
        ),
        (
            ["example-cli-v0.3.0", "example-cli-v0.3", "example-cli-v0"],
            "example-cli: v0.3.0",
            false,
        ),
    ];

    // Mock the release pull request associated with the current commit.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main");
        body[0]["body"] = json!(build_pull_request_body(packages.clone()).unwrap());

        when.method(GET)
            .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
        then.status(200).json_body(body);
    });

    // Mock the creation of each annotated tag, its reference, and the package release.
    let mut server_mocks = Vec::new();
    for (i, (pkg, (tag_names, title, make_latest))) in packages.iter().zip(expected_releases).enumerate() {
        // Mock the creation of the annotated tags and their references via the GitHub API.
        for (j, tag_name) in tag_names.into_iter().enumerate() {
            let api_tag_id = format!("{:040x}", i * 3 + j + 1);
            server_mocks.push(server.mock(|when, then| {
                when.method(POST)
                    .path("/repos/octocat/Hello-World/git/tags")
                    .json_body(json!({
                        "tag": tag_name,
                        "message": tag_name,
                        "object": head.to_string(),
                        "type": "commit",
                    }));
                then.status(201).json_body(json!({
                    "node_id": format!("tag-object-{api_tag_id}"),
                    "tag": tag_name,
                    "sha": api_tag_id,
                    "url": format!("https://api.github.com/repos/octocat/Hello-World/git/tags/{api_tag_id}"),
                    "message": tag_name,
                }));
            }));
            server_mocks.push(server.mock(|when, then| {
                when.method(POST)
                    .path("/repos/octocat/Hello-World/git/refs")
                    .json_body(json!({
                        "ref": format!("refs/tags/{tag_name}"),
                        "sha": api_tag_id,
                    }));
                then.status(201).json_body(json!({
                    "ref": format!("refs/tags/{tag_name}"),
                    "node_id": format!("tag-ref-{api_tag_id}"),
                    "url": format!("https://api.github.com/repos/octocat/Hello-World/git/refs/tags/{tag_name}"),
                    "object": {
                        "type": "tag",
                        "sha": api_tag_id,
                        "url": format!("https://api.github.com/repos/octocat/Hello-World/git/tags/{api_tag_id}"),
                    },
                }));
            }));
        }
        // Mock the creation of the release for the package via the GitHub API.
        let tag_name = tag_names[0];
        server_mocks.push(server.mock(|when, then| {
            when.method(GET)
                .path(format!("/repos/octocat/Hello-World/releases/tags/{tag_name}"));
            then.status(404).json_body(json!({"message": "Not Found"}));
        }));
        server_mocks.push(server.mock(|when, then| {
            let mut body: serde_json::Value = serde_json::from_str(include_str!(
                "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ))
            .unwrap();
            body["html_url"] = json!(format!("https://github.com/octocat/Hello-World/releases/{tag_name}"));

            when.method(POST)
                .path("/repos/octocat/Hello-World/releases")
                .json_body(json!({
                    "tag_name": tag_name,
                    "target_commitish": head.to_string(),
                    "name": title,
                    "body": pkg.changelog_md,
                    "make_latest": if make_latest { "true" } else { "false" },
                }));
            then.status(201).json_body(body);
        }));
    }

    // Run the `release` command for the current commit.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .assert()
        .success();

    // Verify that the original branch, commit and clean working directory were preserved.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    // Verify that a Git tag and release was created for each package.
    commit_pulls_mock.assert_calls(1);
    for mock in server_mocks {
        mock.assert_calls(1);
    }
}

/// Tests that the `release` command makes no changes when the release pull request has already been released
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_release_command_noop_when_already_released(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({"git": {"provider": {"github": {"host": server.base_url()}}}}).to_string())
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let head = stage_and_commit(&repo, &["."], "chore: release v0.3.0").unwrap();

    // Create and push an existing release tag, then remove the local tag.
    // NB: The package should be skipped based on the remote tag, even when it does not exist locally.
    let tag_id = git_reflow_api::git::tag(&repo, "v0.3.0", head, false).unwrap();
    git_reflow_api::git::push_tag(&repo, "v0.3.0", false).unwrap();
    repo.tag_delete("v0.3.0").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust-workspace".to_string(),
        version: "0.3.0".parse().unwrap(),
        changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions".to_string()),
    };

    // Mock the release pull request associated with the current commit.
    // NB: We don't expect any releases to be retrieved or created for the already released package.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main");
        body[0]["body"] = json!(build_pull_request_body([pkg]).unwrap());

        when.method(GET)
            .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let get_mock = server.mock(|when, then| {
        when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v0.3.0");
        then.status(200)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ));
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST).path("/repos/octocat/Hello-World/releases");
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ));
    });

    // Run the `release` command and check its JSON output.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .assert()
        .success()
        .stdout("[]\n");

    // Verify the Git repository state remains unchanged, and that the existing remote tag was not changed.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(repo.tag_names(None).unwrap().is_empty());
    assert_eq!(remote_repo.tag_names(None).unwrap().len(), 1);
    assert_eq!(
        remote_repo
            .find_reference("refs/tags/v0.3.0")
            .unwrap()
            .target()
            .unwrap(),
        tag_id,
    );

    // Verify that no releases were retrieved or created for the already released package.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(0);
    create_mock.assert_calls(0);
}

/// Tests that the `release` command with `--dry-run` reports on the activity that would happen without taking action
/// for the [`GitHubProvider`](git_reflow_api::provider::github::GitHubProvider) provider.
#[rstest]
fn test_release_command_with_dry_run(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling commits via the GitHub API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } }
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let head = stage_and_commit(&repo, &["."], "chore: release v0.3.0").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust-workspace".to_string(),
        version: "0.3.0".parse().unwrap(),
        changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions".to_string()),
    };

    // Mock the release pull request associated with the current commit.
    // NB: We don't expect any releases to be retrieved or created during a dry run.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main");
        body[0]["body"] = json!(build_pull_request_body([pkg]).unwrap());

        when.method(GET)
            .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let get_mock = server.mock(|when, then| {
        when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v0.3.0");
        then.status(404)
            .header("content-type", "application/json")
            .body(r#"{"message":"Not Found"}"#);
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST).path("/repos/octocat/Hello-World/releases");
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ));
    });

    // Run the `release` command with `--dry-run` for the current commit.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .arg("--dry-run")
        .assert()
        .success();

    // Verify the Git repository state remains unchanged, and that no tags were created or pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(repo.tag_names(None).unwrap().is_empty());
    assert!(remote_repo.tag_names(None).unwrap().is_empty());

    // Verify that no releases were retrieved or created during the dry run.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(0);
    create_mock.assert_calls(0);

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}
