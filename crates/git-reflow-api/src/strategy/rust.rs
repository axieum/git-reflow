use crate::settings::{AppConfig, pkg::PackageConfig};
use crate::strategy::{BaseStrategy, Strategy};
use anyhow::{Context, anyhow, bail};
use semver::Version;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;
use toml_edit::{DocumentMut, Item};
use tracing::{debug, trace};

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
        // Update the `Cargo.toml` file.
        let cargo_toml_path = config.dir.join("Cargo.toml");
        if Self::write_version_to_cargo_toml(&cargo_toml_path, new_version, dry_run)? {
            // Update the `Cargo.lock` file.
            let cargo_lock_path = Self::write_version_to_cargo_lock(
                &std::env::current_dir()?,
                &config.dir,
                config.name(),
                new_version,
                dry_run,
            )?;
            return Ok(vec![cargo_toml_path, cargo_lock_path]);
        }
        Ok(vec![])
    }

    /// Writes new dependency versions to the `Cargo.toml` and `Cargo.lock` files.
    ///
    /// Dependencies without an explicit version are left unchanged.
    ///
    /// # Arguments
    ///
    /// * `versions` - A map of dependency names to their new versions.
    /// * `config` - The package configuration.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result containing a list of the changed file paths.
    fn write_dependency_versions(
        &self,
        versions: &HashMap<String, Version>,
        config: &PackageConfig,
        dry_run: bool,
    ) -> anyhow::Result<Vec<PathBuf>> {
        // Short-circuit when there are no dependency versions to update.
        if versions.is_empty() {
            return Ok(vec![]);
        }

        // Update the `Cargo.toml` file.
        let cargo_toml_path = config.dir.join("Cargo.toml");
        if Self::write_dependency_versions_to_cargo_toml(&cargo_toml_path, versions, dry_run)? {
            // Update the `Cargo.lock` file.
            let cargo_lock_path = Self::write_dependency_versions_to_cargo_lock(
                &std::env::current_dir()?,
                &config.dir,
                versions,
                dry_run,
            )?;
            return Ok(vec![cargo_toml_path, cargo_lock_path]);
        }

        Ok(vec![])
    }

    /// Suggests the package name from the `[package.name]` field of the `Cargo.toml` file.
    ///
    /// For a Cargo workspace, it will return the directory name.
    ///
    /// # Arguments
    ///
    /// * `dir` - The package directory to get the name for.
    ///
    /// # Returns
    ///
    /// The package name for the given Rust project.
    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String> {
        // Parse the `Cargo.toml` file.
        let filename = &dir.join("Cargo.toml");
        let contents = fs::read_to_string(filename)?;
        let data: CargoToml = toml::from_str(&contents)?;

        // Return the package name from the `[package.name]` field or fallback to the directory name.
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
            .map_err(|err| anyhow!("could not find package name in `{}`: {err}", filename.display()))
    }

    /// Suggests Cargo workspace members as package configurations that should be included.
    ///
    /// # Arguments
    ///
    /// * `dir` - The directory to a *possible* Cargo workspace.
    ///
    /// # Returns
    ///
    /// The package configurations of each Cargo workspace member if any.
    fn suggest_packages(&self, dir: &Path) -> anyhow::Result<Vec<PackageConfig>> {
        // Parse the `Cargo.toml` file.
        let filename = &dir.join("Cargo.toml");
        if !filename.is_file() {
            return Ok(vec![]);
        }
        let contents =
            fs::read_to_string(filename).map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let data: Value =
            toml::from_str(&contents).map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        let workspace = data.get("workspace");

        // Collect the workspace member globs.
        let members = workspace
            .and_then(|workspace| workspace.get("members"))
            .and_then(|members| members.as_array())
            .into_iter()
            .flatten()
            .filter_map(|member| member.as_str())
            .collect::<Vec<_>>();

        // Collect the workspace exclude globs and expand them to paths.
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

        // Return package configurations for each in-scope workspace member.
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
            // Filter out any excluded paths.
            .filter(|path| !exclude.contains(path))
            // Filter out any paths that do not contain a `Cargo.toml` file.
            .filter(|path| path.join("Cargo.toml").try_exists().unwrap_or(false))
            .map(|path| {
                PackageConfig {
                    dir: path,
                    strategy: Some(Strategy::Rust(RustStrategy::default())),
                    make_latest: false,
                    ..Default::default()
                }
                .apply_defaults()
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }

    /// Finds the local dependencies of a Rust crate that are affected by:
    ///
    ///   * Checking `[dependencies]`;
    ///   * Checking `[build-dependencies]`;
    ///   * Excluding `[dev-dependencies]` because they should not trigger a release.
    ///
    /// # Arguments
    ///
    /// * `config` - The app configuration.
    /// * `pkg_config` - The package configuration to find dependencies for.
    ///
    /// # Returns
    ///
    /// A result containing a list of dependency crate names, if any.
    fn find_dependencies(&self, config: &AppConfig, pkg_config: &PackageConfig) -> anyhow::Result<Vec<String>> {
        // Parse the `Cargo.toml` file of the crate.
        let cargo_toml_path = pkg_config.dir.join("Cargo.toml");
        let contents = fs::read_to_string(&cargo_toml_path)
            .map_err(|err| anyhow!("could not read `{}`: {err}", cargo_toml_path.display()))?;
        let data: Value = toml::from_str(&contents)
            .map_err(|err| anyhow!("could not parse `{}`: {err}", cargo_toml_path.display()))?;

        // Build a set of all dependency names.
        let dependency_names: HashSet<&str> = ["dependencies", "build-dependencies"]
            .iter()
            .filter_map(|key| data.get(*key)?.as_table())
            .flat_map(|dependencies| dependencies.keys().map(String::as_str))
            .collect();

        // Return the dependency names that are also configured as packages.
        Ok(config
            .packages
            .iter()
            .filter(|pkg| dependency_names.contains(pkg.name()))
            .map(|pkg| pkg.name().to_string())
            .collect())
    }
}

impl RustStrategy {
    /// Writes a new version to the given `Cargo.toml` file.
    ///
    /// # Arguments
    ///
    /// * `filename` - The path to the `Cargo.toml` file.
    /// * `new_version` - The new version to apply.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result of whether an update was made.
    fn write_version_to_cargo_toml(filename: &Path, new_version: &Version, dry_run: bool) -> anyhow::Result<bool> {
        // Parse the `Cargo.toml` file.
        let filename_display = filename.display();
        let contents =
            fs::read_to_string(filename).map_err(|err| anyhow!("could not read `{filename_display}`: {err}"))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{filename_display}`: {err}"))?;

        // Update the `[package.version]` field if it exists.
        if let Some(package) = data["package"].as_table_mut() {
            if !dry_run {
                debug!("set `[package.version]` to `{new_version}` at `{filename_display}`");
                package["version"] = toml_edit::value(new_version.to_string());
                fs::write(filename, data.to_string())
                    .map_err(|err| anyhow!("failed to write `[package.version]` to `{filename_display}`: {err}"))?;
            } else {
                debug!("set `[package.version]` to `{new_version}` at `{filename_display}` (dry run)");
            }
            Ok(true)
        } else {
            debug!("refusing to write `[package.version]` to Cargo workspace at `{filename_display}`");
            Ok(false)
        }
    }

    /// Writes a new version for a crate to the `Cargo.lock` file.
    ///
    /// It will find the closest `Cargo.lock` file from a given package directory upwards.
    ///
    /// # Arguments
    ///
    /// * `root` - The root directory.
    /// * `dir` - The package directory.
    /// * `name` - The name of the package to update.
    /// * `new_version` - The new version to apply.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result containing the path to the updated `Cargo.lock` file.
    ///
    /// # See Also
    ///
    /// * [`Self::find_cargo_lock()`] - For finding the closest `Cargo.lock` file.
    fn write_version_to_cargo_lock(
        root: &Path,
        dir: &Path,
        name: &str,
        new_version: &Version,
        dry_run: bool,
    ) -> anyhow::Result<PathBuf> {
        // Parse the `Cargo.lock` file.
        let lockfile = Self::find_cargo_lock(dir, root)?;
        let lockfile_display = lockfile.strip_prefix(root)?.display();
        let contents =
            fs::read_to_string(&lockfile).map_err(|err| anyhow!("could not read `{lockfile_display}`: {err}"))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{lockfile_display}`: {err}"))?;

        // Find the `[[package]]` table for the given package name.
        let package = data
            .get_mut("package")
            .and_then(Item::as_array_of_tables_mut)
            .and_then(|packages| {
                packages
                    .iter_mut()
                    .find(|pkg| pkg.get("name").and_then(Item::as_str) == Some(name))
            })
            .context(format!("package `{name}` not found in `{lockfile_display}`"))?;

        // Update the `version` field in the found package table.
        if !dry_run {
            debug!("set `[[package]].version` to `{new_version}` for `{name}` at `{lockfile_display}`");
            package["version"] = toml_edit::value(new_version.to_string());
            fs::write(&lockfile, data.to_string())
                .map_err(|err| anyhow!("failed to write `[[package]].version` to `{lockfile_display}`: {err}"))?;
        } else {
            debug!("set `[[package]].version` to `{new_version}` for `{name}` at `{lockfile_display}` (dry run)");
        }

        Ok(lockfile)
    }

    /// Writes new dependency versions to the given `Cargo.toml` file.
    ///
    /// # Arguments
    ///
    /// * `filename` - The path to the `Cargo.toml` file.
    /// * `versions` - A map of dependency names to their new versions.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result of whether any updates were made.
    fn write_dependency_versions_to_cargo_toml(
        filename: &Path,
        versions: &HashMap<String, Version>,
        dry_run: bool,
    ) -> anyhow::Result<bool> {
        // Parse the `Cargo.toml` file.
        let contents =
            fs::read_to_string(filename).map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        // Update the dependency versions in the `Cargo.toml` file and write it back if any changes were made.
        let changed = Self::update_dependency_tables(data.as_table_mut(), versions, filename);
        if changed {
            if !dry_run {
                debug!("write dependency versions to `{}`", filename.display());
                fs::write(filename, data.to_string())
                    .map_err(|err| anyhow!("failed to write dependency versions to `{}`: {err}", filename.display()))?;
            } else {
                debug!("write dependency versions to `{}` (dry run)", filename.display());
            }
        }

        Ok(changed)
    }

    /// Writes new dependency versions to the given `Cargo.lock` file.
    ///
    /// # Arguments
    ///
    /// * `root` - The root directory.
    /// * `dir` - The package directory.
    /// * `versions` - A map of dependency names to their new versions.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result containing the path to the updated `Cargo.lock` file.
    fn write_dependency_versions_to_cargo_lock(
        root: &Path,
        dir: &Path,
        versions: &HashMap<String, Version>,
        dry_run: bool,
    ) -> anyhow::Result<PathBuf> {
        // Parse the `Cargo.lock` file.
        let lockfile = Self::find_cargo_lock(dir, root)?;
        let lockfile_display = lockfile.strip_prefix(root)?.display();
        let contents =
            fs::read_to_string(&lockfile).map_err(|err| anyhow!("could not read `{lockfile_display}`: {err}"))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{lockfile_display}`: {err}"))?;

        // Update the dependency versions in the `Cargo.lock` file.
        let mut changed = false;
        if let Some(packages) = data.get_mut("package").and_then(Item::as_array_of_tables_mut) {
            for package in packages.iter_mut() {
                // Check if the dependency has a version to be updated.
                let Some(version) = package
                    .get("name")
                    .and_then(Item::as_str)
                    .and_then(|name| versions.get(name))
                else {
                    continue;
                };

                // Update the dependency to the new version.
                let new_version = version.to_string();
                if package.get("version").and_then(Item::as_str) != Some(new_version.as_str()) {
                    debug!("set lockfile dependency version to `{version}` at `{lockfile_display}`");
                    package["version"] = toml_edit::value(new_version);
                    changed = true;
                }
            }
        }

        // Write the `Cargo.lock` file back if any changes were made.
        if changed {
            if !dry_run {
                debug!("write dependency versions to `{lockfile_display}`");
                fs::write(&lockfile, data.to_string())
                    .map_err(|err| anyhow!("failed to write dependency versions to `{lockfile_display}`: {err}"))?;
            } else {
                debug!("write dependency versions to `{lockfile_display}` (dry run)");
            }
        }

        Ok(lockfile)
    }

    /// Updates dependency versions in nested `Cargo.toml` tables.
    ///
    /// # Arguments
    ///
    /// * `table` - The TOML table to update.
    /// * `versions` - A map of dependency names to their new versions.
    /// * `filename` - The path to the TOML file being updated (for logging).
    ///
    /// # Returns
    ///
    /// Whether any dependency versions were updated in the table.
    fn update_dependency_tables(
        table: &mut dyn toml_edit::TableLike,
        versions: &HashMap<String, Version>,
        filename: &Path,
    ) -> bool {
        let mut changed = false;
        for (key, item) in table.iter_mut() {
            // Descend through workspace and target tables until a dependency table is reached.
            let Some(table) = item.as_table_like_mut() else {
                continue;
            };
            if !matches!(key.get(), "dependencies" | "dev-dependencies" | "build-dependencies") {
                changed |= Self::update_dependency_tables(table, versions, filename);
                continue;
            }

            // For each dependency, check if it is in scope and update its version if necessary.
            for (name, dependency) in table.iter_mut() {
                // Check if the dependency has a version to be updated.
                let package_name = dependency.get("package").and_then(Item::as_str).unwrap_or(name.get());
                let Some(version) = versions.get(package_name) else {
                    continue;
                };

                // If the dependency is a string, update it directly, otherwise update the `version` field in the table.
                let version_item = if dependency.is_str() {
                    Some(&mut *dependency)
                } else {
                    dependency.get_mut("version")
                };

                // Skip dependencies that do not specify a version, i.e. are path-only or workspace-inherited.
                let Some(value) = version_item.and_then(Item::as_value_mut).filter(|value| value.is_str()) else {
                    trace!("skip dependency `{name}` without a version at `{}`", filename.display());
                    continue;
                };

                // Update the dependency to the new version.
                let new_version = version.to_string();
                if value.as_str() != Some(new_version.as_str()) {
                    debug!(
                        "set dependency `{name}` version to `{version}` at `{}`",
                        filename.display(),
                    );
                    let mut replacement = toml_edit::Value::from(new_version);
                    *replacement.decor_mut() = value.decor().clone();
                    *value = replacement;
                    changed = true;
                }
            }
        }

        changed
    }

    /// Finds the closest `Cargo.lock` file from a given package directory upwards.
    ///
    /// This will not traverse any further than the given `relative` directory.
    ///
    /// # Arguments
    ///
    /// * `dir` - The package directory.
    /// * `relative` - The relative directory to scope the search to, e.g. [`std::env::current_dir()`].
    ///
    /// # Returns
    ///
    /// A result containing the `Cargo.lock` path if found.
    pub fn find_cargo_lock(dir: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
        let mut dir = relative.join(dir);

        // Traverse up until we find `Cargo.lock` or reach `cwd`.
        while dir.starts_with(relative) {
            // Check if `Cargo.lock` exists at this level.
            let lockfile = dir.join("Cargo.lock");
            if lockfile.is_file() {
                return Ok(lockfile);
            }
            // Move to the parent directory.
            if !dir.pop() {
                break;
            }
        }

        bail!(
            "could not find a `Cargo.lock` file for workspace member `{}`",
            dir.display(),
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
    use indoc::indoc;
    use rstest::{fixture, rstest};

    /// A test fixture for a default Rust strategy.
    #[fixture]
    fn strategy() -> RustStrategy {
        RustStrategy::default()
    }

    /// Tests that the suggested name for a directory is extracted from the `Cargo.toml`.
    #[rstest]
    fn test_suggest_name_from_cargo_toml(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("Cargo.toml")
            .write_str("[package]\nname = \"magic\"")
            .unwrap();

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "magic");
    }

    /// Tests that the suggested name for a directory is the folder when `Cargo.toml` is workspace.
    #[rstest]
    fn test_suggest_name_from_cargo_toml_workspace(strategy: RustStrategy) {
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
    fn test_suggest_name_from_malformed_cargo_toml(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("Cargo.toml").write_str("[package]").unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "TOML parse error at line 1, column 1\n  |\n1 | [package]\n  | ^^^^^^^^^\nmissing field `name`\n",
        );
    }

    /// Tests that the suggested name for a directory cannot be extracted if no `Cargo.toml` file exists.
    #[rstest]
    fn test_suggest_name_from_missing_cargo_toml(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();

        let err = strategy.suggest_name(&temp_dir).unwrap_err();
        let io_err = err.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
    }

    /// Tests that there are no suggested packages for a non-workspace Cargo project.
    #[rstest]
    fn test_suggest_packages(strategy: RustStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("Cargo.toml")
            .write_str("[package]\nname = \"magic\"")
            .unwrap();

        assert_eq!(strategy.suggest_packages(&temp_dir).unwrap().len(), 0);
    }

    /// Tests that members from a Cargo workspace are suggested as packages.
    #[rstest]
    fn test_suggest_packages_with_cargo_workspace(strategy: RustStrategy) {
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
        temp_dir
            .child("Cargo.toml")
            .write_str("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\", \"other/*\"]\nexclude = [\"crates/c\"]")
            .unwrap();
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
        crate_e.child("README.md").write_str("not a Cargo package").unwrap();

        let packages = strategy.suggest_packages(&temp_dir).unwrap();
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].dir, crate_a.path());
        assert_eq!(packages[1].dir, crate_b.path());
        assert_eq!(packages[2].dir, crate_d.path());
    }

    /// Tests that a new version is written to a given `Cargo.toml` file.
    #[rstest]
    fn test_write_version_to_cargo_toml() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        cargo_toml
            .write_str("[package]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n")
            .unwrap();

        let result = RustStrategy::write_version_to_cargo_toml(&cargo_toml, &Version::new(1, 1, 0), false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);
        cargo_toml.assert("[package]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version is *not* written to a Cargo workspace `Cargo.toml` file.
    #[rstest]
    fn test_write_version_to_cargo_toml_with_workspace() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        cargo_toml
            .write_str("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]")
            .unwrap();

        let result = RustStrategy::write_version_to_cargo_toml(&cargo_toml, &Version::new(1, 1, 0), false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);
        cargo_toml.assert("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]");
    }

    /// Tests that a new version is *not* written to a given `Cargo.toml` file when performing a dry run.
    #[rstest]
    fn test_write_version_to_cargo_toml_with_dry_run() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        cargo_toml
            .write_str("[package]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n")
            .unwrap();

        let result = RustStrategy::write_version_to_cargo_toml(&cargo_toml, &Version::new(1, 1, 0), true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), true);
        // The version should not be updated because this is a dry run.
        cargo_toml.assert("[package]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n");
    }

    /// Tests that a new version is written to a package's `Cargo.lock` file.
    #[rstest]
    fn test_write_version_to_cargo_lock() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("Cargo.lock");
        let crate_a = temp_dir.child("crates/a");
        crate_a.create_dir_all().unwrap();
        lockfile.write_str("# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n").unwrap();

        let result =
            RustStrategy::write_version_to_cargo_lock(&temp_dir, &crate_a, "crate-a", &Version::new(1, 1, 0), false);
        assert!(result.is_ok());
        lockfile.assert(
            "# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.1.0\"\n",
        );
    }

    /// Tests that a new version for an unknown package cannot be written to a `Cargo.lock` file.
    #[rstest]
    fn test_write_version_to_cargo_lock_with_unknown_package() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("Cargo.lock");
        lockfile.write_str("# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"").unwrap();

        let result =
            RustStrategy::write_version_to_cargo_lock(&temp_dir, &temp_dir, "unknown", &Version::new(1, 1, 0), false);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "package `unknown` not found in `Cargo.lock`"
        );
    }

    /// Tests that a new version is *not* written to a package's `Cargo.lock` file when performing a dry run.
    #[rstest]
    fn test_write_version_to_cargo_lock_with_dry_run() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("Cargo.lock");
        let crate_a = temp_dir.child("crates/a");
        crate_a.create_dir_all().unwrap();
        lockfile.write_str("# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n").unwrap();

        let result =
            RustStrategy::write_version_to_cargo_lock(&temp_dir, &crate_a, "crate-a", &Version::new(1, 1, 0), true);
        assert!(result.is_ok());
        // The version should not be updated because this is a dry run.
        lockfile.assert(
            "# This file is automatically @generated by Cargo.\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n",
        );
    }

    /// Tests that new dependency versions are written to the `Cargo.toml` and `Cargo.lock` files.
    #[rstest]
    fn test_write_dependency_versions(strategy: RustStrategy) {
        // Create a Rust crate with some local dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        cargo_toml
            .write_str(indoc! {r#"
                [package]
                name = "magic"
                version = "1.0.0"

                [dependencies]
                api = { path = "../api" }
                core = { path = "../core", version = "1.0.0" }

                [build-dependencies]
                build = "^1.0.0"
                "#})
            .unwrap();
        let cargo_lock = temp_dir.child("Cargo.lock");
        cargo_lock
            .write_str(indoc! {r#"
                # This file is automatically @generated by Cargo.
                # It is not intended for manual editing.
                version = 4

                [[package]]
                name = "api"
                version = "1.0.0"

                [[package]]
                name = "build"
                version = "1.0.0"

                [[package]]
                name = "core"
                version = "1.0.0"
                dependencies = [
                 "example-api",
                ]
            "#})
            .unwrap();

        // Build a package configuration for the crate.
        let config = PackageConfig {
            dir: temp_dir.path().to_path_buf(),
            name: Some("magic".to_string()),
            ..Default::default()
        };

        // Write new dependency versions.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("build".to_string(), Version::new(1, 2, 0)),
            ("core".to_string(), Version::new(1, 1, 0)),
        ]);
        let result = strategy.write_dependency_versions(&versions, &config, false);

        // Verify that the new versions were written correctly.
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec![cargo_toml.to_path_buf(), cargo_lock.to_path_buf()]
        );
        cargo_toml.assert(indoc! {r#"
            [package]
            name = "magic"
            version = "1.0.0"

            [dependencies]
            api = { path = "../api" }
            core = { path = "../core", version = "1.1.0" }

            [build-dependencies]
            build = "1.2.0"
            "#});
        cargo_lock.assert(indoc! {r#"
            # This file is automatically @generated by Cargo.
            # It is not intended for manual editing.
            version = 4

            [[package]]
            name = "api"
            version = "1.1.0"

            [[package]]
            name = "build"
            version = "1.2.0"

            [[package]]
            name = "core"
            version = "1.1.0"
            dependencies = [
             "example-api",
            ]
        "#});
    }

    /// Tests that new dependency versions are *not* written when performing a dry run.
    #[rstest]
    fn test_write_dependency_versions_with_dry_run(strategy: RustStrategy) {
        // Create a Rust crate with some local dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        let cargo_toml_contents = indoc! {r#"
            [package]
            name = "magic"
            version = "1.0.0"

            [dependencies]
            api = { path = "../api" }
            core = { path = "../core", version = "1.0.0" }

            [build-dependencies]
            build = "^1.0.0"
            "#};
        cargo_toml.write_str(cargo_toml_contents).unwrap();
        let cargo_lock = temp_dir.child("Cargo.lock");
        let cargo_lock_contents = indoc! {r#"
            # This file is automatically @generated by Cargo.
            # It is not intended for manual editing.
            version = 4

            [[package]]
            name = "api"
            version = "1.0.0"

            [[package]]
            name = "build"
            version = "1.0.0"

            [[package]]
            name = "core"
            version = "1.0.0"
            dependencies = [
             "example-api",
            ]
            "#};
        cargo_lock.write_str(cargo_lock_contents).unwrap();

        // Build a package configuration for the crate.
        let config = PackageConfig {
            dir: temp_dir.path().to_path_buf(),
            name: Some("magic".to_string()),
            ..Default::default()
        };

        // Write new dependency versions.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("build".to_string(), Version::new(1, 2, 0)),
            ("core".to_string(), Version::new(1, 1, 0)),
        ]);
        let result = strategy.write_dependency_versions(&versions, &config, true);

        // Verify that the new versions were reported as written, but the files were not actually modified.
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec![cargo_toml.to_path_buf(), cargo_lock.to_path_buf()]
        );
        cargo_toml.assert(cargo_toml_contents);
        cargo_lock.assert(cargo_lock_contents);
    }

    /// Tests that an empty map of dependency versions causes a short-circuit and no files are written.
    #[rstest]
    fn test_write_dependency_versions_with_no_dependencies(strategy: RustStrategy) {
        // Create a Rust crate.
        let temp_dir = assert_fs::TempDir::new().unwrap();

        // Build a package configuration for the crate.
        let config = PackageConfig {
            dir: temp_dir.path().to_path_buf(),
            name: Some("magic".to_string()),
            ..Default::default()
        };

        // Write new dependency versions with an empty map.
        let result = strategy.write_dependency_versions(&HashMap::new(), &config, false);

        // Verify that no files were written.
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    /// Tests that new dependency versions are written to the `Cargo.toml` file.
    #[rstest]
    fn test_write_dependency_versions_to_cargo_toml() {
        // Create a Rust crate with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        let contents = indoc! {r#"
            [package]
            name = "magic"
            version = "1.0.0"

            [dependencies]
            api = { path = "../api" }
            core = { path = "../core", version = "1.0.0", features = ["extra"] }
            other = "1.0.0"
            inherited = { workspace = true }
            alias = { package = "renamed", version = "1.0.0" }

            [dev-dependencies]
            test = "1.0.0"

            [build-dependencies]
            build = "^1.0.0"

            [target.'cfg(unix)'.dependencies]
            core = "1.0.0"

            [workspace.dependencies]
            core = { version = "1.0.0", path = "../core" }

            [dependencies.expanded]
            version = "1.0.0" # Keep this comment.
            path = "../expanded"
            "#};
        cargo_toml.write_str(contents).unwrap();

        // Write new dependency versions.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("inherited".to_string(), Version::new(1, 1, 0)),
            ("renamed".to_string(), Version::new(2, 0, 0)),
            ("test".to_string(), Version::new(1, 3, 0)),
            ("build".to_string(), Version::new(1, 4, 0)),
            ("expanded".to_string(), Version::new(1, 5, 0)),
        ]);
        let result = RustStrategy::write_dependency_versions_to_cargo_toml(&cargo_toml, &versions, false);

        // Verify that only matching, explicitly versioned dependencies were changed.
        assert!(result.is_ok());
        assert!(result.unwrap());
        cargo_toml.assert(indoc! {r#"
            [package]
            name = "magic"
            version = "1.0.0"

            [dependencies]
            api = { path = "../api" }
            core = { path = "../core", version = "1.2.0", features = ["extra"] }
            other = "1.0.0"
            inherited = { workspace = true }
            alias = { package = "renamed", version = "2.0.0" }

            [dev-dependencies]
            test = "1.3.0"

            [build-dependencies]
            build = "1.4.0"

            [target.'cfg(unix)'.dependencies]
            core = "1.2.0"

            [workspace.dependencies]
            core = { version = "1.2.0", path = "../core" }

            [dependencies.expanded]
            version = "1.5.0" # Keep this comment.
            path = "../expanded"
            "#});

        // Reapplying persisted versions must not report any further manifest changes.
        assert!(!RustStrategy::write_dependency_versions_to_cargo_toml(&cargo_toml, &versions, false).unwrap());
    }

    /// Tests that new dependency versions are *not* written to `Cargo.toml` when performing a dry run.
    #[rstest]
    fn test_write_dependency_versions_to_cargo_toml_with_dry_run() {
        // Create a Rust crate with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let cargo_toml = temp_dir.child("Cargo.toml");
        let contents = indoc! {r#"
            [package]
            name = "magic"
            version = "1.0.0"

            [dependencies]
            api = { path = "../api" }
            core = { path = "../core", version = "1.0.0", features = ["extra"] }
            other = "1.0.0"
            inherited = { workspace = true }
            alias = { package = "renamed", version = "1.0.0" }

            [dev-dependencies]
            test = "1.0.0"

            [build-dependencies]
            build = "^1.0.0"

            [target.'cfg(unix)'.dependencies]
            core = "1.0.0"

            [workspace.dependencies]
            core = { version = "1.0.0", path = "../core" }

            [dependencies.expanded]
            version = "1.0.0" # Keep this comment.
            path = "../expanded"
            "#};
        cargo_toml.write_str(contents).unwrap();

        // Write new dependency versions with dry run.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("inherited".to_string(), Version::new(1, 1, 0)),
            ("renamed".to_string(), Version::new(2, 0, 0)),
            ("test".to_string(), Version::new(1, 3, 0)),
            ("build".to_string(), Version::new(1, 4, 0)),
            ("expanded".to_string(), Version::new(1, 5, 0)),
        ]);
        let result = RustStrategy::write_dependency_versions_to_cargo_toml(&cargo_toml, &versions, true);

        // Verify that the file was reported as changed, but the contents were not actually modified.
        assert!(result.is_ok());
        assert!(result.unwrap());
        cargo_toml.assert(contents);
    }

    /// Tests that new dependency versions are written to the `Cargo.lock` file.
    #[rstest]
    fn test_write_dependency_versions_to_cargo_lock() {
        // Create a Rust workspace with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let crate_dir = temp_dir.child("crates/magic");
        crate_dir.create_dir_all().unwrap();
        let cargo_lock = temp_dir.child("Cargo.lock");
        let contents = indoc! {r#"
            # This file is automatically @generated by Cargo.
            # It is not intended for manual editing.
            version = 4

            [[package]]
            name = "api"
            version = "1.0.0"

            [[package]]
            name = "core"
            version = "1.0.0"
            dependencies = [
             "api",
            ]

            [[package]]
            name = "other"
            version = "1.0.0"
            "#};
        cargo_lock.write_str(contents).unwrap();

        // Write new dependency versions.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("missing".to_string(), Version::new(2, 0, 0)),
        ]);
        let result = RustStrategy::write_dependency_versions_to_cargo_lock(&temp_dir, &crate_dir, &versions, false);

        // Verify that the new dependency versions were written correctly.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), cargo_lock.to_path_buf());
        cargo_lock.assert(indoc! {r#"
            # This file is automatically @generated by Cargo.
            # It is not intended for manual editing.
            version = 4

            [[package]]
            name = "api"
            version = "1.1.0"

            [[package]]
            name = "core"
            version = "1.2.0"
            dependencies = [
             "api",
            ]

            [[package]]
            name = "other"
            version = "1.0.0"
            "#});
    }

    /// Tests that new dependency versions are *not* written to `Cargo.lock` when performing a dry run.
    #[rstest]
    fn test_write_dependency_versions_to_cargo_lock_with_dry_run() {
        // Create a Rust workspace with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let crate_dir = temp_dir.child("crates/magic");
        crate_dir.create_dir_all().unwrap();
        let cargo_lock = temp_dir.child("Cargo.lock");
        let contents = indoc! {r#"
            # This file is automatically @generated by Cargo.
            # It is not intended for manual editing.
            version = 4

            [[package]]
            name = "api"
            version = "1.0.0"

            [[package]]
            name = "core"
            version = "1.0.0"
            dependencies = [
             "api",
            ]

            [[package]]
            name = "other"
            version = "1.0.0"
            "#};
        cargo_lock.write_str(contents).unwrap();

        // Write new dependency versions with dry run.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("missing".to_string(), Version::new(2, 0, 0)),
        ]);
        let result = RustStrategy::write_dependency_versions_to_cargo_lock(&temp_dir, &crate_dir, &versions, true);

        // Verify that the file was reported as changed, but the contents were not actually modified.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), cargo_lock.to_path_buf());
        cargo_lock.assert(contents);
    }

    /// Tests that dependencies of a Rust crate are found correctly.
    #[rstest]
    #[case::runtime("[dependencies]\nmy-package = { path = '../my-package' }", true)]
    #[case::build("[build-dependencies]\nmy-package = { path = '../my-package' }", true)]
    #[case::optional("[dependencies]\nmy-package = { path = '../my-package', optional = true }", true)]
    #[case::dev("[dev-dependencies]\nmy-package = { path = '../my-package' }", false)]
    #[case::empty("[package]\nname = 'my-api'", false)]
    #[case::out_of_scope("[dependencies]\nother-package = '1.0'", false)]
    fn test_find_dependencies(strategy: RustStrategy, #[case] manifest: &str, #[case] expected: bool) {
        // Create a Rust workspace with some crates and local dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let package = temp_dir.child("crates/my-api");
        package.create_dir_all().unwrap();
        package.child("Cargo.toml").write_str(manifest).unwrap();

        // Create an app configuration for the workspace.
        let config = AppConfig {
            packages: vec![
                PackageConfig {
                    dir: package.path().to_path_buf(),
                    name: Some("my-api".to_string()),
                    ..Default::default()
                },
                PackageConfig {
                    name: Some("my-package".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // Find dependencies of `my-api`.
        let dependencies = strategy.find_dependencies(&config, &config.packages[0]).unwrap();
        assert_eq!(dependencies, if expected { vec!["my-package"] } else { vec![] });
    }

    /// Tests that the `Cargo.lock` file is found for a standalone Rust crate.
    #[rstest]
    fn test_find_cargo_lock() {
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
    fn test_find_cargo_lock_with_cargo_workspace_member() {
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
    fn test_find_cargo_lock_when_not_found() {
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
    fn test_find_cargo_lock_does_not_escape_cwd() {
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
