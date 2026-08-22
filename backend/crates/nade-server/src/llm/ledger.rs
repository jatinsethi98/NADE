//! `llm_calls`: what was spent, by whom, and whether the ceiling is reached.
//!
//! # Why the ceiling is not enforced by returning an error
//!
//! [`Llm::chat`](nade_agent_sdk::Llm::chat)'s contract says an `Err` means
//! *"the host should retry this job later"*. A breach reported that way would
//! be retried by the queue — five times, with backoff — and every retry that
//! got far enough would spend again. PLAN.md names that path explicitly and
//! forbids it.
//!
//! So the breach travels out of band. [`SpendGuard`] holds an `AtomicBool` that
//! the adapter sets and the *job handler* reads; the handler ends the run with
//! `Engine::cancel`, a loud terminal `run_ended`, instead of handing the job
//! back to the queue. The flag has to be an `Arc` the handler cloned **before**
//! the adapter was moved into `Engine::new` — the engine takes its `Llm` by
//! value, so there is no way to reach back in for it afterwards.
//!
//! # How exact the ceiling is
//!
//! Exact at its boundary, loose under concurrency, and both on purpose.
//!
//! Every amount is integer nano-USD ([`super::cost`]), so `spent >= ceiling`
//! has no rounding in it at all. What it cannot be is *globally* serialising: a
//! call is priced only after the provider answers, so `NADE_WORKERS` runs can
//! each pass the check before any of them records. The real bound is
//!
//! > `ceiling + NADE_WORKERS x (cost of one model turn)`
//!
//! which at Haiku prices and a 50k-token run budget is a worst case of about
//! $0.25 per extra worker, against a $1.00 ceiling.
//!
//! Tightening it was considered and rejected. Reserving the maximum cost before
//! the call and reconciling after would bind the ceiling exactly, but a crash
//! between the two halves leaves a permanently over-charged row — and this
//! table is also the honest record of what the user actually spent. A ledger
//! that lies is worse than a ceiling that overshoots by cents.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use sqlx::PgPool;
use uuid::Uuid;

use super::{
    cost::{self, Tokens},
    Purpose,
};

/// One row of `llm_calls`, ready to insert.
#[derive(Debug, Clone)]
pub struct Record {
    pub account_id: Uuid,
    pub purpose: Purpose,
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub model: String,
    pub tokens: Tokens,
    /// Charged amount, nano-USD. Priced by the caller because only the adapter
    /// sees the provider's full token breakdown.
    pub cost_nano_usd: i64,
    pub ok: bool,
    pub error: Option<String>,
}

impl Record {
    /// A successful call, priced from `model`'s rates.
    #[must_use]
    pub fn ok(
        account_id: Uuid,
        purpose: Purpose,
        model: impl Into<String>,
        tokens: Tokens,
    ) -> Self {
        let model = model.into();
        let cost_nano_usd = cost::cost_nano_usd(&model, tokens);
        Self {
            account_id,
            purpose,
            agent_id: None,
            run_id: None,
            model,
            tokens,
            cost_nano_usd,
            ok: true,
            error: None,
        }
    }

    /// A call that failed. Still recorded, and still charged for whatever the
    /// provider reported — a failure after a partial stream is not free, and a
    /// burst of failures should be visible in the ledger rather than absent
    /// from it.
    #[must_use]
    pub fn failed(
        account_id: Uuid,
        purpose: Purpose,
        model: impl Into<String>,
        tokens: Tokens,
        error: impl Into<String>,
    ) -> Self {
        let mut record = Self::ok(account_id, purpose, model, tokens);
        record.ok = false;
        record.error = Some(truncate_error(&error.into()));
        record
    }

    /// Attach the agent this call belongs to.
    #[must_use]
    pub fn for_agent(mut self, agent_id: Uuid) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    /// Attach the run this call belongs to.
    #[must_use]
    pub fn for_run(mut self, run_id: Uuid) -> Self {
        self.run_id = Some(run_id);
        self
    }
}

/// `llm_calls.error` is diagnostic, not a transcript. A provider that answers
/// with a megabyte of HTML must not put a megabyte into every ledger row.
const MAX_ERROR_CHARS: usize = 500;

fn truncate_error(error: &str) -> String {
    // EDGE: unicode. Truncating by bytes would split a multi-byte character and
    // produce invalid UTF-8; `char_indices` cuts on a boundary.
    match error.char_indices().nth(MAX_ERROR_CHARS) {
        None => error.to_owned(),
        Some((idx, _)) => format!("{}...", &error[..idx]),
    }
}

