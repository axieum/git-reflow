# GitHub Action

The `axieum/git-reflow` action runs [`release`](./06-release.md) and then [`pr`](./05-pr.md) by default. On each push
to your release branch, it publishes any release associated with that commit, then prepares the next release pull
request.

## Usage

```yaml
name: Release
on:
  push:
    branches: [ main ]

permissions:
  contents: write
  pull-requests: write

jobs:
  # Create Git tags for any recently merged release PRs, and/or prepare the next release PR.
  release:
    name: Release

    runs-on: ubuntu-latest

    steps:
      - name: ✨ Checkout repository
        uses: actions/checkout@v7
        with:
          fetch-depth: 0

      - id: reflow
        name: 🙌 Prepare release
        uses: axieum/git-reflow@v1

    outputs:
      release: ${{ steps.reflow.outputs.release }}

  # If there were any releases, publish them.
  # NB: Swap this out for your own publishing stage.
  publish:
    name: Publish

    needs: release
    runs-on: ubuntu-latest
    if: ${{ needs.release.outputs.release != '{}' }}

    steps:
      - name: ✨ Checkout repository
        uses: actions/checkout@v7

      - name: 🚀 Publish release
        run: uv publish
```

> [!IMPORTANT]
> When using the default token, enable "[Allow GitHub Actions to create and approve pull requests][actions-pr-settings]"
> in your repository's Actions settings.

## Inputs

| Input     | Description                                                            |
|-----------|------------------------------------------------------------------------|
| `command` | The `git-reflow` command to run. Omit to run both `release` then `pr`. |
| `token`   | GitHub API token. Defaults to `${{ github.token }}`.                   |
| `config`  | Optional configuration file path relative to the workspace.            |

## Outputs

| Output     | Description                                              |
|------------|----------------------------------------------------------|
| `prs`      | JSON array of pull requests created or updated, or `[]`. |
| `pr`       | The first entry in `prs`, or `{}`.                       |
| `releases` | JSON array of released packages, or `[]`.                |
| `release`  | The first entry in `releases`, or `{}`.                  |

> [!TIP]
> Use `fromJSON()` to read an output in a workflow expression, for example
> `${{ fromJSON(steps.reflow.outputs.releases) }}`.

[actions-pr-settings]: https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/managing-github-actions-settings-for-a-repository#preventing-github-actions-from-creating-or-approving-pull-requests
