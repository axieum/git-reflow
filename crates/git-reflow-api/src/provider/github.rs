use crate::provider::{BaseGitProvider, PullRequest};
use anyhow::Context;
use async_trait::async_trait;
use octocrab::Octocrab;
use octocrab::params::State;
use std::env;
use std::sync::OnceLock;
use tracing::trace;

/// The [GitHub](https://github.com/) Git provider.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GitHubProvider {
    /// The GitHub host, or a full API URL for a custom endpoint.
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
    ///
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
            let base_uri = if self.host.contains("://") {
                self.host.clone()
            } else {
                format!("https://{}/api/v3", self.host)
            };
            builder = builder.base_uri(base_uri)?;
        }

        // Build the Octocrab client once and return it.
        let client = builder.build().context("failed to build octocrab client")?;
        Ok(self.client.get_or_init(|| client))
    }
}

#[async_trait]
impl BaseGitProvider for GitHubProvider {
    async fn upsert_pull_request(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<PullRequest> {
        let client = self.client()?;
        let pulls = client.pulls(owner, repo);

        let head_filter = format!("{owner}:{head}");
        trace!("find open pull requests with head: {head_filter}; base: {base}");
        let prs = pulls
            .list()
            .state(State::Open)
            .head(&head_filter)
            .base(base)
            .per_page(1)
            .send()
            .await
            .context("could not list pull requests")?;

        // If a pull request already exists, update it.
        if let Some(pr) = prs.items.first() {
            trace!("updating existing pull request #{}", pr.number);
            let updated = pulls
                .update(pr.number)
                .title(title)
                .body(body)
                .send()
                .await
                .context("failed to update pull request")?;
            return Ok(PullRequest {
                number: updated.number,
                url: updated
                    .html_url
                    .map(|url| url.to_string())
                    .unwrap_or_else(|| format!("https://{}/{}/{}/pull/{}", self.host, owner, repo, updated.number)),
                head: head.to_string(),
                base: base.to_string(),
                is_new: false,
            });
        }

        // A pull request does not exist yet, create one.
        trace!("creating new pull request: {} -> {}", head, base);
        let created = pulls
            .create(title, head, base)
            .body(body)
            .send()
            .await
            .context("failed to create pull request")?;

        Ok(PullRequest {
            number: created.number,
            url: created
                .html_url
                .map(|url| url.to_string())
                .unwrap_or_else(|| format!("https://{}/{}/{}/pull/{}", self.host, owner, repo, created.number)),
            head: head.to_string(),
            base: base.to_string(),
            is_new: true,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use httpmock::prelude::*;
    use rstest::{fixture, rstest};

    /// A test fixture for a default GitHub provider.
    #[fixture]
    fn provider() -> GitHubProvider {
        GitHubProvider::default()
    }

    /// Starts an HTTP mock server and configures the [`Octocrab`] client to use it for the given provider.
    pub(crate) async fn mock_octocrab(provider: &GitHubProvider) -> MockServer {
        let server = MockServer::start_async().await;
        provider
            .client
            .set(
                Octocrab::builder()
                    .base_uri(server.base_url())
                    .unwrap()
                    .build()
                    .unwrap(),
            )
            .unwrap();
        server
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

    /// Tests that a new pull request is created when one does not exist yet.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_creates_a_new_pr(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/pulls")
                .query_param("head", "octocat:reflow--branches--main")
                .query_param("base", "main");
            then.status(200).header("content-type", "application/json").body("[]");
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST).path("/repos/octocat/Hello-World/pulls");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_pulls_retrieve.json"));
        });

        // Create a new pull request.
        let pr = provider
            .upsert_pull_request(
                "octocat",
                "Hello-World",
                "reflow--branches--main",
                "main",
                "chore: release v1.0.0",
                "...",
            )
            .await
            .unwrap();

        // Ensure the pull request was created successfully.
        list_mock.assert_async().await;
        create_mock.assert_async().await;
        assert_eq!(
            pr,
            PullRequest {
                number: 1347,
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                is_new: true,
            }
        );
    }

    /// Tests that an existing pull request is updated when one already exists.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_updates_an_existing_pr(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/pulls")
                .query_param("head", "octocat:reflow--branches--main")
                .query_param("base", "main");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_pulls_list.json"));
        });
        let update_mock = server.mock(|when, then| {
            when.method(PATCH).path("/repos/octocat/Hello-World/pulls/1347");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_pulls_retrieve.json"));
        });

        // Update an existing pull request.
        let pr = provider
            .upsert_pull_request(
                "octocat",
                "Hello-World",
                "reflow--branches--main",
                "main",
                "chore: release v1.0.0",
                "...",
            )
            .await
            .unwrap();

        // Ensure the existing pull request was updated successfully.
        list_mock.assert_async().await;
        update_mock.assert_async().await;
        assert_eq!(
            pr,
            PullRequest {
                number: 1347,
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                is_new: false,
            }
        );
    }
}
