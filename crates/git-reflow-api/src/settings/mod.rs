use crate::settings::git::GitConfig;
use crate::settings::pkg::PackageConfig;
use crate::strategy::BaseStrategy;
use anyhow::Context;
use config::{Config, Environment, File};
use std::path::PathBuf;
use tracing::{debug, trace};

pub mod git;
pub mod pkg;

/// The `git-reflow` configuration.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AppConfig {
    /// The Git provider configuration.
    #[serde(default = "deserialize_empty_struct")]
    pub git: GitConfig,
    /// A list of path globs, compiled when deserialized, to exclude from package discovery.
    #[serde(
        default,
        deserialize_with = "deserialize_exclude",
        serialize_with = "serialize_exclude"
    )]
    pub exclude: Vec<glob::Pattern>,
    /// A list of package configurations.
    #[serde(default = "default_packages")]
    pub packages: Vec<PackageConfig>,
}

/// Returns the default value for `$.packages`.
fn default_packages() -> Vec<PackageConfig> {
    vec![PackageConfig {
        include_name_in_tag: false, // NB: The root package should not include the name in tag.
        make_latest: true,          // NB: The root package should be marked as latest by default.
        ..Default::default()
    }]
}

/// Deserialises the package exclusion globs from `$.exclude`.
fn deserialize_exclude<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<glob::Pattern>, D::Error> {
    let patterns: Vec<String> = serde::Deserialize::deserialize(deserializer)?;
    patterns
        .iter()
        .map(|pattern| glob::Pattern::new(pattern).map_err(serde::de::Error::custom))
        .collect()
}

/// Serialises the package exclusion globs in `$.exclude` as strings.
fn serialize_exclude<S: serde::Serializer>(patterns: &[glob::Pattern], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(patterns.iter().map(glob::Pattern::as_str))
}

/// Deserialises an empty [serde] struct, applying defaults.
fn deserialize_empty_struct<'de, T: serde::Deserialize<'de>>() -> T {
    let empty = std::iter::empty::<((), ())>();
    let de = serde::de::value::MapDeserializer::<_, serde::de::value::Error>::new(empty);
    T::deserialize(de).unwrap()
}

impl AppConfig {
    /// Returns a given package configuration by its name.
    ///
    /// # Arguments
    ///
    /// * `name` - The package name or `.` for root.
    ///
    /// # Returns
    ///
    /// The package configuration if it exists.
    pub fn get_package(&self, name: &str) -> Option<&PackageConfig> {
        if name == "." {
            // root pkg
            let root_dir = PathBuf::from(name);
            self.packages.iter().find(|pkg| pkg.dir == root_dir)
        } else {
            // named pkg
            self.packages.iter().find(|pkg| pkg.name() == name)
        }
    }

    /// Applies default values to missing configuration options.
    pub fn apply_defaults(mut self) -> anyhow::Result<Self> {
        let mut packages = Vec::new();
        for package in self.packages {
            // For each package, cascade apply defaults.
            let package = package.apply_defaults()?;

            // If the package is a workspace, suggest nested packages and add them to the list.
            let suggested_packages = if package.workspace {
                package.strategy().suggest_packages(&package.dir)?
            } else {
                Vec::new()
            };

            // Add the package and any workspace packages to the list.
            packages.push(package);
            packages.extend(
                suggested_packages
                    .into_iter()
                    // Filter out any packages that already exist in the list.
                    .filter(|pkg| !packages.iter().any(|p| p.dir == pkg.dir))
                    // Filter out any packages that are excluded by the configuration (`$.exclude`).
                    .filter(|pkg| {
                        let excluded = self.exclude.iter().any(|glob| glob.matches_path(&pkg.dir));
                        if excluded {
                            debug!(
                                "skipping package '{}' at '{}' (excluded by config)",
                                pkg.name.as_deref().unwrap_or("?"),
                                pkg.dir.display(),
                            );
                        }
                        !excluded
                    })
                    .map(PackageConfig::apply_defaults)
                    .collect::<anyhow::Result<Vec<_>>>()?,
            );
        }
        self.packages = packages;
        Ok(self)
    }
}

