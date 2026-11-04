use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_api::git::stage_and_commit;
use git_reflow_api::provider::{PullRequestPackage, build_pull_request_body};
use git_reflow_fixtures::{add_origin_remote, project_repo};
use git2::Repository;
use httpmock::prelude::*;
use rstest::rstest;
use serde_json::json;

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
    // - Disabling commits via the GitHub API;
    // - Disabling the creation of additional major and minor Git tags.
    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": {
                    "provider": { "github": { "host": server.base_url(), "commit-via-api": false } },
                    "tag-major-and-minor": false, // NB: By default, this is `true`, so disable it for this test.
                },
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let head = stage_and_commit(&repo, ["."], "chore: release v1.2.3").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-rust".to_string(),
        version: "1.2.3".parse().unwrap(),
        changelog_md: Some("## [1.2.3] - 2026-10-15\n\n### Features\n\n- Print subtractions".to_string()),
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
                "body": pkg.changelog_md.as_ref().unwrap(),
                "make_latest": "true",
            }));
        then.status(201)
            .header("content-type", "application/json")
            .json_body(body);
    });

    // Run the `release` command for the current commit.
    Command::cargo_bin("git-reflow")
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
