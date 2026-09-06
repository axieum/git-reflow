use crate::common::{INSTA_STDOUT_FILTERS, project_repo};
use assert_cmd::Command;
use rstest::*;

mod common;

/// Tests that the `config` command produces the expected output for various project fixtures.
#[rstest]
#[case::example_python_poetry("example-python-poetry")]
#[case::example_python_setup_py("example-python-setup-py")]
#[case::example_python_uv("example-python-uv")]
#[case::example_python_uv_workspace("example-python-uv-workspace")]
#[case::example_rust("example-rust")]
#[case::example_rust_workspace("example-rust-workspace")]
fn test_config_command(#[case] fixture: &str) {
    let (project_dir, _repo) = project_repo(fixture);

    let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap();
    let assert = cmd
        .current_dir(&project_dir)
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
