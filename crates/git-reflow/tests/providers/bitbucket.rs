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
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_cloud(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    let server = MockServer::start();
    let list_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/2.0/repositories/axieum/Hello-World/pullrequests")
            .query_param("state", "OPEN")
            .query_param(
                "q",
                concat!(
                    r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                    r#"source.branch.name = "reflow--branches--main" AND destination.branch.name = "main""#,
                ),
            )
            .query_param("pagelen", "1");
        then.status(200).json_body(json!({ "values": [] }));
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/2.0/repositories/axieum/Hello-World/pullrequests")
            .json_body_includes(
                json!({
                    "source": { "branch": { "name": "reflow--branches--main" } },
                    "destination": { "branch": { "name": "main" } },
                })
                .to_string(),
            );
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
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
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_cloud_creates_separate_pull_requests(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    let server = MockServer::start();
    let server_mocks = vec![
        server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .query_param("state", "OPEN")
                .query_param(
                    "q",
                    concat!(
                        r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                        r#"source.branch.name = "reflow--branches--main--example-rust-workspace" AND "#,
                        r#"destination.branch.name = "main""#,
                    ),
                )
                .query_param("pagelen", "1");
            then.status(200).json_body(json!({ "values": [] }));
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .json_body_includes(
                    json!({
                        "source": { "branch": { "name": "reflow--branches--main--example-rust-workspace" } },
                        "destination": { "branch": { "name": "main" } },
                    })
                    .to_string(),
                );
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_pulls_retrieve.json"
                ));
        }),
        server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .query_param("state", "OPEN")
                .query_param(
                    "q",
                    concat!(
                        r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                        r#"source.branch.name = "reflow--branches--main--example-api" AND "#,
                        r#"destination.branch.name = "main""#,
                    ),
                )
                .query_param("pagelen", "1");
            then.status(200).json_body(json!({ "values": [] }));
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .json_body_includes(
                    json!({
                        "source": { "branch": { "name": "reflow--branches--main--example-api" } },
                        "destination": { "branch": { "name": "main" } },
                    })
                    .to_string(),
                );
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_pulls_retrieve.json"
                ));
        }),
        server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .query_param("state", "OPEN")
                .query_param(
                    "q",
                    concat!(
                        r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                        r#"source.branch.name = "reflow--branches--main--example-cli" AND "#,
                        r#"destination.branch.name = "main""#,
                    ),
                )
                .query_param("pagelen", "1");
            then.status(200).json_body(json!({ "values": [] }));
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .json_body_includes(
                    json!({
                        "source": { "branch": { "name": "reflow--branches--main--example-cli" } },
                        "destination": { "branch": { "name": "main" } },
                    })
                    .to_string(),
                );
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_pulls_retrieve.json"
                ));
        }),
    ];

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Enabling separate pull requests for each package.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "bitbucket": { "host": server.base_url() } },
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
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

