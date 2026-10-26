use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, process::Command, time};

use crate::response::ErrorCode;

use super::types::{Deployment, RunningDeployment, validate_application_id};

const DEPLOYMENT_LABEL: &str = "sl-runner.deployment-id";
const STARTUP_WINDOW_SECONDS: i64 = 10;

fn stack_spec(deployment: &Deployment) -> Value {
    json!({
        "version": "3.8",
        "services": {
            "app": {
                "image": deployment.image,
                // Compose interprets dollars even in JSON input. Escape literal values.
                "environment": deployment.env.iter().map(|(name, value)| {
                    (name.clone(), value.replace('$', "$$"))
                }).collect::<std::collections::BTreeMap<_, _>>(),
                "labels": {
                    DEPLOYMENT_LABEL: deployment.deployment_id.to_string(),
                    "sl-runner.application-id": deployment.application_id.to_string()
                },
                "logging": {
                    "driver": "json-file",
                    "options": { "max-size": "10m", "max-file": "3" }
                },
                "networks": ["sl-proxy"],
                "deploy": {
                    "replicas": 1,
                    "labels": {
                        "caddy": format!("http://{}", deployment.hostname),
                        "caddy.reverse_proxy": format!("{{{{upstreams {}}}}}", deployment.port),
                    },
                    "restart_policy": { "condition": "any" },
                    "update_config": {
                        "parallelism": 1,
                        "order": "start-first",
                        "failure_action": "rollback"
                    },
                    "rollback_config": { "parallelism": 1, "order": "start-first" }
                }
            }
        },
        "networks": { "sl-proxy": { "external": true } }
    })
}

pub(crate) async fn deploy(deployment: &Deployment) -> Result<()> {
    let stack = format!("sl-{}", deployment.application_id);
    let service_name = format!("{stack}_app");
    let spec = serde_json::to_vec(&stack_spec(deployment))?;
    // Digests are already supplied by the backend; registry resolution is unnecessary.
    docker(
        &[
            "stack",
            "deploy",
            "--with-registry-auth",
            "--compose-file",
            "-",
            "--resolve-image",
            "never",
            &stack,
        ],
        Some(&spec),
    )
    .await?;

    loop {
        let output = docker(&["service", "inspect", &service_name], None).await?;
        let services: Vec<Value> =
            serde_json::from_slice(&output).context("parse service inspection")?;
        let service = services.first().context("Docker returned no service")?;
        let tasks = inspect_tasks(&service_name).await?;
        if converged(service, &tasks, deployment, Utc::now())? {
            return Ok(());
        }
        // The worker bounds the entire deployment, including Docker commands, to 300 seconds.
        time::sleep(Duration::from_secs(2)).await;
    }
}

