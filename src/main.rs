use std::{collections::BTreeMap, path::Path, process::ExitCode};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use sl_runner::{
    database::Store,
    deployment,
    deployment::types::{Deployment, JobState},
    response::{CommandData, CommandError, CommandName, ErrorCode, ErrorData, Response},
    worker,
};
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
    Upgrade {
        #[arg(long)]
        version: Option<String>,
    },
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
    Rollback {
        deployment_id: Uuid,
    },
    Wait {
        deployment_id: Uuid,
    },
    Status {
        deployment_id: Uuid,
    },
    Running {
        application_id: Uuid,
    },
    Remove {
        application_id: Uuid,
    },
    Daemon,
}

impl Command {
    fn context(&self) -> (CommandName, ErrorData) {
        let mut data = ErrorData::default();
        let name = match self {
            Self::Upgrade { .. } => CommandName::Upgrade,
            Self::Migrate => CommandName::Migrate,
            Self::Submit {
                deployment_id,
                application_id,
                ..
            } => {
                data.deployment_id = Some(*deployment_id);
                data.application_id = Some(*application_id);
                CommandName::Submit
            }
            Self::Rollback { deployment_id } => {
                data.deployment_id = Some(*deployment_id);
                CommandName::Rollback
            }
            Self::Wait { deployment_id } => {
                data.deployment_id = Some(*deployment_id);
                CommandName::Wait
            }
            Self::Status { deployment_id } => {
                data.deployment_id = Some(*deployment_id);
                CommandName::Status
            }
            Self::Running { application_id } => {
                data.application_id = Some(*application_id);
                CommandName::Running
            }
            Self::Remove { application_id } => {
                data.application_id = Some(*application_id);
                CommandName::Remove
            }
            Self::Daemon => CommandName::Daemon,
        };
        (name, data)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if !error.use_stderr() => {
            return match error.print() {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        Err(error) => {
            let command = std::env::args_os()
                .nth(1)
                .and_then(|arg| arg.into_string().ok())
                .and_then(|arg| CommandName::from_str(&arg, false).ok());
            // Clap's diagnostic can echo supplied values, including --env secrets.
            let response = Response::failure(
                command,
                ErrorData::default(),
                CommandError {
                    code: ErrorCode::InvalidArgument,
                    message: error.kind().to_string(),
                },
            );
            return match response.write(std::io::stdout().lock()) {
                Ok(()) => ExitCode::from(2),
                Err(_) => ExitCode::FAILURE,
            };
        }
    };
    let (command, mut context) = cli.command.context();
    let (output, exit) = match run(cli, Path::new("/var/lib/sl-runner")).await {
        Ok(data) => (
            Response::success(command, data).write(std::io::stdout().lock()),
            ExitCode::SUCCESS,
        ),
        Err(error) => {
            let error = CommandError::from(error);
            if error.code == ErrorCode::DeploymentFailed {
                context.state = Some(JobState::Failed);
                context.message = Some("deployment failed");
            }
            let output =
                Response::failure(Some(command), context, error).write(std::io::stdout().lock());
            (output, ExitCode::FAILURE)
        }
    };
    if let Err(error) = output {
        tracing::error!(%error, "write command response");
        return ExitCode::FAILURE;
    }
    exit
}

async fn run(cli: Cli, state_dir: &Path) -> Result<CommandData> {
    match cli.command {
        Command::Upgrade { version } => {
            let status = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(include_str!("../scripts/upgrade.sh"))
                .arg("sl-runner upgrade")
                .arg(version.unwrap_or_else(|| "latest".to_owned()))
                .stdout(std::process::Stdio::from(std::io::stderr()))
                .status()
                .await?;
            ensure!(status.success(), "upgrade failed; see stderr for details");
            Ok(CommandData::Upgrade {
                message: "requested runner version is installed; service is active",
            })
        }
        Command::Migrate => {
            Store::migrate(state_dir)
                .await
                .context(ErrorCode::DatabaseError)?;
            Ok(CommandData::Migrate {
                message: "database migrations applied",
            })
        }
        Command::Submit {
            deployment_id,
            image,
            application_id,
            hostname,
            port,
            env,
        } => {
            let deployment = Deployment {
                deployment_id,
                image,
                application_id,
                hostname,
                port,
                env: parse_env(env).context(ErrorCode::InvalidArgument)?,
            };
            deployment.validate().context(ErrorCode::InvalidArgument)?;
            let mut conn = Store::open(state_dir)
                .await
                .context(ErrorCode::DatabaseError)?;
            let queued = deployment::submit(&mut conn, deployment).await?;
            Ok(CommandData::Submit {
                message: if queued {
                    "deployment queued"
                } else {
                    "deployment already exists"
                },
                deployment_id,
                queued,
            })
        }
        Command::Rollback { deployment_id } => {
            let mut conn = Store::open(state_dir)
                .await
                .context(ErrorCode::DatabaseError)?;
            let id = deployment::rollback(&mut conn, deployment_id).await?;
            Ok(CommandData::Rollback {
                message: "rollback queued",
                deployment_id: id,
                target_deployment_id: deployment_id,
                queued: true,
            })
        }
        Command::Wait { deployment_id } => {
            let mut conn = Store::open(state_dir)
                .await
                .context(ErrorCode::DatabaseError)?;
            let result = deployment::wait(&mut conn, deployment_id).await?;
            if result.state == JobState::Failed {
                return Err(anyhow::anyhow!(
                    result
                        .error
                        .unwrap_or_else(|| "no failure reason recorded".into())
                )
                .context(ErrorCode::DeploymentFailed));
            }
            Ok(CommandData::Wait {
                message: "deployment deployed",
                deployment_id,
                state: result.state,
            })
        }
        Command::Status { deployment_id } => {
            let mut conn = Store::open(state_dir)
                .await
                .context(ErrorCode::DatabaseError)?;
            Ok(CommandData::Status(
                deployment::status(&mut conn, deployment_id).await?,
            ))
        }
        Command::Running { application_id } => {
            let deployments = deployment::running(application_id).await?;
            Ok(CommandData::Running {
                application_id,
                deployments,
            })
        }
        Command::Remove { application_id } => {
            deployment::remove(application_id)
                .await
                .context(ErrorCode::DockerError)?;
            Ok(CommandData::Remove {
                message: "application removal requested",
                application_id,
                removal_requested: true,
            })
        }
        Command::Daemon => {
            let mut conn = Store::open(state_dir)
                .await
                .context(ErrorCode::DatabaseError)?;
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
            worker::run(&mut conn, shutdown)
                .await
                .context(ErrorCode::DatabaseError)?;
            Ok(CommandData::Daemon {
                message: "daemon stopped",
                reason: "shutdown_signal",
            })
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
