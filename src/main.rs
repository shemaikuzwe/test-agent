use std::{collections::BTreeMap, path::Path, process::ExitCode};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{
    Deserializer,
    de::{MapAccess, Visitor},
};
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
    /// Remove the runner service and binary.
    Uninstall {
        /// Also delete saved state, registry credentials, and the runner account.
        #[arg(long)]
        purge: bool,
    },
    /// Check whether the installed runner service is active.
    Health,
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
        application_id: String,
        #[arg(long)]
        hostname: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Read a JSON object of environment string values from stdin.
        #[arg(long)]
        env_stdin: bool,
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
        application_id: String,
    },
    Remove {
        application_id: String,
    },
    Daemon,
}

impl Command {
    fn context(&self) -> (CommandName, ErrorData) {
        let mut data = ErrorData::default();
        let name = match self {
            Self::Uninstall { .. } => CommandName::Uninstall,
            Self::Health => CommandName::Health,
            Self::Upgrade { .. } => CommandName::Upgrade,
            Self::Migrate => CommandName::Migrate,
            Self::Submit {
                deployment_id,
                application_id,
                ..
            } => {
                data.deployment_id = Some(*deployment_id);
                data.application_id = Some(application_id.clone());
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
                data.application_id = Some(application_id.clone());
                CommandName::Running
            }
            Self::Remove { application_id } => {
                data.application_id = Some(application_id.clone());
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
            // Clap's diagnostic can echo supplied argument values.
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
        Command::Uninstall { purge } => {
            let status = tokio::process::Command::new("bash")
                .arg("-c")
                .arg(include_str!("../scripts/uninstall.sh"))
                .arg("sl-runner uninstall")
                .arg(if purge { "--purge" } else { "" })
                .stdout(std::process::Stdio::from(std::io::stderr()))
                .status()
                .await?;
            ensure!(status.success(), "uninstall failed; see stderr for details");
            Ok(CommandData::Uninstall {
                message: "runner uninstalled",
                purged: purge,
            })
        }
        Command::Health => {
            let output = tokio::process::Command::new("systemctl")
                .args(["is-active", "sl-runner.service"])
                .output()
                .await
                .context("check sl-runner.service")
                .context(ErrorCode::HealthCheckFailed)?;
            let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !output.status.success() || state != "active" {
                let detail = if state.is_empty() {
                    String::from_utf8_lossy(&output.stderr).trim().to_owned()
                } else {
                    state
                };
                return Err(anyhow::anyhow!("runner service is not active: {detail}")
                    .context(ErrorCode::HealthCheckFailed));
            }
            Ok(CommandData::Health {
                service: "sl-runner.service",
                running: true,
            })
        }
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
            env_stdin,
        } => {
            let deployment = Deployment {
                deployment_id,
                image,
                application_id,
                hostname,
                port,
                env: if env_stdin {
                    parse_env(std::io::stdin().lock()).context(ErrorCode::InvalidArgument)?
                } else {
                    BTreeMap::new()
                },
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
            let deployments = deployment::running(&application_id).await?;
            Ok(CommandData::Running {
                application_id,
                deployments,
            })
        }
        Command::Remove { application_id } => {
            deployment::remove(&application_id)
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

fn parse_env(reader: impl std::io::Read) -> Result<BTreeMap<String, String>> {
    struct EnvironmentVisitor;

    impl<'de> Visitor<'de> for EnvironmentVisitor {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON object with unique keys and string values")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut env = BTreeMap::new();
            while let Some((name, value)) = map.next_entry::<String, String>()? {
                if env.insert(name, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate environment variable name",
                    ));
                }
            }
            Ok(env)
        }
    }

    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    // JSON errors can include input values, so never expose the original error.
    let invalid = || {
        anyhow::anyhow!(
            "environment stdin must be a JSON object with unique keys and string values"
        )
    };
    let env = deserializer
        .deserialize_map(EnvironmentVisitor)
        .map_err(|_| invalid())?;
    deserializer.end().map_err(|_| invalid())?;
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_preserves_string_values() {
        let expected = BTreeMap::from([
            ("EMPTY".to_owned(), String::new()),
            ("TOKEN".to_owned(), "line one\n\"$value=é\"".to_owned()),
        ]);
        let json = serde_json::to_vec(&expected).unwrap();
        assert_eq!(parse_env(json.as_slice()).unwrap(), expected);
        assert!(parse_env(b"{}".as_slice()).unwrap().is_empty());
    }

    #[test]
    fn environment_rejects_invalid_input_without_echoing_secrets() {
        for input in [
            r#"{"SECRET":"hidden","SECRET":"other"}"#,
            r#"{"SECRET":123}"#,
            r#"{"SECRET":null}"#,
            r#"{"SECRET":true}"#,
            r#"{"SECRET":{"hidden":"value"}}"#,
            r#"{"SECRET":"hidden""#,
            r#"{"SECRET":"hidden"} trailing"#,
            r#"["hidden"]"#,
            "",
        ] {
            let error = parse_env(input.as_bytes()).unwrap_err().to_string();
            assert_eq!(
                error,
                "environment stdin must be a JSON object with unique keys and string values"
            );
        }
    }

    #[test]
    fn submit_environment_is_opt_in_and_rejects_old_arguments() {
        let args = [
            "sl-runner",
            "submit",
            "--deployment-id",
            "00000000-0000-4000-8000-000000000001",
            "--application-id",
            "app",
            "--image",
            "registry/app:latest",
        ];
        assert!(matches!(
            Cli::try_parse_from(args).unwrap().command,
            Command::Submit {
                env_stdin: false,
                ..
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(args.into_iter().chain(["--env-stdin"]))
                .unwrap()
                .command,
            Command::Submit {
                env_stdin: true,
                ..
            }
        ));
        assert!(Cli::try_parse_from(args.into_iter().chain(["--env", "SECRET=hidden"])).is_err());
    }
}
