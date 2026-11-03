use crate::provider::{BaseGitProvider, PullRequest};
use anyhow::Context;
use async_trait::async_trait;
use serde_json::json;
use std::env;
use std::sync::OnceLock;
use tracing::trace;

/// The [Bitbucket](https://bitbucket.org/) Git provider.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BitbucketProvider {
    /// The Bitbucket host, or a full API URL for a custom endpoint.
    ///
    /// **Default:** api.bitbucket.org
    #[serde(default = "default_host")]
    pub host: String,
    /// Whether to commit changes and create release tags via the Bitbucket API instead of using the local Git client.
    ///
    /// **Default:** `true`
    #[serde(default = "default_commit_via_api")]
    pub commit_via_api: bool,
    /// The [`reqwest`] client used to interact with the Bitbucket API.
    ///
    /// **See Also:** [`BitbucketProvider::client`]
    #[serde(skip)]
    client: OnceLock<reqwest::Client>,
    /// The base URL for the Bitbucket API, derived from `$.host`.
    #[serde(skip)]
    base_url: OnceLock<reqwest::Url>,
}

/// Returns the default value for `$.host`.
fn default_host() -> String {
    String::from("api.bitbucket.org")
}

/// Returns the default value for `$.commit_via_api`.
fn default_commit_via_api() -> bool {
    true
}

impl Default for BitbucketProvider {
    fn default() -> Self {
        Self {
            host: default_host(),
            commit_via_api: default_commit_via_api(),
            client: OnceLock::new(),
            base_url: OnceLock::new(),
        }
    }
}

