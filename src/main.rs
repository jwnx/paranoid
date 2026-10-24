//! paranoid — deterministic, minimal OCI images from Debian packages.
//!
//! Subcommands:
//!   lock     Resolve dependencies and write paranoid.lock
//!   build    Download packages and build an OCI image from them
//!   cache    Fill a store with the lock's packages
//!   diff     Show how two lock files differ
//!   inspect  Show the contents of a lock file or built image

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli;

use cli::build::BuildArgs;
use cli::cache::CacheArgs;
use cli::diff::DiffArgs;
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

    /// Fill a store with the lock's packages.
    Cache(CacheArgs),

    /// Show how two lock files differ.
    Diff(DiffArgs),

    /// Show the contents of a lock file or image.
    Inspect(InspectArgs),
}

#[tokio::main]
async fn main() -> ExitCode {
    restore_sigpipe();
    let Cli { quiet, command } = Cli::parse();

    let result = match command {
        Commands::Lock(args) => cli::lock::run(args, quiet).await,
        Commands::Build(args) => cli::build::run(args, quiet).await,
        Commands::Cache(args) => cli::cache::run(args, quiet).await,
        Commands::Diff(args) => cli::diff::run(args),
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

/// Let a closed pipe end the process quietly, as it does for other command
/// line tools, instead of making `println!` panic: `paranoid inspect lock |
/// head` should just stop. Rust ignores SIGPIPE by default.
fn restore_sigpipe() {
    #[cfg(unix)]
    // SAFETY: SIG_DFL is a valid disposition for SIGPIPE, and nothing else
    // in the process installs a SIGPIPE handler for this to race with.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
