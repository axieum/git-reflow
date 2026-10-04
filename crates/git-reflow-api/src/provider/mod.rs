use crate::plan::PackageRelease;
use crate::provider::github::GitHubProvider;
use anyhow::{Context, bail, ensure};
use async_trait::async_trait;
use semver::Version;
use std::fmt;
use std::fmt::Display;
use std::str::FromStr;
use tracing::trace;

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
    /// The title of the pull request.
    pub title: String,
    /// The body content of the pull request, if any.
    pub body: Option<String>,
    /// The name of the branch where the changes are implemented, e.g. `reflow--branches--main`.
    pub head: String,
    /// The name of the branch the changes are pulled into, e.g. `main`.
    pub base: String,
    /// Whether the pull request was updated (true) or created (false).
    pub existing: bool,
    /// A list of packages included in the pull request.
    pub packages: Vec<PullRequestPackage>,
}

/// A package release included in a pull request.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PullRequestPackage {
    /// The name of the package.
    pub name: String,
    /// The next (target) version of the package.
    pub version: Version,
    /// The rendered changelog Markdown for the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_md: Option<String>,
}

impl From<&PackageRelease> for PullRequestPackage {
    fn from(release: &PackageRelease) -> Self {
        Self {
            name: release.package_name.clone(),
            version: release.next_version.clone(),
            changelog_md: Some(release.changelog_md.clone()),
        }
    }
}

/// The frontmatter of a pull request body, which contains a list of package names and their versions.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PullRequestFrontmatter {
    /// A list of packages included in the pull request.
    pub packages: Vec<PullRequestPackage>,
}

#[async_trait]
pub trait BaseGitProvider {
    /// Finds pull requests associated with a given commit SHA on the Git provider.
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the repository (user or organization).
    /// * `repo` - The name of the repository.
    /// * `commit` - The commit SHA to search for associated pull requests.
    ///
    /// # Returns
    ///
    /// A result containing a list of pull requests associated with the given commit SHA, if any.
    async fn associated_pull_requests(&self, owner: &str, repo: &str, commit: &str)
    -> anyhow::Result<Vec<PullRequest>>;

