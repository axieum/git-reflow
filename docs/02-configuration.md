# Configuration

This chapter describes how to configure `git-reflow` for your project.

Let's start by printing the current configuration for your project:

```shell
git reflow config
```

If the default configuration does not suit your needs, you can create a configuration file in your repository:

1. `.reflow.json`
2. `.reflow.json5`
3. `.reflow.jsonc`
4. `.reflow.toml`
5. `.reflow.yaml`
6. `.reflow.yml`

## Defaults

The default configuration (ex. `.reflow.toml`) is as follows:

```toml
# A list of path globs to exclude from package discovery.
exclude = []


# The Git configuration.
[git]
# The release branch name prefix.
release-branch-prefix = "reflow--branches--"
# The pattern used when committing release changes.
# The following placeholders are available:
# - `{{ version }}`: The new version being released (excluding 'v' prefix).
# - `{{ package }}`: The name of the package being released.
# - `{{ scope }}`: The conventional commit scope of the package being released (see `packages[].scope`).
# - `{{ branch }}`: The target branch name.
commit-message-pattern = "chore{{#if scope}}({{ scope }}){{/if}}: release v{{ version }}"
# If `true`, create separate pull requests for each package.
separate-pull-requests = false
# If `true`, create additional tags for both major (e.g. v1) and minor (e.g. v1.2) _stable_ versions.
tag-major-and-minor = true


# The GitHub Git provider.
# NB: Only one `git.provider.*` table can be configured at one time.
[git.provider.github]
# The GitHub host, or a full API URL for a custom endpoint.
# NB: It prefers the `GITHUB_API_URL` environment variable if set.
host = "api.github.com"
# Whether to commit changes and create release tags via the GitHub API instead of using the local Git client.
commit-via-api = true


# The Bitbucket Git provider.
# NB: Only one `git.provider.*` table can be configured at one time.
[git.provider.bitbucket]
# The Bitbucket host, or a full API URL for a custom endpoint.
# NB: It prefers the `BITBUCKET_URL` environment variable if set.
host = "api.bitbucket.org"
# Whether to create release tags via the Bitbucket API instead of using the local Git client.
tag_via_api = true


# A list of package configurations.
[[packages]]
# The directory of the package root.
dir = "."
# The name of the package.
name = "<auto detected>"
# If `true`, detect nested packages if any.
workspace = true
# If `true`, prefix the git tag with the package name, e.g. `git-reflow-api-v1.0.0`.
# NB: It will default to `false` for the first package else `true`.
include-name-in-tag = false
# The changelog path relative to the package directory.
# NB: If `null`, the changelog will be disabled for this package.
changelog-path = "CHANGELOG.md"
# A list of additional glob patterns to include in the release changes, e.g. `["crates/git-reflow-api"]`.
# NB: This affects the semantic version bump as well as the release notes.
include-changes = []
# A list of additional files to update.
update-files = []
# If `true`, only create a git tag for this package without creating a release.
tag-only = false
# If `true`, mark this release as the latest on the Git provider.
# NB: It will default to `true` for the first package else `false`.
make-latest = true


# NB: Add any additional packages by adding another `[[packages]]` table entry.
```
