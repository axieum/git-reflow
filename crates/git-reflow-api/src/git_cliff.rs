use anyhow::{Context, anyhow, bail};
use semver::Version;
use serde_json::Value;
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::str;
use std::str::FromStr;
use tracing::{debug, trace};

/// Trait to abstract `git-cliff` command execution.
pub trait CommandRunner {
    fn run<'a>(&self, args: &'a [&'a str], input: Option<&'a str>) -> anyhow::Result<Output>;
}

/// The `git-cliff` command executor.
pub struct GitCliffRunner;

impl CommandRunner for GitCliffRunner {
    fn run<'a>(&self, args: &'a [&'a str], input: Option<&'a str>) -> anyhow::Result<Output> {
        let mut child = Command::new("git-cliff")
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn `git-cliff` process")?;

        if let Some(data) = input
            && let Some(mut stdin) = child.stdin.take()
        {
            stdin
                .write_all(data.as_bytes())
                .context("failed to write JSON context to `git-cliff` stdin")?;
        }

        child
            .wait_with_output()
            .context("failed to wait for `git-cliff` process")
    }
}

/// Spawns a [`git-cliff`](https://github.com/orhun/git-cliff) process in a given directory and
/// returns the context as a parsed JSON object.
///
/// > `git-cliff --include-path ${dir}/**/* --tag-pattern ${tag_prefix}.* --unreleased --bump --context`
///
/// # Arguments
///
/// * `dir` - The path to scope the commits to.
/// * `tag_prefix` - The package's release tag prefix, e.g. `v` or `my-package-v`.
/// * `extra_commits` - A list of synthetic commit messages, such as dependency updates, if any.
/// * `runner` - The `git-cliff` command runner.
///
/// # Returns
///
/// A result containing the parsed `git-cliff` output context for unreleased changes.
pub fn run_git_cliff(
    dir: &Path,
    tag_prefix: &str,
    extra_commits: &[String],
    runner: Option<&dyn CommandRunner>,
) -> anyhow::Result<Option<Value>> {
    // Prepare `git-cliff` arguments.
    let include_path = match dir.to_str() {
        Some(".") => Path::new("**").join("*"), // git-cliff would reject `./**/*`
        _ => dir.join("**").join("*"),
    };
    let include_path = include_path.to_string_lossy();
    let tag_pattern = format!(r"^{}.*$", regex::escape(tag_prefix));
    let mut args = vec![
        "--include-path",
        &include_path,
        "--tag-pattern",
        &tag_pattern,
        "--unreleased",
        "--bump",
        "--context",
    ];
    for message in extra_commits {
        args.extend(["--with-commit", message]);
    }
    debug!("$ git-cliff {}", &args.join(" "));

    // Invoke the `git-cliff` command.
    let runner = runner.unwrap_or(&GitCliffRunner);
    let output = runner.run(&args, None)?;

    // Check the `git-cliff` output.
    if output.status.success() {
        let json_str = str::from_utf8(&output.stdout)?;
        match serde_json::from_str::<Value>(json_str) {
            Ok(mut context) => {
                if let Some(object) = context.as_array_mut().and_then(|a| a.first_mut()) {
                    // Strip tag prefixes retained by `git-cliff`, e.g. `my-package-v1.2.3` becomes `1.2.3`.
                    for path in ["/version", "/previous/version"] {
                        if let Some(value) = object.pointer_mut(path)
                            && let Some(version) = value.as_str()
                        {
                            let stripped_version = version
                                .strip_prefix(tag_prefix)
                                .unwrap_or(version)
                                .trim_start_matches('v')
                                .to_string();
                            *value = Value::String(stripped_version);
                        }
                    }
                    // Trace the parsed context and return.
                    trace!("↳ {}", serde_json::to_string_pretty(object)?);
                    return Ok(Some(object.clone()));
                }
                Ok(None)
            }
            Err(err) => bail!("failed to parse git-cliff JSON output: {}", err),
        }
    } else {
        bail!("git-cliff error: {}", str::from_utf8(&output.stderr)?)
    }
}

