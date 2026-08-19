//! The durable job queue. Postgres only - PLAN.md §Recorded deviations 1.
//!
//! Shape, per PLAN.md §Postgres schema:
//!
//! * claim with `for update skip locked`, one row at a time;
//! * a lease that a heartbeat extends every 60 s while the handler runs;
//! * a reaper that reclaims **only** expired leases and never a live one;
//! * failure backs off `2^attempts` minutes; the 5th failure dead-letters the
//!   job and writes an `audit_log` row.
//!
//! Every timestamp in here is computed by PostgreSQL (`now()`), never by a
//! worker. That is what makes the queue immune to clock skew between workers -
//! edge case 8.

use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::watch;

// ---------------------------------------------------------------- config --

#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// How long a claim holds a job before the reaper may take it back.
    pub lease: Duration,
    /// How often a running handler extends its lease. Must be < `lease`.
    pub heartbeat: Duration,
    /// Failures after which a job is dead-lettered.
    pub max_attempts: i32,
    /// How long an idle worker waits before polling again.
    pub poll_interval: Duration,
    /// How often a worker sweeps expired leases.
    pub reap_interval: Duration,
    /// How long a shutting-down worker waits for its in-flight job before
    /// releasing the lease so someone else can pick it up.
    pub shutdown_grace: Duration,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(300),
            heartbeat: Duration::from_secs(60),
            max_attempts: 5,
            poll_interval: Duration::from_secs(1),
            reap_interval: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(30),
        }
    }
}

// ------------------------------------------------------------------- job --

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    /// Failures *before* this attempt. 0 the first time a job runs.
    pub attempts: i32,
}

/// What a failure did to the row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Failure {
    /// Failure count after this failure.
    pub attempts: i32,
    /// True when the job was dead-lettered and will never be claimed again.
    pub dead: bool,
    /// Exactly `run_after - now()`, measured inside the same statement, so the
    /// backoff schedule can be asserted without touching the wall clock.
    pub backoff: Duration,
}

// ----------------------------------------------------------------- queue --

#[derive(Debug, Clone)]
pub struct Queue {
    pool: PgPool,
    config: QueueConfig,
}

impl Queue {
    #[must_use]
    pub fn new(pool: PgPool, config: QueueConfig) -> Self {
        Self { pool, config }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    #[must_use]
    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    /// Put a job on the queue. `run_after` defaults to now.
    ///
    /// # Errors
    /// Returns an error if the insert fails.
    pub async fn enqueue(
        &self,
        kind: &str,
        payload: &Value,
        run_after: Option<DateTime<Utc>>,
    ) -> sqlx::Result<i64> {
        sqlx::query_scalar(
            "insert into jobs (kind, payload, run_after) \
             values ($1, $2, coalesce($3::timestamptz, now())) returning id",
        )
        .bind(kind)
        .bind(payload)
        .bind(run_after)
        .fetch_one(&self.pool)
        .await
    }

    /// Enqueue unless an identical job is already waiting.
    ///
    /// `Ok(None)` means one already was, and the caller's work is therefore
    /// already scheduled. This is what turns a burst of Gmail push
    /// notifications into one walk instead of twenty that queue behind each
    /// other's account lock.
    ///
    /// The conflict target restates the partial index's predicate verbatim so
    /// PostgreSQL can infer the index. Note what the predicate does **not**
    /// mention: `locked_by`. Excluding locked rows would let a notification
    /// enqueue while an earlier walk is still running - but `fail`, `release`
    /// and `reap_expired_leases` all clear `locked_by`, so a replacement row
    /// would collide the instant the original failed, was released at shutdown,
    /// or had its lease reaped, leaving the queue unable to record its own
    /// failures during the outage that caused them. The notification that gets
    /// suppressed instead is picked up by `sync_state.rerun_requested`.
    ///
    /// # Errors
    /// Returns an error if the insert fails.
    pub async fn enqueue_unique(
        &self,
        kind: &str,
        payload: &Value,
        run_after: Option<DateTime<Utc>>,
        dedupe_key: &str,
    ) -> sqlx::Result<Option<i64>> {
        sqlx::query_scalar(
            "insert into jobs (kind, payload, run_after, dedupe_key) \
             values ($1, $2, coalesce($3::timestamptz, now()), $4) \
             on conflict (dedupe_key) \
                 where dedupe_key is not null and done_at is null and dead_at is null \
             do nothing \
             returning id",
        )
        .bind(kind)
        .bind(payload)
        .bind(run_after)
        .bind(dedupe_key)
        .fetch_optional(&self.pool)
        .await
    }

    /// Take at most one ready job of a kind we can actually run.
    ///
    /// `for update skip locked` makes this safe for any number of concurrent
    /// workers: the row lock is held only for the length of this statement, and
    /// the lease it stamps keeps the job invisible afterwards.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub async fn claim(&self, worker: &str, kinds: &[String]) -> sqlx::Result<Option<Job>> {
        // EDGE (empty input): a worker with no handlers claims nothing, and we
        // do not bother the database to find that out.
        if kinds.is_empty() {
            return Ok(None);
        }
        sqlx::query_as::<_, Job>(
            "with ready as ( \
                 select id from jobs \
                  where done_at is null \
                    and dead_at is null \
                    and run_after <= now() \
                    and (lease_expires_at is null or lease_expires_at < now()) \
                    and kind = any($2::text[]) \
                  order by run_after \
                  for update skip locked \
                  limit 1 \
             ) \
             update jobs j \
                set locked_by = $1, \
                    lease_expires_at = now() + make_interval(secs => $3::double precision) \
               from ready \
              where j.id = ready.id \
             returning j.id, j.kind, j.payload, j.attempts",
        )
        .bind(worker)
        .bind(kinds)
        .bind(self.config.lease.as_secs_f64())
        .fetch_optional(&self.pool)
        .await
    }