/// Tests that the `pr` command makes no changes or pull requests when the remote is already up-to-date
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_cloud_noop_when_remote_is_up_to_date(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API for the initial pull request.
    let server = MockServer::start();
    let list_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/2.0/repositories/axieum/Hello-World/pullrequests")
            .query_param("state", "OPEN")
            .query_param(
                "q",
                concat!(
                    r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                    r#"source.branch.name = "reflow--branches--main" AND destination.branch.name = "main""#,
                ),
            )
            .query_param("pagelen", "1");
        then.status(200).json_body(json!({ "values": [] }));
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/2.0/repositories/axieum/Hello-World/pullrequests")
            .json_body_includes(
                json!({
                    "source": { "branch": { "name": "reflow--branches--main" } },
                    "destination": { "branch": { "name": "main" } },
                })
                .to_string(),
            );
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
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
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_cloud_with_dry_run(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    // NB: We don't expect any API calls to be made, so don't add any mocks.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
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

/// Tests that the `pr` command writes changes, pushes a release branch, and outputs the created pull request
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_data_center(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    let server = MockServer::start();
    let list_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
            .query_param("state", "OPEN")
            .query_param("direction", "INCOMING")
            .query_param("at", "refs/heads/main")
            .query_param("limit", "100")
            .query_param("start", "0");
        then.status(200).json_body(json!({ "values": [], "isLastPage": true }));
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
            .json_body_includes(
                json!({
                    "fromRef": {
                        "id": "refs/heads/reflow--branches--main",
                        "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                    },
                    "toRef": {
                        "id": "refs/heads/main",
                        "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                    },
                })
                .to_string(),
            );
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
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
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_data_center_creates_separate_pull_requests(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    let server = MockServer::start();
    let list_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
            .query_param("state", "OPEN")
            .query_param("direction", "INCOMING")
            .query_param("at", "refs/heads/main")
            .query_param("limit", "100")
            .query_param("start", "0");
        then.status(200).json_body(json!({ "values": [], "isLastPage": true }));
    });
    let create_mocks = vec![
        server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .json_body_includes(
                    json!({
                        "fromRef": {
                            "id": "refs/heads/reflow--branches--main--example-rust-workspace",
                            "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                        },
                        "toRef": {
                            "id": "refs/heads/main",
                            "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                        },
                    })
                    .to_string(),
                );
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
                ));
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .json_body_includes(
                    json!({
                        "fromRef": {
                            "id": "refs/heads/reflow--branches--main--example-api",
                            "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                        },
                        "toRef": {
                            "id": "refs/heads/main",
                            "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                        },
                    })
                    .to_string(),
                );
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
                ));
        }),
        server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .json_body_includes(
                    json!({
                        "fromRef": {
                            "id": "refs/heads/reflow--branches--main--example-cli",
                            "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                        },
                        "toRef": {
                            "id": "refs/heads/main",
                            "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                        },
                    })
                    .to_string(),
                );
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
                ));
        }),
    ];

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Enabling separate pull requests for each package.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "bitbucket": { "host": server.base_url() } },
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
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
    list_mock.assert_calls(3);
    for mock in create_mocks {
        mock.assert_calls(1);
    }

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `pr` command makes no changes or pull requests when the remote is already up-to-date
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_data_center_noop_when_remote_is_up_to_date(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API for the initial pull request.
    let server = MockServer::start();
    let list_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
            .query_param("state", "OPEN")
            .query_param("direction", "INCOMING")
            .query_param("at", "refs/heads/main")
            .query_param("limit", "100")
            .query_param("start", "0");
        then.status(200).json_body(json!({ "values": [], "isLastPage": true }));
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
            .json_body_includes(
                json!({
                    "fromRef": {
                        "id": "refs/heads/reflow--branches--main",
                        "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                    },
                    "toRef": {
                        "id": "refs/heads/main",
                        "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                    },
                })
                .to_string(),
            );
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
            ));
    });

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
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
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_pr_command_via_data_center_with_dry_run(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    // NB: We don't expect any API calls to be made, so don't add any mocks.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
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

/// Tests that the `release` command creates Git tags and outputs released packages without provider release URLs
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_cloud(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Disabling tags via the Bitbucket API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "bitbucket": { "host": server.base_url(), "tag-via-api": false } } }
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

    // Mock the release pull request associated with the current commit.
    // NB: Bitbucket does not support provider releases.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["source"]["branch"]["name"] = json!("reflow--branches--main");
        body["values"][0]["destination"]["branch"]["name"] = json!("main");
        body["values"][0]["summary"]["raw"] = json!(build_pull_request_body(packages).unwrap());

        when.method(GET).path(format!(
            "/2.0/repositories/axieum/Hello-World/commit/{head}/pullrequests"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });

    // Run the `release` command for the current commit.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
        .arg("release")
        .assert();

    // Verify that the release pull request was retrieved successfully.
    commit_pulls_mock.assert_calls(1);
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

/// Tests that the `release` command creates Git tags via the Bitbucket Cloud API and outputs released packages
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_cloud_tags_via_api(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be checked.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, _remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Enabling tags via the Bitbucket API (default behaviour);
    // - Enabling the creation of additional major and minor Git tags (default behaviour).
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "bitbucket": { "host": server.base_url(), "tag-via-api": true } },
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
    let expected_tags = [
        // The expected semantic version, minor and major tags for each package.
        "v0.3.0",
        "v0.3",
        "v0",
        "example-api-v0.2.0",
        "example-api-v0.2",
        "example-api-v0",
        "example-cli-v0.3.0",
        "example-cli-v0.3",
        "example-cli-v0",
    ];

    // Mock the release pull request associated with the current commit.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["source"]["branch"]["name"] = json!("reflow--branches--main");
        body["values"][0]["destination"]["branch"]["name"] = json!("main");
        body["values"][0]["summary"]["raw"] = json!(build_pull_request_body(packages).unwrap());

        when.method(GET).path(format!(
            "/2.0/repositories/axieum/Hello-World/commit/{head}/pullrequests"
        ));
        then.status(200).json_body(body);
    });

    // Mock the lookup and creation of each annotated tag via the Bitbucket Cloud API.
    // NB: Bitbucket does not support provider releases.
    let mut server_mocks = Vec::new();
    for tag_name in expected_tags {
        server_mocks.push(server.mock(|when, then| {
            when.method(GET)
                .path(format!("/2.0/repositories/axieum/Hello-World/refs/tags/{tag_name}"));
            then.status(404);
        }));
        server_mocks.push(server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags")
                .json_body(json!({
                    "message": tag_name,
                    "name": tag_name,
                    "target": { "hash": head.to_string() },
                }));
            then.status(201);
        }));
    }

    // Run the `release` command for the current commit.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
        .arg("release")
        .assert()
        .success();

    // Verify that the original branch, commit and clean working directory were preserved.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    // Verify that the release pull request was retrieved and all Git tags were created via the API.
    commit_pulls_mock.assert_calls(1);
    for mock in server_mocks {
        mock.assert_calls(1);
    }
}

