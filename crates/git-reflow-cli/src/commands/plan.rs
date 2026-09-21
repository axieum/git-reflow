use anyhow::Context;
use git_reflow_api::plan::plan_releases;
use git_reflow_api::settings::AppConfig;
use serde_json::json;

/// The `$ git reflow plan [package] ...` command.
#[derive(clap::Parser, Debug)]
pub struct PlanCommand {
    /// The package/s to plan releases for [default: all].
    #[arg(value_name = "PACKAGE", num_args = 0..)]
    pub packages: Vec<String>,
    /// The target branch name for the release [default: current branch].
    #[arg(short, long, value_name = "BRANCH")]
    pub target_branch: Option<String>,
    /// Show the `git-cliff` context in the output.
    #[arg(short = 'c', long, default_value_t = false)]
    pub show_context: bool,
}

impl PlanCommand {
    /// Plans the releases for all (or given) packages without actually performing the release.
    ///
    /// # Arguments
    ///
    /// * `config` - The app configuration.
    pub async fn plan(self, config: &AppConfig) -> anyhow::Result<()> {
        // Determine the target branch for the release.
        let target_branch = match self.target_branch {
            Some(target_branch) => target_branch,
            None => {
                let repo = git2::Repository::discover(".").context("not a git repository")?;
                let head = repo.head().context("failed to get HEAD")?;
                head.shorthand().context("failed to get branch name")?.to_string()
            }
        };

        // Plan the package release/s.
        let mut plan = plan_releases(config, &self.packages, &target_branch).await?;

        // If the user *does not* want to see the `git-cliff` context, replace it with an empty object.
        if !self.show_context {
            for release in &mut plan {
                release.context = json!({});
            }
        }

        // Serialise the package release plan/s to JSON and print it.
        let json = serde_json::to_string_pretty(&plan)?;
        println!("{json}");
        Ok(())
    }
}
