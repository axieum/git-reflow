use crate::commands::config::ConfigCommand;
use crate::commands::plan::PlanCommand;
use crate::commands::pr::PullRequestCommand;
use clap::{ColorChoice, Parser, builder::PathBufValueParser};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use git_reflow_api::settings;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::{env, io};
use tracing::{error, trace};
use tracing_log::AsTrace;

mod commands;

#[derive(clap::Parser, Debug)]
#[command(about, version, author)]
pub struct CliArgs {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
    /// The config file path.
    #[arg(short, long, value_name = "PATH", value_parser = PathBufValueParser::new())]
    pub config: Option<PathBuf>,
    /// Increase logging verbosity.
    #[command(flatten)]
    pub verbose: Verbosity<InfoLevel>,
    /// Control when to use colour.
    #[arg(long, default_value_t = ColorChoice::Auto, value_enum)]
    pub color: ColorChoice,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Plan the releases for packages/s without actually releasing anything.
    Plan(PlanCommand),
    /// Create or update release pull request/s for changes to package/s.
    #[command(name = "pr")]
    PullRequest(PullRequestCommand),
    /// Print the configuration and exit.
    Config(ConfigCommand),
}

/// The main entrypoint of the `git-reflow` command-line interface.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse the command-line arguments.
    let cli = CliArgs::parse();

    // Set up the logging.
    tracing_subscriber::fmt()
        .with_env_filter(
            // Prefer the `REFLOW_LOG` environment variable, e.g. `REFLOW_LOG=debug,handlebars=debug`.
            tracing_subscriber::EnvFilter::try_from_env("REFLOW_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(cli.verbose.log_level_filter().as_trace().into())
                    .from_env_lossy()
                    .add_directive("handlebars=warn".parse().unwrap())
                    .add_directive("hyper_rustls=warn".parse().unwrap())
                    .add_directive("hyper_util=warn".parse().unwrap())
                    .add_directive("octocrab=warn".parse().unwrap())
                    .add_directive("tower=warn".parse().unwrap())
            }),
        )
        .with_ansi(match cli.color {
            ColorChoice::Auto => env::var("NO_COLOR").is_err() && io::stdout().is_terminal(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        })
        .init();

    // Run the command and exit.
    if let Err(err) = run(cli).await {
        error!("{err:?}");
        std::process::exit(1);
    }
    Ok(())
}

/// Executes the necessary commands from the parsed command-line arguments.
async fn run(cli: CliArgs) -> anyhow::Result<()> {
    // Load the configuration.
    let config = settings::load(cli.config)?;

    // Run the command.
    trace!("run `{:?}` command", cli.command);
    match cli.command {
        // $ git reflow config
        Command::Config(cmd) => cmd.print_config(&config)?,
        // $ git reflow plan [package] ...
        Command::Plan(cmd) => cmd.plan(&config).await?,
        // $ git reflow pr [package] ...
        Command::PullRequest(cmd) => cmd.create_pull_requests(&config).await?,
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use clap::CommandFactory;

    /// Tests that the command-line interface is valid.
    #[test]
    fn verify_cli() {
        CliArgs::command().debug_assert();
    }
}
