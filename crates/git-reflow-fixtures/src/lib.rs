use assert_fs::{TempDir, prelude::*};
use git2::Repository;
use rstest::*;
use std::path::PathBuf;

/// A list of [Insta](https://insta.rs/docs) snapshot testing filters of CLI stdout.
pub const INSTA_STDOUT_FILTERS: [(&str, &str); 5] = [
    (r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}.\d{6}Z", "[TIMESTAMP]"),
    (r"\d{4}-\d{2}-\d{2}", "[DATE]"),
    (r"\.tmp\w{6}", "[TEMPDIR]"),
    (r#""[^"]*\[TEMPDIR\]""#, r#""[TEMPDIR]""#), // NB: Turns "/home/**/[TEMPDIR]" into "[TEMPDIR]".
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name)
}

/// Returns a temporary folder with a copy of a test project fixture.
///
/// It is primarily used in integration tests that require access to example project directories.
///
/// # Arguments
///
/// * `name` - The name of the project directory in `fixtures`.
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
/// * `name` - The name of the project directory in `fixtures`.
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

    // Configure the Git author.
    let mut config = repository.config().unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@localhost").unwrap();

    (temp_dir, repository)
}

/// Adds a remote with the given name to the provided local Git repository, pointing
/// to a new temporary bare Git repository.
///
/// # Arguments
///
/// - `local_repo` - The local Git repository to add the remote to.
/// - `remote_name` - The name of the remote to be added, e.g. `origin`.
/// - `owner` - The owner of the remote repository, i.e. the GitHub username.
/// - `repo_name` - The name of the remote repository, e.g. `my-repo`.
///
/// # Returns
///
/// A tuple containing:
///
/// - A [`assert_fs::TempDir`] representing the temporary directory containing the bare remote Git repository.
/// - A [`git2::Repository`] representing the bare remote Git repository.
pub fn add_origin_remote(
    local_repo: &Repository,
    remote_name: &str,
    owner: &str,
    repo_name: &str,
) -> (TempDir, Repository) {
    // Initialise a new bare Git repository.
    let temp_dir = TempDir::new().unwrap();
    let remote_path = temp_dir.child(owner).child(format!("{}.git", repo_name));
    remote_path.create_dir_all().unwrap();
    let remote_repo = Repository::init_bare(&remote_path).unwrap();

    // Add the remote to the local repository.
    local_repo
        .remote(
            remote_name,
            &format!(
                "file://localhost/{}",
                remote_path.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();

    (temp_dir, remote_repo)
}
