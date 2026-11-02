use assert_cmd::Command;
use assert_fs::TempDir;
use git_reflow_api::git::stage_and_commit;
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, project_repo};
use git2::Repository;
use rstest::*;
use std::fs;
use std::io::Write;

/// Tests that the `plan` command allows scoping to the given package names.
#[rstest]
fn test_plan_command_with_scope(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

    // Run the `plan` command, scoping it to the `example-api` package only.
    let mut cmd = Command::cargo_bin("git-reflow").unwrap();
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

/// Tests that the `plan` command handles multiple tags correctly, e.g. `v1`, `v1.1`, `v1.1.0`, `example-api-v1`, etc.
#[rstest]
fn test_plan_command_with_multiple_tags(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let commit = stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

    // Create Git tags for the current versions of the packages.
    let commit = repo.find_commit(commit).unwrap();
    for tag in [
        "v1",
        "v1.0",
        "v1.0.0",
        "example-api-v1",
        "example-api-v1.0",
        "example-api-v1.0.0",
        "example-cli-v1",
        "example-cli-v1.0",
        "example-cli-v1.0.0",
    ] {
        repo.tag_lightweight(tag, commit.as_object(), false).unwrap();
    }

    // Create some changes in the packages and commit those changes.
    let main_rs_path = project_dir.path().join("crates/example-cli/src/main.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&main_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}\n"
    )
    .unwrap();
    let commit = stage_and_commit(&repo, &["."], "feat(cli): add a `subtract` function").unwrap();

    // Create Git tags for the new versions of the changed packages.
    // NB: The root package and `example-cli` package have a new feature, but `example-api` remains unchanged.
    let commit = repo.find_commit(commit).unwrap();
    for tag in [
        "v1",
        "v1.1",
        "v1.1.0",
        "example-cli-v1",
        "example-cli-v1.1",
        "example-cli-v1.1.0",
    ] {
        repo.tag_lightweight(tag, commit.as_object(), true).unwrap();
    }

    // Create another change in the packages and commit those changes.
    let lib_rs_path = project_dir.path().join("crates/example-api/src/lib.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&lib_rs_path).unwrap(),
        "\npub fn multiply(a: i32, b: i32) -> i32 {{ a * b }}"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat(api): add a `multiply` function").unwrap();

    writeln!(
        fs::OpenOptions::new().append(true).open(&main_rs_path).unwrap(),
        "\npub fn multiply(a: i32, b: i32) -> i32 {{ example_api::multiply(5, 9) }}"
    )
    .unwrap();
    stage_and_commit(&repo, &["."], "feat(cli): consume the `multiply` function").unwrap();

    // Run the `plan` command.
    let mut cmd = Command::cargo_bin("git-reflow").unwrap();
    let assert = cmd.current_dir(&project_dir).arg("plan").assert().success();

    // Verify that the expected versions were planned for.
    // NB: We expect `example-rust-workspace@v1.2.0`, `example-api@v1.1.0`, and `example-cli@v1.2.0`.
    let json = serde_json::from_slice::<serde_json::Value>(&assert.get_output().stdout).unwrap();
    let plan = json.as_array().unwrap();
    assert_eq!(plan.len(), 3);
    assert_eq!(plan[0]["package-name"], "example-rust-workspace");
    assert_eq!(plan[0]["current-version"], "1.1.0");
    assert_eq!(plan[0]["next-version"], "1.2.0");
    assert_eq!(plan[1]["package-name"], "example-api");
    assert_eq!(plan[1]["current-version"], "1.0.0");
    assert_eq!(plan[1]["next-version"], "1.1.0");
    assert_eq!(plan[2]["package-name"], "example-cli");
    assert_eq!(plan[2]["current-version"], "1.1.0");
    assert_eq!(plan[2]["next-version"], "1.2.0");

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}

/// Tests that the `plan` command allows showing the `git-cliff` context.
#[rstest]
fn test_plan_command_with_show_context(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();

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
    stage_and_commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

    // Run the `plan` command, showing the `git-cliff` context.
    let mut cmd = Command::cargo_bin("git-reflow").unwrap();
    let assert = cmd
        .current_dir(&project_dir)
        .arg("plan")
        .arg("--show-context")
        .assert()
        .success();

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(String::from_utf8_lossy(&assert.get_output().stdout));
    });
}
