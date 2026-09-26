use anyhow::{Context, anyhow, bail, ensure};
use git2::{Repository, StatusOptions};
use std::path::{Path, PathBuf};
use tracing::{debug, error, warn};

/// Ensures that the Git working directory is clean (no uncommitted changes).
///
/// # Arguments
///
/// - `repo` - The Git repository to check.
///
/// # Returns
///
/// An error if the working directory is not clean, otherwise okay.
pub fn ensure_clean_working_directory(repo: &Repository) -> anyhow::Result<()> {
    let dirty_files = get_dirty_files(repo)?;
    ensure!(
        dirty_files.is_empty(),
        "working directory is not clean - please commit or stash your changes first:\n  {}",
        dirty_files.join("\n  ")
    );
    Ok(())
}

/// Returns a list of files with uncommitted changes in the working directory.
///
/// # Arguments
///
/// * `repo` - The Git repository to check.
///
/// # Returns
///
/// Returns a vector of file paths that have uncommitted changes, or empty if
/// the working directory is clean.
///
/// # Errors
///
/// Returns an error if the repository status cannot be determined.
pub fn get_dirty_files(repo: &Repository) -> anyhow::Result<Vec<String>> {
    Ok(repo
        .statuses(Some(
            StatusOptions::new().include_untracked(false).include_ignored(false),
        ))
        .context("failed to get Git repository status")?
        .iter()
        .filter_map(|entry| entry.path().map(|status| status.to_string()).ok())
        .collect::<Vec<_>>())
}

/// Sanitises a valid Git branch name.
///
///   * Replacing non-alphanumeric characters with dashes;
///   * Collapsing multiple dashes into one;
///   * Removing leading/trailing dashes;
///   * Converting to lowercase.
///
/// # Arguments
///
/// * `name` - The branch name to sanitise.
///
/// # Returns
///
/// A result containing a valid Git branch name for the given name, or an error if the name would be empty.
pub fn sanitize_branch_name(name: &str) -> anyhow::Result<String> {
    // Sanitise the branch name.
    let branch = name
        // Replace non-alphanumeric characters with dashes.
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        // Collapse multiple dashes into one.
        .split('-')
        // Trim leading/trailing dashes.
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        // Convert to lowercase.
        .to_lowercase();

    // If the resulting branch is empty, return an error.
    ensure!(
        !branch.is_empty(),
        "the name `{}` would result in an empty branch name",
        name
    );

    Ok(branch)
}

/// Returns the given paths relative to the Git repository's working directory.
///
/// # Arguments
///
/// - `repo` - The Git repository.
/// - `pathspecs` - The paths to make relative to the repository.
///
/// # Returns
///
/// A result containing a list of paths relative to the repository's working directory.
pub fn paths_relative_to_repo<T, I>(repo: &Repository, pathspecs: I) -> anyhow::Result<Vec<PathBuf>>
where
    T: AsRef<Path>,
    I: IntoIterator<Item = T>,
{
    let repo_dir = repo
        .workdir()
        .context("repository has no working directory")?
        .canonicalize()
        .context("could not resolve repository directory")?;
    pathspecs
        .into_iter()
        .map(|path| {
            let path = path.as_ref();
            let relative = if path.is_absolute() {
                path.canonicalize()
                    .with_context(|| format!("could not resolve file `{}`", path.display()))?
                    .strip_prefix(&repo_dir)
                    .with_context(|| format!("file `{}` is outside the repository", path.display()))?
                    .to_path_buf()
            } else {
                path.to_path_buf()
            };
            Ok(relative
                .strip_prefix(".")
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(&relative)
                .to_path_buf())
        })
        .collect()
}

