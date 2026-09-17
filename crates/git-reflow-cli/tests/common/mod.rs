use assert_fs::TempDir;
use assert_fs::prelude::PathCopy;
use git2::Repository;
use rstest::*;
use std::path::PathBuf;

/// A list of [Insta](https://insta.rs/docs) snapshot testing filters of CLI stdout.
#[allow(dead_code)]
pub const INSTA_STDOUT_FILTERS: [(&str, &str); 3] = [
    (r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}.\d{6}Z", "[TIMESTAMP]"),
    (r"\.tmp\w{6}", "[TEMPDIR]"),
    (r"[a-f0-9]{7,40}", "[COMMIT SHA]"),
];

/// Returns the path to a test project fixture.
///
/// # Arguments
///
/// * `name` - The name of the project directory in `tests/fixtures`.
///
/// # Returns
///
/// A [`PathBuf`] representing the full path to the requested test project fixture.
///
/// # Notes
///
/// - This function uses the `CARGO_MANIFEST_DIR` environment variable to ensure the path is
///   resolved correctly regardless of the current working directory.
#[fixture]
pub fn project_path(#[default(".")] name: &str) -> PathBuf {
    // Locate the test project fixture source.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Returns a temporary folder with a copy of a test project fixture.
///
/// It is primarily used in integration tests that require access to example project directories.
///
/// # Arguments
///
/// * `name` - The name of the project directory in `tests/fixtures`.
///
/// # Returns
///
/// A [`assert_fs::TempDir`] representing the full path to the requested test project fixture.
///
/// # Notes
///
/// - The returned path is a temporary directory with the test project fixture files copied into.
#[fixture]
pub fn project_dir(#[default(".")] name: &str) -> TempDir {
    // Locate the test project fixture source.
    let fixture_path = project_path(name);

    // Copy the test project fixture source into a temporary directory.
    // This ensures that tests can modify the project without affecting the test source code.
    let temp_dir = TempDir::new().unwrap();
    temp_dir.copy_from(&fixture_path, &["**/*", "!*.export"]).unwrap();

    temp_dir
}

/// Returns a temporary folder with a copy of a test project fixture initialised
/// as a Git repository.
///
/// # Arguments
///
/// * `name` - The name of the project directory in `tests/fixtures`.
///
/// # Returns
///
/// A tuple containing:
///
/// - A [`assert_fs::TempDir`] representing the full path to the requested test project fixture.
/// - A [`git2::Repository`] representing the Git repository initialised in the temporary directory.
#[fixture]
pub fn project_repo(#[default(".")] name: &str) -> (TempDir, Repository) {
    // Initialise a new Git repository.
    let temp_dir = project_dir(name);
    let repository = Repository::init(&temp_dir).unwrap();

    (temp_dir, repository)
}
