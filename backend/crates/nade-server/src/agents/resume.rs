//! `resume_run`: carrying a human's decision — or the sweep's — into the run.
//!
//! # Why this is not `run_agent` with a different payload
//!
//! `agents::run::dedupe_key` is `run_agent:{run_id}`, and
//! `Queue::enqueue_unique_in` is `on conflict … do nothing` over an index
//! partial on *pending* jobs. If a stale `run_agent` job for the run is still
//! pending — the worker died and the lease has not lapsed, or a backoff is
//! scheduled — an enqueue under that key is **silently swallowed**. The run
//! would then sit `queued` with a consumed token and no job to move it, because
//! the approve transaction deliberately does not touch `run_journal`: replay
//! would find the approval still pending and park the run straight back.
//!
//! A distinct kind cannot collide with that, and its own key
//! (`resume_run:{run_id}:{step_seq}`) is single-use by construction — one
//! approval, one decision, one job.
//!
//! # The one guard this handler must not have
//!
//! `run_agent` opens with `if is_terminal(&run.status) { return Ok(()) }`.
//! Copying that here would make **skip a no-op**: `API.md` §7 has the skip
//! transaction move the run to `skipped` — a terminal status — before the
//! engine has written anything, so the guard would fire and
//! `approval_resolved`/`run_ended` would never be journalled. `Engine::resume`
//! is its own guard: it replays first and returns the recorded terminal outcome
//! when the journal already ends on `run_ended`.

use std::sync::Arc;

use async_trait::async_trait;
use nade_agent_sdk::{Error as SdkError, Resolution, Seq};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    jobs::{Handler, Job, JobContext, Queue},
    state::AppState,
};

/// The job kind. `lib.rs::register_handlers` reserves the name.
pub const KIND: &str = "resume_run";

/// One job per decision, not one per run.
///
/// A run can be gated more than once — `max_steps` is 12, and an agent holding
/// both `write_note` and `draft_reply` can pause, be approved, and pause again
/// — so the run id alone would suppress the second decision while the first
/// job was still pending.
#[must_use]
pub fn dedupe_key(run_id: Uuid, step_seq: Seq) -> String {
    format!("{KIND}:{run_id}:{step_seq}")
}

/// How a paused run was settled. The wire form of [`Resolution`], minus the
/// timer, which no v1 tool can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Skip,
    Expire,
}

impl Decision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Skip => "skip",
            Self::Expire => "expire",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "approve" => Some(Self::Approve),
            "skip" => Some(Self::Skip),
            "expire" => Some(Self::Expire),
            _ => None,
        }
    }

    const fn resolution(self, step_seq: Seq) -> Resolution {
        match self {
            Self::Approve => Resolution::Approve { step_seq },
            Self::Skip => Resolution::Skip { step_seq },
            Self::Expire => Resolution::Expire { step_seq },
        }
    }
}

/// Enqueue the resume in the caller's transaction.
///
/// **The `Option` is checked.** `enqueue_unique_in` answers `Ok(None)` when the
/// partial unique index suppressed the insert, and `agents::run::enqueue_in`
/// throws that away with `.map(|_| ())` — which is the exact blind spot this
/// module exists to avoid. Here it can only mean a second pending job for the
/// same decision, which the token's single use should already have made
/// impossible, so it fails the transaction rather than committing a decision
/// nothing will carry out.
///
/// # Errors
/// Returns an error if the insert fails, or if it was suppressed.
pub async fn enqueue_in<'e, E>(
    executor: E,
    run_id: Uuid,
    step_seq: Seq,
    decision: Decision,
) -> anyhow::Result<()>
where
    E: sqlx::PgExecutor<'e>,
{
    let payload = json!({
        "run_id": run_id,
        "step_seq": step_seq,
        "decision": decision.as_str(),
    });
    let enqueued = Queue::enqueue_unique_in(
        executor,
        KIND,
        &payload,
        None,
        &dedupe_key(run_id, step_seq),
    )
    .await?;
    anyhow::ensure!(
        enqueued.is_some(),
        "a resume job for run {run_id} step {step_seq} was already pending"
    );
    Ok(())
}