/// Spawns a [`git-cliff`](https://github.com/orhun/git-cliff) process and renders the Markdown
/// changelog for the given context to a string.
///
/// > `git-cliff --from-context - --output -`
///
/// # Arguments
///
/// * `context` - The `git-cliff` context data.
/// * `runner` - The `git-cliff` command runner.
///
/// # Returns
///
/// A result containing the rendered changelog Markdown for the given `git-cliff` context.
pub fn render_changelog_markdown(context: &Value, runner: Option<&dyn CommandRunner>) -> anyhow::Result<String> {
    // Serialise the `git-cliff` context to JSON and prepare the command arguments.
    let context_json = serde_json::to_string(&[context]).context("failed to serialize context")?;
    let args = ["--from-context", "-", "--output", "-"];

    // Invoke the `git-cliff` command.
    trace!("$ git-cliff --from-context - --output -");
    let runner = runner.unwrap_or(&GitCliffRunner);
    let output = runner.run(&args, Some(&context_json))?;

    // Check the `git-cliff` output.
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)
            .context("git-cliff output was not valid UTF-8")?
            .trim()
            .to_string())
    } else {
        bail!("git-cliff error: {}", str::from_utf8(&output.stderr)?)
    }
}

/// Prepends the given changelog Markdown to a changelog file.
///
/// # Arguments
///
/// * `path` - The path to the changelog file.
/// * `changelog_md` - The changelog Markdown content to prepend.
///
/// # Returns
///
/// A result of whether the changelog was successfully written to.
pub fn write_changelog_markdown(path: &Path, changelog_md: &str) -> anyhow::Result<()> {
    // Create a temporary `CHANGELOG.md` file to stream the new changelog content.
    let temp_file = tempfile::NamedTempFile::new().context("failed to create temp changelog file")?;
    let mut writer = BufWriter::new(&temp_file);

    // Write the new changelog content to the file.
    writer
        .write_all(changelog_md.as_bytes())
        .context("failed to write new changelog content")?;
    writer
        .write(b"\n")
        .context("failed to write new line after new changelog content")?;

    // Write the existing changelog content to the file, if it exists.
    if path.exists() {
        let existing_file = fs::File::open(path)
            .with_context(|| format!("failed to open existing changelog file `{}`", path.display()))?;
        let mut reader = BufReader::new(existing_file);
        writer
            .write(b"\n")
            .context("failed to write new line before existing changelog content")?;
        std::io::copy(&mut reader, &mut writer).context("failed to write existing changelog content")?;
    }

    writer.flush().context("failed to flush changelog content")?;
    drop(writer);

    // Move the new changelog file to the target path.
    if !path.exists()
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create changelog directory `{}`", parent.display()))?;
    }
    fs::rename(&temp_file, path)
        .with_context(|| format!("failed to rename new changelog file to `{}`", path.display()))?;

    Ok(())
}

