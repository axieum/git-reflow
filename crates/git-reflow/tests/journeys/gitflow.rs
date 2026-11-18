use crate::common::{assert_reflow_plan, assert_reflow_prs, assert_reflow_releases};
use assert_cmd::Command;
use assert_fs::{TempDir, prelude::*};
use git_reflow_api::git::{create_or_reset_branch, fetch_branch, push_branch, push_tag, stage_and_commit, tag};
use git_reflow_api::provider::{PullRequestPackage, build_pull_request_body};
use git_reflow_fixtures::{add_origin_remote, project_repo};
use git2::Repository;
use httpmock::prelude::*;
use rstest::*;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;

/// Tests that the [Gitflow workflow](https://www.atlassian.com/git/tutorials/comparing-workflows/gitflow-workflow) is supported.
#[rstest]
fn test_gitflow_workflow(#[with("example-rust")] project_repo: (TempDir, Repository)) {
    let (project_dir, repo) = project_repo;
    let (_remote_dir, remote_repo) = add_origin_remote(&repo, "origin", "octocat", "Hello-World");
    let server = MockServer::start();
    let merge = |branch: &str| {
        Command::new("git")
            .current_dir(&project_dir)
            .args(["-c", "commit.gpgsign=false", "merge", "--no-ff", "--no-edit", branch])
            .assert()
            .success();
    };

    project_dir
        .child(".reflow.json")
        .write_str(
            &json!({
                "git": { "provider": { "github": { "host": server.base_url(), "commit-via-api": false } } },
            })
            .to_string(),
        )
        .unwrap();

    // Commit the current files to the `main` branch.
    repo.set_head("refs/heads/main").unwrap();
    let initial_commit = stage_and_commit(&repo, &["."], "chore: initial commit").unwrap();
    tag(&repo, "v1.0.0", initial_commit, false).unwrap();
    push_branch(&repo, "main", false).unwrap();
    push_tag(&repo, "v1.0.0", false).unwrap();

    // Create a `develop` branch from `main` and switch to it.
    create_or_reset_branch(&repo, "develop", initial_commit).unwrap();

    // Create a `feature` branch from `develop` and switch to it.
    let develop_commit = repo.head().unwrap().peel_to_commit().unwrap().id();
    create_or_reset_branch(&repo, "feature", develop_commit).unwrap();

    // Make some changes to the code and commit them to the `feature` branch.
    let main_rs_path = project_dir.path().join("src").join("main.rs");
    writeln!(
        fs::OpenOptions::new().append(true).open(&main_rs_path).unwrap(),
        "\npub fn subtract(a: i32, b: i32) -> i32 {{ a - b }}"
    )
    .unwrap();
    let feature_commit = stage_and_commit(&repo, &["."], "feat: add a `subtract` function").unwrap();

    // Merge the `feature` branch into the `develop` branch.
    create_or_reset_branch(&repo, "develop", develop_commit).unwrap();
    merge("feature");
    let develop_head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(develop_head.parent_count(), 2);
    assert_eq!(develop_head.parent_id(0).unwrap(), develop_commit);
    assert_eq!(develop_head.parent_id(1).unwrap(), feature_commit);
    push_branch(&repo, "develop", false).unwrap();

    // Plan the beta feature release on `develop`.
    let mut changelog_md = String::new();
    assert_reflow_plan(&project_dir, &["--show-context"], |plan| {
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0]["package-name"], "example-rust");
        assert_eq!(plan[0]["current-version"], "1.0.0");
        assert_eq!(plan[0]["next-version"], "1.1.0-beta.1");
        assert_eq!(plan[0]["commit-message"], "chore: release v1.1.0-beta.1");
        assert_eq!(plan[0]["dependencies"], json!({}));
        assert_eq!(plan[0]["context"]["version"], "1.1.0-beta.1");
        assert!(
            plan[0]["context"]["commits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|commit| commit["id"] == feature_commit.to_string())
        );
        changelog_md = plan[0]["changelog-md"].as_str().unwrap().to_string();
        assert!(changelog_md.starts_with("## [1.1.0-beta.1] - "));
        assert!(changelog_md.contains("Add a `subtract` function"));
    });

    let package = PullRequestPackage {
        name: "example-rust".to_string(),
        version: "1.1.0-beta.1".parse().unwrap(),
        changelog_md: Some(changelog_md.clone()),
    };
    let pr_body = build_pull_request_body([package.clone()]).unwrap();
    let mut github_pr: Value = serde_json::from_str(include_str!(
        "../../../git-reflow-api/tests/fixtures/github_pulls_retrieve.json"
    ))
    .unwrap();
    github_pr["title"] = json!("chore: release v1.1.0-beta.1");
    github_pr["body"] = json!(pr_body);
    github_pr["head"]["ref"] = json!("reflow--branches--develop");
    github_pr["base"]["ref"] = json!("develop");

    let list_prs_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/repos/octocat/Hello-World/pulls")
            .query_param("head", "octocat:reflow--branches--develop")
            .query_param("base", "develop");
        then.status(200).json_body(json!([]));
    });
    let create_pr_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/repos/octocat/Hello-World/pulls")
            .json_body(json!({
                "title": "chore: release v1.1.0-beta.1",
                "head": "reflow--branches--develop",
                "base": "develop",
                "body": pr_body,
            }));
        then.status(201).json_body(github_pr.clone());
    });

    // Create a release PR without changing the checked-out branch or working tree.
    assert_reflow_prs(&project_dir, &[], |prs| {
        assert_eq!(
            prs,
            &[json!({
                "number": 1347,
                "url": "https://github.com/octocat/Hello-World/pull/1347",
                "title": "chore: release v1.1.0-beta.1",
                "body": pr_body,
                "head": "reflow--branches--develop",
                "base": "develop",
                "existing": false,
                "packages": [package],
            })],
        );
    });
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "develop");
    assert_eq!(repo.head().unwrap().target().unwrap(), develop_head.id());
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    let release_commit = remote_repo
        .find_reference("refs/heads/reflow--branches--develop")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(release_commit.message().unwrap(), "chore: release v1.1.0-beta.1");
    assert_eq!(release_commit.parent_count(), 1);
    assert_eq!(release_commit.parent_id(0).unwrap(), develop_head.id());
    let diff = remote_repo
        .diff_tree_to_tree(
            Some(&release_commit.parent(0).unwrap().tree().unwrap()),
            Some(&release_commit.tree().unwrap()),
            None,
        )
        .unwrap();
    let changed_paths = diff
        .deltas()
        .map(|delta| delta.new_file().path().unwrap().to_path_buf())
        .collect::<Vec<_>>();
    assert_eq!(
        changed_paths,
        ["CHANGELOG.md", "Cargo.lock", "Cargo.toml"].map(std::path::PathBuf::from),
    );

    // Repeating the command must not create another release commit or PR.
    assert_reflow_prs(&project_dir, &[], |prs| assert!(prs.is_empty()));
    assert_eq!(
        remote_repo
            .refname_to_id("refs/heads/reflow--branches--develop")
            .unwrap(),
        release_commit.id(),
    );

    // Simulate merging the PR from the real remote release branch.
    let release_ref = fetch_branch(&repo, "origin", "reflow--branches--develop")
        .unwrap()
        .unwrap();
    merge(&release_ref);
    let released_head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(released_head.parent_count(), 2);
    assert_eq!(released_head.parent_id(0).unwrap(), develop_head.id());
    assert_eq!(released_head.parent_id(1).unwrap(), release_commit.id());
    push_branch(&repo, "develop", false).unwrap();

    for filename in ["Cargo.toml", "Cargo.lock"] {
        let contents = fs::read_to_string(project_dir.child(filename)).unwrap();
        assert!(contents.contains("version = \"1.1.0-beta.1\""), "{filename}");
        assert!(!contents.contains("version = \"1.0.0\""), "{filename}");
    }
    assert_eq!(
        fs::read_to_string(project_dir.child("CHANGELOG.md"))
            .unwrap()
            .replace("\r\n", "\n")
            .trim_end(),
        changelog_md,
    );

    github_pr["state"] = json!("closed");
    github_pr["merged"] = json!(true);
    github_pr["merge_commit_sha"] = json!(released_head.id().to_string());
    github_pr["head"]["sha"] = json!(release_commit.id().to_string());
    let associated_prs_mock = server.mock(|when, then| {
        when.method(GET).path(format!(
            "/repos/octocat/Hello-World/commits/{}/pulls",
            released_head.id()
        ));
        then.status(200).json_body(json!([github_pr]));
    });
    let get_release_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/repos/octocat/Hello-World/releases/tags/v1.1.0-beta.1");
        then.status(404).json_body(json!({ "message": "Not Found" }));
    });
    let create_release_mock = server.mock(|when, then| {
        let mut release: Value = serde_json::from_str(include_str!(
            "../../../git-reflow-api/tests/fixtures/github_releases_retrieve.json"
        ))
        .unwrap();
        release["html_url"] = json!("https://github.com/octocat/Hello-World/releases/v1.1.0-beta.1");
        release["tag_name"] = json!("v1.1.0-beta.1");
        release["target_commitish"] = json!(released_head.id().to_string());
        release["name"] = json!("v1.1.0-beta.1");
        release["body"] = json!(changelog_md);

        when.method(POST)
            .path("/repos/octocat/Hello-World/releases")
            .json_body(json!({
                "tag_name": "v1.1.0-beta.1",
                "target_commitish": released_head.id().to_string(),
                "name": "v1.1.0-beta.1",
                "body": changelog_md,
                "make_latest": "true",
            }));
        then.status(201).json_body(release);
    });

    // Tag the merged release PR, not the unmerged release-branch commit.
    assert_reflow_releases(&project_dir, &[], |releases| {
        assert_eq!(
            releases,
            &[json!({
                "package-name": "example-rust",
                "version": "1.1.0-beta.1",
                "major": 1,
                "minor": 1,
                "patch": 0,
                "pre": "beta.1",
                "build": null,
                "tag-name": "v1.1.0-beta.1",
                "url": "https://github.com/octocat/Hello-World/releases/v1.1.0-beta.1",
                "changelog-md": changelog_md,
            })],
        );
    });
    assert_reflow_releases(&project_dir, &[], |releases| assert!(releases.is_empty()));
    assert_eq!(repo.head().unwrap().target().unwrap(), released_head.id());
    assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");

    // The beta release is already tagged, so forward merges have no unreleased changes.
    // todo: Assert RC and stable promotions once channel changes alone can trigger a release.
    for (branch, source) in [("release/1.1.0", "develop"), ("main", "release/1.1.0")] {
        let main_commit = repo.refname_to_id("refs/heads/main").unwrap();
        create_or_reset_branch(&repo, branch, main_commit).unwrap();
        assert_reflow_plan(&project_dir, &[], |plan| assert!(plan.is_empty()));
        merge(source);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2);
        assert_eq!(head.parent_id(0).unwrap(), main_commit);
        assert_eq!(
            head.parent_id(1).unwrap(),
            repo.refname_to_id(&format!("refs/heads/{source}")).unwrap(),
        );
        push_branch(&repo, branch, false).unwrap();

        assert_reflow_plan(&project_dir, &[], |plan| assert!(plan.is_empty()));
        assert_reflow_prs(&project_dir, &[], |prs| assert!(prs.is_empty()));
        let no_prs_mock = server.mock(|when, then| {
            when.method(GET)
                .path(format!("/repos/octocat/Hello-World/commits/{}/pulls", head.id()));
            then.status(200).json_body(json!([]));
        });
        assert_reflow_releases(&project_dir, &[], |releases| assert!(releases.is_empty()));
        no_prs_mock.assert_calls(1);
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), branch);
        assert_eq!(repo.head().unwrap().target().unwrap(), head.id());
        assert!(repo.statuses(None).unwrap().is_empty(), "uncommitted changes");
        assert!(
            remote_repo
                .find_reference(&format!("refs/heads/reflow--branches--{branch}"))
                .is_err()
        );
        assert_eq!(head.tree_id(), released_head.tree_id());
    }

    for repository in [&repo, &remote_repo] {
        let names = repository.tag_names(None).unwrap();
        let mut names = names.iter().map(|name| name.unwrap().unwrap()).collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["v1.0.0", "v1.1.0-beta.1"]);
        for (name, commit) in [("v1.0.0", initial_commit), ("v1.1.0-beta.1", released_head.id())] {
            assert_eq!(
                repository
                    .find_reference(&format!("refs/tags/{name}"))
                    .unwrap()
                    .peel_to_commit()
                    .unwrap()
                    .id(),
                commit,
            );
        }
    }
    list_prs_mock.assert_calls(1);
    create_pr_mock.assert_calls(1);
    associated_prs_mock.assert_calls(2);
    get_release_mock.assert_calls(1);
    create_release_mock.assert_calls(1);
}
