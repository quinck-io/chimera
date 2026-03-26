use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::{ChimeraConfig, ChimeraPaths, DaemonConfig, default_root, load_config};
use crate::daemon::{Daemon, format_status_display, is_process_alive, read_state_file};

#[derive(Parser)]
#[command(
    name = "chimera",
    about = "GitHub Actions self-hosted runner daemon",
    version
)]
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
            init_tracing(&DaemonConfig::default());
            crate::github::registration::register(&url, &token, &name, &labels, &root).await
        }

        Command::Unregister { name, root } => {
            init_tracing(&DaemonConfig::default());
            crate::github::registration::unregister(&name, &root).await
        }

        Command::Start { root } => {
            let paths = ChimeraPaths::new(root);
            let config = load_config(&paths.config_file()).context("loading config")?;
            init_tracing(&config.daemon);
            run_start(paths, config).await
        }

        Command::Status { root } => run_status(root),
    }
}

async fn run_start(paths: ChimeraPaths, config: ChimeraConfig) -> Result<()> {
    if config.runners.is_empty() {
        bail!("no runners registered. Use 'chimera register' first.");
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => info!("received SIGINT"),
                    _ = sigterm.recv() => info!("received SIGTERM"),
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to register SIGTERM handler, using SIGINT only");
                let _ = ctrl_c.await;
                info!("received SIGINT");
            }
        }
        let _ = shutdown_tx.send(true);
    });

    let daemon = Daemon::new(paths, config);
    daemon.run(shutdown_rx).await
}

fn run_status(root: PathBuf) -> Result<()> {
    let paths = ChimeraPaths::new(root);
    let state_path = paths.state_file();

    if state_path.exists()
        && let Ok(snapshot) = read_state_file(&state_path)
        && is_process_alive(snapshot.pid)
    {
        print!("{}", format_status_display(&snapshot));
        return Ok(());
    }

    println!("Daemon: not running");
    println!();

    let config_path = paths.config_file();
    if !config_path.exists() {
        println!("No chimera configuration found. Run 'chimera register' first.");
        return Ok(());
    }

    let config = load_config(&config_path)?;
    if config.runners.is_empty() {
        println!("No runners registered.");
        return Ok(());
    }

    println!("Root:       {}", paths.root.display());
    println!("Work dir:   {}", paths.work_dir().display());
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

fn init_tracing(daemon_config: &DaemonConfig) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let use_json = daemon_config.log_format == "json";

    if use_json {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }
}