impl BitbucketProvider {
    /// Constructs a [`reqwest`] client using the `BITBUCKET_TOKEN` environment variable.
    ///
    /// # Returns
    ///
    /// A [`reqwest::Client`] instance or an error if the `BITBUCKET_TOKEN` environment variable is not set.
    fn client(&self) -> anyhow::Result<&reqwest::Client> {
        // Short-circuit if the client is already initialised.
        if let Some(client) = self.client.get() {
            return Ok(client);
        }

        // Prepare the reqwest client with the Bitbucket token from the environment variable.
        let token = env::var("BITBUCKET_TOKEN").context("`BITBUCKET_TOKEN` environment variable is not set")?;
        let builder = reqwest::Client::builder().default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            let mut bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
                .context("failed to create bearer token header")?;
            bearer.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, bearer);
            headers
        });

        // Prepare the base URI to the API endpoint.
        self.base_url.get_or_init(|| {
            let base_url = if self.host.contains("://") {
                self.host.clone()
            } else {
                format!("https://{}/", self.host)
            };
            base_url.parse().expect("invalid Bitbucket API URL")
        });

        // Build the reqwest client once and return it.
        let client = builder.build().context("failed to build reqwest client")?;
        Ok(self.client.get_or_init(|| client))
    }

    /// Builds an endpoint URL by joining the given path with the configured base URL.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to join with the base URL.
    ///
    /// # Returns
    ///
    /// A result containing the [`reqwest::Url`].
    fn endpoint(&self, path: &str) -> anyhow::Result<reqwest::Url> {
        self.base_url
            .get()
            .expect("reqwest client is uninitialized")
            .join(path)
            .context("failed to join endpoint path")
    }

    /// Creates an annotated Git tag via Bitbucket at the given commit SHA, replacing the remote tag
    /// if it already exists.
    ///
    /// See: [Bitbucket Cloud API Documentation](https://developer.atlassian.com/cloud/bitbucket/rest/api-group-refs/#api-repositories-workspace-repo-slug-refs-tags-post),
    /// [Bitbucket Server API Documentation](https://developer.atlassian.com/server/bitbucket/rest/v1005/api-group-repository/#api-git-latest-projects-projectkey-repos-repositoryslug-tags-post)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the Bitbucket repository (user or organisation).
    /// * `repo` - The name of the Bitbucket repository.
    /// * `name` - The name of the tag to create.
    /// * `commit_sha` - The commit SHA to tag.
    ///
    /// # Returns
    ///
    /// A result indicating whether the tag was created successfully.
    pub async fn tag(&self, owner: &str, repo: &str, name: &str, commit_sha: &str) -> anyhow::Result<()> {
        if self.host == "api.bitbucket.org" {
            self.tag_via_cloud(owner, repo, name, commit_sha).await
        } else {
            self.tag_via_data_center(owner, repo, name, commit_sha).await
        }
    }

    /// Creates an annotated Git tag via Bitbucket Cloud at the given commit SHA, replacing the remote tag
    /// if it already exists.
    ///
    /// NB: The replacement uses separate delete and create requests and is not atomic.
    ///
    /// See: [Bitbucket Cloud API Documentation](https://developer.atlassian.com/cloud/bitbucket/rest/api-group-refs/#api-repositories-workspace-repo-slug-refs-tags-post)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the Bitbucket repository (user or organisation).
    /// * `repo` - The name of the Bitbucket repository.
    /// * `name` - The name of the tag to create.
    /// * `commit_sha` - The commit SHA to tag.
    ///
    /// # Returns
    ///
    /// A result indicating whether the tag was created successfully.
    pub async fn tag_via_cloud(&self, owner: &str, repo: &str, name: &str, commit_sha: &str) -> anyhow::Result<()> {
        let client = self.client()?;
        let endpoint = self.endpoint(&format!("2.0/repositories/{}/{}/refs/tags", owner, repo))?;

        // Check whether the tag already exists, and needs to be deleted before creating a new one.
        trace!("check for existing tag `{name}`");
        let tag_endpoint = self.endpoint(&format!("2.0/repositories/{}/{}/refs/tags/{}", owner, repo, name))?;
        let existing = client
            .get(tag_endpoint.clone())
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("failed to find tag `{name}` via Bitbucket Cloud on `{owner}/{repo}`"))?;
        if existing.status() != reqwest::StatusCode::NOT_FOUND {
            existing
                .error_for_status()
                .with_context(|| format!("failed to find tag `{name}` via Bitbucket Cloud on `{owner}/{repo}`"))?;

            // Validate the target commit before removing the existing tag.
            trace!("validate target commit `{commit_sha}` before deleting existing tag `{name}`");
            client
                .get(self.endpoint(&format!("2.0/repositories/{}/{}/commit/{}", owner, repo, commit_sha))?)
                .header("Accept", "application/json")
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .with_context(|| {
                    format!("failed to find commit `{commit_sha}` via Bitbucket Cloud on `{owner}/{repo}`")
                })?;

            // Bitbucket Cloud cannot force-create a tag, so delete it before recreating it.
            trace!("delete existing tag `{name}`");
            client
                .delete(tag_endpoint)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .with_context(|| format!("failed to delete tag `{name}` via Bitbucket Cloud on `{owner}/{repo}`"))?;
        }

        // Create the annotated tag object.
        trace!("create annotated tag `{name}` at commit `{commit_sha}`");
        client
            .post(endpoint)
            .header("Accept", "application/json")
            .json(&json!({
                "message": name,
                "name": name,
                "target": { "hash": commit_sha },
            }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("failed to create tag `{name}` via Bitbucket Cloud on `{owner}/{repo}`"))?;

        Ok(())
    }

    /// Creates an annotated Git tag via Bitbucket Data Center at the given commit SHA, replacing the remote tag
    /// if it already exists.
    ///
    /// See: [Bitbucket Server API Documentation](https://developer.atlassian.com/server/bitbucket/rest/v1005/api-group-repository/#api-git-latest-projects-projectkey-repos-repositoryslug-tags-post)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the Bitbucket repository (user or organisation).
    /// * `repo` - The name of the Bitbucket repository.
    /// * `name` - The name of the tag to create.
    /// * `commit_sha` - The commit SHA to tag.
    ///
    /// # Returns
    ///
    /// A result indicating whether the tag was created successfully.
    pub async fn tag_via_data_center(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
        commit_sha: &str,
    ) -> anyhow::Result<()> {
        let client = self.client()?;
        let endpoint = self.endpoint(&format!("rest/git/latest/projects/{}/repos/{}/tags", owner, repo))?;

        // Create the annotated tag object.
        trace!("create annotated tag `{name}` at commit `{commit_sha}`");
        client
            .post(endpoint)
            .header("Accept", "application/json;charset=UTF-8")
            .json(&json!({
                "force": true,
                "message": name,
                "name": name,
                "startPoint": commit_sha,
                "type": "ANNOTATED",
            }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("failed to create tag `{name}` via Bitbucket Data Center on `{owner}/{repo}`"))?;

        Ok(())
    }
}

