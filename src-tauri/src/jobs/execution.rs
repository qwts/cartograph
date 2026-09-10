//! Atomic attempt metadata and fenced lifecycle operations (SPEC-07).

use super::{Job, JobStore, JobTransitionError, job_row};
use crate::job_execution::{
    ExecutionIdentity, ExecutionNamespace, ExecutionReservation, JobExecution, JobLockTarget,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use std::sync::Arc;

const META_DDL: &str = "CREATE TABLE job_execution_meta (version INTEGER PRIMARY KEY CHECK(version = 1), namespace TEXT NOT NULL) STRICT";
const ATTEMPTS_DDL: &str = "CREATE TABLE job_attempts (job_id INTEGER PRIMARY KEY, generation INTEGER NOT NULL CHECK(generation >= 0), owner TEXT, CHECK((generation = 0 AND owner IS NULL) OR (generation > 0 AND owner IS NOT NULL))) STRICT";
pub(super) const JOB_SELECT: &str = "SELECT j.id, j.kind, j.status, j.stage, j.progress, j.error, j.artifacts, j.created_at, j.updated_at, a.generation, CASE WHEN typeof(a.owner) = 'text' AND length(CAST(a.owner AS BLOB)) = 32 THEN a.owner ELSE NULL END, a.job_id, a.owner IS NULL FROM jobs j LEFT JOIN job_attempts a ON a.job_id = j.id";

#[derive(Clone, Copy)]
pub(crate) enum ClaimMode {
    StartQueued,
    RetryTerminal,
}

pub(crate) struct ClaimPlan {
    job: Job,
    attempt: Attempt,
    target: JobLockTarget,
}

impl ClaimPlan {
    pub(crate) fn job(&self) -> &Job {
        &self.job
    }
    pub(crate) fn lock_target(&self) -> &JobLockTarget {
        &self.target
    }
}

pub(crate) struct RecoveryCandidate {
    plan: ClaimPlan,
}

impl RecoveryCandidate {
    pub(crate) fn id(&self) -> i64 {
        self.plan.job.id
    }
    pub(crate) fn lock_target(&self) -> &JobLockTarget {
        &self.plan.target
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExecutionCheck {
    Running,
    Cancelled,
    Terminal,
}

#[derive(Debug)]
pub(crate) enum ExecutionUpdate {
    Applied(Job),
    Unchanged(Job),
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Attempt {
    generation: i64,
    owner: Option<String>,
}

pub(super) fn one(rows: usize) -> rusqlite::Result<()> {
    if rows == 1 {
        Ok(())
    } else {
        Err(rusqlite::Error::InvalidQuery)
    }
}

pub(super) fn missing(error: rusqlite::Error) -> JobTransitionError {
    match error {
        rusqlite::Error::QueryReturnedNoRows => JobTransitionError::Missing,
        error => JobTransitionError::Store(error),
    }
}

pub(super) fn is_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn initialize(
    conn: &mut Connection,
    path: &Path,
) -> rusqlite::Result<ExecutionNamespace> {
    let store_path = dunce::canonicalize(path).map_err(|_| rusqlite::Error::InvalidQuery)?;
    use cap_fs_ext::MetadataExt;
    if std::fs::metadata(&store_path)
        .map_err(|_| rusqlite::Error::InvalidQuery)?
        .nlink()
        != 1
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('job_execution_meta', 'job_attempts')",
        [],
        |r| r.get(0),
    )?;
    if count == 0 {
        tx.execute_batch(META_DDL)?;
        tx.execute_batch(ATTEMPTS_DDL)?;
        one(tx.execute("INSERT INTO job_execution_meta (version, namespace) VALUES (1, lower(hex(randomblob(16))))", [])?)?;
    }
    let namespace = ExecutionNamespace {
        value: read_namespace(&tx)?,
        store_path,
    };
    validate(&tx, &namespace)?;
    tx.commit()?;
    Ok(namespace)
}

fn read_namespace(conn: &Connection) -> rusqlite::Result<String> {
    for (name, expected) in [
        ("job_execution_meta", META_DDL),
        ("job_attempts", ATTEMPTS_DDL),
    ] {
        let ddl: Option<String> = conn.query_row("SELECT CASE WHEN type = 'table' AND length(CAST(sql AS BLOB)) <= 1024 THEN sql ELSE NULL END FROM sqlite_master WHERE name = ?1", [name], |r| r.get(0))?;
        if ddl.as_deref() != Some(expected) {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    let unexpected: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE (tbl_name IN ('job_execution_meta', 'job_attempts') AND type != 'table' AND name NOT GLOB 'sqlite_*') OR (tbl_name = 'jobs' AND type = 'trigger')", [], |r| r.get(0))?;
    let count: i64 = conn.query_row("SELECT count(*) FROM job_execution_meta", [], |r| r.get(0))?;
    if unexpected != 0 || count != 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let value: Option<String> = conn.query_row("SELECT CASE WHEN version = 1 AND typeof(namespace) = 'text' AND length(CAST(namespace AS BLOB)) = 32 THEN namespace ELSE NULL END FROM job_execution_meta", [], |r| r.get(0))?;
    value
        .filter(|v| is_token(v))
        .ok_or(rusqlite::Error::InvalidQuery)
}

pub(super) fn validate(conn: &Connection, expected: &ExecutionNamespace) -> rusqlite::Result<()> {
    if read_namespace(conn)? != expected.value {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

pub(super) fn row_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Attempt>> {
    let id: Option<i64> = row.get(11)?;
    if id.is_none() {
        return Ok(None);
    }
    let generation: i64 = row.get(9)?;
    let owner: Option<String> = row.get(10)?;
    let owner_is_null: bool = row.get(12)?;
    if generation < 0
        || (generation == 0 && !owner_is_null)
        || (generation > 0 && !owner.as_deref().is_some_and(is_token))
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    // The SQL projection bounds text before allocation. A non-NULL malformed
    // generation-zero owner is also rejected even when that projection is NULL.
    Ok(Some(Attempt { generation, owner }))
}

fn read_state(conn: &Connection, id: i64) -> rusqlite::Result<(Job, Option<Attempt>)> {
    conn.query_row(&format!("{JOB_SELECT} WHERE j.id = ?1"), [id], |row| {
        Ok((job_row(row)?, row_attempt(row)?))
    })
}

pub(super) fn read_job(conn: &Connection, id: i64) -> rusqlite::Result<Job> {
    read_state(conn, id).map(|(job, _)| job)
}

fn plan(namespace: &ExecutionNamespace, job: Job, attempt: Attempt) -> ClaimPlan {
    ClaimPlan {
        target: JobLockTarget {
            namespace: namespace.clone(),
            id: job.id,
            existing: attempt.generation > 0,
        },
        job,
        attempt,
    }
}

fn matches_plan(conn: &Connection, plan: &ClaimPlan) -> Result<bool, JobTransitionError> {
    let current = read_state(conn, plan.job.id).optional()?;
    Ok(current.is_some_and(|(job, attempt)| {
        job.kind == plan.job.kind
            && job.status == plan.job.status
            && attempt.as_ref() == Some(&plan.attempt)
    }))
}

impl JobStore {
    pub(crate) fn execution_namespace(&self) -> ExecutionNamespace {
        self.namespace.clone()
    }

    pub(crate) fn claim_plan(
        &self,
        id: i64,
        mode: ClaimMode,
    ) -> Result<ClaimPlan, JobTransitionError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let (job, attempt) = read_state(&tx, id).map_err(missing)?;
        let attempt = attempt.ok_or(JobTransitionError::LegacyUnknown)?;
        let allowed = match mode {
            ClaimMode::StartQueued => job.status == "queued" && attempt.generation == 0,
            ClaimMode::RetryTerminal => {
                matches!(job.status.as_str(), "failed" | "cancelled" | "interrupted")
            }
        };
        if !allowed {
            return Err(JobTransitionError::InvalidFrom {
                verb: match mode {
                    ClaimMode::StartQueued => "start",
                    ClaimMode::RetryTerminal => "retry",
                },
                status: job.status,
            });
        }
        tx.commit()?;
        Ok(plan(&self.namespace, job, attempt))
    }

    pub(crate) fn claim_execution(
        &mut self,
        plan: &ClaimPlan,
        reservation: ExecutionReservation,
    ) -> Result<(Job, JobExecution), JobTransitionError> {
        self.check_reservation(plan, &reservation)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        if !matches_plan(&tx, plan)? {
            return Err(JobTransitionError::StaleAttempt);
        }
        let generation = plan
            .attempt
            .generation
            .checked_add(1)
            .ok_or(JobTransitionError::GenerationOverflow)?;
        let owner: String = tx.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
        one(tx.execute("UPDATE job_attempts SET generation = ?2, owner = ?3 WHERE job_id = ?1 AND generation = ?4 AND owner IS ?5", params![plan.job.id, generation, owner, plan.attempt.generation, plan.attempt.owner])?)?;
        one(tx.execute("UPDATE jobs SET status = 'running', stage = NULL, progress = NULL, error = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1 AND kind = ?2 AND status = ?3", params![plan.job.id, plan.job.kind, plan.job.status])?)?;
        let job = read_job(&tx, plan.job.id)?;
        tx.commit()?;
        Ok((
            job,
            JobExecution {
                inner: Arc::new(ExecutionIdentity {
                    namespace: self.namespace.clone(),
                    id: plan.job.id,
                    generation,
                    owner,
                    reservation,
                }),
            },
        ))
    }

    fn check_reservation(
        &self,
        plan: &ClaimPlan,
        reservation: &ExecutionReservation,
    ) -> Result<(), JobTransitionError> {
        if plan.target.namespace != self.namespace
            || reservation.target.namespace != self.namespace
            || reservation.target.id != plan.job.id
            || reservation.target.existing != plan.target.existing
        {
            return Err(JobTransitionError::ForeignStore);
        }
        reservation.verify()
    }

    pub(crate) fn recovery_candidates(&self) -> Result<Vec<RecoveryCandidate>, JobTransitionError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let candidates = {
            let mut stmt = tx.prepare(&format!(
                "{JOB_SELECT} WHERE j.status = 'running' AND a.job_id IS NOT NULL ORDER BY j.id"
            ))?;
            let states = stmt
                .query_map([], |row| Ok((job_row(row)?, row_attempt(row)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            states
                .into_iter()
                .map(|(job, attempt)| {
                    let attempt = attempt.ok_or(JobTransitionError::InvalidMetadata)?;
                    if attempt.generation == 0 {
                        return Err(JobTransitionError::InvalidMetadata);
                    }
                    Ok(RecoveryCandidate {
                        plan: plan(&self.namespace, job, attempt),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(candidates)
    }

    pub(crate) fn recover_reserved(
        &mut self,
        candidate: &RecoveryCandidate,
        reservation: ExecutionReservation,
    ) -> Result<Option<Job>, JobTransitionError> {
        self.check_reservation(&candidate.plan, &reservation)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        if !matches_plan(&tx, &candidate.plan)? {
            tx.commit()?;
            return Ok(None);
        }
        one(tx.execute("UPDATE jobs SET status = 'interrupted', updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1 AND status = 'running' AND EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = jobs.id AND a.generation = ?2 AND a.owner = ?3)", params![candidate.id(), candidate.plan.attempt.generation, candidate.plan.attempt.owner])?)?;
        let job = read_job(&tx, candidate.id())?;
        tx.commit()?;
        Ok(Some(job))
    }

    pub(crate) fn check_execution(
        &self,
        execution: &JobExecution,
    ) -> Result<ExecutionCheck, JobTransitionError> {
        self.validate_handle(execution)?;
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let job = checked_job(&tx, execution)?;
        let check = match job.status.as_str() {
            "running" => ExecutionCheck::Running,
            "cancelled" => ExecutionCheck::Cancelled,
            "done" | "failed" => ExecutionCheck::Terminal,
            "interrupted" => return Err(JobTransitionError::Stopped),
            _ => return Err(JobTransitionError::InvalidMetadata),
        };
        tx.commit()?;
        Ok(check)
    }

    fn validate_handle(&self, execution: &JobExecution) -> Result<(), JobTransitionError> {
        if execution.inner.namespace != self.namespace {
            return Err(JobTransitionError::ForeignStore);
        }
        execution.verify()
    }

    pub(crate) fn progress_execution(
        &mut self,
        execution: &JobExecution,
        stage: &str,
        percent: f64,
    ) -> Result<ExecutionUpdate, JobTransitionError> {
        if !percent.is_finite() {
            return Err(JobTransitionError::InvalidMetadata);
        }
        self.update_execution(
            execution,
            Update::Progress(stage, percent.clamp(0.0, 100.0)),
        )
    }

    pub(crate) fn finish_execution(
        &mut self,
        execution: &JobExecution,
        artifacts: &[String],
    ) -> Result<ExecutionUpdate, JobTransitionError> {
        let json = serde_json::to_string(artifacts).expect("string vec serializes");
        self.update_execution(execution, Update::Finish(&json))
    }

    pub(crate) fn fail_execution(
        &mut self,
        execution: &JobExecution,
        error: &str,
    ) -> Result<ExecutionUpdate, JobTransitionError> {
        self.update_execution(execution, Update::Fail(error))
    }

    fn update_execution(
        &mut self,
        execution: &JobExecution,
        update: Update<'_>,
    ) -> Result<ExecutionUpdate, JobTransitionError> {
        self.validate_handle(execution)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let job = checked_job(&tx, execution)?;
        match job.status.as_str() {
            "cancelled" | "done" | "failed" => {
                tx.commit()?;
                return Ok(ExecutionUpdate::Unchanged(job));
            }
            "interrupted" => return Err(JobTransitionError::Stopped),
            "running" => {}
            _ => return Err(JobTransitionError::InvalidMetadata),
        }
        let suffix = "updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1 AND status = 'running' AND EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = jobs.id AND a.generation = ?2 AND a.owner = ?3)";
        let identity = &execution.inner;
        let changed = match update {
            Update::Progress(stage, percent) => tx.execute(&format!("UPDATE jobs SET stage = ?4, progress = ?5, {suffix}"), params![identity.id, identity.generation, identity.owner, stage, percent])?,
            Update::Finish(artifacts) => tx.execute(&format!("UPDATE jobs SET status = 'done', progress = 100.0, error = NULL, artifacts = ?4, {suffix}"), params![identity.id, identity.generation, identity.owner, artifacts])?,
            Update::Fail(error) => tx.execute(&format!("UPDATE jobs SET status = 'failed', error = ?4, {suffix}"), params![identity.id, identity.generation, identity.owner, error])?,
        };
        one(changed)?;
        let job = read_job(&tx, execution.id())?;
        tx.commit()?;
        Ok(ExecutionUpdate::Applied(job))
    }
}

enum Update<'a> {
    Progress(&'a str, f64),
    Finish(&'a str),
    Fail(&'a str),
}

fn checked_job(conn: &Connection, execution: &JobExecution) -> Result<Job, JobTransitionError> {
    let (job, attempt) = read_state(conn, execution.id()).map_err(missing)?;
    if !attempt.is_some_and(|a| {
        a.generation == execution.inner.generation
            && a.owner.as_deref() == Some(execution.inner.owner.as_str())
    }) {
        return Err(JobTransitionError::StaleAttempt);
    }
    Ok(job)
}