/// Creates a branch at the given commit, overwriting any existing local branch.
///
/// # Arguments
///
/// - `repo` - The Git repository to create the branch in.
/// - `name` - The name of the branch to create.
/// - `commit_id` - The commit ID to create the branch at.
///
/// # Returns
///
/// A result of whether the branch was created successfully or not.
pub fn create_or_reset_branch(repo: &Repository, name: &str, commit_id: git2::Oid) -> anyhow::Result<()> {
    // Resolve the commit and branch reference.
    let commit = repo.find_commit(commit_id).context("failed to find base commit")?;
    let branch_ref = format!("refs/heads/{name}");

    // If the branch already exists and is checked out, reset it to the given commit.
    let head = repo.head().context("failed to get repository HEAD")?;
    if head.name().context("failed to resolve repository HEAD")? == branch_ref {
        repo.reset(commit.as_object(), git2::ResetType::Hard, None)
            .with_context(|| format!("could not reset branch `{name}`"))?;
        return Ok(());
    }

    // Otherwise, create or overwrite the branch at the given commit and check it out.
    repo.branch(name, &commit, true)
        .with_context(|| format!("could not create branch `{name}`"))?;
    repo.checkout_tree(commit.as_object(), Some(git2::build::CheckoutBuilder::new().force()))
        .with_context(|| format!("could not checkout branch `{name}`"))?;
    repo.set_head(&branch_ref)
        .with_context(|| format!("could not move HEAD to `{name}`"))?;

    Ok(())
}

/// Stages and commits the specified changes in the Git repository.
///
/// # Arguments
///
/// * `repo` - The Git repository to commit changes in.
/// * `pathspecs` - A list of file paths/globs to stage and commit.
/// * `message` - The commit message to use for the new commit.
///
/// # Returns
///
/// A result containing the Git object ID (OID) of the new commit.
pub fn commit<T, I>(repo: &Repository, pathspecs: I, message: &str) -> anyhow::Result<git2::Oid>
where
    T: AsRef<Path>,
    I: IntoIterator<Item = T>,
{
    // Stage the changes in the index.
    let pathspecs = paths_relative_to_repo(repo, pathspecs)?;
    let mut index = repo.index().context("could not acquire index")?;
    index
        .add_all(pathspecs, git2::IndexAddOption::DEFAULT, None)
        .context("could not add changes")?;

    // Determine the parent commit(s) for the new commit.
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().context("failed to peel HEAD to commit")?),
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => None,
        Err(e) => return Err(e).context("failed to get HEAD"),
    };
    let parents = parent.iter().collect::<Vec<_>>();

    // Commit the changes to the repository and return.
    let tree_oid = index.write_tree().context("could not stage changes")?;
    index.write().context("could not write Git index")?;
    let signature = repo.signature()?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &repo.find_tree(tree_oid)?,
        &parents,
    )
    .context("could not commit changes")
}

/// Pushes the specified branch to the `origin` remote.
///
/// # Arguments
///
/// - `repo` - The Git repository to push the branch from.
/// - `branch_name` - The name of the branch to push.
/// - `force` - Whether to force push the branch.
///
/// # Returns
///
/// A result indicating whether the push was successful or not.
pub fn push_branch(repo: &Repository, branch_name: &str, force: bool) -> anyhow::Result<()> {
    let mut remote = repo.find_remote("origin").context("failed to find origin remote")?;
    let refspec = format!(
        "{}refs/heads/{branch_name}:refs/heads/{branch_name}",
        if force { "+" } else { "" }
    );
    remote
        .push(&[&refspec], None)
        .with_context(|| format!("failed to push branch `{branch_name}` to origin"))
}

/// A parsed Git remote URL containing the host, owner, and repository names.
pub struct RemoteRef {
    /// The host of the Git remote, e.g. `github.com`.
    pub host: String,
    /// The owner of the repository, e.g. `axieum`.
    pub owner: String,
    /// The name of the repository, e.g. `git-reflow`.
    pub repo: String,
}