pub struct ResumeRunHandler {
    state: AppState,
}

impl ResumeRunHandler {
    #[must_use]
    pub fn shared(state: AppState) -> Arc<dyn Handler> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Handler for ResumeRunHandler {
    /// A decision that will never be delivered must not leave the run parked
    /// in a state nothing moves.
    ///
    /// The approve transaction commits the user's answer — token spent, card
    /// `resolved`, run `queued` — and then depends on this job to carry it out.
    /// Five failures later the queue gives up, and before this hook existed the
    /// run sat `queued` for ever: `run_agent` has a different dedupe key,
    /// nothing enqueues it, and there is no stuck-run reaper anywhere. The card
    /// said "Saved to Notes." about a note that was never written.
    ///
    /// So the run is ended, loudly, and the card is corrected to say what
    /// actually happened. `Engine::cancel` writes `run_ended` through the
    /// journal, which is the only author `API.md` §6.1 allows.
    async fn on_dead_letter(&self, job: Job, _ctx: JobContext) {
        let Some(run_id) = crate::json::uuid_at(&job.payload, "run_id") else {
            return;
        };
        crate::agents::run::abandon(&self.state, run_id, crate::agents::run::DEAD_LETTER_DETAIL)
            .await;
    }

    async fn handle(&self, job: Job, _ctx: JobContext) -> anyhow::Result<()> {
        let run_id = crate::json::uuid_at(&job.payload, "run_id")
            .ok_or_else(|| anyhow::anyhow!("resume_run job {} has no run_id", job.id))?;
        let step_seq: Seq = job
            .payload
            .get("step_seq")
            .and_then(Value::as_u64)
            .and_then(|seq| Seq::try_from(seq).ok())
            .ok_or_else(|| anyhow::anyhow!("resume_run job {} has no step_seq", job.id))?;
        let decision = job
            .payload
            .get("decision")
            .and_then(Value::as_str)
            .and_then(Decision::parse)
            .ok_or_else(|| anyhow::anyhow!("resume_run job {} has no decision", job.id))?;

        let outcome =
            crate::agents::run::resume_one(&self.state, run_id, decision.resolution(step_seq))
                .await;

        match outcome {
            Ok(()) => Ok(()),
            // Both mean an earlier delivery already won, which is success.
            // `AlreadyResolved` is the same button pressed twice or the same
            // job delivered twice; `NoPendingApproval` is the run having moved
            // past this step entirely. `StepMismatch` is the one that matters
            // most: a stale decision naming an *older* step must never be
            // applied to whatever is open now, and the SDK refuses it for us.
            Err(error) => match error.downcast_ref::<SdkError>() {
                Some(SdkError::AlreadyResolved { .. } | SdkError::NoPendingApproval(_)) => {
                    tracing::info!(%run_id, step_seq, "resume_run: already settled");
                    Ok(())
                }
                Some(SdkError::StepMismatch { expected, got, .. }) => {
                    tracing::warn!(
                        %run_id, ?expected, got,
                        "resume_run: a decision named a step the run is not parked on"
                    );
                    audit(
                        &self.state,
                        run_id,
                        "resume_step_mismatch",
                        &format!("expected {expected:?}, got {got}"),
                    )
                    .await;
                    Ok(())
                }
                _ => Err(error),
            },
        }
    }
}

async fn audit(state: &AppState, run_id: Uuid, action: &str, detail: &str) {
    let account: Option<Uuid> =
        sqlx::query_scalar("select account_id from agent_runs where id = $1")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    // A run whose row is already gone has no account to file under, and an
    // `audit_log.account_id` is not null — so there is nothing to write.
    let Some(account) = account else { return };
    crate::agents::audit(
        &state.pool,
        account,
        action,
        json!({ "run_id": run_id, "detail": detail }),
    )
    .await;
}

#[cfg(test)]
mod tests;
