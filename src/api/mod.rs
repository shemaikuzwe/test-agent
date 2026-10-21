use std::time::Duration;

use anyhow::{Context, Result, ensure};
use reqwest::{Certificate, Client, Identity, StatusCode, Url, redirect::Policy};
use thiserror::Error;
use uuid::Uuid;

use crate::{config::Config, deployment::types::StatusUpdate};

const RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("hosting API transport failure")]
    Transport,
    #[error("hosting API returned HTTP {0}")]
    Status(StatusCode),
    #[error("hosting API response exceeds 64 KiB")]
    ResponseTooLarge,
}

pub struct Api {
    client: Client,
    base_url: Url,
}

impl Api {
    pub async fn new(config: &Config) -> Result<Self> {
        config.validate()?;
        let mut identity = tokio::fs::read(&config.client_cert)
            .await
            .context("read client certificate")?;
        identity.push(b'\n');
        identity.extend(
            tokio::fs::read(&config.client_key)
                .await
                .context("read client key")?,
        );
        let ca = tokio::fs::read(&config.server_ca)
            .await
            .context("read server CA")?;
        let certificates = Certificate::from_pem_bundle(&ca).context("parse server CA")?;
        ensure!(
            !certificates.is_empty(),
            "server CA bundle contains no certificates"
        );
        let client = Client::builder()
            .identity(Identity::from_pem(&identity).context("parse client identity")?)
            .tls_certs_only(certificates)
            .redirect(Policy::none())
            .https_only(true)
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("sl-runner/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("configure authenticated HTTP client")?;
        Ok(Self {
            client,
            base_url: config.api_url.clone(),
        })
    }

    pub async fn update_status(&self, id: Uuid, status: &StatusUpdate) -> Result<(), ApiError> {
        // Both the origin and path are controlled locally, never supplied by a deployment.
        let url = self
            .base_url
            .join(&format!("internal/runner/deployments/{id}/status"))
            .expect("fixed path and UUID form a valid URL");
        let mut response = self
            .client
            .put(url)
            .json(status)
            .send()
            .await
            .map_err(|_| ApiError::Transport)?;
        if !response.status().is_success() {
            return Err(ApiError::Status(response.status()));
        }
        let mut body_len = 0;
        while let Some(chunk) = response.chunk().await.map_err(|_| ApiError::Transport)? {
            if body_len + chunk.len() > RESPONSE_LIMIT {
                return Err(ApiError::ResponseTooLarge);
            }
            body_len += chunk.len();
        }
        Ok(())
    }
}