/// Parses a Git remote URL into its host, owner, and repository names.
///
/// # Arguments
///
/// * `url` - The Git remote URL to parse, e.g. `git@github.com:axieum/git-reflow.git`.
///
/// # Returns
///
/// A result containing a [`RemoteRef`] containing the parsed host, owner, and repository names.
pub fn parse_remote_url(url: &str) -> anyhow::Result<RemoteRef> {
    let trimmed = url.trim_end_matches(".git");
    let host: String;
    let path: String;

    // Parse the URL into its host and path.
    if !trimmed.contains("://") {
        // SCP-like syntax, e.g. `git@host:owner/repo.git`.
        if let Some((host_part, path_part)) = trimmed.split_once(':') {
            host = host_part.rsplit('@').next().unwrap_or(host_part).to_string();
            path = path_part.to_string();
        } else {
            bail!("unrecognised remote URL: {url}");
        }
    } else {
        // URL-style syntax, e.g. `https://`, `ssh://`, or `git://`.
        let parsed = url::Url::parse(trimmed).context("failed to parse remote URL")?;
        host = parsed
            .host_str()
            .or_else(|| (parsed.scheme() == "file").then_some("localhost"))
            .ok_or_else(|| anyhow!("missing host in remote URL: {url}"))?
            .to_string();
        path = parsed.path().to_string();
    }

    // Split the path into owner and repo, ignoring any leading path segments.
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let (owner, repo) = path
        .rsplit_once('/')
        .ok_or_else(|| anyhow!("missing owner/repo in remote URL path: {path}"))?;
    let owner = owner.rsplit('/').next().unwrap_or(owner);
    if owner.is_empty() || repo.is_empty() {
        bail!("empty owner or repo in remote URL: {url}");
    }

    Ok(RemoteRef {
        host,
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// Returns the parsed Git `origin` remote URL for the given repository, if it has a remote.
///
/// # Arguments
///
/// * `repo` - The Git repository to get the remote for.
///
/// # Returns
///
/// A result containing an optional [`RemoteRef`] containing the parsed host, owner, and repository names, if any.
pub fn get_origin_remote(repo: &Repository) -> anyhow::Result<Option<RemoteRef>> {
    repo.find_remote("origin")
        .ok()
        .map(|remote| {
            let url = remote.url().context("origin remote has no URL")?;
            parse_remote_url(url)
        })
        .transpose()
}

/// A guard that ensures the Git repository is restored to its original state
/// even if an error occurs or the process is terminated.
///
/// # Examples
///
/// ```no_run
/// use git2::Repository;
/// use git_reflow_api::git::BranchGuard;
///
/// fn example() -> anyhow::Result<()> {
///     let repo = Repository::open(".")?;
///
///     // Create a guard - it will restore on drop.
///     let mut guard = BranchGuard::from_head(&repo)?;
///
///     // Do risky operations...
///     // If an error occurs, the guard will restore the git state.
///
///     // Success - disarm the guard to prevent it from restoring.
///     guard.disarm();
///     Ok(())
/// }
/// ```
pub struct BranchGuard<'repo> {
    pub repo: &'repo Repository,
    pub original_branch: String,
    pub original_commit_id: git2::Oid,
    pub should_restore: bool,
}

impl<'repo> BranchGuard<'repo> {
    /// Creates a new branch guard that will restore to the given branch and commit on drop.
    ///
    /// # Arguments
    ///
    /// * `repo` - The Git repository reference.
    /// * `original_branch` - The branch name to restore to.
    /// * `original_commit_id` - The commit ID to reset to.
    pub fn new(repo: &'repo Repository, original_branch: String, original_commit_id: git2::Oid) -> Self {
        Self {
            repo,
            original_branch,
            original_commit_id,
            should_restore: true,
        }
    }

    /// Creates a new branch guard, automatically detecting the current branch and
    /// commit ID from the repository's `HEAD` to restore to on drop.
    ///
    /// # Arguments
    ///
    /// * `repo` - The Git repository reference.
    ///
    /// # Errors
    ///
    /// Returns an error if `HEAD` cannot be resolved to a branch and commit.
    pub fn from_head(repo: &'repo Repository) -> anyhow::Result<Self> {
        let head = repo.head().context("failed to get repository HEAD")?;
        let original_branch = head
            .shorthand()
            .context("failed to resolve current branch name from HEAD")?
            .to_string();
        let original_commit_id = head
            .peel_to_commit()
            .context("failed to resolve current commit from HEAD")?
            .id();

        Ok(Self::new(repo, original_branch, original_commit_id))
    }

    /// Prevents the guard from performing a hard reset when dropped,
    /// allowing the changes to persist.
    ///
    /// Call this after successfully completing all operations that require the guard.
    pub fn disarm(&mut self) {
        self.should_restore = false;
    }
}

impl Drop for BranchGuard<'_> {
    fn drop(&mut self) {
        if !self.should_restore {
            return;
        }

        warn!(
            "restoring repository to original state (branch: {}, commit: {})",
            self.original_branch, self.original_commit_id
        );

        // Restore HEAD to the original branch
        if let Err(e) = self.repo.set_head(&format!("refs/heads/{}", self.original_branch)) {
            error!("failed to restore HEAD: {}", e);
            return;
        }

        // Reset to the original commit (hard reset to discard any changes)
        if let Ok(commit) = self.repo.find_commit(self.original_commit_id) {
            if let Err(e) = self.repo.reset(commit.as_object(), git2::ResetType::Hard, None) {
                error!("failed to reset to original commit: {}", e);
                return;
            }
        } else {
            error!("failed to find original commit: {}", self.original_commit_id);
            return;
        }

        debug!("✓ repository restored to original state");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use git2::Signature;
    use rstest::rstest;

    /// Tests that anticipated branch names are sanitised for use in Git branch names.
    #[rstest]
    #[case::uppercase("my-BRANCH-nAmE", "my-branch-name")]
    #[case::special_chars("my/package@1.0", "my-package-1-0")]
    #[case::scoped_package("@scope/pkg", "scope-pkg")]
    #[case::multiple_dashes("multiple---dashes", "multiple-dashes")]
    #[case::leading_trailing_dashes("-leading--and-trailing-", "leading-and-trailing")]
    #[case::multiple_dots("release..candidate", "release-candidate")]
    #[case::leading_trailing_dots("...release.", "release")]
    #[case::complex_chars("my--weird*^branch----!~!-na*me", "my-weird-branch-na-me")]
    fn test_sanitize_branch_name(#[case] name: &str, #[case] expected: &str) {
        let branch = sanitize_branch_name(name).unwrap();
        assert_eq!(branch, expected);
        assert!(git2::Reference::is_valid_name(&format!("refs/heads/{branch}")));
    }

    /// Tests that invalid branch names return an error when they cannot be sanitised.
    #[rstest]
    #[case::empty("")]
    #[case::all_invalid_chars("@/.")]
    fn test_sanitize_branch_name_when_invalid(#[case] name: &str) {
        let branch = sanitize_branch_name(name);
        assert!(branch.is_err(), "expected error, got: {:?}", branch.unwrap());
    }

    /// Tests that paths are converted to relative to a given Git repository root.
    #[test]
    fn test_paths_relative_to_repo() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a nested path and test various absolute and relative paths.
        let relative = PathBuf::from("nested").join("release.toml");
        temp_dir.child(&relative).write_str("version = '1.0.0'").unwrap();
        let paths = [
            repo.workdir().unwrap().join(&relative),
            PathBuf::from(".").join(&relative),
            relative.clone(),
        ];

        // Verify that the paths are converted to relative to the repository root.
        assert_eq!(
            paths_relative_to_repo(&repo, paths.into_iter()).unwrap(),
            vec![relative; 3]
        );
        assert_eq!(paths_relative_to_repo(&repo, ["."]).unwrap(), vec![PathBuf::from(".")]);
    }

    /// Tests that a paths outside the repository return an error when converted to relative to the Git repository root.
    #[test]
    fn test_paths_relative_to_repo_returns_error_when_outside() {
        // Create a Git repository.
        let (_temp_dir, repo) = create_test_repo();

        // Create a path outside the repository root.
        let outside_dir = assert_fs::TempDir::new().unwrap();
        let outside = outside_dir.child("outside.toml");
        outside.write_str("version = '1.0.0'").unwrap();

        // Verify that an error is returned.
        assert!(paths_relative_to_repo(&repo, &[outside.path()]).is_err());
    }

    /// Tests that a clean working directory passes.
    #[test]
    fn test_ensure_clean_working_directory_when_clean() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Commit tracked files.
        temp_dir.child("clean.txt").write_str("clean content").unwrap();
        commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

        // Verify that a clean working directory passes.
        let result = ensure_clean_working_directory(&repo);
        assert!(result.is_ok(), "unexpected error: {:?}", result.unwrap_err());
    }

    /// Tests that a dirty working directory returns an error with the dirty files.
    #[test]
    fn test_ensure_clean_working_directory_when_dirty() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Add a `.gitignore` file.
        temp_dir.child(".gitignore").write_str("ignored.txt\n").unwrap();
        commit(&repo, &[".gitignore"], "chore: add gitignore").unwrap();

        // Commit tracked files.
        temp_dir.child("clean.txt").write_str("clean content").unwrap();
        commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

        // Write some changes to the filesystem.
        temp_dir.child("tracked.txt").write_str("updated content").unwrap();
        temp_dir.child("untracked.txt").write_str("untracked content").unwrap();
        temp_dir.child("ignored.txt").write_str("ignored content").unwrap();

        // Verify that an error is returned with the dirty files.
        let result = ensure_clean_working_directory(&repo);
        assert!(result.is_err(), "expected error but got ok");
        assert_eq!(
            result.unwrap_err().to_string(),
            "working directory is not clean - please commit or stash your changes first:\n  tracked.txt"
        );
    }

    /// Tests that dirty files include only modified files, excluding untracked and ignored files.
    #[test]
    fn test_get_dirty_files_returns_modified_files() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Add a `.gitignore` file.
        temp_dir.child(".gitignore").write_str("ignored.txt\n").unwrap();
        commit(&repo, &[".gitignore"], "chore: add gitignore").unwrap();

        // Commit tracked files.
        temp_dir.child("clean.txt").write_str("clean content").unwrap();
        commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

        // Write some changes to the filesystem.
        temp_dir.child("tracked.txt").write_str("updated content").unwrap();
        temp_dir.child("untracked.txt").write_str("untracked content").unwrap();
        temp_dir.child("ignored.txt").write_str("ignored content").unwrap();

        // Verify that the dirty files are expected.
        let mut dirty_files = get_dirty_files(&repo).unwrap();
        dirty_files.sort();

        assert!(!dirty_files.contains(&"clean.txt".to_string()));
        assert_eq!(dirty_files, vec!["tracked.txt".to_string()]);
    }

    /// Tests that files are committed correctly and the new commit exists in the repository.
    #[test]
    fn test_commit() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a new file and commit it.
        temp_dir.child("README.md").write_str("lorem ipsum").unwrap();
        let commit_id = commit(&repo, &["README.md"], "docs: add `README.md`").unwrap();

        // Verify that the commit was created and the file is tracked.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.id(), commit_id);
        assert!(repo.find_commit(commit_id).is_ok());
    }

    /// Tests that a branch is pushed to the origin remote.
    #[test]
    fn test_push_branch() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let remote_dir = assert_fs::TempDir::new().unwrap();
        let remote_repo = Repository::init_bare(remote_dir.path()).unwrap();
        repo.remote("origin", remote_dir.path().to_str().unwrap()).unwrap();

        // Create and push a branch.
        let base = repo.head().unwrap().target().unwrap();
        create_or_reset_branch(&repo, "feature", base).unwrap();
        push_branch(&repo, "feature", false).unwrap();

        // Verify that the remote branch points to the local commit.
        assert_eq!(
            remote_repo.find_reference("refs/heads/feature").unwrap().target(),
            Some(base)
        );
    }

    /// Tests that force pushing a branch overwrites the origin remote branch.
    #[test]
    fn test_push_branch_with_force() {
        // Create a local and remote Git repository.
        let (temp_dir, repo) = create_test_repo();
        let remote_dir = assert_fs::TempDir::new().unwrap();
        let remote_repo = Repository::init_bare(remote_dir.path()).unwrap();
        repo.remote("origin", remote_dir.path().to_str().unwrap()).unwrap();

        // Push a branch and then advance it on the remote.
        let base = repo.head().unwrap().target().unwrap();
        create_or_reset_branch(&repo, "feature", base).unwrap();
        push_branch(&repo, "feature", false).unwrap();
        temp_dir.child("tracked.txt").write_str("remote version").unwrap();
        commit(&repo, &["tracked.txt"], "remote version").unwrap();
        push_branch(&repo, "feature", false).unwrap();

        // Rewrite the local branch.
        create_or_reset_branch(&repo, "feature", base).unwrap();
        temp_dir.child("tracked.txt").write_str("local version").unwrap();
        let local_version = commit(&repo, &["tracked.txt"], "local version").unwrap();

        // Ensure that only a force push succeeds.
        assert!(push_branch(&repo, "feature", false).is_err());
        push_branch(&repo, "feature", true).unwrap();

        // Verify that the remote branch points to the rewritten commit.
        assert_eq!(
            remote_repo.find_reference("refs/heads/feature").unwrap().target(),
            Some(local_version)
        );
    }

    /// Tests that a new branch is created.
    #[test]
    fn test_create_or_reset_branch() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a base and latest commit.
        // NB: We'll create a new branch at the base commit, so the later commit shouldn't exist on the new branch.
        temp_dir.child("tracked.txt").write_str("base").unwrap();
        let base_commit = commit(&repo, &["tracked.txt"], "base").unwrap();
        temp_dir.child("tracked.txt").write_str("latest").unwrap();
        commit(&repo, &["tracked.txt"], "latest").unwrap();

        // Create a new branch at the base commit.
        create_or_reset_branch(&repo, "feature", base_commit).unwrap();

        // Verify that the new branch was created and checked out.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        assert_eq!(repo.head().unwrap().target(), Some(base_commit));
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("tracked.txt")).unwrap(),
            "base"
        );
        assert!(repo.statuses(None).unwrap().is_empty());
    }

    /// Tests that an existing branch is overwritten when creating a new branch with the same name.
    #[test]
    fn test_create_or_reset_branch_overwrites_existing_branch() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a base and latest commit.
        // NB: We'll create a new branch at the latest commit, but not check it out yet.
        temp_dir.child("tracked.txt").write_str("base").unwrap();
        let base_commit = commit(&repo, &["tracked.txt"], "base").unwrap();
        temp_dir.child("tracked.txt").write_str("latest").unwrap();
        let latest_commit = commit(&repo, &["tracked.txt"], "latest").unwrap();
        repo.branch("feature", &repo.find_commit(latest_commit).unwrap(), false)
            .unwrap();

        // Overwrite the existing branch at the base commit.
        create_or_reset_branch(&repo, "feature", base_commit).unwrap();

        // Verify that the branch was overwritten.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        assert_eq!(repo.head().unwrap().target(), Some(base_commit));
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("tracked.txt")).unwrap(),
            "base"
        );
        assert!(repo.statuses(None).unwrap().is_empty());
    }

    /// Tests that an already checked out matching branch is reset to the given commit.
    #[test]
    fn test_create_or_reset_branch_resets_checked_out_branch() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a base and latest commit.
        // NB: We'll create a new branch at the latest commit, and switch to it.
        temp_dir.child("tracked.txt").write_str("base").unwrap();
        let base_commit = commit(&repo, &["tracked.txt"], "base").unwrap();
        temp_dir.child("tracked.txt").write_str("latest").unwrap();
        let latest_commit = commit(&repo, &["tracked.txt"], "latest").unwrap();
        create_or_reset_branch(&repo, "feature", latest_commit).unwrap();
        assert_eq!(repo.head().unwrap().target(), Some(latest_commit));

        // Reset the currently checked out branch to the base commit.
        create_or_reset_branch(&repo, "feature", base_commit).unwrap();

        // Verify that the currently checked out branch was reset.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        assert_eq!(repo.head().unwrap().target(), Some(base_commit));
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("tracked.txt")).unwrap(),
            "base"
        );
        assert!(repo.statuses(None).unwrap().is_empty());
    }

    /// Tests that a remote URLs are parsed correctly.
    #[rstest]
    #[rustfmt::skip]
    #[case::github_ssh("git@github.com:axieum/git-reflow.git", "github.com", "axieum", "git-reflow")]
    #[case::github_https("https://github.com/axieum/git-reflow.git", "github.com", "axieum", "git-reflow")]
    #[case::github_enterprise_ssh("git@github.org.com:axieum/git-reflow.git", "github.org.com", "axieum", "git-reflow")]
    #[case::github_enterprise_ssh_with_port("ssh://git@github.org.com:2222/axieum/git-reflow.git", "github.org.com", "axieum", "git-reflow")]
    #[case::github_enterprise_https("https://github.org.com/axieum/git-reflow.git", "github.org.com", "axieum", "git-reflow")]
    #[case::bitbucket_ssh("git@bitbucket.org:axieum/git-reflow.git", "bitbucket.org", "axieum", "git-reflow")]
    #[case::bitbucket_https("https://bitbucket.org/axieum/git-reflow.git", "bitbucket.org", "axieum", "git-reflow")]
    #[case::bitbucket_server_ssh("ssh://git@bitbucket.org.com/axieum/git-reflow.git", "bitbucket.org.com", "axieum", "git-reflow")]
    #[case::bitbucket_server_ssh_with_port("ssh://git@bitbucket.org.com:2222/axieum/git-reflow.git", "bitbucket.org.com", "axieum", "git-reflow")]
    #[case::bitbucket_server_https("https://bitbucket.org.com/scm/axieum/git-reflow.git", "bitbucket.org.com", "axieum", "git-reflow")]
    #[case::gitea_ssh("git@gitea.org.com:axieum/git-reflow.git", "gitea.org.com", "axieum", "git-reflow")]
    #[case::gitea_https("https://gitea.org.com/axieum/git-reflow.git", "gitea.org.com", "axieum", "git-reflow")]
    #[case::gitea_enterprise_ssh("ssh://git@bitbucket.org.com/axieum/git-reflow.git", "bitbucket.org.com", "axieum", "git-reflow")]
    #[case::gitea_enterprise_ssh_with_port("ssh://git@bitbucket.org.com:2222/axieum/git-reflow.git", "bitbucket.org.com", "axieum", "git-reflow")]
    #[case::gitea_enterprise_https("https://bitbucket.org.com/scm/axieum/git-reflow.git", "bitbucket.org.com", "axieum", "git-reflow")]
    #[case::gitlab_ssh("git@gitlab.com:axieum/git-reflow.git", "gitlab.com", "axieum", "git-reflow")]
    #[case::gitlab_https("https://gitlab.com/axieum/git-reflow.git", "gitlab.com", "axieum", "git-reflow")]
    #[case::gitlab_enterprise_ssh("git@gitlab.org.com:axieum/git-reflow.git", "gitlab.org.com", "axieum", "git-reflow")]
    #[case::gitlab_enterprise_ssh_with_port("ssh://git@gitlab.org.com:2222/axieum/git-reflow.git", "gitlab.org.com", "axieum", "git-reflow")]
    #[case::gitlab_enterprise_https("https://gitlab.org.com/axieum/git-reflow.git", "gitlab.org.com", "axieum", "git-reflow")]
    #[case::local_file("file://localhost/tmp/octocat/Hello-World.git", "localhost", "octocat", "Hello-World")]
    fn test_parse_remote_url(
        #[case] url: &str,
        #[case] expected_host: &str,
        #[case] expected_owner: &str,
        #[case] expected_repo: &str,
    ) {
        let remote = parse_remote_url(url).unwrap();
        assert_eq!(remote.host, expected_host);
        assert_eq!(remote.owner, expected_owner);
        assert_eq!(remote.repo, expected_repo);
    }

    /// Tests that the Git `origin` remote is parsed correctly.
    #[test]
    fn test_get_origin_remote() {
        // Create a Git repository with an `origin` remote URL.
        let (_temp_dir, repo) = create_test_repo();
        repo.remote("origin", "git@github.com:axieum/git-reflow.git").unwrap();

        // Verify that the remote is parsed correctly.
        let remote = get_origin_remote(&repo).unwrap().expect("origin remote should exist");
        assert_eq!(remote.host, "github.com");
        assert_eq!(remote.owner, "axieum");
        assert_eq!(remote.repo, "git-reflow");
    }

    /// Tests that the Git `origin` remote returns `None` if it does not exist.
    #[test]
    fn test_get_origin_remote_when_none() {
        // Create a Git repository without an `origin` remote URL.
        let (_temp_dir, repo) = create_test_repo();

        // Verify that the parsed remote is `None`.
        let remote = get_origin_remote(&repo).unwrap();
        assert!(remote.is_none());
    }

    /// Tests that a `BranchGuard` restores the repository state when dropped without disarming.
    #[test]
    fn test_branch_guard_restores_on_drop() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();
        let initial_head = repo.head().unwrap();
        let initial_branch = initial_head.shorthand().unwrap().to_string();
        let initial_commit_id = initial_head.peel_to_commit().unwrap().id();

        // Create a branch guard from the current HEAD.
        let guard = BranchGuard::from_head(&repo).unwrap();

        // Create a new branch and switch to it.
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("test-branch", &parent, false).unwrap();
        repo.set_head("refs/heads/test-branch").unwrap();
        repo.checkout_head(None).unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "test-branch");

        // Commit changes to the new branch.
        temp_dir.child("test.txt").write_str("test content").unwrap();
        let new_commit_id = commit(&repo, &["test.txt"], "feat: add test.txt").unwrap();
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), new_commit_id);

        // Drop the guard without disarming to restore.
        drop(guard); // The guard is dropped here and should perform a restore since not disarmed.

        // Verify that the git repository was restored to its original state.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), initial_branch);
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), initial_commit_id);
        assert!(!repo.path().parent().unwrap().join("test.txt").exists());
    }

    /// Tests that a `BranchGuard` does not restore when disarmed.
    #[test]
    fn test_branch_guard_disarm_prevents_restoration() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a branch guard from the current HEAD.
        let mut guard = BranchGuard::from_head(&repo).unwrap();

        // Create a new branch and switch to it.
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature-branch", &parent, false).unwrap();
        repo.set_head("refs/heads/feature-branch").unwrap();
        repo.checkout_head(None).unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature-branch");

        // Commit changes to the new branch.
        temp_dir.child("feature.txt").write_str("feature content").unwrap();
        let new_commit_id = commit(&repo, &["feature.txt"], "feat: add feature.txt").unwrap();

        // Create another branch and switch to it.
        repo.branch("other-branch", &parent, false).unwrap();
        repo.set_head("refs/heads/other-branch").unwrap();
        repo.checkout_head(None).unwrap();

        // Disarm the guard.
        guard.disarm();
        drop(guard); // The guard is dropped here but should NOT perform a restore since disarmed.

        // Verify that we're on the latest branch.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "other-branch");

        // Verify the new branch and commit still exist - the guard didn't do a hard reset.
        assert!(repo.find_branch("feature-branch", git2::BranchType::Local).is_ok());
        assert!(repo.find_branch("other-branch", git2::BranchType::Local).is_ok());
        assert!(repo.find_commit(new_commit_id).is_ok());
    }

    /// Tests that a `BranchGuard` restores state when an error occurs mid-operation.
    #[test]
    fn test_branch_guard_restores_on_error() {
        // Create a Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let initial_head = repo.head().unwrap();
        let initial_branch = initial_head.shorthand().unwrap().to_string();
        let initial_commit_id = initial_head.peel_to_commit().unwrap().id();

        // Simulate an operation that fails.
        let result: Result<(), &str> = (|| {
            let _guard = BranchGuard::from_head(&repo).unwrap();

            // Create a new branch and switch to it.
            let parent = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("failed-branch", &parent, false).unwrap();
            repo.set_head("refs/heads/failed-branch").unwrap();
            repo.checkout_head(None).unwrap();

            // Simulate an error - the guard will restore since not disarmed.
            Err("simulated error")
        })();

        // Verify restoration happened.
        assert!(result.is_err());
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), initial_branch);
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), initial_commit_id);
    }

    /// Helper function to create a test repository with an initial commit.
    fn create_test_repo() -> (assert_fs::TempDir, Repository) {
        // Create a new repository.
        let temp_dir = assert_fs::TempDir::new().unwrap();
        let repo = Repository::init(temp_dir.path()).unwrap();

        // Configure the Git author.
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@localhost").unwrap();

        // Create an initial commit.
        let sig = Signature::now("Test", "test@localhost").unwrap();
        let tree_oid = repo.index().unwrap().write_tree().unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "chore: initial commit",
            &repo.find_tree(tree_oid).unwrap(),
            &[],
        )
        .unwrap();

        (temp_dir, repo)
    }
}
