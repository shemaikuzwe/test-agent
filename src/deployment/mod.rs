mod swarm;
pub mod types;

pub(crate) use swarm::deploy;
pub use swarm::{remove, running};

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time;
use uuid::Uuid;

use crate::{database::Store, response::ErrorCode};
use types::{Deployment, JobState};

pub async fn submit(store: &mut Store, deployment: Deployment) -> Result<bool> {
    let deployment_id = deployment.deployment_id;
    deployment.validate().context(ErrorCode::InvalidArgument)?;
    let queued = store
        .enqueue_job(&deployment)
        .await
        .context(ErrorCode::DatabaseError)?;
    if queued {
        tracing::info!(deployment_id = %deployment_id, "deployment queued");
    } else {
        tracing::info!(deployment_id = %deployment_id, "deployment already queued");
    }
    Ok(queued)
}

pub async fn rollback(store: &mut Store, deployment_id: Uuid) -> Result<Uuid> {
    let target = store
        .get_job(deployment_id)
        .await
        .with_context(|| format!("read rollback deployment {deployment_id}"))?;
    if target.state != JobState::Deployed {
        return Err(
            anyhow::anyhow!("rollback target must be a successful deployment")
                .context(ErrorCode::InvalidRollbackTarget),
        );
    }
    let mut deployment = target.deployment;
    let new_id = Uuid::new_v4();
    deployment.deployment_id = new_id;
    submit(store, deployment).await?;
    Ok(new_id)
}

pub async fn wait(store: &mut Store, id: Uuid) -> Result<types::DeploymentStatus> {
    loop {
        let deployment = status(store, id).await?;
        match deployment.state {
            JobState::Deployed | JobState::Failed => return Ok(deployment),
            JobState::Pending | JobState::Running => {
                time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

pub async fn status(store: &mut Store, id: Uuid) -> Result<types::DeploymentStatus> {
    let job = store.get_job(id).await?;
    Ok(types::DeploymentStatus {
        deployment_id: job.deployment.deployment_id,
        application_id: job.deployment.application_id,
        state: job.state,
        image: job.deployment.image,
        error: job.error,
        created_at: job.created_at,
        updated_at: job.updated_at,
    })
}
