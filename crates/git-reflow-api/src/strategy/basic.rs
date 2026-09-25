use crate::settings::pkg::PackageConfig;
use crate::strategy::BaseStrategy;
use anyhow::Context;
use semver::Version;
use std::path::{Path, PathBuf};
use tracing::debug;

/// The basic release strategy.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BasicStrategy {
    // empty
}

impl BaseStrategy for BasicStrategy {
    /// Writes a new version to the `VERSION.txt` file.
    ///
    /// # Arguments
    ///
    /// * `new_version` - The new version to apply.
    /// * `config` - The package configuration.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result containing a list of the changed file paths.
    fn write_version(
        &self,
        new_version: &Version,
        config: &PackageConfig,
        dry_run: bool,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let path = config.dir.join("VERSION.txt");
        if !dry_run {
            std::fs::write(&path, format!("v{new_version}"))?;
            debug!("write `v{}` to `{}`", new_version, path.display());
        } else {
            debug!("write `v{}` to `{}` (dry run)", new_version, path.display());
        }
        Ok(vec![path])
    }

    /// Suggests the package name from the directory name.
    ///
    /// # Arguments
    ///
    /// * `dir` - The package directory to get the name for.
    ///
    /// # Returns
    ///
    /// The package name for the given project.
    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String> {
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            std::env::current_dir()?.join(dir)
        };
        dir.file_name()
            .and_then(|name| name.to_str())
            .map(|s| s.to_string())
            .context("no valid directory name found")
    }

    fn suggest_packages(&self, _dir: &Path) -> anyhow::Result<Vec<PackageConfig>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::{TempDir, prelude::*};
    use rstest::{fixture, rstest};

    /// A test fixture for a default basic strategy.
    #[fixture]
    fn strategy() -> BasicStrategy {
        BasicStrategy::default()
    }

    /// Tests that the version is written to a file in the package root.
    #[rstest]
    fn write_version(strategy: BasicStrategy) {
        let dir = TempDir::new().unwrap();
        let config = PackageConfig {
            dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let changed_files = strategy
            .write_version(&Version::parse("1.2.3-rc.2").unwrap(), &config, false)
            .unwrap();

        dir.child("VERSION.txt").assert("v1.2.3-rc.2");
        assert_eq!(changed_files, vec![config.dir.join("VERSION.txt")]);
    }

    /// Tests that the version is *not* written to a file in the package root when performing a dry run.
    #[rstest]
    fn write_version_with_dry_run(strategy: BasicStrategy) {
        let dir = TempDir::new().unwrap();
        let config = PackageConfig {
            dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        dir.child("VERSION.txt").write_str("v1.2.3-rc.1").unwrap();
        strategy
            .write_version(&Version::parse("1.2.3-rc.2").unwrap(), &config, true)
            .unwrap();

        // The version should not be updated because this is a dry run.
        dir.child("VERSION.txt").assert("v1.2.3-rc.1");
    }

    /// Tests that the suggested name for a path is the folder's name.
    #[rstest]
    #[case::top_level("git-reflow", Ok("git-reflow"))]
    #[case::multi_level("crates/git-reflow-api", Ok("git-reflow-api"))]
    #[case::relative_path("./magic", Ok("magic"))]
    #[case::root_dir("/", Err("no valid directory name found"))]
    fn suggest_name_for_dir(strategy: BasicStrategy, #[case] filename: &str, #[case] expected: Result<&str, &str>) {
        let result = strategy.suggest_name(Path::new(filename));
        match (result, expected) {
            (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
            (Err(actual), Err(expected)) => assert_eq!(actual.to_string(), expected),
            _ => panic!("unexpected result"), // coverage-ignore: unreachable when tests pass
        }
    }

    /// Tests that the suggested name for the current directory is its folder name.
    #[rstest]
    fn suggest_name_for_current_dir(strategy: BasicStrategy) {
        let dir = std::env::current_dir().unwrap();
        let dir_name = dir.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(strategy.suggest_name(Path::new(".")).unwrap(), dir_name);
    }

    /// Tests that there are no suggested packages.
    #[rstest]
    fn suggest_packages(strategy: BasicStrategy) {
        assert_eq!(strategy.suggest_packages(Path::new(".")).unwrap().len(), 0);
    }
}
