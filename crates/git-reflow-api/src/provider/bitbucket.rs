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

    /// Finds pull requests associated with a given commit SHA on Bitbucket Cloud.
    ///
    /// See: [Bitbucket Cloud API Documentation](https://developer.atlassian.com/cloud/bitbucket/rest/api-group-pullrequests/#api-repositories-workspace-repo-slug-commit-commit-pullrequests-get)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the repository (user or organisation).
    /// * `repo` - The name of the repository.
    /// * `commit` - The commit SHA to search for associated pull requests.
    ///
    /// # Returns
    ///
    /// A result containing a list of pull requests associated with the given commit SHA, if any.
    pub async fn associated_pull_requests_via_cloud(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
    ) -> anyhow::Result<Vec<PullRequest>> {
        #[derive(serde::Deserialize)]
        struct Response {
            values: Vec<CloudPullRequest>,
        }

        #[derive(serde::Deserialize)]
        struct CloudPullRequest {
            id: u64,
            title: String,
            summary: Summary,
            links: Links,
            source: BranchReference,
            destination: BranchReference,
        }

        #[derive(serde::Deserialize)]
        struct Summary {
            raw: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct Links {
            html: Link,
        }

        #[derive(serde::Deserialize)]
        struct Link {
            href: String,
        }

        #[derive(serde::Deserialize)]
        struct BranchReference {
            branch: Branch,
        }

        #[derive(serde::Deserialize)]
        struct Branch {
            name: String,
        }

        let client = self.client()?;
        let endpoint = self.endpoint(&format!(
            "2.0/repositories/{}/{}/commit/{}/pullrequests",
            owner, repo, commit
        ))?;

        // Find all pull requests associated with the given commit SHA.
        trace!("find pull requests associated with commit `{commit}`");
        let prs = client
            .get(endpoint)
            .header("Accept", "application/json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| {
                format!("could not list associated pull requests via Bitbucket Cloud for commit `{commit}`")
            })?
            .json::<Response>()
            .await
            .with_context(|| {
                format!("could not parse associated pull requests from Bitbucket Cloud for commit `{commit}`")
            })?
            .values
            .into_iter()
            .map(|pr| PullRequest {
                number: pr.id,
                url: pr.links.html.href,
                title: pr.title,
                body: pr.summary.raw,
                head: pr.source.branch.name,
                base: pr.destination.branch.name,
                existing: true,
                packages: vec![],
            })
            .collect::<Vec<PullRequest>>();

        trace!("found {} associated pull requests for commit `{commit}`", prs.len());
        Ok(prs)
    }

    /// Finds pull requests associated with a given commit SHA on Bitbucket Data Center.
    ///
    /// See: [Bitbucket Server API Documentation](https://developer.atlassian.com/server/bitbucket/rest/v1005/api-group-pull-requests/#api-api-latest-projects-projectkey-repos-repositoryslug-commits-commitid-pull-requests-get)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the repository (user or organisation).
    /// * `repo` - The name of the repository.
    /// * `commit` - The commit SHA to search for associated pull requests.
    ///
    /// # Returns
    ///
    /// A result containing a list of pull requests associated with the given commit SHA, if any.
    pub async fn associated_pull_requests_via_data_center(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
    ) -> anyhow::Result<Vec<PullRequest>> {
        #[derive(serde::Deserialize)]
        struct Response {
            values: Vec<DataCenterPullRequest>,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct DataCenterPullRequest {
            id: u64,
            title: String,
            description: Option<String>,
            links: Links,
            from_ref: BranchReference,
            to_ref: BranchReference,
        }

        #[derive(serde::Deserialize)]
        struct Links {
            #[serde(rename = "self")]
            self_links: Vec<Link>,
        }

        #[derive(serde::Deserialize)]
        struct Link {
            href: String,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BranchReference {
            display_id: String,
        }

        let client = self.client()?;
        let endpoint = self.endpoint(&format!(
            "rest/api/latest/projects/{}/repos/{}/commits/{}/pull-requests",
            owner, repo, commit
        ))?;

        // Find all pull requests associated with the given commit SHA.
        trace!("find pull requests associated with commit `{commit}`");
        let prs = client
            .get(endpoint)
            .header("Accept", "application/json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| {
                format!("could not list associated pull requests via Bitbucket Data Center for commit `{commit}`")
            })?
            .json::<Response>()
            .await
            .with_context(|| {
                format!("could not parse associated pull requests from Bitbucket Data Center for commit `{commit}`")
            })?
            .values
            .into_iter()
            .map(|pr| {
                let url = pr
                    .links
                    .self_links
                    .into_iter()
                    .next()
                    .with_context(|| format!("missing URL for Bitbucket Data Center pull request #{}", pr.id))?
                    .href;
                Ok(PullRequest {
                    number: pr.id,
                    url,
                    title: pr.title,
                    body: pr.description,
                    head: pr.from_ref.display_id,
                    base: pr.to_ref.display_id,
                    existing: true,
                    packages: vec![],
                })
            })
            .collect::<anyhow::Result<Vec<PullRequest>>>()?;

        trace!("found {} associated pull requests for commit `{commit}`", prs.len());
        Ok(prs)
    }

    /// Creates or updates a pull request on Bitbucket Cloud.
    ///
    /// See: [Bitbucket Cloud API Documentation](https://developer.atlassian.com/cloud/bitbucket/rest/api-group-pullrequests/#api-repositories-workspace-repo-slug-pullrequests-post)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the repository (user or organisation).
    /// * `repo` - The name of the repository.
    /// * `head` - The name of the branch where your changes are implemented.
    /// * `base` - The name of the branch you want the changes pulled into.
    /// * `title` - The title of the pull request.
    /// * `body` - The body content of the pull request.
    ///
    /// # Returns
    ///
    /// A result containing the pull request number, URL, and whether it was created or updated.
    pub async fn upsert_pull_request_via_cloud(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<PullRequest> {
        #[derive(serde::Deserialize)]
        struct Response {
            values: Vec<CloudPullRequest>,
        }

        #[derive(serde::Deserialize)]
        struct CloudPullRequest {
            id: u64,
            title: String,
            summary: Summary,
            links: Links,
        }

        #[derive(serde::Deserialize)]
        struct Summary {
            raw: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct Links {
            html: Link,
        }

        #[derive(serde::Deserialize)]
        struct Link {
            href: String,
        }

        let client = self.client()?;
        let endpoint = self.endpoint(&format!("2.0/repositories/{}/{}/pullrequests", owner, repo))?;

        // Find an open pull request with the same head and base branches.
        trace!("find open pull requests with head: {owner}:{head}; base: {base}");
        let prs = client
            .get(endpoint.clone())
            .query(&[
                ("state", "OPEN"),
                (
                    "q",
                    format!(
                        "source.repository.full_name = {} AND source.branch.name = {} AND destination.branch.name = {}",
                        json!(format!("{owner}/{repo}")),
                        json!(head),
                        json!(base),
                    )
                    .as_str(),
                ),
                ("pagelen", "1"),
            ])
            .header("Accept", "application/json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("could not list pull requests via Bitbucket Cloud")?
            .json::<Response>()
            .await
            .context("could not parse pull requests from Bitbucket Cloud")?;

        // If a pull request already exists, update it.
        if let Some(pr) = prs.values.into_iter().next() {
            let updated = if pr.title != title || pr.summary.raw.as_deref() != Some(body) {
                trace!("updating existing pull request #{}", pr.id);
                client
                    .put(self.endpoint(&format!("2.0/repositories/{}/{}/pullrequests/{}", owner, repo, pr.id))?)
                    .header("Accept", "application/json")
                    .json(&json!({
                        "title": title,
                        "description": body,
                    }))
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .context("failed to update pull request via Bitbucket Cloud")?
                    .json::<CloudPullRequest>()
                    .await
                    .context("could not parse updated pull request from Bitbucket Cloud")?
            } else {
                trace!("existing pull request #{} is already up-to-date", pr.id);
                pr
            };
            return Ok(PullRequest {
                number: updated.id,
                url: updated.links.html.href,
                title: updated.title,
                body: updated.summary.raw,
                head: head.to_string(),
                base: base.to_string(),
                existing: true,
                packages: vec![],
            });
        }

        // A pull request does not exist yet, create one.
        trace!("creating new pull request: {} -> {}", head, base);
        let created = client
            .post(endpoint)
            .header("Accept", "application/json")
            .json(&json!({
                "title": title,
                "description": body,
                "source": { "branch": { "name": head } },
                "destination": { "branch": { "name": base } },
            }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("failed to create pull request via Bitbucket Cloud")?
            .json::<CloudPullRequest>()
            .await
            .context("could not parse created pull request from Bitbucket Cloud")?;

        Ok(PullRequest {
            number: created.id,
            url: created.links.html.href,
            title: title.to_string(),
            body: Some(body.to_string()),
            head: head.to_string(),
            base: base.to_string(),
            existing: false,
            packages: vec![],
        })
    }

    /// Creates or updates a pull request on Bitbucket Data Center.
    ///
    /// See: [Bitbucket Server API Documentation](https://developer.atlassian.com/server/bitbucket/rest/v1005/api-group-pull-requests/#api-api-latest-projects-projectkey-repos-repositoryslug-pull-requests-post)
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the repository (user or organisation).
    /// * `repo` - The name of the repository.
    /// * `head` - The name of the branch where your changes are implemented.
    /// * `base` - The name of the branch you want the changes pulled into.
    /// * `title` - The title of the pull request.
    /// * `body` - The body content of the pull request.
    ///
    /// # Returns
    ///
    /// A result containing the pull request number, URL, and whether it was created or updated.
    pub async fn upsert_pull_request_via_data_center(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<PullRequest> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Response {
            values: Vec<DataCenterPullRequest>,
            is_last_page: bool,
            next_page_start: Option<u64>,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct DataCenterPullRequest {
            id: u64,
            version: u64,
            title: String,
            description: Option<String>,
            links: Links,
            from_ref: BranchReference,
        }

        #[derive(serde::Deserialize)]
        struct BranchReference {
            id: String,
            repository: Repository,
        }

        #[derive(serde::Deserialize)]
        struct Repository {
            slug: String,
            project: Project,
        }

        #[derive(serde::Deserialize)]
        struct Project {
            key: String,
        }

        #[derive(serde::Deserialize)]
        struct Links {
            #[serde(rename = "self")]
            self_links: Vec<Link>,
        }

        #[derive(serde::Deserialize)]
        struct Link {
            href: String,
        }

        let client = self.client()?;
        let endpoint = self.endpoint(&format!(
            "rest/api/latest/projects/{}/repos/{}/pull-requests",
            owner, repo
        ))?;

        // Find an open pull request with the same head and base branches.
        trace!("find open pull requests with head: {owner}:{head}; base: {base}");
        let head_ref = format!("refs/heads/{head}");
        let base_ref = format!("refs/heads/{base}");
        let mut start = 0;
        let existing = loop {
            let prs = client
                .get(endpoint.clone())
                .query(&[
                    ("state", "OPEN"),
                    ("direction", "INCOMING"),
                    ("at", base_ref.as_str()),
                    ("limit", "100"),
                    ("start", start.to_string().as_str()),
                ])
                .header("Accept", "application/json")
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .context("could not list pull requests via Bitbucket Data Center")?
                .json::<Response>()
                .await
                .context("could not parse pull requests from Bitbucket Data Center")?;
            if let Some(pr) = prs.values.into_iter().find(|pr| {
                pr.from_ref.id == head_ref
                    && pr.from_ref.repository.slug.eq_ignore_ascii_case(repo)
                    && pr.from_ref.repository.project.key.eq_ignore_ascii_case(owner)
            }) {
                break Some(pr);
            }
            if prs.is_last_page {
                break None;
            }
            let next_start = prs
                .next_page_start
                .context("missing next page start for pull requests from Bitbucket Data Center")?;
            anyhow::ensure!(
                next_start > start,
                "invalid next page start for pull requests from Bitbucket Data Center"
            );
            start = next_start;
        };

        // If a pull request already exists, update it.
        if let Some(pr) = existing {
            let updated = if pr.title != title || pr.description.as_deref() != Some(body) {
                trace!("updating existing pull request #{}", pr.id);
                client
                    .put(self.endpoint(&format!(
                        "rest/api/latest/projects/{}/repos/{}/pull-requests/{}",
                        owner, repo, pr.id
                    ))?)
                    .header("Accept", "application/json")
                    .json(&json!({
                        "version": pr.version,
                        "title": title,
                        "description": body,
                    }))
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .context("failed to update pull request via Bitbucket Data Center")?
                    .json::<DataCenterPullRequest>()
                    .await
                    .context("could not parse updated pull request from Bitbucket Data Center")?
            } else {
                trace!("existing pull request #{} is already up-to-date", pr.id);
                pr
            };
            let url = updated
                .links
                .self_links
                .into_iter()
                .next()
                .with_context(|| format!("missing URL for Bitbucket Data Center pull request #{}", updated.id))?
                .href;
            return Ok(PullRequest {
                number: updated.id,
                url,
                title: updated.title,
                body: updated.description,
                head: head.to_string(),
                base: base.to_string(),
                existing: true,
                packages: vec![],
            });
        }

        // A pull request does not exist yet, create one.
        trace!("creating new pull request: {} -> {}", head, base);
        let created = client
            .post(endpoint)
            .header("Accept", "application/json")
            .json(&json!({
                "title": title,
                "description": body,
                "fromRef": {
                    "id": head_ref,
                    "repository": { "slug": repo, "project": { "key": owner } },
                },
                "toRef": {
                    "id": base_ref,
                    "repository": { "slug": repo, "project": { "key": owner } },
                },
            }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .context("failed to create pull request via Bitbucket Data Center")?
            .json::<DataCenterPullRequest>()
            .await
            .context("could not parse created pull request from Bitbucket Data Center")?;
        let url = created
            .links
            .self_links
            .into_iter()
            .next()
            .with_context(|| format!("missing URL for Bitbucket Data Center pull request #{}", created.id))?
            .href;

        Ok(PullRequest {
            number: created.id,
            url,
            title: title.to_string(),
            body: Some(body.to_string()),
            head: head.to_string(),
            base: base.to_string(),
            existing: false,
            packages: vec![],
        })
    }
}

#[async_trait]
impl BaseGitProvider for BitbucketProvider {
    async fn associated_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
    ) -> anyhow::Result<Vec<PullRequest>> {
        if self.host == "api.bitbucket.org" {
            self.associated_pull_requests_via_cloud(owner, repo, commit).await
        } else {
            self.associated_pull_requests_via_data_center(owner, repo, commit).await
        }
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
        if self.host == "api.bitbucket.org" {
            self.upsert_pull_request_via_cloud(owner, repo, head, base, title, body)
                .await
        } else {
            self.upsert_pull_request_via_data_center(owner, repo, head, base, title, body)
                .await
        }
    }

    async fn upsert_release(
        &self,
        owner: &str,
        repo: &str,
        _tag_name: &str,
        _commit_sha: &str,
        _title: &str,
        _body: &str,
        _make_latest: bool,
    ) -> anyhow::Result<String> {
        trace!("skip release creation via Bitbucket for `{owner}/{repo}` - not supported");
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
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
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
                    "target": { "hash": commit_sha },
                }));
            then.status(201).json_body(json!({
                "message": "v1.0.0",
                "name": "v1.0.0",
                "target": { "hash": commit_sha },
                "type": "TAG",
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider.tag("axieum", "Hello-World", "v1.0.0", commit_sha).await;

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
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let get_tag_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/refs/tags/v1.0.0")
                .header("Accept", "application/json");
            then.status(200);
        });
        let get_commit_mock = server.mock(|when, then| {
            when.method(GET)
                .path(&format!("/2.0/repositories/axieum/Hello-World/commit/{commit_sha}"))
                .header("Accept", "application/json");
            then.status(200).json_body(json!({
                "hash": commit_sha,
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
                    "target": { "hash": commit_sha },
                }));
            then.status(201).json_body(json!({
                "message": "v1.0.0",
                "name": "v1.0.0",
                "target": { "hash": commit_sha },
                "type": "TAG",
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider.tag("axieum", "Hello-World", "v1.0.0", commit_sha).await;

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
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let tag_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/git/latest/projects/axieum/repos/Hello-World/tags")
                .header("Accept", "application/json;charset=UTF-8")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "force": true,
                    "message": "v1.0.0",
                    "name": "v1.0.0",
                    "startPoint": commit_sha,
                    "type": "ANNOTATED",
                }));
            then.status(201).json_body(json!({
                "id": "refs/tags/v1.0.0",
                "displayId": "v1.0.0",
                "latestCommit": commit_sha,
                "type": "TAG",
            }));
        });

        // Create a tag against the given commit SHA.
        let result = provider.tag("axieum", "Hello-World", "v1.0.0", commit_sha).await;

        // Ensure the annotated tag was created successfully.
        tag_mock.assert_async().await;
        result.unwrap();
    }

    /// Tests that associated pull requests are returned from Bitbucket Cloud for a given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_associated_pull_requests_via_cloud(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path(&format!(
                    "/2.0/repositories/axieum/Hello-World/commit/{commit_sha}/pullrequests"
                ))
                .header("Accept", "application/json");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/bitbucket_cloud_commit_pulls.json"));
        });

        // Find associated pull requests for a commit SHA.
        let prs = provider
            .associated_pull_requests("axieum", "Hello-World", commit_sha)
            .await;

        // Ensure the associated pull requests were retrieved successfully.
        commit_pulls_mock.assert_async().await;
        assert_eq!(
            prs.unwrap(),
            vec![PullRequest {
                number: 1347,
                url: "https://bitbucket.org/axieum/Hello-World/pull-requests/1347".to_string(),
                title: "Amazing new feature".to_string(),
                body: Some("Please pull these awesome changes in!".to_string()),
                head: "new-topic".to_string(),
                base: "master".to_string(),
                existing: true,
                packages: vec![],
            }],
        );
    }

    /// Tests that no associated pull requests are returned from Bitbucket Cloud for a given commit SHA when none exist.
    #[rstest]
    #[tokio::test]
    async fn test_associated_pull_requests_via_cloud_when_none_exist(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path(&format!(
                    "/2.0/repositories/axieum/Hello-World/commit/{commit_sha}/pullrequests"
                ))
                .header("Accept", "application/json");
            then.status(200).json_body(json!({ "values": [] }));
        });

        // Find associated pull requests for a commit SHA.
        let prs = provider
            .associated_pull_requests("axieum", "Hello-World", commit_sha)
            .await;

        // Ensure the associated pull requests were retrieved successfully.
        commit_pulls_mock.assert_async().await;
        assert!(prs.unwrap().is_empty());
    }

    /// Tests that associated pull requests are returned from Bitbucket Data Center for a given commit SHA.
    #[rstest]
    #[tokio::test]
    async fn test_associated_pull_requests_via_data_center(#[with("bb.example.com")] provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Data Center API.
        let server = mock_reqwest(&provider).await;
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path(&format!(
                    "/rest/api/latest/projects/axieum/repos/Hello-World/commits/{commit_sha}/pull-requests"
                ))
                .header("Accept", "application/json");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../tests/fixtures/bitbucket_data_center_commit_pulls.json"
                ));
        });

        // Find associated pull requests for a commit SHA.
        let prs = provider
            .associated_pull_requests("axieum", "Hello-World", commit_sha)
            .await;

        // Ensure the associated pull requests were retrieved successfully.
        commit_pulls_mock.assert_async().await;
        assert_eq!(
            prs.unwrap(),
            vec![PullRequest {
                number: 1347,
                url: "https://bb.example.com/projects/axieum/repos/Hello-World/pull-requests/1347".to_string(),
                title: "Amazing new feature".to_string(),
                body: Some("Please pull these awesome changes in!".to_string()),
                head: "new-topic".to_string(),
                base: "master".to_string(),
                existing: true,
                packages: vec![],
            }],
        );
    }

    /// Tests that no associated pull requests are returned from Bitbucket Data Center for a given commit SHA when none exist.
    #[rstest]
    #[tokio::test]
    async fn test_associated_pull_requests_via_data_center_when_none_exist(
        #[with("bb.example.com")] provider: BitbucketProvider,
    ) {
        // Set up a mock server to simulate the Bitbucket Data Center API.
        let server = mock_reqwest(&provider).await;
        let commit_sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let commit_pulls_mock = server.mock(|when, then| {
            when.method(GET)
                .path(&format!(
                    "/rest/api/latest/projects/axieum/repos/Hello-World/commits/{commit_sha}/pull-requests"
                ))
                .header("Accept", "application/json");
            then.status(200).json_body(json!({ "values": [] }));
        });

        // Find associated pull requests for a commit SHA.
        let prs = provider
            .associated_pull_requests("axieum", "Hello-World", commit_sha)
            .await;

        // Ensure the associated pull requests were retrieved successfully.
        commit_pulls_mock.assert_async().await;
        assert!(prs.unwrap().is_empty());
    }

    /// Tests that a new pull request is created on Bitbucket Cloud when one does not exist yet.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_via_cloud_creates_a_new_pr(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .header("Accept", "application/json")
                .query_param("state", "OPEN")
                .query_param(
                    "q",
                    concat!(
                        r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                        r#"source.branch.name = "reflow--branches--main" AND destination.branch.name = "main""#,
                    ),
                )
                .query_param("pagelen", "1");
            then.status(200).json_body(json!({ "values": [] }));
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "title": "chore: release v1.0.0",
                    "description": "...",
                    "source": { "branch": { "name": "reflow--branches--main" } },
                    "destination": { "branch": { "name": "main" } },
                }));
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/bitbucket_cloud_pulls_retrieve.json"));
        });

        // Create a new pull request.
        let pr = provider
            .upsert_pull_request(
                "axieum",
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
                url: "https://bitbucket.org/axieum/Hello-World/pull-requests/1347".to_string(),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: false,
                packages: vec![],
            },
        );
    }

    /// Tests that an existing pull request is updated on Bitbucket Cloud when one already exists.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_via_cloud_updates_an_existing_pr(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .header("Accept", "application/json")
                .query_param("state", "OPEN")
                .query_param(
                    "q",
                    concat!(
                        r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                        r#"source.branch.name = "reflow--branches--main" AND destination.branch.name = "main""#,
                    ),
                )
                .query_param("pagelen", "1");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/bitbucket_cloud_pulls_list.json"));
        });
        let update_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests/1347")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "title": "chore: release v1.0.0",
                    "description": "...",
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/bitbucket_cloud_pulls_retrieve.json"));
        });

        // Update an existing pull request.
        let pr = provider
            .upsert_pull_request(
                "axieum",
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
                url: "https://bitbucket.org/axieum/Hello-World/pull-requests/1347".to_string(),
                title: "chore: release v1.0.0".to_string(),
                body: Some("...".to_string()),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: true,
                packages: vec![],
            },
        );
    }

    /// Tests that an existing pull request is not updated on Bitbucket Cloud when it is already up-to-date.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_via_cloud_does_not_update_if_already_up_to_date(provider: BitbucketProvider) {
        // Set up a mock server to simulate the Bitbucket Cloud API.
        let server = mock_reqwest(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests")
                .header("Accept", "application/json")
                .query_param("state", "OPEN")
                .query_param(
                    "q",
                    concat!(
                        r#"source.repository.full_name = "axieum/Hello-World" AND "#,
                        r#"source.branch.name = "reflow--branches--main" AND destination.branch.name = "main""#,
                    ),
                )
                .query_param("pagelen", "1");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!("../../tests/fixtures/bitbucket_cloud_pulls_list.json"));
        });
        let update_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/2.0/repositories/axieum/Hello-World/pullrequests/1347");
            then.status(200);
        });

        // Update an existing pull request.
        let pr = provider
            .upsert_pull_request(
                "axieum",
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
        update_mock.assert_calls_async(0).await;
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                number: 1347,
                url: "https://bitbucket.org/axieum/Hello-World/pull-requests/1347".to_string(),
                title: "Amazing new feature".to_string(),
                body: Some("Please pull these awesome changes in!".to_string()),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: true,
                packages: vec![],
            },
        );
    }

    /// Tests that a new pull request is created on Bitbucket Data Center when no matching pull request exists.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_via_data_center_creates_a_new_pr(
        #[with("bb.example.com")] provider: BitbucketProvider,
    ) {
        // Set up a mock server to simulate the Bitbucket Data Center API.
        let server = mock_reqwest(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .header("Accept", "application/json")
                .query_param("state", "OPEN")
                .query_param("direction", "INCOMING")
                .query_param("at", "refs/heads/main")
                .query_param("limit", "100")
                .query_param("start", "0");
            then.status(200).json_body(json!({ "values": [], "isLastPage": true }));
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "title": "chore: release v1.0.0",
                    "description": "...",
                    "fromRef": {
                        "id": "refs/heads/reflow--branches--main",
                        "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                    },
                    "toRef": {
                        "id": "refs/heads/main",
                        "repository": { "slug": "Hello-World", "project": { "key": "axieum" } },
                    },
                }));
            then.status(201)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
                ));
        });

        // Create a new pull request.
        let pr = provider
            .upsert_pull_request(
                "axieum",
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
                url: "https://bb.example.com/projects/axieum/repos/Hello-World/pull-requests/1347".to_string(),
                title: "chore: release v1.0.0".to_string(),
                body: Some("...".to_string()),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: false,
                packages: vec![],
            },
        );
    }

    /// Tests that an existing pull request is updated on Bitbucket Data Center when one already exists.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_via_data_center_updates_an_existing_pr(
        #[with("bb.example.com")] provider: BitbucketProvider,
    ) {
        // Set up a mock server to simulate the Bitbucket Data Center API.
        let server = mock_reqwest(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .header("Accept", "application/json")
                .query_param("state", "OPEN")
                .query_param("direction", "INCOMING")
                .query_param("at", "refs/heads/main")
                .query_param("limit", "100")
                .query_param("start", "0");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../tests/fixtures/bitbucket_data_center_pulls_list.json"
                ));
        });
        let update_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests/1347")
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "version": 7,
                    "title": "chore: release v1.0.0",
                    "description": "...",
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../tests/fixtures/bitbucket_data_center_pulls_retrieve.json"
                ));
        });

        // Update an existing pull request.
        let pr = provider
            .upsert_pull_request(
                "axieum",
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
                url: "https://bb.example.com/projects/axieum/repos/Hello-World/pull-requests/1347".to_string(),
                title: "chore: release v1.0.0".to_string(),
                body: Some("...".to_string()),
                head: "reflow--branches--main".to_string(),
                base: "main".to_string(),
                existing: true,
                packages: vec![],
            },
        );
    }

    /// Tests that an existing pull request is not updated on Bitbucket Data Center when it is already up-to-date.
    #[rstest]
    #[tokio::test]
    async fn test_upsert_pull_request_via_data_center_does_not_update_if_already_up_to_date(
        #[with("bb.example.com")] provider: BitbucketProvider,
    ) {
        // Set up a mock server to simulate the Bitbucket Data Center API.
        let server = mock_reqwest(&provider).await;
        let list_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests")
                .header("Accept", "application/json")
                .query_param("state", "OPEN")
                .query_param("direction", "INCOMING")
                .query_param("at", "refs/heads/main")
                .query_param("limit", "100")
                .query_param("start", "0");
            then.status(200)
                .header("content-type", "application/json")
                .body(include_str!(
                    "../../tests/fixtures/bitbucket_data_center_pulls_list.json"
                ));
        });
        let update_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/rest/api/latest/projects/axieum/repos/Hello-World/pull-requests/1347");
            then.status(200);
        });

        // Update an existing pull request.
        let pr = provider
            .upsert_pull_request(
                "axieum",
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
        update_mock.assert_calls_async(0).await;
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                number: 1347,
                url: "https://bb.example.com/projects/axieum/repos/Hello-World/pull-requests/1347".to_string(),
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
