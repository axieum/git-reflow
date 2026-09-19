# git-reflow-fixtures

A collection of example projects written in various languages for testing purposes.

You can find the example projects in the [`fixtures`](./fixtures) directory.

### Usage

In your test, use the [`project_repo`](../src/lib.rs) fixture to create a
temporary copy of the project fixture files, e.g.

```rust
use git_reflow_fixtures::project_repo;
use git2::Repository;
use rstest::*;
use tempfile::TempDir;

/// Tests that a Rust project is correctly set up.
#[rstest]
#[tokio::test]
async fn test_rust_project(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;

    // Write your test here.
}
```

> [!NOTE]
> You can write to the files in the project directory as it is a temporary copy.

> [!WARNING]
> The temporary project directory lives for as long as the `project_dir`
> variable exists.
