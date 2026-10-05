use crate::provider::{BaseGitProvider, PullRequest};
use anyhow::Context;
use async_trait::async_trait;
use octocrab::Octocrab;
use octocrab::commits::PullRequestTarget;
use octocrab::params::State;
use std::borrow::Cow;
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
    async fn associated_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
    ) -> anyhow::Result<Vec<PullRequest>> {
        let client = self.client()?;
        let commits = client.commits(owner, repo);

        // Find all pull requests associated with the given commit SHA.
        trace!("find pull requests associated with commit `{commit}`");
        let prs = commits
            .associated_pull_requests(PullRequestTarget::Sha(commit.to_string()))
            .send()
            .await
            .with_context(|| format!("could not list associated pull requests for commit `{commit}`"))?
            .items
            .iter()
            .map(|pr| PullRequest {
                number: pr.number,
                url: pr
                    .html_url
                    .as_ref()
                    .map(|url| url.to_string())
                    .unwrap_or_else(|| format!("https://{}/{}/{}/pull/{}", self.host, owner, repo, pr.number)),
                title: pr.title.clone().unwrap_or_default(),
                body: pr.body.clone(),
                head: pr.head.ref_field.clone(),
                base: pr.base.ref_field.clone(),
                existing: true,
                packages: vec![],
            })
            .collect::<Vec<PullRequest>>();

        trace!("found {} associated pull requests for commit `{commit}`", prs.len());
        Ok(prs)
    }

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

        // Find an open pull request with the same head and base branches.
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
            let updated = if pr.title.as_deref() != Some(title) || pr.body.as_deref() != Some(body) {
                trace!("updating existing pull request #{}", pr.number);
                let updated = pulls
                    .update(pr.number)
                    .title(title)
                    .body(body)
                    .send()
                    .await
                    .context("failed to update pull request")?;
                Cow::Owned(updated)
            } else {
                trace!("existing pull request #{} is already up-to-date", pr.number);
                Cow::Borrowed(pr)
            };
            return Ok(PullRequest {
                number: updated.number,
                url: updated
                    .html_url
                    .as_ref()
                    .map(|url| url.to_string())
                    .unwrap_or_else(|| format!("https://{}/{}/{}/pull/{}", self.host, owner, repo, updated.number)),
                title: updated.title.clone().unwrap_or_default(),
                body: updated.body.clone(),
                head: head.to_string(),
                base: base.to_string(),
                existing: true,
                packages: vec![],
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
            title: title.to_string(),
            body: Some(body.to_string()),
            head: head.to_string(),
            base: base.to_string(),
            existing: false,
            packages: vec![],
        })
    }

    async fn upsert_release(
        &self,
        owner: &str,
        repo: &str,
        tag_name: &str,
        commit_sha: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let client = self.client()?;
        let repos = client.repos(owner, repo);
        let releases = repos.releases();

        // Check for an existing release with the same tag name.
        trace!("find release with tag name: {}", tag_name);
        if let Ok(release) = releases.get_by_tag(tag_name).await.context("could not find release") {
            trace!(
                "release with tag name `{}` already exists: {}",
                tag_name, release.html_url,
            );
            return Ok(release.html_url.to_string());
        }

        // A release does not exist yet, create one.
        trace!("creating new release with tag name: {}", tag_name);
        let created = releases
            .create(tag_name)
            .target_commitish(commit_sha)
            .name(title)
            .body(body)
            .send()
            .await
            .context("failed to create release")?;

        Ok(created.html_url.to_string())
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

    /// Tests that associated pull requests are returned for a given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_associated_pull_requests(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/commits/6dcb09b5b57875f334f61aebed695e2e4193db5e/pulls");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_commit_pulls.json"));
        });

        // Find associated pull requests for a commit SHA.
        let prs = provider
            .associated_pull_requests("octocat", "Hello-World", "6dcb09b5b57875f334f61aebed695e2e4193db5e")
            .await;

        // Ensure the associated pull requests were retrieved successfully.
        commit_pulls_mock.assert_async().await;
        assert_eq!(
            prs.unwrap(),
            vec![PullRequest {
                number: 1347,
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                title: "Amazing new feature".to_string(),
                body: Some("Please pull these awesome changes in!".to_string()),
                head: "new-topic".to_string(),
                base: "master".to_string(),
                existing: true,
                packages: vec![],
            }],
        );
    }

    /// Tests that no associated pull requests are returned for a given commit SHA when none exist.
    #[rstest]
    #[tokio::test]
    async fn test_associated_pull_requests_when_none_exist(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/repos/octocat/Hello-World/commits/6dcb09b5b57875f334f61aebed695e2e4193db5e/pulls");
            then.status(200).header("content-type", "application/json").body("[]");
        });

        // Find associated pull requests for a commit SHA.
        let prs = provider
            .associated_pull_requests("octocat", "Hello-World", "6dcb09b5b57875f334f61aebed695e2e4193db5e")
            .await;

        // Ensure the associated pull requests were retrieved successfully.
        commit_pulls_mock.assert_async().await;
        assert!(prs.unwrap().is_empty());
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
            .await;

        // Ensure the pull request was created successfully.
        list_mock.assert_async().await;
        create_mock.assert_async().await;
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                number: 1347,
                title: "chore: release v1.0.0".to_string(),
                body: Some("...".to_string()),
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: false,
                packages: vec![],
            },
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
            .await;

        // Ensure the existing pull request was updated successfully.
        list_mock.assert_async().await;
        update_mock.assert_async().await;
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                number: 1347,
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                title: "Amazing new feature".to_string(),
                body: Some("Please pull these awesome changes in!".to_string()),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: true,
                packages: vec![],
            },
        );
    }

    /// Tests that an existing pull request is not updated when it is already up-to-date.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_does_not_update_if_already_up_to_date(provider: GitHubProvider) {
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

        // Update an existing pull request.
        let pr = provider
            .upsert_pull_request(
                "octocat",
                "Hello-World",
                "reflow--branches--main",
                "main",
                // NB: This title and body match the existing PR in the fixture.
                "Amazing new feature",
                "Please pull these awesome changes in!",
            )
            .await;

        // Ensure the existing pull request remains unchanged.
        // NB: We expect it to fetch the existing pull request, but not update it.
        list_mock.assert_async().await;
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                number: 1347,
                url: "https://github.com/octocat/Hello-World/pull/1347".to_string(),
                title: "Amazing new feature".to_string(),
                body: Some("Please pull these awesome changes in!".to_string()),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: true,
                packages: vec![],
            },
        );
    }
}
