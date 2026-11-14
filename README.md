<div align="center">

![](./.github/banner.png "git-reflow")

_A tool for automating semantic releases by raising pull-requests when commits enter release branches_

</div>

## Features

- Zero-configuration by detecting package manifest files
- Automatic changelog generation and version bumping via [git-cliff]
- Multi-package configuration support
  - 🦀 [Cargo]
  - 🐘 [Gradle] (coming soon)
  - ⚡ [pnpm] (coming soon)
  - 🐍 [uv]
- Integration with popular Git hosting platforms:
  - [GitHub]
  - [GitLab] (coming soon)
  - [Gitea] (coming soon)
  - [Bitbucket]

## Documentation

New to `git-reflow`? Start with the [installation guide](docs/01-installation.md), then explore the following guides:

- [Configuration](docs/02-configuration.md) &mdash; configure packages and release settings.
- [Commands](docs/03-commands.md) &mdash; explore the available commands.
- [Planning releases](docs/04-plan.md) &mdash; preview and review release changes.
- [Release pull requests](docs/05-pr.md) &mdash; create or update release pull requests.
- [Publishing releases](docs/06-release.md) &mdash; tag and release your packages.

# Licence

`git-reflow` is open-sourced software licenced under the [MIT licence][licence].

[bitbucket]: https://bitbucket.org
[cargo]: https://doc.rust-lang.org/cargo
[gitea]: https://gitea.io
[github]: https://github.com
[gitlab]: https://gitlab.com
[git-cliff]: https://github.com/orhun/git-cliff
[gradle]: https://gradle.org/
[licence]: https://opensource.org/licenses/MIT
[pnpm]: https://pnpm.io
[uv]: https://docs.astral.sh/uv
