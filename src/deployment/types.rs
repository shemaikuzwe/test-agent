use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub deployment_id: Uuid,
    pub node_id: Uuid,
    pub image: String,
}

impl Deployment {
    pub fn validate(&self, node_id: Uuid) -> Result<()> {
        ensure!(
            !self.deployment_id.is_nil(),
            "deployment ID must not be nil"
        );
        ensure!(
            self.node_id == node_id,
            "deployment belongs to another node"
        );
        let (name, digest) = self
            .image
            .rsplit_once("@sha256:")
            .ok_or_else(|| anyhow::anyhow!("image must be pinned to a sha256 digest"))?;
        ensure!(
            !name.is_empty()
                && !name.contains('@')
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
    Prepared,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Prepared => "prepared",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "prepared" => Self::Prepared,
            "failed" => Self::Failed,
            _ => anyhow::bail!("unknown stored job state"),
        })
    }

    pub fn wire(self) -> RunnerState {
        match self {
            Self::Pending | Self::Running => RunnerState::Received,
            Self::Prepared => RunnerState::Prepared,
            Self::Failed => RunnerState::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    Received,
    Prepared,
    Failed,
}

impl RunnerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Prepared => "prepared",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusUpdate {
    pub node_id: Uuid,
    pub state: RunnerState,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