#[async_trait]
impl BaseGitProvider for BitbucketProvider {
    async fn associated_pull_requests(
        &self,
        _owner: &str,
        _repo: &str,
        _commit: &str,
    ) -> anyhow::Result<Vec<PullRequest>> {
        todo!();
    }

    async fn upsert_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        _head: &str,
        _base: &str,
        _title: &str,
        _body: &str,
    ) -> anyhow::Result<PullRequest> {
        todo!();
    }

    async fn upsert_release(
        &self,
        _owner: &str,
        _repo: &str,
        _tag_name: &str,
        _commit_sha: &str,
        _title: &str,
        _body: &str,
        _make_latest: bool,
    ) -> anyhow::Result<String> {
        todo!();
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use httpmock::{MockServer, prelude::*};
    use rstest::{fixture, rstest};

    /// A test fixture for a default Bitbucket provider.
    #[fixture]
    fn provider(#[default("api.bitbucket.org")] host: &str) -> BitbucketProvider {
        BitbucketProvider {
            host: host.to_string(),
            ..Default::default()
        }
    }

    /// Starts an HTTP mock server and configures the [`reqwest`] client to use it for the given provider.
    pub(crate) async fn mock_reqwest(provider: &BitbucketProvider) -> MockServer {
        let server = MockServer::start_async().await;
        provider.client.set(reqwest::Client::new()).unwrap();
        provider.base_url.set(server.base_url().parse().unwrap()).unwrap();
        server
    }

    /// Tests that a [`reqwest`] client is built _once_ from the `BITBUCKET_TOKEN` environment variable.
    #[rstest]
    #[tokio::test]
    async fn test_client(provider: BitbucketProvider) {
        // Set the `BITBUCKET_TOKEN` environment variable for testing.
        unsafe { std::env::set_var("BITBUCKET_TOKEN", "test-token") }

        // Build the reqwest client.
        provider.client().expect("failed to build reqwest client");

        // Ensure that the client is built only once.
        unsafe { std::env::remove_var("BITBUCKET_TOKEN") }
        provider.client().expect("reqwest client should be reused");
    }

    /// Tests that an [`reqwest`] client cannot be built when the `BITBUCKET_TOKEN` environment variable is not set.
    #[rstest]
    #[tokio::test]
    async fn test_client_without_token(provider: BitbucketProvider) {
        // Ensure the `BITBUCKET_TOKEN` environment variable is not set for this test.
        if std::env::var("BITBUCKET_TOKEN").is_ok() {
            unsafe { std::env::remove_var("BITBUCKET_TOKEN") }
        }

        // Attempt to build the reqwest client.
        provider
            .client()
            .expect_err("the reqwest client was unexpectedly built");
    }

    /// Tests that an endpoint URL is constructed correctly from the base URL and a given path.
    #[rstest]
    #[tokio::test]
    async fn test_endpoint(provider: BitbucketProvider) {
        // Set the `BITBUCKET_TOKEN` environment variable for testing.
        unsafe { std::env::set_var("BITBUCKET_TOKEN", "test-token") }

        // Build the reqwest client.
        provider.client().expect("failed to build reqwest client");

        // Verify that the endpoint URL is constructed correctly.
        let endpoint = provider.endpoint("2.0/repositories/axieum/Hello-World").unwrap();
        assert_eq!(
            endpoint.as_str(),
            "https://api.bitbucket.org/2.0/repositories/axieum/Hello-World",
        );
    }

    /// Tests that an endpoint URL is constructed correctly with a custom host URL and a given path.
    #[rstest]
    #[tokio::test]
    async fn test_endpoint_with_custom_host(#[with("bb.example.com")] provider: BitbucketProvider) {
        // Set the `BITBUCKET_TOKEN` environment variable for testing.
        unsafe { std::env::set_var("BITBUCKET_TOKEN", "test-token") }

        // Build the reqwest client.
        provider.client().expect("failed to build reqwest client");

        // Verify that the endpoint URL is constructed correctly with the custom host.
        let endpoint = provider.endpoint("2.0/repositories/axieum/Hello-World").unwrap();
        assert_eq!(
            endpoint.as_str(),
            "https://bb.example.com/2.0/repositories/axieum/Hello-World",
        );
    }

    /// Tests that a Git tag is created on Bitbucket Cloud against the given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_tag_via_cloud(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let get_tag_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags/v1.0.0")
                .header("Accept", "application/json");
            then.status(404);
        });
        let create_tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "message": "v1.0.0",
                    "name": "v1.0.0",
                    "target": { "hash": "6dcb09b5b57875f334f61aebed695e2e4193db5e" },
                }));
            then.status(201).json_body(json!({
                "message": "v1.0.0",
                "name": "v1.0.0",
                "target": { "hash": "6dcb09b5b57875f334f61aebed695e2e4193db5e" },
                "type": "TAG",
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider
            .tag(
                "axieum",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
            )
            .await;

        // Ensure the annotated tag was created successfully.
        get_tag_mock.assert_async().await;
        create_tag_mock.assert_async().await;
        result.unwrap();
    }

    /// Tests that a Git tag is replaced on Bitbucket Cloud against the given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_tag_via_cloud_with_existing_tag(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let get_tag_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags/v1.0.0")
                .header("Accept", "application/json");
            then.status(200);
        });
        let get_commit_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/commit/6dcb09b5b57875f334f61aebed695e2e4193db5e")
                .header("Accept", "application/json");
            then.status(200).json_body(json!({
                "hash": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                "type": "commit",
            }));
        });
        let delete_tag_mock = server.mock(|when, then| {
            when.method(DELETE)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags/v1.0.0");
            then.status(204);
        });
        let create_tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "message": "v1.0.0",
                    "name": "v1.0.0",
                    "target": { "hash": "6dcb09b5b57875f334f61aebed695e2e4193db5e" },
                }));
            then.status(201).json_body(json!({
                "message": "v1.0.0",
                "name": "v1.0.0",
                "target": { "hash": "6dcb09b5b57875f334f61aebed695e2e4193db5e" },
                "type": "TAG",
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider
            .tag(
                "axieum",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
            )
            .await;

        // Ensure the annotated tag was created successfully.
        get_tag_mock.assert_async().await;
        get_commit_mock.assert_async().await;
        delete_tag_mock.assert_async().await;
        create_tag_mock.assert_async().await;
        result.unwrap();
    }

    /// Tests that a Git tag is created on Bitbucket Data Center against the given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_tag_via_data_center(#[with("bb.example.com")] provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Data Center API.
        let server = mock_reqwest(&provider).await;
        let tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/git/latest/projects/axieum/repos/Hello-World/tags")
                .header("Accept", "application/json;charset=UTF-8")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "force": true,
                    "message": "v1.0.0",
                    "name": "v1.0.0",
                    "startPoint": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                    "type": "ANNOTATED",
                }));
            then.status(201).json_body(json!({
                "id": "refs/tags/v1.0.0",
                "displayId": "v1.0.0",
                "latestCommit": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                "type": "TAG",
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider
            .tag(
                "axieum",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
            )
            .await;

        // Ensure the annotated tag was created successfully.
        tag_mock.assert_async().await;
        result.unwrap();
    }
}
