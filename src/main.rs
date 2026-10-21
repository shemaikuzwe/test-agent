use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use sl_runner::{
    api::Api, config::Config, database::Store, deployment, deployment::types::Deployment, worker,
};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Scriptylabs hosting deployment runner")]
struct Cli {
    #[arg(long, global = true, default_value = "config.json")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Migrate,
    Submit {
        #[arg(long)]
        deployment_id: Uuid,
        #[arg(long)]
        node_id: Uuid,
        #[arg(long)]
        image: String,
    },
    Daemon,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "runner failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let config = Config::load(&cli.config).await?;
    match cli.command {
        Command::Migrate => Store::migrate(&config.state_dir).await,
        Command::Submit {
            deployment_id,
            node_id,
            image,
        } => {
            let mut conn = Store::open(&config.state_dir).await?;
            deployment::submit(
                &mut conn,
                Deployment {
                    deployment_id,
                    node_id,
                    image,
                },
                config.node_id,
            )
            .await?;
            Ok(())
        }
        Command::Daemon => {
            let api = Api::new(&config).await?;
            let mut conn = Store::open(&config.state_dir).await?;
            let (stop, shutdown) = watch::channel(false);
            let mut sigterm = signal(SignalKind::terminate())?;
            let mut sigint = signal(SignalKind::interrupt())?;
            tokio::spawn(async move {
                tokio::select! {
                    _ = sigterm.recv() => {}
                    _ = sigint.recv() => {}
                }
                let _ = stop.send(true);
            });
            worker::run(&api, &mut conn, shutdown).await
        }
    }
}
