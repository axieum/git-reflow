use crate::provider::GitProvider;

/// The Git provider configuration.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GitConfig {
    /// The type of Git provider.
    ///
    /// **Allowed values:** `bitbucket`, `gitea`, `github`, or `gitlab`
    ///
    /// **Default:** github
    #[serde(default)]
    pub provider: GitProvider,
    /// The release branch name prefix.
    ///
    /// **Default:** reflow--branches--
    #[serde(default = "default_release_branch_prefix")]
    pub release_branch_prefix: String,
    /// The pattern used when committing release changes.
    ///
    /// The following placeholders are available:
    /// - `{{ version }}`: The new version being released (excluding 'v' prefix).
    /// - `{{ package }}`: The name of the package being released.
    /// - `{{ branch }}`: The target branch name.
    ///
    /// **Default:** chore: release v{{ version }}
    #[serde(default = "default_commit_message_pattern")]
    pub commit_message_pattern: String,
    /// If `true`, create separate pull requests for each package.
    ///
    /// **Default**: `false`
    #[serde(default)]
    pub separate_pull_requests: bool,
}

/// Returns the default value for `$.commit_message_pattern`.
fn default_commit_message_pattern() -> String {
    String::from("chore: release v{{ version }}")
}

/// Returns the default value for `$.release_branch_prefix`.
fn default_release_branch_prefix() -> String {
    String::from("reflow--branches--")
}
