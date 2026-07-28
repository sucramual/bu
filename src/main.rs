use std::path::PathBuf;
use std::process::ExitCode;

use bu::{AppError, format_report, load_config, run_recycle, run_status};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(about = "Report and safely recycle configured Git worktree benches")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report configured benches without changing Git state.
    Status,
    /// Recycle eligible benches after guarded rechecks.
    Recycle,
}

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("bu: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, AppError> {
    let cli = Cli::parse();
    let config = load_config(cli.config)?;

    match cli.command {
        Command::Status => {
            let report = run_status(&config);
            print!("{}", format_report(&report));
            Ok(if report.has_failures() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Recycle => {
            let report = run_recycle(&config);
            print!("{}", format_report(&report));
            Ok(if report.has_failures() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
