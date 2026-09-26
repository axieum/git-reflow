use crate::git_cliff::{get_version_from_git_cliff_context, render_changelog_markdown, run_git_cliff};
use crate::settings::AppConfig;
use anyhow::Context;
use handlebars::Handlebars;
use semver::Version;
use serde_json::{Value, json};
use tracing::debug;

/// A planned package release.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PackageRelease {
    /// The package configuration.
    pub package_name: String,
    /// The current version of the package, if it exists.
    pub current_version: Option<Version>,
    /// The next (target) version of the package.
    pub next_version: Version,
    /// The commit message for the package release.
    pub commit_message: String,
    /// The rendered changelog markdown for the package.
    pub changelog_md: String,
    /// The `git-cliff` context for the package.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub context: Value,
}

/// Plans the releases for all in-scope packages.
///
/// # Arguments
///
/// * `config` - The app configuration.
/// * `package_names` - The package names to plan releases for (leave empty for all).
/// * `target_branch` - The target branch for the release.
///
/// # Returns
///
/// A result of all planned package release information.
pub async fn plan_releases(
    config: &AppConfig,
    package_names: &[String],
    target_branch: &str,
) -> anyhow::Result<Vec<PackageRelease>> {
    let mut planned_releases = Vec::with_capacity(config.packages.len());
    for pkg in &config.packages {
        // Skip packages that are not in the list of package names to plan releases for, if any.
        let pkg_name = pkg.name().to_string();
        if !package_names.is_empty() && !package_names.contains(&pkg_name) {
            debug!("skipping {}", pkg_name);
            continue;
        }

        // Run `git-cliff` in the package directory.
        let context = match run_git_cliff(&pkg.dir, None)? {
            // If the context contains a bump type, it indicates that there are changes to be released.
            // If the context does not contain a previous version, it also indicates that this is the first release.
            Some(ctx) if !ctx["bump_type"].is_null() || ctx["previous"].is_null() => {
                debug!("{} has unreleased changes ✏️", pkg_name);
                ctx
            }
            // Otherwise, there are no changes to be released.
            _ => {
                debug!("{} is up-to-date ✅", pkg_name);
                continue;
            }
        };

        // Parse the version and render the changelog markdown for this package.
        let (current_version, next_version) = get_version_from_git_cliff_context(&context)?;
        let changelog_md = render_changelog_markdown(&context, None)?;

        // Render the commit message for this package release.
        // NB: The scope falls back to the package name unless this is the root package (see `PackageConfig::scope`).
        let commit_message = Handlebars::new()
            .render_template(
                &config.git.commit_message_pattern,
                &json!({
                    "package": pkg_name,
                    "scope": pkg.scope(),
                    "version": next_version.to_string(),
                    "branch": target_branch,
                }),
            )
            .context("failed to render commit message")?;

        // Append the planned release information for this package.
        planned_releases.push(PackageRelease {
            package_name: pkg_name,
            current_version,
            next_version,
            commit_message,
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
        let planned = plan_releases(&config, &[], "main").await.unwrap();
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].package_name, "example-rust-workspace");
        assert_eq!(planned[1].package_name, "example-api");
        assert_eq!(planned[0].current_version, Some(Version::parse("1.0.0").unwrap()));
        assert_eq!(planned[1].current_version, Some(Version::parse("1.0.0").unwrap()));
        assert_eq!(planned[0].next_version, Version::parse("1.1.0").unwrap());
        assert_eq!(planned[1].next_version, Version::parse("1.1.0").unwrap());
        assert_eq!(planned[0].commit_message, "chore: release v1.1.0");
        assert_eq!(planned[1].commit_message, "chore(example-api): release v1.1.0");
        assert!(!planned[0].changelog_md.is_empty());
        assert!(!planned[1].changelog_md.is_empty());
        assert!(planned[0].context.is_object());
        assert!(planned[1].context.is_object());
    }

    /// Tests that only the selected package releases are planned correctly.
    #[rstest]
    #[tokio::test]
    async fn test_plan_releases_with_selected(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
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

        // Call `plan_releases` with a selection of package names and assert that the planned releases are as expected.
        // NB: We expect one planned release: the `example-api` package as it is the only one selected.
        let planned = plan_releases(&config, &["example-api".to_string()], "main")
            .await
            .unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].package_name, "example-api");
    }
}
