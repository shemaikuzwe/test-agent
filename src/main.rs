use std::{collections::BTreeMap, path::Path, process::ExitCode};

use anyhow::{Result, ensure};
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
        #[arg(long, value_name = "KEY=VALUE")]
        env: Vec<String>,
    },
    /// Restore a successful deployment's saved configuration as a new deployment.
    Rollback {
        #[arg(long)]
        deployment_id: Uuid,
    },
    Wait {
        #[arg(long)]
        deployment_id: Uuid,
    },
    Status {
        #[arg(long)]
        deployment_id: Uuid,
    },
    Running {
        #[arg(long)]
        application_id: Uuid,
    },
    Remove {
        #[arg(long)]
        application_id: Uuid,
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
            env,
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
                    env: parse_env(env)?,
                },
            )
            .await?;
            Ok(())
        }
        Command::Rollback { deployment_id } => {
            let mut conn = Store::open(state_dir).await?;
            let id = deployment::rollback(&mut conn, deployment_id).await?;
            println!("{}", serde_json::json!({ "deploymentId": id }));
            Ok(())
        }
        Command::Wait { deployment_id } => {
            let mut conn = Store::open(state_dir).await?;
            deployment::wait(&mut conn, deployment_id).await
        }
        Command::Status { deployment_id } => {
            let mut conn = Store::open(state_dir).await?;
            println!(
                "{}",
                serde_json::to_string(&deployment::status(&mut conn, deployment_id).await?)?
            );
            Ok(())
        }
        Command::Running { application_id } => {
            println!(
                "{}",
                serde_json::to_string(&deployment::running(application_id).await?)?
            );
            Ok(())
        }
        Command::Remove { application_id } => deployment::remove(application_id).await,
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

fn parse_env(entries: Vec<String>) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for entry in entries {
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("environment variable must use KEY=VALUE"))?;
        ensure!(
            !env.contains_key(name),
            "duplicate environment variable name"
        );
        env.insert(name.to_owned(), value.to_owned());
    }
    Ok(env)
}
