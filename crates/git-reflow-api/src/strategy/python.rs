use crate::settings::pkg::PackageConfig;
use crate::strategy::{BaseStrategy, Strategy};
use anyhow::{anyhow, bail};
use regex::Regex;
use semver::Version;
use std::borrow::Cow;
use std::fs;
use std::path::Path;
use toml::Value;
use toml_edit::DocumentMut;
use tracing::debug;

/// The [Python](https://www.python.org/) release strategy.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PythonStrategy {
    // empty
}

impl BaseStrategy for PythonStrategy {
    /// Writes a new version to the `pyproject.toml` and `setup.py` files.
    ///
    /// # Arguments
    ///
    /// * `new_version` - The new version to apply.
    /// * `config` - The package configuration.
    fn write_version(&self, new_version: &Version, config: &PackageConfig) -> anyhow::Result<()> {
        // Update the `pyproject.toml` file
        let pyproject = &config.dir.join("pyproject.toml");
        if pyproject.try_exists()? {
            return Self::write_version_to_pyproject_toml(&pyproject, &new_version);
        } else {
            debug!(
                "a `pyproject.toml` file was not found at `{}`, skipping",
                pyproject.display()
            );
        }

        // Update the `setup.py` file
        let setup_py = &config.dir.join("setup.py");
        if setup_py.try_exists()? {
            return Self::write_version_to_setup_py(&setup_py, new_version);
        } else {
            debug!(
                "a `setup.py` file was not found at `{}`, skipping",
                setup_py.display()
            );
        }

        Ok(())
    }

    /// Suggests the package name from either the `pyproject.toml` or `setup.py` files.
    ///
    /// It will check for the package name in the following order:
    ///
    ///   1. `pyproject.toml`
    ///
    ///      - `project.name`
    ///      - `tool.poetry.name`
    ///
    ///   2. `setup.py`
    ///
    ///      - `name = ...`
    ///
    /// # Arguments
    /// * `dir` - The package directory to get the name for.
    ///
    /// # Returns
    /// The package name for the given Python project.
    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String> {
        // Try `pyproject.toml`
        let pyproject = &dir.join("pyproject.toml");
        if pyproject.try_exists()? {
            if let Some(name) = Self::suggest_name_from_pyproject_toml(pyproject)? {
                return Ok(name);
            }
        }

        // Try `setup.py`
        let setup_py = &dir.join("setup.py");
        if setup_py.try_exists()? {
            if let Some(name) = Self::suggest_name_from_setup_py(setup_py)? {
                return Ok(name);
            }
        }

        // Unsupported file
        Err(anyhow!("could not determine name for package"))
    }

    /// Suggests Python workspace members as package configurations that should be included.
    ///
    /// # Arguments
    /// * `dir` - The directory to a *possible* Python workspace.
    ///
    /// # Returns
    /// The package configurations of each Python workspace member if any.
    fn suggest_packages(&self, dir: &Path) -> anyhow::Result<Vec<PackageConfig>> {
        let filename = &dir.join("pyproject.toml");
        if !filename.is_file() {
            return Ok(vec![]);
        }
        let contents = fs::read_to_string(filename)
            .map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let data: Value = toml::from_str(&contents)
            .map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        // Try `uv` workspace
        if let Some(workspace) = data
            .get("tool")
            .and_then(|tool| tool.get("uv"))
            .and_then(|uv| uv.get("workspace"))
        {
            return Self::suggest_packages_from_uv_workspace(dir, workspace);
        }

        Ok(vec![])
    }
}

impl PythonStrategy {
    /// Writes a new version to the given `pyproject.toml` file.
    ///
    /// It will check for the package version in the following order:
    ///
    ///   1. `project.version`
    ///   2. `tool.poetry.version`
    ///
    /// It will reject dynamically versioned projects.
    ///
    /// # Arguments
    /// * `filename` - The path to the `pyproject.toml` file.
    /// * `new_version` - The new version to apply.
    ///
    /// # Returns
    /// A result of whether the update was successful.
    fn write_version_to_pyproject_toml(
        filename: &Path,
        new_version: &Version,
    ) -> anyhow::Result<()> {
        let contents = fs::read_to_string(filename)
            .map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;
        let mut data = contents
            .parse::<DocumentMut>()
            .map_err(|err| anyhow!("could not parse `{}`: {err}", filename.display()))?;

        // `project.version`
        if let Some(project) = data["project"].as_table_mut() {
            project["version"] = toml_edit::value(new_version.to_string());
            fs::write(filename, data.to_string()).map_err(|err| {
                anyhow!(
                    "failed to write `[project.version]` to `{}`: {err}",
                    filename.display()
                )
            })?;
            debug!(
                "set `[project.version]` to `{new_version}` at `{}`",
                filename.display()
            );
            return Ok(());
        }

        // `tool.poetry.version`
        if let Some(poetry) = data["tool"]
            .as_table_mut()
            .and_then(|tool| tool["poetry"].as_table_mut())
        {
            poetry["version"] = toml_edit::value(new_version.to_string());
            fs::write(filename, data.to_string()).map_err(|err| {
                anyhow!(
                    "failed to write `[tool.poetry.version]` to `{}`: {err}",
                    filename.display()
                )
            })?;
            debug!(
                "set `[tool.poetry.version]` to `{new_version}` at `{}`",
                filename.display()
            );
            return Ok(());
        }

        bail!(
            "could not find `[project.version]` or `[tool.poetry.version]` in `{}`",
            filename.display()
        )
    }

