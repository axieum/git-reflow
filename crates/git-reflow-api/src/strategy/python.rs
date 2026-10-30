use crate::settings::{AppConfig, pkg::PackageConfig};
use crate::strategy::{BaseStrategy, Strategy};
use anyhow::{Context, anyhow, bail};
use regex::Regex;
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;
use toml_edit::{DocumentMut, Item};
use tracing::{debug, trace};

/// The [Python](https://www.python.org/) release strategy.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PythonStrategy {
    // empty
}

impl BaseStrategy for PythonStrategy {
    /// Writes a new version to the `pyproject.toml` and `uv.lock` files.
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
        // Update the `pyproject.toml` file.
        let pyproject_toml_path = config.dir.join("pyproject.toml");
        if Self::write_version_to_pyproject_toml(&pyproject_toml_path, new_version, dry_run)? {
            // Update the `uv.lock` file.
            let uv_lock_path = Self::write_version_to_uv_lock(
                &std::env::current_dir()?,
                &config.dir,
                config.name(),
                new_version,
                dry_run,
            )?;
            return Ok(vec![pyproject_toml_path, uv_lock_path]);
        }
        Ok(vec![])
    }

    /// Writes new dependency versions to the `pyproject.toml` and `uv.lock` files.
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

        // Update the `pyproject.toml` file.
        let pyproject_toml_path = config.dir.join("pyproject.toml");
        if pyproject_toml_path.try_exists()? {
            if Self::write_dependency_versions_to_pyproject_toml(&pyproject_toml_path, versions, dry_run)? {
                // Update the `uv.lock` file.
                let uv_lock_path = Self::write_dependency_versions_to_uv_lock(
                    &std::env::current_dir()?,
                    &config.dir,
                    versions,
                    dry_run,
                )?;
                return Ok(vec![pyproject_toml_path, uv_lock_path]);
            }
        } else {
            debug!(
                "a `pyproject.toml` file was not found at `{}`, skipping dependencies",
                pyproject_toml_path.display(),
            );
        }

        Ok(vec![])
    }

    /// Suggests the package name from either the `pyproject.toml` file.
    ///
    /// It will check for the package name in the following order:
    ///
    ///   1. `project.name`
    ///
    /// # Arguments
    ///
    /// * `dir` - The package directory to get the name for.
    ///
    /// # Returns
    ///
    /// The package name for the given Python project.
    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String> {
        // Try `pyproject.toml`.
        let pyproject = &dir.join("pyproject.toml");
        if pyproject.try_exists()?
            && let Some(name) = Self::suggest_name_from_pyproject_toml(pyproject)?
        {
            return Ok(name);
        }

        // Unsupported file.
        Err(anyhow!("could not determine name for package"))
    }

    /// Suggests Python workspace members as package configurations that should be included.
    ///
    /// # Arguments
    ///
    /// * `dir` - The directory to a *possible* Python workspace.
    ///
    /// # Returns
    ///
    /// The package configurations of each Python workspace member if any.
    fn suggest_packages(&self, dir: &Path) -> anyhow::Result<Vec<PackageConfig>> {
        // Parse the `pyproject.toml` file.
        let filename = &dir.join("pyproject.toml");
        if !filename.is_file() {
            return Ok(vec![]);
        }
        let contents =
            fs::read_to_string(filename).map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let data: Value =
            toml::from_str(&contents).map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        // Try `uv` workspace.
        if let Some(workspace) = data
            .get("tool")
            .and_then(|tool| tool.get("uv"))
            .and_then(|uv| uv.get("workspace"))
        {
            return Self::suggest_packages_from_uv_workspace(dir, workspace);
        }

        Ok(vec![])
    }

    /// Finds the local dependencies of a Python package that are affected by:
    ///
    ///   * Checking `[project.dependencies]`;
    ///   * Checking `[project.optional-dependencies]`;
    ///   * Checking `[build-system.requires]`;
    ///   * Excluding `[dependency-groups]` because they should not trigger a release.
    ///
    /// # Arguments
    ///
    /// * `config` - The app configuration.
    /// * `pkg_config` - The package configuration to find dependencies for.
    ///
    /// # Returns
    ///
    /// A result containing a list of dependency package names, if any.
    fn find_dependencies(&self, config: &AppConfig, pkg_config: &PackageConfig) -> anyhow::Result<Vec<String>> {
        // Parse the `pyproject.toml` file.
        let filename = pkg_config.dir.join("pyproject.toml");
        if !filename.is_file() {
            debug!(
                "a `pyproject.toml` file was not found at `{}`, skipping",
                filename.display(),
            );
            return Ok(vec![]);
        }
        let contents =
            fs::read_to_string(&filename).map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let data: Value =
            toml::from_str(&contents).map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        // Build a set of all dependency names.
        let requirement_name = Regex::new(r"(?i)^\s*([a-z0-9][a-z0-9._-]*)(?:\s*(?:\[|[<>=!~(;@])|$)")?;
        let dependencies = [("project", "dependencies"), ("build-system", "requires")]
            .iter()
            .filter_map(|(section, key)| data.get(section)?.get(key)?.as_array())
            .flatten();
        let optional_dependencies = data
            .get("project")
            .and_then(|project| project.get("optional-dependencies"))
            .and_then(Value::as_table)
            .into_iter()
            .flat_map(|extras| extras.values())
            .filter_map(Value::as_array)
            .flatten();
        let dependency_names: HashSet<&str> = dependencies
            .chain(optional_dependencies)
            .filter_map(Value::as_str)
            .filter_map(|requirement| requirement_name.captures(requirement)?.get(1).map(|name| name.as_str()))
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

impl PythonStrategy {
    /// Writes a new version to the given `pyproject.toml` file.
    ///
    /// It will check for the package version in the following order:
    ///
    ///   1. `project.version`
    ///
    /// # Arguments
    ///
    /// * `filename` - The path to the `pyproject.toml` file.
    /// * `new_version` - The new version to apply.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result of whether an update was made.
    fn write_version_to_pyproject_toml(filename: &Path, new_version: &Version, dry_run: bool) -> anyhow::Result<bool> {
        // Parse the `pyproject.toml` file.
        let filename_display = filename.display();
        let contents =
            fs::read_to_string(filename).map_err(|err| anyhow!("could not read `{filename_display}`: {err}"))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{filename_display}`: {err}"))?;

        // Update the `[project.version]` field if it exists.
        if let Some(project) = data["project"].as_table_mut() {
            if !dry_run {
                debug!("set `[project.version]` to `{new_version}` at `{filename_display}`");
                project["version"] = toml_edit::value(new_version.to_string());
                fs::write(filename, data.to_string())
                    .map_err(|err| anyhow!("failed to write `[project.version]` to `{filename_display}`: {err}"))?;
            } else {
                debug!("set `[project.version]` to `{new_version}` at `{filename_display}` (dry run)");
            }
            Ok(true)
        } else {
            debug!("could not find `[project.version]` in `{filename_display}`");
            Ok(false)
        }
    }

    /// Writes a new version for a crate to the `uv.lock` file.
    ///
    /// It will find the closest `uv.lock` file from a given package directory upwards.
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
    /// A result containing the path to the updated `uv.lock` file.
    ///
    /// # See Also
    ///
    /// * [`Self::find_uv_lock()`] - For finding the closest `uv.lock` file.
    fn write_version_to_uv_lock(
        root: &Path,
        dir: &Path,
        name: &str,
        new_version: &Version,
        dry_run: bool,
    ) -> anyhow::Result<PathBuf> {
        // Parse the `uv.lock` file.
        let lockfile = Self::find_uv_lock(dir, root)?;
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

    /// Suggests Python workspace members from a `uv` workspace as package configurations
    /// that should be included.
    ///
    /// # Arguments
    ///
    /// * `dir` - The directory to a Python workspace.
    /// * `workspace` - The `tool.uv.workspace` TOML value.
    ///
    /// # Returns
    ///
    /// The package configurations of each Python workspace member if any.
    fn suggest_packages_from_uv_workspace(dir: &Path, workspace: &Value) -> anyhow::Result<Vec<PackageConfig>> {
        // Collect the workspace member globs.
        let members = workspace
            .get("members")
            .and_then(|members| members.as_array())
            .into_iter()
            .flatten()
            .filter_map(|member| member.as_str())
            .collect::<Vec<_>>();

        // Collect the workspace exclude globs and expand them to paths.
        let exclude = workspace
            .get("exclude")
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
                    .map_err(|err| anyhow!("invalid package glob `{member}`: {err}"))
            })
            .flatten()
            .filter_map(|entry| {
                entry
                    .as_ref()
                    .map_err(|err| anyhow!("error reading package: {err}"))
                    .ok()
                    .cloned()
            })
            // Filter out any excluded paths.
            .filter(|path| !exclude.contains(path))
            // Filter out any paths that do not contain a `pyproject.toml` file.
            .filter(|path| path.join("pyproject.toml").try_exists().unwrap_or(false))
            .map(|path| {
                PackageConfig {
                    dir: path,
                    strategy: Some(Strategy::Python(PythonStrategy::default())),
                    make_latest: false,
                    ..Default::default()
                }
                .apply_defaults()
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }

    /// Suggests the package name from a `pyproject.toml` file.
    ///
    /// It will check for the package name in the following order:
    ///
    ///   1. `project.name`
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the `pyproject.toml` file.
    ///
    /// # Returns
    ///
    /// The package name if successfully extracted.
    fn suggest_name_from_pyproject_toml(path: &Path) -> anyhow::Result<Option<String>> {
        // Parse the `pyproject.toml` file.
        let content = fs::read_to_string(path).map_err(|err| anyhow!("could not read `{}`: {err}", path.display()))?;
        let data: Value =
            toml::from_str(&content).map_err(|err| anyhow!("could not parse `{}`: {err}", path.display()))?;

        // Try `project.name`.
        if let Some(name) = data.get("project").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
            return Ok(Some(name.to_string()));
        }

        Ok(None)
    }

    /// Writes new dependency versions to the given `pyproject.toml` file.
    ///
    /// # Arguments
    ///
    /// * `filename` - The path to the `pyproject.toml` file.
    /// * `versions` - A map of dependency names to their new versions.
    /// * `dry_run` - If true, do not actually write the changes.
    ///
    /// # Returns
    ///
    /// A result of whether any updates were made.
    fn write_dependency_versions_to_pyproject_toml(
        filename: &Path,
        versions: &HashMap<String, Version>,
        dry_run: bool,
    ) -> anyhow::Result<bool> {
        // Parse the `pyproject.toml` file.
        let contents =
            fs::read_to_string(filename).map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        // Match explicitly versioned requirements while retaining extras and environment markers.
        let requirement =
            Regex::new(r"(?i)^(\s*([a-z0-9][a-z0-9._-]*)(?:\s*\[[^\]]*\])?\s*)(\(?\s*[<>=!~][^;@]*?)(\s*(?:;.*)?)$")?;
        let mut changed = false;

        // Update the dependency versions in the `project` table.
        if let Some(project) = data.get_mut("project").and_then(Item::as_table_like_mut) {
            changed |= Self::update_dependency_tables(project, versions, &requirement, filename);
        }

        // Update the dependency versions in the `build-system` table.
        if let Some(build) = data.get_mut("build-system").and_then(Item::as_table_like_mut) {
            changed |= Self::update_dependency_tables(build, versions, &requirement, filename);
        }

        // Update the dependency versions in the `dependency-groups` table.
        if let Some(groups) = data.get_mut("dependency-groups").and_then(Item::as_table_like_mut) {
            for (_, group) in groups.iter_mut() {
                if let Some(requirements) = group.as_array_mut() {
                    changed |= Self::update_dependency_array(requirements, versions, &requirement, filename);
                }
            }
        }

        // Write the manifest back if any changes were made.
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

    /// Writes new dependency versions to the given `uv.lock` file.
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
    /// A result containing the path to the updated `uv.lock` file.
    fn write_dependency_versions_to_uv_lock(
        root: &Path,
        dir: &Path,
        versions: &HashMap<String, Version>,
        dry_run: bool,
    ) -> anyhow::Result<PathBuf> {
        // Parse the `uv.lock` file.
        let lockfile = Self::find_uv_lock(dir, root)?;
        let lockfile_display = lockfile.strip_prefix(root)?.display();
        let contents =
            fs::read_to_string(&lockfile).map_err(|err| anyhow!("could not read `{lockfile_display}`: {err}"))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{lockfile_display}`: {err}"))?;

        // Update the dependency versions in the `uv.lock` file.
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

        // Write the `uv.lock` file back if any changes were made.
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

    /// Updates dependency versions in `pyproject.toml` tables.
    ///
    /// # Arguments
    ///
    /// * `table` - The TOML table to update.
    /// * `versions` - A map of dependency names to their new versions.
    /// * `requirement` - The pattern matching explicitly versioned Python requirements.
    /// * `filename` - The path to the TOML file being updated (for logging).
    ///
    /// # Returns
    ///
    /// Whether any dependency versions were updated in the table.
    fn update_dependency_tables(
        table: &mut dyn toml_edit::TableLike,
        versions: &HashMap<String, Version>,
        requirement: &Regex,
        filename: &Path,
    ) -> bool {
        let mut changed = false;
        for (key, item) in table.iter_mut() {
            // Descend through the tables until a depenedency array or table is reached.
            if matches!(key.get(), "dependencies" | "dev-dependencies" | "requires") {
                // If the dependencies are in an array of strings, update each string-like entry.
                if let Some(requirements) = item.as_array_mut() {
                    changed |= Self::update_dependency_array(requirements, versions, requirement, filename);
                    continue;
                }

                // If the dependencies are in a table, update each dependency entry.
                if let Some(dependencies) = item.as_table_like_mut() {
                    // For each dependency, check if it is in scope and update its version if necessary.
                    for (name, dependency) in dependencies.iter_mut() {
                        // Check if the dependency has a version to be updated.
                        let Some(version) = versions.get(name.get()) else {
                            continue;
                        };

                        // If the dependency is a string, update it directly, otherwise update the `version` field in the table.
                        let version_item = if dependency.is_str() {
                            Some(&mut *dependency)
                        } else {
                            dependency.get_mut("version")
                        };

                        // Skip dependencies that do not specify a version, i.e. are path-only or workspace-inherited.
                        let Some(value) = version_item
                            .and_then(Item::as_value_mut)
                            .filter(|value| value.as_str().is_some_and(|v| !matches!(v, "*" | "")))
                        else {
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

            // Update each optional dependency extra.
            } else if key.get() == "optional-dependencies"
                && let Some(extras) = item.as_table_like_mut()
            {
                for (_, extra) in extras.iter_mut() {
                    if let Some(requirements) = extra.as_array_mut() {
                        changed |= Self::update_dependency_array(requirements, versions, requirement, filename);
                    }
                }
            }
        }

        changed
    }

    /// Updates explicitly versioned Python requirement strings in a TOML array.
    ///
    /// # Arguments
    ///
    /// * `array` - The TOML array to update.
    /// * `versions` - A map of dependency names to their new versions.
    /// * `requirement` - The pattern matching explicitly versioned Python requirements.
    /// * `filename` - The path to the TOML file being updated (for logging).
    ///
    /// # Returns
    ///
    /// Whether any dependency versions were updated in the array.
    fn update_dependency_array(
        array: &mut toml_edit::Array,
        versions: &HashMap<String, Version>,
        requirement: &Regex,
        filename: &Path,
    ) -> bool {
        let mut changed = false;
        for value in array.iter_mut() {
            // Check if the dependency is a string that matches the requirement pattern.
            let Some(captures) = value.as_str().and_then(|value| requirement.captures(value)) else {
                continue;
            };

            // Check if the dependency has a version to be updated.
            let Some(version) = versions.get(&captures[2]) else {
                continue;
            };

            // Preserve extras, parentheses, markers, and TOML comments around the new constraint.
            let specifier = if captures[3].trim().starts_with('(') {
                format!("(=={version})")
            } else {
                format!("=={version}")
            };

            // Update the dependency to the new version.
            let new_requirement = format!("{}{}{}", &captures[1], specifier, &captures[4]);
            if value.as_str() != Some(new_requirement.as_str()) {
                debug!(
                    "set dependency `{}` version to `{version}` at `{}`",
                    &captures[2],
                    filename.display(),
                );
                let mut replacement = toml_edit::Value::from(new_requirement);
                *replacement.decor_mut() = value.decor().clone();
                *value = replacement;
                changed = true;
            }
        }

        changed
    }

    /// Finds the closest `uv.lock` file from a given package directory upwards.
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
    /// A result containing the `uv.lock` path if found.
    pub fn find_uv_lock(dir: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
        let mut dir = relative.join(dir);

        // Traverse up until we find `uv.lock` or reach `cwd`.
        while dir.starts_with(relative) {
            // Check if `uv.lock` exists at this level.
            let lockfile = dir.join("uv.lock");
            if lockfile.is_file() {
                return Ok(lockfile);
            }
            // Move to the parent directory.
            if !dir.pop() {
                break;
            }
        }

        bail!(
            "could not find a `uv.lock` file for workspace member `{}`",
            dir.display(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use indoc::indoc;
    use rstest::{fixture, rstest};

    /// A test fixture for a default Python strategy.
    #[fixture]
    fn strategy() -> PythonStrategy {
        PythonStrategy::default()
    }

    /// Tests that the suggested name for a directory is extracted from a `pyproject.toml` file.
    #[rstest]
    fn test_suggest_name_for_dir_with_pyproject_toml_file(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[project]\nname = \"mystery\"")
            .unwrap();
        temp_dir.child("version.txt").write_str("1.0.0").unwrap(); // red-herring

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "mystery");
    }

    /// Tests that the suggested name for a directory is extracted from the first file.
    #[rstest]
    fn test_suggest_name_for_dir_with_multiple_files(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("setup.py").write_str("name = \"foo\"").unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[project]\nname = \"bar\"")
            .unwrap();
        temp_dir.child("version.txt").write_str("1.0.0").unwrap(); // red-herring

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "bar");
    }

    /// Tests that the suggested name for a directory is not extracted from a directory with no
    /// relevant files to the strategy.
    #[rstest]
    fn test_suggest_name_for_dir_with_no_relevant_files(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("CHANGELOG.md").write_str("...").unwrap();
        temp_dir.child("version.txt").write_str("1.0.0").unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "could not determine name for package",
        );
    }

    /// Tests that the suggested name for a directory is not extracted if all files
    /// are present but none yield a result.
    #[rstest]
    fn test_suggest_name_for_dir_fallthrough(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("pyproject.toml").write_str("[invalid.toml]").unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "could not determine name for package",
        );
    }

    /// Tests that the suggested name for a directory is extracted from the `project.name` field
    /// of a `pyproject.toml` file.
    #[rstest]
    fn test_suggest_name_from_pyproject_toml_file_with_project_section() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml.write_str("[project]\nname = \"magic\"").unwrap();

        let result = PythonStrategy::suggest_name_from_pyproject_toml(&pyproject_toml).unwrap();
        assert_eq!(result.unwrap(), "magic");
    }

    /// Tests that the suggested name for a directory is not extracted from a `pyproject.toml` file
    /// with an unknown structure.
    #[rstest]
    fn test_suggest_name_from_pyproject_toml_file_with_unknown_structure() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml.write_str("[something]\nname = \"magic\"").unwrap();

        let result = PythonStrategy::suggest_name_from_pyproject_toml(&pyproject_toml).unwrap();
        assert!(result.is_none(), "expected no package name");
    }

    /// Tests that there are no suggested packages for a non-workspace Python project.
    #[rstest]
    fn test_suggest_packages(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[project]\nname = \"magic\"")
            .unwrap();

        assert_eq!(strategy.suggest_packages(&temp_dir).unwrap().len(), 0);
    }

    /// Tests that members from a `uv` workspace are suggested as packages.
    #[rstest]
    fn test_suggest_packages_from_uv_workspace() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pkg_a = temp_dir.child("packages/a");
        let pkg_b = temp_dir.child("packages/b");
        let pkg_c = temp_dir.child("packages/c"); // exclude = ["packages/c"]
        let pkg_d = temp_dir.child("other/d");
        let pkg_e = temp_dir.child("other/not-a-python-pkg");
        pkg_a.create_dir_all().unwrap();
        pkg_b.create_dir_all().unwrap();
        pkg_c.create_dir_all().unwrap();
        pkg_d.create_dir_all().unwrap();
        pkg_e.create_dir_all().unwrap();

        let root_toml: Value =
            toml::from_str("[tool.uv.workspace]\nmembers = [\"packages/*\", \"other/*\"]\nexclude = [\"packages/c\"]")
                .unwrap();
        let workspace = root_toml
            .get("tool")
            .and_then(|t| t.get("uv"))
            .and_then(|u| u.get("workspace"))
            .unwrap();

        temp_dir
            .child("pyproject.toml")
            .write_str(&root_toml.to_string())
            .unwrap();
        pkg_a
            .child("pyproject.toml")
            .write_str("[project]\nname = \"a\"")
            .unwrap();
        pkg_b
            .child("pyproject.toml")
            .write_str("[project]\nname = \"b\"")
            .unwrap();
        pkg_c
            .child("pyproject.toml")
            .write_str("[project]\nname = \"c\"")
            .unwrap();
        pkg_d
            .child("pyproject.toml")
            .write_str("[project]\nname = \"d\"")
            .unwrap();
        pkg_e.child("README.md").write_str("not a Python package").unwrap();

        let packages = PythonStrategy::suggest_packages_from_uv_workspace(&temp_dir, &workspace).unwrap();
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].dir, pkg_a.path());
        assert_eq!(packages[1].dir, pkg_b.path());
        assert_eq!(packages[2].dir, pkg_d.path());
    }

    /// Tests that a new version is written to a given `pyproject.toml` file.
    #[rstest]
    fn test_write_version_to_pyproject_toml() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[project]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n")
            .unwrap();

        let result = PythonStrategy::write_version_to_pyproject_toml(&pyproject_toml, &Version::new(1, 1, 0), false);
        assert!(result.is_ok());
        pyproject_toml.assert("[project]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version is *not* written to a given `pyproject.toml` file when performing a dry run.
    #[rstest]
    fn test_write_version_to_pyproject_toml_with_dry_run() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[project]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n")
            .unwrap();

        let result = PythonStrategy::write_version_to_pyproject_toml(&pyproject_toml, &Version::new(1, 1, 0), true);
        assert!(result.is_ok());
        // The version should not be updated because this is a dry run.
        pyproject_toml.assert("[project]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n");
    }

    /// Tests that a new version cannot be written to a `pyproject.toml` file with an unknown structure.
    #[rstest]
    fn test_write_version_to_pyproject_toml_with_unknown_structure() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[something]\nname = \"magic\"\n# no version\n")
            .unwrap();

        let result = PythonStrategy::write_version_to_pyproject_toml(&pyproject_toml, &Version::new(1, 1, 0), false);
        assert!(!result.unwrap());
    }

    /// Tests that a new version is written to a package's `uv.lock` file.
    #[rstest]
    fn test_write_version_to_uv_lock() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("uv.lock");
        let crate_a = temp_dir.child("crates/a");
        crate_a.create_dir_all().unwrap();
        lockfile
            .write_str("version = 1\nrevision=1\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n")
            .unwrap();

        let result =
            PythonStrategy::write_version_to_uv_lock(&temp_dir, &crate_a, "crate-a", &Version::new(1, 1, 0), false);
        assert!(result.is_ok());
        lockfile.assert("version = 1\nrevision=1\n[[package]]\nname = \"crate-a\"\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version for an unknown package cannot be written to a `uv.lock` file.
    #[rstest]
    fn test_write_version_to_uv_lock_with_unknown_package() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("uv.lock");
        lockfile
            .write_str("version = 1\nrevision=1\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"")
            .unwrap();

        let result =
            PythonStrategy::write_version_to_uv_lock(&temp_dir, &temp_dir, "unknown", &Version::new(1, 1, 0), false);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "package `unknown` not found in `uv.lock`"
        );
    }

    /// Tests that a new version is *not* written to a package's `uv.lock` file when performing a dry run.
    #[rstest]
    fn test_write_version_to_uv_lock_with_dry_run() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let lockfile = temp_dir.child("uv.lock");
        let crate_a = temp_dir.child("crates/a");
        crate_a.create_dir_all().unwrap();
        lockfile
            .write_str("version = 1\nrevision=1\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n")
            .unwrap();

        let result =
            PythonStrategy::write_version_to_uv_lock(&temp_dir, &crate_a, "crate-a", &Version::new(1, 1, 0), true);
        assert!(result.is_ok());
        // The version should not be updated because this is a dry run.
        lockfile.assert("version = 1\nrevision=1\n[[package]]\nname = \"crate-a\"\nversion = \"1.0.0\"\n");
    }

    /// Tests that new dependency versions are written to the `pyproject.toml` and `uv.lock` files.
    #[rstest]
    fn test_write_dependency_versions(strategy: PythonStrategy) {
        // Create a Python project with some local dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str(indoc! {r#"
                [project]
                name = "magic"
                version = "1.0.0"
                dependencies = ["api", "core>=1.0.0"]

                [build-system]
                requires = ["build~=1.0.0"]
                "#})
            .unwrap();
        let uv_lock = temp_dir.child("uv.lock");
        uv_lock
            .write_str(indoc! {r#"
                version = 1
                revision = 1

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
                    { name = "api" },
                ]
                "#})
            .unwrap();

        // Build a package configuration for the project.
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
            vec![pyproject_toml.to_path_buf(), uv_lock.to_path_buf()]
        );
        pyproject_toml.assert(indoc! {r#"
            [project]
            name = "magic"
            version = "1.0.0"
            dependencies = ["api", "core==1.1.0"]

            [build-system]
            requires = ["build==1.2.0"]
            "#});
        uv_lock.assert(indoc! {r#"
            version = 1
            revision = 1

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
                { name = "api" },
            ]
            "#});
    }

    /// Tests that new dependency versions are *not* written when performing a dry run.
    #[rstest]
    fn test_write_dependency_versions_with_dry_run(strategy: PythonStrategy) {
        // Create a Python project with some local dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        let pyproject_toml_contents = indoc! {r#"
            [project]
            name = "magic"
            version = "1.0.0"
            dependencies = ["api", "core>=1.0.0"]

            [build-system]
            requires = ["build~=1.0.0"]
            "#};
        pyproject_toml.write_str(pyproject_toml_contents).unwrap();
        let uv_lock = temp_dir.child("uv.lock");
        let uv_lock_contents = indoc! {r#"
            version = 1
            revision = 1

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
                { name = "api" },
            ]
            "#};
        uv_lock.write_str(uv_lock_contents).unwrap();

        // Build a package configuration for the project.
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
            vec![pyproject_toml.to_path_buf(), uv_lock.to_path_buf()]
        );
        pyproject_toml.assert(pyproject_toml_contents);
        uv_lock.assert(uv_lock_contents);
    }

    /// Tests that an empty map of dependency versions causes a short-circuit and no files are written.
    #[rstest]
    fn test_write_dependency_versions_with_no_dependencies(strategy: PythonStrategy) {
        // Create a Python project.
        let temp_dir = assert_fs::TempDir::new().unwrap();

        // Build a package configuration for the project.
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

    /// Tests that new dependency versions are written to the `pyproject.toml` file.
    #[rstest]
    fn test_write_dependency_versions_to_pyproject_toml() {
        // Create a Python project with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        let contents = indoc! {r#"
            [project]
            name = "magic"
            version = "1.0.0"
            dependencies = [
                "api",
                "core[extra]>=1.0.0,<2.0.0; python_version >= '3.12'", # Keep this comment.
                "other==1.0.0",
                "direct @ file:///packages/direct",
                "expanded (>=1.0.0)",
            ]

            [project.optional-dependencies]
            cli = ["optional[cli]~=1.0.0"]

            [dependency-groups]
            dev = ["test!=1.0.0", { include-group = "lint" }]
            lint = ["lint>=1.0.0"]

            [build-system]
            requires = ["build~=1.0.0"]
            "#};
        pyproject_toml.write_str(contents).unwrap();

        // Write new dependency versions.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("direct".to_string(), Version::new(1, 1, 0)),
            ("expanded".to_string(), Version::new(1, 5, 0)),
            ("optional".to_string(), Version::new(1, 6, 0)),
            ("test".to_string(), Version::new(1, 3, 0)),
            ("lint".to_string(), Version::new(1, 7, 0)),
            ("build".to_string(), Version::new(1, 4, 0)),
        ]);
        let result = PythonStrategy::write_dependency_versions_to_pyproject_toml(&pyproject_toml, &versions, false);

        // Verify that only matching, explicitly versioned dependencies were changed.
        assert!(result.is_ok());
        assert!(result.unwrap());
        pyproject_toml.assert(indoc! {r#"
            [project]
            name = "magic"
            version = "1.0.0"
            dependencies = [
                "api",
                "core[extra]==1.2.0; python_version >= '3.12'", # Keep this comment.
                "other==1.0.0",
                "direct @ file:///packages/direct",
                "expanded (==1.5.0)",
            ]

            [project.optional-dependencies]
            cli = ["optional[cli]==1.6.0"]

            [dependency-groups]
            dev = ["test==1.3.0", { include-group = "lint" }]
            lint = ["lint==1.7.0"]

            [build-system]
            requires = ["build==1.4.0"]
            "#});

        // Reapplying persisted versions must not report any further manifest changes.
        assert!(
            !PythonStrategy::write_dependency_versions_to_pyproject_toml(&pyproject_toml, &versions, false).unwrap()
        );
    }

    /// Tests that new dependency versions are *not* written to `pyproject.toml` when performing a dry run.
    #[rstest]
    fn test_write_dependency_versions_to_pyproject_toml_with_dry_run() {
        // Create a Python project with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        let contents = indoc! {r#"
            [project]
            name = "magic"
            version = "1.0.0"
            dependencies = [
                "api",
                "core[extra]>=1.0.0,<2.0.0; python_version >= '3.12'", # Keep this comment.
                "other==1.0.0",
                "direct @ file:///packages/direct",
                "expanded (>=1.0.0)",
            ]

            [project.optional-dependencies]
            cli = ["optional[cli]~=1.0.0"]

            [dependency-groups]
            dev = ["test!=1.0.0", { include-group = "lint" }]
            lint = ["lint>=1.0.0"]

            [build-system]
            requires = ["build~=1.0.0"]
            "#};
        pyproject_toml.write_str(contents).unwrap();

        // Write new dependency versions with dry run.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("direct".to_string(), Version::new(1, 1, 0)),
            ("expanded".to_string(), Version::new(1, 5, 0)),
            ("optional".to_string(), Version::new(1, 6, 0)),
            ("test".to_string(), Version::new(1, 3, 0)),
            ("lint".to_string(), Version::new(1, 7, 0)),
            ("build".to_string(), Version::new(1, 4, 0)),
        ]);
        let result = PythonStrategy::write_dependency_versions_to_pyproject_toml(&pyproject_toml, &versions, true);

        // Verify that the file was reported as changed, but the contents were not actually modified.
        assert!(result.is_ok());
        assert!(result.unwrap());
        pyproject_toml.assert(contents);
    }

    /// Tests that new dependency versions are written to the `uv.lock` file.
    #[rstest]
    fn test_write_dependency_versions_to_uv_lock() {
        // Create a Python workspace with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let package_dir = temp_dir.child("packages/magic");
        package_dir.create_dir_all().unwrap();
        let uv_lock = temp_dir.child("uv.lock");
        let contents = indoc! {r#"
            version = 1
            revision = 1

            [[package]]
            name = "api"
            version = "1.0.0"

            [[package]]
            name = "core"
            version = "1.0.0"
            dependencies = [
                { name = "api" },
            ]

            [[package]]
            name = "other"
            version = "1.0.0"
            "#};
        uv_lock.write_str(contents).unwrap();

        // Write new dependency versions.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("missing".to_string(), Version::new(2, 0, 0)),
        ]);
        let result = PythonStrategy::write_dependency_versions_to_uv_lock(&temp_dir, &package_dir, &versions, false);

        // Verify that the new dependency versions were written correctly.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), uv_lock.to_path_buf());
        uv_lock.assert(indoc! {r#"
            version = 1
            revision = 1

            [[package]]
            name = "api"
            version = "1.1.0"

            [[package]]
            name = "core"
            version = "1.2.0"
            dependencies = [
                { name = "api" },
            ]

            [[package]]
            name = "other"
            version = "1.0.0"
            "#});
    }

    /// Tests that new dependency versions are *not* written to `uv.lock` when performing a dry run.
    #[rstest]
    fn test_write_dependency_versions_to_uv_lock_with_dry_run() {
        // Create a Python workspace with some dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let package_dir = temp_dir.child("packages/magic");
        package_dir.create_dir_all().unwrap();
        let uv_lock = temp_dir.child("uv.lock");
        let contents = indoc! {r#"
            version = 1
            revision = 1

            [[package]]
            name = "api"
            version = "1.0.0"

            [[package]]
            name = "core"
            version = "1.0.0"
            dependencies = [
                { name = "api" },
            ]

            [[package]]
            name = "other"
            version = "1.0.0"
            "#};
        uv_lock.write_str(contents).unwrap();

        // Write new dependency versions with dry run.
        let versions = HashMap::from([
            ("api".to_string(), Version::new(1, 1, 0)),
            ("core".to_string(), Version::new(1, 2, 0)),
            ("missing".to_string(), Version::new(2, 0, 0)),
        ]);
        let result = PythonStrategy::write_dependency_versions_to_uv_lock(&temp_dir, &package_dir, &versions, true);

        // Verify that the file was reported as changed, but the contents were not actually modified.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), uv_lock.to_path_buf());
        uv_lock.assert(contents);
    }

    /// Tests that dependencies of a Python package are found correctly.
    #[rstest]
    #[case::runtime("[project]\ndependencies = ['my-package>=1.0']", true)]
    #[case::extras("[project]\ndependencies = ['my-package[cli]']", true)]
    #[case::marker("[project]\ndependencies = ['my-package; python_version >= \"3.12\"']", true)]
    #[case::url("[project]\ndependencies = ['my-package @ file:///packages/my-package']", true)]
    #[case::optional("[project.optional-dependencies]\ncli = ['my-package']", true)]
    #[case::dev("[dependency-groups]\ndev = ['my-package']", false)]
    #[case::build("[build-system]\nrequires = ['my-package>=1.0']", true)]
    #[case::empty("[project]\nname = 'my-api'", false)]
    #[case::out_of_scope("[project]\ndependencies = ['other-package>=1.0']", false)]
    fn test_find_dependencies(strategy: PythonStrategy, #[case] manifest: &str, #[case] expected: bool) {
        // Create a Python workspace with some packages and local dependencies.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let package = temp_dir.child("packages/my-api");
        package.create_dir_all().unwrap();
        package.child("pyproject.toml").write_str(manifest).unwrap();

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

    /// Tests that the `uv.lock` file is found for a standalone uv project.
    #[rstest]
    fn test_find_uv_lock() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[project]\nname = \"a\"")
            .unwrap();
        temp_dir.child("uv.lock").write_str("version = 1").unwrap();

        let lockfile = PythonStrategy::find_uv_lock(&temp_dir, &temp_dir);
        assert_eq!(lockfile.unwrap(), temp_dir.join("uv.lock"));
    }

    /// Tests that the `uv.lock` file is found for a uv workspace member.
    #[rstest]
    fn test_find_uv_lock_with_cargo_workspace_member() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let container = temp_dir.child("project");
        let package_a = container.child("packages/a");
        package_a.create_dir_all().unwrap();
        container
            .child("pyproject.toml")
            .write_str("[tool.uv.workspace]\nmembers = [\"packages/*\"]")
            .unwrap();
        container.child("uv.lock").write_str("version = 1").unwrap();
        package_a
            .child("pyproject.toml")
            .write_str("[project]\nname = \"a\"")
            .unwrap();

        let lockfile = PythonStrategy::find_uv_lock(&package_a, &temp_dir);
        assert_eq!(lockfile.unwrap(), container.join("uv.lock"));
    }

    /// Tests that a `uv.lock` file is not returned when it can't be found.
    #[rstest]
    fn test_find_uv_lock_when_not_found() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let package_a = temp_dir.child("packages/a");
        package_a.create_dir_all().unwrap();
        package_a
            .child("pyproject.toml")
            .write_str("[project]\nname = \"a\"")
            .unwrap();

        let lockfile = PythonStrategy::find_uv_lock(&package_a, &temp_dir);
        assert!(lockfile.is_err());
        assert!(
            lockfile
                .unwrap_err()
                .to_string()
                .starts_with("could not find a `uv.lock` file for workspace member")
        );
    }

    /// Tests that `uv.lock` files are only searched for within the current working directory.
    #[rstest]
    fn test_find_uv_lock_does_not_escape_cwd() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let project = temp_dir.child("projects");
        let package_a = project.child("packages/a");
        package_a.create_dir_all().unwrap();
        temp_dir.child("uv.lock").write_str("version = 1").unwrap();
        package_a
            .child("pyproject.toml")
            .write_str("[project]\nname = \"a\"")
            .unwrap();

        let lockfile = PythonStrategy::find_uv_lock(&package_a, &project); // DO NOT escape project path
        assert!(lockfile.is_err());
        assert!(
            lockfile
                .unwrap_err()
                .to_string()
                .starts_with("could not find a `uv.lock` file for workspace member")
        );
    }
}
