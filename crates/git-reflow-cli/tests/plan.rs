use assert_cmd::Command;
use assert_fs::TempDir;
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, project_repo};
use git2::Repository;
use rstest::*;
use std::fs;
use std::io::Write;

/// Tests that the `plan` command produces the expected output.
#[rstest]
fn test_plan_command(#[with("example-python-uv-workspace")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();
    repo.tag_lightweight("example-api-v1.0.0", commit.as_object(), false)
        .unwrap();
    repo.tag_lightweight("example-cli-v1.0.0", commit.as_object(), false)
        .unwrap();

    // Create some changes in the packages and commit those changes.
    let mod_py_path = project_dir
        .path()
        .join("packages/example-api/src/example/api/__init__.py");
    writeln!(
        fs::OpenOptions::new().append(true).open(&mod_py_path).unwrap(),
        "\ndef subtract(a: int, b: int):\n    return a - b"
    )
    .unwrap();
    git_reflow_api::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

    // Run the `plan` command.
    let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap();
    let assert = cmd.current_dir(&project_dir).arg("plan").assert().success();

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `plan` command allows scoping to the given package names.
#[rstest]
fn test_plan_command_with_scope(#[with("example-python-uv-workspace")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();
    repo.tag_lightweight("example-api-v1.0.0", commit.as_object(), false)
        .unwrap();
    repo.tag_lightweight("example-cli-v1.0.0", commit.as_object(), false)
        .unwrap();

    // Create some changes in the packages and commit those changes.
    let mod_py_path = project_dir
        .path()
        .join("packages/example-api/src/example/api/__init__.py");
    writeln!(
        fs::OpenOptions::new().append(true).open(&mod_py_path).unwrap(),
        "\ndef subtract(a: int, b: int):\n    return a - b"
    )
    .unwrap();
    git_reflow_api::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

    // Run the `plan` command, scoping it to the `example-api` package only.
    let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap();
    let assert = cmd
        .current_dir(&project_dir)
        .arg("plan")
        .arg("example-api")
        .assert()
        .success();

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `plan` command allows outputting the `git-cliff` context.
#[rstest]
fn test_plan_command_with_show_context(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    git_reflow_api::git::commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    repo.tag_lightweight("v1.0.0", commit.as_object(), false).unwrap();
    repo.tag_lightweight("example-api-v1.0.0", commit.as_object(), false)
        .unwrap();
    repo.tag_lightweight("example-cli-v1.0.0", commit.as_object(), false)
        .unwrap();

    // Create some changes in the packages and commit those changes.
    let lib_rs_path = project_dir.path().join("crates/example-api/src/lib.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    git_reflow_api::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

    // Run the `plan` command, showing the `git-cliff` context.
    let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap();
    let assert = cmd
        .current_dir(&project_dir)
        .arg("plan")
        .arg("--show-context")
        .assert()
        .success();

    // Parse the output as JSON and assert that the `context` field is present for each package release.
    let json: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(json[0]["context"].is_object());
    assert!(json[0]["context"]["commits"].is_array());
    assert!(json[1]["context"].is_object());
    assert!(json[1]["context"]["commits"].is_array());
}