    /// Writes a new version to the given `setup.py` file.
    ///
    /// # Arguments
    /// * `filename` - The path to the `setup.py` file.
    /// * `new_version` - The new version to apply.
    ///
    /// # Returns
    /// A result of whether the update was successful.
    fn write_version_to_setup_py(filename: &Path, new_version: &Version) -> anyhow::Result<()> {
        let contents = fs::read_to_string(filename)
            .map_err(|err| anyhow!("could not read `{}`: {err}", filename.display()))?;

        let re = Regex::new(r#"(version(?:\s*|:\s?[^'"]+)?=\s*['"])(.+?)(['"](,|\r|\n|$))"#)?;
        let new_contents = re.replace(&contents, |caps: &regex::Captures| {
            format!("{}{}{}", &caps[1], new_version.to_string(), &caps[3])
        });

        // Check if the contents were actually modified, i.e. got a new `&str` reference
        if let Cow::Owned(new_contents) = new_contents {
            fs::write(filename, new_contents).map_err(|err| {
                anyhow!(
                    "failed to write `version` to `{}`: {err}",
                    filename.display()
                )
            })?;
            return Ok(());
        }

        bail!("could not find `version` in `{}`", filename.display())
    }

    /// Suggests Python workspace members from a `uv` workspace as package configurations
    /// that should be included.
    ///
    /// # Arguments
    /// * `dir` - The directory to a Python workspace.
    /// * `workspace` - The `tool.uv.workspace` TOML value.
    ///
    /// # Returns
    /// The package configurations of each Python workspace member if any.
    fn suggest_packages_from_uv_workspace(
        dir: &Path,
        workspace: &Value,
    ) -> anyhow::Result<Vec<PackageConfig>> {
        let members = workspace
            .get("members")
            .and_then(|members| members.as_array())
            .into_iter()
            .flatten()
            .filter_map(|member| member.as_str())
            .collect::<Vec<_>>();

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
            .filter(|path| !exclude.contains(path))
            .filter(|path| path.join("pyproject.toml").try_exists().unwrap_or(false))
            .map(|path| {
                PackageConfig {
                    dir: path,
                    strategy: Some(Strategy::Python(PythonStrategy::default())),
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
    ///   2. `tool.poetry.name`
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the `pyproject.toml` file.
    ///
    /// # Returns
    ///
    /// The package name if successfully extracted.
    fn suggest_name_from_pyproject_toml(path: &Path) -> anyhow::Result<Option<String>> {
        let content = fs::read_to_string(path)
            .map_err(|err| anyhow!("could not read `{}`: {err}", path.display()))?;
        let data: Value = toml::from_str(&content)
            .map_err(|err| anyhow!("could not parse `{}`: {err}", path.display()))?;

        // `project.name`
        if let Some(name) = data
            .get("project")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        {
            return Ok(Some(name.to_string()));
        }

        // `tool.poetry.name`
        if let Some(name) = data
            .get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        {
            return Ok(Some(name.to_string()));
        }

        Ok(None)
    }

    /// Suggests the package name from a `setup.py` file.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the `setup.py` file.
    ///
    /// # Returns
    ///
    /// The package name if successfully extracted.
    fn suggest_name_from_setup_py(path: &Path) -> anyhow::Result<Option<String>> {
        let content = fs::read_to_string(path)
            .map_err(|err| anyhow!("could not read `{}`: {err}", path.display()))?;
        let re = Regex::new(r#"name(?:\s*|:\s?[^'"]+)?=\s*['"](.+?)['"](,|\r|\n|$)"#)?;
        if let Some(captures) = re.captures(&content) {
            return Ok(Some(captures[1].to_string()));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use rstest::{fixture, rstest};

    /// A test fixture for a default Python strategy.
    #[fixture]
    fn strategy() -> PythonStrategy {
        PythonStrategy::default()
    }

    /// Tests that the suggested name for a directory is extracted from a `pyproject.toml` file.
    #[rstest]
    fn suggest_name_for_dir_with_pyproject_toml_file(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[project]\nname = \"mystery\"")
            .unwrap();
        temp_dir.child("version.txt").write_str("1.0.0").unwrap(); // red-herring

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "mystery");
    }

    /// Tests that the suggested name for a directory is extracted from a `setup.py` file.
    #[rstest]
    fn suggest_name_for_dir_with_setup_py_file(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("setup.py")
            .write_str("name = \"machine\"")
            .unwrap();
        temp_dir.child("version.txt").write_str("1.0.0").unwrap(); // red-herring

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "machine");
    }

    /// Tests that the suggested name for a directory is extracted from the first file.
    #[rstest]
    fn suggest_name_for_dir_with_multiple_files(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("setup.py")
            .write_str("name = \"foo\"")
            .unwrap();
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
    fn suggest_name_for_dir_with_no_relevant_files(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("CHANGELOG.md").write_str("...").unwrap();
        temp_dir.child("version.txt").write_str("1.0.0").unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "could not determine name for package",
        );
    }

    /// Tests that the suggested name for a directory can be extracted from other files if
    /// previous files checked yield no result.
    #[rstest]
    fn suggest_name_for_dir_fallthrough(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[invalid.toml]")
            .unwrap();
        temp_dir
            .child("setup.py")
            .write_str("name = \"bar\"")
            .unwrap();

        assert_eq!(strategy.suggest_name(&temp_dir).unwrap(), "bar");
    }

    /// Tests that the suggested name for a directory is not extracted if all files
    /// are present but none yield a result.
    #[rstest]
    fn suggest_name_for_dir_fallthrough_all(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[invalid.toml]")
            .unwrap();
        temp_dir.child("setup.py").write_str("# no name").unwrap();

        assert_eq!(
            strategy.suggest_name(&temp_dir).unwrap_err().to_string(),
            "could not determine name for package",
        );
    }

    /// Tests that the suggested name for a directory is extracted from the `project.name` field
    /// of a `pyproject.toml` file.
    #[rstest]
    fn suggest_name_from_pyproject_toml_file_with_project_section() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[project]\nname = \"magic\"")
            .unwrap();

        let result = PythonStrategy::suggest_name_from_pyproject_toml(&pyproject_toml).unwrap();
        assert_eq!(result.unwrap(), "magic");
    }

    /// Tests that the suggested name for a directory is extracted from the `tool.poetry.name` field
    /// of a `pyproject.toml` file.
    #[rstest]
    fn suggest_name_from_pyproject_toml_file_with_poetry_section() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[tool.poetry]\nname = \"magic\"")
            .unwrap();

        let result = PythonStrategy::suggest_name_from_pyproject_toml(&pyproject_toml).unwrap();
        assert_eq!(result.unwrap(), "magic");
    }

    /// Tests that the suggested name for a directory is not extracted from a `pyproject.toml` file
    /// with an unknown structure.
    #[rstest]
    fn suggest_name_from_pyproject_toml_file_with_unknown_structure() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[something]\nname = \"magic\"")
            .unwrap();

