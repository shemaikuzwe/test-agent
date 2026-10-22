use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::{sync::watch, time};

use crate::{
    database::Store,
    deployment::{
        self,
        types::{Deployment, JobState},
    },
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const JOB_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(store: &mut Store, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let requeued = store.requeue_interrupted_job().await?;
    if requeued > 0 {
        tracing::warn!(requeued, "requeued jobs interrupted by a previous run");
    }
    tracing::info!("worker started");
    while !*shutdown.borrow() {
        if !drain(store, &shutdown).await? {
            tokio::select! {
                _ = time::sleep(POLL_INTERVAL) => {}
                _ = shutdown.changed() => {}
            }
        }
    }
    tracing::info!("worker stopped");
    Ok(())
}

async fn drain(store: &mut Store, shutdown: &watch::Receiver<bool>) -> Result<bool> {
    let mut worked = false;
    while !*shutdown.borrow() {
        let Some(job) = store.claim_next_job().await? else {
            break;
        };
        worked = true;
        process_job(store, job.deployment).await?;
    }
    Ok(worked)
}

async fn process_job(store: &mut Store, deployment: Deployment) -> Result<()> {
    let id = deployment.deployment_id;
    tracing::info!(deployment_id = %id, "processing deployment");
    let result = time::timeout(JOB_TIMEOUT, async {
        deployment.validate()?;
        deployment::deploy(&deployment).await
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!("deployment timed out")));

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
