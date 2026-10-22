use anyhow::{Context, anyhow, bail, ensure};
use git2::{Delta, DiffOptions, Repository, StatusOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::{debug, error, trace, warn};

/// Ensures that the Git working directory is clean (no uncommitted changes).
///
/// # Arguments
///
/// * `repo` - The Git repository to check.
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

/// A change to a file, with raw file contents.
#[derive(Debug, PartialEq, Eq)]
pub enum FileChange {
    /// Creates a file that does not exist in the base commit.
    Create { path: String, contents: Vec<u8> },
    /// Replaces the contents of a file in the base commit.
    Update { path: String, contents: Vec<u8> },
    /// Deletes a file from the base commit.
    Delete { path: String },
}

/// Collects changes from a staged tree relative to a base commit.
///
/// It reads file contents from Git blobs without accessing the working directory or
/// modifying the index. Renames are represented as a deletion followed by a creation.
///
/// # Arguments
///
/// * `repo` - The Git repository to check.
/// * `base` - The commit against which changes are calculated.
/// * `tree_id` - The staged tree ID returned by [`stage`].
///
/// # Returns
///
/// A result containing creations, updates, and deletions relative to `base`.
pub fn collect_staged_changes(
    repo: &Repository,
    base: git2::Oid,
    tree_id: git2::Oid,
) -> anyhow::Result<Vec<FileChange>> {
    let base_tree = repo
        .find_commit(base)
        .context("failed to find base commit")?
        .tree()
        .context("failed to read base tree")?;
    let tree = repo.find_tree(tree_id).context("failed to find staged tree")?;
    let diff = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&tree), None)
        .context("failed to compare base commit with staged tree")?;
    let mut changes = Vec::new();
    for delta in diff.deltas() {
        let old = delta.old_file();
        let new = delta.new_file();
        let path = new.path().or_else(|| old.path()).context("changed file has no path")?;
        let path = path
            .to_str()
            .context("changed file path is not valid UTF-8")?
            .to_string();
        if delta.status() == Delta::Deleted {
            changes.push(FileChange::Delete { path });
            continue;
        }
        let contents = repo
            .find_blob(new.id())
            .with_context(|| format!("failed to read staged blob for `{path}`"))?
            .content()
            .to_vec();
        changes.push(if delta.status() == Delta::Added {
            FileChange::Create { path, contents }
        } else {
            FileChange::Update { path, contents }
        });
    }
    Ok(changes)
}

/// Sanitises a valid Git branch name by:
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
/// * `repo` - The Git repository.
/// * `pathspecs` - The paths to make relative to the repository.
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
/// * `repo` - The Git repository to create the branch in.
/// * `name` - The name of the branch to create.
/// * `commit_id` - The commit ID to create the branch at.
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

/// Checks if two references point to trees with identical content.
///
/// # Arguments
///
/// * `repo` - The Git repository to check.
/// * `source_ref` - The source reference to compare.
/// * `target_ref` - The target reference to compare.
///
/// # Returns
///
/// A result of whether the two references point to identical tree content.
pub fn trees_match(repo: &Repository, source_ref: &str, target_ref: &str) -> anyhow::Result<bool> {
    // Resolve the source reference to a tree.
    let source_tree = repo
        .revparse_single(source_ref)
        .with_context(|| format!("failed to find source reference `{source_ref}`"))?
        .peel_to_tree()
        .with_context(|| format!("failed to peel source reference `{source_ref}` to tree"))?;

    // Try to resolve the target reference to a tree.
    // NB: If the target reference does not exist yet, assume the trees do not match.
    let target_tree = match repo.revparse_single(target_ref) {
        Ok(obj) => obj
            .peel_to_tree()
            .with_context(|| format!("failed to peel target reference `{target_ref}` to tree"))?,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(false),
        Err(e) => bail!(e),
    };

    // If the tree IDs are identical, then the trees match.
    if source_tree.id() == target_tree.id() {
        return Ok(true);
    }

    // Otherwise, we need to check the diff between the two trees.
    let mut diff_opts = DiffOptions::new();
    diff_opts.include_untracked(false);

    let diff = repo.diff_tree_to_tree(Some(&source_tree), Some(&target_tree), Some(&mut diff_opts))?;
    Ok(diff.deltas().count() == 0)
}

/// Stages the specified paths and returns the resulting tree ID without creating a commit.
///
/// # Arguments
///
/// * `repo` - The Git repository to stage changes in.
/// * `pathspecs` - A list of file paths/globs to stage.
///
/// # Returns
///
/// A result containing the Git object ID (OID) of the staged tree.
pub fn stage<T, I>(repo: &Repository, pathspecs: I) -> anyhow::Result<git2::Oid>
where
    T: AsRef<Path>,
    I: IntoIterator<Item = T>,
{
    let pathspecs = paths_relative_to_repo(repo, pathspecs)?;
    let mut index = repo.index().context("could not acquire index")?;
    index
        .add_all(pathspecs, git2::IndexAddOption::DEFAULT, None)
        .context("could not add changes")?;
    let tree_oid = index.write_tree().context("could not stage changes")?;
    index.write().context("could not write Git index")?;
    Ok(tree_oid)
}