    /// Push the lease out. Returns `false` when we no longer hold it - the job
    /// was reclaimed or finished by somebody else and we must stop touching it.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub async fn heartbeat(&self, id: i64, worker: &str) -> sqlx::Result<bool> {
        let extended: Option<i64> = sqlx::query_scalar(
            "update jobs \
                set lease_expires_at = now() + make_interval(secs => $3::double precision) \
              where id = $1 and locked_by = $2 \
                and done_at is null and dead_at is null \
                and lease_expires_at is not null and lease_expires_at > now() \
             returning id",
        )
        .bind(id)
        .bind(worker)
        .bind(self.config.lease.as_secs_f64())
        .fetch_optional(&self.pool)
        .await?;
        Ok(extended.is_some())
    }

    /// Mark the job done. Returns `false` if we had already lost the lease.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub async fn complete(&self, id: i64, worker: &str) -> sqlx::Result<bool> {
        let done: Option<i64> = sqlx::query_scalar(
            "update jobs \
                set done_at = now(), locked_by = null, lease_expires_at = null, error = null \
              where id = $1 and locked_by = $2 and done_at is null and dead_at is null \
             returning id",
        )
        .bind(id)
        .bind(worker)
        .fetch_optional(&self.pool)
        .await?;
        Ok(done.is_some())
    }

    /// Record a failure: back off `2^attempts` minutes and, on the
    /// `max_attempts`-th failure, dead-letter the job and write an audit row.
    ///
    /// `Ok(None)` means we no longer held the lease and touched nothing.
    ///
    /// # Errors
    /// Returns an error if the transaction fails.
    pub async fn fail(&self, id: i64, worker: &str, error: &str) -> sqlx::Result<Option<Failure>> {
        let mut tx = self.pool.begin().await?;

        // Every SET expression sees the *old* row, so `2 ^ attempts` is the
        // pre-increment count: failure 1 waits 1 min, 2 waits 2, 3 waits 4,
        // 4 waits 8, 5 waits 16 - and the 5th is also the one that dies.
        // `least(attempts, 20)` keeps the exponent inside `int` no matter what.
        let row: Option<(i32, bool, String, f64)> = sqlx::query_as(
            "update jobs \
                set attempts = attempts + 1, \
                    error = $3, \
                    locked_by = null, \
                    lease_expires_at = null, \
                    run_after = now() + make_interval(mins => (2 ^ least(attempts, 20))::int), \
                    dead_at = case when attempts + 1 >= $4 then now() else null end \
              where id = $1 and locked_by = $2 and done_at is null and dead_at is null \
             returning attempts, \
                       (dead_at is not null), \
                       kind, \
                       extract(epoch from (run_after - now()))::double precision",
        )
        .bind(id)
        .bind(worker)
        .bind(error)
        .bind(self.config.max_attempts)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((attempts, dead, kind, backoff_secs)) = row else {
            tx.rollback().await?;
            return Ok(None);
        };

        if dead {
            // Same transaction as the dead-lettering, so the audit trail can
            // never disagree with the queue.
            sqlx::query(
                "insert into audit_log (account_id, actor, action, subject) \
                 values (null, 'system', 'job_dead_letter', $1)",
            )
            .bind(json!({
                "job_id": id,
                "kind": kind,
                "attempts": attempts,
                "worker": worker,
                "error": error,
            }))
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        Ok(Some(Failure {
            attempts,
            dead,
            backoff: Duration::from_secs_f64(backoff_secs.max(0.0)),
        }))
    }

    /// Give a claimed job straight back, unchanged. Used when a worker is
    /// shutting down and would rather someone else ran it.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub async fn release(&self, id: i64, worker: &str) -> sqlx::Result<bool> {
        let released: Option<i64> = sqlx::query_scalar(
            "update jobs set locked_by = null, lease_expires_at = null \
              where id = $1 and locked_by = $2 and done_at is null and dead_at is null \
             returning id",
        )
        .bind(id)
        .bind(worker)
        .fetch_optional(&self.pool)
        .await?;
        Ok(released.is_some())
    }

    /// Unlock every job whose lease has run out.
    ///
    /// The `lease_expires_at < now()` predicate is the whole safety argument:
    /// a job whose worker is still heartbeating has a lease in the future and
    /// is invisible here. Returns how many rows were reclaimed.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub async fn reap_expired_leases(&self) -> sqlx::Result<u64> {
        let result = sqlx::query(
            "update jobs set locked_by = null, lease_expires_at = null \
              where done_at is null and dead_at is null \
                and lease_expires_at is not null and lease_expires_at < now()",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

// --------------------------------------------------------------- handlers --

/// What a handler is handed. Later phases hang the Gmail client, the LLM
/// adapters and the agent engine off here; P1 only needs the queue itself so a
/// handler can enqueue follow-up work.
#[derive(Debug, Clone)]
pub struct JobContext {
    pub queue: Queue,
}

/// One job kind's implementation.
///
/// Returning `Err` fails the job into the backoff schedule; panicking does the
/// same and is contained (the worker loop survives it).
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, job: Job, ctx: JobContext) -> anyhow::Result<()>;
}

