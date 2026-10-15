//! paranoid — deterministic, minimal OCI images from Debian packages.
//!
//! Subcommands:
//!   lock     Resolve dependencies and write paranoid.lock
//!   build    Download packages and build an OCI image from them
//!   inspect  Show the contents of a lock file or built image

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli;

use cli::build::BuildArgs;
use cli::inspect::InspectArgs;
use cli::lock::LockArgs;
use cli::style as st;

#[derive(Parser)]
#[command(
    name = "paranoid",
    version,
    about = "Build deterministic, minimal OCI images from Debian packages",
    long_about = None,
)]
struct Cli {
    /// Suppress all output except errors.
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Resolve dependencies and write paranoid.lock.
    Lock(LockArgs),

    /// Download packages and build an OCI image from them.
    Build(BuildArgs),

    /// Show the contents of a lock file or image.
    Inspect(InspectArgs),
}

#[tokio::main]
async fn main() -> ExitCode {
    let Cli { quiet, command } = Cli::parse();

    let result = match command {
        Commands::Lock(args) => cli::lock::run(args, quiet).await,
        Commands::Build(args) => cli::build::run(args, quiet).await,
        Commands::Inspect(args) => cli::inspect::run(args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            st::error(&e);
            ExitCode::FAILURE
        }
    }
}
