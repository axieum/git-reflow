use crate::provider::BaseGitProvider;
use anyhow::Context;
use async_trait::async_trait;
use octocrab::Octocrab;
use std::env;
use std::sync::OnceLock;

/// The [GitHub](https://github.com/) Git provider.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GitHubProvider {
    /// The GitHub host URL.
    ///
    /// **Default:** github.com
    #[serde(default = "default_host")]
    host: String,
    /// The [`Octocrab`] client used to interact with the GitHub API.
    ///
    /// **See Also:** [`GitHubProvider::client`]
    #[serde(skip)]
    client: OnceLock<Octocrab>,
}

/// Returns the default value for `$.host`.
fn default_host() -> String {
    String::from("github.com")
}

impl Default for GitHubProvider {
    fn default() -> Self {
        Self {
            host: default_host(),
            client: OnceLock::new(),
        }
    }
}

impl GitHubProvider {
    /// Constructs an [`Octocrab`] client using the `GITHUB_TOKEN` environment variable.
    ///
    /// # Returns
    /// An [`Octocrab`] instance or an error if the `GITHUB_TOKEN` environment variable is not set.
    fn client(&self) -> anyhow::Result<&Octocrab> {
        // Short-circuit if the client is already initialised.
        if let Some(client) = self.client.get() {
            return Ok(client);
        }

        // Prepare the Octocrab client with the GitHub token from the environment variable.
        let token = env::var("GITHUB_TOKEN").context("`GITHUB_TOKEN` environment variable is not set")?;
        let mut builder = Octocrab::builder().personal_token(token);

        // For GitHub Enterprise, set the base URI to the API endpoint.
        if self.host != "github.com" {
            builder = builder.base_uri(format!("https://{}/api/v3", self.host))?;
        }

        // Build the Octocrab client once and return it.
        let client = builder.build().context("failed to build octocrab client")?;
        Ok(self.client.get_or_init(|| client))
    }
}

#[async_trait]
impl BaseGitProvider for GitHubProvider {
    async fn find_pull_request(&self, _repo: &str, _branch: &str) -> anyhow::Result<Option<String>> {
        let _client = self.client()?;
        todo!()
    }

    async fn upsert_pull_request(
        &self,
        _repo: &str,
        _branch: &str,
        _title: &str,
        _body: &str,
    ) -> anyhow::Result<String> {
        let _client = self.client()?;
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::{fixture, rstest};

    /// A test fixture for a default GitHub provider.
    #[fixture]
    fn provider() -> GitHubProvider {
        GitHubProvider::default()
    }

    /// Tests that an [`Octocrab`] client is built _once_ from the `GITHUB_TOKEN` environment variable.
    #[rstest]
    #[tokio::test]
    async fn test_client(provider: GitHubProvider) {
        // Set the `GITHUB_TOKEN` environment variable for testing.
        unsafe { std::env::set_var("GITHUB_TOKEN", "test-token") }

        // Build the Octocrab client.
        provider.client().expect("failed to build octocrab client");

        // Ensure that the client is built only once.
        unsafe { std::env::remove_var("GITHUB_TOKEN") }
        provider.client().expect("octocrab client should be reused");
    }

    /// Tests that an [`Octocrab`] client cannot be built when the `GITHUB_TOKEN` environment variable is not set.
    #[rstest]
    #[tokio::test]
    async fn test_client_without_token(provider: GitHubProvider) {
        // Ensure the `GITHUB_TOKEN` environment variable is not set for this test.
        if std::env::var("GITHUB_TOKEN").is_ok() {
            unsafe { std::env::remove_var("GITHUB_TOKEN") }
        }

        // Attempt to build the Octocrab client.
        provider
            .client()
            .expect_err("the octocrab client was unexpectedly built");
    }
}