        let result = PythonStrategy::suggest_name_from_pyproject_toml(&pyproject_toml).unwrap();
        assert!(result.is_none(), "expected no package name");
    }

    /// Tests that the suggested name for a directory is extracted from a `setup.py` file.
    #[rstest]
    fn suggest_name_from_setup_py_file() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let setup_py = temp_dir.child("setup.py");
        setup_py
            .write_str(
                r#"#!/usr/bin/env python
from distutils.core import setup
setup(name="foo", version="1.0", py_modules=["foo"])"#,
            )
            .unwrap();

        let result = PythonStrategy::suggest_name_from_setup_py(&setup_py).unwrap();
        assert_eq!(result.unwrap(), "foo");
    }

    /// Tests that the suggested name for a directory is extracted from a complex `setup.py` file.
    #[rstest]
    fn suggest_name_from_setup_py_file_complex() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let setup_py = temp_dir.child("setup.py");
        setup_py
            .write_str(
                r#"#!/usr/bin/env python
from typing import Final

from distutils.core import setup

name: Final[str] = "bar"
setup(name=name, version="1.0", py_modules=["foo"])"#,
            )
            .unwrap();

        let result = PythonStrategy::suggest_name_from_setup_py(&setup_py).unwrap();
        assert_eq!(result.unwrap(), "bar");
    }

    /// Tests that the suggested name for a directory is not extracted from a `setup.py` file
    /// with no name present.
    #[rstest]
    fn suggest_name_from_setup_py_file_with_no_name() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let setup_py = temp_dir.child("setup.py");
        setup_py
            .write_str(
                r#"#!/usr/bin/env python
from distutils.core import setup
setup()"#,
            )
            .unwrap();

        let result = PythonStrategy::suggest_name_from_setup_py(&setup_py).unwrap();
        assert!(result.is_none(), "expected no package name");
    }

    /// Tests that there are no suggested packages for a non-workspace Python project.
    #[rstest]
    fn suggest_packages(strategy: PythonStrategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir
            .child("pyproject.toml")
            .write_str("[project]\nname = \"magic\"")
            .unwrap();

        assert_eq!(strategy.suggest_packages(&temp_dir).unwrap().len(), 0);
    }

    /// Tests that members from a `uv` workspace are suggested as packages.
    #[rstest]
    fn suggest_packages_from_uv_workspace() {
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

        let root_toml: Value = toml::from_str("[tool.uv.workspace]\nmembers = [\"packages/*\", \"other/*\"]\nexclude = [\"packages/c\"]").unwrap();
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
        pkg_e
            .child("README.md")
            .write_str("not a Python package")
            .unwrap();

        let packages =
            PythonStrategy::suggest_packages_from_uv_workspace(&temp_dir, &workspace).unwrap();
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].dir, pkg_a.path());
        assert_eq!(packages[1].dir, pkg_b.path());
        assert_eq!(packages[2].dir, pkg_d.path());
    }

    /// Tests that a new version is written to a given `pyproject.toml` file.
    #[rstest]
    fn write_version_to_pyproject_toml() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml.write_str("[project]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n").unwrap();

        let result = PythonStrategy::write_version_to_pyproject_toml(
            &pyproject_toml,
            &Version::new(1, 1, 0),
        );
        assert!(result.is_ok());
        pyproject_toml.assert("[project]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version is written to a given `pyproject.toml` file with a `[tool.poetry]` section.
    #[rstest]
    fn write_version_to_pyproject_toml_with_poetry() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml.write_str("[tool.poetry]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.0.0\"\n").unwrap();

        let result = PythonStrategy::write_version_to_pyproject_toml(
            &pyproject_toml,
            &Version::new(1, 1, 0),
        );
        assert!(result.is_ok());
        pyproject_toml.assert("[tool.poetry]\nname = \"magic\"\n# DO NOT manually edit the version\nversion = \"1.1.0\"\n");
    }

    /// Tests that a new version cannot be written to a `pyproject.toml` file with an unknown structure.
    #[rstest]
    fn write_version_to_pyproject_toml_with_unknown_structure() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let pyproject_toml = temp_dir.child("pyproject.toml");
        pyproject_toml
            .write_str("[something]\nname = \"magic\"\n# no version\n")
            .unwrap();

        let result = PythonStrategy::write_version_to_pyproject_toml(
            &pyproject_toml,
            &Version::new(1, 1, 0),
        );
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            format!(
                "could not find `[project.version]` or `[tool.poetry.version]` in `{}`",
                pyproject_toml.display()
            )
        );
    }

    /// Tests that a new version is written to a given `setup.py` file.
    #[rstest]
    fn write_version_to_setup_py() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let setup_py = temp_dir.child("setup.py");
        setup_py
            .write_str(
                r#"#!/usr/bin/env python
from distutils.core import setup
setup(name="foo", version="1.0.0", py_modules=["foo"])"#,
            )
            .unwrap();

        let result = PythonStrategy::write_version_to_setup_py(&setup_py, &Version::new(1, 1, 0));
        assert!(result.is_ok());
        setup_py.assert(
            r#"#!/usr/bin/env python
from distutils.core import setup
setup(name="foo", version="1.1.0", py_modules=["foo"])"#,
        );
    }

    /// Tests that a new version cannot be written to a `setup.py` file with an unknown structure.
    #[rstest]
    fn write_version_to_setup_py_with_unknown_structure() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let setup_py = temp_dir.child("setup.py");
        setup_py
            .write_str(
                r#"#!/usr/bin/env python
from distutils.core import setup
setup(name="foo", py_modules=["foo"])"#,
            )
            .unwrap();

        let result = PythonStrategy::write_version_to_setup_py(&setup_py, &Version::new(1, 1, 0));
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            format!("could not find `version` in `{}`", setup_py.display())
        );
    }
}
