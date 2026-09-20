use anyhow::{Context, anyhow, bail};
use git2::Repository;
use tracing::{debug, error, warn};

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
        .statuses(None)
        .context("failed to get Git repository status")?
        .iter()
        .filter_map(|entry| entry.path().map(|status| status.to_string()).ok())
        .collect::<Vec<_>>())
}

/// Sanitises a package name into a valid Git branch name by:
///
///   * Replacing non-alphanumeric characters with dashes;
///   * Collapsing multiple dashes into one;
///   * Removing leading/trailing dashes.
///
/// # Arguments
///
/// * `pkg_name` - The package name to sanitise.
///
/// # Returns
///
/// The valid Git branch name for the given package name.
pub fn sanitize_branch_name(pkg_name: &str) -> String {
    pkg_name
        // Replace non-alphanumeric characters with dashes
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        // Collapse multiple dashes into one
        .split('-')
        // Trim leading/trailing dashes
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
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
pub fn commit(repo: &Repository, pathspecs: &[&str], message: &str) -> anyhow::Result<git2::Oid> {
    // Stage the changes in the index.
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

    /// Tests that package names are sanitized for use in Git branch names.
    #[rstest]
    #[case::special_chars("my/package@1.0", "my-package-1.0")]
    #[case::scoped_package("@scope/pkg", "scope-pkg")]
    #[case::multiple_dashes("multiple---dashes", "multiple-dashes")]
    #[case::leading_trailing_dashes("-leading--and-trailing-", "leading-and-trailing")]
    #[case::complex_chars("my--weird*^branch----!~!-na*me", "my-weird-branch-na-me")]
    fn test_sanitize_branch_name(#[case] branch_name: &str, #[case] expected: &str) {
        assert_eq!(sanitize_branch_name(branch_name), expected);
    }

    /// Tests that dirty files include both modified tracked files and untracked files.
    #[test]
    fn test_get_dirty_files_returns_modified_and_untracked_files() {
        // Create a git repository.
        let (temp_dir, repo) = create_test_repo();

        // Commit tracked files.
        temp_dir.child("clean.txt").write_str("clean content").unwrap();
        commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

        // Write some changes to the filesystem.
        temp_dir.child("tracked.txt").write_str("updated content").unwrap();
        temp_dir.child("untracked.txt").write_str("untracked content").unwrap();

        // Verify that the dirty files are expected.
        let mut dirty_files = get_dirty_files(&repo).unwrap();
        dirty_files.sort();

        assert!(!dirty_files.contains(&"clean.txt".to_string()));
        assert_eq!(
            dirty_files,
            vec!["tracked.txt".to_string(), "untracked.txt".to_string()]
        );
    }

    /// Tests that files are committed correctly and the new commit exists in the repository.
    #[test]
    fn test_commit() {
        // Create a git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a new file and commit it.
        temp_dir.child("README.md").write_str("lorem ipsum").unwrap();
        let commit_id = commit(&repo, &["README.md"], "docs: add `README.md`").unwrap();

        // Verify that the commit was created and the file is tracked.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.id(), commit_id);
        assert!(repo.find_commit(commit_id).is_ok());
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
        // Create a git repository with an `origin` remote URL.
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
        // Create a git repository without an `origin` remote URL.
        let (_temp_dir, repo) = create_test_repo();

        // Verify that the parsed remote is `None`.
        let remote = get_origin_remote(&repo).unwrap();
        assert!(remote.is_none());
    }

    /// Tests that a `BranchGuard` restores the repository state when dropped without disarming.
    #[test]
    fn test_branch_guard_restores_on_drop() {
        // Create a git repository.
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
        // Create a git repository.
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
        // Create a git repository.
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