/// Tests that the `release` command makes no changes when the release pull request has already been released
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_cloud_noop_when_already_released(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
    // NB: We don't expect any tags to be retrieved or created via the API for the already released package.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["source"]["branch"]["name"] = json!("reflow--branches--main");
        body["values"][0]["destination"]["branch"]["name"] = json!("main");
        body["values"][0]["summary"]["raw"] = json!(build_pull_request_body([pkg]).unwrap());

        when.method(GET).path(format!(
            "/2.0/repositories/axieum/Hello-World/commit/{head}/pullrequests"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let get_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/2.0/repositories/axieum/Hello-World/refs/tags/v0.3.0");
        then.status(200);
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST).path("/2.0/repositories/axieum/Hello-World/refs/tags");
        then.status(201);
    });

    // Run the `release` command and check its JSON output.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
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

    // Verify that no tags were retrieved or created via the API for the already released package.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(0);
    create_mock.assert_calls(0);
}

/// Tests that the `release` command with `--dry-run` reports on the activity that would happen without taking action
/// for the Cloud [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_cloud_with_dry_run(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Cloud API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
    // NB: We don't expect any tags to be retrieved or created via the API during a dry run.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_cloud_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["source"]["branch"]["name"] = json!("reflow--branches--main");
        body["values"][0]["destination"]["branch"]["name"] = json!("main");
        body["values"][0]["summary"]["raw"] = json!(build_pull_request_body([pkg]).unwrap());

        when.method(GET).path(format!(
            "/2.0/repositories/axieum/Hello-World/commit/{head}/pullrequests"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let get_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/2.0/repositories/axieum/Hello-World/refs/tags/v0.3.0");
        then.status(404);
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST).path("/2.0/repositories/axieum/Hello-World/refs/tags");
        then.status(201);
    });

    // Run the `release` command with `--dry-run` for the current commit.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_CLOUD", "1")
        .arg("release")
        .arg("--dry-run")
        .assert()
        .success();

    // Verify the Git repository state remains unchanged, and that no tags were created or pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(repo.tag_names(None).unwrap().is_empty());
    assert!(remote_repo.tag_names(None).unwrap().is_empty());

    // Verify that no tags were retrieved or created via the API during the dry run.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(0);
    create_mock.assert_calls(0);

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `release` command creates Git tags and outputs released packages without provider release URLs
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_data_center(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Disabling tags via the Bitbucket API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "bitbucket": { "host": server.base_url(), "tag-via-api": false } } }
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

    // Mock the release pull request associated with the current commit.
    // NB: Bitbucket does not support provider releases.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["fromRef"]["id"] = json!("refs/heads/reflow--branches--main");
        body["values"][0]["fromRef"]["displayId"] = json!("reflow--branches--main");
        body["values"][0]["toRef"]["id"] = json!("refs/heads/main");
        body["values"][0]["toRef"]["displayId"] = json!("main");
        body["values"][0]["description"] = json!(build_pull_request_body(packages).unwrap());

        when.method(GET).path(format!(
            "/rest/api/latest/projects/axieum/repos/Hello-World/commits/{head}/pull-requests"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });

    // Run the `release` command for the current commit.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
        .arg("release")
        .assert();

    // Verify that the release pull request was retrieved successfully.
    commit_pulls_mock.assert_calls(1);
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