/// Insert one row, logging rather than failing if the write does not land.
///
/// A run must not die because its accounting did — but a silent gap in the
/// ledger is exactly how a ceiling stops binding, so it is loud. `let _ =
/// record(..)` was the original spelling at both call sites and is the quiet
/// half of the same bug.
///
/// One function, because there were two: the adapter and the spec compiler had
/// each grown their own wrapper around this, with two wordings of the same
/// warning to grep for.
pub async fn record_or_log(pool: &PgPool, entry: &Record) {
    if let Err(err) = record(pool, entry).await {
        tracing::error!(
            error = %err,
            account_id = %entry.account_id,
            purpose = %entry.purpose,
            "could not write an llm_calls row; the spend ceiling is now under-counting"
        );
    }
}

/// Insert one row.
///
/// # Errors
/// Returns an error if the insert fails.
pub async fn record(pool: &PgPool, entry: &Record) -> sqlx::Result<()> {
    sqlx::query(
        "insert into llm_calls \
           (account_id, purpose, agent_id, run_id, model, \
            tokens_in, tokens_out, cache_read_tokens, cache_write_tokens, \
            cost_usd, ok, error) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11, $12)",
    )
    .bind(entry.account_id)
    .bind(entry.purpose.as_str())
    .bind(entry.agent_id)
    .bind(entry.run_id)
    .bind(&entry.model)
    .bind(i32::try_from(entry.tokens.input).unwrap_or(i32::MAX))
    .bind(i32::try_from(entry.tokens.output).unwrap_or(i32::MAX))
    .bind(i32::try_from(entry.tokens.cache_read).unwrap_or(i32::MAX))
    .bind(i32::try_from(entry.tokens.cache_write).unwrap_or(i32::MAX))
    // Bound as text and cast, not as a float: `sqlx` carries no decimal type in
    // this build, and binding `f64` would undo in one line everything
    // `llm::cost` exists to guarantee.
    .bind(cost::nano_to_numeric(entry.cost_nano_usd))
    .bind(entry.ok)
    .bind(entry.error.as_deref())
    .execute(pool)
    .await
    .map(|_| ())
}

/// What `account_id` has spent since midnight UTC, in nano-USD.
///
/// The day boundary is [`SINCE_UTC_MIDNIGHT`], which carries the reasoning.
///
/// The multiply and cast happen in PostgreSQL because `sqlx` is built here
/// without a decimal type: `numeric(14,9) * 1e9` is exactly integral, so the
/// `bigint` cast rounds nothing.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn spent_today_nano(pool: &PgPool, account_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!(
        "select (coalesce(sum(cost_usd), 0) * 1000000000)::bigint \
           from llm_calls \
          where account_id = $1 and created_at >= {SINCE_UTC_MIDNIGHT}"
    ))
    .bind(account_id)
    .fetch_one(pool)
    .await
}

/// How many triage calls `agent_id` has made since midnight UTC.
///
/// Backs PLAN.md's <=20 messages per agent per day. P5 is what enforces it;
/// the query lands with the ledger so the cap is not a second thing to discover
/// later.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn triage_calls_today(pool: &PgPool, agent_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(&format!(
        "select count(*) from llm_calls \
          where agent_id = $1 and purpose = 'triage' \
            and created_at >= {SINCE_UTC_MIDNIGHT}"
    ))
    .bind(agent_id)
    .fetch_one(pool)
    .await
}

/// Midnight UTC, as a `timestamptz`, for a `created_at >= ...` predicate.
///
/// **The day boundary is UTC**, deliberately. A per-account timezone would make
/// the ceiling depend on a column that does not exist and a test depend on
/// where it runs; UTC is the only boundary both the server and the test suite
/// can agree on without one.
///
/// Note the **second** `at time zone 'utc'`. Without it the expression is a
/// naked `timestamp`, and comparing a `timestamptz` against one converts it
/// using the *session* timezone — which this cluster reports as
/// `America/New_York`, not UTC. The boundary then moves by the server's offset,
/// so the ceiling silently stops binding for the first hours of each UTC day
/// west of it. The docstring claimed UTC; the SQL did not deliver it.
///
/// That is the whole reason this is a constant rather than a phrase typed at
/// each site: three statements needed it, one of them in another module, and
/// the failure it guards against is silent.
pub const SINCE_UTC_MIDNIGHT: &str =
    "date_trunc('day', now() at time zone 'utc') at time zone 'utc'";

