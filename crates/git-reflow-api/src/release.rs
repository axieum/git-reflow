use crate::git::{get_origin_remote, push_tag, tag, tag_exists_on_remote};
use crate::provider::{BaseGitProvider, parse_pull_request_body};
use crate::settings::AppConfig;
use anyhow::{Context, anyhow, ensure};
use semver::Version;
use tracing::{debug, trace};

/// A package that has been released.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleasedPackage {
    /// The name of the package.
    pub package_name: String,
    /// The version of the package.
    pub version: Version,
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
                    pkg.version
                );
                ensure!(
                    pkg.changelog_md.is_some(),
                    "package `{} v{}` is missing a changelog in the pull request body",
                    pkg.name,
                    pkg.version
                );
                let changelog_md = pkg.changelog_md.as_ref().unwrap();
                trace!("↳ {changelog_md}");

                // Check if a Git tag already exists for this package name and version.
                let tag_name = build_git_tag_name(config, &pkg.name, &pkg.version)?;
                if tag_exists_on_remote(&repo, "origin", &tag_name)? {
                    debug!("tag `{}` already exists, skipping package", tag_name);
                    continue;
                }

                // Create a Git tag for the package release.
                if !dry_run {
                    debug!("🏷️ create tag `{tag_name}`");
                    tag(&repo, &tag_name, commit, true)?;
                } else {
                    debug!("🏷️ create tag `{tag_name}` (dry run)");
                }

                // Push the Git tag to the remote repository.
                if !dry_run {
                    debug!("push tag `{tag_name}` to remote `{}`", remote.url);
                    push_tag(&repo, &tag_name, true)?;
                } else {
                    debug!("push tag `{tag_name}` to remote `{}` (dry run)", remote.url);
                }

                // If the package is configured as `tag-only`, we are done.
                if config.get_package(&pkg.name).is_some_and(|p| p.tag_only) {
                    debug!("package `{}` is configured with `tag-only`, skipping release", pkg.name);
                    releases.push(ReleasedPackage {
                        package_name: pkg.name.clone(),
                        version: pkg.version.clone(),
                        tag_name: Some(tag_name),
                        url: None,
                        changelog_md: changelog_md.to_owned(),
                    });
                    continue;
                }

                // Create a release on the Git provider for the package release.
                let release_title = build_release_title(config, &pkg.name, &pkg.version)?;
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
/// * `config` - The app configuration.
/// * `package_name` - The name of the package being released.
/// * `version` - The version of the package being released.
///
/// # Returns
///
/// A result containing the Git tag name, e.g. `v1.2.3` or `example-api-v1.2.3`.
pub fn build_git_tag_name(config: &AppConfig, package_name: &str, version: &Version) -> anyhow::Result<String> {
    let include_name_in_tag = config
        .get_package(package_name)
        .ok_or_else(|| anyhow!("package `{package_name}` is not configured"))?
        .include_name_in_tag;
    Ok(if include_name_in_tag {
        format!("{}-v{}", package_name, version)
    } else {
        format!("v{}", version)
    })
}

/// Builds the release title for a given package release.
///
/// # Arguments
///
/// * `config` - The app configuration.
/// * `package_name` - The name of the package being released.
/// * `version` - The version of the package being released.
///
/// # Returns
///
/// A result containing the release title, e.g. `v1.2.3` or `example-api: v1.2.3`.
pub fn build_release_title(config: &AppConfig, package_name: &str, version: &Version) -> anyhow::Result<String> {
    let include_name_in_title = config
        .get_package(package_name)
        .ok_or_else(|| anyhow!("package `{package_name}` is not configured"))?
        .include_name_in_tag;
    Ok(if include_name_in_title {
        format!("{}: v{}", package_name, version)
    } else {
        format!("v{}", version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::pkg::PackageConfig;
    use semver::Version;

    /// Tests that a Git tag name is built correctly for a package that includes its name in the tag.
    #[test]
    fn test_build_git_tag_name() {
        let config = AppConfig {
            packages: vec![PackageConfig {
                name: Some("example-package".to_string()),
                include_name_in_tag: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            build_git_tag_name(&config, "example-package", &Version::parse("1.2.0").unwrap()).unwrap(),
            "example-package-v1.2.0",
        );
    }

    /// Tests that the built Git tag name does not include the package name when `include_name_in_tag` is false.
    #[test]
    fn test_build_git_tag_name_without_include_name_in_tag() {
        let config = AppConfig {
            packages: vec![PackageConfig {
                name: Some("example-package".to_string()),
                include_name_in_tag: false,
                ..Default::default()
            }],
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
        let config = AppConfig {
            packages: vec![PackageConfig {
                name: Some("example-package".to_string()),
                include_name_in_tag: true,
                ..Default::default()
            }],
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
        let config = AppConfig {
            packages: vec![PackageConfig {
                name: Some("example-package".to_string()),
                include_name_in_tag: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            build_release_title(&config, "example-package", &Version::parse("1.2.0").unwrap()).unwrap(),
            "v1.2.0",
        );
    }
}
