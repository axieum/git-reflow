use crate::git::paths_relative_to_repo;
use crate::provider::{BaseGitProvider, PullRequest};
use anyhow::Context;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use git2::{Pathspec, PathspecFlags, Status, StatusOptions};
use octocrab::Octocrab;
use octocrab::params::State;
use serde_json::json;
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

    #[allow(clippy::too_many_arguments)]
    pub async fn create_commit_on_branch(
        &self,
        owner: &str,
        repo_name: &str,
        repo: &git2::Repository,
        branch_name: &str,
        message: &str,
        body: Option<&str>,
        pathspecs: &[&str],
    ) -> anyhow::Result<git2::Oid> {
        #[derive(Debug, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CreateCommitOnBranchMutation {
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
            .context("failed to get branch target OID")?
            .to_string();

        // Prepare the file changes for the commit.
        let workdir = repo.workdir().context("repository has no working directory")?;
        let mut additions: Vec<FileAddition> = Vec::new();
        let mut deletions: Vec<FileDeletion> = Vec::new();
        if !pathspecs.is_empty() {
            // Ensure that the pathspecs are relative to the repository root.
            let specs = paths_relative_to_repo(repo, pathspecs)?;
            let pathspec = Pathspec::new(specs)?;

            // Include untracked files and recurse into untracked directories to capture all changes.
            let mut options = StatusOptions::new();
            options
                .include_untracked(true)
                .recurse_untracked_dirs(true)
                .renames_head_to_index(true)
                .renames_index_to_workdir(true);

            // For each status entry, determine if it is an addition or deletion.
            for entry in repo.statuses(Some(&mut options))?.iter() {
                let old_path = entry
                    .head_to_index()
                    .or_else(|| entry.index_to_workdir())
                    .and_then(|delta| delta.old_file().path());
                let new_path = entry
                    .index_to_workdir()
                    .or_else(|| entry.head_to_index())
                    .and_then(|delta| delta.new_file().path());
                if old_path.is_some_and(|path| pathspec.matches_path(path, PathspecFlags::DEFAULT))
                    && new_path.is_some_and(|path| pathspec.matches_path(path, PathspecFlags::DEFAULT))
                {
                    let status = entry.status();

                    // If the file is deleted or renamed, add it to the deletions list.
                    if status.intersects(
                        Status::INDEX_DELETED | Status::WT_DELETED | Status::INDEX_RENAMED | Status::WT_RENAMED,
                    ) {
                        let old_path = old_path.context("deleted file has no path")?;
                        deletions.push(FileDeletion {
                            path: old_path
                                .to_str()
                                .context("deleted file path is not valid UTF-8")?
                                .to_string(),
                        });
                    }

                    // If the file is new, modified, or renamed, add it to the additions list with its base64 contents.
                    if status.intersects(
                        Status::INDEX_NEW
                            | Status::INDEX_MODIFIED
                            | Status::INDEX_RENAMED
                            | Status::INDEX_TYPECHANGE
                            | Status::WT_NEW
                            | Status::WT_MODIFIED
                            | Status::WT_RENAMED
                            | Status::WT_TYPECHANGE,
                    ) {
                        let new_path = new_path.context("added file has no path")?;
                        additions.push(FileAddition {
                            path: new_path
                                .to_str()
                                .context("added file path is not valid UTF-8")?
                                .to_string(),
                            contents: STANDARD.encode(
                                std::fs::read(workdir.join(new_path))
                                    .with_context(|| format!("failed to read file `{}`", new_path.display()))?,
                            ),
                        });
                    }
                }
            }
        }

        // Construct the GraphQL mutation payload.
        let payload = CreateCommitOnBranchMutation {
            input: CreateCommitOnBranchInput {
                branch: CommittableBranch {
                    repository_name_with_owner: format!("{owner}/{repo_name}"),
                    branch_name: branch_name.to_string(),
                },
                expected_head_oid,
                file_changes: FileChanges { additions, deletions },
                message: CommitMessage {
                    headline: message.to_string(),
                    body: body.map(|s| s.to_string()),
                },
            },
        };

        // Execute the GraphQL mutation to create the commit on the branch.
        trace!("execute GraphQL mutation `createCommitOnBranch` with: {payload:#?}");
        let response: CreateCommitOnBranchResponse = client
            .graphql(&json!({
                "query": "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
                "variables": payload,
            }))
            .await?;

        // Return the OID of the newly created commit.
        Ok(git2::Oid::from_str(&response.create_commit_on_branch.commit.oid)?)
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
            head: head.to_string(),
            base: base.to_string(),
            is_new: true,
            packages: vec![],
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use assert_fs::{TempDir, prelude::*};
    use git2::Repository;
    use httpmock::prelude::*;
    use rstest::{fixture, rstest};
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
                packages: vec![],
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
                packages: vec![],
            }
        );
    }

    /// Tests that a commit is created on GitHub against the given branch name.
    #[rstest]
    #[tokio::test]
    async fn test_create_commit_on_branch(provider: GitHubProvider) {
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
        crate::git::commit(&repo, ["."], "initial").unwrap();
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
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new("old.txt")).unwrap();
        index.add_path(Path::new("renamed.txt")).unwrap();
        index.write().unwrap();

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

        // Create a commit on the branch.
        let pathspecs = ["old.txt", "deleted.txt", "modified.txt", "assets/*"];
        let oid = provider
            .create_commit_on_branch("octocat", "Hello-World", &repo, &branch_name, "feat", None, &pathspecs)
            .await
            .unwrap();

        // Ensure the commit was created successfully.
        graphql_mock.assert_async().await;
        assert_eq!(
            oid,
            git2::Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap(),
        );
    }
}
