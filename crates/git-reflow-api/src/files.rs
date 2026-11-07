use crate::plan::PackageRelease;
use crate::settings::pkg::PackageConfig;
use anyhow::{Context, anyhow, bail, ensure};
use handlebars::Handlebars;
use jsonc_parser::{ParseOptions, common::Ranged};
use regex::regex;
use serde_json::json;
use serde_json_path::{JsonPath, PathElement};
use std::cmp::Reverse;
use std::fs;
use std::io::BufWriter;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use toml_edit::DocumentMut;
use tracing::{debug, warn};
use yaml_edit::{
    ScalarStyle, ScalarType, ScalarValue, YamlFile,
    path::{PathError, YamlPath},
};

/// A file to be updated.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UpdateFile {
    /// The path to the file to be updated.
    pub path: PathBuf,
    /// If the file is a known file type, this field will contain the path to the field that should be updated.
    ///
    /// For JSON files, use JSONPath notation, e.g. `$.version` or `$.packages[*].version`.
    ///
    /// For TOML files, use dotted notation, e.g. `package.version` or `metadata.1.version`.
    ///
    /// For YAML files, use dotted notation, e.g. `package.version`, `metadata[1].version` or `metadata.app\.version`.
    #[serde(default)]
    pub field: Option<String>,
    /// The pattern used when updating the field's value in the file.
    ///
    /// The following placeholders are available:
    /// - `{{ package }}`: The name of the package being released.
    /// - `{{ version }}`: The new version being released (excluding 'v' prefix).
    /// - `{{ major }}`: The major version of the package.
    /// - `{{ minor }}`: The minor version of the package.
    /// - `{{ patch }}`: The patch version of the package.
    /// - `{{ pre }}`: The pre-release version of the package, if any.
    /// - `{{ build }}`: The build metadata in the version of the package, if any.
    ///
    /// **Default:** v{{ version }}
    #[serde(default = "default_value")]
    pub value: Option<String>,
}

/// Returns the default value for `$.value`.
fn default_value() -> Option<String> {
    Some(String::from("v{{ version }}"))
}

/// Updates a file based on the provided file update, package release information, and package configuration.
///
/// # Arguments
///
/// * `file` - The file update information.
/// * `release` - The package release information.
/// * `config` - The package configuration.
/// * `dry_run` - If true, do not actually write the changes.
///
/// # Returns
///
/// A result containing the changed file path if the field existed.
pub fn update_file(
    file: &UpdateFile,
    release: &PackageRelease,
    config: &PackageConfig,
    dry_run: bool,
) -> anyhow::Result<Option<PathBuf>> {
    // Normalise the path to the file relative to the package root.
    let path = config.dir.join(&file.path);
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                ensure!(
                    normalized.pop(),
                    "path `{}` escapes the repository root",
                    path.display()
                );
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!("path `{}` must be relative to the repository root", path.display());
            }
        }
    }

    // Short-circuit if the file does not exist.
    if !normalized.try_exists()? {
        warn!("file `{}` does not exist, skipping update", normalized.display());
        return Ok(None);
    }

    // Update the file based on its file type.
    match normalized.extension().and_then(|ext| ext.to_str()) {
        Some("json" | "json5" | "jsonc") => update_json_file(&normalized, file, release, dry_run),
        Some("toml") => update_toml_file(&normalized, file, release, dry_run),
        Some("yaml" | "yml") => update_yaml_file(&normalized, file, release, dry_run),
        _ => update_generic_file(&normalized, release, dry_run),
    }
}

/// Renders the value of a file update using the specified package release information.
///
/// # Arguments
///
/// * `file` - The file update information.
/// * `release` - The package release information.
///
/// # Returns
///
/// A result containing the rendered value.
pub fn render_value(file: &UpdateFile, release: &PackageRelease) -> anyhow::Result<String> {
    Handlebars::new()
        .render_template(
            file.value.as_deref().context("missing `value` for file update")?,
            &json!({
                "package": release.package_name,
                "version": release.next_version.to_string(),
                "major": release.next_version.major,
                "minor": release.next_version.minor,
                "patch": release.next_version.patch,
                "pre": release.next_version.pre.as_str(),
                "build": release.next_version.build.as_str(),
            }),
        )
        .context(format!("failed to render value for `{}`", file.path.display()))
}

