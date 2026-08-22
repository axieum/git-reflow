use config::{Config, Environment, File};
use crate::settings::pkg::PackageConfig;
use std::path::PathBuf;
use anyhow::Context;
use tracing::{debug, trace};

pub mod pkg;

/// The `git-reflow` configuration.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AppConfig {
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

impl AppConfig {
    /// Returns a given package configuration by its name.
    ///
    /// # Arguments
    /// * `name` - The package name or `.` for root.
    ///
    /// # Returns
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
        // Apply defaults to each package configuration
        self.packages = self
            .packages
            .into_iter()
            // For each package, cascade apply defaults
            .map(|pkg| pkg.apply_defaults())
            .collect::<anyhow::Result<Vec<_>>>()?; // Stop if any package fails to apply defaults
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

    // Load the configuration and deserialise it.
    let config = builder.build().context("failed to build config")?;
    let result: AppConfig = config.try_deserialize().context("failed to deserialise config")?;

    // Apply defaults to the configuration.
    let result = result.apply_defaults()?;

    // Print debug information about the found packages.
    for pkg in &result.packages {
        debug!(
            "📦 found package '{}' at '{}'",
            pkg.name.as_deref().unwrap_or("?"),
            pkg.dir.to_str().unwrap()
        );
    }

    // Return the resulting configuration.
    trace!("resulting configuration: {result:#?}");
    Ok(result)
}
