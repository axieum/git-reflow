use crate::detect::detect_strategy;
use crate::strategy::{BaseStrategy, Strategy};
use anyhow::Context;
use std::path::PathBuf;
use tracing::trace;

/// The individual package configuration.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PackageConfig {
    /// The directory of the package root.
    ///
    /// **Default:** `.`
    pub dir: PathBuf,
    /// The name of the package.
    ///
    /// **Default:** `<auto-detected>`
    pub name: Option<String>,
    /// The release strategy.
    ///
    /// **Default:** \<auto detected>
    #[serde(deserialize_with = "crate::strategy::string_or_struct")]
    pub strategy: Option<Strategy>,
    /// If `true`, detect nested packages if any.
    ///
    /// **Default:** `true`
    pub workspace: bool,
    /// If `true`, prefix the git tag with the package name, e.g. `git-reflow-api-v1.0.0`.
    ///
    /// **Default:** `true`
    pub include_name_in_tag: bool,
    /// The changelog path relative to the package directory.
    ///
    /// **Default:** `CHANGELOG.md`
    pub changelog_path: Option<String>,
}

impl Default for PackageConfig {
    fn default() -> Self {
        Self {
            // The default package directory is the current working directory.
            dir: PathBuf::from("."),
            // The package name is auto-detected from the package's manifest file, e.g. `Cargo.toml`.
            name: None,
            // The package strategy is auto-detected from the pacakge's manifest file, e.g. `Cargo.toml`
            strategy: None,
            // By default, we assume that the package is part of a workspace and will detect nested packages if any.
            workspace: true,
            // By default, we include the package name in the git tag, e.g. `git-reflow-api-v1.0.0`.
            include_name_in_tag: true,
            // The default changelog path is `CHANGELOG.md` relative to the package directory.
            changelog_path: Some(String::from("CHANGELOG.md")),
        }
    }
}

impl PackageConfig {
    /// Returns the guaranteed name of the package.
    pub fn name(&self) -> &str {
        self.name.as_ref().unwrap()
    }

    /// Returns the guaranteed release strategy for the package.
    pub fn strategy(&self) -> &Strategy {
        self.strategy.as_ref().unwrap()
    }

    /// Returns the path to the package's changelog file.
    pub fn changelog_path(&self) -> PathBuf {
        self.changelog_path
            .as_ref()
            .map_or(self.dir.join("CHANGELOG.md"), |c| self.dir.join(c))
    }

    /// Applies default values to missing configuration options.
    pub fn apply_defaults(mut self) -> anyhow::Result<Self> {
        // If the strategy is not specified, attempt to detect it based on files present.
        if self.strategy.is_none() {
            self.strategy = detect_strategy(&self.dir).map(Some).context(r#"To resolve this issue:
  ├ Verify you provided the correct `packages[].dir` path in your configuration;
  ├ Ensure the directory contains a supported configuration file, e.g. `pyproject.toml` or `package.json`;
  ⌊ Otherwise, manually set the release strategy via `packages[].strategy` in your configuration.

For further assistance, run `git reflow --help` or visit https://github.com/axieum/git-reflow."#,
            )?;
            trace!(
                "detected release strategy `{}` in `{}`",
                self.strategy(),
                self.dir.display()
            );
        }

        // If the package name is not specified, attempt to extract it from the strategy's files.
        if self.name.is_none() {
            self.name = self.strategy().suggest_name(&self.dir).map(Some).context(r#"To resolve this issue:
  ├ Verify you provided the correct `packages[].dir` path in your configuration;
  ├ Ensure the directory contains a supported configuration file, e.g. `pyproject.toml` or `package.json`;
  ⌊ Otherwise, manually set the package name via `packages[].name` in your configuration.

For further assistance, run `git reflow --help` or visit https://github.com/axieum/git-reflow."#,
            )?;
            trace!(
                "detected package name `{}` in `{}`",
                self.name(),
                self.dir.display()
            );
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::basic::BasicStrategy;

    /// Tests that an empty package config deserialises to defaults.
    #[test]
    fn deserializes_defaults() {
        let config: PackageConfig = serde_json::from_str("{}").unwrap();

        assert_eq!(config.dir, PathBuf::from("."));
        assert_eq!(config.name, None);
        assert_eq!(config.strategy, None);
        assert!(config.workspace);
        assert!(config.include_name_in_tag);
        assert_eq!(
            config.changelog_path(),
            PathBuf::from(".").join("CHANGELOG.md")
        );
    }

    /// Tests that explicit package config values are deserialised and retained when defaults are applied.
    #[test]
    fn deserializes_explicit_values_and_applies_defaults() {
        let config: PackageConfig = serde_json::from_str(
            r#"{
                "dir": "packages/api",
                "name": "api",
                "strategy": "basic",
                "workspace": false,
                "include-name-in-tag": false,
                "changelog-path": "docs/changes.md"
            }"#,
        )
        .unwrap();
        let config = config.apply_defaults().unwrap();

        assert_eq!(config.dir, PathBuf::from("packages/api"));
        assert_eq!(config.name(), "api");
        assert_eq!(
            config.strategy(),
            &Strategy::Basic(BasicStrategy::default())
        );
        assert!(!config.workspace);
        assert!(!config.include_name_in_tag);
        assert_eq!(
            config.changelog_path(),
            PathBuf::from("packages/api").join("docs/changes.md")
        );
    }
}
