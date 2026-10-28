use crate::git_cliff::{get_version_from_git_cliff_context, render_changelog_markdown, run_git_cliff};
use crate::settings::{AppConfig, pkg::PackageConfig};
use crate::strategy::BaseStrategy;
use anyhow::{Context, bail};
use handlebars::Handlebars;
use semver::Version;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// A planned package release.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PackageRelease {
    /// The name of the package.
    pub package_name: String,
    /// The current version of the package, if it exists.
    pub current_version: Option<Version>,
    /// The next (target) version of the package.
    pub next_version: Version,
    /// The commit message for the package release.
    pub commit_message: String,
    /// The rendered changelog Markdown for the package.
    pub changelog_md: String,
    /// A map of directly changing dependencies to their planned target versions, if any.
    pub dependencies: HashMap<String, Version>,
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
    // Build a dependency graph of the configured packages, and sort them in release order.
    let dependency_graph = build_dependency_graph(config)?;
    let ordered_packages = sort_packages_by_dependency_order(&config.packages, &dependency_graph)?;

    // For each package, check if there are unreleased changes and plan its release.
    let mut planned_releases = Vec::with_capacity(config.packages.len());
    let mut planned_versions: HashMap<String, Version> = HashMap::new();
    for pkg in ordered_packages {
        // Skip packages that are not in the list of package names to plan releases for, if any.
        let pkg_name = pkg.name().to_string();
        if !package_names.is_empty() && !package_names.contains(&pkg_name) {
            debug!("skipping {}", pkg_name);
            continue;
        }

        // Collect changing dependencies and a synthetic commit for each dependency update.
        let mut dependencies = HashMap::new();
        let mut extra_commits = Vec::new();
        for dep in &dependency_graph[&pkg_name] {
            if let Some(version) = planned_versions.get(dep) {
                debug!(
                    "{} depends on {} which is planned for release - bumping version",
                    pkg_name, dep
                );
                dependencies.insert(dep.clone(), version.clone());
                extra_commits.push(format!("fix(deps): update dependency `{}` to v{}", dep, version));
            }
        }

        // Run `git-cliff` in the package directory.
        let context = match run_git_cliff(&pkg.dir, &extra_commits, None)? {
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

        // Parse the version and render the changelog Markdown for this package.
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
        planned_versions.insert(pkg_name.clone(), next_version.clone());
        planned_releases.push(PackageRelease {
            package_name: pkg_name,
            current_version,
            next_version,
            commit_message,
            changelog_md,
            dependencies,
            context,
        });
    }
    Ok(planned_releases)
}

/// Builds a dependency graph of the configured packages.
///
/// # Arguments
///
/// * `config` - The app configuration.
///
/// # Returns
///
/// A result containing a map of package names to their dependencies.
pub fn build_dependency_graph(config: &AppConfig) -> anyhow::Result<HashMap<String, Vec<String>>> {
    config
        .packages
        .iter()
        .map(|pkg| Ok((pkg.name().to_string(), pkg.strategy().find_dependencies(config, pkg)?)))
        .collect::<anyhow::Result<HashMap<_, _>>>()
}