/// The ceiling was reached. Not an `Err` from `chat` — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breached {
    /// Nano-USD already spent today.
    pub spent_nano_usd: i64,
    /// Nano-USD the account is allowed per day.
    pub ceiling_nano_usd: i64,
}

impl std::fmt::Display for Breached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daily model spend ceiling reached: ${} of ${} used today",
            cost::nano_to_numeric(self.spent_nano_usd),
            cost::nano_to_numeric(self.ceiling_nano_usd)
        )
    }
}

/// Reads the ledger before a call and raises a flag the job handler can see.
///
/// Cheap to clone; every clone shares one flag.
#[derive(Debug, Clone)]
pub struct SpendGuard {
    pool: PgPool,
    account_id: Uuid,
    ceiling_nano_usd: i64,
    tripped: Arc<AtomicBool>,
}

impl SpendGuard {
    #[must_use]
    pub fn new(pool: PgPool, account_id: Uuid, ceiling_nano_usd: i64) -> Self {
        Self {
            pool,
            account_id,
            ceiling_nano_usd,
            tripped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The account this guard watches.
    #[must_use]
    pub fn account_id(&self) -> Uuid {
        self.account_id
    }

    /// Whether the ceiling was hit since this guard was built.
    ///
    /// The job handler reads this after `Engine::run` returns an error, to tell
    /// "the model was unreachable" (retry the job) from "the account is out of
    /// budget" (cancel the run, and do not retry).
    #[must_use]
    pub fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }

    /// Refuse the next call if today's spend has already reached the ceiling.
    ///
    /// Called **before** the request goes out — after would be too late to stop
    /// the spend it is supposed to stop.
    ///
    /// # Errors
    /// Returns `Ok(Err(Breached))` when the ceiling is reached, and the outer
    /// `Err` only when the ledger itself could not be read. The two are
    /// separated because they route differently: a breach cancels the run and a
    /// database failure retries the job.
    pub async fn check(&self) -> sqlx::Result<Result<(), Breached>> {
        // EDGE: a ceiling of 0 means "no model calls at all", and must block on
        // an empty ledger where `spent (0) >= ceiling (0)` is already true.
        let spent = spent_today_nano(&self.pool, self.account_id).await?;
        if spent >= self.ceiling_nano_usd {
            self.tripped.store(true, Ordering::SeqCst);
            return Ok(Err(Breached {
                spent_nano_usd: spent,
                ceiling_nano_usd: self.ceiling_nano_usd,
            }));
        }
        Ok(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_provider_error_is_truncated_on_a_character_boundary() {
        let huge = "e".repeat(MAX_ERROR_CHARS * 3);
        let out = truncate_error(&huge);
        assert!(out.len() < huge.len());
        assert!(out.ends_with("..."));
    }

    #[test]
    fn truncation_does_not_split_a_multibyte_character() {
        // Every char is 4 bytes, so a byte-wise cut would produce invalid UTF-8
        // and this would panic rather than fail.
        let emoji = "\u{1F600}".repeat(MAX_ERROR_CHARS * 2);
        let out = truncate_error(&emoji);
        assert!(out.is_char_boundary(out.len()));
        assert!(out.chars().count() <= MAX_ERROR_CHARS + 3);
    }

    #[test]
    fn a_short_error_is_left_alone() {
        assert_eq!(truncate_error("nope"), "nope");
    }

    #[test]
    fn a_record_prices_itself_from_the_model() {
        let record = Record::ok(
            Uuid::nil(),
            Purpose::Run,
            "claude-haiku-4-5",
            Tokens {
                input: 1_000,
                output: 100,
                ..Tokens::default()
            },
        );
        // 1000 in x 1000 nano + 100 out x 5000 nano.
        assert_eq!(record.cost_nano_usd, 1_000 * 1_000 + 100 * 5_000);
        assert!(record.ok);
    }

    #[test]
    fn a_failed_record_is_still_charged_and_still_recorded() {
        let record = Record::failed(
            Uuid::nil(),
            Purpose::Run,
            "claude-haiku-4-5",
            Tokens {
                input: 10,
                ..Tokens::default()
            },
            "upstream exploded",
        );
        assert!(!record.ok);
        assert_eq!(record.cost_nano_usd, 10_000);
        assert_eq!(record.error.as_deref(), Some("upstream exploded"));
    }

    // ------------------------------------------- against a real database --

    use crate::test_support::test_db;

    async fn an_account(pool: &PgPool) -> Uuid {
        sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
            .bind(format!("ledger-{}@example.com", Uuid::new_v4()))
            .fetch_one(pool)
            .await
            .expect("account")
    }

    #[tokio::test]
    async fn a_recorded_cost_comes_back_out_of_the_database_unchanged() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;

        // Nine decimal places, the full precision of the column, chosen so any
        // rounding anywhere in the round trip would show up.
        let mut entry = Record::ok(account, Purpose::Run, "claude-haiku-4-5", Tokens::default());
        entry.cost_nano_usd = 123_456_789;
        record(&db.pool, &entry).await.expect("record");

        assert_eq!(
            spent_today_nano(&db.pool, account).await.unwrap(),
            123_456_789,
            "money must survive the round trip exactly; a float would not"
        );
    }

    #[tokio::test]
    async fn costs_accumulate_across_calls() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        for _ in 0..3 {
            let mut entry =
                Record::ok(account, Purpose::Run, "claude-haiku-4-5", Tokens::default());
            entry.cost_nano_usd = 333_333_333;
            record(&db.pool, &entry).await.unwrap();
        }
        assert_eq!(
            spent_today_nano(&db.pool, account).await.unwrap(),
            999_999_999
        );
    }

    #[tokio::test]
    async fn the_ceiling_blocks_at_exactly_the_limit_and_not_a_nano_before() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        let ceiling = 1_000_000_000; // $1.00

        // One nano under. The comparison is `>=`, so this must still pass - and
        // this is the assertion that a float implementation would fail
        // intermittently rather than never.
        let mut entry = Record::ok(account, Purpose::Run, "claude-haiku-4-5", Tokens::default());
        entry.cost_nano_usd = ceiling - 1;
        record(&db.pool, &entry).await.unwrap();

        let guard = SpendGuard::new(db.pool.clone(), account, ceiling);
        assert!(
            guard.check().await.unwrap().is_ok(),
            "$0.999999999 must not block"
        );
        assert!(!guard.tripped());

        // The nano that reaches the ceiling.
        let mut last = Record::ok(account, Purpose::Run, "claude-haiku-4-5", Tokens::default());
        last.cost_nano_usd = 1;
        record(&db.pool, &last).await.unwrap();

        let breach = guard.check().await.unwrap().expect_err("$1.00 must block");
        assert_eq!(breach.spent_nano_usd, ceiling);
        assert_eq!(breach.ceiling_nano_usd, ceiling);
        assert!(
            guard.tripped(),
            "the handler reads this to choose cancel over retry"
        );
    }

