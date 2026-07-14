mod commands;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use miette::IntoDiagnostic as _;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Parser)]
#[command(name = "d3j", about = "Structural three-way merge")]
struct Cli {
    /// Generate shell completions and exit.
    #[arg(long, value_enum)]
    completions: Option<Shell>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Merge two branches against their common origin.
    Merge(commands::merge::MergeArgs),
    /// Check a merged file against the universality conditions.
    Check(commands::check::CheckArgs),
}

/// Exit codes: 0 merged/correct, 1 conflicts/violations, 2 error.
/// miette's Result would map every error to 1, so errors are rendered
/// by hand (Report's Debug is the fancy renderer).
fn main() -> ExitCode {
    miette::set_panic_hook();
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    match run() {
        Ok(code) => code,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::from(2)
        }
    }
}

fn run() -> miette::Result<ExitCode> {
    let cli = Cli::parse();

    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "d3j", &mut std::io::stdout());
        return Ok(ExitCode::SUCCESS);
    }

    let Some(command) = cli.command else {
        Cli::command().print_help().into_diagnostic()?;
        return Ok(ExitCode::SUCCESS);
    };

    match command {
        Commands::Merge(args) => commands::merge::run(args),
        Commands::Check(args) => commands::check::run(args),
    }
}
