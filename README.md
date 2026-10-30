<div align="center">

# git-reflow

`git-reflow` is a tool for automating sematic releases by raising pull-requests when commits enter release branches.

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
  - [Bitbucket] (coming soon)

## Installation

```shell
cargo install git-reflow --locked
```

## Usage

```bash
git reflow --help
```

## Configuring

Print the current configuration for your project:

```bash
git reflow config
```

The following files are attempted in order: `.reflow.json`, `.reflow.json5`, `.reflow.jsonc`,
`.reflow.toml`, `.reflow.yaml`, `.reflow.yml`.

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
