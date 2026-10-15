use crate::git::{get_origin_remote, push_tag, tag, tag_exists_on_remote};
use crate::provider::{BaseGitProvider, parse_pull_request_body};
use crate::settings::{AppConfig, pkg::PackageConfig};
use anyhow::{Context, anyhow, ensure};
use semver::Version;
use std::fmt::Display;
use tracing::{debug, trace};

/// A package that has been released.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleasedPackage {
    /// The name of the package.
    pub package_name: String,
    /// The version of the package.
    pub version: Version,
    /// The major version of the package.
    pub major: u64,
    /// The minor version of the package.
    pub minor: u64,
    /// The patch version of the package.
    pub patch: u64,
    /// The pre-release version of the package, if any.
    pub pre: Option<String>,
    /// The build metadata in the version of the package, if any.
    pub build: Option<String>,
    /// The Git tag name for the package release, if created.
    pub tag_name: Option<String>,
    /// The URL of the release on the Git provider, if created.
    pub url: Option<String>,
    /// The rendered changelog Markdown for the package.
    pub changelog_md: String,
}

/// Tags and releases the most recently merged release pull request.
///
/// # Arguments
///
/// * `config` - The app configuration.
/// * `dry_run` - If true, do not actually release anything.
///
/// # Returns
///
/// A result containing the packages that were released, if any.
pub async fn create_releases(config: &AppConfig, dry_run: bool) -> anyhow::Result<Vec<ReleasedPackage>> {
    // Open the Git repository in the current working directory.
    let repo = git2::Repository::discover(".").context("not a git repository")?;
    let remote = get_origin_remote(&repo)?.context("git remote `origin` is not configured")?;
    let commit = repo
        .head()
        .context("failed to get repository HEAD")?
        .peel_to_commit()
        .context("failed to resolve current commit from HEAD")?
        .id();
    let commit_sha = commit.to_string();

    // Find an associated release pull request for the current commit.
    match config
        .git
        .provider
        .associated_pull_requests(&remote.owner, &remote.repo, &commit_sha)
        .await?
        .into_iter()
        .find(|pr| pr.head.starts_with(&config.git.release_branch_prefix))
    {
        // There is a release pull request associated with the current commit.
        Some(mut pr) => {
            // Trace the discovered release pull request number and commit SHA.
            debug!("found release pull request #{} for commit `{}`", pr.number, commit_sha);

            // Determine the packages that were released in the pull request.
            // NB: This will return an error if there are no packages found in the pull request body.
            pr.packages = parse_pull_request_body(pr.body.as_ref())?;

            // Release each package in the pull request.
            let mut releases: Vec<ReleasedPackage> = Vec::with_capacity(pr.packages.len());
            for (i, pkg) in pr.packages.iter().enumerate() {
                // Trace the package name and version that is being released.
                debug!(
                    "[{}/{}] releasing package `{} v{}`",
                    i + 1,
                    pr.packages.len(),
                    pkg.name,
                    pkg.version,
                );
                let pkg_config = config
                    .get_package(&pkg.name)
                    .ok_or_else(|| anyhow!("package `{}` is not configured", pkg.name))?;

                // Ensure that the package has a changelog in the pull request body.
                ensure!(
                    pkg.changelog_md.is_some(),
                    "package `{} v{}` is missing a changelog in the pull request body",
                    pkg.name,
                    pkg.version,
                );
                let changelog_md = pkg.changelog_md.as_ref().unwrap();
                trace!("↳ {changelog_md}");

                // Check if a Git tag already exists for this package name and version.
                let tag_name = build_git_tag_name(pkg_config, &pkg.name, &pkg.version)?;
                if tag_exists_on_remote(&repo, "origin", &tag_name)? {
                    debug!("tag `{}` already exists, skipping package", tag_name);
                    continue;
                }

                // Prepare any additional Git tags for the package release, if configured.
                let extra_tags = if config.git.tag_major_and_minor && pkg.version.pre.is_empty() {
                    vec![
                        build_git_tag_name(
                            pkg_config,
                            &pkg.name,
                            format_args!("{}.{}", pkg.version.major, pkg.version.minor),
                        )?,
                        build_git_tag_name(pkg_config, &pkg.name, pkg.version.major)?,
                    ]
                } else {
                    vec![]
                };

                // Create and push Git tag(s) for the package release.
                for name in std::iter::once(&tag_name).chain(extra_tags.iter()) {
                    if !dry_run {
                        // Create a Git tag for the package release.
                        debug!("🏷️ create tag `{name}`");
                        tag(&repo, name, commit, true)?;
                        // Push the Git tag to the remote repository.
                        debug!("push tag `{name}` to remote `{}`", remote.url);
                        push_tag(&repo, name, true)?;
                    } else {
                        debug!("🏷️ create tag `{name}` (dry run)");
                        debug!("push tag `{name}` to remote `{}` (dry run)", remote.url);
                    }
                }

                // If the package is configured as `tag-only`, we are done.
                if pkg_config.tag_only {
                    debug!("package `{}` is configured with `tag-only`, skipping release", pkg.name);
                    releases.push(ReleasedPackage {
                        package_name: pkg.name.clone(),
                        version: pkg.version.clone(),
                        major: pkg.version.major,
                        minor: pkg.version.minor,
                        patch: pkg.version.patch,
                        pre: (!pkg.version.pre.is_empty()).then(|| pkg.version.pre.to_string()),
                        build: (!pkg.version.build.is_empty()).then(|| pkg.version.build.to_string()),
                        tag_name: Some(tag_name),
                        url: None,
                        changelog_md: changelog_md.to_owned(),
                    });
                    continue;
                }

                // Create a release on the Git provider for the package release.
                let release_title = build_release_title(pkg_config, &pkg.name, &pkg.version)?;
                let release_url = if !dry_run {
                    debug!("creating {} release: {release_title}", config.git.provider);
                    let url = config
                        .git
                        .provider
                        .upsert_release(
                            &remote.owner,
                            &remote.repo,
                            &tag_name,
                            &commit_sha,
                            &release_title,
                            changelog_md,
                            pkg_config.make_latest,
                        )
                        .await?;
                    debug!("🚀 created {} release at {url}", config.git.provider);
                    Some(url)
                } else {
                    debug!("creating {} release: {release_title} (dry run)", config.git.provider);
                    debug!("🚀 created {} release (dry run)", config.git.provider);
                    None
                };

                // Emit the released package information.
                releases.push(ReleasedPackage {
                    package_name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    major: pkg.version.major,
                    minor: pkg.version.minor,
                    patch: pkg.version.patch,
                    pre: (!pkg.version.pre.is_empty()).then(|| pkg.version.pre.to_string()),
                    build: (!pkg.version.build.is_empty()).then(|| pkg.version.build.to_string()),
                    tag_name: Some(tag_name),
                    url: release_url,
                    changelog_md: changelog_md.to_owned(),
                });
            }

            Ok(releases)
        }
        // Short-circuit if no release pull request is found for the current commit.
        None => {
            debug!("nothing to release for commit {}", commit_sha);
            Ok(vec![])
        }
    }
}