/// `kind` -> handler. Later phases register `triage_message`, `run_agent`, and
/// friends here; a worker only ever claims kinds it can run.
#[derive(Default)]
pub struct Registry {
    handlers: HashMap<String, Arc<dyn Handler>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("kinds", &self.kinds())
            .finish()
    }
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for `kind`, replacing any previous one.
    pub fn register(&mut self, kind: impl Into<String>, handler: Arc<dyn Handler>) -> &mut Self {
        self.handlers.insert(kind.into(), handler);
        self
    }

    #[must_use]
    pub fn get(&self, kind: &str) -> Option<Arc<dyn Handler>> {
        self.handlers.get(kind).cloned()
    }

    /// Sorted, so the claim query's parameter is stable and easy to read in
    /// logs.
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self.handlers.keys().cloned().collect();
        kinds.sort();
        kinds
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

/// Wrap an async closure as a [`Handler`].
pub fn handler_fn<F, Fut>(f: F) -> Arc<dyn Handler>
where
    F: Fn(Job, JobContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    struct FnHandler<F>(F);

    #[async_trait]
    impl<F, Fut> Handler for FnHandler<F>
    where
        F: Fn(Job, JobContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        async fn handle(&self, job: Job, ctx: JobContext) -> anyhow::Result<()> {
            (self.0)(job, ctx).await
        }
    }

    Arc::new(FnHandler(f))
}

// ---------------------------------------------------------------- worker --

/// A single claim-run-report loop.
#[derive(Debug)]
pub struct Worker {
    id: String,
    queue: Queue,
    registry: Arc<Registry>,
}

