use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::fixture::{FileWriteStr, PathChild};
use git_reflow_api::git::stage_and_commit;
use git_reflow_api::provider::{PullRequestPackage, build_pull_request_body};
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, add_origin_remote, commit_patch, project_repo};
use git2::Repository;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use rstest::rstest;
use serde_json::json;
use std::fs;
use std::io::Write;

/// Tests that the `plan` command produces the expected output.
/// for the [`PythonStrategy`](git_reflow_api::strategy::python::PythonStrategy) strategy.
#[rstest]
fn test_plan_command(#[with("example-python")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create a Git tag for the current version of the package.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();

    // Create some changes in the package and commit those changes.
    let mod_py_path = project_dir.path().join("src/example/__init__.py");
    writeln!(
        fs::OpenOptions::new().append(true).open(&mod_py_path).unwrap(),
        "\ndef subtract(a: int, b: int):\n    return a - b"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat: add a `subtract` function").unwrap();

    // Run the `plan` command.
    let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap();
    let assert = cmd.current_dir(&project_dir).arg("plan").assert().success();

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `pr` command writes changes, pushes a release branch, and outputs the created pull request
/// for the [`PythonStrategy`](git_reflow_api::strategy::python::PythonStrategy) strategy.
#[rstest]
fn test_pr_command(#[with("example-python")] project_repo: (TempDir, Repository)) {
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

    // Create a Git tag for the current version of the package.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();

    // Create some changes in the packages and commit those changes.
    let mod_py_path = project_dir.path().join("src/example/__init__.py");
    writeln!(
        fs::OpenOptions::new().append(true).open(&mod_py_path).unwrap(),
        "\ndef subtract(a: int, b: int):\n    return a - b"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat: add a `subtract` function").unwrap();
    let head = repo.head().unwrap().target().unwrap();

    // Run the `pr` command for the changed package.
    let assert = Command::cargo_bin(env!("CARGO_PKG_NAME"))
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
        insta::assert_snapshot!("pr_command_diff", commit_patch(&remote_repo, &release_commit));
    });
}

/// Tests that the `release` command creates a Git tag, release, and outputs the released package
/// for the [`PythonStrategy`](git_reflow_api::strategy::python::PythonStrategy) strategy.
#[rstest]
fn test_release_command(#[with("example-python")] project_repo: (TempDir, Repository)) {
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
    let head = stage_and_commit(&repo, &["."], "chore: release v1.1.0").unwrap();

    // Prepare the package included in the release pull request.
    let pkg = PullRequestPackage {
        name: "example-python".to_string(),
        version: "1.1.0".parse().unwrap(),
        changelog_md: Some("## [1.1.0] - 2026-09-26\n\n### Features\n\n- Print subtractions\n".to_string()),
    };

    // Mock the release pull request associated with the current commit and the creation of its release.
    let commit_pulls_mock = server.mock(|when, then| {
        let mut body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/github_commit_pulls.json"
        ))
        .unwrap();
        body[0]["head"]["ref"] = json!("reflow--branches--main--example-python");
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
            "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
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
        .assert();

    // Verify that a release was created for the package.
    commit_pulls_mock.assert_calls(1);
    get_mock.assert_calls(1);
    create_mock.assert_calls(1);
    let assert = assert.success();

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

    // Verify the output of the command as expected.
    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}
