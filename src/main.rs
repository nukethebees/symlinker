use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Links file to read, or create with import. Its parent is the destination.
    file: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a links file for the directory's immediate symlinks; never overwrite a file.
    Import { directory: PathBuf },
    /// Show the reconciliation plan without making changes.
    Plan,
    /// Validate, show, and execute the reconciliation plan.
    Apply,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Import { directory } = &cli.command {
        let count = symlinker::import(directory, &cli.file)?;
        println!("Imported {count} symlinks into {}", cli.file.display());

        return Ok(());
    }

    let plan = symlinker::plan(&cli.file)?;
    let mut output = io::stdout().lock();
    plan.display(&mut output)?;
    output.flush()?;

    if matches!(cli.command, Command::Apply) {
        plan.apply()?;
    }

    Ok(())
}
