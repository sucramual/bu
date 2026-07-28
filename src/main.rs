use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use bu::{
    AppError, StatusFormat, format_report, format_status_report, load_config, run_recycle,
    run_status,
};
use clap::{Parser, Subcommand, ValueEnum};

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
    Status {
        /// Include full bench paths, dirty files, and failure diagnostics.
        #[arg(short, long)]
        verbose: bool,
        /// Control ANSI styling in status output.
        #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
        color: ColorMode,
    },
    /// Recycle eligible benches after guarded rechecks.
    Recycle {
        /// Control ANSI styling in recycle output.
        #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
        color: ColorMode,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
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
        Command::Status { verbose, color } => {
            let report = run_status(&config);
            print!(
                "{}",
                format_status_report(
                    &report,
                    StatusFormat {
                        verbose,
                        use_color: resolve_color(
                            color,
                            std::io::stdout().is_terminal(),
                            std::env::var_os("NO_COLOR").is_some()
                        ),
                    },
                )
            );
            Ok(if report.has_failures() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Recycle { color } => {
            let report = run_recycle(&config);
            print!(
                "{}",
                format_report(
                    &report,
                    resolve_color(
                        color,
                        std::io::stdout().is_terminal(),
                        std::env::var_os("NO_COLOR").is_some()
                    ),
                )
            );
            Ok(if report.has_failures() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

fn resolve_color(mode: ColorMode, stdout_is_terminal: bool, no_color: bool) -> bool {
    match mode {
        ColorMode::Auto => stdout_is_terminal && !no_color,
        ColorMode::Always => true,
        ColorMode::Never => false,
    }
}
