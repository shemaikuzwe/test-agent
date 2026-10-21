use std::{path::Path, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use sl_runner::{database::Store, deployment, deployment::types::Deployment, worker};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Scriptylabs hosting deployment runner")]
struct Cli {
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
        image: String,
        #[arg(long)]
        application_id: Uuid,
        #[arg(long)]
        hostname: String,
        #[arg(long)]
        port: u16,
    },
    Wait {
        #[arg(long)]
        deployment_id: Uuid,
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
    let state_dir = Path::new("/var/lib/sl-runner");
    match cli.command {
        Command::Migrate => Store::migrate(state_dir).await,
        Command::Submit {
            deployment_id,
            image,
            application_id,
            hostname,
            port,
        } => {
            let mut conn = Store::open(state_dir).await?;
            deployment::submit(
                &mut conn,
                Deployment {
                    deployment_id,
                    image,
                    application_id,
                    hostname,
                    port,
                },
            )
            .await?;
            Ok(())
        }
        Command::Wait { deployment_id } => {
            let mut conn = Store::open(state_dir).await?;
            deployment::wait(&mut conn, deployment_id).await
        }
        Command::Daemon => {
            let mut conn = Store::open(state_dir).await?;
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
            worker::run(&mut conn, shutdown).await
        }
    }
}
