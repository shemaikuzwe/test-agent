mod swarm;
pub mod types;

pub(crate) use swarm::deploy;
pub use swarm::{remove, running};

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
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

pub async fn rollback(store: &mut Store, deployment_id: Uuid) -> Result<Uuid> {
    let target = store
        .get_job(deployment_id)
        .await
        .with_context(|| format!("read rollback deployment {deployment_id}"))?;
    ensure!(
        target.state == JobState::Deployed,
        "rollback target must be a successful deployment"
    );
    let mut deployment = target.deployment;
    let new_id = Uuid::new_v4();
    deployment.deployment_id = new_id;
    submit(store, deployment).await?;
    Ok(new_id)
}

pub async fn wait(store: &mut Store, id: Uuid) -> Result<()> {
    loop {
        let job = store
            .get_job(id)
            .await
            .with_context(|| format!("read deployment {id} while waiting"))?;
        match job.state {
            JobState::Deployed => {
                tracing::info!(deployment_id = %id, "deployment deployed");
                return Ok(());
            }
            JobState::Failed => bail!(
                "deployment {id} failed: {}",
                job.error.as_deref().unwrap_or("no failure reason recorded")
            ),
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
        state: job.state.as_str().to_owned(),
        image: job.deployment.image,
        error: job.error,
        created_at: job.created_at,
        updated_at: job.updated_at,
    })
}
