use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub deployment_id: Uuid,
    pub application_id: String,
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
        validate_application_id(&self.application_id)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
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
    pub application_id: String,
    pub state: JobState,
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

// Application IDs become Docker stack names; accept hosting CUIDs and existing UUIDs.
pub fn validate_application_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 63
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "application ID must contain 1-63 ASCII letters, digits, hyphens or underscores"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_application_id;

    #[test]
    fn accepts_hosting_ids_and_existing_uuids() {
        assert!(validate_application_id("cm123hostingservice").is_ok());
        assert!(validate_application_id("00000000-0000-4000-8000-000000000002").is_ok());
    }

    #[test]
    fn rejects_invalid_stack_names() {
        for id in ["", "a/b", "a b", "$(whoami)", &"a".repeat(64)] {
            assert!(validate_application_id(id).is_err());
        }
    }
}
