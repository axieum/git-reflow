use crate::git::{
    BranchGuard, commit, create_or_reset_branch, ensure_clean_working_directory, get_origin_remote, push_branch,
    sanitize_branch_name,
};
use crate::git_cliff::{CommandRunner, apply_git_cliff_context};
use crate::plan::PackageRelease;
use crate::provider::{BaseGitProvider, PullRequest};
use crate::settings::AppConfig;
use crate::strategy::BaseStrategy;
use anyhow::{Context, anyhow, ensure};
use std::path::PathBuf;
use tracing::{debug, warn};

/// Applies the release plan to the package manifest files and commits them.
///
/// # Arguments
///
/// - `config` - The app configuration.
/// - `plan` - The release plan to apply.
/// - `dry_run` - If true, do not actually write the changes or commit them.
/// - `cliff_runner` - An optional command runner for executing `git-cliff` commands.
///
/// # Returns
///
/// A result containing the pull requests created or updated by the release plan.
pub async fn apply_release_plan(
    config: &AppConfig,
    plan: &[PackageRelease],
    dry_run: bool,
    cliff_runner: Option<&dyn CommandRunner>,
) -> anyhow::Result<Vec<PullRequest>> {
    // Short-circuit if there are no releases to apply.
    if plan.is_empty() {
        warn!("no releases in the plan to apply");
        return Ok(vec![]);
    }

    // Open the Git repository in the current working directory.
    let repo = git2::Repository::discover(".").context("not a git repository")?;
    let remote = get_origin_remote(&repo)?.context("git remote `origin` is not configured")?;

    // Ensure the working directory is clean before applying the release plan.
    ensure_clean_working_directory(&repo)?;

    // Create a Git branch guard to safely roll back if an error occurs.
    let mut guard = BranchGuard::from_head(&repo)?;

    // Apply all releases together, or one per branch when separate pull requests are enabled.
    let group_size = if config.git.separate_pull_requests {
        1
    } else {
        plan.len()
    };
    let mut pull_requests = Vec::with_capacity(group_size);
    for (i, releases) in plan.chunks(group_size).enumerate() {
        // Trace the release group being applied.
        if config.git.separate_pull_requests {
            debug!(
                "[{}/{}] applying package release `{}`",
                i + 1,
                plan.len(),
                releases[0].package_name,
            );
        } else {
            debug!("applying {} combined package releases", plan.len());
        }

        // Prepare the branch name, pull request title & description for the release group.
        let branch_name = if config.git.separate_pull_requests {
            // For separate pull requests, append the package name to the branch name to avoid conflicts.
            format!(
                "{}{}--{}",
                config.git.release_branch_prefix,
                guard.original_branch,
                sanitize_branch_name(&releases[0].package_name)?,
            )
        } else {
            // For a combined pull request, the target branch name is sufficient enough.
            format!("{}{}", config.git.release_branch_prefix, guard.original_branch)
        };
        let pr_title = &releases[0].commit_message;
        let pr_body = if config.git.separate_pull_requests {
            // Use the changelog Markdown for the single release as the pull request body.
            releases[0].changelog_md.clone()
        } else {
            // Use a summary of all changelog Markdowns for the multiple releases as the pull request body.
            releases
                .iter()
                .map(|release| {
                    format!(
                        r#"<details>
                        <summary>{}: v{}</summary>

                        {}
                        </details>
                        "#,
                        release.package_name, release.next_version, release.changelog_md
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        // Create or reset the target branch for the release group.
        if !dry_run {
            debug!("create branch `{}`", branch_name);
            create_or_reset_branch(&repo, &branch_name, guard.original_commit_id)?;
        } else {
            debug!("create branch `{}` (dry run)", branch_name);
        }

        // Write the changes to the package files.
        let mut changed_files = Vec::new();
        for (j, release) in releases.iter().enumerate() {
            // Trace the package release being applied.
            if releases.len() > 1 {
                debug!(
                    "[{}/{}] writing changes to package `{}`",
                    j + 1,
                    releases.len(),
                    release.package_name,
                );
            } else {
                debug!("writing changes to package `{}`", release.package_name);
            }

            // Write the changes to the package files, and ensure changes were actually made.
            let files = write_package_release(config, release, dry_run, cliff_runner).await?;
            ensure!(
                !files.is_empty(),
                "no changes were made for package `{}`",
                release.package_name
            );

            changed_files.extend(files);
        }

        // Commit the changes to the branch.
        if !dry_run {
            debug!("commit changes for branch `{}`", branch_name);
            let commit_id = commit(&repo, &changed_files, pr_title)?;
            debug!("✅ created commit: {} ({})", &pr_title, commit_id);
        } else {
            debug!("✅ created commit `{}` (dry run)", &pr_title);
        }

        // Push the branch to the remote Git repository.
        if !dry_run {
            debug!("push branch `{}`", branch_name);
            push_branch(&repo, &branch_name, true)?;
        } else {
            debug!("push branch `{}` (dry run)", branch_name);
        }

        // Create or update the pull request for the branch.
        if !dry_run {
            debug!(
                "creating pull request: `{}` -> `{}`",
                branch_name, guard.original_branch
            );
            let pr = config
                .git
                .provider
                .upsert_pull_request(
                    &remote.owner,
                    &remote.repo,
                    &branch_name,
                    &guard.original_branch,
                    pr_title,
                    &pr_body,
                )
                .await?;
            debug!(
                "🔀 {} pull request #{}: {}",
                if pr.is_new { "created" } else { "updated" },
                pr.number,
                pr.url,
            );
            pull_requests.push(pr);
        } else {
            debug!("skipping pull request for branch `{}` (dry run)", branch_name);
        }
    }

    // Reset the Git repository to the original branch and commit.
    debug!(
        "restoring to original branch `{}` and commit `{}`",
        guard.original_branch, guard.original_commit_id
    );
    repo.set_head(&format!("refs/heads/{}", guard.original_branch))?;
    repo.reset(
        repo.find_commit(guard.original_commit_id)?.as_object(),
        git2::ResetType::Hard,
        None,
    )?;

    // Disarm the Git branch guard, since the release plan was applied successfully and return.
    guard.disarm();
    Ok(pull_requests)
}

/// Writes the changes to the package manifest files.
///
/// # Arguments
///
/// - `config` - The app configuration.
/// - `release` - The package release information.
/// - `dry_run` - If true, do not actually write the changes or commit them.
/// - `cliff_runner` - An optional command runner for executing `git-cliff` commands.
///
/// # Returns
///
/// A result containing a list of the changed file paths.
pub async fn write_package_release(
    config: &AppConfig,
    release: &PackageRelease,
    dry_run: bool,
    cliff_runner: Option<&dyn CommandRunner>,
) -> anyhow::Result<Vec<PathBuf>> {
    // Look up the package configuration.
    let package = config
        .get_package(&release.package_name)
        .ok_or_else(|| anyhow!("package `{}` is not configured", release.package_name))?;

    // Track the changed files for this package release, so that they can be committed later.
    let mut changed_files: Vec<PathBuf> = Vec::new();

    // Write the version to the package manifest file/s.
    let manifest_paths = package
        .strategy()
        .write_version(&release.next_version, package, dry_run)?;
    changed_files.extend(manifest_paths);

    // Write the changelog Markdown to the changelog file, if it is configured.
    // NB: We use `git-cliff` to write the changelog, so that it can be formatted and templated consistently.
    //     This means the `context` from the release plan is actually required, suggesting `plan --show-context`.
    if let Some(changelog_path) = package.changelog_path() {
        if !dry_run {
            debug!("write changelog to `{}`", changelog_path.display());
            apply_git_cliff_context(&changelog_path, &release.context, cliff_runner)?;
        } else {
            debug!("write changelog to `{}` (dry run)", changelog_path.display());
        }
        changed_files.push(changelog_path);
    }

    Ok(changed_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_cliff::tests::MockGitCliffRunner;
    use crate::provider::{GitProvider, github::GitHubProvider, github::tests::mock_octocrab};
    use assert_fs::TempDir;
    use git_reflow_fixtures::{add_origin_remote, project_repo};
    use git2::Repository;
    use httpmock::prelude::*;
    use rstest::rstest;
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::Output;

    /// Tests that a grouped release creates one pull request containing all package releases.
    #[rstest]
    #[tokio::test]
    async fn test_apply_release_plan(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
        // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
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
                .body(include_str!("../tests/fixtures/github_pulls_retrieve.json"));
        });
        config.git.provider = GitProvider::GitHub(provider);

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        crate::git::commit(&repo, &["."], "chore: initial commit").unwrap();

        // Set up a mock `git-cliff` runner to simulate writing the changelog files.
        let mut cliff_runner = MockGitCliffRunner::new();
        cliff_runner.expect_run().returning(move |_, _| {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0), // success.
                stdout: vec![],
                stderr: vec![],
            })
        });

        // Prepare a release plan.
        let plan = vec![
            PackageRelease {
                package_name: "example-rust-workspace".to_string(),
                current_version: Some(semver::Version::parse("0.2.0").unwrap()),
                next_version: semver::Version::parse("0.3.0").unwrap(),
                commit_message: "chore: release v0.3.0".to_string(),
                changelog_md: r#"## [0.3.0] - 2026-09-26

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                - *(cli)* Print subtractions
                "#
                .to_string(),
                context: json!({}), // NB: `git-cliff` is not actually invoked, so an empty context will suffice.
            },
            PackageRelease {
                package_name: "example-api".to_string(),
                current_version: Some(semver::Version::parse("0.1.0").unwrap()),
                next_version: semver::Version::parse("0.2.0").unwrap(),
                commit_message: "chore(example-api): release v0.2.0".to_string(),
                changelog_md: r#"## [0.2.0] - 2026-09-26

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                "#
                .to_string(),
                context: json!({}),
            },
            PackageRelease {
                package_name: "example-cli".to_string(),
                current_version: Some(semver::Version::parse("0.2.0").unwrap()),
                next_version: semver::Version::parse("0.3.0").unwrap(),
                commit_message: "chore(example-cli): release v0.3.0".to_string(),
                changelog_md: r#"## [0.3.0] - 2026-09-26

                ### 🚀 Features

                - *(cli)* Print subtractions
                "#
                .to_string(),
                context: json!({}),
            },
        ];

        let head = repo.head().unwrap().target().unwrap();

        // Call `apply_release_plan` with the release plan.
        let prs = apply_release_plan(&config, &plan, false, Some(&cliff_runner))
            .await
            .unwrap();

        // Verify the Git repository state was restored, and that the release branch was pushed up.
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
        assert!(remote_repo.find_reference("refs/heads/reflow--branches--main").is_ok());

        // Verify that a pull request was created for the release branch.
        // NB: We expect one combined pull request, since `separate_pull_requests` is false by default.
        list_mock.assert_async().await;
        create_mock.assert_calls_async(1).await;
        assert_eq!(
            prs,
            vec![PullRequest {
                number: 1347,
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                is_new: true,
            }]
        );
    }

    /// Tests that multiple pull requests for each package release are created when configured to do so.
    #[rstest]
    #[tokio::test]
    async fn test_apply_release_plan_with_separate_prs(
        #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
    ) {
        // Create a Git repository and add an `origin` remote so that release branches can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();
        config.git.separate_pull_requests = true; // NB: Here, we enable separate pull requests.

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
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
                    .body(include_str!("../tests/fixtures/github_pulls_retrieve.json"));
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
                    .body(include_str!("../tests/fixtures/github_pulls_retrieve.json"));
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
                    .body_includes("example-cli");
                then.status(201)
                    .header("content-type", "application/json")
                    .body(include_str!("../tests/fixtures/github_pulls_retrieve.json"));
            }),
        ];
        config.git.provider = GitProvider::GitHub(provider);

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        crate::git::commit(&repo, &["."], "chore: initial commit").unwrap();

        // Set up a mock `git-cliff` runner to simulate writing the changelog files.
        let mut cliff_runner = MockGitCliffRunner::new();
        cliff_runner.expect_run().returning(move |_, _| {
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0), // success.
                stdout: vec![],
                stderr: vec![],
            })
        });

        // Prepare a release plan.
        let plan = vec![
            PackageRelease {
                package_name: "example-rust-workspace".to_string(),
                current_version: Some(semver::Version::parse("0.2.0").unwrap()),
                next_version: semver::Version::parse("0.3.0").unwrap(),
                commit_message: "chore: release v0.3.0".to_string(),
                changelog_md: r#"## [0.3.0] - 2026-09-26

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                - *(cli)* Print subtractions
                "#
                .to_string(),
                context: json!({}), // NB: `git-cliff` is not actually invoked, so an empty context will suffice.
            },
            PackageRelease {
                package_name: "example-api".to_string(),
                current_version: Some(semver::Version::parse("0.1.0").unwrap()),
                next_version: semver::Version::parse("0.2.0").unwrap(),
                commit_message: "chore(example-api): release v0.2.0".to_string(),
                changelog_md: r#"## [0.2.0] - 2026-09-26

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                "#
                .to_string(),
                context: json!({}),
            },
            PackageRelease {
                package_name: "example-cli".to_string(),
                current_version: Some(semver::Version::parse("0.2.0").unwrap()),
                next_version: semver::Version::parse("0.3.0").unwrap(),
                commit_message: "chore(example-cli): release v0.3.0".to_string(),
                changelog_md: r#"## [0.3.0] - 2026-09-26

                ### 🚀 Features

                - *(cli)* Print subtractions
                "#
                .to_string(),
                context: json!({}),
            },
        ];

        let head = repo.head().unwrap().target().unwrap();

        // Call `apply_release_plan` with the release plan.
        let prs = apply_release_plan(&config, &plan, false, Some(&cliff_runner))
            .await
            .unwrap();

        // Verify the Git repository state was restored, and that the release branches were pushed up.
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
        for branch in [
            "reflow--branches--main--example-rust-workspace",
            "reflow--branches--main--example-api",
            "reflow--branches--main--example-cli",
        ] {
            assert!(remote_repo.find_reference(&format!("refs/heads/{}", branch)).is_ok());
        }

        // Verify that multiple pull requests were created for each release branch.
        // NB: We expect three pull requests, since `separate_pull_requests` was enabled.
        for mock in server_mocks {
            mock.assert_async().await;
        }
        assert_eq!(
            prs,
            vec![
                PullRequest {
                    number: 1347,
                    url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                    head: "reflow--branches--main--example-rust-workspace".to_string(),
                    base: "main".to_string(),
                    is_new: true,
                },
                PullRequest {
                    number: 1347, // NB: The mock server returns the same PR number for all three requests.
                    url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                    head: "reflow--branches--main--example-api".to_string(),
                    base: "main".to_string(),
                    is_new: true,
                },
                PullRequest {
                    number: 1347, // NB: The mock server returns the same PR number for all three requests.
                    url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                    head: "reflow--branches--main--example-cli".to_string(),
                    base: "main".to_string(),
                    is_new: true,
                },
            ]
        );
    }

    /// Tests that an empty release plan does not create any pull requests.
    #[tokio::test]
    async fn test_apply_release_plan_with_empty_plan() {
        let prs = apply_release_plan(&AppConfig::default(), &[], false, None)
            .await
            .unwrap();
        assert!(prs.is_empty());
    }
}