impl Worker {
    #[must_use]
    pub fn new(queue: Queue, registry: Arc<Registry>) -> Self {
        Self {
            id: format!("worker-{}", uuid::Uuid::new_v4()),
            queue,
            registry,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Run until `shutdown` flips to `true` (or its sender is dropped).
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let kinds = self.registry.kinds();
        let config = self.queue.config().clone();
        tracing::info!(worker = %self.id, ?kinds, "job worker started");

        let mut next_reap = Instant::now();

        while !*shutdown.borrow() {
            if Instant::now() >= next_reap {
                match self.queue.reap_expired_leases().await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::warn!(worker = %self.id, reclaimed = n, "reclaimed expired job leases")
                    }
                    Err(error) => tracing::warn!(worker = %self.id, %error, "reaper failed"),
                }
                next_reap = Instant::now() + config.reap_interval;
            }

            match self.queue.claim(&self.id, &kinds).await {
                Ok(Some(job)) => self.execute(job, &shutdown).await,
                Ok(None) => idle(&mut shutdown, config.poll_interval).await,
                Err(error) => {
                    tracing::warn!(worker = %self.id, %error, "claim failed");
                    idle(&mut shutdown, config.poll_interval).await;
                }
            }
        }

        tracing::info!(worker = %self.id, "job worker stopped");
    }

    async fn execute(&self, job: Job, shutdown: &watch::Receiver<bool>) {
        let Some(handler) = self.registry.get(&job.kind) else {
            // Only reachable if the registry changed under us; the claim query
            // filters by kind.
            let _ = self
                .queue
                .fail(job.id, &self.id, "no handler registered for this kind")
                .await;
            return;
        };

        let job_id = job.id;
        let kind = job.kind.clone();
        let span = tracing::info_span!("job", worker = %self.id, id = job_id, kind = %kind);
        let _entered = span.enter();

        let ctx = JobContext {
            queue: self.queue.clone(),
        };
        let mut task = tokio::spawn(async move { handler.handle(job, ctx).await });
        let aborter = task.abort_handle();

        // Heartbeat while the handler runs. If the lease is gone the handler is
        // aborted immediately: another worker owns this job now and two of us
        // must not both write its effects.
        let heartbeat = tokio::spawn({
            let queue = self.queue.clone();
            let worker = self.id.clone();
            let every = self.queue.config().heartbeat;
            async move {
                loop {
                    tokio::time::sleep(every).await;
                    match queue.heartbeat(job_id, &worker).await {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::warn!(id = job_id, "lease lost; aborting handler");
                            aborter.abort();
                            return;
                        }
                        // A transient database blip is not proof the lease is
                        // gone; keep trying until it either works or the lease
                        // genuinely expires.
                        Err(error) => tracing::warn!(id = job_id, %error, "heartbeat failed"),
                    }
                }
            }
        });

        let grace = self.queue.config().shutdown_grace;
        let mut shutdown = shutdown.clone();
        let outcome = tokio::select! {
            joined = &mut task => Some(joined),
            () = async {
                while !*shutdown.borrow() {
                    if shutdown.changed().await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(grace).await;
            } => None,
        };
        heartbeat.abort();

        match outcome {
            Some(Ok(Ok(()))) => match self.queue.complete(job_id, &self.id).await {
                Ok(true) => tracing::debug!(id = job_id, "job done"),
                Ok(false) => tracing::warn!(id = job_id, "job finished but the lease was gone"),
                Err(error) => tracing::error!(id = job_id, %error, "could not mark the job done"),
            },
            Some(Ok(Err(error))) => self.report_failure(job_id, &format!("{error:#}")).await,
            Some(Err(join_error)) if join_error.is_panic() => {
                // EDGE (crash mid-step): a panicking handler is contained by
                // the spawn; the worker loop is untouched and the job just
                // fails into the normal backoff schedule.
                let detail = panic_message(join_error.into_panic());
                tracing::error!(id = job_id, %detail, "job handler panicked");
                self.report_failure(job_id, &format!("handler panicked: {detail}"))
                    .await;
            }
            // Cancelled: the heartbeat task aborted us because the lease was
            // stolen. The row belongs to another worker - do not touch it.
            Some(Err(_)) => tracing::warn!(id = job_id, "handler cancelled after losing the lease"),
            None => {
                tracing::warn!(id = job_id, "shutdown grace expired; releasing the lease");
                task.abort();
                if let Err(error) = self.queue.release(job_id, &self.id).await {
                    tracing::error!(id = job_id, %error, "could not release the lease");
                }
            }
        }
    }

    async fn report_failure(&self, job_id: i64, error: &str) {
        match self.queue.fail(job_id, &self.id, error).await {
            Ok(Some(failure)) if failure.dead => tracing::error!(
                id = job_id,
                attempts = failure.attempts,
                "job dead-lettered"
            ),
            Ok(Some(failure)) => tracing::warn!(
                id = job_id,
                attempts = failure.attempts,
                retry_in_secs = failure.backoff.as_secs(),
                "job failed; will retry"
            ),
            Ok(None) => tracing::warn!(id = job_id, "job failed but the lease was gone"),
            Err(error) => tracing::error!(id = job_id, %error, "could not record the failure"),
        }
    }
}

