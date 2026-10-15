use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_api::provider::{PullRequestPackage, build_pull_request_body};
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, add_origin_remote, project_repo};
use git2::Repository;
use httpmock::prelude::*;
use rstest::rstest;
use serde_json::json;

/// Tests that the `release` command creates a Git tag, release, and outputs the released package.
#[rstest]
fn test_release_command_creates_a_release(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
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
    let head = git_reflow_api::git::commit(&repo, ["."], "chore: release v1.1.0").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust".to_string(),
        version: "1.1.0".parse().unwrap(),
        changelog_md: Some("## [1.1.0] - 2026-09-26\n\n### Features\n\n- Print subtractions\n".to_string()),
    };

    // Mock the release pull request associated with the current commit and the creation of its release.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main--example-rust");
        body[0]["body"] = json!(build_pull_request_body([pkg.clone()]).unwrap());

        when.method(GET)
            .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let get_mock = server.mock(|when, then| {
        when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v1.1.0");
        then.status(404)
            .header("content-type", "application/json")
            .body(r#"{"message":"Not Found"}"#);
    });
    let create_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
        ))
        .unwrap();
        body["html_url"] = json!("https://github.com/octocat/Hello-World/releases/v1.1.0");

        when.method(POST)
            .path("/repos/octocat/Hello-World/releases")
            .json_body(json!({
                "tag_name": "v1.1.0",
                "target_commitish": head.to_string(),
                "name": "v1.1.0",
                "body": format!("{}\n", pkg.changelog_md.as_ref().unwrap()),
                "make_latest": "true",
            }));
        then.status(201)
            .header("content-type", "application/json")
            .json_body(body);
    });

    // Run the `release` command for the current commit.
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .assert()
        .success();

    // Verify the Git repository state remains unchanged, and that the release tags were pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    for tag_name in ["v1.1.0", "v1.1", "v1"] {
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

    // Verify that a release was created for the package.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(1);
    create_mock.assert_calls(1);

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `release` command creates Git tags, releases, and outputs a list of released packages.
#[rstest]
fn test_release_command_creates_multiple_releases(
    #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
) {
    // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
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
    let head = git_reflow_api::git::commit(&repo, ["."], "chore: release v0.3.0").unwrap();

    // Prepare the packages included in the release pull request.
    let packages = vec![
        PullRequestPackage {
            name: "example-rust-workspace".to_string(),
            version: "0.3.0".parse().unwrap(),
            changelog_md: Some(
                "## [0.3.0] - 2026-09-26\n\n### Features\n\n- Add a `subtract` function and print subtractions\n"
                    .to_string(),
            ),
        },
        PullRequestPackage {
            name: "example-api".to_string(),
            version: "0.2.0".parse().unwrap(),
            changelog_md: Some("## [0.2.0] - 2026-09-26\n\n### Features\n\n- Add a `subtract` function\n".to_string()),
        },
        PullRequestPackage {
            name: "example-cli".to_string(),
            version: "0.3.0".parse().unwrap(),
            changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions\n".to_string()),
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
            "../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
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
                "../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
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
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .assert()
        .success();

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

    // Verify that a release was created for each package.
    commit_pulls_mock.assert_calls(1);
    for mock in server_mocks {
        mock.assert_calls(1);
    }

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `release` command allows skipping the creation of additional major and minor Git tags.
#[rstest]
fn test_release_command_can_skip_major_and_minor_tags(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

    // Set up a mock server to simulate the GitHub API.
    let server = MockServer::start();

    // Configure `git-reflow` by:
    // - Setting the GitHub host to the mock server's base URL;
    // - Disabling the creation of additional major and minor Git tags.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "github": { "host": server.base_url() } },
                    "tag-major-and-minor": false, // NB: By default, this is `true`, so disable it for this test.
                },
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let head = git_reflow_api::git::commit(&repo, ["."], "chore: release v1.2.3").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust".to_string(),
        version: "1.2.3".parse().unwrap(),
        changelog_md: Some("## [1.2.3] - 2026-10-15\n\n### Features\n\n- Print subtractions\n".to_string()),
    };

    // Mock the release pull request associated with the current commit and the creation of its release.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main--example-rust");
        body[0]["body"] = json!(build_pull_request_body([pkg.clone()]).unwrap());

        when.method(GET)
            .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(body);
    });
    let get_mock = server.mock(|when, then| {
        when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v1.2.3");
        then.status(404)
            .header("content-type", "application/json")
            .body(r#"{"message":"Not Found"}"#);
    });
    let create_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
        ))
        .unwrap();
        body["html_url"] = json!("https://github.com/octocat/Hello-World/releases/v1.2.3");

        when.method(POST)
            .path("/repos/octocat/Hello-World/releases")
            .json_body(json!({
                "tag_name": "v1.2.3",
                "target_commitish": head.to_string(),
                "name": "v1.2.3",
                "body": format!("{}\n", pkg.changelog_md.as_ref().unwrap()),
                "make_latest": "true",
            }));
        then.status(201)
            .header("content-type", "application/json")
            .json_body(body);
    });

    // Run the `release` command for the current commit.
    Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_TOKEN", "test-token")
        .arg("release")
        .assert()
        .success();

    // Verify the Git repository state remains unchanged, and that only the full release tag was pushed up.
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
    assert_eq!(
        repo.find_reference("refs/tags/v1.2.3")
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id(),
        head,
    );
    assert_eq!(
        remote_repo
            .find_reference("refs/tags/v1.2.3")
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id(),
        head,
    );
    for tag_name in ["v1.2", "v1"] {
        assert!(repo.find_reference(&format!("refs/tags/{tag_name}")).is_err());
        assert!(remote_repo.find_reference(&format!("refs/tags/{tag_name}")).is_err());
    }

    // Verify that a release was created for the package.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(1);
    create_mock.assert_calls(1);
}

