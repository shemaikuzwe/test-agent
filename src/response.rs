use std::{
    fmt,
    io::{self, Write},
};

use chrono::{SecondsFormat, Utc};
use clap::ValueEnum;
use serde::Serialize;
use uuid::Uuid;

use crate::deployment::types::{DeploymentStatus, JobState, RunningDeployment};

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum CommandName {
    Upgrade,
    Migrate,
    Submit,
    Rollback,
    Wait,
    Status,
    Running,
    Remove,
    Daemon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    DeploymentNotFound,
    DeploymentFailed,
    InvalidRollbackTarget,
    ApplicationNotFound,
    DatabaseError,
    DockerError,
    InternalError,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ErrorCode {}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
}

impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        let code = if matches!(
            error.downcast_ref::<diesel::result::Error>(),
            Some(diesel::result::Error::NotFound)
        ) {
            ErrorCode::DeploymentNotFound
        } else {
            error
                .downcast_ref::<ErrorCode>()
                .copied()
                .unwrap_or(ErrorCode::InternalError)
        };
        Self {
            code,
            message: format!("{error:#}"),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status")]
pub enum Response<T> {
    #[serde(rename = "SUCCESS")]
    Success {
        timestamp: String,
        command: Option<CommandName>,
        data: T,
    },
    #[serde(rename = "ERROR")]
    Error {
        timestamp: String,
        command: Option<CommandName>,
        data: T,
        error: CommandError,
    },
}

impl<T: Serialize> Response<T> {
    pub fn success(command: CommandName, data: T) -> Self {
        Self::Success {
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            command: Some(command),
            data,
        }
    }

    pub fn failure(command: Option<CommandName>, data: T, error: CommandError) -> Self {
        Self::Error {
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            command,
            data,
            error,
        }
    }

    pub fn write(&self, mut writer: impl Write) -> io::Result<()> {
        // Serialize before writing, so serialization errors cannot leave partial JSON.
        let json = serde_json::to_vec(self)?;
        writer.write_all(&json)?;
        writer.write_all(b"\n")
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum CommandData {
    Upgrade {
        message: &'static str,
    },
    Migrate {
        message: &'static str,
    },
    Submit {
        message: &'static str,
        deployment_id: Uuid,
        queued: bool,
    },
    Rollback {
        message: &'static str,
        deployment_id: Uuid,
        target_deployment_id: Uuid,
        queued: bool,
    },
    Wait {
        message: &'static str,
        deployment_id: Uuid,
        state: JobState,
    },
    Status(DeploymentStatus),
    Running {
        application_id: Uuid,
        deployments: Vec<RunningDeployment>,
    },
    Remove {
        message: &'static str,
        application_id: Uuid,
        removal_requested: bool,
    },
    Daemon {
        message: &'static str,
        reason: &'static str,
    },
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<JobState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<&'static str>,
}
