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
    RunEnded, RunStarted, RunWaiting, RunWoken, StepDone, StepStarted,
};
use crate::llm::Llm;
use crate::message::{CallContext, ChatRequest, Message, TokenUsage, ToolCall};
use crate::run::{
    ApprovalRequest, Decision, FailureReason, Resolution, RunInput, RunOutcome, RunStats, RunStatus,
};
use crate::tool::{self, control, Tool, ToolSet};

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
    /// call the model at all). Spending *exactly* the budget is allowed.
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

    /// Grace added to every expiry comparison, to absorb clock skew between the
    /// machine that wrote the approval and the one that resolves it.
    ///
    /// An approval is refused only when `now > expires_at + leeway`.
    ///
    /// Default `60s`.
    pub clock_skew_leeway: Duration,

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
/// implements.
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
    /// This is the call a job worker makes, including after a crash. It is safe
    /// to make it repeatedly:
    ///
    /// * an empty journal starts the run and records `input`;
    /// * a journal that already has entries is **replayed**, and the `input`
    ///   argument is ignored in favour of the one recorded at the start;
    /// * a run that is finished, waiting on a human, or parked on a timer is
    ///   returned as-is without appending anything or executing anything.
    ///
    /// Errors are transport failures — an unreachable model, a journal that
    /// will not commit. They leave the run exactly where its journal says it
    /// is, and the host should retry.
    pub async fn run(&self, run_id: RunId, input: impl Into<RunInput>) -> Result<RunOutcome> {
        let entries = self.journal.load(run_id).await?;
        let mut state = if entries.is_empty() {
            let input = input.into();
            let mut state = RunState::fresh(&input);
            self.append(
                run_id,
                &mut state,
                EntryKind::RunStarted,
                &RunStarted { input },
            )
            .await?;
            state
        } else {
            RunState::replay(run_id, &entries)?
        };

        if let Some(outcome) = self.parked_outcome(run_id, &state)? {
            return Ok(outcome);
        }
        self.drive(run_id, &mut state).await
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
    /// Resolving a run that has already finished is a no-op: the recorded
    /// terminal outcome is returned and nothing is appended. That is what makes
    /// a replayed approval — the same button pressed twice, the same webhook
    /// delivered twice — harmless.
    pub async fn resume(&self, run_id: RunId, resolution: Resolution) -> Result<RunOutcome> {
        let entries = self.journal.load(run_id).await?;
        if entries.is_empty() {
            // Resolving a run that was never started. Report the mismatch the
            // caller actually made rather than "corrupt journal".
            return Err(match resolution {
                Resolution::Timer => Error::NotWaiting(run_id),
                _ => Error::NoPendingApproval(run_id),
            });
        }
        let mut state = RunState::replay(run_id, &entries)?;

        // EDGE: duplicate resume of a finished run. The terminal entry is the
        // authority; no second run, no second effect, no new journal entries.
        if let Some(ended) = &state.ended {
            return terminal_outcome(run_id, ended, state.stats());
        }

        match resolution {
            Resolution::Timer => {
                if state.waiting.is_none() {
                    return Err(Error::NotWaiting(run_id));
                }
                let woken = RunWoken {
                    woken_at: Utc::now(),
                };
                self.append(run_id, &mut state, EntryKind::RunWoken, &woken)
                    .await?;
                state.waiting = None;
            }
            Resolution::Approve | Resolution::Skip | Resolution::Expire => {
                let Some(pending) = state.pending_approval.clone() else {
                    return Err(Error::NoPendingApproval(run_id));
                };

                let decision = match resolution {
                    Resolution::Skip => Decision::Skip,
                    Resolution::Expire => Decision::Expire,
                    // EDGE: approval expired, and EDGE: clock skew. The
                    // comparison carries an explicit leeway so a resolver whose
                    // clock runs ahead of the writer's cannot reject a decision
                    // a human made in time.
                    Resolution::Approve if self.is_expired(pending.expires_at, Utc::now()) => {
                        Decision::Expire
                    }
                    _ => Decision::Approve,
                };
                let reason = (decision == Decision::Expire && resolution == Resolution::Approve)
                    .then(|| "ttl_expired".to_string());

                let resolved = ApprovalResolved {
                    step_seq: pending.step_seq,
                    decision,
                    resolved_at: Utc::now(),
                    reason,
                };
                self.append(run_id, &mut state, EntryKind::ApprovalResolved, &resolved)
                    .await?;
                state.pending_approval = None;
                state.resolved_approvals.insert(pending.step_seq, decision);

                match decision {
                    Decision::Skip => {
                        return self
                            .end(run_id, &mut state, RunStatus::Skipped, None, None)
                            .await
                    }
                    Decision::Expire => {
                        return self
                            .end(run_id, &mut state, RunStatus::Expired, None, None)
                            .await
                    }
                    Decision::Approve => {}
                }
            }
        }

        self.drive(run_id, &mut state).await
    }

    // ---- internals ---------------------------------------------------------

    /// The outcome of a run that is already finished or parked, if it is.
    fn parked_outcome(&self, run_id: RunId, state: &RunState) -> Result<Option<RunOutcome>> {
        if let Some(ended) = &state.ended {
            return terminal_outcome(run_id, ended, state.stats()).map(Some);
        }
        if let Some(waiting) = &state.waiting {
            return Ok(Some(RunOutcome::Waiting {
                run_id,
                wake_at: waiting.wake_at,
                stats: state.stats(),
            }));
        }
        if let Some(pending) = &state.pending_approval {
            return Ok(Some(pending_outcome(run_id, pending, state.stats())));
        }
        Ok(None)
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
        let entry = Entry::new(seq, kind, payload)?;
        let committed = self.journal.append(run_id, entry).await?;
        if committed != seq {
            return Err(Error::journal(format!(
                "journal committed seq {committed} but the engine allocated {seq}"
            )));
        }
        state.last_seq = committed;
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
        self.end(run_id, state, RunStatus::Failed, None, Some(reason))
            .await
    }

    async fn drive(&self, run_id: RunId, state: &mut RunState) -> Result<RunOutcome> {
        // Terminates because every iteration that does not return opens at
        // least one new step, and `max_steps` bounds how many may be opened.
        loop {
            if let Some(outcome) = self.advance_calls(run_id, state).await? {
                return Ok(outcome);
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
                    // Belt and braces behind the approval gate: a gated step is
                    // dispatched only after an explicit approval is on record.
                    // Reaching here otherwise would mean the journal and the
                    // state machine disagree, and the safe reading of that is
                    // "do not run the thing a human has not agreed to".
                    if record.gated
                        && state.resolved_approvals.get(&record.step_seq)
                            != Some(&Decision::Approve)
                    {
                        return Err(Error::CorruptJournal {
                            run: run_id,
                            message: format!(
                                "step {} needs approval but none is recorded",
                                record.step_seq
                            ),
                        });
                    }
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
                    let mut dispatched = call.clone();
                    dispatched.context = Some(CallContext {
                        run_id,
                        step_seq,
                        effect_id: eid,
                        replay: false,
                        dispatched_at: Utc::now(),
                    });

                    // EDGE: the model named a tool that does not exist. It is
                    // not dispatched, but it does open a step: an agent that
                    // hallucinates names must not get unlimited free retries.
                    let gated = self
                        .tools
                        .get(&call.name)
                        .is_some_and(|t| t.requires_approval(&dispatched));

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
                    };

                    if gated {
                        let requested_at = Utc::now();
                        let expires_at = self
                            .config
                            .approval_ttl
                            .map(|ttl| deadline(requested_at, ttl));
                        let payload = ApprovalRequested {
                            step_seq,
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            args: call.arguments.clone(),
                            args_hash: hash,
                            effect_id: eid,
                            requested_at,
                            expires_at,
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
    /// Returns `Some` only when the tool parked the run on a timer.
    async fn execute(
        &self,
        run_id: RunId,
        state: &mut RunState,
        call: &ToolCall,
        record: StepRecord,
    ) -> Result<Option<RunOutcome>> {
        let replay = record.started;
        let attempt = record.attempt.saturating_add(1);

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
        };
        self.append(run_id, state, EntryKind::StepStarted, &started)
            .await?;
        if !record.started {
            state.steps_executed += 1;
        }

        let dispatched = ToolCall {
            id: call.id.clone(),
            name: record.tool.clone(),
            arguments: record.args.clone(),
            context: Some(CallContext {
                run_id,
                step_seq: record.step_seq,
                effect_id: record.effect_id,
                replay,
                dispatched_at: Utc::now(),
            }),
        };

        // Step 2: the effect.
        let (raw, is_error) = match self.tools.get(&record.tool) {
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

        // Step 3: close the step.
        let done = StepDone {
            step_seq: record.step_seq,
            call_id: call.id.clone(),
            tool: record.tool.clone(),
            result,
            is_error,
            truncated,
        };
        self.append(run_id, state, EntryKind::StepDone, &done)
            .await?;

        state.messages.push(tool_message(&done));
        if let Some(entry) = state.steps.get_mut(&call.id) {
            entry.started = true;
            entry.done = true;
            entry.attempt = attempt;
        }

        if let Some(until) = parked_until {
            let waiting = RunWaiting {
                step_seq: record.step_seq,
                wake_at: until,
            };
            self.append(run_id, state, EntryKind::RunWaiting, &waiting)
                .await?;
            state.waiting = Some(waiting);
            return Ok(Some(RunOutcome::Waiting {
                run_id,
                wake_at: until,
                stats: state.stats(),
            }));
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

        // EDGE: token budget exceeded by the turn just received. Spending
        // exactly the budget is allowed; a single token more is not.
        let spent = state.usage.total();
        if spent > self.config.token_budget {
            let reason = FailureReason::TokenBudgetExceeded {
                limit: self.config.token_budget,
                spent,
            };
            return self.breach(run_id, state, reason).await.map(Some);
        }

        if state.calls.is_empty() {
            // EDGE: empty or whitespace-only model output is a finished run
            // with nothing to say, not a failure. The raw text stays in the
            // journal; only the outcome is trimmed.
            // EDGE: `stop_reason: tool_use` with no calls is treated as the end
            // of the run, which is also what stops a model from looping on
            // empty turns forever.
            let output = response
                .text
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            return self
                .end(run_id, state, RunStatus::Done, output, None)
                .await
                .map(Some);
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
            dispatched_at: pending.requested_at,
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
    seen_call_ids: HashSet<String>,
    pending_approval: Option<ApprovalRequested>,
    resolved_approvals: HashMap<Seq, Decision>,
    waiting: Option<RunWaiting>,
    ended: Option<RunEnded>,
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
            seen_call_ids: HashSet::new(),
            pending_approval: None,
            resolved_approvals: HashMap::new(),
            waiting: None,
            ended: None,
        }
    }

    fn stats(&self) -> RunStats {
        RunStats {
            steps: self.steps_executed,
            usage: self.usage,
            last_seq: self.last_seq,
        }
    }

    /// Rebuild a run from its journal.
    ///
    /// The conversation is reconstructed from the recorded model turns and tool
    /// results, so resuming never pays a provider for a turn it already bought.
    fn replay(run_id: RunId, entries: &[Entry]) -> Result<Self> {
        let Some(first) = entries.first() else {
            return Err(Error::CorruptJournal {
                run: run_id,
                message: "journal is empty".to_string(),
            });
        };
        if first.kind != EntryKind::RunStarted {
            return Err(Error::CorruptJournal {
                run: run_id,
                message: format!("first entry is '{}', expected 'run_started'", first.kind),
            });
        }
        let started: RunStarted = first.payload_as(run_id)?;
        let mut state = Self::fresh(&started.input);

        for entry in entries {
            // Sequences must be strictly increasing; anything else means the
            // journal is not the ordered log the engine depends on.
            if entry.seq <= state.last_seq {
                return Err(Error::CorruptJournal {
                    run: run_id,
                    message: format!("entry seq {} does not follow {}", entry.seq, state.last_seq),
                });
            }
            state.last_seq = entry.seq;

            match entry.kind {
                EntryKind::RunStarted => {}
                EntryKind::ModelResponse => {
                    let payload: ModelResponse = entry.payload_as(run_id)?;
                    state.turn = payload.turn;
                    state.usage.add(payload.usage);
                    for call in &payload.tool_calls {
                        state.seen_call_ids.insert(call.id.clone());
                    }
                    state.messages.push(Message::Assistant {
                        text: payload.text.clone(),
                        tool_calls: payload.tool_calls.clone(),
                    });
                    state.calls = payload.tool_calls;
                }
                EntryKind::ApprovalRequested => {
                    let payload: ApprovalRequested = entry.payload_as(run_id)?;
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
                        },
                    );
                    state.pending_approval = Some(payload);
                }
                EntryKind::ApprovalResolved => {
                    let payload: ApprovalResolved = entry.payload_as(run_id)?;
                    state
                        .resolved_approvals
                        .insert(payload.step_seq, payload.decision);
                    if state
                        .pending_approval
                        .as_ref()
                        .is_some_and(|p| p.step_seq == payload.step_seq)
                    {
                        state.pending_approval = None;
                    }
                }
                EntryKind::StepStarted => {
                    let payload: StepStarted = entry.payload_as(run_id)?;
                    match state.steps.get_mut(&payload.call_id) {
                        // Either a gated step that has now been approved, or —
                        // when `started` is already set — a retry after a crash.
                        // A retry is not a new step and must not consume budget
                        // a second time.
                        Some(existing) => {
                            let first_attempt = !existing.started;
                            existing.started = true;
                            existing.attempt = existing.attempt.max(payload.attempt);
                            if first_attempt {
                                state.steps_executed += 1;
                            }
                        }
                        None => {
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
                                },
                            );
                            state.steps_executed += 1;
                        }
                    }
                }
                EntryKind::StepDone => {
                    let payload: StepDone = entry.payload_as(run_id)?;
                    if let Some(existing) = state.steps.get_mut(&payload.call_id) {
                        existing.started = true;
                        existing.done = true;
                    }
                    state.messages.push(tool_message(&payload));
                }
                EntryKind::RunWaiting => {
                    state.waiting = Some(entry.payload_as(run_id)?);
                }
                EntryKind::RunWoken => {
                    state.waiting = None;
                }
                EntryKind::CapBreached => {}
                EntryKind::RunEnded => {
                    state.ended = Some(entry.payload_as(run_id)?);
                }
                EntryKind::Other(_) => {
                    return Err(Error::CorruptJournal {
                        run: run_id,
                        message: format!(
                            "entry {} has kind '{}', which this version does not understand",
                            entry.seq, entry.kind
                        ),
                    });
                }
            }
        }

        Ok(state)
    }

    fn open_step(&mut self, call_id: String, record: StepRecord) {
        self.seen_call_ids.insert(call_id.clone());
        if self.steps.insert(call_id, record).is_none() {
            self.steps_opened += 1;
        }
    }
}
