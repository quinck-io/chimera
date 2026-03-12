mod cache;
mod cli;
mod config;
mod daemon;
mod docker;
mod github;
mod job;
mod node;
mod runner;
mod utils;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    cli::run(cli::Cli::parse()).await
}
