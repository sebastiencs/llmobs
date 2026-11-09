use std::io::{self, Write};

use clap::{Parser, Subcommand};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "llmobs", version, about = "LLM observability server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the llmobs version.
    Version,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => print_version(),
    }
}

fn print_version() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "llmobs {}", env!("CARGO_PKG_VERSION"))
}
