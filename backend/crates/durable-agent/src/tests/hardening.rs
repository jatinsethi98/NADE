//! The second adversarial review, one reproduction each.
//!
//! Where `integrity.rs` asks whether replay *notices* a damaged journal, this
//! file asks the sharper question the review put: for every field replay reads,
//! is it **validated against something the engine wrote earlier**, or merely
//! **recomputed from the payload that claims it**? A field of the second kind
//! is not checked at all — a consistent forgery satisfies it — and that is how
//! a journal came to be able to swap the tool and the arguments of a step while
//! keeping a real model call's id.
//!
//! The other half of the file is about *durability of decisions*: a guard rail
//! the engine reaches by asking a tool at replay time is only as durable as the
//! answer, and the answer was in nobody's journal.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{json, Value};

use super::{config, engine, engine_over, pending_step};
use crate::ids::{args_hash, effect_id, RunId, Seq};
use crate::journal::{
    ApprovalRequested, Entry, EntryKind, ModelResponse, RunStarted, StepDone, StepStarted,
};
use crate::message::{ChatResponse, StopReason, ToolCall};
use crate::run::{FailureReason, Resolution, RunInput, RunOutcome, RunStatus};
use crate::testing::{CountingTool, MemoryJournal};
use crate::tool::{tool_fingerprint, ReplayPolicy, Tool};
use crate::{Error, Result};

// ---- a hand-built journal ---------------------------------------------------

/// An entry stamped now.
fn entry<P: serde::Serialize>(seq: Seq, kind: EntryKind, payload: &P) -> Entry {
    entry_at(seq, kind, payload, Utc::now())
}

/// An entry stamped whenever the caller likes.
fn entry_at<P: serde::Serialize>(
    seq: Seq,
    kind: EntryKind,
    payload: &P,
    created_at: DateTime<Utc>,
) -> Entry {
    Entry::at(seq, kind, payload, created_at).expect("entry builds")
}

/// An entry whose payload is written as raw JSON, so a test can produce bytes
/// this version would never emit.
fn raw(seq: Seq, kind: &str, payload: Value) -> Entry {
    Entry {
        seq,
        kind: EntryKind::from_tag(kind),
        payload,
        created_at: Utc::now(),
    }
}

/// The opening of a journal whose one model turn asked for `call`.
fn model_asked_for(call: &ToolCall) -> Vec<Entry> {
    vec![
        entry(
            1,
            EntryKind::RunStarted,
            &RunStarted::new(RunInput::user("take a note")),
        ),
        entry(
            2,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 1,
                text: None,
                tool_calls: vec![call.clone()],
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
        ),
    ]
}

/// A `step_started` at seq 3 that is internally consistent in every respect —
/// derived `effect_id`, recomputed `args_hash`, the live tool's fingerprint —
/// and says whatever the caller wants about *which* action the step is.
fn step_opening(run: RunId, call_id: &str, tool: &dyn Tool, args: Value) -> Entry {
    entry(
        3,
        EntryKind::StepStarted,
        &StepStarted {
            step_seq: 3,
            call_id: call_id.to_string(),
            tool: tool.name().to_string(),
            args: args.clone(),
            args_hash: args_hash(&args),
            effect_id: effect_id(run, 3),
            attempt: 1,
            opened_at: Utc::now(),
            tool_fingerprint: Some(tool_fingerprint(tool)),
            replay_policy: ReplayPolicy::Retry,
        },
    )
}

/// Drive `entries` as `run`'s journal, under `tools`.
async fn drive(run: RunId, entries: Vec<Entry>, tools: Vec<Arc<dyn Tool>>) -> Result<RunOutcome> {
    let journal = MemoryJournal::new();
    for e in entries {
        journal.force_append(run, e);
    }
    let (engine, _llm) = engine_over(
        vec![ChatResponse::text("an unscripted turn would panic")],
        tools,
        config(),
        journal,
    );
    engine.run(run, "take a note").await
}

// ---- N1: a step's action is bound to the model call that asked for it -------