/// Commits a previously staged tree in the Git repository.
///
/// Does not stage files or modify the index. The consumer must call [`stage`]
/// first and pass the returned tree ID.
///
/// # Arguments
///
/// * `repo` - The Git repository to commit changes in.
/// * `tree_oid` - The staged tree ID returned by [`stage`].
/// * `message` - The commit message to use for the new commit.
///
/// # Returns
///
/// A result containing the Git object ID (OID) of the new commit.
pub fn commit(repo: &Repository, tree_oid: git2::Oid, message: &str) -> anyhow::Result<git2::Oid> {
    // Determine the parent commit(s) for the new commit.
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().context("failed to peel HEAD to commit")?),
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => None,
        Err(e) => return Err(e).context("failed to get HEAD"),
    };
    let parents = parent.iter().collect::<Vec<_>>();

    // Commit the changes to the repository and return.
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

/// Both stages and commits the specified paths with the given commit message.
///
/// This is equivalent to:
///
/// ```no_run
/// use git_reflow_api::git::{commit, stage};
/// use git2::Repository;
///
/// fn example() -> anyhow::Result<()> {
///     let repo = Repository::open(".")?;
///     let tree_id = stage(&repo, ["README.md"])?;
///     let commit_id = commit(&repo, tree_id, "docs(README): add example code")?;
///     println!("created commit: {commit_id}");
///     Ok(())
/// }
/// ```
///
/// # Arguments
///
/// * `repo` - The Git repository to commit changes in.
/// * `pathspecs` - A list of file paths/globs to stage.
/// * `message` - The commit message to use for the new commit.
///
/// # Returns
///
/// A result containing the Git object ID (OID) of the new commit.
pub fn stage_and_commit<T, I>(repo: &Repository, pathspecs: I, message: &str) -> anyhow::Result<git2::Oid>
where
    T: AsRef<Path>,
    I: IntoIterator<Item = T>,
{
    let tree_id = stage(repo, pathspecs)?;
    commit(repo, tree_id, message)
}

/// Creates a tag at the given commit.
///
/// # Arguments
///
/// * `repo` - The Git repository to create the tag in.
/// * `name` - The name of the tag to create.
/// * `commit_id` - The commit ID to create the tag at.
/// * `force` - Whether to force the creation of the tag if it already exists.
///
/// # Returns
///
/// A result containing the Git object ID (OID) of the new tag.
pub fn tag(repo: &Repository, name: &str, commit_id: git2::Oid, force: bool) -> anyhow::Result<git2::Oid> {
    let commit = repo.find_commit(commit_id).context("failed to find commit")?;
    let signature = repo.signature()?;
    repo.tag(name, commit.as_object(), &signature, "", force)
        .with_context(|| format!("could not create tag `{name}`"))
}

/// Fetches the specified reference from the named remote.
///
/// NB: This function spawns the `git` CLI as a child process to ensure that
///     the fetch respects the user's Git configuration, e.g. authentication.
///
/// # Arguments
///
/// * `repo` - The Git repository to fetch the branch into.
/// * `remote_name` - The name of the remote to fetch from, e.g. `origin`.
/// * `refspec` - The name of the reference to fetch, e.g. `refs/heads/feature` or `refs/tags/v1.0.0`.
///
/// # Returns
///
/// A result indicating whether the fetch was successful or not.
pub fn fetch(repo: &Repository, remote_name: &str, refspec: &str) -> anyhow::Result<()> {
    // Prepare `git` arguments.
    let workdir = repo.workdir().unwrap_or(repo.path());

    // Invoke the `git` command.
    // NB: We use the `git` CLI here to ensure that the fetch respects the user's Git configuration, e.g. authentication.
    trace!("$ git fetch {} {}", remote_name, refspec);
    let child = Command::new("git")
        .current_dir(workdir) // NB: Set both the current directory and `-C`; better safe than sorry.
        .arg("-C")
        .arg(workdir)
        .args(["fetch", remote_name, refspec])
        .arg("--porcelain")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn `git` process")?;
    let output = child.wait_with_output().context("failed to wait for `git` process")?;

    // Check the `git` output.
    if output.status.success() {
        trace!("↳ {}", str::from_utf8(&output.stdout)?);
        Ok(())
    } else {
        bail!("git error ({}): {}", output.status, str::from_utf8(&output.stderr)?);
    }
}

/// Fetches the specified branch from the named remote.
///
/// NB: This function spawns the `git` CLI as a child process to ensure that
///     the fetch respects the user's Git configuration, e.g. authentication.
///
/// # Arguments
///
/// * `repo` - The Git repository to fetch the branch into.
/// * `remote_name` - The name of the remote to fetch from, e.g. `origin`.
/// * `branch_name` - The name of the branch to fetch.
///
/// # Returns
///
/// A result containing the remote branch reference if it exists, e.g. `refs/remotes/origin/feature`.
pub fn fetch_branch(repo: &Repository, remote_name: &str, branch_name: &str) -> anyhow::Result<Option<String>> {
    match fetch(repo, remote_name, &format!("refs/heads/{branch_name}")) {
        Ok(_) => Ok(Some(format!("refs/remotes/{remote_name}/{branch_name}"))),
        Err(e) if e.to_string().contains("couldn't find remote ref") => Ok(None),
        Err(e) => Err(e),
    }
}

