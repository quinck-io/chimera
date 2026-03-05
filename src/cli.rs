use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tracing::info;

use crate::config::{ChimeraPaths, default_root, load_config, load_runner_credentials};
use crate::runner::Runner;

#[derive(Parser)]
#[command(name = "chimera", about = "GitHub Actions self-hosted runner daemon")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Register a new runner with GitHub
    Register {
        /// GitHub repository or organization URL
        #[arg(long)]
        url: String,

        /// One-time registration token from GitHub Settings
        #[arg(long)]
        token: String,

        /// Runner name (must be unique within the scope)
        #[arg(long)]
        name: String,

        /// Additional custom labels
        #[arg(long, value_delimiter = ',')]
        labels: Vec<String>,

        /// Chimera root directory
        #[arg(long, default_value_os_t = default_root())]
        root: PathBuf,
    },

    /// Remove a registered runner
    Unregister {
        /// Runner name to remove
        #[arg(long)]
        name: String,

        /// Chimera root directory
        #[arg(long, default_value_os_t = default_root())]
        root: PathBuf,
    },

    /// Start the runner daemon
    Start {
        /// Run a specific runner (default: first registered)
        #[arg(long)]
        runner: Option<String>,

        /// Chimera root directory
        #[arg(long, default_value_os_t = default_root())]
        root: PathBuf,
    },

    /// Show registered runners and their status
    Status {
        /// Chimera root directory
        #[arg(long, default_value_os_t = default_root())]
        root: PathBuf,
    },
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Register {
            url,
            token,
            name,
            labels,
            root,
        } => {
            init_tracing();
            crate::github::registration::register(&url, &token, &name, &labels, &root).await
        }

        Command::Unregister { name, root } => {
            init_tracing();
            crate::github::registration::unregister(&name, &root).await
        }

        Command::Start { runner, root } => {
            init_tracing();
            run_start(runner, root).await
        }

        Command::Status { root } => run_status(root),
    }
}

async fn run_start(runner_name: Option<String>, root: PathBuf) -> Result<()> {
    let paths = ChimeraPaths::new(root);
    let config = load_config(&paths.config_file()).context("loading config")?;

    if config.runners.is_empty() {
        bail!("no runners registered. Use 'chimera register' first.");
    }

    let name = runner_name.unwrap_or_else(|| config.runners[0].clone());
    if !config.runners.contains(&name) {
        bail!("runner '{}' not found in config", name);
    }

    let creds = load_runner_credentials(&paths.runners_dir(), &name)
        .with_context(|| format!("loading credentials for runner '{name}'"))?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");

        tokio::select! {
            _ = ctrl_c => info!("received SIGINT"),
            _ = sigterm.recv() => info!("received SIGTERM"),
        }

        let _ = shutdown_tx.send(true);
    });

    let runner = Runner::new(name, creds);
    runner.start(shutdown_rx).await
}

fn run_status(root: PathBuf) -> Result<()> {
    let paths = ChimeraPaths::new(root);
    let config_path = paths.config_file();

    if !config_path.exists() {
        println!(
            "No chimera configuration found at {}",
            config_path.display()
        );
        println!("Run 'chimera register' to set up a runner.");
        return Ok(());
    }

    let config = load_config(&config_path)?;

    if config.runners.is_empty() {
        println!("No runners registered.");
        return Ok(());
    }

    println!("Root:       {}", paths.root.display());
    println!("Work dir:   {}", paths.work_dir().display());
    println!("Log dir:    {}", paths.logs_dir().display());
    println!("Temp dir:   {}", paths.tmp_dir().display());
    println!("Tool cache: {}", paths.tool_cache_dir().display());
    println!();
    println!("Registered runners:");
    for name in &config.runners {
        let runner_dir = paths.runner_dir(name);
        let status = if runner_dir.join("runner.json").exists() {
            "configured"
        } else {
            "missing credentials"
        };
        println!("  {name}: {status}");
    }

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
