use crate::git::{FileChange, collect_staged_changes};
use crate::provider::{BaseGitProvider, PullRequest};
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use octocrab::Octocrab;
use octocrab::commits::PullRequestTarget;
use octocrab::params::State;
use octocrab::params::repos::Reference;
use octocrab::repos::releases::MakeLatest;
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
    pub host: String,
    /// Whether to commit changes and create release tags via the GitHub API instead of using the local Git client.
    ///
    /// **Default:** `true`
    #[serde(default = "default_commit_via_api")]
    pub commit_via_api: bool,
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

/// Returns the default value for `$.commit_via_api`.
fn default_commit_via_api() -> bool {
    true
}

impl Default for GitHubProvider {
    fn default() -> Self {
        Self {
            host: default_host(),
            commit_via_api: default_commit_via_api(),
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

    /// Creates a commit via GitHub on the given branch with the specified commit message and file changes.
    ///
    /// See: [GitHub API Documentation](https://docs.github.com/en/graphql/reference/commits#mutation-createcommitonbranch)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the GitHub repository (user or organisation).
    /// * `repo_name` - The name of the GitHub repository.
    /// * `repo` - The local Git repository.
    /// * `branch_name` - The name of the branch to commit to.
    /// * `headline` - The commit message headline.
    /// * `body` - An optional commit message body.
    /// * `tree_id` - The staged tree ID returned by [`crate::git::stage`].
    ///
    /// # Returns
    ///
    /// A result containing the Git object ID (OID) of the new commit.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit(
        &self,
        owner: &str,
        repo_name: &str,
        repo: &git2::Repository,
        branch_name: &str,
        tree_id: git2::Oid,
        headline: &str,
        body: Option<&str>,
    ) -> anyhow::Result<git2::Oid> {
        #[derive(Debug, serde::Serialize)]
        struct CreateCommitOnBranchMutation {
            query: &'static str,
            variables: CommitVariables,
        }

        #[derive(Debug, serde::Serialize)]
        struct CommitVariables {
            input: CreateCommitOnBranchInput,
        }

        #[derive(Debug, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CreateCommitOnBranchInput {
            branch: CommittableBranch,
            expected_head_oid: String,
            message: CommitMessage,
            file_changes: FileChanges,
        }

        #[derive(Debug, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CommittableBranch {
            repository_name_with_owner: String,
            branch_name: String,
        }

        #[derive(Debug, serde::Serialize)]
        struct CommitMessage {
            headline: String,
            body: Option<String>,
        }

        #[derive(Debug, serde::Serialize)]
        struct FileChanges {
            additions: Vec<FileAddition>,
            deletions: Vec<FileDeletion>,
        }

        #[derive(Debug, serde::Serialize)]
        struct FileAddition {
            path: String,
            contents: String,
        }

        #[derive(Debug, serde::Serialize)]
        struct FileDeletion {
            path: String,
        }

