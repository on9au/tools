use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tool_core::{InstallLayout, install_binary_file};

/// Installs binaries into the managed tools directory.
#[derive(Debug, Parser)]
#[command(
    name = "tool-installer",
    version,
    about = "Install personal tool binaries"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Installs a binary into the managed bin directory.
    Install(InstallArgs),
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// The source binary to install.
    source: PathBuf,
    /// Optional destination name. Defaults to the source file name.
    #[arg(long)]
    name: Option<String>,
    /// Always copy the source file instead of trying a hard link first.
    #[arg(long)]
    copy: bool,
}

fn main() -> ExitCode {
    match try_main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let layout = InstallLayout::discover()?;

    match cli.command {
        Commands::Install(install_args) => install_binary(&layout, install_args),
    }
}

/// Installs one binary into the managed bin directory.
fn install_binary(layout: &InstallLayout, install_args: InstallArgs) -> Result<ExitCode> {
    let source = install_args
        .source
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", install_args.source.display()))?;

    let destination_name = match install_args.name {
        Some(name) => name,
        None => default_name(&source)?,
    };

    let destination = install_binary_file(layout, &source, &destination_name, install_args.copy)?;

    println!(
        "installed {} -> {}",
        source.display(),
        destination.display()
    );

    Ok(ExitCode::SUCCESS)
}

/// Returns the default installed name derived from the source file name.
fn default_name(source: &Path) -> Result<String> {
    tool_core::display_name(source)
}
