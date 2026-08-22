use std::path::PathBuf;

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

    /// Returns the path to the package's changelog file.
    pub fn changelog_path(&self) -> PathBuf {
        self.changelog_path
            .as_ref()
            .map_or(self.dir.join("CHANGELOG.md"), |c| self.dir.join(c))
    }

    /// Applies default values to missing configuration options.
    pub fn apply_defaults(self) -> anyhow::Result<Self> {
        Ok(self)
    }
}
