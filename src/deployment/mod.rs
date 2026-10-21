pub mod types;

use anyhow::Result;
use uuid::Uuid;

use crate::{api::Api, database::Store};
use types::{Deployment, JobState};

pub async fn submit(
    store: &mut Store,
    deployment: Deployment,
    configured_node: Uuid,
) -> Result<bool> {
    let deployment_id = deployment.deployment_id;
    deployment.validate(configured_node)?;
    let queued = store.enqueue_job(&deployment).await?;
    if queued {
        tracing::info!(deployment_id = %deployment_id, "deployment queued");
    } else {
        tracing::info!(deployment_id = %deployment_id, "deployment already queued");
    }
    Ok(queued)
}

pub async fn process_request(api: &Api, store: &mut Store, deployment: Deployment) -> Result<()> {
    let id = deployment.deployment_id;
    tracing::info!(deployment_id = %id, "processing deployment");
    update_status(api, store, id).await?;

    match deployment.validate(deployment.node_id) {
        Ok(()) => {
            //TODO: Container execution will be added in a later milestone.
            store.complete_job(id, JobState::Prepared, None).await?;
            tracing::info!(deployment_id = %id, "deployment prepared");
        }
        Err(error) => {
            store
                .complete_job(id, JobState::Failed, Some(&error.to_string()))
                .await?;
            tracing::warn!(deployment_id = %id, error = %error, "deployment failed");
        }
    }
    update_status(api, store, id).await
}

async fn update_status(api: &Api, store: &mut Store, id: Uuid) -> Result<()> {
    let status = store.get_job(id).await?.status()?;
    if let Err(error) = api.update_status(id, &status).await {
        tracing::warn!(deployment_id = %id, error = %error, "status delivery failed");
    }
    Ok(())
}