/// Tests that the `release` command makes no changes when the release pull request has already been released.
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
    let head = git_reflow_api::git::commit(&repo, ["."], "chore: release v0.3.0").unwrap();

    // Create and push an existing release tag, then remove the local tag.
    // NB: The package should be skipped based on the remote tag, even when it does not exist locally.
    let tag_id = git_reflow_api::git::tag(&repo, "v0.3.0", head, false).unwrap();
    git_reflow_api::git::push_tag(&repo, "v0.3.0", false).unwrap();
    repo.tag_delete("v0.3.0").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust-workspace".to_string(),
        version: "0.3.0".parse().unwrap(),
        changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions\n".to_string()),
    };

    // Mock the release pull request associated with the current commit.
    // NB: We don't expect any releases to be retrieved or created for the already released package.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
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
                "../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ));
    });
    let create_mock = server.mock(|when, then| {
        when.method(POST).path("/repos/octocat/Hello-World/releases");
        then.status(201)
            .header("content-type", "application/json")
            .body(include_str!(
                "../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ));
    });

    // Run the `release` command and check its JSON output.
    Command::cargo_bin(env!("CARGO_PKG_NAME"))
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

/// Tests that the `release` command with `--dry-run` reports on the activity that would happen without taking action.
#[rstest]
fn test_release_command_with_dry_run(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    // Create a Git repository and add an `origin` remote so that we ensure nothing is pushed to it.
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
    let head = git_reflow_api::git::commit(&repo, ["."], "chore: release v0.3.0").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust-workspace".to_string(),
        version: "0.3.0".parse().unwrap(),
        changelog_md: Some("## [0.3.0] - 2026-09-26\n\n### Features\n\n- Print subtractions\n".to_string()),
    };

    // Mock the release pull request associated with the current commit.
    // NB: We don't expect any releases to be retrieved or created during a dry run.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
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
                "../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
            ));
    });

    // Run the `release` command with `--dry-run` for the current commit.
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
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