    /// Creates or updates a pull request on the Git provider.
    ///
    /// # Arguments
    ///
    /// * `owner` - The owner of the repository (user or organization).
    /// * `repo` - The name of the repository.
    /// * `head` - The name of the branch where your changes are implemented.
    /// * `base` - The name of the branch you want the changes pulled into.
    /// * `title` - The title of the pull request.
    /// * `body` - The body content of the pull request.
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
    async fn associated_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        commit: &str,
    ) -> anyhow::Result<Vec<PullRequest>> {
        match self {
            GitProvider::GitHub(provider) => provider.associated_pull_requests(owner, repo, commit).await,
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

/// Builds a pull request body from a list of package names, versions and their changelog Markdown.
///
/// For a single package, the body will contain the changelog Markdown for that package, e.g.
///
/// ```markdown
/// ---
/// packages:
/// - name: example-rust-workspace
///   version: 1.2.0
/// ---
///
/// ...
/// ```
///
/// For multiple packages, the body will contain each package in a `<details>` HTML tag with the package
/// name as the `id` attribute, e.g.
///
/// ```markdown
/// ---
/// packages:
/// - name: example-rust-workspace
///   version: 1.2.0
/// - name: example-api
///   version: 1.1.0-rc.6
/// ---
///
/// <details id="example-rust-workspace">
/// <summary>example-rust-workspace: v1.2.0</summary>
///
/// ...
/// </details>
///
/// <details id="example-api">
/// <summary>example-api: v1.1.0-rc.6</summary>
///
/// ...
/// </details>
/// ```
///
/// # Arguments
///
/// * `packages` - A list of packages to include in the pull request body.
///
/// # Returns
///
/// A result containing the pull request body as a Markdown string.
///
/// # Errors
///
/// Returns an error if no packages are provided or the frontmatter cannot be serialised.
pub fn build_pull_request_body(
    packages: impl IntoIterator<Item = impl Into<PullRequestPackage>>,
) -> anyhow::Result<String> {
    let mut frontmatter = PullRequestFrontmatter {
        packages: packages.into_iter().map(Into::into).collect(),
    };
    ensure!(
        !frontmatter.packages.is_empty(),
        "pull request body is empty as it does not contain any packages"
    );
    let body = if frontmatter.packages.len() == 1 {
        // Use the changelog Markdown for the single release as the pull request body.
        format!("{}\n", frontmatter.packages[0].changelog_md.take().unwrap_or_default())
    } else {
        // Use a summary of all changelog Markdowns for the multiple releases as the pull request body.
        frontmatter
            .packages
            .iter_mut()
            .map(|release| {
                format!(
                    "<details id=\"{}\">\n<summary>{}: v{}</summary>\n\n{}</details>\n",
                    release.name,
                    release.name,
                    release.version,
                    release.changelog_md.take().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(format!("---\n{}---\n\n{}", serde_yaml::to_string(&frontmatter)?, body))
}

/// Parses the package names, versions and their changelog Markdown from a pull request body.
///
/// It expects a Markdown frontmatter at the top of the body, e.g.
///
/// ```yaml
/// ---
/// packages:
/// - name: example-rust-workspace
///   version: 1.2.0
/// - name: example-api
///   version: 1.1.0-rc.6
/// ---
/// ```
///
/// For a single package, the rest of the body is treated as the changelog content.
///
/// For multiple packages, the changelog content for each package exists in a `<details>` HTML tag with the package
/// name as the `id` attribute, e.g.
///
/// ```html
/// <details id="example-rust-workspace">
/// <summary>example-rust-workspace: v1.2.0</summary>
///
/// ...
/// </details>
/// ```
///
/// # Arguments
///
/// * `body` - The Markdown content of the pull request body, if any.
///
/// # Returns
///
/// A result containing a list of packages with their names, versions, and changelog Markdown.
///
/// # Errors
///
/// If the frontmatter is missing, malformed, or does not contain any packages, an error is returned.
pub fn parse_pull_request_body(body: Option<&String>) -> anyhow::Result<Vec<PullRequestPackage>> {
    match body {
        Some(body) => {
            // Parse the frontmatter to extract the package information.
            trace!("parse pull request body: {body}");
            let body = body.replace("\r\n", "\n");
            let content = body
                .strip_prefix("---\n")
                .context("pull request body does not start with a frontmatter")?;
            let (frontmatter, content) = content
                .split_once("\n---\n\n")
                .context("pull request body does not contain a valid frontmatter")?;

            trace!("parse pull request frontmatter: {frontmatter}");
            let mut frontmatter: PullRequestFrontmatter =
                serde_yaml::from_str(frontmatter).context("invalid pull request body frontmatter")?;

            // Ensure that the frontmatter contains at least one package.
            ensure!(
                !frontmatter.packages.is_empty(),
                "pull request body frontmatter does not contain any packages"
            );

            // If the frontmatter contains only one package, then treat the rest of the body as the changelog.
            if frontmatter.packages.len() == 1 {
                let package = &mut frontmatter.packages[0];
                package.changelog_md = Some(content.to_string());
                return Ok(frontmatter.packages);
            }

            // For multiple packages, extract the changelog for each package from the `details` HTML tags in the body.
            for package in &mut frontmatter.packages {
                if let Some((_, details)) = content.split_once(&format!("<details id=\"{}\">", package.name))
                    && let Some((details, _)) = details.split_once("</details>")
                    && let Some((_, changelog_md)) = details.split_once("</summary>\n\n")
                {
                    package.changelog_md = Some(changelog_md.to_string());
                }
            }
            Ok(frontmatter.packages)
        }
        None => bail!("pull request body is empty and does not contain any packages"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;
    use rstest::rstest;

    /// Tests that a pull request body can be built from a list of a single package.
    #[test]
    fn test_build_pull_request_body() {
        let packages = vec![PullRequestPackage {
            name: "example-rust-workspace".to_string(),
            version: Version::parse("1.2.0").unwrap(),
            changelog_md: Some(
                indoc! {r#"
                    ## [1.2.0] - [DATE]

                    ### 🚀 Features

                    - *(api)* Add a `subtract` function"#}
                .to_string(),
            ),
        }];
        assert_eq!(
            build_pull_request_body(packages).unwrap(),
            indoc! {r#"
                ---
                packages:
                - name: example-rust-workspace
                  version: 1.2.0
                ---

                ## [1.2.0] - [DATE]

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                "#},
        );
    }

    /// Tests that a pull request body can be built from a list of multiple packages.
    #[test]
    fn test_build_pull_request_body_with_multiple() {
        let packages = vec![
            PullRequestPackage {
                name: "example-rust-workspace".to_string(),
                version: Version::parse("1.2.0").unwrap(),
                changelog_md: Some(
                    indoc! {r#"
                        ## [1.2.0] - [DATE]

                        ### 🚀 Features

                        - Add a project-wide feature
                        - *(api)* Add a `subtract` function
                        "#}
                    .to_string(),
                ),
            },
            PullRequestPackage {
                name: "example-api".to_string(),
                version: Version::parse("1.1.0").unwrap(),
                changelog_md: Some(
                    indoc! {r#"
                        ## [1.1.0] - [DATE]

                        ### 🚀 Features

                        - *(api)* Add a `subtract` function
                        "#}
                    .to_string(),
                ),
            },
        ];
        assert_eq!(
            build_pull_request_body(packages).unwrap(),
            indoc! {r#"
                ---
                packages:
                - name: example-rust-workspace
                  version: 1.2.0
                - name: example-api
                  version: 1.1.0
                ---

                <details id="example-rust-workspace">
                <summary>example-rust-workspace: v1.2.0</summary>

                ## [1.2.0] - [DATE]

                ### 🚀 Features

                - Add a project-wide feature
                - *(api)* Add a `subtract` function
                </details>

                <details id="example-api">
                <summary>example-api: v1.1.0</summary>

                ## [1.1.0] - [DATE]

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                </details>
                "#},
        );
    }

    /// Tests that a pull request body cannot be built from an empty list of packages.
    #[test]
    fn test_build_pull_request_body_with_none() {
        let result = build_pull_request_body(Vec::<PullRequestPackage>::new());
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "pull request body is empty as it does not contain any packages",
        );
    }

    /// Tests that a pull request body can be built from a list of planned releases.
    #[test]
    fn test_build_pull_request_body_from_planned_releases() {
        let plan = PackageRelease {
            package_name: "example-api".to_string(),
            current_version: None,
            next_version: Version::parse("1.1.0-rc.6").unwrap(),
            commit_message: "chore: release".to_string(),
            changelog_md: "Changelog".to_string(),
            context: serde_json::Value::Null,
        };
        let package = PullRequestPackage {
            name: "example-api".to_string(),
            version: Version::parse("1.1.0-rc.6").unwrap(),
            changelog_md: Some("Changelog".to_string()),
        };
        assert_eq!(
            build_pull_request_body(vec![&plan]).unwrap(),
            build_pull_request_body(vec![package]).unwrap(),
        );
    }

    /// Tests that a package and its changelog Markdown can be parsed from a pull request body.
    #[rstest]
    #[case("\n")]
    #[case("\r\n")]
    fn test_parse_pull_request_body(#[case] line_ending: &str) {
        assert_eq!(
            parse_pull_request_body(Some(
                &indoc! {r#"
                    ---
                    packages:
                    - name: 'example-rust-workspace'
                      version: '1.1.0'
                    ---

                    ## [1.1.0] - [DATE]

                    ### 🚀 Features

                    - *(api)* Add a `subtract` function
                    "#}
                .replace('\n', line_ending)
            ))
            .unwrap(),
            vec![PullRequestPackage {
                name: "example-rust-workspace".to_string(),
                version: Version::parse("1.1.0").unwrap(),
                changelog_md: Some(
                    indoc! {r#"
                        ## [1.1.0] - [DATE]

                        ### 🚀 Features

                        - *(api)* Add a `subtract` function
                        "#}
                    .to_string()
                ),
            }],
        );
    }

    /// Tests that multiple packages and their changelog Markdown can be parsed from a pull request body.
    #[rstest]
    #[case("\n")]
    #[case("\r\n")]
    fn test_parse_pull_request_body_with_multiple(#[case] line_ending: &str) {
        assert_eq!(
            parse_pull_request_body(Some(
                &indoc! {r#"
                    ---
                    packages:
                    - name: 'example-rust-workspace'
                      version: '1.2.0'
                    - name: 'example-api'
                      version: '1.1.0-rc.6'
                    ---

                    <details id="example-rust-workspace">
                    <summary>example-rust-workspace: v1.2.0</summary>

                    ## [1.2.0] - [DATE]

                    ### 🚀 Features

                    - Add a project-wide feature
                    - *(api)* Add a `subtract` function
                    </details>

                    <details id="example-api">
                    <summary>example-api: v1.1.0-rc.6</summary>

                    ## [1.1.0-rc.6] - [DATE]

                    ### 🚀 Features

                    - *(api)* Add a `subtract` function
                    </details>
                    "#}
                .replace('\n', line_ending)
            ))
            .unwrap(),
            vec![
                PullRequestPackage {
                    name: "example-rust-workspace".to_string(),
                    version: Version::parse("1.2.0").unwrap(),
                    changelog_md: Some(
                        indoc! {r#"
                            ## [1.2.0] - [DATE]

                            ### 🚀 Features

                            - Add a project-wide feature
                            - *(api)* Add a `subtract` function
                            "#}
                        .to_string()
                    ),
                },
                PullRequestPackage {
                    name: "example-api".to_string(),
                    version: Version::parse("1.1.0-rc.6").unwrap(),
                    changelog_md: Some(
                        indoc! {r#"
                            ## [1.1.0-rc.6] - [DATE]

                            ### 🚀 Features

                            - *(api)* Add a `subtract` function
                            "#}
                        .to_string()
                    ),
                },
            ],
        );
    }

    /// Tests that an empty list of packages is returned when the pull request body is `None`.
    #[test]
    fn test_parse_pull_request_body_with_none() {
        let result = parse_pull_request_body(None);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "pull request body is empty and does not contain any packages",
        );
    }

    /// Tests that a missing frontmatter prevents packages from being parsed from a pull request body.
    #[test]
    fn test_parse_pull_request_body_with_missing_frontmatter() {
        let result = parse_pull_request_body(Some(
            &indoc! {r#"
                ## [1.1.0] - [DATE]

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                "#}
            .to_string(),
        ));
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "pull request body does not start with a frontmatter"
        );
    }

    /// Tests that a malformed frontmatter prevents packages from being parsed from a pull request body.
    #[test]
    fn test_parse_pull_request_body_with_malformed_frontmatter() {
        let result = parse_pull_request_body(Some(
            &indoc! {r#"
                ---
                packages:
                - name: 'example-rust-workspace'
                  version: '1.1.0'
                - name: 'example-api'
                ---

                ...
                "#}
            .to_string(),
        ));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "invalid pull request body frontmatter");
    }

    /// Tests that horizontal lines does not interfere with the frontmatter in a pull request body.
    #[test]
    fn test_parse_pull_request_body_with_horizontal_line() {
        let result = parse_pull_request_body(Some(
            &indoc! {r#"
                ---
                packages:
                - name: 'example-rust-workspace'
                  version: '1.1.0'
                - name: 'example-api'
                  version: '1.1.0'
                ---

                <details id="example-rust-workspace">
                <summary>example-rust-workspace: v1.1.0</summary>

                ## [1.1.0] - [DATE]

                --- <!-- horizontal line should not interfere with frontmatter -->

                ### 🚀 Features

                - Add a project-wide feature
                - *(api)* Add a `subtract` function
                </details>

                <details id="example-api">
                <summary>example-api: v1.1.0</summary>

                ## [1.1.0] - [DATE]

                ### 🚀 Features

                - *(api)* Add a `subtract` function
                </details>
                "#}
            .to_string(),
        ));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);
    }
}