        #[derive(Debug, serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CreateCommitOnBranchResponse {
            create_commit_on_branch: CreateCommitOnBranchPayload,
        }

        #[derive(Debug, serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CreateCommitOnBranchPayload {
            commit: Commit,
        }

        #[derive(Debug, serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Commit {
            oid: String,
        }

        let client = self.client()?;

        // Find the expected head OID of the branch.
        let expected_head_oid = repo
            .find_reference(&format!("refs/heads/{branch_name}"))
            .context("failed to find branch reference")?
            .target()
            .context("failed to get branch target OID")?;

        // Prepare the file changes for the commit.
        let changes = collect_staged_changes(repo, expected_head_oid, tree_id)?;
        let mut additions = Vec::new();
        let mut deletions = Vec::new();
        for change in changes {
            match change {
                FileChange::Create { path, contents } | FileChange::Update { path, contents } => {
                    additions.push(FileAddition {
                        path,
                        contents: STANDARD.encode(contents),
                    });
                }
                FileChange::Delete { path } => {
                    deletions.push(FileDeletion { path });
                }
            }
        }
        trace!(
            owner, repo_name, branch_name,
            expected_head_oid = %expected_head_oid,
            additions = additions.len(),
            deletions = deletions.len(),
            "execute GraphQL mutation `createCommitOnBranch`"
        );

        // Construct the GraphQL mutation payload.
        let payload = CreateCommitOnBranchMutation {
            query: "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
            variables: CommitVariables {
                input: CreateCommitOnBranchInput {
                    branch: CommittableBranch {
                        repository_name_with_owner: format!("{owner}/{repo_name}"),
                        branch_name: branch_name.to_string(),
                    },
                    expected_head_oid: expected_head_oid.to_string(),
                    file_changes: FileChanges { additions, deletions },
                    message: CommitMessage {
                        headline: headline.to_string(),
                        body: body.map(|s| s.to_string()),
                    },
                },
            },
        };

        // Execute the GraphQL mutation to create the commit on the branch.
        let response: CreateCommitOnBranchResponse = client
            .graphql(&payload)
            .await
            .with_context(|| format!("failed to create commit via GitHub on `{owner}/{repo_name}:{branch_name}`"))?;

        // Return the OID of the newly created commit.
        git2::Oid::from_str(&response.create_commit_on_branch.commit.oid)
            .context("GitHub returned an invalid commit OID")
    }

    /// Creates an annotated Git tag via GitHub at the given commit SHA, replacing the remote tag if it already exists.
    ///
    /// See: [GitHub API Documentation](https://docs.github.com/en/rest/git/tags?apiVersion=2026-03-10#create-a-tag-object)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the GitHub repository (user or organisation).
    /// * `repo` - The name of the GitHub repository.
    /// * `name` - The name of the tag to create.
    /// * `commit_sha` - The commit SHA to tag.
    ///
    /// # Returns
    ///
    /// A result indicating whether the tag was created successfully.
    pub async fn tag(&self, owner: &str, repo: &str, name: &str, commit_sha: &str) -> anyhow::Result<()> {
        let git = self.client()?.git(owner, repo);
        let reference = Reference::Tag(name.to_string());

        // Create the annotated tag object.
        trace!("create annotated tag `{name}` at commit `{commit_sha}`");
        let tag = git
            .create_tag(name, name, commit_sha, "commit")
            .send()
            .await
            .with_context(|| format!("failed to create tag `{name}` via GitHub on `{owner}/{repo}`"))?;

        // Create the reference, or force-update it when the tag already exists.
        trace!("set tag reference `{name}` to point to `{commit_sha}`");
        match git.create_ref(&reference, &tag.sha).await {
            // The reference already exists, force-update it to point to the new tag.
            Err(octocrab::Error::GitHub { source, .. })
                if source.status_code.as_u16() == 422 && source.message == "Reference already exists" =>
            {
                trace!("update existing tag reference `{name}` to point to `{commit_sha}`: {source:?}");
                git.update_ref(&reference, tag.sha).force(true).send().await
            }
            // The reference was created successfully.
            result => result,
        }
        .with_context(|| format!("failed to set tag reference `{name}` via GitHub on `{owner}/{repo}`"))?;

        Ok(())
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
        make_latest: bool,
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
            .make_latest(if make_latest {
                MakeLatest::True
            } else {
                MakeLatest::False
            })
            .send()
            .await
            .context("failed to create release")?;

        Ok(created.html_url.to_string())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::git::stage_and_commit;
    use assert_fs::{TempDir, prelude::*};
    use git2::Repository;
    use httpmock::prelude::*;
    use rstest::{fixture, rstest};
    use serde_json::json;
    use std::path::Path;

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

    /// Tests that a new release is created when one with the same tag name does not exist yet.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_release_creates_a_new_release(
        provider: GitHubProvider,
        #[values(true, false)] make_latest: bool,
    ) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let get_mock = server.mock(|when, then| {
            when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v1.0.0");
            then.status(404)
                .header("content-type", "application/json")
                .body(r#"{"message":"Not Found"}"#);
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/releases")
                .json_body(serde_json::json!({
                    "tag_name": "v1.0.0",
                    "target_commitish": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                    "name": "Release v1.0.0",
                    "body": "...",
                    "make_latest": if make_latest { "true" } else { "false" },
                }));
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_releases_retrieve.json"));
        });

        // Create a new release.
        let release = provider
            .upsert_release(
                "octocat",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                "Release v1.0.0",
                "...",
                make_latest,
            )
            .await;

        // Ensure the release was created successfully.
        get_mock.assert_async().await;
        create_mock.assert_async().await;
        assert_eq!(
            release.unwrap(),
            "https://github.com/octocat/Hello-World/releases/v1.0.0"
        );
    }

    /// Tests that an existing release is returned when one with the same tag name already exists.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_release_returns_an_existing_release(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let get_mock = server.mock(|when, then| {
            when.method(GET).path("/repos/octocat/Hello-World/releases/tags/v1.0.0");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_releases_retrieve.json"));
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST).path("/repos/octocat/Hello-World/releases");
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/github_releases_retrieve.json"));
        });

        // Retrieve an existing release.
        let release = provider
            .upsert_release(
                "octocat",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                "Release v1.0.0",
                "...",
                true,
            )
            .await;

        // Ensure the existing release was returned without creating a new one.
        get_mock.assert_async().await;
        create_mock.assert_calls_async(0).await;
        assert_eq!(
            release.unwrap(),
            "https://github.com/octocat/Hello-World/releases/v1.0.0"
        );
    }

    /// Tests that a commit is created on GitHub against the given branch name.
    #[rstest]
    #[tokio::test]
    async fn test_commit(provider: GitHubProvider) {
        // Initialise a new Git repository.
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(&temp_dir).unwrap();

        // Configure the Git author.
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@localhost").unwrap();

        // Create some files in the repository and commit them.
        for name in ["old.txt", "deleted.txt", "modified.txt", "skipped.txt"] {
            temp_dir.child(name).write_str("original").unwrap();
        }
        stage_and_commit(&repo, ["."], "initial").unwrap();
        let head_oid = repo.head().unwrap().target().unwrap();
        let branch_name = repo.head().unwrap().shorthand().unwrap().to_string();

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

        // Stage the selected files and then make another edit that must not be uploaded.
        let pathspecs = ["old.txt", "renamed.txt", "deleted.txt", "modified.txt", "assets/*"];
        let tree_id = crate::git::stage(&repo, pathspecs).unwrap();
        temp_dir.child("modified.txt").write_str("not staged").unwrap();

        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let graphql_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .json_body(json!({
                    "query": "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
                    "variables": {
                        "input": {
                            "branch": {
                                "repositoryNameWithOwner": "octocat/Hello-World",
                                "branchName": branch_name,
                            },
                            "expectedHeadOid": head_oid.to_string(),
                            "message": {
                                "headline": "feat",
                                "body": null
                            },
                            "fileChanges": {
                                "additions": [
                                    { "path": "assets/image.bin", "contents": "AP8K" },
                                    { "path": "modified.txt", "contents": "dXBkYXRlZA==" },
                                    { "path": "renamed.txt", "contents": "b3JpZ2luYWw=" }
                                ],
                                "deletions": [
                                    { "path": "deleted.txt" },
                                    { "path": "old.txt" }
                                ]
                            }
                        }
                    }
                }));
            then.status(200).header("content-type", "application/json").body(
                r#"{"data":{"createCommitOnBranch":{"commit":{"oid":"0123456789abcdef0123456789abcdef01234567"}}}}"#,
            );
        });

        // Create a commit from the captured staged tree on the branch.
        let oid = provider
            .commit("octocat", "Hello-World", &repo, &branch_name, tree_id, "feat", None)
            .await;

        // Ensure the commit was created successfully.
        graphql_mock.assert_async().await;
        assert_eq!(
            oid.unwrap(),
            git2::Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap(),
        );

        // Verify that the local branch, index, and working-directory contents are unchanged.
        assert_eq!(repo.head().unwrap().target().unwrap(), head_oid);
        assert_eq!(repo.index().unwrap().write_tree().unwrap(), tree_id);
        assert_eq!(
            repo.index().unwrap().get_path(Path::new("renamed.txt"), 0).unwrap().id,
            git2::Oid::hash_object(git2::ObjectType::Blob, b"original").unwrap(),
        );
        assert_eq!(std::fs::read(temp_dir.child("modified.txt")).unwrap(), b"not staged");
    }

    /// Tests that GraphQL errors and invalid commit OIDs are returned with operation context.
    #[rstest]
    #[case::graphql_error(
        r#"{"errors":[{"message":"expected head does not match"}]}"#,
        "failed to create commit via GitHub"
    )]
    #[case::invalid_oid(
        r#"{"data":{"createCommitOnBranch":{"commit":{"oid":"invalid"}}}}"#,
        "GitHub returned an invalid commit OID"
    )]
    #[tokio::test]
    async fn test_commit_with_errors(provider: GitHubProvider, #[case] response: &str, #[case] expected_error: &str) {
        // Initialise a new Git repository.
        let temp_dir = TempDir::new().unwrap();
        let repo = Repository::init(&temp_dir).unwrap();

        // Configure the Git author.
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@localhost").unwrap();

        // Commit a file and modify its working-directory contents.
        temp_dir.child("file.txt").write_str("original").unwrap();
        stage_and_commit(&repo, ["."], "initial").unwrap();
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        temp_dir.child("file.txt").write_str("updated").unwrap();

        // Stage the updated file for the API commit.
        let tree_id = crate::git::stage(&repo, ["file.txt"]).unwrap();

        // Mock a GraphQL error or an invalid commit OID.
        let server = mock_octocrab(&provider).await;
        let graphql_mock = server.mock(|when, then| {
            when.method(POST).path("/graphql");
            then.status(200)
                .header("content-type", "application/json")
                .body(response);
        });

        // Attempt to create a commit using the mocked response.
        let result = provider
            .commit("octocat", "Hello-World", &repo, &branch, tree_id, "headline", None)
            .await;

        // Verify that the request was submitted and the error includes operation context.
        graphql_mock.assert_async().await;
        let error = result.unwrap_err();
        assert!(error.to_string().contains(expected_error), "{error:#}");
    }

    /// Tests that a Git tag is created on GitHub against the given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_tag(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/git/tags")
                .json_body(json!({
                    "tag": "v1.0.0",
                    "message": "v1.0.0",
                    "object": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                    "type": "commit",
                }));
            then.status(201).json_body(json!({
                "node_id": "tag-object",
                "tag": "v1.0.0",
                "sha": "0123456789abcdef0123456789abcdef01234567",
                "url": "https://api.github.com/repos/octocat/Hello-World/git/tags/0123456789abcdef0123456789abcdef01234567",
                "message": "v1.0.0",
            }));
        });
        let ref_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/git/refs")
                .json_body(json!({
                    "ref": "refs/tags/v1.0.0",
                    "sha": "0123456789abcdef0123456789abcdef01234567",
                }));
            then.status(201).json_body(json!({
                "ref": "refs/tags/v1.0.0",
                "node_id": "tag-ref",
                "url": "https://api.github.com/repos/octocat/Hello-World/git/refs/tags/v1.0.0",
                "object": {
                    "type": "tag",
                    "sha": "0123456789abcdef0123456789abcdef01234567",
                    "url": "https://api.github.com/repos/octocat/Hello-World/git/tags/0123456789abcdef0123456789abcdef01234567",
                },
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider
            .tag(
                "octocat",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
            )
            .await;

        // Ensure the annotated tag and its reference were created successfully.
        tag_mock.assert_async().await;
        ref_mock.assert_async().await;
        result.unwrap();
    }

    /// Tests that a Git tag is replaced on GitHub against the given commit SHA when it already exists.
    #[rstest]
    #[tokio::test]
    async fn test_tag_updates_an_existing_tag(provider: GitHubProvider) {
        // Set up a mock server to simulate the GitHub API.
        let server = mock_octocrab(&provider).await;
        let tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/git/tags")
                .json_body(json!({
                    "tag": "v1.0.0",
                    "message": "v1.0.0",
                    "object": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
                    "type": "commit",
                }));
            then.status(201).json_body(json!({
                "node_id": "tag-object",
                "tag": "v1.0.0",
                "sha": "0123456789abcdef0123456789abcdef01234567",
                "url": "https://api.github.com/repos/octocat/Hello-World/git/tags/0123456789abcdef0123456789abcdef01234567",
                "message": "v1.0.0",
            }));
        });
        let create_ref_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/git/refs")
                .json_body(json!({
                    "ref": "refs/tags/v1.0.0",
                    "sha": "0123456789abcdef0123456789abcdef01234567",
                }));
            then.status(422)
                .json_body(json!({"message": "Reference already exists"}));
        });
        let update_ref_mock = server.mock(|when, then| {
            when.method(PATCH)
                .path("/repos/octocat/Hello-World/git/refs/tags/v1.0.0")
                .json_body(json!({
                    "sha": "0123456789abcdef0123456789abcdef01234567",
                    "force": true,
                }));
            then.status(200).json_body(json!({
                "ref": "refs/tags/v1.0.0",
                "node_id": "tag-ref",
                "url": "https://api.github.com/repos/octocat/Hello-World/git/refs/tags/v1.0.0",
                "object": {
                    "type": "tag",
                    "sha": "0123456789abcdef0123456789abcdef01234567",
                    "url": "https://api.github.com/repos/octocat/Hello-World/git/tags/0123456789abcdef0123456789abcdef01234567",
                },
            }));
        });

        // Replace an existing tag against the given commit SHA.
        let result = provider
            .tag(
                "octocat",
                "Hello-World",
                "v1.0.0",
                "6dcb09b5b57875f334f61aebed695e2e4193db5e",
            )
            .await;

        // Ensure the annotated tag was created and its existing reference was force-updated.
        tag_mock.assert_async().await;
        create_ref_mock.assert_async().await;
        update_ref_mock.assert_async().await;
        result.unwrap();
    }

    /// Tests that REST API errors and invalid commit SHAs are returned with operation context.
    #[rstest]
    #[case::api_error(
        403,
        "6dcb09b5b57875f334f61aebed695e2e4193db5e",
        r#"{"message":"Resource not accessible by integration"}"#,
        "failed to create tag `v1.0.0` via GitHub on `octocat/Hello-World`"
    )]
    #[case::invalid_sha(
        422,
        "invalid",
        r#"{"message":"Validation Failed","errors":[{"resource":"Tag","field":"object","code":"invalid"}]}"#,
        "failed to create tag `v1.0.0` via GitHub on `octocat/Hello-World`"
    )]
    #[tokio::test]
    async fn test_tag_with_errors(
        provider: GitHubProvider,
        #[case] status: u16,
        #[case] commit_sha: &str,
        #[case] response: &str,
        #[case] expected_error: &str,
    ) {
        // Set up a mock server to simulate a GitHub API error.
        let server = mock_octocrab(&provider).await;
        let tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/repos/octocat/Hello-World/git/tags")
                .json_body(json!({
                    "tag": "v1.0.0",
                    "message": "v1.0.0",
                    "object": commit_sha,
                    "type": "commit",
                }));
            then.status(status)
                .header("content-type", "application/json")
                .body(response);
        });
        let ref_mock = server.mock(|when, then| {
            when.method(POST).path("/repos/octocat/Hello-World/git/refs");
            then.status(201);
        });

        // Attempt to create a tag using the mocked response.
        let result = provider.tag("octocat", "Hello-World", "v1.0.0", commit_sha).await;

        // Ensure the error includes operation context and no reference was created.
        tag_mock.assert_async().await;
        ref_mock.assert_calls_async(0).await;
        let error = result.unwrap_err();
        assert!(error.to_string().contains(expected_error), "{error:#}");
    }
}
