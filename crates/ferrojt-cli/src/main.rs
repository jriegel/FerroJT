//! `ferrojt` command line interface.

mod cli;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    cli::init_tracing(cli.verbose, cli.log.as_deref())?;

    match &cli.command {
        Command::Info(args) => cli::info(args),
        Command::Inspect(args) => cli::inspect(args),
        Command::Convert(args) => cli::convert(args),
        Command::Ls(args) => cli::ls(args),
    }
}
