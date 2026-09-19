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

#[async_trait]
pub trait BaseGitProvider {
    async fn find_pull_request(&self, repo: &str, branch: &str) -> anyhow::Result<Option<String>>;

    async fn upsert_pull_request(&self, repo: &str, branch: &str, title: &str, body: &str) -> anyhow::Result<String>;
}

#[async_trait]
impl BaseGitProvider for GitProvider {
    async fn find_pull_request(&self, repo: &str, branch: &str) -> anyhow::Result<Option<String>> {
        match self {
            GitProvider::GitHub(provider) => provider.find_pull_request(repo, branch).await,
        }
    }

    async fn upsert_pull_request(&self, repo: &str, branch: &str, title: &str, body: &str) -> anyhow::Result<String> {
        match self {
            GitProvider::GitHub(provider) => provider.upsert_pull_request(repo, branch, title, body).await,
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