/// N1: the critical hole. A journal that reuses a **real** call id while naming
/// a different tool passes every check the engine had — `effect_id` and
/// `args_hash` are recomputed from the forged payload, and the fingerprint is
/// the live tool's — and replay dispatches the substituted action.
#[tokio::test]
async fn a_forged_step_cannot_substitute_the_tool_the_model_asked_for() {
    let note = Arc::new(CountingTool::new("note"));
    let send = Arc::new(CountingTool::new("send_email"));
    let run = RunId::new();

    let mut entries = model_asked_for(&ToolCall::new("c1", "note", json!({"body": "hello"})));
    // Same call id. Different tool, different arguments, everything derived
    // recomputed so the forgery is self-consistent. No `step_done`, so the step
    // is open and replay must re-dispatch it.
    entries.push(step_opening(
        run,
        "c1",
        &*send,
        json!({"to": "victim@example.com"}),
    ));

    let result = drive(
        run,
        entries,
        vec![note.clone() as Arc<dyn Tool>, send.clone() as Arc<dyn Tool>],
    )
    .await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "a step whose tool is not the one the model asked for must be refused, got {result:?}"
    );
    assert_eq!(
        send.executions(),
        0,
        "the substituted action must never be dispatched"
    );
    assert_eq!(note.executions(), 0);
}

/// N1, the quieter half: keep the tool, swap only the arguments. Nothing about
/// the entry is inconsistent — `args_hash` is recomputed from the substituted
/// arguments — so only a comparison against the model's own call can catch it.
#[tokio::test]
async fn a_forged_step_cannot_substitute_the_arguments_the_model_asked_for() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();

    let mut entries = model_asked_for(&ToolCall::new("c1", "note", json!({"body": "hello"})));
    entries.push(step_opening(
        run,
        "c1",
        &*note,
        json!({"body": "something the model never wrote"}),
    ));

    let result = drive(run, entries, vec![note.clone() as Arc<dyn Tool>]).await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "arguments must match the call that requested them, got {result:?}"
    );
    assert_eq!(note.executions(), 0);
}

/// N1 on the approval path: the forgery decides what a *human* is shown.
#[tokio::test]
async fn a_forged_approval_cannot_substitute_the_tool_the_model_asked_for() {
    let draft = Arc::new(CountingTool::mutating("draft"));
    let wire = Arc::new(CountingTool::mutating("wire_transfer"));
    let run = RunId::new();
    let args = json!({"amount": 10_000});

    let mut entries = model_asked_for(&ToolCall::new("c1", "draft", json!({"body": "hi"})));
    entries.push(entry(
        3,
        EntryKind::ApprovalRequested,
        &ApprovalRequested {
            step_seq: 3,
            call_id: "c1".to_string(),
            tool: "wire_transfer".to_string(),
            args: args.clone(),
            args_hash: args_hash(&args),
            effect_id: effect_id(run, 3),
            requested_at: Utc::now(),
            expires_at: None,
            tool_fingerprint: Some(tool_fingerprint(&*wire)),
            replay_policy: ReplayPolicy::Retry,
        },
    ));

    let result = drive(
        run,
        entries,
        vec![
            draft.clone() as Arc<dyn Tool>,
            wire.clone() as Arc<dyn Tool>,
        ],
    )
    .await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "a human must not be asked about an action no model turn requested, got {result:?}"
    );
}

/// N1's state-machine half: a model turn may not walk away from a step that has
/// started and not finished. Replay used to accept it and quietly drop the
/// step — whose effect may already exist — from the run's future.
#[tokio::test]
async fn a_model_turn_that_abandons_an_unfinished_step_is_refused() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "hello"});

    let mut entries = model_asked_for(&ToolCall::new("c1", "note", args.clone()));
    entries.push(step_opening(run, "c1", &*note, args));
    // No `step_done`. The engine would have retried the step before buying
    // another turn; this journal claims it did not.
    entries.push(entry(
        4,
        EntryKind::ModelResponse,
        &ModelResponse {
            turn: 2,
            text: Some("all done".to_string()),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    ));

    let result = drive(run, entries, vec![note.clone() as Arc<dyn Tool>]).await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "a turn that abandons an open step must be refused, got {result:?}"
    );
}

