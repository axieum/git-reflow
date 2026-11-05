use assert_cmd::Command;
use assert_fs::TempDir;
use git_reflow_fixtures::{INSTA_STDOUT_FILTERS, project_repo};
use git2::Repository;
use rstest::*;

/// Tests that the `config` command produces the expected output for various project fixtures.
#[rstest]
#[case::example_python("example-python")]
#[case::example_python_workspace("example-python-workspace")]
#[case::example_rust("example-rust")]
#[case::example_rust_workspace("example-rust-workspace")]
fn test_config_command(#[case] fixture: &str) {
    let (project_dir, _repo) = project_repo(fixture);

    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env_remove("GITHUB_API_URL")
        .arg("config")
        .assert()
        .success();

    insta::with_settings!({ filters => INSTA_STDOUT_FILTERS }, {
        insta::assert_snapshot!(
            format!("config_command__{fixture}"),
            String::from_utf8_lossy(&assert.get_output().stdout),
        );
    });
}

/// Tests that the GitHub provider is preferred when GitHub environment variables are present.
#[rstest]
fn test_config_command_with_github_env_vars(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    let (project_dir, _repo) = project_repo;

    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env("GITHUB_API_URL", "https://gh.example.com")
        .env("GITHUB_TOKEN", "test-token")
        .arg("config")
        .assert()
        .success();

    let result: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(result["git"]["provider"]["github"]["host"], "https://gh.example.com");
}

/// Tests that the Bitbucket provider is preferred when Bitbucket environment variables are present.
#[rstest]
fn test_config_command_with_bitbucket_env_vars(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    let (project_dir, _repo) = project_repo;

    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(&project_dir)
        .env_remove("GITHUB_API_URL")
        .env_remove("GITHUB_TOKEN")
        .env("BITBUCKET_TOKEN", "test-token")
        .env("BITBUCKET_URL", "https://bb.example.com")
        .arg("config")
        .assert()
        .success();

    let result: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(result["git"]["provider"]["bitbucket"]["host"], "https://bb.example.com");
}
