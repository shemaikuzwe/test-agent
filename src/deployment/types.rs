use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub deployment_id: Uuid,
    pub application_id: Uuid,
    pub image: String,
    pub hostname: String,
    pub port: u16,
    pub env: BTreeMap<String, String>,
}

impl Deployment {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in &self.env {
            ensure!(
                !name.is_empty() && !name.contains(['=', '\0']),
                "environment variable name must be nonempty and contain neither = nor NUL"
            );
            ensure!(
                !value.contains('\0'),
                "environment variable value must not contain NUL"
            );
        }
        ensure!(
            !self.deployment_id.is_nil(),
            "deployment ID must not be nil"
        );
        ensure!(
            !self.application_id.is_nil(),
            "application ID must not be nil"
        );
        let hostname = &self.hostname;
        ensure!(
            hostname.len() <= 253
                && hostname.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                }),
            "hostname must be a DNS name without a scheme, port, or path"
        );
        ensure!(self.port != 0, "port must be between 1 and 65535");
        let (name, digest) = self
            .image
            .rsplit_once("@sha256:")
            .ok_or_else(|| anyhow::anyhow!("image must be pinned to a sha256 digest"))?;
        ensure!(
            !name.is_empty()
                && !name.contains(['@', '$'])
                && !name.chars().any(char::is_whitespace)
                && digest.len() == 64
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "image must be pinned to a sha256 digest"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Deployed,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Deployed => "deployed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "deployed" => Self::Deployed,
            "failed" => Self::Failed,
            _ => anyhow::bail!("unknown stored job state"),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStatus {
    pub deployment_id: Uuid,
    pub application_id: Uuid,
    pub state: String,
    pub image: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunningDeployment {
    pub deployment_id: Uuid,
    pub image: String,
}
