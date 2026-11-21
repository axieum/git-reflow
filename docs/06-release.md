# `release`

The `git reflow release` command tags the packages from a merged [release pull request](./05-pr.md) and creates
releases on the configured Git provider where supported. It uses the checked-out commit (`HEAD`) to find the pull
request.

> [!WARNING]
> Make sure `HEAD` is associated with a release pull request. The command does not search earlier commits.

## Usage

After merging the release pull request, make sure `HEAD` is the commit that landed on the target branch when the
pull request was merged. Then run:

```shell
git reflow release
```

The command requires an `origin` remote and access to the [configured Git provider](./02-configuration.md).
For a live run, make sure the credentials also allow creating tags and, where supported, releases.

To preview which packages would be released without creating tags or provider releases:

```shell
git reflow release --dry-run
```

A dry run still queries the provider for the pull request and checks which version tags already exist on `origin`.

## How it works

1. **Find the release pull request.** Ask the Git provider for pull requests associated with `HEAD` and select the
   first whose source branch starts with `git.release-branch-prefix`.
2. **Read the release metadata.** Use the package names, versions, and changelogs recorded in the pull request
   body by `git reflow pr`. Keep this generated frontmatter metadata intact.
3. **Tag the checked-out commit.** Skip any package whose full version tag already exists on `origin`. Otherwise,
   create a tag such as `v1.2.3` or `example-api-v1.2.3`, depending on `include-name-in-tag`. With
   `git.tag-major-and-minor` enabled (the default), also create or move the major and minor tags, such as `v1`
   and `v1.2`. Pre-releases get only the full version tag.
4. **Create the provider release.** Use the changelog from the pull request's description as the release notes. Packages configured
   with `tag-only` skip this step.
   provider release.

## Output

The output is a JSON array with one entry per released package, including tag-only packages. The output is `[]`
when no release pull request matches `HEAD` or every package already has its full version tag on `origin`.

| Field          | Description                                                       |
|----------------|-------------------------------------------------------------------|
| `package-name` | The configured package name.                                      |
| `version`      | The full semantic version.                                        |
| `major`        | The major version number.                                         |
| `minor`        | The minor version number.                                         |
| `patch`        | The patch version number.                                         |
| `pre`          | The pre-release identifier, or `null` if absent.                  |
| `build`        | The build metadata, or `null` if absent.                           |
| `tag-name`     | The full version tag name.                                        |
| `url`          | The provider release URL, or `null` if there is no provider release. |
| `changelog-md` | The rendered Markdown changelog from the pull request.            |

A dry run returns the same fields for the packages that would be released. It includes the planned `tag-name`
and always sets `url` to `null`.
