use crate::provider::github::GitHubProvider;
use async_trait::async_trait;
use std::fmt;
use std::fmt::Display;
use std::str::FromStr;

pub mod github;

/// Available Git providers.
///
/// They define how to interact with a Git hosting service.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitProvider {
    /// The [GitHub](https://github.com/) Git provider.
    GitHub(GitHubProvider),
}

/// A pull request that has been created or updated on a Git provider.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PullRequest {
    /// The pull request number.
    pub number: u64,
    /// The URL of the pull request.
    pub url: String,
    /// The name of the branch where the changes are implemented, e.g. `reflow--branches--main`.
    pub head: String,
    /// The name of the branch the changes are pulled into, e.g. `main`.
    pub base: String,
    /// Whether the pull request was created (true) or updated (false).
    pub is_new: bool,
}

#[async_trait]
pub trait BaseGitProvider {
    /// Creates or updates a pull request on the Git provider.
    ///
    /// # Arguments
    ///
    /// - `owner` - The owner of the repository (user or organization).
    /// - `repo` - The name of the repository.
    /// - `head` - The name of the branch where your changes are implemented.
    /// - `base` - The name of the branch you want the changes pulled into.
    /// - `title` - The title of the pull request.
    /// - `body` - The body content of the pull request.
    ///
    /// # Returns
    ///
    /// A result containing the pull request number, URL, and whether it was created or updated.
    async fn upsert_pull_request(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<PullRequest>;
}

#[async_trait]
impl BaseGitProvider for GitProvider {
    async fn upsert_pull_request(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<PullRequest> {
        match self {
            GitProvider::GitHub(provider) => provider.upsert_pull_request(owner, repo, head, base, title, body).await,
        }
    }
}

impl Display for GitProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitProvider::GitHub(_) => write!(f, "github"),
        }
    }
}

impl Default for GitProvider {
    fn default() -> Self {
        GitProvider::GitHub(GitHubProvider::default())
    }
}

impl FromStr for GitProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(GitProvider::GitHub(GitHubProvider::default())),
            &_ => Err(format!("unknown Git provider `{s}`").to_string()),
        }
    }
}