/// Returns the parsed [Semantic Version](https://semver.org/) from the `git-cliff` context.
///
/// # Arguments
///
/// * `context` - The `git-cliff` context data.
/// * `tag_prefix` - The package's release tag prefix to remove before parsing.
///
/// # Returns
///
/// A result containing the current (`$.previous.version`) and next (`$.version`)
/// versions as parsed [Semantic Version](https://semver.org/).
pub fn get_version_from_git_cliff_context(
    context: &Value,
    tag_prefix: &str,
) -> anyhow::Result<(Option<Version>, Version)> {
    let parse_version = |version: &str| {
        let version = version
            .strip_prefix(tag_prefix)
            .unwrap_or(version)
            .trim_start_matches('v');
        Version::from_str(version).map_err(|err| anyhow!("invalid semver version in git-cliff context: {}", err))
    };
    let current_version = context
        .get("previous")
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_str())
        .map(parse_version)
        .transpose()?;

    let next_version = context
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing version in git-cliff context"))
        .and_then(parse_version)?;

    Ok((current_version, next_version))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use indoc::indoc;
    use mockall::mock;
    use rstest::rstest;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;

    const TAG_PATTERN: &str = r"^v.*$";

    mock! {
        /// The mock `git-cliff` command executor.
        pub(crate) GitCliffRunner {}

        impl CommandRunner for GitCliffRunner {
            fn run<'a>(&self, args: &'a [&'a str], input: Option<&'a str>) -> anyhow::Result<Output>;
        }
    }

    /// Tests that the JSON context from running `git-cliff` is parsed and returned.
    #[test]
    fn test_run_git_cliff_with_success() {
        let include_dir = Path::new("crates").join("pkg-a");
        let include_glob = include_dir.join("**").join("*");

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: r#"[{"version": "1.0.0"}]"#.as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        let result = run_git_cliff(&include_dir, "v", &[], Some(&runner));

        assert!(result.is_ok());
        assert_eq!(result.unwrap().unwrap(), serde_json::json!({"version": "1.0.0"}));
    }

    /// Tests that tag prefixes are stripped from the JSON context when running `git-cliff` with tag prefixes.
    #[test]
    fn test_run_git_cliff_with_tag_prefix() {
        let include_dir = Path::new("crates").join("pkg-a");
        let include_glob = include_dir.join("**").join("*");
        let tag_prefix = "example-api-v";

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    "^example\\-api\\-v.*$",
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: r#"[{"version": "example-api-v1.1.0", "previous": {"version": "example-api-v1.0.0"}}]"#
                        .as_bytes()
                        .to_vec(),
                    stderr: vec![],
                })
            });

        let result = run_git_cliff(&include_dir, tag_prefix, &[], Some(&runner));

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().unwrap(),
            serde_json::json!({"version": "1.1.0", "previous": {"version": "1.0.0"}}),
        );
    }

    /// Tests that each synthetic commit message is passed to `git-cliff` and returned in its context.
    #[test]
    fn test_run_git_cliff_with_extra_commits() {
        // Prepare the include path and synthetic dependency updates.
        let include_dir = Path::new("crates").join("pkg-a");
        let include_glob = include_dir.join("**").join("*");
        let extra_commits = [
            "fix(deps): update dependency `api` to v1.1.0".to_string(),
            "fix(deps): update dependency `core` to v2.0.0".to_string(),
        ];

        // Expect a separate argument pair for each synthetic commit.
        let expected_commits = extra_commits.clone();
        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                    "--with-commit",
                    &expected_commits[0],
                    "--with-commit",
                    &expected_commits[1],
                ]
            })
            .returning(|_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: r#"[{"version": "1.0.0", "commits": [{"raw_message": "fix(deps): update dependency `api` to v1.1.0"}, {"raw_message": "fix(deps): update dependency `core` to v2.0.0"}]}]"#.as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        // Run `git-cliff` with all synthetic commits.
        let result = run_git_cliff(&include_dir, "v", &extra_commits, Some(&runner));

        // Verify that the returned context retains every synthetic commit exactly once.
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().unwrap()["commits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|commit| commit["raw_message"].as_str().unwrap())
                .collect::<Vec<_>>(),
            extra_commits
        );
    }

    /// Tests that an empty JSON context from running `git-cliff` is handled gracefully.
    #[test]
    fn test_run_git_cliff_with_empty_context() {
        let include_dir = Path::new("crates").join("pkg-a");
        let include_glob = include_dir.join("**").join("*");

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: r#"[]"#.as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        let result = run_git_cliff(&include_dir, "v", &[], Some(&runner));

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    /// Tests that a non-zero exit code from `git-cliff` is handled gracefully.
    #[test]
    fn test_run_git_cliff_with_erroneous_exit_code() {
        let include_dir = Path::new("crates").join("pkg-b");
        let include_glob = include_dir.join("**").join("*");

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(1), // failure
                    stdout: vec![],
                    stderr: "something went wrong".as_bytes().to_vec(),
                })
            });

        let result = run_git_cliff(&include_dir, "v", &[], Some(&runner));

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "git-cliff error: something went wrong");
    }

    /// Tests that unknown `git-cliff` JSON context output is handled gracefully.
    #[test]
    fn test_run_git_cliff_with_unknown_json_output() {
        let include_dir = Path::new("crates").join("pkg-c");
        let include_glob = include_dir.join("**").join("*");

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: r#"{"not": "array"}"#.as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        let result = run_git_cliff(&include_dir, "v", &[], Some(&runner));

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    /// Tests that malformed `git-cliff` JSON context output is handled gracefully.
    #[test]
    fn test_run_git_cliff_with_malformed_json_output() {
        let include_dir = Path::new("crates").join("pkg-c");
        let include_glob = include_dir.join("**").join("*");

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(),
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: "invalid json".as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        let result = run_git_cliff(&include_dir, "v", &[], Some(&runner));

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "failed to parse git-cliff JSON output: expected value at line 1 column 1"
        );
    }

    /// Tests that running `git-cliff` with current dir (i.e. `.`) trims leading `./` edge-case.
    #[test]
    fn test_run_git_cliff_with_current_dir() {
        let include_dir = Path::new(".");
        let include_glob = Path::new("**").join("*"); // no leading `./`

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| {
                args == [
                    "--include-path",
                    &include_glob.to_string_lossy(), // should be `**/*` instead of `./**/*`
                    "--tag-pattern",
                    TAG_PATTERN,
                    "--unreleased",
                    "--bump",
                    "--context",
                ]
            })
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: r#"[{"version": "1.0.0"}]"#.as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        let result = run_git_cliff(&include_dir, "v", &[], Some(&runner));

        assert!(result.is_ok());
        assert_eq!(result.unwrap().unwrap(), serde_json::json!({"version": "1.0.0"}));
    }

    /// Tests that the changelog Markdown is rendered from a given `git-cliff` context.
    #[test]
    fn test_render_changelog_markdown_with_success() {
        let context = serde_json::json!({"version": "1.0.0"});
        let expected_markdown = indoc! {r#"
            ## [0.3.0] - 2026-09-26

            ### 🚀 Features

            - *(api)* Add a `subtract` function
            - *(cli)* Print subtractions"#};

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| args == ["--from-context", "-", "--output", "-"])
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(0), // success
                    stdout: expected_markdown.as_bytes().to_vec(),
                    stderr: vec![],
                })
            });

        let result = render_changelog_markdown(&context, Some(&runner));

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected_markdown);
    }

    /// Tests that a non-zero exit code from `git-cliff` is handled gracefully when rendering Markdown.
    #[test]
    fn test_render_changelog_markdown_with_erroneous_exit_code() {
        let context = serde_json::json!({"version": "1.0.0"});

        let mut runner = MockGitCliffRunner::new();
        runner
            .expect_run()
            .withf(move |args, _| args == ["--from-context", "-", "--output", "-"])
            .returning(move |_, _| {
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(1), // failure
                    stdout: vec![],
                    stderr: "something went wrong".as_bytes().to_vec(),
                })
            });

        let result = render_changelog_markdown(&context, Some(&runner));

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "git-cliff error: something went wrong");
    }

    /// Tests that new changelog content is prepended to an existing `CHANGELOG.md` file.
    #[test]
    fn test_write_changelog_markdown() {
        // Write an existing `CHANGELOG.md` file.
        let temp_file = assert_fs::NamedTempFile::new("CHANGELOG.md").unwrap();
        temp_file
            .write_str(&indoc! {r#"
                ## [0.3.0] - 2026-09-26

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                - *(cli)* Print subtractions"#})
            .unwrap();

        // Prepend new changelog content to the existing `CHANGELOG.md` file.
        write_changelog_markdown(
            temp_file.path(),
            &indoc! {r#"
                ## [0.4.0] - 2026-09-27

                ### 🐛 Bug Fixes

                - *(api)* Handle negative numbers correctly"#},
        )
        .unwrap();

        // Verify that the `CHANGELOG.md` file contains the new content followed by the existing content.
        temp_file.assert(indoc! {r#"
            ## [0.4.0] - 2026-09-27

            ### 🐛 Bug Fixes

            - *(api)* Handle negative numbers correctly

            ## [0.3.0] - 2026-09-26

            ### 🚀 Features

            - *(api)* Add a `subtract` function
            - *(cli)* Print subtractions"#});
    }

    /// Tests that a new `CHANGELOG.md` file is created if it does not exist yet.
    #[test]
    fn test_write_changelog_markdown_with_new_file() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let changelog_path = temp_dir.child("CHANGELOG.md");

        // Prepend new changelog content to the existing `CHANGELOG.md` file.
        write_changelog_markdown(
            changelog_path.path(),
            &indoc! {r#"
                ## [0.4.0] - 2026-09-27

                ### 🐛 Bug Fixes

                - *(api)* Handle negative numbers correctly"#},
        )
        .unwrap();

        // Verify that the `CHANGELOG.md` file contains the new content followed by the existing content.
        changelog_path.assert(indoc! {r#"
            ## [0.4.0] - 2026-09-27

            ### 🐛 Bug Fixes

            - *(api)* Handle negative numbers correctly
            "#});
    }

    /// Tests that both the current and next versions are returned from the `git-cliff` JSON context.
    #[rstest]
    #[case::bare("v", "1.0.1", "1.1.0")]
    #[case::root("v", "v1.0.1", "v1.1.0")]
    #[case::cli("git-reflow-v", "git-reflow-v1.0.1", "git-reflow-v1.1.0")]
    #[case::api("git-reflow-api-v", "git-reflow-api-v1.0.1", "git-reflow-api-v1.1.0")]
    fn test_get_version_from_git_cliff_context_with_valid_previous_version(
        #[case] prefix: &str,
        #[case] previous: &str,
        #[case] next: &str,
    ) {
        let context = serde_json::json!({"version": next, "previous": {"version": previous}});
        let result = get_version_from_git_cliff_context(&context, prefix);

        assert_eq!(result.unwrap(), (Some(Version::new(1, 0, 1)), Version::new(1, 1, 0)));
    }

    /// Tests that a valid next version (with no previous version) is returned from the `git-cliff` JSON context.
    #[test]
    fn test_get_version_from_git_cliff_context_with_valid_version() {
        let context = serde_json::json!({"version": "example-api-v1.0.1"});
        let result = get_version_from_git_cliff_context(&context, "example-api-v");

        assert_eq!(result.unwrap(), (None, Version::new(1, 0, 1)));
    }

    /// Tests that a valid prefixed next version is returned from the `git-cliff` JSON context.
    #[test]
    fn test_get_version_from_git_cliff_context_with_valid_v_prefixed_version() {
        let context = serde_json::json!({"version": "v1.1.0"});
        let result = get_version_from_git_cliff_context(&context, "v");

        assert_eq!(result.unwrap(), (None, Version::new(1, 1, 0)));
    }

    /// Tests that an invalid next version is handled gracefully.
    #[test]
    fn test_get_version_from_git_cliff_context_with_invalid_version() {
        let context = serde_json::json!({"version": "invalid version"});
        let result = get_version_from_git_cliff_context(&context, "v");

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "invalid semver version in git-cliff context: unexpected character 'i' while parsing major version number"
        );
    }

    /// Tests that an invalid previous version is handled gracefully.
    #[test]
    fn test_get_version_from_git_cliff_context_with_invalid_previous_version() {
        let context = serde_json::json!({"version": "1.1.0", "previous": {"version": "invalid version"}});
        let result = get_version_from_git_cliff_context(&context, "v");

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "invalid semver version in git-cliff context: unexpected character 'i' while parsing major version number"
        );
    }

    /// Tests that a missing version is handled gracefully.
    #[test]
    fn test_get_version_from_git_cliff_context_with_missing_version() {
        let context = serde_json::json!({"dummy": "text"});
        let result = get_version_from_git_cliff_context(&context, "v");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "missing version in git-cliff context");
    }

    /// Tests that prefixed prerelease and build metadata survive version parsing.
    #[test]
    fn test_get_version_from_git_cliff_context_with_metadata() {
        let context = serde_json::json!({
            "version": "git-reflow-api-v0.2.0-rc.2+build.2",
            "previous": {"version": "git-reflow-api-v0.2.0-rc.1+build.1"},
        });
        let result = get_version_from_git_cliff_context(&context, "git-reflow-api-v");

        assert_eq!(
            result.unwrap(),
            (
                Some(Version::parse("0.2.0-rc.1+build.1").unwrap()),
                Version::parse("0.2.0-rc.2+build.2").unwrap(),
            ),
        );
    }
}
