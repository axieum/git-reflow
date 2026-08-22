use crate::commands::{config::ConfigCommand};
use clap::{builder::PathBufValueParser, ColorChoice, Parser};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use git_reflow_api::settings;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::{env, io};
use tracing::{trace, error};
use tracing_log;
use tracing_log::AsTrace;
use tracing_subscriber;

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
        .with_max_level(cli.verbose.log_level_filter().as_trace())
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
        Command::Config(cmd) => cmd.print_config(&config).await?,
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