/// Sorts packages with dependencies first, reporting cycles or unresolved dependencies.
///
/// # Arguments
///
/// * `packages` - The list of packages to sort.
/// * `dependencies` - A map of package names to their dependencies.
///
/// # Returns
///
/// A result containing the sorted list of packages, or an error if there are cycles or unresolved dependencies.
pub fn sort_packages_by_dependency_order<'a>(
    packages: &'a [PackageConfig],
    dependencies: &HashMap<String, Vec<String>>,
) -> anyhow::Result<Vec<&'a PackageConfig>> {
    let mut remaining: Vec<_> = packages.iter().collect();
    let mut ordered = Vec::with_capacity(packages.len());
    let mut visited = HashSet::new();
    while !remaining.is_empty() {
        let Some(index) = remaining.iter().position(|pkg| {
            dependencies
                .get(pkg.name())
                .into_iter()
                .flatten()
                .all(|dep| visited.contains(dep.as_str()))
        }) else {
            bail!(
                "cyclic or unresolved package dependencies: {}",
                remaining.iter().map(|pkg| pkg.name()).collect::<Vec<_>>().join(", ")
            );
        };
        let pkg = remaining.remove(index);
        visited.insert(pkg.name());
        ordered.push(pkg);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::stage_and_commit;
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
        stage_and_commit(&repo, ["."], "chore: initial commit").unwrap();

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
        stage_and_commit(&repo, ["."], "feat(api): add a `subtract` function").unwrap();

        // Call `plan_releases` and assert that the planned releases are as expected.
        // NB: We expect three planned releases: one for the root package, one for the `example-api` package, and
        //     one for the `example-cli` package since it depends on `example-api`.
        let planned = plan_releases(&config, &[], "main").await.unwrap();
        assert_eq!(planned.len(), 3);
        assert_eq!(planned[0].package_name, "example-rust-workspace");
        assert_eq!(planned[1].package_name, "example-api");
        assert_eq!(planned[2].package_name, "example-cli");
        assert_eq!(planned[0].current_version, Some(Version::parse("1.0.0").unwrap()));
        assert_eq!(planned[1].current_version, Some(Version::parse("1.0.0").unwrap()));
        assert_eq!(planned[0].next_version, Version::parse("1.1.0").unwrap());
        assert_eq!(planned[1].next_version, Version::parse("1.1.0").unwrap());
        assert_eq!(planned[2].next_version, Version::parse("1.0.1").unwrap());
        assert_eq!(planned[0].commit_message, "chore: release v1.1.0");
        assert_eq!(planned[1].commit_message, "chore(example-api): release v1.1.0");
        assert!(!planned[0].changelog_md.is_empty());
        assert!(!planned[1].changelog_md.is_empty());
        assert!(
            planned[2]
                .changelog_md
                .contains("Update dependency `example-api` to v1.1.0")
        );
        assert!(planned[0].dependencies.is_empty());
        assert!(planned[1].dependencies.is_empty());
        assert_eq!(
            planned[2].dependencies,
            HashMap::from([("example-api".to_string(), planned[1].next_version.clone())]),
        );
        assert!(planned[0].context.is_object());
        assert!(planned[1].context.is_object());
        assert!(planned[2].context.is_object());
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

        // Call `plan_releases` with a selection of package names and assert that the planned releases are as expected.
        // NB: We expect one planned release: the `example-api` package as it is the only one selected.
        let planned = plan_releases(&config, &["example-api".to_string()], "main")
            .await
            .unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].package_name, "example-api");
        assert!(planned[0].dependencies.is_empty());
    }

    /// Tests that the dependency graph is built correctly.
    #[rstest]
    fn test_build_dependency_graph(#[with("example-rust-workspace")] project_repo: (TempDir, Repository)) {
        let (project_dir, _repo) = project_repo;

        // Load the configuration for the project repository.
        std::env::set_current_dir(&project_dir).unwrap();
        let config = crate::settings::load(None).unwrap();

        // Build the dependency graph and assert that it matches the expected structure.
        let graph = build_dependency_graph(&config).unwrap();
        assert_eq!(graph.len(), 3);
        assert!(graph["example-rust-workspace"].is_empty());
        assert!(graph["example-api"].is_empty());
        assert_eq!(graph["example-cli"], vec!["example-api"]);
    }

    /// Tests that configured packages are returned in dependency order, i.e. dependencies precede dependants.
    #[rstest]
    #[case::chain(vec![vec!["b"], vec!["c"], vec![]], Some(vec!["c", "b", "a"]))]
    #[case::independent(vec![vec![], vec![], vec![]], Some(vec!["a", "b", "c"]))]
    #[case::shared(vec![vec!["c"], vec!["c"], vec![]], Some(vec!["c", "a", "b"]))]
    #[case::cycle(vec![vec!["b"], vec!["a"], vec![]], None)]
    #[case::self_dependency(vec![vec!["a"], vec![], vec![]], None)]
    #[case::missing(vec![vec!["unknown"], vec![], vec![]], None)]
    fn test_sort_packages_by_dependency_order(
        #[case] dependencies: Vec<Vec<&str>>,
        #[case] expected: Option<Vec<&str>>,
    ) {
        // Build a list of configured packages.
        let packages: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|name| PackageConfig {
                name: Some(name.to_string()),
                ..Default::default()
            })
            .collect();

        // Build a dependency graph of the configured packages.
        let graph = packages
            .iter()
            .zip(dependencies)
            .map(|(pkg, deps)| (pkg.name().to_string(), deps.into_iter().map(String::from).collect()))
            .collect();

        // Sort the packages by dependency order.
        let result = sort_packages_by_dependency_order(&packages, &graph);

        // Verify that the result matches the expected order, or an error if there is a cycle or missing dependency.
        if let Some(expected) = expected {
            assert_eq!(
                result.unwrap().iter().map(|pkg| pkg.name()).collect::<Vec<_>>(),
                expected
            );
        } else {
            assert!(result.is_err());
        }
    }
}