/// Updates a JSON file at the given path, replacing the value of the specified field/s with the rendered value.
///
/// # Arguments
///
/// * `path` - The path to the JSON file to be updated.
/// * `file` - The file update information.
/// * `release` - The package release information.
/// * `dry_run` - If true, do not actually write the changes.
///
/// # Returns
///
/// A result containing the changed file path if the field existed.
pub fn update_json_file(
    path: &Path,
    file: &UpdateFile,
    release: &PackageRelease,
    dry_run: bool,
) -> anyhow::Result<Option<PathBuf>> {
    // Prepare the field and value to be updated.
    let field = file.field.as_deref().context("missing `field` for JSON file update")?;
    let value = render_value(file, release)?;

    // Parse the JSON file.
    let path_display = path.display();
    let mut contents = fs::read_to_string(path).map_err(|err| anyhow!("could not read `{path_display}`: {err}"))?;
    let data = jsonc_parser::parse_to_ast(
        &contents,
        &Default::default(),
        &ParseOptions {
            allow_missing_commas: false,
            ..Default::default()
        },
    )
    .map_err(|err| anyhow!("could not parse `{path_display}`: {err}"))?;
    let root = data
        .value
        .as_ref()
        .with_context(|| format!("could not parse `{path_display}`: missing JSON value"))?;

    // Resolve JSONPath matches to their original source ranges.
    let jsonpath =
        JsonPath::parse(field).with_context(|| format!("invalid JSONPath `{field}` for `{path_display}`"))?;
    let json: serde_json::Value = root.clone().into();
    let mut ranges = jsonpath
        .query_located(&json)
        .locations()
        .map(|location| {
            location
                .iter()
                .try_fold(root, |item, segment| match segment {
                    // Find the last property with the given name.
                    PathElement::Name(name) => item
                        .as_object()?
                        .properties
                        .iter()
                        .rev()
                        .find(|property| property.name.as_str() == *name)
                        .map(|property| &property.value),
                    // Find the array element at the given index.
                    PathElement::Index(index) => item.as_array()?.elements.get(*index),
                })
                // If the item is a number, mark it so that we can avoid quoting the replacement value.
                .map(|item| (item.range(), item.as_number_lit().is_some()))
                .with_context(|| format!("could not locate JSONPath match `{location}` in `{path_display}`"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Replacing an ancestor also replaces its descendants, so retain only distinct outermost matches.
    ranges.sort_unstable_by_key(|(range, _)| (range.start, Reverse(range.end)));
    ranges.dedup_by(|(range, _), (previous, _)| range.end <= previous.end);

    // If any of the matches are numbers, ensure that the new value is a valid JSON number.
    if ranges.iter().any(|(_, is_number)| *is_number) {
        value
            .parse::<serde_json::Number>()
            .with_context(|| format!("invalid JSON number `{value}` for `{field}` in `{path_display}`"))?;
    }

    // Update every match, working backwards to preserve the remaining source offsets.
    if !ranges.is_empty() {
        if !dry_run {
            debug!("set `{field}` to `{value}` at `{path_display}`");
            let replacement = serde_json::to_string(&value)
                .map_err(|err| anyhow!("could not serialize `{field}` for `{path_display}`: {err}"))?;
            for (range, is_number) in ranges.into_iter().rev() {
                contents.replace_range(range.start..range.end, if is_number { &value } else { &replacement });
            }
            fs::write(path, contents).map_err(|err| anyhow!("failed to write `{field}` to `{path_display}`: {err}"))?;
        } else {
            for (range, _) in ranges {
                debug!("set `{field}` to `{value}` ({range:?}) at `{path_display}` (dry run)");
            }
        }
        Ok(Some(path.to_path_buf()))
    } else {
        warn!("refusing to write non-existent `{field}` to `{value}` at `{path_display}`");
        Ok(None)
    }
}

/// Updates a TOML file at the given path, replacing the value of the specified field with the rendered value.
///
/// # Arguments
///
/// * `path` - The path to the TOML file to be updated.
/// * `file` - The file update information.
/// * `release` - The package release information.
/// * `dry_run` - If true, do not actually write the changes.
///
/// # Returns
///
/// A result containing the changed file path if the field existed.
pub fn update_toml_file(
    path: &Path,
    file: &UpdateFile,
    release: &PackageRelease,
    dry_run: bool,
) -> anyhow::Result<Option<PathBuf>> {
    // Prepare the field and value to be updated.
    let field = file.field.as_deref().context("missing `field` for TOML file update")?;
    let value = render_value(file, release)?;

    // Parse the TOML file.
    let path_display = path.display();
    let contents = fs::read_to_string(path).map_err(|err| anyhow!("could not read `{path_display}`: {err}"))?;
    let mut data = contents
        .parse::<DocumentMut>()
        .map_err(|err| anyhow!("could not parse `{path_display}`: {err}"))?;

    // Resolve dotted table keys and array indices without creating missing entries.
    let item = field.split('.').try_fold(data.as_item_mut(), |item, segment| {
        if item.is_array() || item.is_array_of_tables() {
            item.get_mut(segment.parse::<usize>().ok()?)
        } else {
            item.get_mut(segment)
        }
    });

    // Update the field if it exists.
    if let Some(current) = item.and_then(toml_edit::Item::as_value_mut) {
        if !dry_run {
            debug!("set `{field}` to `{value}` at `{path_display}`");
            let mut replacement = toml_edit::Value::from(value);
            *replacement.decor_mut() = current.decor().clone();
            *current = replacement;
            fs::write(path, data.to_string())
                .map_err(|err| anyhow!("failed to write `{field}` to `{path_display}`: {err}"))?;
        } else {
            debug!("set `{field}` to `{value}` at `{path_display}` (dry run)");
        }
        Ok(Some(path.to_path_buf()))
    } else {
        debug!("refusing to write non-existent `{field}` to `{value}` at `{path_display}`");
        Ok(None)
    }
}

/// Updates a YAML file at the given path, replacing the value of the specified field with the rendered value.
///
/// # Arguments
///
/// * `path` - The path to the YAML file to be updated.
/// * `file` - The file update information.
/// * `release` - The package release information.
/// * `dry_run` - If true, do not actually write the changes.
///
/// # Returns
///
/// A result containing the changed file path if the field existed.
pub fn update_yaml_file(
    path: &Path,
    file: &UpdateFile,
    release: &PackageRelease,
    dry_run: bool,
) -> anyhow::Result<Option<PathBuf>> {
    // Prepare the field and value to be updated.
    let field = file.field.as_deref().context("missing `field` for YAML file update")?;
    let value = render_value(file, release)?;

    // Parse the YAML file.
    let path_display = path.display();
    let contents = fs::read_to_string(path).map_err(|err| anyhow!("could not read `{path_display}`: {err}"))?;
    let data = contents
        .parse::<YamlFile>()
        .map_err(|err| anyhow!("could not parse `{path_display}`: {err}"))?;
    let mut documents = data.documents();
    let document = documents
        .next()
        .with_context(|| format!("could not parse `{path_display}`: missing YAML value"))?;
    ensure!(
        documents.next().is_none(),
        "could not parse `{path_display}`: multiple YAML documents are not supported"
    );

    // Find the current value at the specified path.
    let current = match document.try_get_path(field) {
        // The field exists, use it.
        Ok(current) => current,
        // The field does not exist, return.
        Err(PathError::NotFound { .. }) => {
            warn!("refusing to write non-existent `{field}` to `{value}` at `{path_display}`");
            return Ok(None);
        }
        // The field could not be read, bail.
        Err(err) => return Err(anyhow!("could not read YAML path `{field}` in `{path_display}`: {err}")),
    };

    // Determine the replacement value based on the current value's type and style.
    let replacement = match current.as_scalar().map(ScalarValue::from_scalar) {
        // If the current value is a number, ensure that the new value is a valid YAML number.
        Some(current) if matches!(current.scalar_type(), ScalarType::Integer | ScalarType::Float) => {
            value
                .parse::<serde_json::Number>()
                .with_context(|| format!("invalid YAML number `{value}` for `{field}` in `{path_display}`"))?;
            ScalarValue::parse(value.as_str())
        }
        // If the current value is a string with a non-plain style, preserve that style in the replacement.
        Some(current) if current.style() != ScalarStyle::Plain => {
            ScalarValue::with_style(value.as_str(), current.style())
        }
        // Otherwise, treat the replacement value as a plain string.
        _ => ScalarValue::string(value.as_str()),
    };

    // Update the field.
    if !dry_run {
        debug!("set `{field}` to `{value}` at `{path_display}`");
        document
            .try_set_path(field, replacement)
            .with_context(|| format!("failed to update YAML path `{field}` in `{path_display}`"))?;
        fs::write(path, data.to_string())
            .map_err(|err| anyhow!("failed to write `{field}` to `{path_display}`: {err}"))?;
    } else {
        debug!("set `{field}` to `{value}` at `{path_display}` (dry run)");
    }
    Ok(Some(path.to_path_buf()))
}

/// Updates a generic file at the given path, replacing the value inside comment blocks with the rendered value.
///
/// # Arguments
///
/// * `path` - The path to the file to be updated.
/// * `release` - The package release information.
/// * `dry_run` - If true, do not actually write the changes.
///
/// # Returns
///
/// A result containing the changed file path if a comment block existed.
pub fn update_generic_file(path: &Path, release: &PackageRelease, dry_run: bool) -> anyhow::Result<Option<PathBuf>> {
    // Prepare the regex patterns for inline and block comments, as well as the value matching patterns.
    let inline_pattern = regex!(r"x-git-reflow-(?<scope>version|major|minor|patch)(\s+(?<pkg>[\w.]+[\w.-]+))?");
    let block_start_pattern =
        regex!(r"x-git-reflow-start-(?<scope>version|major|minor|patch)(\s+(?<pkg>[\w.]+[\w.-]+))?");
    let block_end_pattern = regex!(r"x-git-reflow-end-(?<scope>version|major|minor|patch)");
    let version_pattern =
        regex!(r"(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(-(?<pre>[\w.]+))?(\+(?<build>[-\w.]+))?");
    let single_version_pattern = regex!(r"\d+\b");

    enum BlockScope {
        Version, // The entire version string, e.g. `1.2.3-beta+build`.
        Major,   // The major version number, e.g. `1` in `1.2.3`.
        Minor,   // The minor version number, e.g. `2` in `1.2.3`.
        Patch,   // The patch version number, e.g. `3` in `1.2.3`.
    }

    // Read the file.
    let path_display = path.display();
    let contents = fs::read_to_string(path).map_err(|err| anyhow!("could not read `{path_display}`: {err}"))?;

    // For each line, look for an inline-comment or a block-comment.
    let mut changed = false;
    let mut in_block: Option<BlockScope> = None;
    let mut new_contents = BufWriter::new(Vec::with_capacity(contents.len()));
    for line in contents.lines() {
        writeln!(
            new_contents,
            "{}",
            // If the line contains an inline comment, replace the value on the same line.
            if let Some(captures) = inline_pattern.captures(line) {
                if captures
                    .name("pkg")
                    .filter(|pkg| pkg.as_str() != release.package_name)
                    .is_some()
                {
                    line.to_string()
                } else {
                    let new_line = match captures.name("scope").unwrap().as_str() {
                        "version" => version_pattern.replace(line, release.next_version.to_string()),
                        "major" => single_version_pattern.replace(line, release.next_version.major.to_string()),
                        "minor" => single_version_pattern.replace(line, release.next_version.minor.to_string()),
                        "patch" => single_version_pattern.replace(line, release.next_version.patch.to_string()),
                        _ => unreachable!(),
                    }
                    .to_string();
                    changed |= !new_line.eq(line);
                    new_line
                }

            // If the line contains a start block comment, enter block mode and leave the line unchanged.
            } else if let Some(captures) = block_start_pattern.captures(line) {
                if captures
                    .name("pkg")
                    .filter(|pkg| pkg.as_str() != release.package_name)
                    .is_some()
                {
                    line.to_string()
                } else {
                    in_block = match captures.name("scope").unwrap().as_str() {
                        "version" => Some(BlockScope::Version),
                        "major" => Some(BlockScope::Major),
                        "minor" => Some(BlockScope::Minor),
                        "patch" => Some(BlockScope::Patch),
                        _ => unreachable!(),
                    };
                    line.to_string()
                }

            // If the line contains an end block comment, exit block mode and leave the line unchanged.
            } else if block_end_pattern.is_match(line) {
                in_block = None;
                line.to_string()

            // If we are in block mode, replace the value on the line based on the block scope.
            } else if let Some(scope) = &in_block {
                let new_line = match scope {
                    BlockScope::Version => version_pattern.replace(line, release.next_version.to_string()),
                    BlockScope::Major => single_version_pattern.replace(line, release.next_version.major.to_string()),
                    BlockScope::Minor => single_version_pattern.replace(line, release.next_version.minor.to_string()),
                    BlockScope::Patch => single_version_pattern.replace(line, release.next_version.patch.to_string()),
                }
                .to_string();
                changed |= !new_line.eq(line);
                new_line

            // Otherwise, leave the line unchanged.
            } else {
                line.to_string()
            },
        )?;
    }

    // Write the updated file back if there were any changes.
    if changed {
        if !dry_run {
            debug!("write updated `{path_display}`");
            fs::write(path, new_contents.into_inner()?)
                .map_err(|err| anyhow!("failed to write to `{path_display}`: {err}"))?;
        } else {
            debug!("write updated `{path_display} (dry run)`");
        }
    }

    Ok(if changed { Some(path.to_path_buf()) } else { None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::{TempDir, prelude::*};
    use indoc::indoc;
    use rstest::{fixture, rstest};
    use semver::Version;
    use std::collections::HashMap;

    /// A test fixture for a temporary directory with a package configuration.
    #[fixture]
    fn package() -> (TempDir, PackageConfig) {
        let temp_dir = TempDir::new().unwrap();
        let config = PackageConfig {
            dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        (temp_dir, config)
    }

    /// A test fixture for package release information.
    #[fixture]
    fn release(#[default("1.1.0")] version: &str) -> PackageRelease {
        PackageRelease {
            package_name: "magic".to_string(),
            current_version: None,
            next_version: Version::parse(version).unwrap(),
            commit_message: "".to_string(),
            changelog_md: "".to_string(),
            dependencies: HashMap::new(),
            context: serde_json::Value::Null,
        }
    }

    /// Tests that a JSON file can be updated.
    #[rstest]
    fn test_update_json_file(package: (TempDir, PackageConfig), #[with("2.0.0")] release: PackageRelease) {
        // Create a temporary JSON file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.json");
        file.write_str("{\"metadata\": {\"tagMajor\": \"v1\"}}\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("$.metadata.tagMajor".to_string()),
            value: Some("v{{ major }}".to_string()),
        };

        // Update the JSON file and verify the result.
        let result = update_json_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert("{\"metadata\": {\"tagMajor\": \"v2\"}}\n");
    }

    /// Tests that a JSON file with a JSONPath that matches multiple fields can be updated.
    #[rstest]
    fn test_update_json_file_with_multiple_matches(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary JSON file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.jsonc");
        file.write_str(indoc! {r#"
                {
                    "metadata": [
                        {
                            "name": "redherring",
                            "version": "1.0.0"
                        },
                        {
                            "name": "magic",
                            "version": "1.0.0"
                        }
                    ]
                }
            "#})
            .unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("$.metadata[*].version".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the JSON file and verify the result.
        let result = update_json_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            {
                "metadata": [
                    {
                        "name": "redherring",
                        "version": "1.1.0"
                    },
                    {
                        "name": "magic",
                        "version": "1.1.0"
                    }
                ]
            }
        "#});
    }

    /// Tests that a JSON file with a field as a number type can be updated.
    #[rstest]
    fn test_update_json_file_with_number_values(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary JSON file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.jsonc");
        file.write_str(indoc! {r#"
                {
                    "tagMajor": 1,
                    "tagMinor": 0, // This is the field being updated, note it is a number not a string.
                    "tagPatch": 0
                }
            "#})
            .unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("$.tagMinor".to_string()),
            value: Some("{{ minor }}".to_string()),
        };

        // Update the JSON file and verify the result.
        let result = update_json_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            {
                "tagMajor": 1,
                "tagMinor": 1, // This is the field being updated, note it is a number not a string.
                "tagPatch": 0
            }
        "#});
    }

    /// Tests that a JSON file update fails when the `field` is not configured.
    #[rstest]
    fn test_update_json_file_with_unconfigured_field(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary JSON file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.json");
        file.write_str("{\"metadata\": {\"tagMajor\": \"v1\"}}\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: None, // NB: The unconfigured field should trigger an error.
            value: Some("{{ version }}".to_string()),
        };

        // Update the JSON file and verify that the file remains unchanged.
        let result = update_json_file(&file, &update_file, &release, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "missing `field` for JSON file update");
        file.assert("{\"metadata\": {\"tagMajor\": \"v1\"}}\n");
    }

    /// Tests that a JSON file is not updated when the `field` does not exist in the file.
    #[rstest]
    fn test_update_json_file_with_nonexistent_field(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary JSON file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.json");
        file.write_str("{\"metadata\": {\"tagMajor\": \"v1\"}}\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("$.metadata.nonexistent".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the JSON file and verify that the file remains unchanged.
        let result = update_json_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), None);
        file.assert("{\"metadata\": {\"tagMajor\": \"v1\"}}\n");
    }

    /// Tests that a JSON file is *not* written to when performing a dry run.
    #[rstest]
    fn test_update_json_file_with_dry_run(package: (TempDir, PackageConfig), #[with("2.0.0")] release: PackageRelease) {
        // Create a temporary JSON file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.json");
        file.write_str("{\"metadata\": {\"tagMajor\": \"v1\"}}\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("$.metadata.tagMajor".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the JSON file with dry run enabled and verify that the file remains unchanged.
        let result = update_json_file(&file, &update_file, &release, true);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert("{\"metadata\": {\"tagMajor\": \"v1\"}}\n");
    }

    /// Tests that a TOML file can be updated.
    #[rstest]
    fn test_update_toml_file(package: (TempDir, PackageConfig), #[with("2.0.0")] release: PackageRelease) {
        // Create a temporary TOML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.toml");
        file.write_str("[metadata]\ntag-major = \"v1\"\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("metadata.tag-major".to_string()),
            value: Some("v{{ major }}".to_string()),
        };

        // Update the TOML file and verify the result.
        let result = update_toml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert("[metadata]\ntag-major = \"v2\"\n");
    }

    /// Tests that a TOML file with tables can be updated.
    #[rstest]
    fn test_update_toml_file_with_tables(package: (TempDir, PackageConfig), #[with("1.1.0")] release: PackageRelease) {
        // Create a temporary TOML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.toml");
        file.write_str(indoc! {r#"
                [[metadata]]
                name = "redherring"
                version = "1.0.0"

                [[metadata]]
                name = "magic"
                # DO NOT manually edit the version.
                version = "1.0.0"
            "#})
            .unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("metadata.1.version".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the TOML file and verify the result.
        let result = update_toml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            [[metadata]]
            name = "redherring"
            version = "1.0.0"

            [[metadata]]
            name = "magic"
            # DO NOT manually edit the version.
            version = "1.1.0"
        "#});
    }

    /// Tests that a TOML file update fails when the `field` is not configured.
    #[rstest]
    fn test_update_toml_file_with_unconfigured_field(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary TOML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.toml");
        file.write_str("[metadata]\ntag-major = \"v1\"\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: None, // NB: The unconfigured field should trigger an error.
            value: Some("{{ version }}".to_string()),
        };

        // Update the TOML file and verify that the file remains unchanged.
        let result = update_toml_file(&file, &update_file, &release, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "missing `field` for TOML file update");
        file.assert("[metadata]\ntag-major = \"v1\"\n");
    }

    /// Tests that a TOML file is not updated when the `field` does not exist in the file.
    #[rstest]
    fn test_update_toml_file_with_nonexistent_field(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary TOML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.toml");
        file.write_str("[metadata]\ntag-major = \"v1\"\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("metadata.nonexistent".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the TOML file and verify that the file remains unchanged.
        let result = update_toml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), None);
        file.assert("[metadata]\ntag-major = \"v1\"\n");
    }

    /// Tests that a TOML file is *not* written to when performing a dry run.
    #[rstest]
    fn test_update_toml_file_with_dry_run(package: (TempDir, PackageConfig), #[with("2.0.0")] release: PackageRelease) {
        // Create a temporary TOML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.toml");
        file.write_str("[metadata]\ntag-major = \"v1\"\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("metadata.tag-major".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the TOML file with dry run enabled and verify that the file remains unchanged.
        let result = update_toml_file(&file, &update_file, &release, true);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert("[metadata]\ntag-major = \"v1\"\n");
    }

    /// Tests that a YAML file can be updated.
    #[rstest]
    fn test_update_yaml_file(package: (TempDir, PackageConfig), #[with("2.0.0")] release: PackageRelease) {
        // Create a temporary YAML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.yaml");
        file.write_str("metadata:\n  tagMajor: v1\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("metadata.tagMajor".to_string()),
            value: Some("v{{ major }}".to_string()),
        };

        // Update the YAML file and verify the result.
        let result = update_yaml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert("metadata:\n  tagMajor: v2\n");
    }

    /// Tests that a YAML file's sequence entry can be updated using bracket or dotted indices.
    #[rstest]
    fn test_update_yaml_file_with_sequences(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
        #[values("metadata[1].version", "metadata.1.version")] field: &str,
    ) {
        // Create a temporary YAML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.yml");
        file.write_str(indoc! {r#"
                metadata:
                  - name: redherring
                    version: "1.0.0"
                  - name: magic
                    # DO NOT manually edit the version.
                    version: '1.0.0'
            "#})
            .unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some(field.to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the YAML file and verify the result.
        let result = update_yaml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            metadata:
              - name: redherring
                version: "1.0.0"
              - name: magic
                # DO NOT manually edit the version.
                version: '1.1.0'
        "#});
    }

    /// Tests that a YAML file with a field as a number type can be updated.
    #[rstest]
    fn test_update_yaml_file_with_number_values(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary YAML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.yml");
        file.write_str(indoc! {r#"
                tagMajor: 1
                tagMinor: 0 # This is the field being updated, note it is a number not a string.
                tagPatch: 0
            "#})
            .unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("tagMinor".to_string()),
            value: Some("{{ minor }}".to_string()),
        };

        // Update the YAML file and verify the result.
        let result = update_yaml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            tagMajor: 1
            tagMinor: 1 # This is the field being updated, note it is a number not a string.
            tagPatch: 0
        "#});
    }

    /// Tests that a YAML file update fails when the `field` is not configured.
    #[rstest]
    fn test_update_yaml_file_with_unconfigured_field(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
    ) {
        // Create a temporary YAML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.yaml");
        file.write_str("metadata:\n  tagMajor: v1\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: None, // NB: The unconfigured field should trigger an error.
            value: Some("{{ version }}".to_string()),
        };

        // Update the YAML file and verify that the file remains unchanged.
        let result = update_yaml_file(&file, &update_file, &release, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "missing `field` for YAML file update");
        file.assert("metadata:\n  tagMajor: v1\n");
    }

    /// Tests that a YAML file is not updated when the `field` does not exist in the file.
    #[rstest]
    fn test_update_yaml_file_with_nonexistent_field(
        package: (TempDir, PackageConfig),
        #[with("1.1.0")] release: PackageRelease,
        #[values("metadata.nonexistent", "missing.version", "versions[1]")] field: &str,
    ) {
        // Create a temporary YAML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.yaml");
        file.write_str("metadata:\n  tagMajor: v1\nversions: [v1]\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some(field.to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the YAML file and verify that the file remains unchanged.
        let result = update_yaml_file(&file, &update_file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), None);
        file.assert("metadata:\n  tagMajor: v1\nversions: [v1]\n");
    }

    /// Tests that a YAML file is *not* written to when performing a dry run.
    #[rstest]
    fn test_update_yaml_file_with_dry_run(package: (TempDir, PackageConfig), #[with("2.0.0")] release: PackageRelease) {
        // Create a temporary YAML file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("metadata.yaml");
        file.write_str("metadata:\n  tagMajor: v1\n").unwrap();

        // Prepare the update file.
        let update_file = UpdateFile {
            path: file.path().to_path_buf(),
            field: Some("metadata.tagMajor".to_string()),
            value: Some("{{ version }}".to_string()),
        };

        // Update the YAML file with dry run enabled and verify that the file remains unchanged.
        let result = update_yaml_file(&file, &update_file, &release, true);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert("metadata:\n  tagMajor: v1\n");
    }

    /// Tests that a generic file with inline comments can be updated.
    #[rstest]
    fn test_update_generic_file_with_inline_comments(
        package: (TempDir, PackageConfig),
        #[with("1.2.3-rc.2+def45gh")] release: PackageRelease,
    ) {
        // Create a temporary file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("README.md");
        file.write_str(indoc! {r#"
                # example-project

                This is an example project.

                **Latest version:** v1.0.0-rc.1+abc12de <!-- x-git-reflow-version -->

                **Major version:** @v1 <!-- x-git-reflow-major -->
                **Minor version:** @v0 <!-- x-git-reflow-minor -->
                **Patch version:** @v0 <!-- x-git-reflow-patch -->

                **In-scope version:** v0.0.1 <!-- x-git-reflow-version magic -->
                **Out-of-scope version:** v0.0.1 <!-- x-git-reflow-version red-herring -->
            "#})
            .unwrap();

        // Update the generic file and verify the result.
        let result = update_generic_file(&file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            # example-project

            This is an example project.

            **Latest version:** v1.2.3-rc.2+def45gh <!-- x-git-reflow-version -->

            **Major version:** @v1 <!-- x-git-reflow-major -->
            **Minor version:** @v2 <!-- x-git-reflow-minor -->
            **Patch version:** @v3 <!-- x-git-reflow-patch -->

            **In-scope version:** v1.2.3-rc.2+def45gh <!-- x-git-reflow-version magic -->
            **Out-of-scope version:** v0.0.1 <!-- x-git-reflow-version red-herring -->
        "#});
    }

    /// Tests that a generic file with block comments can be updated.
    #[rstest]
    fn test_update_generic_file_with_block_comments(
        package: (TempDir, PackageConfig),
        #[with("1.2.3-rc.4+build.5")] release: PackageRelease,
    ) {
        // Create a temporary file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("README.md");
        file.write_str(indoc! {r#"
                # example-project

                This is an example project.

                <!-- x-git-reflow-start-version -->
                **Latest version:** v1.0.0-rc.2+build.2
                **Previous version:** v1.0.0-rc.1+build.1 <!-- both are changed in the same block -->
                <!-- x-git-reflow-end-version -->

                <!-- x-git-reflow-start-major -->
                **Major version:** @v1
                <!-- x-git-reflow-end-major -->

                <!-- x-git-reflow-start-minor -->
                **Minor version:** @v0
                <!-- x-git-reflow-end-minor -->

                <!-- x-git-reflow-start-patch -->
                **Patch version:** @v0
                <!-- x-git-reflow-end-patch -->

                <!-- x-git-reflow-start-version magic -->
                **In-scope version:** v0.0.1
                <!-- x-git-reflow-end-version -->

                <!-- x-git-reflow-start-version red-herring -->
                **Out-of-scope version:** v0.0.1
                <!-- x-git-reflow-end-version -->
            "#})
            .unwrap();

        // Update the generic file and verify the result.
        let result = update_generic_file(&file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(indoc! {r#"
            # example-project

            This is an example project.

            <!-- x-git-reflow-start-version -->
            **Latest version:** v1.2.3-rc.4+build.5
            **Previous version:** v1.2.3-rc.4+build.5 <!-- both are changed in the same block -->
            <!-- x-git-reflow-end-version -->

            <!-- x-git-reflow-start-major -->
            **Major version:** @v1
            <!-- x-git-reflow-end-major -->

            <!-- x-git-reflow-start-minor -->
            **Minor version:** @v2
            <!-- x-git-reflow-end-minor -->

            <!-- x-git-reflow-start-patch -->
            **Patch version:** @v3
            <!-- x-git-reflow-end-patch -->

            <!-- x-git-reflow-start-version magic -->
            **In-scope version:** v1.2.3-rc.4+build.5
            <!-- x-git-reflow-end-version -->

            <!-- x-git-reflow-start-version red-herring -->
            **Out-of-scope version:** v0.0.1
            <!-- x-git-reflow-end-version -->
        "#});
    }

    /// Tests that a generic file is *not* written to when no inline or block comments exist.
    #[rstest]
    fn test_update_generic_file_with_no_changes(
        package: (TempDir, PackageConfig),
        #[with("2.0.0")] release: PackageRelease,
    ) {
        // Create a temporary file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("README.md");
        let contents = indoc! {r#"
            # example-project

            This is an example project.

            <!-- x-git-reflow-start-version -->
            **Latest version:** v2.0.0
            <!-- x-git-reflow-end-version -->

            **Major version:** @v2 <!-- x-git-reflow-major -->
        "#};
        file.write_str(&contents).unwrap();

        // Update the generic file and verify that the file remains unchanged.
        let result = update_generic_file(&file, &release, false);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert!(result.unwrap().is_none());
        file.assert(contents);
    }

    /// Tests that a generic file is *not* written to when performing a dry run.
    #[rstest]
    fn test_update_generic_file_with_dry_run(
        package: (TempDir, PackageConfig),
        #[with("2.0.0")] release: PackageRelease,
    ) {
        // Create a temporary file to be updated.
        let (temp_dir, _config) = package;
        let file = temp_dir.child("README.md");
        let contents = indoc! {r#"
            # example-project

            This is an example project.

            **Latest version:** v1.0.0-rc.1+abc12de <!-- x-git-reflow-version -->

            **Major version:** @v1 <!-- x-git-reflow-major -->
            **Minor version:** @v0 <!-- x-git-reflow-minor -->
            **Patch version:** @v0 <!-- x-git-reflow-patch -->
        "#};
        file.write_str(&contents).unwrap();

        // Update the generic file with dry run enabled and verify that the file remains unchanged.
        let result = update_generic_file(&file, &release, true);
        assert!(result.is_ok(), "failed to update file: {:?}", result);
        assert_eq!(result.unwrap(), Some(file.path().to_path_buf()));
        file.assert(contents);
    }
}