/// The same order rule at the other end: a turn with no tool calls **is** the
/// run's answer, so nothing may follow it. Otherwise a forged turn appended
/// after a finished run reopens it and dispatches whatever it likes.
#[tokio::test]
async fn a_model_turn_after_the_run_answered_is_refused() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "hello"});

    let entries = vec![
        entry(
            1,
            EntryKind::RunStarted,
            &RunStarted::new(RunInput::user("take a note")),
        ),
        entry(
            2,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 1,
                text: Some("nothing to do".to_string()),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ),
        entry(
            3,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 2,
                text: None,
                tool_calls: vec![ToolCall::new("c1", "note", args.clone())],
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
        ),
        entry(
            4,
            EntryKind::StepStarted,
            &StepStarted {
                step_seq: 4,
                call_id: "c1".to_string(),
                tool: "note".to_string(),
                args: args.clone(),
                args_hash: args_hash(&args),
                effect_id: effect_id(run, 4),
                attempt: 1,
                opened_at: Utc::now(),
                tool_fingerprint: Some(tool_fingerprint(&*note)),
                replay_policy: ReplayPolicy::Retry,
            },
        ),
    ];

    let result = drive(run, entries, vec![note.clone() as Arc<dyn Tool>]).await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "nothing may follow the turn that answered the run, got {result:?}"
    );
    assert_eq!(note.executions(), 0);
}

/// A pin, not a fix: an opening entry with no issuing model turn at all was
/// already refused, and must stay refused.
#[tokio::test]
async fn an_opening_entry_with_no_model_turn_at_all_is_refused() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let entries = vec![
        entry(
            1,
            EntryKind::RunStarted,
            &RunStarted::new(RunInput::user("take a note")),
        ),
        entry(
            2,
            EntryKind::StepStarted,
            &StepStarted {
                step_seq: 2,
                call_id: "c1".to_string(),
                tool: "note".to_string(),
                args: json!({}),
                args_hash: args_hash(&json!({})),
                effect_id: effect_id(run, 2),
                attempt: 1,
                opened_at: Utc::now(),
                tool_fingerprint: Some(tool_fingerprint(&*note)),
                replay_policy: ReplayPolicy::Retry,
            },
        ),
    ];

    let result = drive(run, entries, vec![note.clone() as Arc<dyn Tool>]).await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "got {result:?}"
    );
    assert_eq!(note.executions(), 0);
}

/// The control for this file: a journal whose step really is the call the model
/// made replays and runs, so the checks above are not simply refusing
/// everything.
#[tokio::test]
async fn a_faithful_journal_still_replays_and_runs_the_step() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "hello"});

    let mut entries = model_asked_for(&ToolCall::new("c1", "note", args.clone()));
    entries.push(step_opening(run, "c1", &*note, args.clone()));

    let journal = MemoryJournal::new();
    for e in entries {
        journal.force_append(run, e);
    }
    let (engine, _llm) = engine_over(
        vec![ChatResponse::text("saved")],
        vec![note.clone() as Arc<dyn Tool>],
        config(),
        journal,
    );

    let outcome = engine.run(run, "take a note").await.expect("replays");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(note.executions(), 1, "the interrupted step was re-executed");
    assert_eq!(note.calls()[0].arguments, args);
}

// ---- N2: the gate is re-evaluated on every dispatch -------------------------

/// A gated tool that counts how often the engine asks whether it is gated.
#[derive(Debug, Default)]
struct GateProbe {
    gate_checks: Arc<AtomicU32>,
    executions: Arc<AtomicU32>,
}

impl GateProbe {
    fn gate_checks(&self) -> u32 {
        self.gate_checks.load(Ordering::SeqCst)
    }

    fn executions(&self) -> u32 {
        self.executions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for GateProbe {
    fn name(&self) -> &str {
        "draft"
    }

    fn schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn requires_approval(&self, _call: &ToolCall) -> bool {
        self.gate_checks.fetch_add(1, Ordering::SeqCst);
        true
    }

    async fn execute(&self, _call: &ToolCall) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }
}

/// N2: `Tool::requires_approval` is documented as "consulted before a step is
/// opened **and again before every dispatch, including a re-dispatch after a
/// crash**". It was not: the check short-circuited on `!record.gated`, so a
/// step that was already gated skipped the re-evaluation entirely.
#[tokio::test]
async fn requires_approval_is_re_evaluated_before_every_dispatch() {
    let tool = Arc::new(GateProbe::default());
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("sent"),
        ],
        vec![tool.clone() as Arc<dyn Tool>],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let step = pending_step(&paused);
    assert_eq!(
        tool.gate_checks(),
        1,
        "the gate is evaluated once when the step opens"
    );

    // Approved and executed — then killed before `step_done` committed.
    journal.fail_append_at(6);
    engine
        .resume(run, Resolution::approve(step))
        .await
        .expect_err("crashes after the effect");
    assert_eq!(tool.executions(), 1);

