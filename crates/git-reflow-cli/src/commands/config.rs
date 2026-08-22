use git_reflow_api::settings::AppConfig;

/// The `$ git reflow config` command.
#[derive(clap::Parser, Debug)]
pub struct ConfigCommand {}

impl ConfigCommand {
    /// Prints the `git-reflow` configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The app configuration.
    pub async fn print_config(self, config: &AppConfig) -> anyhow::Result<()> {
        // Serialise the configuration to JSON and print it.
        let json = serde_json::to_string_pretty(config)?;
        println!("{json}");
        Ok(())
    }
}
