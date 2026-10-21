use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub api_url: Url,
    pub node_id: Uuid,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
    pub server_ca: PathBuf,
    pub state_dir: PathBuf,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self> {
        let contents = tokio::fs::read(path)
            .await
            .context("read runner configuration")?;
        let config: Self =
            serde_json::from_slice(&contents).context("parse runner configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let url = &self.api_url;
        ensure!(
            url.scheme() == "https" && url.host_str().is_some(),
            "api_url must use HTTPS"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "api_url must be an origin without credentials, path, query, or fragment"
        );
        ensure!(!self.node_id.is_nil(), "node_id must not be nil");
        for path in [
            &self.client_cert,
            &self.client_key,
            &self.server_ca,
            &self.state_dir,
        ] {
            ensure!(path.is_absolute(), "configuration paths must be absolute");
        }
        Ok(())
    }
}
