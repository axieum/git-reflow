## [1.0.0] - 2026-11-10

### 🚀 Features

- [**breaking**] Initial release

## [0.4.0] - 2026-11-08

### 🚀 Features

- *(api)* Add `Release-As` commit footer support for overriding versions (#16)

## [0.3.1] - 2026-11-07

### 🐛 Bug Fixes

- *(api)* Fetch tags created via API into the local repository

## [0.3.0] - 2026-11-07

### 🚀 Features

- Add `update-files` option to packages to allow editing arbitrary files (#12)

## [0.2.0] - 2026-11-05

### 🚀 Features

- Add Bitbucket support (#4)
- Detect the Git provider from environment variables (#6)
- Allow including additional paths for a package (#7)

### 🐛 Bug Fixes

- *(api)* Ensure at most one package has `include-name-in-tag` set to `false`
- *(api)* Normalise changelog paths and prevent escaping the repo root (#8)
- *(api)* Use the first configured package's commit message as pr title (#9)
- *(api)* Default GitHub host to `api.github.com`

## [0.1.1] - 2026-11-02

### 🐛 Bug Fixes

- *(api)* Pass tag prefixes to `git-cliff` when running in a monorepo
- *(api)* Ensure pull request changelogs have consistent EOF

## [0.1.0] - 2026-11-01

### 🚀 Features

- Initial release
