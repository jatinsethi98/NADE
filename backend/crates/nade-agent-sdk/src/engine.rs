//! The tool-calling loop, the approval gate, and the replay machinery that
//! makes a crashed run safe to restart.

use std::collections::{HashMap, HashSet};
use std::future::{poll_fn, Future};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::ids::{args_hash, effect_id, RunId, Seq};
use crate::journal::{
    ApprovalRequested, ApprovalResolved, CapBreached, Entry, EntryKind, Journal, ModelResponse,
    RunEnded, RunStarted, RunWaiting, RunWoken, StepDone, StepStarted, JOURNAL_FORMAT,
};
use crate::llm::Llm;
use crate::message::{CallContext, ChatRequest, Message, TokenUsage, ToolCall};
use crate::run::{
    ApprovalRequest, Decision, FailureReason, Resolution, RunInput, RunOutcome, RunStats, RunStatus,
};
use crate::tool::{self, control, tool_fingerprint, ReplayPolicy, Tool, ToolSet};

/// Caps and knobs. Every default is the one `PLAN.md` fixes for NADE, but
/// nothing here is NADE-specific.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Maximum tool steps a run may **open**, counting a step that is waiting
    /// on approval and a step that named a tool that does not exist.
    ///
    /// Checked before a step is opened, so a run never asks a human to approve
    /// something it could not have run anyway. Re-executing a step after a
    /// crash does not consume extra budget: the cap counts distinct steps, not
    /// attempts.
    ///
    /// Default `12`.
    pub max_steps: u32,

    /// Maximum tokens, input plus output, across every model turn of a run.
    ///
    /// Enforced twice: after a turn (the response that pushed the run over ends
    /// it) and before the next turn (a run with nothing left to spend does not
    /// call the model at all). Spending *exactly* the budget is allowed. The
    /// after-a-turn rule is re-applied on replay, so a crash cannot let a run
    /// take a step the turn that broke the budget had already forbidden.
    ///
    /// Default `50_000`.
    pub token_budget: u64,

    /// Maximum serialised bytes of a single tool result.
    ///
    /// Larger results are replaced by an explicit truncation envelope before
    /// they reach either the journal or the model — never silently shortened.
    /// Values below a few hundred bytes cannot fit the envelope's own fields.
    ///
    /// Default `16_384`.
    pub max_tool_result_bytes: usize,

    /// How long an approval stays approvable. `None` means forever.
    ///
    /// The engine has no clock of its own: a stale approval is only noticed when
    /// someone tries to resolve it, and is then refused. Ageing runs out
    /// proactively is the host's job — an expiry sweep that calls
    /// [`Engine::resume`] with [`Resolution::Expire`].
    ///
    /// Default `Some(7 days)`.
    pub approval_ttl: Option<Duration>,

    /// Grace added to every expiry comparison, to absorb a resolver clock that
    /// runs **ahead** of the machine that wrote the approval.
    ///
    /// An approval is refused only when `now > expires_at + leeway`.
    ///
    /// # The clock assumption, precisely
    ///
    /// Expiry is wall-clock arithmetic on timestamps written by possibly
    /// different machines, so it is only as good as those clocks. This crate
    /// bounds the damage from both directions:
    ///
    /// * **Resolver ahead of writer** — a decision made in time could look
    ///   late. This leeway absorbs it; pick it larger than your worst expected
    ///   skew (NTP-disciplined hosts are typically well under a second; 60 s is
    ///   the default and is generous).
    /// * **Resolver behind writer** — the dangerous direction, because a
    ///   backwards jump would make an objectively stale approval approvable
    ///   again. The engine therefore never reads a "now" earlier than the newest
    ///   `created_at` in the run's own journal: time cannot run backwards past
    ///   the last durable fact about the run. A machine whose clock jumps back
    ///   an hour still cannot un-expire an approval whose journal has entries
    ///   from ten minutes ago.
    ///
    /// What is *not* defended: a clock that jumps **forward** by more than
    /// `leeway` expires approvals early, and a run whose journal has been idle
    /// for the whole TTL has no recent entry to floor the comparison with. If
    /// expiry has to be exact, expire from a sweep that reads
    /// [`ApprovalRequest::expires_at`] out of your own database rather than
    /// relying on the engine to notice.
    ///
    /// Default `60s`.
    pub clock_skew_leeway: Duration,

    /// How far ahead of the local clock a journal entry's `created_at` may sit
    /// before the journal is refused. `None` disables the bound.
    ///
    /// The floor described on [`clock_skew_leeway`](EngineConfig::clock_skew_leeway)
    /// only defends the backwards direction, and it does so by taking the newest
    /// `created_at` in the run's journal as a lower bound on "now". That makes
    /// the floor as trustworthy as the journal: **one entry stamped in the
    /// future displaces the run's clock for the rest of its life**, and every
    /// timestamp the run subsequently writes — including the `expires_at` of
    /// later approvals — is displaced with it.
    ///
    /// A faithful [`Journal`] cannot produce such an entry: only `Entry`'s own
    /// `created_at` feeds the floor, and the engine assigns it. A buggy or
    /// hostile implementation can, and this crate is not entitled to assume one.
    /// So the floor is bounded from above as well: an entry further ahead than
    /// this is [`Error::CorruptJournal`] rather than silently accepted.
    ///
    /// Pick it larger than the skew between machines that write one run's
    /// journal, and far smaller than
    /// [`approval_ttl`](EngineConfig::approval_ttl).
    ///
    /// Default `Some(5 minutes)`.
    pub max_journal_clock_drift: Option<Duration>,

    /// Model identifier passed through on every [`ChatRequest`]. Adapters that
    /// serve one model can ignore it.
    pub model: Option<String>,

    /// Per-turn output ceiling passed through on every [`ChatRequest`].
    pub max_output_tokens: Option<u32>,

    /// Sampling temperature passed through on every [`ChatRequest`].
    pub temperature: Option<f32>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_steps: 12,
            token_budget: 50_000,
            max_tool_result_bytes: 16 * 1024,
            approval_ttl: Some(Duration::from_secs(7 * 24 * 60 * 60)),
            clock_skew_leeway: Duration::from_secs(60),
            max_journal_clock_drift: Some(Duration::from_secs(5 * 60)),
            model: None,
            max_output_tokens: None,
            temperature: None,
        }
    }
}

/// Saturating conversion; a duration too large for `chrono` becomes "never".
fn delta(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX)
}

