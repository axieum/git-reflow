## [0.1.0] - 2026-11-01

### 🚀 Features

- Add `config` command
- *(settings)* Add release strategy configuration
- *(rust)* Add Rust release strategy
- *(python)* Add Python release strategy
- *(api)* Add Git helpers including a `BranchGuard` for restoring Git state on failure
- *(api)* Add `git-cliff` helpers
- *(api)* Add `git` settings
- Stub GitHub provider and its settings
- *(api)* Add `commit` Git helper
- *(api)* Plan the releases for all in-scope packages
- *(api)* Render the commit message for each planned release
- *(api)* Add Git remote url parser helpers
- *(api)* Allow selecting packages to plan releases for
- *(api)* Upsert pull requests for the GitHub provider
- *(api)* Add dry run support to strategy version writes
- *(api)* Allow disabling the changelog in configuration
- *(git)* Add `ensure_clean_working_directory` helper
- *(git)* Add `create_or_reset_branch` helper
- *(api)* Add `push_branch` git helper
- *(api)* Return a list of changed files when writing versions via strategy
- *(api)* Add `paths_relative_to_repo` git helper
- *(api)* Apply release plan and upsert its pull request(s)
- Include packages in each pull request upserted via `pr` command
- Add frontmatter to pr body, and emit pr title, body and changelogs in `pr` command output
- *(api)* Parse a pull request body to find package releases
- *(api)* Find associated pull requests for GitHub commits
- *(api)* Add git tag helpers
- Add `release` command
- *(api)* Add `make_latest` package option for marking a release as latest
- *(api)* Add `git.tag-major-and-minor` setting for creating extra tags
- *(api)* Add `commit_via_api` option to the GitHub provider
- *(api)* Commit via the GitHub API
- *(api)* Tag via the GitHub API
- *(api)* Find package dependencies via the configured strategies
- *(api)* Allow passing a synthetic commit to `git-cliff`
- Plan dependant package version updates
- Apply dependant package version updates
- *(api)* Add `exclude` setting to omit workspace members

### 🐛 Bug Fixes

- *(git-cliff)* Prepend changelog to file instead of overwriting
- *(api)* Create changelog file if it does not exist when applying `git-cliff` context
- *(api)* Canonicalise paths relative to the git repository
- *(api)* Correct indenting in combined pull request body Markdown
- *(api)* Set `include-name-in-tag` to `false` for the first package
- *(api)* Default the first package to `make-latest` only
- *(api)* Prevent duplicate packages from clobbering the configuration

### 🚜 Refactor

- *(config)* Add package strategy to found packages debug messages
- *(api)* Return both the current and next versions from the `git-cliff` context
- Linting changes
- Cross-platform compatibility
- *(git)* Repurpose `render_branch_name` to build the full branch name
- *(cli)* Update `plan` command to show git-cliff context by default
- *(api)* Downgrade debug logs to trace during GitHub provider operations
- *(api)* Adjust fields on `PullRequest` struct
- *(api)* Adjust `sanitize_branch_name` and add `file://` git remote support
- *(api)* Move debug lines to before statements when writing versions
- *(api)* Stub a pull request when applying a dry run
- *(api)* Improve logs when pushing branches to remote
- *(api)* Spawn `git` to handle auth when pushing branches
- *(api)* Function layout when applying a release plan
- *(api)* Check if remote branch is already up-to-date
- Write changelog from the release plan
- *(api)* Do not create new changelog file just to replace it
- *(api)* Skip commit if the remote is already up-to-date
- Drop support for `setup.py` and Poetry python projects
- *(cli)* Rename crate `git-reflow-cli` to `git-reflow`

### 🎨 Styling

- Increase line length from 100 to 120
- Docstring formatting

### 🧪 Testing

- *(cli)* Add example project fixtures and test the `config` command
- Move project fixtures to new `git-reflow-fixtures` crate
- Allow tests to run on unix
- *(api)* Add `should_push_to_remote` tests during apply release plan
- *(api)* Update test for applying a release plan when already up-to-date
- *(api)* Add tests for `create_releases`

### 💼 Other

- Add metadata to Cargo crate manifests
