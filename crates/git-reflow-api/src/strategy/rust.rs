use crate::settings::pkg::PackageConfig;
use crate::strategy::{BaseStrategy, Strategy};
use anyhow::{Context, anyhow, bail};
use semver::Version;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;
use toml_edit::{DocumentMut, Item};
use tracing::debug;

/// The [Rust](https://www.rust-lang.org/) release strategy.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RustStrategy {
    // empty
}

impl BaseStrategy for RustStrategy {
    /// Writes a new version to the `Cargo.toml` and `Cargo.lock` files.
    ///
    /// # Arguments
    ///
    /// * `new_version` - The new version to apply.
    /// * `config` - The package configuration.
    fn write_version(&self, new_version: &Version, config: &PackageConfig) -> anyhow::Result<()> {
        // Update the `Cargo.toml` file
        if Self::write_version_to_cargo_toml(&config.dir.join("Cargo.toml"), new_version)? {
            // Update the `Cargo.lock` file
            Self::write_version_to_cargo_lock(
                &std::env::current_dir()?,
                &config.dir,
                config.name(),
                new_version,
            )?;
        }
        Ok(())
    }

    /// Suggests the package name from the `[package.name]` field of the `Cargo.toml` file.
    ///
    /// For a Cargo workspace, it will return the directory name.
    ///
    /// # Arguments
    /// * `dir` - The package directory to get the name for.
    ///
    /// # Returns
    /// The package name for the given Rust project.
    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String> {
        let filename = &dir.join("Cargo.toml");
        let contents = fs::read_to_string(filename)?;
        let data: CargoToml = toml::from_str(&contents)?;

        data.package
            .map_or_else(
                || {
                    let dir = if dir.is_absolute() {
                        dir.to_path_buf()
                    } else {
                        std::env::current_dir()?.join(dir)
                    };
                    dir.file_name()
                        .and_then(|name| name.to_str())
                        .map(|s| s.to_string())
                        .context("`[package.name]` not specified and no valid directory name found")
                },
                |pkg| Ok(pkg.name),
            )
            .map_err(|err| {
                anyhow!(
                    "could not find package name in `{}`: {err}",
                    filename.display()
                )
            })
    }

