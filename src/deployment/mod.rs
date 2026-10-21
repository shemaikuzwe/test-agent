mod swarm;
pub mod types;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::time;
use uuid::Uuid;

use crate::database::Store;
use types::{Deployment, JobState};

pub async fn submit(store: &mut Store, deployment: Deployment) -> Result<bool> {
    let deployment_id = deployment.deployment_id;
    deployment.validate()?;
    let queued = store.enqueue_job(&deployment).await?;
    if queued {
        tracing::info!(deployment_id = %deployment_id, "deployment queued");
    } else {
        tracing::info!(deployment_id = %deployment_id, "deployment already queued");
    }
    Ok(queued)
}

pub async fn wait(store: &mut Store, id: Uuid) -> Result<()> {
    loop {
        let job = store
            .get_job(id)
            .await
            .with_context(|| format!("read deployment {id} while waiting"))?;
        match job.job_state()? {
            JobState::Deployed => {
                tracing::info!(deployment_id = %id, "deployment deployed");
                return Ok(());
            }
            JobState::Failed => bail!(
                "deployment {id} failed: {}",
                job.error.as_deref().unwrap_or("no failure reason recorded")
            ),
            JobState::Pending | JobState::Running | JobState::Prepared => {
                time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

pub async fn process_request(store: &mut Store, deployment: Deployment) -> Result<()> {
    let id = deployment.deployment_id;
    tracing::info!(deployment_id = %id, "processing deployment");

    let result = async {
        deployment.validate()?;
        swarm::deploy(&deployment).await
    }
    .await;
    match result {
        Ok(()) => {
            store.complete_job(id, JobState::Deployed, None).await?;
            tracing::info!(deployment_id = %id, "deployment deployed");
        }
        Err(error) => {
            store
                .complete_job(id, JobState::Failed, Some(&error.to_string()))
                .await?;
            tracing::warn!(deployment_id = %id, error = %error, "deployment failed");
        }
    }
    Ok(())
}
