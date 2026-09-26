use anyhow::Context;
use git_reflow_api::apply::apply_release_plan;
use git_reflow_api::plan::plan_releases;
use git_reflow_api::settings::AppConfig;
use std::path::PathBuf;
use tracing::debug;

/// The `$ git reflow pr [package] ...` command.
#[derive(clap::Parser, Debug)]
pub struct PullRequestCommand {
    /// The package/s to create pull requests for [default: all].
    #[arg(value_name = "PACKAGE", num_args = 0..)]
    pub packages: Vec<String>,
    /// The target branch name for the pull request [default: current branch].
    #[arg(short, long, value_name = "BRANCH")]
    pub target_branch: Option<String>,
    /// The path to the release plan JSON file [default: generated].
    #[arg(short, long, value_name = "PLAN")]
    pub plan: Option<PathBuf>,
    /// Report on the activity that would happen without taking action.
    #[arg(long)]
    pub dry_run: bool,
}

impl PullRequestCommand {
    /// Creates or updates release pull request(s) on the configured Git provider.
    ///
    /// # Arguments
    ///
    /// * `config` - The app configuration.
    pub async fn create_pull_requests(self, config: &AppConfig) -> anyhow::Result<()> {
        // Determine the target branch for the pull requests.
        let target_branch = match self.target_branch {
            Some(target_branch) => target_branch,
            None => {
                let repo = git2::Repository::discover(".").context("not a git repository")?;
                let head = repo.head().context("failed to get HEAD")?;
                head.shorthand().context("failed to get branch name")?.to_string()
            }
        };

        // Plan the package release/s.
        // NB: If `--plan` is provided, we use it, otherwise, we generate a new plan for them.
        //     This allows the user to `git reflow plan > plan.json`, edit it, and then use it here.
        let plan = if let Some(plan_path) = self.plan {
            debug!("reading release plan from `{}`", plan_path.display());
            let plan_json = std::fs::read_to_string(&plan_path).context("could not read release plan")?;
            serde_json::from_str(&plan_json).context("malformed release plan")?
        } else {
            debug!("creating a new release plan");
            plan_releases(config, &self.packages, &target_branch).await?
        };

        // Write the changes to each branch.
        let prs = if plan.is_empty() {
            vec![]
        } else {
            apply_release_plan(config, &plan, self.dry_run, None).await?
        };

        // Serialise the package release outcome/s to JSON and print it.
        let json = serde_json::to_string_pretty(&prs)?;
        println!("{json}");
        Ok(())
    }
}