/// Builds the Git tag name for a given package release.
///
/// # Arguments
///
/// * `pkg_config` - The package configuration.
/// * `package_name` - The name of the package being released.
/// * `version` - The version of the package being released.
///
/// # Returns
///
/// A result containing the Git tag name, e.g. `v1.2.3` or `example-api-v1.2.3`.
pub fn build_git_tag_name(
    pkg_config: &PackageConfig,
    package_name: &str,
    version: impl Display,
) -> anyhow::Result<String> {
    Ok(if pkg_config.include_name_in_tag {
        format!("{}-v{}", package_name, version)
    } else {
        format!("v{}", version)
    })
}

/// Builds the release title for a given package release.
///
/// # Arguments
///
/// * `pkg_config` - The package configuration.
/// * `package_name` - The name of the package being released.
/// * `version` - The version of the package being released.
///
/// # Returns
///
/// A result containing the release title, e.g. `v1.2.3` or `example-api: v1.2.3`.
pub fn build_release_title(
    pkg_config: &PackageConfig,
    package_name: &str,
    version: &Version,
) -> anyhow::Result<String> {
    Ok(if pkg_config.include_name_in_tag {
        format!("{}: v{}", package_name, version)
    } else {
        format!("v{}", version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::commit;
    use crate::provider::{
        GitProvider, PullRequestPackage, build_pull_request_body, github::GitHubProvider, github::tests::mock_octocrab,
    };
    use crate::settings::pkg::PackageConfig;
    use assert_fs::TempDir;
    use git_reflow_fixtures::{add_origin_remote, project_repo};
    use git2::Repository;
    use httpmock::prelude::*;
    use indoc::indoc;
    use rstest::rstest;
    use semver::Version;
    use serde_json::json;

    /// Tests that a Git tag and release is created for a single package in a separated release pull request.
    #[rstest]
    #[tokio::test]
    async fn test_create_releases(#[with("example-rust")] project_repo: (TempDir, Repository)) {
        // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();
        config.git.separate_pull_requests = true;

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        let head = commit(&repo, &["."], "chore: release v1.1.0").unwrap();

        // Prepare the package included in the release pull request.
        let pkg = PullRequestPackage {
            name: "example-rust".to_string(),
            version: Version::parse("1.1.0").unwrap(),
            changelog_md: Some(
                indoc! {r#"
                    ## [1.1.0] - 2026-09-26

                    ### Features

                    - Print subtractions
                    "#}
                .to_string(),
            ),
        };

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
        let commit_pulls_mock = server.mock(|when, then| {
            let mut body: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/github_commit_pulls.json")).unwrap();
            body[0]["head"]["ref"] = json!("reflow--branches--main--example-rust");
            body[0]["body"] = json!(indoc! {r#"
                ---
                packages:
                - name: example-rust
                  version: 1.1.0
                ---

                ## [1.1.0] - 2026-09-26

                ### Features

                - Print subtractions
                "#});

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
            let mut body: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/github_releases_retrieve.json")).unwrap();
            body["html_url"] = json!("https://github.com/octocat/Hello-World/releases/v1.1.0");

            when.method(POST)
                .path("/repos/octocat/Hello-World/releases")
                .json_body(json!({
                    "tag_name": "v1.1.0",
                    "target_commitish": head.to_string(),
                    "name": "v1.1.0",
                    "body": pkg.changelog_md,
                    "make_latest": "true",
                }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(body);
        });
        config.git.provider = GitProvider::GitHub(provider);

        // Call `create_releases` for the current commit.
        let releases = create_releases(&config, false).await.unwrap();

        // Verify that the release tag was created locally and pushed to the remote at the current commit.
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
        assert_eq!(
            repo.find_reference("refs/tags/v1.1.0")
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id(),
            head,
        );
        assert_eq!(
            remote_repo
                .find_reference("refs/tags/v1.1.0")
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id(),
            head,
        );

        // Verify that a release was created for the package with its version, tag, URL and changelog.
        commit_pulls_mock.assert_async().await;
        get_mock.assert_async().await;
        create_mock.assert_async().await;
        assert_eq!(releases.len(), 1);
        let release = &releases[0];
        assert_eq!(release.package_name, pkg.name);
        assert_eq!(release.version, pkg.version);
        assert_eq!(release.major, pkg.version.major);
        assert_eq!(release.minor, pkg.version.minor);
        assert_eq!(release.patch, pkg.version.patch);
        assert!(release.pre.is_none());
        assert!(release.build.is_none());
        assert_eq!(release.tag_name.as_deref(), Some("v1.1.0"));
        assert_eq!(
            release.url.as_deref(),
            Some("https://github.com/octocat/Hello-World/releases/v1.1.0"),
        );
        assert_eq!(release.changelog_md, pkg.changelog_md.as_ref().unwrap().as_str());
    }

    /// Tests that all Git tags and releases are created for the workspace packages in a release pull request.
    #[rstest]
    #[tokio::test]
    async fn test_create_releases_with_workspace(
        #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
    ) {
        // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        let head = commit(&repo, &["."], "chore: release v0.3.0").unwrap();

        // Prepare the packages included in the release pull request.
        let packages = vec![
            PullRequestPackage {
                name: "example-rust-workspace".to_string(),
                version: Version::parse("0.3.0").unwrap(),
                changelog_md: Some(
                    indoc! {r#"
                        ## [0.3.0] - 2026-09-26

                        ### Features

                        - Add a `subtract` function and print subtractions
                        "#}
                    .to_string(),
                ),
            },
            PullRequestPackage {
                name: "example-api".to_string(),
                version: Version::parse("0.2.0").unwrap(),
                changelog_md: Some(
                    indoc! {r#"
                        ## [0.2.0] - 2026-09-26

                        ### Features

                        - Add a `subtract` function
                        "#}
                    .to_string(),
                ),
            },
            PullRequestPackage {
                name: "example-cli".to_string(),
                version: Version::parse("0.3.0").unwrap(),
                changelog_md: Some(
                    indoc! {r#"
                        ## [0.3.0] - 2026-09-26

                        ### Features

                        - Print subtractions
                        "#}
                    .to_string(),
                ),
            },
        ];
        let expected_releases = [
            // The expected tag name, release title, and whether the package should be marked as the latest release.
            ("v0.3.0", "v0.3.0", true),
            ("example-api-v0.2.0", "example-api: v0.2.0", false),
            ("example-cli-v0.3.0", "example-cli: v0.3.0", false),
        ];

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
        let commit_pulls_mock = server.mock(|when, then| {
            let mut body: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/github_commit_pulls.json")).unwrap();
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
                let mut body: serde_json::Value =
                    serde_json::from_str(include_str!("../tests/fixtures/github_releases_retrieve.json")).unwrap();
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
        config.git.provider = GitProvider::GitHub(provider);

        // Call `create_releases` for the current commit.
        let releases = create_releases(&config, false).await.unwrap();

        // Verify that all release tags were created locally and pushed to the remote at the current commit.
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
        for (tag_name, _, _) in expected_releases {
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

        // Verify that a release was created for each package with its version, tag, URL and changelog.
        commit_pulls_mock.assert_async().await;
        for mock in server_mocks {
            mock.assert_async().await;
        }
        assert_eq!(releases.len(), packages.len());
        for ((release, pkg), (tag_name, _, _)) in releases.iter().zip(&packages).zip(expected_releases) {
            assert_eq!(release.package_name, pkg.name);
            assert_eq!(release.version, pkg.version);
            assert_eq!(release.major, pkg.version.major);
            assert_eq!(release.minor, pkg.version.minor);
            assert_eq!(release.patch, pkg.version.patch);
            assert!(release.pre.is_none());
            assert!(release.build.is_none());
            assert_eq!(release.tag_name.as_deref(), Some(tag_name));
            assert_eq!(
                release.url.as_deref(),
                Some(format!("https://github.com/octocat/Hello-World/releases/{tag_name}").as_str()),
            );
            assert_eq!(release.changelog_md, pkg.changelog_md.as_ref().unwrap().as_str());
        }
    }

    /// Tests that major and minor Git tags are created if the `git.tag-major-and-minor` setting is configured.
    #[rstest]
    #[tokio::test]
    async fn test_create_releases_with_tag_major_and_minor(
        #[with("example-rust")] project_repo: (TempDir, Repository),
        #[values(true, false)] tag_major_and_minor: bool,
    ) {
        // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();
        config.git.separate_pull_requests = true;
        config.git.tag_major_and_minor = tag_major_and_minor;

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        let head = commit(&repo, &["."], "chore: release v1.1.0").unwrap();

        // Prepare the package included in the release pull request.
        let pkg = PullRequestPackage {
            name: "example-rust".to_string(),
            version: Version::parse("1.2.3").unwrap(),
            changelog_md: Some(
                indoc! {r#"
                    ## [1.2.3] - 2026-10-15

                    ### Features

                    - Print subtractions
                    "#}
                .to_string(),
            ),
        };

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
        server.mock(|when, then| {
            let mut body: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/github_commit_pulls.json")).unwrap();
            body[0]["head"]["ref"] = json!("reflow--branches--main--example-rust");
            body[0]["body"] = json!(indoc! {r#"
                ---
                packages:
                - name: example-rust
                  version: 1.2.3
                ---

                ## [1.2.3] - 2026-10-15

                ### Features

                - Print subtractions
                "#});

            when.method(GET)
                .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
            then.status(200)
                .header("content-type", "application/json")
                .json_body(body);
        });
        server.mock(|when, then| {
            when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v1.1.0");
            then.status(404)
                .header("content-type", "application/json")
                .body(r#"{"message":"Not Found"}"#);
        });
        server.mock(|when, then| {
            let mut body: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/github_releases_retrieve.json")).unwrap();
            body["html_url"] = json!("https://github.com/octocat/Hello-World/releases/v1.2.3");

            when.method(POST)
                .path("/repos/octocat/Hello-World/releases")
                .json_body(json!({
                    "tag_name": "v1.2.3",
                    "target_commitish": head.to_string(),
                    "name": "v1.2.3",
                    "body": pkg.changelog_md,
                    "make_latest": "true",
                }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(body);
        });
        config.git.provider = GitProvider::GitHub(provider);

        // Call `create_releases` for the current commit.
        create_releases(&config, false).await.unwrap();

        // Verify that release tag was created locally and pushed to the remote at the current commit.
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
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

        // Verify that the major and minor tags were created locally and pushed to the remote, if configured.
        for tag_name in ["v1.2", "v1"] {
            if tag_major_and_minor {
                assert_eq!(
                    repo.find_reference(&format!("refs/tags/{}", tag_name))
                        .unwrap()
                        .peel_to_commit()
                        .unwrap()
                        .id(),
                    head,
                );
                assert_eq!(
                    remote_repo
                        .find_reference(&format!("refs/tags/{}", tag_name))
                        .unwrap()
                        .peel_to_commit()
                        .unwrap()
                        .id(),
                    head,
                );
            } else {
                assert!(repo.find_reference(&format!("refs/tags/{}", tag_name)).is_err());
                assert!(remote_repo.find_reference(&format!("refs/tags/{}", tag_name)).is_err());
            }
        }
    }

    /// Tests that no releases are created for a release pull request that has already been released.
    #[rstest]
    #[tokio::test]
    async fn test_create_releases_when_already_released(
        #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
    ) {
        // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        let head = commit(&repo, &["."], "chore: release v0.3.0").unwrap();

        // Create and push an existing release tag, then remove the local tag.
        // NB: The package should be skipped based on the remote tag, even when it does not exist locally.
        let tag_id = tag(&repo, "v0.3.0", head, false).unwrap();
        push_tag(&repo, "v0.3.0", false).unwrap();
        repo.tag_delete("v0.3.0").unwrap();

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
        let commit_pulls_mock = server.mock(|when, then| {
            let mut body: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/github_commit_pulls.json")).unwrap();
            body[0]["head"]["ref"] = json!("reflow--branches--main");
            body[0]["body"] = json!(indoc! {r#"
                ---
                packages:
                - name: example-rust-workspace
                  version: 0.3.0
                ---

                ## [0.3.0] - 2026-09-26

                ### Features

                - Print subtractions
                "#});

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
                .body(include_str!("../tests/fixtures/github_releases_retrieve.json"));
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST).path("/repos/octocat/Hello-World/releases");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!("../tests/fixtures/github_releases_retrieve.json"));
        });
        config.git.provider = GitProvider::GitHub(provider);

        // Call `create_releases` for the already released commit.
        let releases = create_releases(&config, false).await;

        // Verify that the existing remote tag was not changed and no local tag was created.
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
        assert!(repo.find_reference("refs/tags/v0.3.0").is_err());
        assert_eq!(
            remote_repo
                .find_reference("refs/tags/v0.3.0")
                .unwrap()
                .target()
                .unwrap(),
            tag_id,
        );

        // Verify that no releases were retrieved or created for the already released package.
        commit_pulls_mock.assert_async().await;
        get_mock.assert_calls_async(0).await;
        create_mock.assert_calls_async(0).await;
        assert!(releases.unwrap().is_empty());
    }

    /// Tests that no releases are created when no release pull request is associated with the current commit.
    #[rstest]
    #[tokio::test]
    async fn test_create_releases_when_no_release_pull_request(
        #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
    ) {
        // Create a Git repository and add an `origin` remote so that release tags can be pushed to it.
        let (project_dir, repo) = project_repo;
        let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let mut config = crate::settings::load(None).unwrap();

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        let head = commit(&repo, &["."], "feat: print subtractions").unwrap();

        // Set up a mock server to simulate the GitHub API.
        let provider = GitHubProvider::default();
        let server = mock_octocrab(&provider).await;
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path(format!("/repos/octocat/Hello-World/commits/{head}/pulls"));
            then.status(200).header("content-type", "application/json").body("[]");
        });
        let release_mock = server.mock(|when, then| {
            when.path_includes("/repos/octocat/Hello-World/releases");
            then.status(500);
        });
        config.git.provider = GitProvider::GitHub(provider);

        // Call `create_releases` for a commit without an associated release pull request.
        let releases = create_releases(&config, false).await;

        // Verify that no tags or releases were created and the repository was not changed.
        commit_pulls_mock.assert_async().await;
        release_mock.assert_calls_async(0).await;
        assert!(releases.unwrap().is_empty());
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert!(repo.statuses(None).unwrap().is_empty());
        assert!(repo.tag_names(None).unwrap().is_empty());
        assert!(remote_repo.tag_names(None).unwrap().is_empty());
    }

    /// Tests that a Git tag name is built correctly for a package that includes its name in the tag.
    #[test]
    fn test_build_git_tag_name() {
        let config = PackageConfig {
            name: Some("example-package".to_string()),
            include_name_in_tag: true,
            ..Default::default()
        };
        assert_eq!(
            build_git_tag_name(&config, "example-package", &Version::parse("1.2.0").unwrap()).unwrap(),
            "example-package-v1.2.0",
        );
    }

    /// Tests that a Git tag name is built correctly for a manually provided version string, e.g. `1.2`.
    #[test]
    fn test_build_git_tag_name_with_display_version() {
        let config = PackageConfig {
            name: Some("example-package".to_string()),
            include_name_in_tag: true,
            ..Default::default()
        };
        assert_eq!(
            build_git_tag_name(&config, "example-package", "1").unwrap(),
            "example-package-v1"
        );
        assert_eq!(
            build_git_tag_name(&config, "example-package", "1.2").unwrap(),
            "example-package-v1.2"
        );
    }

    /// Tests that the built Git tag name does not include the package name when `include_name_in_tag` is false.
    #[test]
    fn test_build_git_tag_name_without_include_name_in_tag() {
        let config = PackageConfig {
            name: Some("example-package".to_string()),
            include_name_in_tag: false,
            ..Default::default()
        };
        assert_eq!(
            build_git_tag_name(&config, "example-package", &Version::parse("1.2.0").unwrap()).unwrap(),
            "v1.2.0",
        );
    }

    /// Tests that a release title is built correctly for a package that includes its name in the tag.
    #[test]
    fn test_build_release_title() {
        let config = PackageConfig {
            name: Some("example-package".to_string()),
            include_name_in_tag: true,
            ..Default::default()
        };
        assert_eq!(
            build_release_title(&config, "example-package", &Version::parse("1.2.0").unwrap()).unwrap(),
            "example-package: v1.2.0",
        );
    }

    /// Tests that the built release title does not include the package name when `include_name_in_tag` is false.
    #[test]
    fn test_build_release_title_without_include_name_in_tag() {
        let config = PackageConfig {
            name: Some("example-package".to_string()),
            include_name_in_tag: false,
            ..Default::default()
        };
        assert_eq!(
            build_release_title(&config, "example-package", &Version::parse("1.2.0").unwrap()).unwrap(),
            "v1.2.0",
        );
    }
}