    #[tokio::test]
    async fn an_empty_ledger_is_under_any_positive_ceiling() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        assert_eq!(spent_today_nano(&db.pool, account).await.unwrap(), 0);
        let guard = SpendGuard::new(db.pool.clone(), account, 1);
        assert!(guard.check().await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn the_day_boundary_is_utc_and_does_not_depend_on_the_servers_timezone() {
        // A review flagged `date_trunc('day', now() at time zone 'utc')` as a
        // naked `timestamp` whose comparison against a `timestamptz` happens in
        // the **session** timezone - and this cluster's `postgresql.conf` really
        // does say `timezone = 'America/New_York'`. It would have been a live
        // under-count of spend for the first hours of every UTC day.
        //
        // It is not, and the reason is worth pinning rather than rediscovering:
        // `sqlx` negotiates `TimeZone=UTC` on every connection, so the two
        // spellings coincide. The query now carries the explicit second
        // `at time zone 'utc'` anyway - correctness that does not depend on a
        // driver default - and this test guards the default, because if a
        // future `sqlx` or an `after_connect` hook stopped pinning UTC, every
        // naked-timestamp comparison in the crate would shift at once.
        let db = test_db().await;

        let session_tz: String = sqlx::query_scalar("show timezone")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            session_tz, "UTC",
            "sqlx no longer pins the session timezone; every naked-timestamp \
             comparison in this crate just changed meaning"
        );

        // And the boundary itself: a row a second before UTC midnight is
        // yesterday, one a second after is today, whatever the server thinks.
        let account = an_account(&db.pool).await;
        for (offset, nano) in [("-1 second", 111), ("+1 second", 222)] {
            sqlx::query(
                "insert into llm_calls (account_id, purpose, model, cost_usd, ok, created_at) \
                 values ($1, 'run', 'm', $2::numeric, true, \
                         date_trunc('day', now() at time zone 'utc') at time zone 'utc' \
                         + $3::interval)",
            )
            .bind(account)
            .bind(cost::nano_to_numeric(nano))
            .bind(offset)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        assert_eq!(spent_today_nano(&db.pool, account).await.unwrap(), 222);
    }

    #[tokio::test]
    async fn a_zero_ceiling_blocks_immediately_which_is_how_you_turn_the_model_off() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        // EDGE: `spent (0) >= ceiling (0)` is already true on an empty ledger.
        let guard = SpendGuard::new(db.pool.clone(), account, 0);
        assert!(guard.check().await.unwrap().is_err());
    }