/// Loads and parses the configuration.
///
/// ℹ️ Specify a custom config file via the `-c / --config <PATH>` CLI flag.
///
/// # Arguments
///
/// * `config_path` - An optional path to the configuration file.
///
/// # Returns
///
/// The parsed configuration instance.
pub fn load(config_path: Option<PathBuf>) -> anyhow::Result<AppConfig> {
    debug!("load configuration");

    // Add default configuration sources.
    let mut builder = Config::builder()
        .add_source(File::with_name(".reflow.json").required(false))
        .add_source(File::with_name(".reflow.json5").required(false))
        .add_source(File::with_name(".reflow.jsonc").required(false))
        .add_source(File::with_name(".reflow.toml").required(false))
        .add_source(File::with_name(".reflow.yaml").required(false))
        .add_source(File::with_name(".reflow.yml").required(false));

    // Add the user-specified configuration file, if provided.
    if let Some(path) = config_path {
        builder = builder.add_source(File::from(path).required(true));
    }

    // Add environment variables with highest priority.
    builder = builder.add_source(Environment::with_prefix("GIT_REFLOW"));

    // Load the configuration and deserialise it into mutable JSON.
    let config = builder.build().context("failed to build config")?;
    let mut config: serde_json::Value = config.try_deserialize().context("failed to deserialise config")?;

    // Apply defaults to the first package, if present.
    if let Some(package) = config
        .pointer_mut("/packages/0")
        .and_then(serde_json::Value::as_object_mut)
    {
        // The main package should not include the name in Git tags by default.
        package.entry("include-name-in-tag").or_insert(false.into());
        // The main package should be marked as latest by default.
        package.entry("make-latest").or_insert(true.into());
    }

    // Apply defaults to the configuration.
    let result: AppConfig = serde_json::from_value(config).context("failed to deserialise config")?;
    let result = result.apply_defaults()?;

    // Print debug information about the found packages.
    for pkg in &result.packages {
        debug!(
            "📦 found {} package '{}' at '{}'",
            pkg.strategy(),
            pkg.name.as_deref().unwrap_or("?"),
            pkg.dir.to_str().unwrap()
        );
    }

    // Return the resulting configuration.
    trace!("resulting configuration: {result:#?}");
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{Strategy, basic::BasicStrategy};
    use assert_fs::TempDir;
    use assert_fs::fixture::PathChild;
    use git_reflow_fixtures::project_repo;
    use git2::Repository;
    use std::path::Path;

    /// Tests that workspace discovery adds workspace members and applies their defaults.
    #[rstest::rstest]
    fn test_apply_defaults_with_workspace_discovery_enabled(
        #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
    ) {
        // Create a default configuration with workspace discovery enabled.
        let (project_dir, _repo) = project_repo;
        let config = AppConfig {
            packages: vec![PackageConfig {
                dir: project_dir.path().to_path_buf(),
                ..Default::default()
            }],
            ..Default::default()
        }
        .apply_defaults()
        .unwrap();

        // Verify that the workspace members were discovered and added to the configuration.
        assert!(config.exclude.is_empty());
        assert_eq!(config.packages.len(), 3);
        assert_eq!(config.packages[0].dir, project_dir.path());
        assert_eq!(config.packages[1].dir, project_dir.child("crates/example-api").path());
        assert_eq!(config.packages[2].dir, project_dir.child("crates/example-cli").path());
        assert_eq!(config.packages[1].name, Some("example-api".to_string()));
        assert_eq!(config.packages[2].name, Some("example-cli".to_string()));
        assert_eq!(config.packages[1].scope(), Some("example-api"));
        assert_eq!(config.packages[2].scope(), Some("example-cli"));
        assert!(matches!(config.packages[1].strategy(), Strategy::Rust(_)));
        assert!(matches!(config.packages[2].strategy(), Strategy::Rust(_)));
        assert!(!config.packages[1].make_latest);
        assert!(!config.packages[2].make_latest);
    }

    /// Tests that workspace discovery is skipped when disabled.
    #[test]
    fn test_apply_defaults_with_workspace_discovery_disabled() {
        // Create a default configuration with workspace discovery disabled.
        let config = AppConfig {
            git: GitConfig::default(),
            packages: vec![PackageConfig {
                dir: PathBuf::from("."),
                strategy: Some(Strategy::Basic(BasicStrategy::default())),
                workspace: false,
                ..Default::default()
            }],
            ..Default::default()
        }
        .apply_defaults()
        .unwrap();

        // Verify that the workspace members were not discovered and the configuration remains unchanged.
        assert_eq!(config.packages.len(), 1);
        assert!(!config.packages[0].workspace);
        assert!(config.exclude.is_empty());
    }

    /// Tests that discovered packages are excluded when they match the `$.exclude` globs setting.
    #[rstest::rstest]
    #[case::one_member("crates/example-api", vec!["example-cli"])]
    #[case::all_members("crates/*", vec![])]
    #[case::no_members("crates/missing", vec!["example-api", "example-cli"])]
    fn test_apply_defaults_with_excluded_packages(
        #[with("example-rust-workspace")] project_repo: (TempDir, Repository),
        #[case] pattern: &str,
        #[case] expected: Vec<&str>,
    ) {
        // Create a default configuration with workspace discovery enabled and an exclusion pattern.
        let (project_dir, _repo) = project_repo;
        let pattern = project_dir.path().join(pattern);
        let config = AppConfig {
            exclude: vec![glob::Pattern::new(pattern.to_str().unwrap()).unwrap()],
            packages: vec![PackageConfig {
                dir: project_dir.path().to_path_buf(),
                ..Default::default()
            }],
            ..Default::default()
        }
        .apply_defaults()
        .unwrap();

        // Verify that the in-scope workspace members were discovered and added to the configuration.
        assert_eq!(config.exclude.len(), 1);
        assert_eq!(config.packages.len(), expected.len() + 1); // NB: `+1` for the root package.
        assert_eq!(config.packages[0].dir, project_dir.path());
        assert!(config.packages[0].workspace);
        assert_eq!(
            config.packages[1..].iter().map(PackageConfig::name).collect::<Vec<_>>(),
            expected,
        );
    }

    /// Tests that only the first package defaults to unprefixed Git tags without overriding explicit settings.
    #[rstest::rstest]
    fn test_load_when_packages_include_name_in_tag(#[values(None, Some(true), Some(false))] explicit: Option<bool>) {
        // Prepare two package configurations.
        let mut package = serde_json::json!({ "name": "first", "strategy": "basic" });
        if let Some(value) = explicit {
            package["include-name-in-tag"] = value.into();
        }
        let packages = serde_json::json!([package, { "name": "second", "strategy": "basic" }]);

        // Create a temporary configuration file.
        let file = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
        serde_json::to_writer(&file, &serde_json::json!({ "packages": packages })).unwrap();

        // Load the configuration.
        let config = load(Some(file.path().to_path_buf())).unwrap();

        // Verify that the first package's `include_name_in_tag` is set to the explicit value or defaults to `false`.
        assert_eq!(config.packages[0].include_name_in_tag, explicit.unwrap_or(false));
        assert!(config.packages[1].include_name_in_tag);
    }

    /// Tests that only the first package defaults to make latest without overriding explicit settings.
    #[rstest::rstest]
    fn test_load_when_packages_make_latest(
        #[values(None, Some(true), Some(false))] explicit: Option<bool>,
        #[values(None, Some(true), Some(false))] second_explicit: Option<bool>,
    ) {
        // Prepare two package configurations.
        let mut package = serde_json::json!({ "name": "first", "strategy": "basic" });
        if let Some(value) = explicit {
            package["make-latest"] = value.into();
        }
        let mut second_package = serde_json::json!({ "name": "second", "strategy": "basic" });
        if let Some(value) = second_explicit {
            second_package["make-latest"] = value.into();
        }
        let packages = serde_json::json!([package, second_package]);

        // Create a temporary configuration file.
        let file = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
        serde_json::to_writer(&file, &serde_json::json!({ "packages": packages })).unwrap();

        // Load the configuration.
        let config = load(Some(file.path().to_path_buf())).unwrap();

        // Verify that the first package's `make_latest` is set to the explicit value or defaults to `true`.
        assert_eq!(config.packages[0].make_latest, explicit.unwrap_or(true));
        assert_eq!(config.packages[1].make_latest, second_explicit.unwrap_or(false));
    }

    /// Tests that exclusion globs (i.e. `$.exclude`) are compiled and serialised to their original strings.
    #[test]
    fn test_load_exclude_setting() {
        let exclude = serde_json::json!(["crates/*", "examples/**"]);
        let config: AppConfig = serde_json::from_value(serde_json::json!({ "exclude": exclude })).unwrap();

        assert!(config.exclude[0].matches_path(Path::new("crates/example")));
        assert!(!config.exclude[0].matches_path(Path::new("other/example")));
        assert_eq!(serde_json::to_value(&config).unwrap()["exclude"], exclude);
    }
}