    engine.run(run, "draft").await.expect("resumes");
    assert_eq!(tool.executions(), 2, "the open step was re-dispatched");

    assert_eq!(
        tool.gate_checks(),
        3,
        "the gate must be re-evaluated before each of the two dispatches, not \
         only when the step opened"
    );
}

// ---- N3: the halt decision is durable ---------------------------------------

/// N3: `ReplayPolicy::Halt` stops a run whose step may or may not have landed.
/// The decision was taken by asking the tool at replay time and, if the
/// `run_ended` append did not commit, was recorded nowhere — so a later
/// invocation under a policy that answers `Retry` dispatched the very effect an
/// earlier one had decided must not run.
#[tokio::test]
async fn a_recorded_halt_survives_a_tool_that_later_permits_a_retry() {
    let v1 = Arc::new(CountingTool::new("send").with_replay_policy(ReplayPolicy::Halt));
    let journal = MemoryJournal::new();
    let (engine, _llm) = engine_over(
        vec![
            ChatResponse::tool_call("c1", "send", json!({"to": "a@b.c"})),
            ChatResponse::text("sent"),
        ],
        vec![v1.clone() as Arc<dyn Tool>],
        config(),
        journal.clone(),
    );
    let run = RunId::new();

    // 1 run_started, 2 model_response, 3 step_started, 4 step_done.
    journal.fail_append_at(4);
    engine
        .run(run, "send it")
        .await
        .expect_err("crashes between the effect and its result");
    assert_eq!(v1.executions(), 1, "the send left once");

    // The halt is decided — and the entry recording it never commits.
    journal.fail_append_at(4);
    engine
        .run(run, "send it")
        .await
        .expect_err("the halt is decided but not recorded");
    assert_eq!(v1.executions(), 1, "the halt held for that invocation");

    // The process restarts on a build where the same tool now permits a retry:
    // a code change, or simply a non-deterministic implementation. The
    // interface is unchanged, so the fingerprint still matches.
    let v2 = Arc::new(CountingTool::new("send").with_replay_policy(ReplayPolicy::Retry));
    let (redeployed, _llm) = engine_over(
        vec![ChatResponse::text("sent")],
        vec![v2.clone() as Arc<dyn Tool>],
        config(),
        journal.clone(),
    );

    let outcome = redeployed
        .run(run, "send it")
        .await
        .expect("the run resolves");

    assert!(
        matches!(
            &outcome,
            RunOutcome::Failed {
                reason: FailureReason::AmbiguousEffect { step_seq: 3, .. },
                ..
            }
        ),
        "a halt on record must not be re-litigated, got {outcome:?}"
    );
    assert_eq!(
        v2.executions(),
        0,
        "the effect an earlier invocation refused must stay refused"
    );
}

/// The other direction, which must keep working: a step opened under `Retry`
/// and re-dispatched under a build that now forbids a blind retry is **not**
/// retried. The recorded policy fixes the decision; the current one may only
/// make it stricter.
#[tokio::test]
async fn a_tool_that_now_forbids_a_blind_retry_is_not_retried() {
    let v1 = Arc::new(CountingTool::new("send"));
    let journal = MemoryJournal::new();
    let (engine, _llm) = engine_over(
        vec![
            ChatResponse::tool_call("c1", "send", json!({})),
            ChatResponse::text("sent"),
        ],
        vec![v1.clone() as Arc<dyn Tool>],
        config(),
        journal.clone(),
    );
    let run = RunId::new();

    journal.fail_append_at(4);
    engine.run(run, "send it").await.expect_err("crashes");
    assert_eq!(v1.executions(), 1);

    let v2 = Arc::new(CountingTool::new("send").with_replay_policy(ReplayPolicy::Halt));
    let (redeployed, _llm) = engine_over(
        vec![ChatResponse::text("sent")],
        vec![v2.clone() as Arc<dyn Tool>],
        config(),
        journal.clone(),
    );

    let outcome = redeployed.run(run, "send it").await.expect("resolves");

    assert!(
        matches!(
            &outcome,
            RunOutcome::Failed {
                reason: FailureReason::AmbiguousEffect { .. },
                ..
            }
        ),
        "got {outcome:?}"
    );
    assert_eq!(v2.executions(), 0);
}

// ---- N4: no run is stranded -------------------------------------------------