/// Pushes the specified reference to the named remote.
///
/// NB: This function spawns the `git` CLI as a child process to ensure that
///     the push respects the user's Git configuration, e.g. authentication.
///
/// # Arguments
///
/// * `repo` - The Git repository to push the branch from.
/// * `remote_name` - The name of the remote to push to, e.g. `origin`.
/// * `refspec` - The name of the reference to push.
/// * `force` - Whether to force push the reference.
///
/// # Returns
///
/// A result indicating whether the push was successful or not.
pub fn push(repo: &Repository, remote_name: &str, refspec: &str, force: bool) -> anyhow::Result<()> {
    // Prepare `git` arguments.
    let workdir = repo.workdir().unwrap_or(repo.path());
    let refspec = format!("{}{refspec}:{refspec}", if force { "+" } else { "" });

    // Invoke the `git` command.
    // NB: We use the `git` CLI here to ensure that the push respects the user's Git configuration, e.g. authentication.
    trace!("$ git push {} {}", remote_name, refspec);
    let child = Command::new("git")
        .current_dir(workdir) // NB: Set both the current directory and `-C`; better safe than sorry.
        .arg("-C")
        .arg(workdir)
        .args(["push", remote_name, &refspec])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn `git` process")?;
    let output = child.wait_with_output().context("failed to wait for `git` process")?;

    // Check the `git` output.
    if output.status.success() {
        trace!("↳ {}", str::from_utf8(&output.stdout)?);
        Ok(())
    } else {
        bail!("git error ({}): {}", output.status, str::from_utf8(&output.stderr)?);
    }
}

/// Pushes the specified branch to the `origin` remote.
///
/// NB: This function spawns the `git` CLI as a child process to ensure that
///     the push respects the user's Git configuration, e.g. authentication.
///
/// # Arguments
///
/// * `repo` - The Git repository to push the branch from.
/// * `branch_name` - The name of the branch to push.
/// * `force` - Whether to force push the branch.
///
/// # Returns
///
/// A result indicating whether the push was successful or not.
pub fn push_branch(repo: &Repository, branch_name: &str, force: bool) -> anyhow::Result<()> {
    push(repo, "origin", &format!("refs/heads/{branch_name}"), force)
}

/// Pushes the specified tag to the `origin` remote.
///
/// NB: This function spawns the `git` CLI as a child process to ensure that
///     the push respects the user's Git configuration, e.g. authentication.
///
/// # Arguments
///
/// * `repo` - The Git repository to push the branch from.
/// * `tag_name` - The name of the tag to push.
/// * `force` - Whether to force push the branch.
///
/// # Returns
///
/// A result indicating whether the push was successful or not.
pub fn push_tag(repo: &Repository, tag_name: &str, force: bool) -> anyhow::Result<()> {
    push(repo, "origin", &format!("refs/tags/{tag_name}"), force)
}

/// Lists references matching the given pattern on the named remote without fetching them.
///
/// NB: This function spawns the `git` CLI as a child process to ensure that
///     the fetch respects the user's Git configuration, e.g. authentication.
///
/// # Arguments
///
/// * `repo` - The local Git repository.
/// * `remote_name` - The name of the remote to query, e.g. `origin`.
/// * `refspec` - The reference pattern to match, e.g. `refs/tags/*`.
///
/// # Returns
///
/// A result containing a list of matching references on the remote.
pub fn ls_remote(repo: &Repository, remote_name: &str, refspec: &str) -> anyhow::Result<Vec<String>> {
    // Prepare `git` arguments.
    let workdir = repo.workdir().unwrap_or(repo.path());

    // Invoke the `git` command.
    trace!("$ git ls-remote {} {}", remote_name, refspec);
    let child = Command::new("git")
        .current_dir(workdir) // NB: Set both the current directory and `-C`; better safe than sorry.
        .arg("-C")
        .arg(workdir)
        .args(["ls-remote", "--refs", "--", remote_name, refspec])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn `git` process")?;
    let output = child.wait_with_output().context("failed to wait for `git` process")?;

    // Check the `git` output.
    if output.status.success() {
        let stdout = str::from_utf8(&output.stdout)?;
        trace!("↳ {}", stdout);
        stdout
            .lines()
            .map(|line| {
                let (_, reference) = line.split_once('\t').context("invalid `git ls-remote` output")?;
                Ok(reference.to_string())
            })
            .collect()
    } else {
        bail!("git error ({}): {}", output.status, str::from_utf8(&output.stderr)?);
    }
}