async fn idle(shutdown: &mut watch::Receiver<bool>, poll: Duration) {
    tokio::select! {
        _ = shutdown.changed() => {}
        () = tokio::time::sleep(poll) => {}
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_else(|| {
            payload.downcast_ref::<&'static str>().map_or_else(
                || "<non-string panic payload>".to_owned(),
                |s| (*s).to_owned(),
            )
        })
}

/// Start `count` workers sharing one registry.
#[must_use]
pub fn spawn_workers(
    queue: &Queue,
    registry: &Arc<Registry>,
    count: usize,
    shutdown: &watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    (0..count)
        .map(|_| {
            let worker = Worker::new(queue.clone(), Arc::clone(registry));
            let shutdown = shutdown.clone();
            tokio::spawn(worker.run(shutdown))
        })
        .collect()
}

// ------------------------------------------------------------------ tests --

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::test_support::{test_db, wait_for, TestDb};

    fn queue_with(db: &TestDb, config: QueueConfig) -> Queue {
        Queue::new(db.pool.clone(), config)
    }

    fn fast() -> QueueConfig {
        QueueConfig {
            lease: Duration::from_millis(500),
            heartbeat: Duration::from_millis(100),
            poll_interval: Duration::from_millis(20),
            reap_interval: Duration::from_millis(50),
            shutdown_grace: Duration::from_secs(5),
            ..QueueConfig::default()
        }
    }

    fn kinds(list: &[&str]) -> Vec<String> {
        list.iter().map(|k| (*k).to_owned()).collect()
    }

    #[tokio::test]
    async fn enqueue_then_claim_round_trips_the_payload() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        let payload = json!({"gmail_id": "18f…", "unicode": "配送のお知らせ 🚚", "n": 7});

        let id = queue
            .enqueue("triage_message", &payload, None)
            .await
            .unwrap();
        let job = queue
            .claim("w1", &kinds(&["triage_message"]))
            .await
            .unwrap()
            .expect("a job");

        assert_eq!(job.id, id);
        assert_eq!(job.kind, "triage_message");
        assert_eq!(job.payload, payload);
        assert_eq!(job.attempts, 0);
    }

    /// Edge case 1 (empty input) and 6 (boundary).
    #[tokio::test]
    async fn claim_on_an_empty_queue_returns_none() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        assert!(queue
            .claim("w1", &kinds(&["anything"]))
            .await
            .unwrap()
            .is_none());
        // A worker with no registered kinds claims nothing either.
        queue.enqueue("a", &json!({}), None).await.unwrap();
        assert!(queue.claim("w1", &[]).await.unwrap().is_none());
    }

    /// Criterion E2 / edge case 9.
    #[tokio::test]
    async fn claim_is_exclusive_under_concurrency() {
        const JOBS: usize = 64;
        const WORKERS: usize = 8;

        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        for i in 0..JOBS {
            queue
                .enqueue("race", &json!({ "i": i }), None)
                .await
                .unwrap();
        }

        let tasks: Vec<_> = (0..WORKERS)
            .map(|w| {
                let queue = queue.clone();
                tokio::spawn(async move {
                    let name = format!("w{w}");
                    let mut mine = Vec::new();
                    while let Some(job) = queue.claim(&name, &kinds(&["race"])).await.unwrap() {
                        mine.push(job.id);
                    }
                    mine
                })
            })
            .collect();

        let mut claimed = Vec::new();
        for task in tasks {
            claimed.extend(task.await.unwrap());
        }

        let unique: std::collections::BTreeSet<i64> = claimed.iter().copied().collect();
        assert_eq!(
            claimed.len(),
            unique.len(),
            "a job was claimed twice: {claimed:?}"
        );
        assert_eq!(unique.len(), JOBS, "some jobs were never claimed");
    }

    #[tokio::test]
    async fn claim_skips_jobs_whose_run_after_is_in_the_future() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        let later = Utc::now() + chrono::Duration::hours(1);
        queue
            .enqueue("later", &json!({}), Some(later))
            .await
            .unwrap();
        assert!(queue
            .claim("w1", &kinds(&["later"]))
            .await
            .unwrap()
            .is_none());

        let now = queue.enqueue("later", &json!({}), None).await.unwrap();
        let job = queue
            .claim("w1", &kinds(&["later"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.id, now);
    }

    #[tokio::test]
    async fn claim_only_takes_registered_kinds() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        queue.enqueue("run_agent", &json!({}), None).await.unwrap();
        assert!(queue
            .claim("w1", &kinds(&["triage_message"]))
            .await
            .unwrap()
            .is_none());
        assert!(queue
            .claim("w1", &kinds(&["run_agent"]))
            .await
            .unwrap()
            .is_some());
    }

    /// Criterion E5 / edge case 3 + 5.
    #[tokio::test]
    async fn expired_lease_is_reclaimable() {
        let db = test_db().await;
        let queue = queue_with(
            &db,
            QueueConfig {
                lease: Duration::from_millis(100),
                ..QueueConfig::default()
            },
        );
        let id = queue.enqueue("crashy", &json!({}), None).await.unwrap();

        let first = queue
            .claim("dead-worker", &kinds(&["crashy"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.id, id);
        // Still leased: nobody else may have it.
        assert!(queue
            .claim("w2", &kinds(&["crashy"]))
            .await
            .unwrap()
            .is_none());

        // The worker "crashes" - we simply never heartbeat.
        tokio::time::sleep(Duration::from_millis(400)).await;

        assert_eq!(queue.reap_expired_leases().await.unwrap(), 1);
        let second = queue
            .claim("w2", &kinds(&["crashy"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.id, id);
        assert_eq!(second.attempts, 0, "reclaiming is not a failure");
    }

    /// Criterion E6 / edge case 9 - the reaper must never steal a live lease.
    #[tokio::test]
    async fn reaper_never_touches_a_live_lease() {
        let db = test_db().await;
        let queue = queue_with(
            &db,
            QueueConfig {
                // Generous next to the ~150 ms heartbeat loop below, so a
                // stalled test thread cannot make this flake; the point is the
                // predicate, not the margin.
                lease: Duration::from_millis(2000),
                heartbeat: Duration::from_millis(150),
                ..QueueConfig::default()
            },
        );
        let id = queue.enqueue("long", &json!({}), None).await.unwrap();
        let job = queue
            .claim("owner", &kinds(&["long"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.id, id);

        // Heartbeat past one whole lease period while a reaper runs flat out.
        let deadline = Instant::now() + Duration::from_millis(3000);
        while Instant::now() < deadline {
            assert!(
                queue.heartbeat(id, "owner").await.unwrap(),
                "heartbeat was refused while the job was live"
            );
            assert_eq!(
                queue.reap_expired_leases().await.unwrap(),
                0,
                "the reaper reclaimed a live lease"
            );
            assert!(
                queue
                    .claim("thief", &kinds(&["long"]))
                    .await
                    .unwrap()
                    .is_none(),
                "another worker claimed a live job"
            );
            tokio::time::sleep(Duration::from_millis(120)).await;
        }

        let (locked_by,): (Option<String>,) =
            sqlx::query_as("select locked_by from jobs where id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(locked_by.as_deref(), Some("owner"));
    }

    #[tokio::test]
    async fn heartbeat_extends_the_lease() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        let id = queue.enqueue("hb", &json!({}), None).await.unwrap();
        queue
            .claim("owner", &kinds(&["hb"]))
            .await
            .unwrap()
            .unwrap();

        let before: DateTime<Utc> =
            sqlx::query_scalar("select lease_expires_at from jobs where id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .unwrap();

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(queue.heartbeat(id, "owner").await.unwrap());

        let after: DateTime<Utc> =
            sqlx::query_scalar("select lease_expires_at from jobs where id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(after > before, "{after} should be later than {before}");

        // A stranger cannot extend it.
        assert!(!queue.heartbeat(id, "thief").await.unwrap());
    }

    /// Criterion E7 - measured inside PostgreSQL, so no wall-clock flake.
    #[tokio::test]
    async fn backoff_is_exactly_two_to_the_attempts_minutes() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        let id = queue.enqueue("flaky", &json!({}), None).await.unwrap();

        for expected_attempts in 1..=5 {
            queue.claim("w", &kinds(&["flaky"])).await.unwrap().unwrap();
            let failure = queue.fail(id, "w", "boom").await.unwrap().unwrap();

            assert_eq!(failure.attempts, expected_attempts);
            let expected_minutes = 2u64.pow(u32::try_from(expected_attempts - 1).unwrap());
            assert_eq!(
                failure.backoff,
                Duration::from_secs(expected_minutes * 60),
                "attempt {expected_attempts} should back off {expected_minutes} minutes",
            );

            // Pull it back to now so the next claim can see it.
            sqlx::query("update jobs set run_after = now(), dead_at = null where id = $1")
                .bind(id)
                .execute(&db.pool)
                .await
                .unwrap();
        }
    }

    /// Criterion E8 - the 5th failure dead-letters and audits.
    #[tokio::test]
    async fn fifth_failure_dead_letters_and_writes_an_audit_row() {
        let db = test_db().await;
        let queue = queue_with(&db, QueueConfig::default());
        let id = queue
            .enqueue("doomed", &json!({"why": "always"}), None)
            .await
            .unwrap();

        for attempt in 1..=5 {
            queue
                .claim("w", &kinds(&["doomed"]))
                .await
                .unwrap()
                .unwrap();
            let failure = queue.fail(id, "w", "still broken").await.unwrap().unwrap();
            assert_eq!(failure.dead, attempt == 5, "attempt {attempt}");
            if attempt < 5 {
                sqlx::query("update jobs set run_after = now() where id = $1")
                    .bind(id)
                    .execute(&db.pool)
                    .await
                    .unwrap();
            }
        }

        // Claimable never again, even once `run_after` is in the past.
        sqlx::query("update jobs set run_after = now() where id = $1")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(queue
            .claim("w", &kinds(&["doomed"]))
            .await
            .unwrap()
            .is_none());

        let (done_at, dead_at): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
            sqlx::query_as("select done_at, dead_at from jobs where id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(done_at.is_none(), "a dead-lettered job never completed");
        assert!(dead_at.is_some(), "dead_at is the terminal marker");

        let audit: (String, String, Value) = sqlx::query_as(
            "select actor, action, subject from audit_log where action = 'job_dead_letter'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(audit.0, "system");
        assert_eq!(audit.1, "job_dead_letter");
        assert_eq!(audit.2["job_id"], json!(id));
        assert_eq!(audit.2["kind"], json!("doomed"));
        assert_eq!(audit.2["attempts"], json!(5));
    }

    /// Criterion E11 / edge case 9.
    #[tokio::test]
    async fn a_worker_that_lost_its_lease_cannot_complete_the_job() {
        let db = test_db().await;
        let queue = queue_with(
            &db,
            QueueConfig {
                lease: Duration::from_millis(100),
                ..QueueConfig::default()
            },
        );
        let id = queue.enqueue("stolen", &json!({}), None).await.unwrap();
        queue
            .claim("slow", &kinds(&["stolen"]))
            .await
            .unwrap()
            .unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;
        queue
            .claim("fast", &kinds(&["stolen"]))
            .await
            .unwrap()
            .unwrap();

        assert!(!queue.complete(id, "slow").await.unwrap());
        assert!(queue.fail(id, "slow", "too late").await.unwrap().is_none());
        assert!(!queue.release(id, "slow").await.unwrap());
        assert!(!queue.heartbeat(id, "slow").await.unwrap());

        // The new owner is unharmed.
        assert!(queue.complete(id, "fast").await.unwrap());
    }

    /// Criterion E9 - a panic must not take the loop down with it.
    #[tokio::test]
    async fn panicking_handler_does_not_poison_the_worker() {
        let db = test_db().await;
        let queue = queue_with(&db, fast());

        let ran_after = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::new();
        registry.register(
            "boom",
            handler_fn(|_job, _ctx| async { panic!("handler exploded 💥") }),
        );
        registry.register("fine", {
            let ran_after = Arc::clone(&ran_after);
            handler_fn(move |_job, _ctx| {
                let ran_after = Arc::clone(&ran_after);
                async move {
                    ran_after.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            })
        });

        let boom = queue.enqueue("boom", &json!({}), None).await.unwrap();
        queue.enqueue("fine", &json!({}), None).await.unwrap();

        let (tx, rx) = watch::channel(false);
        let worker = tokio::spawn(Worker::new(queue.clone(), Arc::new(registry)).run(rx));

        wait_for(Duration::from_secs(10), || {
            let ran_after = Arc::clone(&ran_after);
            async move { ran_after.load(Ordering::SeqCst) == 1 }
        })
        .await;

        let (attempts, error): (i32, Option<String>) =
            sqlx::query_as("select attempts, error from jobs where id = $1")
                .bind(boom)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(attempts, 1);
        assert!(
            error.as_deref().unwrap_or_default().contains("panicked"),
            "{error:?}"
        );

        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("worker did not stop")
            .unwrap();
    }

    /// Criterion E10 / edge case 3.
    #[tokio::test]
    async fn graceful_shutdown_lets_an_inflight_job_finish() {
        let db = test_db().await;
        let queue = queue_with(&db, fast());

        let mut registry = Registry::new();
        registry.register(
            "slow",
            handler_fn(|_job, _ctx| async {
                tokio::time::sleep(Duration::from_millis(400)).await;
                Ok(())
            }),
        );

        let id = queue.enqueue("slow", &json!({}), None).await.unwrap();
        let (tx, rx) = watch::channel(false);
        let worker = tokio::spawn(Worker::new(queue.clone(), Arc::new(registry)).run(rx));

        // Wait until the worker has really picked it up, then ask it to stop.
        wait_for(Duration::from_secs(10), || {
            let pool = db.pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("select locked_by from jobs where id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
                    .is_some()
            }
        })
        .await;
        tx.send(true).unwrap();

        tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("worker did not stop")
            .unwrap();

        let (done_at, locked_by, lease): (
            Option<DateTime<Utc>>,
            Option<String>,
            Option<DateTime<Utc>>,
        ) = sqlx::query_as("select done_at, locked_by, lease_expires_at from jobs where id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(done_at.is_some(), "the in-flight job should have finished");
        assert!(
            locked_by.is_none() && lease.is_none(),
            "nothing left leased"
        );
    }

    /// Criterion B8 - the claim predicate must not degrade into a table scan.
    #[tokio::test]
    async fn claim_query_uses_an_index_not_a_seq_scan() {
        let db = test_db().await;
        sqlx::query(
            "insert into jobs (kind, payload, run_after) \
             select 'bulk', '{}'::jsonb, now() + interval '30 days' \
             from generate_series(1, 20000)",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query("insert into jobs (kind, payload) select 'bulk', '{}'::jsonb from generate_series(1, 10)")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("analyze jobs").execute(&db.pool).await.unwrap();

        let plan: Value = sqlx::query_scalar(
            "explain (format json) \
             select id from jobs \
              where done_at is null \
                and dead_at is null \
                and run_after <= now() \
                and (lease_expires_at is null or lease_expires_at < now()) \
                and kind = any($1::text[]) \
              order by run_after \
              for update skip locked \
              limit 1",
        )
        .bind(kinds(&["bulk"]))
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let rendered = plan.to_string();
        assert!(
            rendered.contains("jobs_ready_idx"),
            "claim should walk jobs_ready_idx: {rendered}"
        );
        assert!(
            !rendered.contains("Seq Scan"),
            "claim degraded to a sequential scan: {rendered}"
        );
    }
}
