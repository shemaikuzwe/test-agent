use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use diesel::{prelude::*, sqlite::SqliteConnection};
use diesel_async::{
    AsyncConnection, AsyncMigrationHarness, RunQueryDsl,
    sync_connection_wrapper::SyncConnectionWrapper,
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use uuid::Uuid;

use crate::deployment::types::{Deployment, JobState};

mod schema;

use self::schema::jobs;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = jobs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct JobRecord {
    pub deployment_id: String,
    pub state: String,
    pub image: String,
    pub error: Option<String>,
    pub updated_at: i64,
    pub created_at: i64,
    pub application_id: String,
    #[diesel(select_expression = jobs::hostname.assume_not_null())]
    pub hostname: String,
    #[diesel(select_expression = jobs::port.assume_not_null())]
    pub port: i32,
}

impl JobRecord {
    pub fn job_state(&self) -> Result<JobState> {
        JobState::parse(&self.state)
    }

    pub fn deployment_uuid(&self) -> Result<Uuid> {
        self.deployment_id
            .parse()
            .context("invalid stored deployment ID")
    }

    pub fn deployment(&self) -> Result<Deployment> {
        Ok(Deployment {
            deployment_id: self.deployment_uuid()?,
            application_id: self
                .application_id
                .parse()
                .context("invalid stored application ID")?,
            image: self.image.clone(),
            hostname: self.hostname.clone(),
            port: self.port.try_into().context("invalid stored port")?,
        })
    }
}

pub struct Store {
    connection: SyncConnectionWrapper<SqliteConnection>,
}

impl Store {
    pub async fn open(directory: &Path) -> Result<Self> {
        let path = directory.join("runner.db");
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)
            .context("create runner database")?;
        let connection = SyncConnectionWrapper::<SqliteConnection>::establish(
            path.to_str().context("state path must be valid UTF-8")?,
        )
        .await
        .context("open runner database")?;
        let mut store = Self { connection };
        // `submit` and the daemon use separate processes: let them wait on each
        // other's writes instead of failing with SQLITE_BUSY.
        for pragma in ["PRAGMA busy_timeout = 5000", "PRAGMA journal_mode = WAL"] {
            diesel::sql_query(pragma)
                .execute(&mut store.connection)
                .await
                .context("configure runner database")?;
        }
        Ok(store)
    }

    pub async fn migrate(directory: &Path) -> Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .context("create runner state directory")?;
        let store = Self::open(directory).await?;
        let mut migrations = AsyncMigrationHarness::new(store.connection);
        migrations
            .run_pending_migrations(MIGRATIONS)
            .map_err(|error| anyhow::anyhow!("apply database migrations: {error}"))?;
        Ok(())
    }

    pub async fn enqueue_job(&mut self, deployment: &Deployment) -> Result<bool> {
        let now = now()?;
        let inserted = diesel::insert_or_ignore_into(jobs::table)
            .values((
                jobs::deployment_id.eq(deployment.deployment_id.to_string()),
                jobs::image.eq(&deployment.image),
                jobs::application_id.eq(deployment.application_id.to_string()),
                jobs::hostname.eq(&deployment.hostname),
                jobs::port.eq(i32::from(deployment.port)),
                jobs::state.eq(JobState::Pending.as_str()),
                jobs::updated_at.eq(now),
                jobs::created_at.eq(now),
            ))
            .execute(&mut self.connection)
            .await?;
        Ok(inserted == 1)
    }

    pub async fn get_next_job(&mut self) -> Result<Option<JobRecord>> {
        let next = jobs::table
            .filter(jobs::state.eq(JobState::Pending.as_str()))
            .order((jobs::created_at.asc(), jobs::deployment_id.asc()))
            .select(JobRecord::as_select())
            .first(&mut self.connection)
            .await
            .optional()?;
        let Some(job) = next else {
            return Ok(None);
        };
        let claimed = diesel::update(
            jobs::table
                .find(&job.deployment_id)
                .filter(jobs::state.eq(JobState::Pending.as_str())),
        )
        .set((
            jobs::state.eq(JobState::Running.as_str()),
            jobs::updated_at.eq(now()?),
        ))
        .execute(&mut self.connection)
        .await?;
        if claimed == 0 {
            return Ok(None);
        }
        self.get_job(job.deployment_uuid()?).await.map(Some)
    }

    pub async fn requeue_interrupted_job(&mut self) -> Result<usize> {
        Ok(
            diesel::update(jobs::table.filter(jobs::state.eq(JobState::Running.as_str())))
                .set((
                    jobs::state.eq(JobState::Pending.as_str()),
                    jobs::updated_at.eq(now()?),
                ))
                .execute(&mut self.connection)
                .await?,
        )
    }

    pub async fn get_job(&mut self, id: Uuid) -> Result<JobRecord> {
        jobs::table
            .find(id.to_string())
            .select(JobRecord::as_select())
            .first(&mut self.connection)
            .await
            .context("read deployment state")
    }

    pub async fn complete_job(
        &mut self,
        id: Uuid,
        state: JobState,
        error: Option<&str>,
    ) -> Result<()> {
        ensure!(
            matches!(
                state,
                JobState::Prepared | JobState::Deployed | JobState::Failed
            ),
            "completion requires a terminal job state"
        );
        diesel::update(jobs::table.find(id.to_string()))
            .set((
                jobs::state.eq(state.as_str()),
                jobs::error.eq(error),
                jobs::updated_at.eq(now()?),
            ))
            .execute(&mut self.connection)
            .await?;
        Ok(())
    }
}

fn now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}