/// Checks whether the exact tag exists on the named remote without fetching it.
///
/// # Arguments
///
/// * `repo` - The local Git repository.
/// * `remote_name` - The name of the remote to query, e.g. `origin`.
/// * `tag_name` - The name of the tag to check, e.g. `v1.0.0`.
///
/// # Returns
///
/// A result indicating whether the tag exists on the remote or not.
pub fn tag_exists_on_remote(repo: &Repository, remote_name: &str, tag_name: &str) -> anyhow::Result<bool> {
    let refspec = format!("refs/tags/{tag_name}");
    Ok(ls_remote(repo, remote_name, &refspec)?.contains(&refspec))
}

/// A parsed Git remote URL containing the host, owner, and repository names.
pub struct RemoteRef {
    /// The full URL of the Git remote, e.g. `git@github.com:axieum/git-reflow.git`.
    pub url: String,
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
        url: url.to_string(),
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
        stage_and_commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        stage_and_commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

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
        stage_and_commit(&repo, &[".gitignore"], "chore: add gitignore").unwrap();

        // Commit tracked files.
        temp_dir.child("clean.txt").write_str("clean content").unwrap();
        stage_and_commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        stage_and_commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

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
        stage_and_commit(&repo, &[".gitignore"], "chore: add gitignore").unwrap();

        // Commit tracked files.
        temp_dir.child("clean.txt").write_str("clean content").unwrap();
        stage_and_commit(&repo, &["clean.txt"], "feat: add clean.txt").unwrap();
        temp_dir.child("tracked.txt").write_str("initial content").unwrap();
        stage_and_commit(&repo, &["tracked.txt"], "feat: add tracked.txt").unwrap();

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

    /// Tests that collecting staged changes include creations, updates and deletions with file contents.
    #[test]
    fn test_collect_staged_changes() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create some files in the repository and commit them.
        for name in ["old.txt", "deleted.txt", "modified.txt", "skipped.txt"] {
            temp_dir.child(name).write_str("original").unwrap();
        }
        let base = stage_and_commit(&repo, ["."], "initial").unwrap();

        // Modify the files to simulate additions, deletions, and renames.
        temp_dir.child("modified.txt").write_str("updated").unwrap();
        temp_dir.child("skipped.txt").write_str("not included").unwrap();
        temp_dir.child("assets").create_dir_all().unwrap();
        temp_dir
            .child("assets")
            .child("image.bin")
            .write_binary(&[0, 255, 10])
            .unwrap();
        std::fs::remove_file(temp_dir.child("deleted.txt")).unwrap();
        std::fs::rename(temp_dir.child("old.txt"), temp_dir.child("renamed.txt")).unwrap();

        // Stage only the selected files.
        let pathspecs = ["old.txt", "renamed.txt", "deleted.txt", "modified.txt", "assets/*"];
        let tree_id = stage(&repo, pathspecs).unwrap();

        // Change both the working directory and index after staging above.
        temp_dir
            .child("modified.txt")
            .write_str("later staged contents")
            .unwrap();
        let later_tree_id = stage(&repo, ["modified.txt"]).unwrap();
        std::fs::remove_file(temp_dir.child("modified.txt")).unwrap();
        std::fs::remove_file(temp_dir.child("renamed.txt")).unwrap();

        // Collect changes from the earlier staged tree rather than the current index or working directory.
        let changes = collect_staged_changes(&repo, base, tree_id).unwrap();

        // Verify that renames are represented as a deletion and creation with the file contents.
        assert_eq!(
            changes,
            vec![
                FileChange::Create {
                    path: "assets/image.bin".into(),
                    contents: vec![0, 255, 10],
                },
                FileChange::Delete {
                    path: "deleted.txt".into(),
                },
                FileChange::Update {
                    path: "modified.txt".into(),
                    contents: b"updated".to_vec(),
                },
                FileChange::Delete { path: "old.txt".into() },
                FileChange::Create {
                    path: "renamed.txt".into(),
                    contents: b"original".to_vec(),
                },
            ],
        );

