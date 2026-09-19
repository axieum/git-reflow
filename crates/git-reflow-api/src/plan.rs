use crate::git_cliff::{get_version_from_git_cliff_context, render_changelog_markdown, run_git_cliff};
use crate::settings::AppConfig;
use crate::settings::pkg::PackageConfig;
use semver::Version;
use serde_json::Value;
use tracing::debug;

/// A planned package release.
pub struct PackageRelease<'pkg> {
    /// The package configuration.
    pkg: &'pkg PackageConfig,
    /// The current version of the package, if it exists.
    current_version: Option<Version>,
    /// The next (target) version of the package.
    next_version: Version,
    /// The rendered changelog markdown for the package.
    changelog_md: String,
    /// The `git-cliff` context for the package.
    context: Value,
}

/// Plans the releases for all in-scope packages.
///
/// # Arguments
///
/// * `config` - The app configuration.
///
/// # Returns
///
/// A result of all planned package release information.
pub async fn plan_releases(config: &AppConfig) -> anyhow::Result<Vec<PackageRelease<'_>>> {
    let mut planned_releases = Vec::new();
    for pkg in &config.packages {
        // Run `git-cliff` in the package directory.
        let context = match run_git_cliff(&pkg.dir, None)? {
            // If the context contains a bump type, it indicates that there are changes to be released.
            Some(ctx) if !ctx["bump_type"].is_null() => {
                debug!("{} has unreleased changes ✏️", pkg.name());
                ctx
            }
            // Otherwise, there are no changes to be released.
            _ => {
                debug!("{} is up-to-date ✅", pkg.name());
                continue;
            }
        };

        // Parse the version and render the changelog markdown for this package.
        let (current_version, next_version) = get_version_from_git_cliff_context(&context)?;
        let changelog_md = render_changelog_markdown(&context, None)?;

        // Append the planned release information for this package.
        planned_releases.push(PackageRelease {
            pkg,
            current_version,
            next_version,
            changelog_md,
            context,
        });
    }
    Ok(planned_releases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use git_reflow_fixtures::project_repo;
    use git2::Repository;
    use rstest::rstest;
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    /// Tests that package releases are planned correctly.
    #[rstest]
    #[tokio::test]
    async fn test_plan_releases(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
        let (project_dir, repo) = project_repo;

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let config = crate::settings::load(None).unwrap();

        // Commit the current files to the `main` branch.
        repo.set_head("refs/heads/main").unwrap();
        crate::git::commit(&repo, &["."], "chore: initial commit").unwrap();

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
        crate::git::commit(&repo, &["."], "feat(api): add a `subtract` function").unwrap();

        // Call `plan_releases` and assert that the planned releases are as expected.
        // NB: We expect two planned releases: one for the root package and one for the `example-api` package.
        let planned_releases = plan_releases(&config).await.unwrap();
        assert_eq!(planned_releases.len(), 2);
        assert_eq!(planned_releases[0].pkg.dir, Path::new("."));
        assert_eq!(planned_releases[1].pkg.dir, Path::new("crates/example-api"));
        assert_eq!(planned_releases[0].next_version, Version::parse("1.1.0").unwrap());
        assert_eq!(planned_releases[1].next_version, Version::parse("1.1.0").unwrap());
        assert!(!planned_releases[0].changelog_md.is_empty());
        assert!(!planned_releases[1].changelog_md.is_empty());
        assert!(planned_releases[0].context.is_object());
        assert!(planned_releases[1].context.is_object());
    }
}
