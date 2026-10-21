use std::time::Duration;

use anyhow::Result;
use tokio::{sync::watch, time};

use crate::{api::Api, database::Store, deployment};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const JOB_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(api: &Api, store: &mut Store, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let requeued = store.requeue_interrupted_job().await?;
    if requeued > 0 {
        tracing::warn!(requeued, "requeued jobs interrupted by a previous run");
    }
    tracing::info!("worker started");
    while !*shutdown.borrow() {
        if !drain(api, store, &shutdown).await? {
            tokio::select! {
                _ = time::sleep(POLL_INTERVAL) => {}
                _ = shutdown.changed() => {}
            }
        }
    }
    tracing::info!("worker stopped");
    Ok(())
}

async fn drain(api: &Api, store: &mut Store, shutdown: &watch::Receiver<bool>) -> Result<bool> {
    let mut worked = false;
    while !*shutdown.borrow() {
        let Some(job) = store.get_next_job().await? else {
            break;
        };
        worked = true;
        let id = job.deployment_uuid()?;
        let deployment = job.deployment()?;
        match time::timeout(
            JOB_TIMEOUT,
            deployment::process_request(api, store, deployment),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                tracing::warn!(deployment_id = %id, "deployment timed out");
                store
                    .complete_job(
                        id,
                        deployment::types::JobState::Failed,
                        Some("deployment timed out"),
                    )
                    .await?;
                deployment::update_status(api, store, id).await?;
            }
        }
    }
    Ok(worked)
}
