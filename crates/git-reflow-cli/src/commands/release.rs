use git_reflow_api::release::create_releases;
use git_reflow_api::settings::AppConfig;

/// The `$ git reflow release` command.
#[derive(clap::Parser, Debug)]
pub struct ReleaseCommand {
    /// Report on the activity that would happen without taking action.
    #[arg(long)]
    pub dry_run: bool,
}

impl ReleaseCommand {
    /// Tags and releases the most recently merged release pull request.
    ///
    /// # Arguments
    ///
    /// * `config` - The app configuration.
    pub async fn release(self, config: &AppConfig) -> anyhow::Result<()> {
        // Release the most recently merged release pull request.
        let outcomes = create_releases(config, self.dry_run).await?;

        // Serialise the packages that were released to JSON and print it.
        let json = serde_json::to_string_pretty(&outcomes)?;
        println!("{json}");
        Ok(())
    }
}
