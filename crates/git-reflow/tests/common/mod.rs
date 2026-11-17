#![allow(dead_code)]

use assert_cmd::{Command, assert::Assert};
use serde_json::Value;
use std::path::Path;

/// Runs `git-reflow plan` with additional arguments and asserts on its JSON array.
#[track_caller]
pub fn assert_reflow_plan(dir: impl AsRef<Path>, args: &[&str], assertions: impl FnOnce(&[Value])) -> Assert {
    assert_reflow_json(dir.as_ref(), "plan", args, assertions)
}

/// Runs `git-reflow pr` with additional arguments and asserts on its JSON array.
#[track_caller]
pub fn assert_reflow_prs(dir: impl AsRef<Path>, args: &[&str], assertions: impl FnOnce(&[Value])) -> Assert {
    assert_reflow_json(dir.as_ref(), "pr", args, assertions)
}

/// Runs `git-reflow release` with additional arguments and asserts on its JSON array.
#[track_caller]
pub fn assert_reflow_releases(dir: impl AsRef<Path>, args: &[&str], assertions: impl FnOnce(&[Value])) -> Assert {
    assert_reflow_json(dir.as_ref(), "release", args, assertions)
}

/// Runs `git-reflow` successfully with mocked tokens, then parses and checks its JSON output.
#[track_caller]
fn assert_reflow_json(dir: &Path, command: &str, args: &[&str], assertions: impl FnOnce(&[Value])) -> Assert {
    let assert = Command::cargo_bin("git-reflow")
        .unwrap()
        .current_dir(dir)
        .env("GITHUB_TOKEN", "test-token")
        .env("BITBUCKET_TOKEN", "test-token")
        .arg(command)
        .args(args)
        .assert()
        .success();
    let json: Vec<Value> =
        serde_json::from_slice(&assert.get_output().stdout).expect("git-reflow stdout must be a JSON array");
    assertions(&json);
    assert
}
