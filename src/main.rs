use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    chimera::cli::run(chimera::cli::Cli::parse()).await
}
