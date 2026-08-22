<div align="center">

# git-reflow

`git-reflow` is a tool for automating sematic releases by raising pull-requests when commits enter release branches.

</div>

## Features

- Multi-package configuration support
- Zero-configuration by detecting package manifest files
- Automatic changelog generation and version bumping via [git-cliff]
- Integration with popular Git hosting platforms:
  - GitHub
  - GitLab
  - Gitea
  - Bitbucket

## Installation

`git-reflow` is available in your package manager of choice:

#### Rust

```shell
cargo install git-reflow
```

#### NodeJS

```shell
pnpm add -g git-reflow
```

#### Python

```shell
uv tool install git-reflow
```

## Usage

```bash
git reflow --help
```

## Licence

`git-reflow` is open-sourced software licenced under the [MIT licence][licence].

[licence]: https://opensource.org/licenses/MIT