    #[tokio::test]
    async fn one_accounts_spending_never_counts_against_another() {
        let db = test_db().await;
        let spender = an_account(&db.pool).await;
        let bystander = an_account(&db.pool).await;

        let mut entry = Record::ok(spender, Purpose::Run, "claude-haiku-4-5", Tokens::default());
        entry.cost_nano_usd = 5_000_000_000;
        record(&db.pool, &entry).await.unwrap();

        assert_eq!(
            spent_today_nano(&db.pool, spender).await.unwrap(),
            5_000_000_000
        );
        assert_eq!(
            spent_today_nano(&db.pool, bystander).await.unwrap(),
            0,
            "the ceiling is per account (PLAN.md Dev caps), not per process"
        );
    }

    #[tokio::test]
    async fn a_failed_call_is_recorded_and_still_counts_against_the_ceiling() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        let mut entry = Record::failed(
            account,
            Purpose::Compile,
            "claude-haiku-4-5",
            Tokens {
                input: 100,
                ..Tokens::default()
            },
            "the provider timed out",
        );
        entry.cost_nano_usd = 100_000;
        record(&db.pool, &entry).await.unwrap();

        let (ok, error): (bool, Option<String>) =
            sqlx::query_as("select ok, error from llm_calls where account_id = $1")
                .bind(account)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(!ok);
        assert_eq!(error.as_deref(), Some("the provider timed out"));
        // A burst of failures must be visible in the ledger, not absent from it.
        assert_eq!(spent_today_nano(&db.pool, account).await.unwrap(), 100_000);
    }

    #[tokio::test]
    async fn the_triage_cap_counts_only_triage_calls_for_one_agent() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        let agent: Uuid = sqlx::query_scalar(
            "insert into agents (account_id, name, nl_definition) \
             values ($1, 'A', 'x') returning id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();

        for purpose in [Purpose::Triage, Purpose::Triage, Purpose::Run, Purpose::Ask] {
            let entry = Record::ok(account, purpose, "claude-haiku-4-5", Tokens::default())
                .for_agent(agent);
            record(&db.pool, &entry).await.unwrap();
        }

        assert_eq!(
            triage_calls_today(&db.pool, agent).await.unwrap(),
            2,
            "only `triage` counts toward the per-agent cap"
        );
    }

    #[tokio::test]
    async fn deleting_an_agent_keeps_what_it_spent() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        let agent: Uuid = sqlx::query_scalar(
            "insert into agents (account_id, name, nl_definition) \
             values ($1, 'A', 'x') returning id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let mut entry = Record::ok(account, Purpose::Run, "claude-haiku-4-5", Tokens::default())
            .for_agent(agent);
        entry.cost_nano_usd = 42_000_000;
        record(&db.pool, &entry).await.unwrap();

        sqlx::query("delete from agents where id = $1")
            .bind(agent)
            .execute(&db.pool)
            .await
            .unwrap();

        // `on delete set null`, never cascade: the ceiling is an account-level
        // fact and has to outlive the agent that ran up the bill.
        assert_eq!(
            spent_today_nano(&db.pool, account).await.unwrap(),
            42_000_000
        );
        let orphaned: Option<Uuid> =
            sqlx::query_scalar("select agent_id from llm_calls where account_id = $1")
                .bind(account)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(orphaned.is_none());
    }

    #[tokio::test]
    async fn an_unpriced_purpose_would_be_refused_by_the_check_constraint() {
        let db = test_db().await;
        let account = an_account(&db.pool).await;
        // Not reachable through `Purpose`, which is the point: the DDL is the
        // backstop for a raw insert that bypassed the enum.
        let result = sqlx::query(
            "insert into llm_calls (account_id, purpose, model, ok) values ($1, 'guessing', 'm', true)",
        )
        .bind(account)
        .execute(&db.pool)
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn the_breach_message_names_both_numbers_in_dollars() {
        let message = Breached {
            spent_nano_usd: 1_000_000_000,
            ceiling_nano_usd: 1_000_000_000,
        }
        .to_string();
        assert!(message.contains("1.000000000"), "{message}");
    }
}