    /// Suggests Cargo workspace members as package configurations that should be included.
    ///
    /// # Arguments
    /// * `dir` - The directory to a *possible* Cargo workspace.
    ///
    /// # Returns
    /// The package configurations of each Cargo workspace member if any.
    fn suggest_packages(&self, dir: &Path) -> anyhow::Result<Vec<PackageConfig>> {
        let filename = &dir.join("Cargo.toml");
        let contents = fs::read_to_string(filename)
            .map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let data: Value = toml::from_str(&contents)
            .map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        let workspace = data.get("workspace");

        let members = workspace
            .and_then(|workspace| workspace.get("members"))
            .and_then(|members| members.as_array())
            .into_iter()
            .flatten()
            .filter_map(|member| member.as_str())
            .collect::<Vec<_>>();

        let exclude = workspace
            .and_then(|workspace| workspace.get("exclude"))
            .and_then(|exclude| exclude.as_array())
            .into_iter()
            .flatten()
            .filter_map(|exclude| exclude.as_str())
            .flat_map(|exclude| {
                glob::glob(dir.join(exclude).to_str().unwrap())
                    .map_err(|err| anyhow!("invalid exclude glob `{exclude}`: {err}"))
            })
            .flatten()
            .flatten()
            .collect::<Vec<_>>();

        members
            .iter()
            .flat_map(|member| {
                glob::glob(dir.join(member).to_str().unwrap())
                    .map_err(|err| anyhow!("invalid crate glob `{member}`: {err}"))
            })
            .flatten()
            .filter_map(|entry| {
                entry
                    .as_ref()
                    .map_err(|err| anyhow!("error reading crate: {err}"))
                    .ok()
                    .cloned()
            })
            .filter(|path| !exclude.contains(path))
            .filter(|path| path.join("Cargo.toml").try_exists().unwrap_or(false))
            .map(|path| {
                PackageConfig {
                    dir: path,
                    strategy: Some(Strategy::Rust(RustStrategy::default())),
                    ..Default::default()
                }
                .apply_defaults()
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }
}

impl RustStrategy {
    /// Writes a new version to the given `Cargo.toml` file.
    ///
    /// # Arguments
    /// * `filename` - The path to the `Cargo.toml` file.
    /// * `new_version` - The new version to apply.
    ///
    /// # Returns
    /// A result of whether an update was made.
    fn write_version_to_cargo_toml(filename: &Path, new_version: &Version) -> anyhow::Result<bool> {
        let contents = fs::read_to_string(filename)
            .map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        if let Some(package) = data["package"].as_table_mut() {
            package["version"] = toml_edit::value(new_version.to_string());
            fs::write(filename, data.to_string()).map_err(|err| {
                anyhow!(
                    "failed to write `[package.version]` to `{}`: {err}",
                    filename.display()
                )
            })?;
            debug!(
                "set `[package.version]` to `{new_version}` at `{}`",
                filename.display()
            );
            Ok(true)
        } else {
            debug!(
                "refusing to write `[package.version]` to Cargo workspace at `{}`",
                filename.display()
            );
            Ok(false)
        }
    }

    /// Writes a new version for a crate to the `Cargo.lock` file.
    ///
    /// It will find the closest `Cargo.lock` file from a given package directory upwards.
    ///
    /// # Arguments
    /// * `root` - The root directory.
    /// * `dir` - The package directory.
    /// * `name` - The name of the package to update.
    /// * `new_version` - The new version to apply.
    ///
    /// # Returns
    /// A result of whether the update was successful.
    ///
    /// # See Also
    /// * [`Self::find_cargo_lock()`] - For finding the closest `Cargo.lock` file.
    fn write_version_to_cargo_lock(
        root: &Path,
        dir: &Path,
        name: &str,
        new_version: &Version,
    ) -> anyhow::Result<()> {
        let lockfile = Self::find_cargo_lock(dir, root)?;
        let lockfile_display = lockfile.strip_prefix(root)?.display();
        let contents = fs::read_to_string(&lockfile)
            .map_err(|err| anyhow!("could not read `{lockfile_display}`: {err}"))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{lockfile_display}`: {err}"))?;

        let package = data
            .get_mut("package")
            .and_then(Item::as_array_of_tables_mut)
            .and_then(|packages| {
                packages
                    .iter_mut()
                    .find(|pkg| pkg.get("name").and_then(Item::as_str) == Some(name))
            })
            .context(format!(
                "package `{name}` not found in `{}`",
                lockfile_display
            ))?;

        package["version"] = toml_edit::value(new_version.to_string());
        fs::write(&lockfile, data.to_string()).map_err(|err| {
            anyhow!("failed to write `[[package.version]]` to `{lockfile_display}`: {err}")
        })?;
        debug!("set `[[package]] version` to `{new_version}` for `{name}` at `{lockfile_display}`");

        Ok(())
    }

    /// Finds the closest `Cargo.lock` file from a given package directory upwards.
    ///
    /// This will not traverse any further than the given `relative` directory.
    ///
    /// # Arguments
    /// * `dir` - The package directory.
    /// * `relative` - The relative directory to scope the search to, e.g. [`std::env::current_dir()`].
    ///
    /// # Returns
    /// A result containing the `Cargo.lock` path if found.
    pub fn find_cargo_lock(dir: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
        let mut dir = relative.join(dir);

        // Traverse up until we find `Cargo.lock` or reach `cwd`
        while dir.starts_with(&relative) {
            // Check if `Cargo.lock` exists at this level
            let lockfile = dir.join("Cargo.lock");
            if lockfile.is_file() {
                return Ok(lockfile);
            }
            // Move to the parent directory
            if !dir.pop() {
                break;
            }
        }

        bail!(
            "could not find a `Cargo.lock` file for workspace member `{}`",
            dir.display()
        )
    }
}

#[derive(Deserialize)]
struct CargoToml {
    package: Option<CargoPackageToml>,
}

#[derive(Deserialize)]
struct CargoPackageToml {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use rstest::{fixture, rstest};

    /// A test fixture for a default Rust strategy.
    #[fixture]
    fn strategy() -> RustStrategy {
        RustStrategy::default()
    }

    /// Tests that the suggested name for a directory is extracted from the `Cargo.toml`.
    #[rstest]
    fn suggest_name_from_cargo_toml(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("Cargo.toml")
            .write_str("[package]\nname = \"magic\"")
            .unwrap();

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "magic");
    }

    /// Tests that the suggested name for a directory is the folder when `Cargo.toml` is workspace.
    #[rstest]
    fn suggest_name_from_cargo_toml_workspace(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("Cargo.toml")
            .write_str("[workspace]\nmembers = [\"crates/*\"]")
            .unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap(),
            temp_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(|s| s.to_string())
                .unwrap()
        );
    }

    /// Tests that the suggested name for a directory is not extracted from a malformed `Cargo.toml`.
    #[rstest]
    fn suggest_name_from_malformed_cargo_toml(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("Cargo.toml").write_str("[package]").unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "TOML parse error at line 1, column 1\n  |\n1 | [package]\n  | ^^^^^^^^^\nmissing field `name`\n",
        );
    }

    /// Tests that the suggested name for a directory cannot be extracted if no `Cargo.toml` file exists.
    #[rstest]
    fn suggest_name_from_missing_cargo_toml(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "The system cannot find the file specified. (os error 2)",
        );
    }

    /// Tests that there are no suggested packages for a non-workspace Cargo project.
    #[rstest]
    fn suggest_packages(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("Cargo.toml")
            .write_str("[package]\nname = \"magic\"")
            .unwrap();

        assert_eq!(strategy.suggest_packages(&temp_dir).unwrap().len(), 0);
    }

    /// Tests that members from a Cargo workspace are suggested as packages.
    #[rstest]
    fn suggest_packages_with_cargo_workspace(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let crate_a = temp_dir.child("crates/a");
        let crate_b = temp_dir.child("crates/b");
        let crate_c = temp_dir.child("crates/c"); // exclude = ["crates/c"]
        let crate_d = temp_dir.child("other/d");
        let crate_e = temp_dir.child("other/not-a-rust-pkg");
        crate_a.create_dir_all().unwrap();
        crate_b.create_dir_all().unwrap();
        crate_c.create_dir_all().unwrap();
        crate_d.create_dir_all().unwrap();
        crate_e.create_dir_all().unwrap();
        temp_dir.child("Cargo.toml").write_str("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\", \"other/*\"]\nexclude = [\"crates/c\"]").unwrap();
        crate_a
            .child("Cargo.toml")
            .write_str("[package]\nname = \"a\"")
            .unwrap();
        crate_b
            .child("Cargo.toml")
            .write_str("[package]\nname = \"b\"")
            .unwrap();
        crate_c
            .child("Cargo.toml")
            .write_str("[package]\nname = \"c\"")
            .unwrap();
        crate_d
            .child("Cargo.toml")
            .write_str("[package]\nname = \"d\"")
            .unwrap();
        crate_e
            .child("README.md")
            .write_str("not a Cargo package")
            .unwrap();

        let packages = strategy.suggest_packages(&temp_dir).unwrap();
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].dir, crate_a.path());
        assert_eq!(packages[1].dir, crate_b.path());
        assert_eq!(packages[2].dir, crate_d.path());
    }

    /// Tests that a new version is written to a given `Cargo.toml` file.
    #[rstest]
    fn write_version_to_cargo_toml() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        cargo_toml.write_str("[package]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n").unwrap();

        let result = RustStrategy::write_version_to_cargo_toml(&cargo_toml, &Version::new(1, 1, 0));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);
        cargo_toml.assert("[package]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version is *not* written to a Cargo workspace `Cargo.toml` file.
    #[rstest]
    fn write_version_to_cargo_toml_with_workspace() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        cargo_toml
            .write_str("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]")
            .unwrap();

        let result = RustStrategy::write_version_to_cargo_toml(&cargo_toml, &Version::new(1, 1, 0));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);
        cargo_toml.assert("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]");
    }

    /// Tests that a new version is written to a package's `Cargo.lock` file.
    #[rstest]
    fn write_version_to_cargo_lock() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("Cargo.lock");
        let crate_a = temp_dir.child("crates/a");
        crate_a.create_dir_all().unwrap();
        lockfile.write_str("# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n").unwrap();

        let result = RustStrategy::write_version_to_cargo_lock(
            &temp_dir,
            &crate_a,
            "crate-a",
            &Version::new(1, 1, 0),
        );
        assert!(result.is_ok());
        lockfile.assert("# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version for an unknown package cannot be written to a `Cargo.lock` file.
    #[rstest]
    fn write_version_to_cargo_lock_with_unknown_package() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("Cargo.lock");
        lockfile.write_str("# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"").unwrap();

        let result = RustStrategy::write_version_to_cargo_lock(
            &temp_dir,
            &temp_dir,
            "unknown",
            &Version::new(1, 1, 0),
        );
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "package `unknown` not found in `Cargo.lock`"
        );
    }

    /// Tests that the `Cargo.lock` file is found for a standalone Rust crate.
    #[rstest]
    fn find_cargo_lock() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("Cargo.toml")
            .write_str("[package]\nname = \"a\"")
            .unwrap();
        temp_dir
            .child("Cargo.lock")
            .write_str("# This file is automatically @generated by Cargo.")
            .unwrap();

        let lockfile = RustStrategy::find_cargo_lock(&temp_dir, &temp_dir);
        assert_eq!(lockfile.unwrap(), temp_dir.join("Cargo.lock"));
    }

    /// Tests that the `Cargo.lock` file is found for a Rust workspace member.
    #[rstest]
    fn find_cargo_lock_with_cargo_workspace_member() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let container = temp_dir.child("project");
        let crate_a = container.child("crates/a");
        crate_a.create_dir_all().unwrap();
        container
            .child("Cargo.toml")
            .write_str("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]")
            .unwrap();
        container
            .child("Cargo.lock")
            .write_str("# This file is automatically @generated by Cargo.")
            .unwrap();
        crate_a
            .child("Cargo.toml")
            .write_str("[package]\nname = \"a\"")
            .unwrap();

        let lockfile = RustStrategy::find_cargo_lock(&crate_a, &temp_dir);
        assert_eq!(lockfile.unwrap(), container.join("Cargo.lock"));
    }

    /// Tests that a `Cargo.lock` file is not returned when it can't be found.
    #[rstest]
    fn find_cargo_lock_when_not_found() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let crate_a = temp_dir.child("crates/a");
        crate_a.create_dir_all().unwrap();
        crate_a
            .child("Cargo.toml")
            .write_str("[package]\nname = \"a\"")
            .unwrap();

        let lockfile = RustStrategy::find_cargo_lock(&crate_a, &temp_dir);
        assert!(lockfile.is_err());
        assert!(
            lockfile
                .unwrap_err()
                .to_string()
                .starts_with("could not find a `Cargo.lock` file for workspace member")
        );
    }

    /// Tests that `Cargo.lock` files are only searched for within the current working directory.
    #[rstest]
    fn find_cargo_lock_does_not_escape_cwd() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let project = temp_dir.child("projects");
        let crate_a = project.child("crates/a");
        crate_a.create_dir_all().unwrap();
        temp_dir
            .child("Cargo.lock")
            .write_str("# This file is automatically @generated by Cargo.")
            .unwrap();
        crate_a
            .child("Cargo.toml")
            .write_str("[package]\nname = \"a\"")
            .unwrap();

        let lockfile = RustStrategy::find_cargo_lock(&crate_a, &project); // DO NOT escape project path
        assert!(lockfile.is_err());
        assert!(
            lockfile
                .unwrap_err()
                .to_string()
                .starts_with("could not find a `Cargo.lock` file for workspace member")
        );
    }
}