/// N4: a fingerprint mismatch discovered *after* an approval committed left the
/// run with nothing that could move it — `run` answered `ToolChanged` forever
/// and `resume` answered `AlreadyResolved` forever. `CRITERIA.md` claimed "the
/// run is never stranded"; this is the test that makes the claim true.
#[tokio::test]
async fn a_run_whose_tool_changed_after_approval_can_still_be_ended() {
    let v1 = Arc::new(CountingTool::mutating("draft").with_version("v1"));
    let journal = MemoryJournal::new();
    let (engine, _llm) = engine_over(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("sent"),
        ],
        vec![v1.clone() as Arc<dyn Tool>],
        config(),
        journal.clone(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let step = pending_step(&paused);

    // The human approves, and the build changes underneath before the tool ran.
    let v2 = Arc::new(CountingTool::mutating("draft").with_version("v2"));
    let (redeployed, _llm) = engine_over(
        vec![ChatResponse::text("sent")],
        vec![v2.clone() as Arc<dyn Tool>],
        config(),
        journal.clone(),
    );
    let approved = redeployed.resume(run, Resolution::approve(step)).await;
    assert!(
        matches!(&approved, Err(Error::ToolChanged { .. })),
        "got {approved:?}"
    );
    assert_eq!(v2.executions(), 0);

    // Every entry point that existed before this pass is now a dead end: the
    // decision is on record, so it cannot be retaken, and the step cannot run.
    assert!(matches!(
        redeployed.run(run, "draft").await,
        Err(Error::ToolChanged { .. })
    ));
    for again in [
        Resolution::approve(step),
        Resolution::skip(step),
        Resolution::expire(step),
    ] {
        assert!(
            matches!(
                redeployed.resume(run, again).await,
                Err(Error::AlreadyResolved { .. })
            ),
            "a decision already on record cannot be retaken"
        );
    }
    assert!(matches!(
        redeployed.resume(run, Resolution::timer(step)).await,
        Err(Error::NotWaiting(_))
    ));

    // The explicit way out: the host gives up on the run, and that is a durable
    // terminal fact rather than a permanent error.
    let ended = redeployed
        .cancel(run, "the tool changed under an approved step")
        .await
        .expect("a replayable run can always be ended");

    assert!(ended.is_terminal(), "got {ended:?}");
    assert!(
        matches!(
            &ended,
            RunOutcome::Failed {
                reason: FailureReason::Cancelled { detail },
                ..
            } if detail.contains("tool changed")
        ),
        "got {ended:?}"
    );
    assert_eq!(v2.executions(), 0, "cancelling executes nothing");

    // And it is idempotent: a second cancel reports the recorded outcome.
    let again = redeployed.cancel(run, "and again").await.expect("no-op");
    assert_eq!(again, ended);
}

/// `cancel` is a general kill switch, not a repair for one defect: it ends a
/// run parked on a human without executing the thing the human never answered.
#[tokio::test]
async fn cancelling_a_paused_run_executes_nothing_and_sticks() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("unreachable"),
        ],
        vec![tool.clone() as Arc<dyn Tool>],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let step = pending_step(&paused);

    let ended = engine
        .cancel(run, "the user closed the thread")
        .await
        .expect("ends");
    assert_eq!(ended.status(), RunStatus::Failed);
    assert_eq!(tool.executions(), 0);

    // The decision is durable, and every entry point agrees with it afterwards.
    assert_eq!(engine.run(run, "draft").await.expect("noop"), ended);
    assert_eq!(
        engine
            .resume(run, Resolution::approve(step))
            .await
            .expect("noop"),
        ended,
        "a late approval of a cancelled run settles nothing"
    );
    assert_eq!(tool.executions(), 0);
    assert_eq!(
        super::entries_of(&journal, run, EntryKind::RunEnded).len(),
        1,
        "the terminal entry is written once"
    );
}

/// A run that was never started has no sequence to append at, so there is
/// nothing to cancel and saying so is better than inventing a journal.
#[tokio::test]
async fn cancelling_a_run_that_never_started_is_refused() {
    let (engine, _llm, _journal) = engine(
        vec![ChatResponse::text("unused")],
        vec![Arc::new(CountingTool::new("note")) as Arc<dyn Tool>],
        config(),
    );
    let result = engine.cancel(RunId::new(), "nothing here").await;
    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "got {result:?}"
    );
}

// ---- N5: one future timestamp must not poison a run's clock -----------------