async fn inspect_tasks(service_name: &str) -> Result<Vec<Value>> {
    for attempt in 0..3 {
        let output = docker(
            &["service", "ps", "--quiet", "--no-trunc", service_name],
            None,
        )
        .await?;
        let ids = std::str::from_utf8(&output).context("parse task IDs")?;
        let ids: Vec<&str> = ids.split_whitespace().collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["inspect", "--type", "task"];
        args.extend(&ids);
        let output = docker_output(&args, None).await?;
        if output.status.success() {
            return serde_json::from_slice(&output.stdout).context("parse task inspection");
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // Swarm may prune historical tasks between listing and inspection. Discard the
        // partial result and retry only when every diagnostic names a requested task.
        let missing_tasks = !stderr.is_empty()
            && stderr.lines().all(|line| {
                line.strip_prefix("Error response from daemon: task ")
                    .and_then(|message| message.strip_suffix(" not found"))
                    .is_some_and(|id| ids.contains(&id))
            });
        ensure!(missing_tasks, "docker inspect failed: {stderr}");
        ensure!(
            attempt < 2,
            "docker inspect failed: tasks kept disappearing during 3 inspection attempts: {stderr}"
        );
    }
    unreachable!("the final inspection attempt returns a result or error")
}

pub async fn running(application_id: &str) -> Result<Vec<RunningDeployment>> {
    validate_application_id(application_id).context(ErrorCode::InvalidArgument)?;
    let service = format!("sl-{application_id}_app");
    // Listing gives a structured absence result; Docker CLI failure text is not a contract.
    let output = docker(
        &[
            "service",
            "ls",
            "--filter",
            &format!("name={service}"),
            "--format",
            "{{.Name}}",
        ],
        None,
    )
    .await
    .context(ErrorCode::DockerError)?;
    let names = std::str::from_utf8(&output).context(ErrorCode::DockerError)?;
    if !names.lines().any(|name| name == service) {
        return Err(anyhow::anyhow!("application service not found")
            .context(ErrorCode::ApplicationNotFound));
    }
    let tasks = inspect_tasks(&service)
        .await
        .context(ErrorCode::DockerError)?;
    running_deployments(&tasks).context(ErrorCode::DockerError)
}

fn running_deployments(tasks: &[Value]) -> Result<Vec<RunningDeployment>> {
    let mut deployments = Vec::new();
    for task in tasks {
        // Include tasks still running during start-first updates, even if scheduled to stop.
        if task.pointer("/Status/State").and_then(Value::as_str) != Some("running") {
            continue;
        }
        let container = task
            .pointer("/Spec/ContainerSpec")
            .context("task has no container specification")?;
        let deployment = RunningDeployment {
            deployment_id: container
                .pointer("/Labels/sl-runner.deployment-id")
                .and_then(Value::as_str)
                .context("running task has no deployment ID")?
                .parse()
                .context("invalid running deployment ID")?,
            image: container
                .get("Image")
                .and_then(Value::as_str)
                .context("running task has no image")?
                .to_owned(),
        };
        if !deployments.contains(&deployment) {
            deployments.push(deployment);
        }
    }
    Ok(deployments)
}

pub async fn remove(application_id: &str) -> Result<()> {
    validate_application_id(application_id).context(ErrorCode::InvalidArgument)?;
    let stack = format!("sl-{application_id}");
    docker(&["stack", "rm", &stack], None).await?;
    tracing::info!(%application_id, "application stack removal requested");
    Ok(())
}

async fn docker(args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let output = docker_output(args, input).await?;
    ensure!(
        output.status.success(),
        "docker {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

async fn docker_output(args: &[&str], input: Option<&[u8]>) -> Result<std::process::Output> {
    let mut child = Command::new("docker")
        .args(args)
        .env("DOCKER_HOST", "unix:///var/run/docker.sock")
        .env_remove("DOCKER_CONTEXT")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start Docker CLI")?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("open Docker stdin")?;
        stdin
            .write_all(input)
            .await
            .context("write stack specification")?;
        // Dropping stdin sends EOF before waiting for Docker.
    }
    child
        .wait_with_output()
        .await
        .context("wait for Docker CLI")
}

fn requested_container(container: &Value, deployment: &Deployment) -> bool {
    container.get("Image").and_then(Value::as_str) == Some(deployment.image.as_str())
        && container
            .get("Labels")
            .and_then(|labels| labels.get(DEPLOYMENT_LABEL))
            .and_then(Value::as_str)
            == Some(deployment.deployment_id.to_string().as_str())
}

fn converged(
    service: &Value,
    tasks: &[Value],
    deployment: &Deployment,
    now: DateTime<Utc>,
) -> Result<bool> {
    let update = service
        .pointer("/UpdateStatus/State")
        .and_then(Value::as_str);
    if let Some(
        state @ ("paused" | "rollback_started" | "rollback_paused" | "rollback_completed"),
    ) = update
    {
        bail!(
            "service rollout {state}: {}",
            service
                .pointer("/UpdateStatus/Message")
                .and_then(Value::as_str)
                .unwrap_or("deployment did not converge")
        );
    }
    let container = service
        .pointer("/Spec/TaskTemplate/ContainerSpec")
        .context("service has no container specification")?;
    ensure!(
        requested_container(container, deployment),
        "service no longer targets this deployment"
    );
    let matching: Vec<&Value> = tasks
        .iter()
        .filter(|task| {
            task.pointer("/Spec/ContainerSpec")
                .is_some_and(|container| requested_container(container, deployment))
        })
        .collect();
    // A replacement must not hide a task that already exited during startup.
    for task in &matching {
        if let Some(
            state @ ("rejected" | "failed" | "complete" | "shutdown" | "orphaned" | "remove"),
        ) = task.pointer("/Status/State").and_then(Value::as_str)
        {
            let exit = task
                .pointer("/Status/ContainerStatus/ExitCode")
                .and_then(Value::as_i64)
                .map(|code| format!(" (exit code {code})"))
                .unwrap_or_default();
            bail!(
                "deployment task {state}{exit}: {}. Check runtime logs for more details.",
                task.pointer("/Status/Err")
                    .and_then(Value::as_str)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("container exited before startup completed")
            );
        }
    }
    for task in matching {
        if task["DesiredState"] != "running"
            || task.pointer("/Status/State").and_then(Value::as_str) != Some("running")
        {
            continue;
        }
        // This timestamp belongs to this task's transition to running, not its creation.
        let timestamp = task
            .pointer("/Status/Timestamp")
            .and_then(Value::as_str)
            .context("running task has no state timestamp")?;
        let running_since = DateTime::parse_from_rfc3339(timestamp)
            .context("running task has an invalid state timestamp")?;
        if now.signed_duration_since(running_since).num_seconds() >= STARTUP_WINDOW_SECONDS {
            return Ok(update.is_none() || update == Some("completed"));
        }
    }
    Ok(false)
}