/// Tests that the `release` command creates Git tags via the Bitbucket Data Center API and outputs released packages
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_data_center_tags_via_api(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release tags can be checked.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, _remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Enabling tags via the Bitbucket API (default behaviour);
    // - Enabling the creation of additional major and minor Git tags (default behaviour).
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "bitbucket": { "host": server.base_url(), "tag-via-api": true } },
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
    let expected_tags = [
        // The expected semantic version, minor and major tags for each package.
        "v0.3.0",
        "v0.3",
        "v0",
        "example-api-v0.2.0",
        "example-api-v0.2",
        "example-api-v0",
        "example-cli-v0.3.0",
        "example-cli-v0.3",
        "example-cli-v0",
    ];

    // Mock the release pull request associated with the current commit.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["fromRef"]["id"] = json!("refs/heads/reflow--branches--main");
        body["values"][0]["fromRef"]["displayId"] = json!("reflow--branches--main");
        body["values"][0]["toRef"]["id"] = json!("refs/heads/main");
        body["values"][0]["toRef"]["displayId"] = json!("main");
        body["values"][0]["description"] = json!(build_pull_request_body(packages).unwrap());

        when.method(GET).path(format!(
            "/rest/api/latest/projects/axieum/repos/Hello-World/commits/{head}/pull-requests"
        ));
        then.status(200).json_body(body);
    });

    // Mock the creation of each annotated tag via the Bitbucket Data Center API.
    // NB: Bitbucket does not support provider releases.
    let mut server_mocks = Vec::new();
    for tag_name in expected_tags {
        server_mocks.push(server.mock(|when, then| {
            when.method(POST)
                .path("/rest/git/latest/projects/axieum/repos/Hello-World/tags")
                .json_body(json!({
                    "force": true,
                    "message": tag_name,
                    "name": tag_name,
                    "startPoint": head.to_string(),
                    "type": "ANNOTATED",
                }));
            then.status(201);
        }));
    }

    // Run the `release` command for the current commit.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
        .arg("release")
        .assert()
        .success();

    // Verify that the original branch, commit and clean working directory were preserved.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    // Verify that the release pull request was retrieved and all Git tags were created via the API.
    commit_pulls_mock.assert_calls(1);
    for mock in server_mocks {
        mock.assert_calls(1);
    }
}

/// Tests that the `release` command makes no changes when the release pull request has already been released
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_data_center_noop_when_already_released(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL.
    project_dir
        .child(".reflow.json")
        .write_str(&json!({ "git": { "provider": { "bitbucket": { "host": server.base_url() } } } }).to_string())
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
    // NB: We don't expect any tags to be created via the API for the already released package.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["fromRef"]["id"] = json!("refs/heads/reflow--branches--main");
        body["values"][0]["fromRef"]["displayId"] = json!("reflow--branches--main");
        body["values"][0]["toRef"]["id"] = json!("refs/heads/main");
        body["values"][0]["toRef"]["displayId"] = json!("main");
        body["values"][0]["description"] = json!(build_pull_request_body([pkg]).unwrap());

        when.method(GET).path(format!(
            "/rest/api/latest/projects/axieum/repos/Hello-World/commits/{head}/pull-requests"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/rest/git/latest/projects/axieum/repos/Hello-World/tags");
        then.status(201);
    });

    // Run the `release` command and check its JSON output.
    Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
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

    // Verify that no tags were created via the API for the already released package.
    commit_pulls_mock.assert_calls(1);
    create_mock.assert_calls(0);
}

/// Tests that the `release` command with `--dry-run` reports on the activity that would happen without taking action
/// for the Data Center [`BitbucketProvider`](git_reflow_api::provider::bitbucket::BitbucketProvider) provider.
#[rstest]
fn test_release_command_via_data_center_with_dry_run(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "axieum", "Hello-World");

    // Set up a mock server to simulate the Bitbucket Data Center API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the Bitbucket host to the mock server's base URL;
    // - Disabling tags via the Bitbucket API.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "bitbucket": { "host": server.base_url(), "tag-via-api": false } } }
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
    // NB: We don't expect any tags to be created via the API during a dry run.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/bitbucket_data_center_commit_pulls.json"
        ))
        .unwrap();
        body["values"][0]["fromRef"]["id"] = json!("refs/heads/reflow--branches--main");
        body["values"][0]["fromRef"]["displayId"] = json!("reflow--branches--main");
        body["values"][0]["toRef"]["id"] = json!("refs/heads/main");
        body["values"][0]["toRef"]["displayId"] = json!("main");
        body["values"][0]["description"] = json!(build_pull_request_body([pkg]).unwrap());

        when.method(GET).path(format!(
            "/rest/api/latest/projects/axieum/repos/Hello-World/commits/{head}/pull-requests"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/rest/git/latest/projects/axieum/repos/Hello-World/tags");
        then.status(201);
    });

    // Run the `release` command with `--dry-run` for the current commit.
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("BITBUCKET_TOKEN", "test-token")
        .env_remove("BITBUCKET_CLOUD")
        .arg("release")
        .arg("--dry-run")
        .assert()
        .success();

    // Verify the Git repository state remains unchanged, and that no tags were created or pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert!(repo.tag_names(None).unwrap().is_empty());
    assert!(remote_repo.tag_names(None).unwrap().is_empty());

    // Verify that no tags were created via the API during the dry run.
    commit_pulls_mock.assert_calls(1);
    create_mock.assert_calls(0);

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}