/// `from + d`, saturating at the end of representable time.
///
/// EDGE: chrono's `Add` *panics* on overflow, so an absurd `approval_ttl` or
/// `clock_skew_leeway` would take a server down rather than being clamped.
fn deadline(from: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    from.checked_add_signed(delta(d))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// The agent runtime.
///
/// Generic over the model and the journal; the tool set is a collection of
/// trait objects because a heterogeneous set of tools cannot be one type
/// parameter. `Send + Sync + 'static` whenever `L` and `J` are, so a server can
/// build one at startup and share it through an `Arc`.
///
/// See the [crate docs](crate) for the journal-before-effect contract this
/// implements, and for the guarantee it does and does not provide.
pub struct Engine<L, J> {
    llm: L,
    tools: ToolSet,
    journal: J,
    config: EngineConfig,
}

impl<L, J> std::fmt::Debug for Engine<L, J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("tools", &self.tools.names())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl<L: Llm, J: Journal> Engine<L, J> {
    /// Assemble an engine.
    ///
    /// `tools` is the host-enforced allowlist: the only names advertised to the
    /// model and the only ones that will ever be dispatched. Fails with
    /// [`Error::DuplicateTool`] if two tools share a name. An empty list is
    /// legal — the model is simply told it has no tools.
    pub fn new(
        llm: L,
        tools: impl IntoIterator<Item = Arc<dyn Tool>>,
        journal: J,
        config: EngineConfig,
    ) -> Result<Self> {
        Ok(Self {
            llm,
            tools: ToolSet::new(tools)?,
            journal,
            config,
        })
    }

    /// The configuration this engine was built with.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// The tools this engine will dispatch.
    pub fn tools(&self) -> &ToolSet {
        &self.tools
    }

    /// Start `run_id`, or carry on where a previous attempt left off.
    ///
    /// This is the call a job worker makes, including after a crash, and it is
    /// the **only** call needed to drive a run whose decisions are already on
    /// record. It is safe to make repeatedly:
    ///
    /// * an empty journal starts the run and records `input`;
    /// * a journal that already has entries is **replayed**, and the `input`
    ///   argument is ignored in favour of the one recorded at the start;
    /// * a run that is finished, waiting on a human, or parked on a timer is
    ///   returned as-is without appending anything or executing anything;
    /// * a run whose approval was resolved but whose outcome never committed is
    ///   carried through to that outcome — an approve executes, a skip ends
    ///   `skipped`, an expire ends `expired`;
    /// * a run whose final model response committed but whose `run_ended` did
    ///   not is finished **from the recorded response**. The model is never
    ///   asked to answer twice.
    ///
    /// Errors are transport failures — an unreachable model, a journal that
    /// will not commit — or a refusal to proceed unsafely
    /// ([`Error::CorruptJournal`], [`Error::UnsupportedJournalFormat`],
    /// [`Error::ToolChanged`]). They leave the run exactly where its journal
    /// says it is. A refusal that repeats is not a dead end: as long as the
    /// journal replays, [`Engine::cancel`] will end the run.
    pub async fn run(&self, run_id: RunId, input: impl Into<RunInput>) -> Result<RunOutcome> {
        let entries = self.journal.load(run_id).await?;
        let mut state = if entries.is_empty() {
            let input = input.into();
            let mut state = RunState::fresh(&input);
            self.append(
                run_id,
                &mut state,
                EntryKind::RunStarted,
                &RunStarted::new(input),
            )
            .await?;
            state
        } else {
            RunState::replay(run_id, &entries, &self.config)?
        };

        self.continue_run(run_id, &mut state).await
    }

    /// End `run_id` because the host has decided it cannot or should not
    /// continue. The escape hatch, and the only one.
    ///
    /// Appends `run_ended` with [`RunStatus::Failed`] and
    /// [`FailureReason::Cancelled`], executing nothing. It is **idempotent**: a
    /// run that has already reached a terminal state is returned as-is, and a
    /// second cancel appends nothing.
    ///
    /// # Why this exists
    ///
    /// Everything else in this crate refuses to proceed when proceeding would
    /// be unsafe, and a refusal that repeats forever is not a resolution. The
    /// case that forced the issue: a step's tool fingerprint changes *after* an
    /// approval has committed. The decision is on record so it cannot be
    /// retaken ([`Error::AlreadyResolved`]), and the step cannot execute under
    /// an implementation other than the one a human agreed to
    /// ([`Error::ToolChanged`]) — leaving [`run`](Engine::run) and
    /// [`resume`](Engine::resume) both permanently stuck. Cancelling is the
    /// host saying "then it does not happen", and recording that as a fact.
    ///
    /// # What it deliberately does not offer
    ///
    /// There is no "re-approve under the new implementation". The step's
    /// [`effect_id`](crate::effect_id) was minted for the action the human was
    /// shown; rebinding it would run a *different* action under an identity
    /// somebody authorised for something else, which is the exact failure the
    /// rest of this crate is built to prevent. Cancel, and start a run the human
    /// can approve on its own terms.
    ///
    /// # Its one precondition
    ///
    /// The journal must still replay. A run refused with
    /// [`Error::CorruptJournal`] or [`Error::UnsupportedJournalFormat`] cannot
    /// be cancelled either, because the engine cannot know which sequence to
    /// append at — that is a storage problem, and the host owns it.
    pub async fn cancel(&self, run_id: RunId, detail: impl Into<String>) -> Result<RunOutcome> {
        let entries = self.journal.load(run_id).await?;
        if entries.is_empty() {
            return Err(Error::CorruptJournal {
                run: run_id,
                message: "cannot cancel a run that was never started".to_string(),
            });
        }
        let mut state = RunState::replay(run_id, &entries, &self.config)?;
        if let Some(ended) = &state.ended {
            return terminal_outcome(run_id, ended, state.stats());
        }
        let reason = FailureReason::Cancelled {
            detail: detail.into(),
        };
        self.end(run_id, &mut state, RunStatus::Failed, None, Some(reason))
            .await
    }

    /// Settle a paused run: a human's answer to an approval, or a timer firing.
    ///
    /// The only way a tool guarded by
    /// [`Tool::requires_approval`](crate::Tool::requires_approval) ever runs.
    ///
    /// * [`Resolution::Approve`] executes the pending call — unless its TTL has
    ///   passed, in which case the run ends [`RunStatus::Expired`] and the tool
    ///   is not called.
    /// * [`Resolution::Skip`] and [`Resolution::Expire`] end the run without
    ///   executing anything.
    /// * [`Resolution::Timer`] wakes a run parked by
    ///   [`control::wait_until`](crate::control::wait_until).
    ///
    /// # Every resolution names its step
    ///
    /// The `step_seq` on the resolution must be the step the run is actually
    /// parked on. This is what makes a duplicate delivery safe in the case that
    /// matters: an old approval arriving after the run has moved on to a *new*
    /// approval is refused rather than silently applied to the new one. Take the
    /// value from [`ApprovalRequest::step_seq`] or
    /// [`RunOutcome::Waiting::step_seq`].
    ///
    /// | situation | result | appended | executed |
    /// |---|---|---|---|
    /// | names the pending step | settled | yes | per decision |
    /// | run already finished | its recorded outcome | no | no |
    /// | names an already-decided step | [`Error::AlreadyResolved`] | no | no |
    /// | names some other step | [`Error::StepMismatch`] | no | no |
    /// | nothing is pending | [`Error::NoPendingApproval`] / [`Error::NotWaiting`] | no | no |
    ///
    /// [`Error::AlreadyResolved`] is the answer to the same button pressed
    /// twice or the same webhook delivered twice. Treat it as success: an
    /// earlier delivery already won. If the run still needs driving — the
    /// decision committed but the process died before acting on it — call
    /// [`Engine::run`], which resumes from any recorded decision.
    pub async fn resume(&self, run_id: RunId, resolution: Resolution) -> Result<RunOutcome> {
        let entries = self.journal.load(run_id).await?;
        if entries.is_empty() {
            // Resolving a run that was never started. Report the mismatch the
            // caller actually made rather than "corrupt journal".
            return Err(match resolution {
                Resolution::Timer { .. } => Error::NotWaiting(run_id),
                _ => Error::NoPendingApproval(run_id),
            });
        }
        let mut state = RunState::replay(run_id, &entries, &self.config)?;

        // EDGE: duplicate resume of a finished run. The terminal entry is the
        // authority; no second run, no second effect, no new journal entries.
        if let Some(ended) = &state.ended {
            return terminal_outcome(run_id, ended, state.stats());
        }

        let step_seq = resolution.step_seq();
        match resolution {
            Resolution::Timer { .. } => {
                let Some(waiting) = state.waiting.clone() else {
                    return Err(Error::NotWaiting(run_id));
                };
                // A stale wake-up must not cut short a later, unrelated wait.
                if waiting.step_seq != step_seq {
                    return Err(Error::StepMismatch {
                        run: run_id,
                        expected: Some(waiting.step_seq),
                        got: step_seq,
                    });
                }
                let woken = RunWoken {
                    step_seq,
                    woken_at: self.now(&state),
                };
                self.append(run_id, &mut state, EntryKind::RunWoken, &woken)
                    .await?;
                state.waiting = None;
            }
            Resolution::Approve { .. } | Resolution::Skip { .. } | Resolution::Expire { .. } => {
                // A decision already on record is never re-applied, and never
                // re-pointed at whatever else happens to be open. This is the
                // whole reason a resolution carries a step.
                if let Some(decision) = state.resolved_approvals.get(&step_seq).copied() {
                    return Err(Error::AlreadyResolved {
                        run: run_id,
                        step_seq,
                        decision,
                    });
                }
                let Some(pending) = state.pending_approval.clone() else {
                    return Err(Error::NoPendingApproval(run_id));
                };
                if pending.step_seq != step_seq {
                    return Err(Error::StepMismatch {
                        run: run_id,
                        expected: Some(pending.step_seq),
                        got: step_seq,
                    });
                }

                let now = self.now(&state);
                let intent = resolution
                    .decision()
                    .expect("approve, skip and expire all carry a decision");
                // EDGE: approval expired, and EDGE: clock skew. The comparison
                // carries an explicit leeway so a resolver whose clock runs
                // ahead of the writer's cannot reject a decision a human made in
                // time, and `now` is floored by the journal so one whose clock
                // runs behind cannot revive a decision that has objectively aged
                // out.
                let expired =
                    intent == Decision::Approve && self.is_expired(pending.expires_at, now);
                let decision = if expired { Decision::Expire } else { intent };
                let reason = expired.then(|| "ttl_expired".to_string());

                let resolved = ApprovalResolved {
                    step_seq,
                    decision,
                    resolved_at: now,
                    reason,
                };
                self.append(run_id, &mut state, EntryKind::ApprovalResolved, &resolved)
                    .await?;
                state.pending_approval = None;
                state.resolved_approvals.insert(step_seq, decision);
                // The terminal state a non-approval implies is now a fact about
                // the run, whether or not this process survives to record it.
                state.pending_terminal = terminal_for(decision);
            }
        }

        self.continue_run(run_id, &mut state).await
    }

    // ---- internals ---------------------------------------------------------

    /// Carry a replayed or freshly-resolved run to wherever it should stop.
    ///
    /// The single entry point into execution, so every caller applies the same
    /// precedence: a run that has already ended stays ended; a decision on
    /// record reaches its terminal state; a parked run is reported as-is; and
    /// only then does anything execute.
    async fn continue_run(&self, run_id: RunId, state: &mut RunState) -> Result<RunOutcome> {
        if let Some(ended) = &state.ended {
            return terminal_outcome(run_id, ended, state.stats());
        }
        // A committed Skip or Expire whose `run_ended` never landed. Without
        // this the human's durable decision would be unreachable: `resume` has
        // nothing pending to settle and the gated step must never execute.
        if let Some(status) = state.pending_terminal.take() {
            return self.end(run_id, state, status, None, None).await;
        }
        // A cap breach that was journaled but never finished with `run_ended`.
        if let Some(reason) = state.breached.take() {
            return self
                .end(run_id, state, RunStatus::Failed, None, Some(reason))
                .await;
        }
        if let Some(outcome) = self.parked_outcome(run_id, state) {
            return Ok(outcome);
        }
        self.drive(run_id, state).await
    }

    /// The outcome of a run that is parked on a human or a timer, if it is.
    fn parked_outcome(&self, run_id: RunId, state: &RunState) -> Option<RunOutcome> {
        if let Some(waiting) = &state.waiting {
            return Some(RunOutcome::Waiting {
                run_id,
                step_seq: waiting.step_seq,
                wake_at: waiting.wake_at,
                stats: state.stats(),
            });
        }
        state
            .pending_approval
            .as_ref()
            .map(|pending| pending_outcome(run_id, pending, state.stats()))
    }

    /// The current instant, never earlier than the newest fact in this run's
    /// journal.
    ///
    /// A wall clock that jumps backwards would otherwise make an approval that
    /// has objectively aged out approvable again. The journal is a monotone
    /// witness: whatever the local clock says, time is at least as late as the
    /// last durable entry. See
    /// [`EngineConfig::clock_skew_leeway`](EngineConfig::clock_skew_leeway).
    fn now(&self, state: &RunState) -> DateTime<Utc> {
        Utc::now().max(state.clock_floor)
    }

    fn is_expired(&self, expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        match expires_at {
            None => false,
            Some(at) => now > deadline(at, self.config.clock_skew_leeway),
        }
    }

    async fn append<P: Serialize>(
        &self,
        run_id: RunId,
        state: &mut RunState,
        kind: EntryKind,
        payload: &P,
    ) -> Result<Seq> {
        let seq = state
            .last_seq
            .checked_add(1)
            .ok_or_else(|| Error::journal("journal sequence overflowed"))?;
        let created_at = self.now(state);
        let entry = Entry::at(seq, kind, payload, created_at)?;
        let committed = self.journal.append(run_id, entry).await?;
        if committed != seq {
            return Err(Error::journal(format!(
                "journal committed seq {committed} but the engine allocated {seq}"
            )));
        }
        state.last_seq = committed;
        state.clock_floor = state.clock_floor.max(created_at);
        Ok(seq)
    }

    /// Record a terminal state and build the matching outcome.
    async fn end(
        &self,
        run_id: RunId,
        state: &mut RunState,
        status: RunStatus,
        output: Option<String>,
        reason: Option<FailureReason>,
    ) -> Result<RunOutcome> {
        let ended = RunEnded {
            status,
            output,
            reason,
            steps: state.steps_executed,
            usage: state.usage,
        };
        self.append(run_id, state, EntryKind::RunEnded, &ended)
            .await?;
        state.ended = Some(ended.clone());
        terminal_outcome(run_id, &ended, state.stats())
    }

    /// Journal a cap breach, then end the run failed.
    async fn breach(
        &self,
        run_id: RunId,
        state: &mut RunState,
        reason: FailureReason,
    ) -> Result<RunOutcome> {
        let breach = CapBreached {
            reason: reason.clone(),
        };
        self.append(run_id, state, EntryKind::CapBreached, &breach)
            .await?;
        state.breached = None;
        self.end(run_id, state, RunStatus::Failed, None, Some(reason))
            .await
    }

    async fn drive(&self, run_id: RunId, state: &mut RunState) -> Result<RunOutcome> {
        // EDGE: token budget already broken by a turn recorded before a crash.
        // The rule that ends a run is applied to the journal, not only to the
        // response as it arrives, so replay cannot smuggle in a tool step the
        // original turn had already forbidden.
        let spent = state.usage.total();
        if spent > self.config.token_budget {
            let reason = FailureReason::TokenBudgetExceeded {
                limit: self.config.token_budget,
                spent,
            };
            return self.breach(run_id, state, reason).await;
        }

        // Terminates because every iteration that does not return opens at
        // least one new step, and `max_steps` bounds how many may be opened.
        loop {
            if let Some(outcome) = self.advance_calls(run_id, state).await? {
                return Ok(outcome);
            }
            // A model turn with no tool calls **is** the run's answer, and it is
            // durable. Finishing from it is not an optimisation: asking again
            // would buy a fresh, nondeterministic response, and that response
            // could carry tool calls the recorded answer never had.
            if let Some(response) = state.final_response.take() {
                // EDGE: empty or whitespace-only model output is a finished run
                // with nothing to say, not a failure. The raw text stays in the
                // journal; only the outcome is trimmed.
                return self
                    .end(
                        run_id,
                        state,
                        RunStatus::Done,
                        final_output(&response),
                        None,
                    )
                    .await;
            }
            if let Some(outcome) = self.model_turn(run_id, state).await? {
                return Ok(outcome);
            }
        }
    }

    /// Work through the current turn's tool calls, in the order the model made
    /// them. Returns `Some` when the run must stop here.
    async fn advance_calls(
        &self,
        run_id: RunId,
        state: &mut RunState,
    ) -> Result<Option<RunOutcome>> {
        let mut index = 0;
        while index < state.calls.len() {
            let call = state.calls[index].clone();
            index += 1;

            match state.steps.get(&call.id).cloned() {
                // Already finished, in this attempt or a previous one.
                Some(record) if record.done => continue,

                // Opened before but never closed. Either a gated call whose
                // approval has now arrived, or — the case the whole protocol
                // exists for — a step whose `step_started` committed and whose
                // `step_done` did not. The engine cannot know whether the
                // effect landed, so it runs it again; the effect id is
                // unchanged, so a consumer that upserts ends up with one row.
                Some(record) => {
                    // The approval gate is enforced in `execute`, on the single
                    // path every dispatch takes — including the one below, for a
                    // step that was never gated at all.
                    if let Some(outcome) = self.execute(run_id, state, &call, record).await? {
                        return Ok(Some(outcome));
                    }
                }

                // Not opened yet.
                None => {
                    // EDGE: step cap reached exactly at the boundary. Checked
                    // before the step is opened, so `max_steps` steps run and
                    // the next one never starts — and a human is never asked to
                    // approve something that has no budget to run.
                    if state.steps_opened >= self.config.max_steps {
                        let reason = FailureReason::StepCapExceeded {
                            limit: self.config.max_steps,
                            taken: state.steps_opened,
                        };
                        return self.breach(run_id, state, reason).await.map(Some);
                    }

                    // Saturates rather than panicking in debug; `append`
                    // rejects the overflow properly a line later.
                    let step_seq = state.last_seq.saturating_add(1);
                    let eid = effect_id(run_id, step_seq);
                    let hash = args_hash(&call.arguments);
                    // Fixed once, here, and replayed verbatim on every later
                    // attempt at this step. It also raises the clock floor, so
                    // the entry that records the step cannot end up stamped
                    // *earlier* than the step it opens if the wall clock steps
                    // backwards between the two reads.
                    let opened_at = self.now(state);
                    state.clock_floor = state.clock_floor.max(opened_at);
                    let probe = ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        context: Some(CallContext {
                            run_id,
                            step_seq,
                            effect_id: eid,
                            replay: false,
                            opened_at,
                        }),
                    };

                    // EDGE: the model named a tool that does not exist. It is
                    // not dispatched, but it does open a step: an agent that
                    // hallucinates names must not get unlimited free retries.
                    let found = self.tools.get(&call.name);
                    let gated = found.is_some_and(|t| t.requires_approval(&probe));
                    let fingerprint = found.map(|t| tool_fingerprint(t.as_ref()));
                    // Fixed here, with the rest of the step's identity: the
                    // decision to refuse a blind retry must not depend on
                    // re-asking the tool after a crash. See `StepStarted`.
                    let policy = found.map_or(ReplayPolicy::default(), |t| t.replay_policy(&probe));

                    let record = StepRecord {
                        step_seq,
                        tool: call.name.clone(),
                        args: call.arguments.clone(),
                        args_hash: hash.clone(),
                        effect_id: eid,
                        attempt: 0,
                        started: false,
                        done: false,
                        gated,
                        opened_at,
                        tool_fingerprint: fingerprint.clone(),
                        replay_policy: policy,
                    };

                    if gated {
                        let expires_at =
                            self.config.approval_ttl.map(|ttl| deadline(opened_at, ttl));
                        let payload = ApprovalRequested {
                            step_seq,
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            args: call.arguments.clone(),
                            args_hash: hash,
                            effect_id: eid,
                            requested_at: opened_at,
                            expires_at,
                            tool_fingerprint: fingerprint,
                            replay_policy: policy,
                        };
                        let seq = self
                            .append(run_id, state, EntryKind::ApprovalRequested, &payload)
                            .await?;
                        debug_assert_eq!(seq, step_seq);
                        state.open_step(call.id.clone(), record);
                        state.pending_approval = Some(payload.clone());
                        // Nothing was executed and nothing will be until a
                        // resolution arrives through `resume`.
                        return Ok(Some(pending_outcome(run_id, &payload, state.stats())));
                    }

                    state.open_step(call.id.clone(), record.clone());
                    if let Some(outcome) = self.execute(run_id, state, &call, record).await? {
                        return Ok(Some(outcome));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Open an attempt at `record`'s step, run the tool, close the step.
    ///
    /// Returns `Some` only when the run stops here — parked on a timer, or
    /// halted because an interrupted step must not be retried.
    async fn execute(
        &self,
        run_id: RunId,
        state: &mut RunState,
        call: &ToolCall,
        record: StepRecord,
    ) -> Result<Option<RunOutcome>> {
        let replay = record.started;
        let attempt = record.attempt.saturating_add(1);

        // Every attempt at one step sees identical input; only `replay` moves.
        let dispatched = ToolCall {
            id: call.id.clone(),
            name: record.tool.clone(),
            arguments: record.args.clone(),
            context: Some(CallContext {
                run_id,
                step_seq: record.step_seq,
                effect_id: record.effect_id,
                replay,
                opened_at: record.opened_at,
            }),
        };

        let found = self.tools.get(&record.tool);
        if let Some(outcome) = self
            .check_step_still_means_what_it_meant(
                run_id,
                state,
                &record,
                found,
                &dispatched,
                replay,
            )
            .await?
        {
            return Ok(Some(outcome));
        }

        // Step 1 of the protocol: the fence. Once this commits, the engine must
        // assume the effect may exist.
        let started = StepStarted {
            step_seq: record.step_seq,
            call_id: call.id.clone(),
            tool: record.tool.clone(),
            // EDGE: arguments are journaled in full, not just hashed, so replay
            // never has to reconstruct them from an earlier entry. They are not
            // size-capped: they are the model's own output and are already
            // bounded by its token limit, whereas a tool result is bounded by
            // nothing.
            args: record.args.clone(),
            args_hash: record.args_hash.clone(),
            effect_id: record.effect_id,
            attempt,
            opened_at: record.opened_at,
            tool_fingerprint: record.tool_fingerprint.clone(),
            replay_policy: record.replay_policy,
        };
        self.append(run_id, state, EntryKind::StepStarted, &started)
            .await?;
        if !record.started {
            state.steps_executed += 1;
        }
        if let Some(entry) = state.steps.get_mut(&call.id) {
            entry.started = true;
            entry.attempt = attempt;
        }

        // Step 2: the effect.
        let (raw, is_error) = match found {
            Some(t) => dispatch(t, &dispatched).await,
            None => (
                tool::unknown_tool_error(&record.tool, &self.tools.names()),
                true,
            ),
        };

        // A tool may ask for the run to be parked. The envelope is stripped
        // here and never reaches the model.
        let (parked_until, raw) = match (is_error, control::parse(&raw)) {
            (false, Some((control::Control::Wait { until }, inner))) => (Some(until), inner),
            _ => (None, raw),
        };

        // EDGE: a tool that returns 10 MB. Capped before it can reach either
        // the journal or the prompt, and replaced by an explicit envelope so
        // nothing downstream mistakes a fragment for the whole answer.
        let (result, truncated) = tool::cap_result(raw, self.config.max_tool_result_bytes);

        // Step 3: close the step — and, in the same committed entry, record any
        // pause the tool asked for. A separate `run_waiting` append would leave
        // a window in which a crash loses the delay entirely.
        let done = StepDone {
            step_seq: record.step_seq,
            call_id: call.id.clone(),
            tool: record.tool.clone(),
            result,
            is_error,
            truncated,
            wake_at: parked_until,
        };
        self.append(run_id, state, EntryKind::StepDone, &done)
            .await?;

        state.messages.push(tool_message(&done));
        if let Some(entry) = state.steps.get_mut(&call.id) {
            entry.done = true;
        }

        if let Some(until) = parked_until {
            let waiting = RunWaiting {
                step_seq: record.step_seq,
                wake_at: until,
            };
            state.waiting = Some(waiting.clone());
            // A marker for readers of the journal. The wait is already durable
            // on `step_done`, so losing this append costs nothing but a row.
            self.append(run_id, state, EntryKind::RunWaiting, &waiting)
                .await?;
            return Ok(Some(RunOutcome::Waiting {
                run_id,
                step_seq: record.step_seq,
                wake_at: until,
                stats: state.stats(),
            }));
        }

        Ok(None)
    }

    /// The three conditions every dispatch must satisfy, checked in full on
    /// every attempt — including a re-dispatch after a crash.
    ///
    /// 1. **Same implementation.** The tool's fingerprint must equal the one
    ///    recorded when the step opened. Replay preserves a step's *name*, but a
    ///    deployment can change what that name does, and executing anyway would
    ///    run a different action under an `effect_id` minted for the old one.
    /// 2. **The gate still holds**, in both directions. A step may never execute
    ///    ungated if the tool now requires approval — so
    ///    [`Tool::requires_approval`] is re-evaluated unconditionally rather
    ///    than short-circuited for a step that happens to be gated already. And
    ///    a step opened gated must still hold a recorded [`Decision::Approve`],
    ///    which does not consult the tool at all: the human's yes is the
    ///    authority, not the current build's policy.
    /// 3. **The retry is permitted.** A step opened under
    ///    [`ReplayPolicy::Halt`] is never blind-retried, and neither is one
    ///    whose tool *now* says `Halt`. The recorded policy makes the decision
    ///    durable; the current one may only make it stricter, never looser.
    async fn check_step_still_means_what_it_meant(
        &self,
        run_id: RunId,
        state: &mut RunState,
        record: &StepRecord,
        found: Option<&Arc<dyn Tool>>,
        dispatched: &ToolCall,
        replay: bool,
    ) -> Result<Option<RunOutcome>> {
        // Rule 2, first half — and first of all, because it is the one that
        // asks whether anybody agreed to this happening. A step opened behind a
        // gate stays behind it for life, whatever the current build's policy
        // says: the human's yes is the authority. Reaching here without one
        // means the journal and the state machine disagree, and the safe
        // reading of that is "do not run the thing nobody agreed to".
        if record.gated
            && state.resolved_approvals.get(&record.step_seq) != Some(&Decision::Approve)
        {
            return Err(Error::CorruptJournal {
                run: run_id,
                message: format!(
                    "step {} was opened behind an approval gate but no approval is recorded",
                    record.step_seq
                ),
            });
        }

        let current = found.map(|t| tool_fingerprint(t.as_ref()));
        if current != record.tool_fingerprint {
            let describe = |f: &Option<String>| match f {
                Some(hash) => hash.clone(),
                None => "<no such tool>".to_string(),
            };
            return Err(Error::ToolChanged {
                run: run_id,
                step_seq: record.step_seq,
                tool: record.tool.clone(),
                detail: format!(
                    "fingerprint was {}, is now {}",
                    describe(&record.tool_fingerprint),
                    describe(&current)
                ),
            });
        }

        let Some(t) = found else { return Ok(None) };

        // Rule 2. Evaluated unconditionally, so the check says what it means
        // rather than relying on a caller having already covered one side.
        let needs_approval = t.requires_approval(dispatched);
        if needs_approval && !record.gated {
            // The part that matters most: a build that now gates this call must
            // not execute it just because an older build opened the step
            // ungated.
            return Err(Error::ToolChanged {
                run: run_id,
                step_seq: record.step_seq,
                tool: record.tool.clone(),
                detail: "the tool now requires approval, but this step was opened without a gate"
                    .to_string(),
            });
        }
        // Rule 3. An interrupted attempt at a tool that declared itself unsafe
        // to repeat. The engine will not guess; it stops and quotes the effect
        // id so the host can reconcile against its own records.
        //
        // The policy that decides this was fixed when the step opened and is on
        // the journal, so the decision cannot flip if a redeployment — or a
        // non-deterministic implementation — later answers differently. Asking
        // the tool again can only add a halt, never remove one.
        if replay
            && (record.replay_policy == ReplayPolicy::Halt
                || t.replay_policy(dispatched) == ReplayPolicy::Halt)
        {
            let reason = FailureReason::AmbiguousEffect {
                tool: record.tool.clone(),
                step_seq: record.step_seq,
                effect_id: record.effect_id,
            };
            // Journaled as a breach first, so the refusal is durable even if the
            // `run_ended` that follows it does not commit.
            return self.breach(run_id, state, reason).await.map(Some);
        }

        Ok(None)
    }

    /// Ask the model for one turn. Returns `Some` when the run ends here.
    async fn model_turn(&self, run_id: RunId, state: &mut RunState) -> Result<Option<RunOutcome>> {
        // EDGE: token budget exhausted before the next turn. Refuse to spend
        // what is not there rather than discovering it afterwards.
        let spent = state.usage.total();
        if spent >= self.config.token_budget {
            let reason = FailureReason::TokenBudgetExceeded {
                limit: self.config.token_budget,
                spent,
            };
            return self.breach(run_id, state, reason).await.map(Some);
        }

        let request = ChatRequest {
            messages: state.messages.clone(),
            tools: self.tools.schemas(),
            model: self.config.model.clone(),
            max_output_tokens: self.config.max_output_tokens,
            temperature: self.config.temperature,
        };
        let response = self.llm.chat(request).await?;

        let turn = state.turn + 1;
        // EDGE: a model that emits blank or repeated call ids. Normalised here,
        // once, before anything is journaled — so replay always matches results
        // to calls the same way.
        let calls = normalise_call_ids(&mut state.seen_call_ids, turn, response.tool_calls);

        let payload = ModelResponse {
            turn,
            text: response.text.clone(),
            tool_calls: calls.clone(),
            stop_reason: response.stop_reason.clone(),
            usage: response.usage,
        };
        self.append(run_id, state, EntryKind::ModelResponse, &payload)
            .await?;

        state.turn = turn;
        state.usage.add(response.usage);
        state.messages.push(Message::Assistant {
            text: response.text.clone(),
            tool_calls: calls.clone(),
        });
        state.calls = calls;
        // EDGE: `stop_reason: tool_use` with no calls is treated as the end of
        // the run, which is also what stops a model from looping on empty turns
        // forever. `drive` settles it, by exactly the path replay takes.
        state.final_response = state.calls.is_empty().then_some(payload);

        // EDGE: token budget exceeded by the turn just received. Spending
        // exactly the budget is allowed; a single token more is not.
        let spent = state.usage.total();
        if spent > self.config.token_budget {
            let reason = FailureReason::TokenBudgetExceeded {
                limit: self.config.token_budget,
                spent,
            };
            state.final_response = None;
            return self.breach(run_id, state, reason).await.map(Some);
        }

        Ok(None)
    }
}

/// Run a tool, converting every failure mode into a structured result the model
/// can read.
async fn dispatch(tool: &Arc<dyn Tool>, call: &ToolCall) -> (Value, bool) {
    let name = tool.name().to_string();

    // EDGE: a tool that panics. Unwinding out of a run would strand it with a
    // committed `step_started` and no `step_done`, so the panic is caught and
    // reported like any other tool failure. (A build with `panic = "abort"`
    // cannot catch it; that is a property of the profile, not of this crate.)
    //
    // Boxing first makes the future `Unpin`, so the projection below needs no
    // unsafe code — and no futures crate.
    let mut future = Box::pin(tool.execute(call));
    let outcome =
        poll_fn(
            move |cx| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
                // Report and stop; the future is dropped without being polled again.
                Err(panic) => Poll::Ready(Err(panic)),
            },
        )
        .await;

    match outcome {
        Ok(Ok(value)) => (value, false),
        Ok(Err(err)) => (tool::tool_error(err.code(), &name, &err.to_string()), true),
        Err(panic) => (
            tool::tool_error("tool_panicked", &name, &panic_message(panic)),
            true,
        ),
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "tool panicked".to_string()
    }
}

/// Give every call in a turn an id that is unique within the run.
fn normalise_call_ids(
    seen: &mut HashSet<String>,
    turn: u32,
    calls: Vec<ToolCall>,
) -> Vec<ToolCall> {
    let mut out = Vec::with_capacity(calls.len());
    for (index, mut call) in calls.into_iter().enumerate() {
        let trimmed = call.id.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
            call.id = trimmed.to_string();
        } else {
            let mut candidate = format!("call_{turn}_{index}");
            let mut suffix = 0u32;
            while !seen.insert(candidate.clone()) {
                suffix += 1;
                candidate = format!("call_{turn}_{index}_{suffix}");
            }
            call.id = candidate;
        }
        out.push(call);
    }
    out
}

fn tool_message(done: &StepDone) -> Message {
    Message::Tool {
        call_id: done.call_id.clone(),
        name: done.tool.clone(),
        result: done.result.clone(),
        is_error: done.is_error,
    }
}

/// The run's closing prose: the recorded text, trimmed, or nothing.
fn final_output(response: &ModelResponse) -> Option<String> {
    response
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// The terminal state a decision implies, if it ends the run.
fn terminal_for(decision: Decision) -> Option<RunStatus> {
    match decision {
        Decision::Approve => None,
        Decision::Skip => Some(RunStatus::Skipped),
        Decision::Expire => Some(RunStatus::Expired),
    }
}

fn pending_outcome(run_id: RunId, pending: &ApprovalRequested, stats: RunStats) -> RunOutcome {
    let call = ToolCall {
        id: pending.call_id.clone(),
        name: pending.tool.clone(),
        arguments: pending.args.clone(),
        context: Some(CallContext {
            run_id,
            step_seq: pending.step_seq,
            effect_id: pending.effect_id,
            replay: false,
            opened_at: pending.requested_at,
        }),
    };
    RunOutcome::PendingApproval {
        run_id,
        request: Box::new(ApprovalRequest {
            step_seq: pending.step_seq,
            tool: pending.tool.clone(),
            call,
            args_hash: pending.args_hash.clone(),
            effect_id: pending.effect_id,
            requested_at: pending.requested_at,
            expires_at: pending.expires_at,
        }),
        stats,
    }
}

fn terminal_outcome(run_id: RunId, ended: &RunEnded, stats: RunStats) -> Result<RunOutcome> {
    Ok(match ended.status {
        RunStatus::Done => RunOutcome::Done {
            run_id,
            output: ended.output.clone(),
            stats,
        },
        RunStatus::Skipped => RunOutcome::Skipped { run_id, stats },
        RunStatus::Expired => RunOutcome::Expired { run_id, stats },
        RunStatus::Failed => {
            let reason = ended.reason.clone().ok_or_else(|| Error::CorruptJournal {
                run: run_id,
                message: "'run_ended' says failed but records no reason".to_string(),
            })?;
            RunOutcome::Failed {
                run_id,
                reason,
                stats,
            }
        }
        // A `run_ended` claiming a state the run could still leave can only come
        // from a hand-edited or foreign journal.
        RunStatus::PendingApproval | RunStatus::Waiting => {
            return Err(Error::CorruptJournal {
                run: run_id,
                message: format!(
                    "'run_ended' records the non-terminal status '{}'",
                    ended.status.as_str()
                ),
            })
        }
    })
}

/// What the engine knows about one tool step.
#[derive(Clone, Debug)]
struct StepRecord {
    step_seq: Seq,
    tool: String,
    args: Value,
    args_hash: String,
    effect_id: Uuid,
    attempt: u32,
    /// A `step_started` for this step has committed, so its effect may exist.
    started: bool,
    /// A `step_done` for this step has committed, so it is finished for good.
    done: bool,
    /// Opened by `approval_requested` rather than `step_started`.
    gated: bool,
    /// Fixed when the step was opened; every attempt sees this value.
    opened_at: DateTime<Utc>,
    /// The implementation the step was opened under.
    tool_fingerprint: Option<String>,
    /// The retry policy the tool declared when the step was opened. Durable, so
    /// a halt cannot be undone by re-asking the tool.
    replay_policy: ReplayPolicy,
}

/// Everything about a run, rebuilt from its journal.
#[derive(Debug)]
struct RunState {
    messages: Vec<Message>,
    usage: TokenUsage,
    turn: u32,
    last_seq: Seq,
    /// Distinct steps opened. Bounds `max_steps`.
    steps_opened: u32,
    /// Distinct steps that reached execution. Reported to the host.
    steps_executed: u32,
    /// The current turn's calls, in the order the model made them.
    calls: Vec<ToolCall>,
    /// One record per step, keyed by the (normalised) call id.
    steps: HashMap<String, StepRecord>,
    /// Step sequence to call id, so an entry naming a step can be checked
    /// against the entry that opened it.
    steps_by_seq: HashMap<Seq, String>,
    seen_call_ids: HashSet<String>,
    pending_approval: Option<ApprovalRequested>,
    resolved_approvals: HashMap<Seq, Decision>,
    waiting: Option<RunWaiting>,
    ended: Option<RunEnded>,
    /// A cap breach on record whose `run_ended` never committed.
    breached: Option<FailureReason>,
    /// A Skip or Expire on record whose `run_ended` never committed.
    pending_terminal: Option<RunStatus>,
    /// The last model turn asked for no tools, so it is the run's answer.
    final_response: Option<ModelResponse>,
    /// The newest `created_at` in the journal: a lower bound on "now".
    clock_floor: DateTime<Utc>,
}

impl RunState {
    fn fresh(input: &RunInput) -> Self {
        Self {
            messages: input.seed(),
            usage: TokenUsage::default(),
            turn: 0,
            last_seq: 0,
            steps_opened: 0,
            steps_executed: 0,
            calls: Vec::new(),
            steps: HashMap::new(),
            steps_by_seq: HashMap::new(),
            seen_call_ids: HashSet::new(),
            pending_approval: None,
            resolved_approvals: HashMap::new(),
            waiting: None,
            ended: None,
            breached: None,
            pending_terminal: None,
            final_response: None,
            clock_floor: DateTime::<Utc>::MIN_UTC,
        }
    }

    fn stats(&self) -> RunStats {
        RunStats {
            steps: self.steps_executed,
            usage: self.usage,
            last_seq: self.last_seq,
        }
    }

    /// Rebuild a run from its journal, refusing anything that does not add up.
    ///
    /// The conversation is reconstructed from the recorded model turns and tool
    /// results, so resuming never pays a provider for a turn it already bought.
    ///
    /// Validation is not decoration. An entry whose `effect_id` or `step_seq`
    /// disagrees with the sequence that opened its step would put a *different*
    /// identity on a side effect, which is the one thing this crate exists to
    /// prevent — so every such claim is checked here, before anything can be
    /// dispatched.
    ///
    /// The distinction that governs what is worth checking: a field is either
    /// **compared against something the engine wrote earlier** or merely
    /// **recomputed from the payload asserting it**. Only the first kind is
    /// checked at all — a self-consistent forgery satisfies the second — which
    /// is why a step's tool and arguments are resolved back to the model call
    /// that requested them rather than taken from the entry that claims them.
    fn replay(run_id: RunId, entries: &[Entry], config: &EngineConfig) -> Result<Self> {
        let corrupt = |message: String| Error::CorruptJournal {
            run: run_id,
            message,
        };

        let Some(first) = entries.first() else {
            return Err(corrupt("journal is empty".to_string()));
        };
        if first.kind != EntryKind::RunStarted {
            return Err(corrupt(format!(
                "first entry is '{}', expected 'run_started'",
                first.kind
            )));
        }
        let started: RunStarted = first.payload_as(run_id)?;
        // Before anything else is decoded: a journal in a format this build does
        // not write is refused as such, rather than as whichever payload field
        // happens to be missing first.
        if started.journal_format != JOURNAL_FORMAT {
            return Err(Error::UnsupportedJournalFormat {
                run: run_id,
                found: started.journal_format,
                expected: JOURNAL_FORMAT,
            });
        }
        let mut state = Self::fresh(&started.input);

        // One reading of the wall clock for the whole replay, so the bound below
        // cannot drift between entries.
        let ceiling = config
            .max_journal_clock_drift
            .map(|drift| deadline(Utc::now(), drift));

        for (index, entry) in entries.iter().enumerate() {
            // Sequences start at 1 and increase by exactly one: the journal is
            // the ordered, gapless log the engine allocates against, and a gap
            // means entries are missing rather than merely out of order.
            let expected = state
                .last_seq
                .checked_add(1)
                .ok_or_else(|| corrupt("journal sequence overflowed".to_string()))?;
            if entry.seq != expected {
                return Err(corrupt(format!(
                    "entry {} of the journal has seq {}, expected {expected}",
                    index + 1,
                    entry.seq
                )));
            }
            // Nothing follows a terminal state.
            if state.ended.is_some() {
                return Err(corrupt(format!(
                    "entry {} ('{}') follows 'run_ended'",
                    entry.seq, entry.kind
                )));
            }
            // The clock floor is only as trustworthy as the timestamps it is
            // built from, and it is permanent: one entry from the future would
            // displace this run's clock — and every deadline it goes on to
            // compute — for the rest of its life. Bound it.
            if let Some(ceiling) = ceiling {
                if entry.created_at > ceiling {
                    return Err(corrupt(format!(
                        "entry {} is stamped {}, too far in the future for this engine to \
                         accept as a lower bound on the current time",
                        entry.seq,
                        entry.created_at.to_rfc3339()
                    )));
                }
            }
            state.last_seq = entry.seq;
            state.clock_floor = state.clock_floor.max(entry.created_at);

            match entry.kind {
                EntryKind::RunStarted => {
                    if index != 0 {
                        return Err(corrupt(format!(
                            "entry {} is a second 'run_started'",
                            entry.seq
                        )));
                    }
                }
                EntryKind::ModelResponse => {
                    let payload: ModelResponse = entry.payload_as(run_id)?;
                    if payload.turn != state.turn + 1 {
                        return Err(corrupt(format!(
                            "entry {} is turn {} but the run is on turn {}",
                            entry.seq, payload.turn, state.turn
                        )));
                    }
                    // A turn with no tool calls **is** the run's answer, so the
                    // run ends there. Anything after it would reopen a finished
                    // run — and could dispatch whatever it liked.
                    if state.final_response.is_some() {
                        return Err(corrupt(format!(
                            "entry {} is a model turn after the turn that answered the run",
                            entry.seq
                        )));
                    }
                    // And a turn is never bought while a call of the previous
                    // one is unfinished: the engine completes or retries every
                    // call before asking for another, so a journal that skips
                    // one is a journal that silently drops a step whose effect
                    // may already exist.
                    if let Some(unfinished) = state.first_unfinished_call() {
                        return Err(corrupt(format!(
                            "entry {} opens a model turn while call '{unfinished}' of the \
                             previous turn is unfinished",
                            entry.seq
                        )));
                    }
                    state.turn = payload.turn;
                    state.usage.add(payload.usage);
                    for call in &payload.tool_calls {
                        if !state.seen_call_ids.insert(call.id.clone()) {
                            return Err(corrupt(format!(
                                "entry {} reuses call id '{}'",
                                entry.seq, call.id
                            )));
                        }
                    }
                    state.messages.push(Message::Assistant {
                        text: payload.text.clone(),
                        tool_calls: payload.tool_calls.clone(),
                    });
                    state.calls = payload.tool_calls.clone();
                    // A turn with no calls is the run's recorded answer. If no
                    // `run_ended` follows, that answer still finishes the run.
                    state.final_response = payload.tool_calls.is_empty().then_some(payload);
                }
                EntryKind::ApprovalRequested => {
                    let payload: ApprovalRequested = entry.payload_as(run_id)?;
                    check_identity(
                        run_id,
                        entry.seq,
                        payload.step_seq,
                        Some(entry.seq),
                        &payload.args,
                        &payload.args_hash,
                        payload.effect_id,
                    )?;
                    state.check_opens_a_real_call(
                        run_id,
                        entry.seq,
                        &payload.call_id,
                        &payload.tool,
                        &payload.args,
                    )?;
                    if payload.requested_at > entry.created_at {
                        return Err(corrupt(format!(
                            "entry {} says its step opened after the entry recording it was \
                             created",
                            entry.seq
                        )));
                    }
                    if payload
                        .expires_at
                        .is_some_and(|at| at < payload.requested_at)
                    {
                        return Err(corrupt(format!(
                            "entry {} expires its approval before it was requested",
                            entry.seq
                        )));
                    }
                    if state.steps.contains_key(&payload.call_id) {
                        return Err(corrupt(format!(
                            "entry {} re-opens call '{}'",
                            entry.seq, payload.call_id
                        )));
                    }
                    if let Some(open) = &state.pending_approval {
                        return Err(corrupt(format!(
                            "entry {} asks for a second approval while step {} is still pending",
                            entry.seq, open.step_seq
                        )));
                    }
                    state.open_step(
                        payload.call_id.clone(),
                        StepRecord {
                            step_seq: payload.step_seq,
                            tool: payload.tool.clone(),
                            args: payload.args.clone(),
                            args_hash: payload.args_hash.clone(),
                            effect_id: payload.effect_id,
                            attempt: 0,
                            started: false,
                            done: false,
                            gated: true,
                            opened_at: payload.requested_at,
                            tool_fingerprint: payload.tool_fingerprint.clone(),
                            replay_policy: payload.replay_policy,
                        },
                    );
                    state.pending_approval = Some(payload);
                }
                EntryKind::ApprovalResolved => {
                    let payload: ApprovalResolved = entry.payload_as(run_id)?;
                    // Exactly one resolution per approval, and only for the one
                    // that is open: anything else would settle a step that no
                    // longer — or never did — await a decision.
                    let Some(pending) = state.pending_approval.take() else {
                        return Err(corrupt(format!(
                            "entry {} resolves step {}, but no approval is pending",
                            entry.seq, payload.step_seq
                        )));
                    };
                    if pending.step_seq != payload.step_seq {
                        return Err(corrupt(format!(
                            "entry {} resolves step {}, but step {} is the one pending",
                            entry.seq, payload.step_seq, pending.step_seq
                        )));
                    }
                    state
                        .resolved_approvals
                        .insert(payload.step_seq, payload.decision);
                    state.pending_terminal = terminal_for(payload.decision);
                }
                EntryKind::StepStarted => {
                    let payload: StepStarted = entry.payload_as(run_id)?;
                    check_identity(
                        run_id,
                        entry.seq,
                        payload.step_seq,
                        None,
                        &payload.args,
                        &payload.args_hash,
                        payload.effect_id,
                    )?;
                    if payload.opened_at > entry.created_at {
                        return Err(corrupt(format!(
                            "entry {} says its step opened after the entry recording it was \
                             created",
                            entry.seq
                        )));
                    }
                    match state.steps.get(&payload.call_id).cloned() {
                        // Either a gated step that has now been approved, or —
                        // when `started` is already set — a retry after a crash.
                        // A retry is not a new step and must not consume budget
                        // a second time, nor may it disagree with the record it
                        // is retrying.
                        Some(existing) => {
                            if let Some(message) = existing.disagreement(&payload) {
                                return Err(corrupt(format!(
                                    "entry {} disagrees with the entry that opened step {}: {message}",
                                    entry.seq, existing.step_seq
                                )));
                            }
                            if existing.done {
                                return Err(corrupt(format!(
                                    "entry {} re-opens step {}, which already completed",
                                    entry.seq, existing.step_seq
                                )));
                            }
                            if payload.attempt <= existing.attempt {
                                return Err(corrupt(format!(
                                    "entry {} is attempt {} of step {}, which is already at {}",
                                    entry.seq, payload.attempt, existing.step_seq, existing.attempt
                                )));
                            }
                            if existing.gated
                                && state.resolved_approvals.get(&existing.step_seq)
                                    != Some(&Decision::Approve)
                            {
                                return Err(corrupt(format!(
                                    "entry {} starts gated step {} with no approval on record",
                                    entry.seq, existing.step_seq
                                )));
                            }
                            let first_attempt = !existing.started;
                            let record = state
                                .steps
                                .get_mut(&payload.call_id)
                                .expect("the record was just read");
                            record.started = true;
                            record.attempt = payload.attempt;
                            if first_attempt {
                                state.steps_executed += 1;
                            }
                        }
                        // A step opening for the first time. This is where the
                        // entry is bound to the model call it answers; a retry
                        // is bound transitively, by having to agree with the
                        // record this branch created.
                        None => {
                            state.check_opens_a_real_call(
                                run_id,
                                entry.seq,
                                &payload.call_id,
                                &payload.tool,
                                &payload.args,
                            )?;
                            if payload.step_seq != entry.seq {
                                return Err(corrupt(format!(
                                    "entry {} opens a step but claims step_seq {}",
                                    entry.seq, payload.step_seq
                                )));
                            }
                            if payload.attempt != 1 {
                                return Err(corrupt(format!(
                                    "entry {} opens step {} at attempt {}, expected 1",
                                    entry.seq, payload.step_seq, payload.attempt
                                )));
                            }
                            state.open_step(
                                payload.call_id.clone(),
                                StepRecord {
                                    step_seq: payload.step_seq,
                                    tool: payload.tool.clone(),
                                    args: payload.args.clone(),
                                    args_hash: payload.args_hash.clone(),
                                    effect_id: payload.effect_id,
                                    attempt: payload.attempt,
                                    started: true,
                                    done: false,
                                    gated: false,
                                    opened_at: payload.opened_at,
                                    tool_fingerprint: payload.tool_fingerprint.clone(),
                                    replay_policy: payload.replay_policy,
                                },
                            );
                            state.steps_executed += 1;
                        }
                    }
                }
                EntryKind::StepDone => {
                    let payload: StepDone = entry.payload_as(run_id)?;
                    let Some(existing) = state.steps.get_mut(&payload.call_id) else {
                        return Err(corrupt(format!(
                            "entry {} closes call '{}', which was never opened",
                            entry.seq, payload.call_id
                        )));
                    };
                    if existing.step_seq != payload.step_seq || existing.tool != payload.tool {
                        return Err(corrupt(format!(
                            "entry {} closes step {} ('{}') but call '{}' is step {} ('{}')",
                            entry.seq,
                            payload.step_seq,
                            payload.tool,
                            payload.call_id,
                            existing.step_seq,
                            existing.tool
                        )));
                    }
                    if !existing.started {
                        return Err(corrupt(format!(
                            "entry {} closes step {}, which never started",
                            entry.seq, payload.step_seq
                        )));
                    }
                    if existing.done {
                        return Err(corrupt(format!(
                            "entry {} closes step {} twice",
                            entry.seq, payload.step_seq
                        )));
                    }
                    existing.done = true;
                    state.messages.push(tool_message(&payload));
                    // The wait is durable here, with the step that asked for it.
                    if let Some(wake_at) = payload.wake_at {
                        state.waiting = Some(RunWaiting {
                            step_seq: payload.step_seq,
                            wake_at,
                        });
                    }
                }
                EntryKind::RunWaiting => {
                    let payload: RunWaiting = entry.payload_as(run_id)?;
                    let step = state.step_at(payload.step_seq).ok_or_else(|| {
                        corrupt(format!(
                            "entry {} parks on step {}, which was never opened",
                            entry.seq, payload.step_seq
                        ))
                    })?;
                    if !step.done {
                        return Err(corrupt(format!(
                            "entry {} parks on step {}, which has not completed",
                            entry.seq, payload.step_seq
                        )));
                    }
                    if let Some(current) = &state.waiting {
                        if current != &payload {
                            return Err(corrupt(format!(
                                "entry {} disagrees with the wait recorded on step {}",
                                entry.seq, current.step_seq
                            )));
                        }
                    }
                    state.waiting = Some(payload);
                }
                EntryKind::RunWoken => {
                    let payload: RunWoken = entry.payload_as(run_id)?;
                    let Some(waiting) = state.waiting.take() else {
                        return Err(corrupt(format!(
                            "entry {} wakes a run that is not parked",
                            entry.seq
                        )));
                    };
                    if waiting.step_seq != payload.step_seq {
                        return Err(corrupt(format!(
                            "entry {} wakes step {}, but the run is parked on step {}",
                            entry.seq, payload.step_seq, waiting.step_seq
                        )));
                    }
                }
                EntryKind::CapBreached => {
                    let payload: CapBreached = entry.payload_as(run_id)?;
                    state.breached = Some(payload.reason);
                }
                EntryKind::RunEnded => {
                    state.ended = Some(entry.payload_as(run_id)?);
                    // The terminal entry supersedes anything it was recording.
                    state.pending_terminal = None;
                    state.breached = None;
                    state.final_response = None;
                }
                EntryKind::Other(_) => {
                    return Err(corrupt(format!(
                        "entry {} has kind '{}', which this version does not understand",
                        entry.seq, entry.kind
                    )));
                }
            }
        }

        Ok(state)
    }

    fn open_step(&mut self, call_id: String, record: StepRecord) {
        self.seen_call_ids.insert(call_id.clone());
        self.steps_by_seq.insert(record.step_seq, call_id.clone());
        if self.steps.insert(call_id, record).is_none() {
            self.steps_opened += 1;
        }
    }

    /// The step opened at `seq`, if there is one.
    fn step_at(&self, seq: Seq) -> Option<&StepRecord> {
        self.steps_by_seq
            .get(&seq)
            .and_then(|call_id| self.steps.get(call_id))
    }

    /// The first call of the current turn that has not completed — never opened,
    /// opened and unfinished, or parked on a human. `None` when the turn is
    /// fully settled and the run may legitimately buy another.
    fn first_unfinished_call(&self) -> Option<&str> {
        self.calls
            .iter()
            .find_map(|call| match self.steps.get(&call.id) {
                Some(record) if record.done => None,
                _ => Some(call.id.as_str()),
            })
    }

    /// Bind an entry that opens a step to the model call it claims to answer.
    ///
    /// **This is the check the rest of the integrity work rests on.** Everything
    /// else about an opening entry — its `effect_id`, its `args_hash` — is
    /// derived from the entry's own payload and is therefore satisfied by any
    /// self-consistent forgery. Only the tool name and the arguments can be
    /// compared against something the engine wrote earlier, and if they are not,
    /// a journal can keep a real model call's id while substituting a different
    /// action, and replay will dispatch the substitution.
    ///
    /// The call must belong to the **current** turn, not merely to some turn:
    /// the engine only ever opens steps for the calls of the turn it is working
    /// through, so an id borrowed from an earlier one is as foreign as an
    /// invented one.
    fn check_opens_a_real_call(
        &self,
        run_id: RunId,
        seq: Seq,
        call_id: &str,
        tool: &str,
        args: &Value,
    ) -> Result<()> {
        let corrupt = |message: String| Error::CorruptJournal {
            run: run_id,
            message,
        };
        let Some(call) = self.calls.iter().find(|c| c.id == call_id) else {
            return Err(if self.seen_call_ids.contains(call_id) {
                corrupt(format!(
                    "entry {seq} opens a step for call '{call_id}', which belongs to an \
                     earlier turn"
                ))
            } else {
                corrupt(format!(
                    "entry {seq} opens a step for call '{call_id}', which no model turn produced"
                ))
            });
        };
        if call.name != tool {
            return Err(corrupt(format!(
                "entry {seq} opens call '{call_id}' as tool '{tool}', but the model asked for \
                 '{}'",
                call.name
            )));
        }
        if &call.arguments != args {
            return Err(corrupt(format!(
                "entry {seq} opens call '{call_id}' with arguments the model did not ask for"
            )));
        }
        Ok(())
    }
}

impl StepRecord {
    /// How a repeated `step_started` differs from the entry that opened this
    /// step, if it does. Everything but `attempt` must be identical: a retry is
    /// the *same* action, not a new one wearing the same sequence number.
    fn disagreement(&self, payload: &StepStarted) -> Option<String> {
        if self.step_seq != payload.step_seq {
            return Some(format!(
                "step_seq {} != {}",
                payload.step_seq, self.step_seq
            ));
        }
        if self.tool != payload.tool {
            return Some(format!("tool '{}' != '{}'", payload.tool, self.tool));
        }
        if self.args != payload.args {
            return Some("arguments differ".to_string());
        }
        if self.args_hash != payload.args_hash {
            return Some(format!(
                "args_hash {} != {}",
                payload.args_hash, self.args_hash
            ));
        }
        if self.effect_id != payload.effect_id {
            return Some(format!(
                "effect_id {} != {}",
                payload.effect_id, self.effect_id
            ));
        }
        if self.opened_at != payload.opened_at {
            return Some("opened_at differs".to_string());
        }
        if self.tool_fingerprint != payload.tool_fingerprint {
            return Some("tool fingerprint differs".to_string());
        }
        if self.replay_policy != payload.replay_policy {
            return Some("replay policy differs".to_string());
        }
        None
    }
}

/// Check the claims an entry makes about the step it belongs to.
///
/// `must_open_at` is `Some(seq)` for an entry that is required to *open* its
/// step, so its `step_seq` has to be its own sequence.
fn check_identity(
    run_id: RunId,
    seq: Seq,
    step_seq: Seq,
    must_open_at: Option<Seq>,
    args: &Value,
    hash: &str,
    eid: Uuid,
) -> Result<()> {
    let corrupt = |message: String| Error::CorruptJournal {
        run: run_id,
        message,
    };
    if let Some(own) = must_open_at {
        if step_seq != own {
            return Err(corrupt(format!(
                "entry {seq} opens a step but claims step_seq {step_seq}"
            )));
        }
    }
    let expected = effect_id(run_id, step_seq);
    if eid != expected {
        return Err(corrupt(format!(
            "entry {seq} claims effect id {eid}, but step {step_seq} derives {expected}"
        )));
    }
    let recomputed = args_hash(args);
    if hash != recomputed {
        return Err(corrupt(format!(
            "entry {seq} claims args_hash {hash}, but its arguments hash to {recomputed}"
        )));
    }
    Ok(())
}