/// N5: `Engine::now` is floored by the newest `created_at` in the run's own
/// journal, which is what stops a clock that jumps backwards from reviving an
/// approval. Nothing bounded the floor from above, so a single entry stamped in
/// the future displaced that run's clock for good.
#[tokio::test]
async fn an_entry_stamped_far_in_the_future_is_refused() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let run = RunId::new();
    let now = Utc::now();

    let journal = MemoryJournal::new();
    journal.force_append(
        run,
        entry_at(
            1,
            EntryKind::RunStarted,
            &RunStarted::new(RunInput::user("draft")),
            now,
        ),
    );
    journal.force_append(
        run,
        entry_at(
            2,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 1,
                text: None,
                tool_calls: vec![ToolCall::new("c1", "draft", json!({}))],
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
            now,
        ),
    );
    journal.force_append(
        run,
        entry_at(
            3,
            EntryKind::ApprovalRequested,
            &ApprovalRequested {
                step_seq: 3,
                call_id: "c1".to_string(),
                tool: "draft".to_string(),
                args: json!({}),
                args_hash: args_hash(&json!({})),
                effect_id: effect_id(run, 3),
                requested_at: now,
                expires_at: Some(now + ChronoDuration::hours(1)),
                tool_fingerprint: Some(tool_fingerprint(&*tool)),
                replay_policy: ReplayPolicy::Retry,
            },
            // The poison: one entry claiming to have been written a decade from
            // now. A faithful engine cannot produce this; a buggy or hostile
            // journal can, and the crate may not assume it will not.
            now + ChronoDuration::days(3650),
        ),
    );

    let (engine, _llm) = engine_over(
        vec![ChatResponse::text("unused")],
        vec![tool.clone() as Arc<dyn Tool>],
        config(),
        journal,
    );

    let result = engine.resume(run, Resolution::approve(3)).await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { message, .. }) if message.contains("future")),
        "a journal cannot push a run's clock forward without bound, got {result:?}"
    );
    assert_eq!(tool.executions(), 0);
}

/// The bound is a bound, not a ban: ordinary skew between two machines writing
/// one run's journal is still accepted.
#[tokio::test]
async fn an_entry_a_little_ahead_of_the_local_clock_is_accepted() {
    let tool = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "hello"});
    let soon = Utc::now() + ChronoDuration::seconds(5);

    let journal = MemoryJournal::new();
    journal.force_append(
        run,
        entry_at(
            1,
            EntryKind::RunStarted,
            &RunStarted::new(RunInput::user("take a note")),
            soon,
        ),
    );
    journal.force_append(
        run,
        entry_at(
            2,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 1,
                text: None,
                tool_calls: vec![ToolCall::new("c1", "note", args.clone())],
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
            soon,
        ),
    );

    let (engine, _llm) = engine_over(
        vec![ChatResponse::text("saved")],
        vec![tool.clone() as Arc<dyn Tool>],
        config(),
        journal,
    );

    let outcome = engine.run(run, "take a note").await.expect("replays");
    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1);
}

/// The same class, on a field replay never compared to anything: a step cannot
/// have been opened *after* the entry that records it was created.
#[tokio::test]
async fn a_step_opened_after_the_entry_that_records_it_is_refused() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "hello"});

    let mut entries = model_asked_for(&ToolCall::new("c1", "note", args.clone()));
    entries.push(entry(
        3,
        EntryKind::StepStarted,
        &StepStarted {
            step_seq: 3,
            call_id: "c1".to_string(),
            tool: "note".to_string(),
            args: args.clone(),
            args_hash: args_hash(&args),
            effect_id: effect_id(run, 3),
            attempt: 1,
            // The step claims to have opened long after it was journaled. It is
            // what the tool is handed as `opened_at`, and for a gated step it is
            // what the approval's TTL counts from.
            opened_at: Utc::now() + ChronoDuration::days(3650),
            tool_fingerprint: Some(tool_fingerprint(&*note)),
            replay_policy: ReplayPolicy::Retry,
        },
    ));

    let result = drive(run, entries, vec![note.clone() as Arc<dyn Tool>]).await;

    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "got {result:?}"
    );
    assert_eq!(note.executions(), 0);
}