        // Verify that collecting changes left the HEAD and the later staged tree unchanged.
        assert_eq!(repo.head().unwrap().target().unwrap(), base);
        assert_eq!(repo.index().unwrap().write_tree().unwrap(), later_tree_id);
    }

    /// Tests that collecting staged changes can be relative to an explicit base rather than HEAD.
    #[test]
    fn test_collect_staged_changes_with_explicit_base() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();
        let base = repo.head().unwrap().target().unwrap();

        // Commit a new file so that HEAD differs from the base.
        temp_dir.child("file.txt").write_str("committed after base").unwrap();
        let tree_id = stage(&repo, ["file.txt"]).unwrap();
        commit(&repo, tree_id, "new HEAD").unwrap();

        // Collect changes relative to the earlier base, not the current HEAD.
        let changes = collect_staged_changes(&repo, base, tree_id).unwrap();

        // Verify that the committed file is still reported as a creation relative to the base.
        assert_eq!(
            changes,
            vec![FileChange::Create {
                path: "file.txt".into(),
                contents: b"committed after base".to_vec(),
            }],
        );

        // Verify that comparing the staged tree with its own commit produces no changes.
        let head = repo.head().unwrap().target().unwrap();
        let changes = collect_staged_changes(&repo, head, tree_id).unwrap();
        assert!(changes.is_empty());
    }

    /// Tests that collected staged contents include Git line-ending normalisation.
    #[test]
    fn test_collect_staged_changes_normalizes_contents() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();
        let base = repo.head().unwrap().target().unwrap();

        // Configure text normalisation and stage a file with CRLF line endings.
        temp_dir
            .child(".gitattributes")
            .write_str("file.txt text eol=lf\n")
            .unwrap();
        temp_dir.child("file.txt").write_binary(b"staged\r\n").unwrap();
        let tree_id = stage(&repo, [".gitattributes", "file.txt"]).unwrap();

        // Collect the staged changes relative to the initial commit.
        let changes = collect_staged_changes(&repo, base, tree_id).unwrap();

        // Verify that contents match Git's normalised line endings rather than the working-directory contents.
        assert_eq!(
            changes,
            vec![
                FileChange::Create {
                    path: ".gitattributes".into(),
                    contents: b"file.txt text eol=lf\n".to_vec(),
                },
                FileChange::Create {
                    path: "file.txt".into(),
                    contents: b"staged\n".to_vec(),
                },
            ]
        );
    }

    /// Tests that selected files are staged into a tree and persisted in the index without creating a commit.
    #[test]
    fn test_stage() {
        // Create a Git repository with an existing tracked file.
        let (temp_dir, repo) = create_test_repo();
        temp_dir.child("README.md").write_str("original content").unwrap();
        let head = stage_and_commit(&repo, &["README.md"], "docs: add `README.md`").unwrap();

        // Modify the tracked file and create new files, leaving one outside the selected paths.
        temp_dir.child("README.md").write_str("updated content").unwrap();
        temp_dir.child("release.txt").write_str("release content").unwrap();
        temp_dir.child("unrelated.txt").write_str("unrelated content").unwrap();

        // Stage only the release files without creating a commit.
        let tree_id = stage(&repo, &["README.md", "release.txt"]).unwrap();

        // Verify that the returned tree contains the selected files and their updated contents.
        let tree = repo.find_tree(tree_id).unwrap();
        assert_eq!(tree.len(), 2);
        for (name, content) in [("README.md", "updated content"), ("release.txt", "release content")] {
            let entry = tree.get_name(name).unwrap();
            let blob = repo.find_blob(entry.id()).unwrap();
            assert_eq!(blob.content(), content.as_bytes());
        }
        assert!(tree.get_name("unrelated.txt").is_none());

        // Verify that the staged tree was persisted to the index by reopening the repository.
        let reopened_repo = Repository::open(temp_dir.path()).unwrap();
        assert_eq!(reopened_repo.index().unwrap().write_tree().unwrap(), tree_id);

        // Verify that staging did not move HEAD or change the committed tree.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.id(), head);
        assert_ne!(head_commit.tree_id(), tree_id);
    }

    /// Tests that files are committed correctly and the new commit exists in the repository.
    #[test]
    fn test_commit() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a new file and commit it.
        temp_dir.child("README.md").write_str("lorem ipsum").unwrap();
        let tree_id = stage(&repo, &["README.md"]).unwrap();

        // Stage different contents and then remove the working-directory file.
        temp_dir.child("README.md").write_str("later contents").unwrap();
        let later_tree_id = stage(&repo, ["README.md"]).unwrap();
        std::fs::remove_file(temp_dir.child("README.md")).unwrap();

        // Commit the staged tree without reading or modifying the later index.
        let commit_id = commit(&repo, tree_id, "docs: add `README.md`").unwrap();

        // Verify that the commit uses the staged tree and leaves the later index unchanged.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head_commit.id(), commit_id);
        assert_eq!(head_commit.tree_id(), tree_id);
        assert_eq!(repo.index().unwrap().write_tree().unwrap(), later_tree_id);
        assert!(!temp_dir.child("README.md").path().exists());
    }

    /// Tests that a tag is created correctly and points to the expected commit.
    #[test]
    fn test_tag() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a new file and commit it.
        temp_dir.child("README.md").write_str("lorem ipsum").unwrap();
        let commit_id = stage_and_commit(&repo, &["README.md"], "docs: add `README.md`").unwrap();

        // Create a tag for the commit.
        tag(&repo, "v1.0.0", commit_id, false).unwrap();

        // Verify that the tag was created and points to the correct commit.
        let tag_ref = repo.find_reference("refs/tags/v1.0.0").unwrap();
        assert_eq!(tag_ref.peel_to_commit().unwrap().id(), commit_id);
    }

    /// Tests that a tag is overwritten correctly when forced and points to the expected commit.
    #[test]
    fn test_tag_with_force() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a new file and commit it.
        temp_dir.child("README.md").write_str("lorem ipsum").unwrap();
        let commit_id = stage_and_commit(&repo, &["README.md"], "docs: add `README.md`").unwrap();

        // Create a tag for the commit.
        tag(&repo, "v1.0.0", commit_id, false).unwrap();

        // Create a new commit and force overwrite the tag to point to the new commit.
        temp_dir.child("README.md").write_str("dolor sit amet").unwrap();
        let new_commit_id = stage_and_commit(&repo, &["README.md"], "docs: update `README.md`").unwrap();

        // Ensure that only a force tag succeeds.
        assert!(tag(&repo, "v1.0.0", new_commit_id, false).is_err());
        tag(&repo, "v1.0.0", new_commit_id, true).unwrap();

        // Verify that the tag was created and points to the correct commit.
        let tag_ref = repo.find_reference("refs/tags/v1.0.0").unwrap();
        assert_eq!(tag_ref.peel_to_commit().unwrap().id(), new_commit_id);
    }

    /// Tests that a branch is fetched from the origin remote.
    #[test]
    fn test_fetch_branch() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let (_remote_temp_dir, remote_repo) = create_test_repo();
        repo.remote("origin", remote_repo.path().to_str().unwrap()).unwrap();

        // Create a branch on the remote.
        let base = remote_repo.head().unwrap().target().unwrap();
        create_or_reset_branch(&remote_repo, "feature", base).unwrap();

        // Fetch the branch from the remote.
        let remote_ref = fetch_branch(&repo, "origin", "feature").unwrap();
        assert_eq!(remote_ref.unwrap(), "refs/remotes/origin/feature");

        // Verify that the local repository has the remote branch reference.
        assert_eq!(
            repo.find_reference("refs/remotes/origin/feature").unwrap().target(),
            Some(base),
        );
    }

    /// Tests that a branch that does not exist on the origin remote is handled correctly.
    #[test]
    fn test_fetch_branch_when_it_does_not_exist() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let (_remote_temp_dir, remote_repo) = create_test_repo();
        repo.remote("origin", remote_repo.path().to_str().unwrap()).unwrap();

        // Fetch the branch from the remote.
        let remote_ref = fetch_branch(&repo, "origin", "feature").unwrap();
        assert!(remote_ref.is_none());
    }

    /// Tests that references are listed from the origin remote.
    #[test]
    fn test_ls_remote() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let (_remote_temp_dir, remote_repo) = create_test_repo();
        repo.remote("origin", remote_repo.path().to_str().unwrap()).unwrap();

        // Create tags and a branch on the remote.
        let base = remote_repo.head().unwrap().target().unwrap();
        tag(&remote_repo, "v1.2.3", base, false).unwrap();
        tag(&remote_repo, "v2.0.0", base, false).unwrap();
        create_or_reset_branch(&remote_repo, "feature", base).unwrap();

        // List the tags from the remote.
        let mut remote_refs = ls_remote(&repo, "origin", "refs/tags/*").unwrap();
        remote_refs.sort();

        // Verify that only matching references are returned.
        assert_eq!(remote_refs, vec!["refs/tags/v1.2.3", "refs/tags/v2.0.0"]);

        // Verify that the tags were not fetched into the local repository.
        assert!(repo.find_reference("refs/tags/v1.2.3").is_err());
        assert!(repo.find_reference("refs/tags/v2.0.0").is_err());
    }

    /// Tests that a tag exists on the origin remote.
    #[test]
    fn test_tag_exists_on_remote() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let (_remote_temp_dir, remote_repo) = create_test_repo();
        repo.remote("origin", remote_repo.path().to_str().unwrap()).unwrap();

        // Create a tag on the remote.
        let base = remote_repo.head().unwrap().target().unwrap();
        tag(&remote_repo, "v1.2.3", base, false).unwrap();

        // Verify that the tag exists on the remote.
        assert!(tag_exists_on_remote(&repo, "origin", "v1.2.3").unwrap());

        // Verify that the tag was not fetched into the local repository.
        assert!(repo.find_reference("refs/tags/v1.2.3").is_err());
    }

    /// Tests that a tag does not exist on the origin remote.
    #[test]
    fn test_tag_exists_on_remote_not() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let (_remote_temp_dir, remote_repo) = create_test_repo();
        repo.remote("origin", remote_repo.path().to_str().unwrap()).unwrap();

        // Create a similarly named tag on the remote.
        let remote_base = remote_repo.head().unwrap().target().unwrap();
        tag(&remote_repo, "v1.2.30", remote_base, false).unwrap();

        // Create the requested tag only in the local repository.
        let local_base = repo.head().unwrap().target().unwrap();
        tag(&repo, "v1.2.3", local_base, false).unwrap();

        // Verify that the tag does not exist on the remote.
        assert!(!tag_exists_on_remote(&repo, "origin", "v1.2.3").unwrap());
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
            Some(base),
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
        stage_and_commit(&repo, &["tracked.txt"], "remote version").unwrap();
        push_branch(&repo, "feature", false).unwrap();

        // Rewrite the local branch.
        create_or_reset_branch(&repo, "feature", base).unwrap();
        temp_dir.child("tracked.txt").write_str("local version").unwrap();
        let local_version = stage_and_commit(&repo, &["tracked.txt"], "local version").unwrap();

        // Ensure that only a force push succeeds.
        assert!(push_branch(&repo, "feature", false).is_err());
        push_branch(&repo, "feature", true).unwrap();

        // Verify that the remote branch points to the rewritten commit.
        assert_eq!(
            remote_repo.find_reference("refs/heads/feature").unwrap().target(),
            Some(local_version),
        );
    }

    /// Tests that a tag is pushed to the origin remote.
    #[test]
    fn test_push_tag() {
        // Create a local and remote Git repository.
        let (_temp_dir, repo) = create_test_repo();
        let remote_dir = assert_fs::TempDir::new().unwrap();
        let remote_repo = Repository::init_bare(remote_dir.path()).unwrap();
        repo.remote("origin", remote_dir.path().to_str().unwrap()).unwrap();

        // Create and push a tag.
        let base = repo.head().unwrap().target().unwrap();
        tag(&repo, "v1.2.3", base, false).unwrap();
        push_tag(&repo, "v1.2.3", false).unwrap();

        // Verify that the remote tag points to the local commit.
        assert_eq!(
            remote_repo
                .find_reference("refs/tags/v1.2.3")
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id(),
            base,
        );
    }

    /// Tests that force pushing a tag overwrites the origin remote tag.
    #[test]
    fn test_push_tag_with_force() {
        // Create a local and remote Git repository.
        let (temp_dir, repo) = create_test_repo();
        let remote_dir = assert_fs::TempDir::new().unwrap();
        let remote_repo = Repository::init_bare(remote_dir.path()).unwrap();
        repo.remote("origin", remote_dir.path().to_str().unwrap()).unwrap();

        // Push a tag and then advance it on the remote.
        let base = repo.head().unwrap().target().unwrap();
        tag(&repo, "v1.2.3", base, false).unwrap();
        push_tag(&repo, "v1.2.3", false).unwrap();
        temp_dir.child("tracked.txt").write_str("remote version").unwrap();
        let remote_commit = stage_and_commit(&repo, &["tracked.txt"], "remote version").unwrap();
        tag(&repo, "v1.2.3", remote_commit, true).unwrap();
        push_tag(&repo, "v1.2.3", true).unwrap();

        // Rewrite the local tag.
        repo.tag_delete("v1.2.3").unwrap();
        temp_dir.child("tracked.txt").write_str("local version").unwrap();
        let local_commit = stage_and_commit(&repo, &["tracked.txt"], "local version").unwrap();
        tag(&repo, "v1.2.3", local_commit, true).unwrap();

        // Ensure that only a force push succeeds.
        assert!(push_tag(&repo, "v1.2.3", false).is_err());
        push_tag(&repo, "v1.2.3", true).unwrap();

        // Verify that the remote tag points to the rewritten commit.
        assert_eq!(
            remote_repo
                .find_reference("refs/tags/v1.2.3")
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .id(),
            local_commit,
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
        let base_commit = stage_and_commit(&repo, &["tracked.txt"], "base").unwrap();
        temp_dir.child("tracked.txt").write_str("latest").unwrap();
        stage_and_commit(&repo, &["tracked.txt"], "latest").unwrap();

        // Create a new branch at the base commit.
        create_or_reset_branch(&repo, "feature", base_commit).unwrap();

        // Verify that the new branch was created and checked out.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        assert_eq!(repo.head().unwrap().target(), Some(base_commit));
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("tracked.txt")).unwrap(),
            "base",
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
        let base_commit = stage_and_commit(&repo, &["tracked.txt"], "base").unwrap();
        temp_dir.child("tracked.txt").write_str("latest").unwrap();
        let latest_commit = stage_and_commit(&repo, &["tracked.txt"], "latest").unwrap();
        repo.branch("feature", &repo.find_commit(latest_commit).unwrap(), false)
            .unwrap();

        // Overwrite the existing branch at the base commit.
        create_or_reset_branch(&repo, "feature", base_commit).unwrap();

        // Verify that the branch was overwritten.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        assert_eq!(repo.head().unwrap().target(), Some(base_commit));
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("tracked.txt")).unwrap(),
            "base",
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
        let base_commit = stage_and_commit(&repo, &["tracked.txt"], "base").unwrap();
        temp_dir.child("tracked.txt").write_str("latest").unwrap();
        let latest_commit = stage_and_commit(&repo, &["tracked.txt"], "latest").unwrap();
        create_or_reset_branch(&repo, "feature", latest_commit).unwrap();
        assert_eq!(repo.head().unwrap().target(), Some(latest_commit));

        // Reset the currently checked out branch to the base commit.
        create_or_reset_branch(&repo, "feature", base_commit).unwrap();

        // Verify that the currently checked out branch was reset.
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        assert_eq!(repo.head().unwrap().target(), Some(base_commit));
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("tracked.txt")).unwrap(),
            "base",
        );
        assert!(repo.statuses(None).unwrap().is_empty());
    }

    /// Tests that two commits with identical tree contents are considered matching.
    #[test]
    fn test_trees_match() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a base commit with a file.
        temp_dir.child("file.txt").write_str("base content").unwrap();
        let base_commit = stage_and_commit(&repo, &["file.txt"], "base").unwrap();

        // Create a second commit with the same file content.
        temp_dir.child("file.txt").write_str("base content").unwrap();
        let same_commit = stage_and_commit(&repo, &["file.txt"], "same").unwrap();

        // Verify that the trees match for the same content.
        assert!(trees_match(&repo, &format!("{}", base_commit), &format!("{}", same_commit)).unwrap());
    }

    /// Tests that two commits with different tree contents are considered not matching.
    #[test]
    fn test_trees_match_not() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();

        // Create a base commit with a file.
        let base_commit = stage_and_commit(&repo, &["file.txt"], "base").unwrap();

        // Create a second commit with different file content.
        temp_dir.child("file.txt").write_str("different content").unwrap();
        let different_commit = stage_and_commit(&repo, &["file.txt"], "different").unwrap();

        // Verify that the trees do not match for different content.
        assert!(!trees_match(&repo, &format!("{}", base_commit), &format!("{}", different_commit)).unwrap());
    }

    /// Tests that two commits with the same commit ID are considered matching.
    #[test]
    fn test_trees_match_when_same_commit() {
        // Create a Git repository.
        let (_temp_dir, repo) = create_test_repo();

        // Create a base commit with a file.
        let base_commit = stage_and_commit(&repo, &["file.txt"], "base").unwrap();

        // Verify that the trees match when comparing the same commit.
        assert!(trees_match(&repo, &format!("{}", base_commit), &format!("{}", base_commit)).unwrap());
    }

    /// Tests that trees do not match when the target reference does not exist yet.
    #[test]
    fn test_trees_match_when_target_ref_does_not_exist() {
        // Create a Git repository.
        let (_temp_dir, repo) = create_test_repo();

        // Create a base commit with a file.
        let base_commit = stage_and_commit(&repo, &["file.txt"], "base").unwrap();

        // Verify that the trees match when the target ref does not exist.
        // NB: We assume that a non-existent ref is treated as an empty tree, which when pushed to will match.
        assert!(!trees_match(&repo, &format!("{}", base_commit), "nonexistent-ref").unwrap());
    }

    /// Tests that newly staged changes match a target commit with identical content.
    #[test]
    fn test_trees_match_with_staged_tree() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();
        let base = repo.head().unwrap().peel_to_commit().unwrap();

        // Create a target commit, then reset HEAD and the index while preserving files.
        temp_dir.child("file.txt").write_str("base content").unwrap();
        let target = stage_and_commit(&repo, &["file.txt"], "target").unwrap();
        repo.reset(base.as_object(), git2::ResetType::Mixed, None).unwrap();

        // Stage the same content again, leaving an unrelated file unstaged.
        temp_dir.child("unrelated.txt").write_str("unrelated").unwrap();
        let tree_id = stage(&repo, &["file.txt"]).unwrap();

        // Verify that actual changes were staged without moving HEAD and match the target.
        assert_eq!(
            repo.status_file(Path::new("file.txt")).unwrap(),
            git2::Status::INDEX_NEW
        );
        assert_eq!(repo.head().unwrap().target(), Some(base.id()));
        assert!(repo.find_tree(tree_id).unwrap().get_name("unrelated.txt").is_none());

        // Verify that the staged tree matches the target commit - same file contents.
        assert!(trees_match(&repo, &tree_id.to_string(), &target.to_string()).unwrap());
    }

    /// Tests that newly staged changes do not match a target commit with different content.
    #[test]
    fn test_trees_match_with_staged_tree_not() {
        // Create a Git repository.
        let (temp_dir, repo) = create_test_repo();
        let base = repo.head().unwrap().peel_to_commit().unwrap();

        // Create a target commit, then reset HEAD and the index while preserving files.
        temp_dir.child("file.txt").write_str("base content").unwrap();
        let target = stage_and_commit(&repo, &["file.txt"], "target").unwrap();
        repo.reset(base.as_object(), git2::ResetType::Mixed, None).unwrap();

        // Update the file and write an unrelated file.
        temp_dir.child("file.txt").write_str("updated content").unwrap();
        temp_dir.child("unrelated.txt").write_str("unrelated").unwrap();
        let tree_id = stage(&repo, &["file.txt"]).unwrap();

        // Verify that actual changes were staged without moving HEAD and do not match the target.
        assert_eq!(
            repo.status_file(Path::new("file.txt")).unwrap(),
            git2::Status::INDEX_NEW
        );
        assert_eq!(repo.head().unwrap().target(), Some(base.id()));
        assert!(repo.find_tree(tree_id).unwrap().get_name("unrelated.txt").is_none());

        // Verify that the staged tree does not match the target commit - different file contents.
        assert!(!trees_match(&repo, &tree_id.to_string(), &target.to_string()).unwrap());
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
        assert_eq!(remote.url, url);
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
        let new_commit_id = stage_and_commit(&repo, &["test.txt"], "feat: add test.txt").unwrap();
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
        let new_commit_id = stage_and_commit(&repo, &["feature.txt"], "feat: add feature.txt").unwrap();

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
