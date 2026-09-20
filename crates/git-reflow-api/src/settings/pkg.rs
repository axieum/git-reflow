use crate::detect::detect_strategy;
use crate::strategy::{BaseStrategy, Strategy};
use anyhow::Context;
use std::path::{Path, PathBuf};
use tracing::trace;

/// The individual package configuration.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PackageConfig {
    /// The directory of the package root.
    ///
    /// **Default:** `.`
    #[serde(serialize_with = "serialize_dir_as_slash")]
    pub dir: PathBuf,
    /// The name of the package.
    ///
    /// **Default:** `<auto-detected>`
    pub name: Option<String>,
    /// The conventional commit scope used in commits that affect this package, e.g. `api` in `chore(api): ...`.
    ///
    /// If not specified, the package name is used, unless this is the root package, in which no scope is used.
    ///
    /// **Default:** `$.name` (or `None` for the root package)
    pub scope: Option<String>,
    /// The release strategy.
    ///
    /// **Default:** `<auto-detected>`
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
            // By default, the conventional commit scope should fallback to the package name.
            scope: None,
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

    /// Returns the effective conventional commit scope for the package, if any.
    ///
    /// This is `None` for the root package unless a `scope` is explicitly configured, and is otherwise
    /// resolved to the package `name` by [`Self::apply_defaults`] if not explicitly configured.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
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
            self.strategy = detect_strategy(&self.dir).map(Some).context(
                r#"To resolve this issue:
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
            self.name = self.strategy().suggest_name(&self.dir).map(Some).context(
                r#"To resolve this issue:
  ├ Verify you provided the correct `packages[].dir` path in your configuration;
  ├ Ensure the directory contains a supported configuration file, e.g. `pyproject.toml` or `package.json`;
  ⌊ Otherwise, manually set the package name via `packages[].name` in your configuration.

For further assistance, run `git reflow --help` or visit https://github.com/axieum/git-reflow."#,
            )?;
            trace!("detected package name `{}` in `{}`", self.name(), self.dir.display());
        }

        // If no explicit scope is set, default it to the package name, unless this is the root package.
        if self.scope.is_none() && self.dir != Path::new(".") {
            self.scope = Some(self.name().to_string());
            trace!("default package scope to `{}` in `{}`", self.name(), self.dir.display());
        }

        Ok(self)
    }
}

/// Serializes a [`PathBuf`] using forward slashes (`/`) as the separator, regardless of platform,
/// so that serialized output (and snapshot tests) are consistent across Windows and Unix-like systems.
fn serialize_dir_as_slash<S>(path: &std::path::Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy().replace('\\', "/"))
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
        assert_eq!(config.scope, None);
        assert_eq!(config.strategy, None);
        assert!(config.workspace);
        assert!(config.include_name_in_tag);
        assert_eq!(config.changelog_path(), PathBuf::from(".").join("CHANGELOG.md"));
    }

    /// Tests that the package directory is always serialised with forward slashes, regardless of platform.
    #[test]
    fn serializes_dir_with_forward_slashes() {
        let config = PackageConfig {
            dir: PathBuf::from("crates").join("example-api"),
            ..Default::default()
        };

        let json = serde_json::to_string(&config).unwrap();

        assert!(json.contains(r#""dir":"crates/example-api""#), "json was: {json}");
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
        assert_eq!(config.scope(), Some("api"));
        assert_eq!(config.strategy(), &Strategy::Basic(BasicStrategy::default()));
        assert!(!config.workspace);
        assert!(!config.include_name_in_tag);
        assert_eq!(
            config.changelog_path(),
            PathBuf::from("packages/api").join("docs/changes.md")
        );
    }

    /// Tests that the conventional commit scope defaults to the package name for non-root packages.
    #[test]
    fn scope_defaults_to_the_package_name_for_non_root_packages() {
        let config = PackageConfig {
            dir: PathBuf::from("crates").join("example-api"),
            name: Some(String::from("example-api")),
            strategy: Some(Strategy::Basic(BasicStrategy::default())),
            ..Default::default()
        }
        .apply_defaults()
        .unwrap();

        assert_eq!(config.scope(), Some("example-api"));
    }

    /// Tests that the conventional commit scope remains `None` for the root package when no explicit scope is set.
    #[test]
    fn scope_is_none_for_root_package_by_default() {
        let config = PackageConfig {
            name: Some(String::from("example-rust-workspace")),
            strategy: Some(Strategy::Basic(BasicStrategy::default())),
            ..Default::default()
        }
        .apply_defaults()
        .unwrap();

        assert_eq!(config.scope(), None);
    }

    /// Tests that an explicit conventional commit scope takes precedence and is left untouched by defaulting.
    #[test]
    fn scope_uses_explicit_value_when_set() {
        let config = PackageConfig {
            dir: PathBuf::from("crates").join("example-api"),
            name: Some(String::from("example-api")),
            scope: Some(String::from("api")),
            strategy: Some(Strategy::Basic(BasicStrategy::default())),
            ..Default::default()
        }
        .apply_defaults()
        .unwrap();

        assert_eq!(config.scope(), Some("api"));
    }
}