/// And the same class again, on the approval's own deadline: an entry cannot
/// claim a TTL that ran out before the request was made.
#[tokio::test]
async fn an_approval_that_expired_before_it_was_requested_is_refused() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let run = RunId::new();
    let now = Utc::now();

    let mut entries = model_asked_for(&ToolCall::new("c1", "draft", json!({})));
    entries.push(entry(
        3,
        EntryKind::ApprovalRequested,
        &ApprovalRequested {
            step_seq: 3,
            call_id: "c1".to_string(),
            tool: "draft".to_string(),
            args: json!({}),
            args_hash: args_hash(&json!({})),
            effect_id: effect_id(run, 3),
            requested_at: now,
            expires_at: Some(now - ChronoDuration::days(1)),
            tool_fingerprint: Some(tool_fingerprint(&*tool)),
            replay_policy: ReplayPolicy::Retry,
        },
    ));

    let result = drive(run, entries, vec![tool as Arc<dyn Tool>]).await;
    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "got {result:?}"
    );
}

// ---- N6: an unreadable format is a decision, not a serde error --------------

/// N6: the previous pass added required fields with no defaults, so a journal
/// written by the version before it fails to *deserialise* — before any of the
/// recovery paths can look at it. The crate is unpublished, so no such journal
/// exists; a format that cannot be read is still a design defect, and the
/// answer is a version on the run's opening entry and a typed refusal.
#[tokio::test]
async fn a_journal_from_an_older_format_is_refused_with_a_typed_error() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "x"});

    // Byte-for-byte what the previous version wrote: no `journal_format` on
    // `run_started`, and a `step_started` with no `opened_at`.
    let entries = vec![
        raw(
            1,
            "run_started",
            json!({"input": {"messages": [{"role": "user", "text": "take a note"}]}}),
        ),
        raw(
            2,
            "model_response",
            json!({
                "turn": 1,
                "tool_calls": [{"id": "c1", "name": "note", "arguments": args}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 0, "output_tokens": 0},
            }),
        ),
        raw(
            3,
            "step_started",
            json!({
                "step_seq": 3,
                "call_id": "c1",
                "tool": "note",
                "args": args,
                "args_hash": args_hash(&args),
                "effect_id": effect_id(run, 3),
                "attempt": 1,
            }),
        ),
    ];

    let result = drive(run, entries, vec![note.clone() as Arc<dyn Tool>]).await;

    let code = match &result {
        Err(err) => err.code(),
        Ok(outcome) => panic!("expected a refusal, got {outcome:?}"),
    };
    assert_eq!(
        code, "unsupported_journal_format",
        "an unreadable format must be a deliberate, typed refusal rather than a \
         serde failure dressed up as a corrupt journal; got {result:?}"
    );
    assert_eq!(note.executions(), 0);
}

/// And the same refusal from the other side: a journal written by a *newer*
/// version is refused with the same typed error rather than misread.
#[tokio::test]
async fn a_journal_from_a_newer_format_is_refused_with_a_typed_error() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();

    let entries = vec![raw(
        1,
        "run_started",
        json!({
            "input": {"messages": [{"role": "user", "text": "take a note"}]},
            "journal_format": 9_999,
        }),
    )];

    let result = drive(run, entries, vec![note as Arc<dyn Tool>]).await;

    assert!(
        matches!(
            &result,
            Err(Error::UnsupportedJournalFormat { found: 9_999, .. })
        ),
        "got {result:?}"
    );
}

// ---- the sweep --------------------------------------------------------------

/// Found by asking N1's question of `step_done`: its `result` is what the model
/// reads, and it is recomputed from nothing — but it *is* the record of what
/// happened, so it is trusted by construction. What must not be trusted is a
/// `step_done` claiming a wait for a step that is not the one it closes, which
/// would park the run on a step no resolution can name.
#[tokio::test]
async fn a_wait_recorded_against_the_wrong_step_is_refused() {
    let note = Arc::new(CountingTool::new("note"));
    let run = RunId::new();
    let args = json!({"body": "hello"});

    let mut entries = model_asked_for(&ToolCall::new("c1", "note", args.clone()));
    entries.push(step_opening(run, "c1", &*note, args));
    entries.push(entry(
        4,
        EntryKind::StepDone,
        &StepDone {
            step_seq: 99,
            call_id: "c1".to_string(),
            tool: "note".to_string(),
            result: json!({"ok": true}),
            is_error: false,
            truncated: false,
            wake_at: None,
        },
    ));

    let result = drive(run, entries, vec![note as Arc<dyn Tool>]).await;
    assert!(
        matches!(&result, Err(Error::CorruptJournal { .. })),
        "got {result:?}"
    );
}
