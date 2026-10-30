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
    /// A list of package configurations.
    #[serde(default = "default_packages")]
    pub packages: Vec<PackageConfig>,
}

/// Returns the default value for `$.packages`.
fn default_packages() -> Vec<PackageConfig> {
    vec![PackageConfig {
        include_name_in_tag: false, // The root package should not include the name in tag
        ..Default::default()
    }]
}

/// Deserializes an empty [serde] struct, applying defaults.
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

    /// Tests that defaults are applied with workspace discovery enabled.
    #[test]
    fn test_apply_defaults_with_workspace_discovery_enabled() {
        let config = AppConfig {
            git: GitConfig::default(),
            packages: vec![PackageConfig {
                dir: PathBuf::from("."),
                strategy: Some(Strategy::Basic(BasicStrategy::default())),
                ..Default::default()
            }],
        }
        .apply_defaults()
        .unwrap();

        assert_eq!(config.packages.len(), 1);
        assert_eq!(
            config.packages[0].name(),
            std::env::current_dir().unwrap().file_name().unwrap().to_str().unwrap()
        );
    }

    /// Tests that workspace discovery is skipped when disabled.
    #[test]
    fn test_apply_defaults_with_workspace_discovery_disabled() {
        let config = AppConfig {
            git: GitConfig::default(),
            packages: vec![PackageConfig {
                dir: PathBuf::from("."),
                strategy: Some(Strategy::Basic(BasicStrategy::default())),
                workspace: false,
                ..Default::default()
            }],
        }
        .apply_defaults()
        .unwrap();

        assert_eq!(config.packages.len(), 1);
        assert!(!config.packages[0].workspace);
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
}
