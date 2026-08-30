//! `voltra` — the headless front-end.
//!
//! libobs grew a command-line interface late, which is why automating OBS is
//! awkward today. Voltra keeps a headless entry point from the first commit: it
//! is what CI runs, what benchmarks drive, and what a studio operator can script.

// See the note in voltra-core: panicking helpers stay available inside tests.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod info;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "voltra",
    version,
    about = "Voltra Studio — live compositing, recording and streaming",
    propagate_version = true
)]
struct Cli {
    /// Increase log verbosity (repeat for more detail).
    ///
    /// `RUST_LOG` takes precedence when set, so operators keep full control.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Print build and host information.
    Info,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Info => info::print(&mut std::io::stdout())?,
    }

    Ok(())
}

/// Install the tracing subscriber.
///
/// `RUST_LOG` wins when present; otherwise `-v` flags pick the level. Logging
/// never runs on the render or audio threads, so a formatting subscriber here
/// costs nothing on the hot path.
fn init_tracing(verbosity: u8) {
    let fallback = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("voltra={fallback}")));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}
