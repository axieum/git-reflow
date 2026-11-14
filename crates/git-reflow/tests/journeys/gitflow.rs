use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::fixture::{FileWriteStr, PathChild};
use git_reflow_api::git::stage_and_commit;
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, project_repo};
use git2::Repository;
use rstest::*;
use serde_json::json;
use std::fs;
use std::io::Write;

/// Tests the [Gitflow workflow](https://www.atlassian.com/git/tutorials/comparing-workflows/gitflow-workflow).
#[rstest]
fn test_gitflow_workflow(#[with("example-rust")] project_repo: (TempDir, Repository)) {
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

    // todo: Create a `develop` branch from `main` and switch to it.

    // todo: Create a `feature` branch from `develop` and switch to it.

    // todo: Make some changes to the code and commit them to the `feature` branch.

    // todo: Merge the `feature` branch into the `develop` branch.

    // todo: Run `git reflow plan` and verify that the release plan includes the changes from the `feature` branch and is marked as a pre-release.
    // todo: Run `git reflow pr` and merge the pull request it created for the `develop` branch.
    // todo: Run `git reflow release` and verify that the pre-release Git tags were created.

    // todo: Create a `release` branch from `main` and switch to it.

    // todo: Run `git reflow plan` and verify that no changes are included in the release plan since the `release` branch is based on `main`.

    // todo: Merge the `develop` branch into the `release` branch.

    // todo: Run `git reflow plan` and verify that the release plan includes the changes from the `develop` branch and is marked as a pre-release.
    // todo: Run `git reflow pr` and merge the pull request it created for the `release` branch.
    // todo: Run `git reflow release` and verify that the pre-release Git tags were created.

    // todo: Merge the `release` branch into the `main` branch and switch to it.

    // todo: Run `git reflow plan` and verify that the release plan includes the changes from the `release` branch and is marked as a stable release.
    // todo: Run `git reflow pr` and merge the pull request it created for the `main` branch.
    // todo: Run `git reflow release` and verify that the stable Git tags were created.

    todo!();
}
