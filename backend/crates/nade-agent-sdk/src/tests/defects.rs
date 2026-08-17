//! The protocol defects an adversarial review found, one reproduction each.
//!
//! Every test in this file failed before the fix it names. They are kept
//! together because they are all the same shape: a decision or a fact commits,
//! the process dies, and the engine must not invent a different future.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;

use super::{assert_layout, config, engine, pending_step};
use crate::ids::RunId;
use crate::message::{CallContext, ChatResponse};
use crate::run::{Resolution, RunOutcome, RunStatus};
use crate::testing::{CountingTool, WaitTool};

/// C1: a stale approval must not approve a different step.
///
/// Approval A is issued and approved, its tool runs, the run reaches approval
/// B, and then a delayed duplicate delivery of A arrives. Before the fix the
/// engine resolved whatever `pending_approval` happened to be — so it executed
/// B, a step no human ever saw.
#[tokio::test]
async fn stale_approval_does_not_approve_a_later_step() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, _journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({"n": 1})),
            ChatResponse::tool_call("c2", "draft", json!({"n": 2})),
            ChatResponse::text("both sent"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let first = engine.run(run, "draft two things").await.expect("pauses");
    let step_a = first.approval().expect("paused on A").step_seq;

    let second = engine
        .resume(run, Resolution::approve(step_a))
        .await
        .expect("A is approved");
    let step_b = second.approval().expect("paused on B").step_seq;
    assert_ne!(step_a, step_b, "the two approvals are different steps");
    assert_eq!(tool.executions(), 1, "only A has run");

    // The duplicate delivery of A. It must not be read as an approval of B.
    let replayed = engine.resume(run, Resolution::approve(step_a)).await;

    assert!(
        matches!(
            replayed,
            Err(crate::Error::AlreadyResolved { step_seq, .. }) if step_seq == step_a
        ),
        "a duplicate of A must report that A is already resolved, got {replayed:?}"
    );
    assert_eq!(tool.executions(), 1, "B must not have executed");

    // And the run is still exactly where it was: waiting on B.
    let still = engine.run(run, "unused").await.expect("noop");
    assert_eq!(still.status(), RunStatus::PendingApproval);
    assert_eq!(still.approval().expect("paused").step_seq, step_b);
}

/// C1, the other half: a resolution naming a step that is not the pending one
/// and was never resolved is rejected rather than applied to whatever is open.
#[tokio::test]
async fn resolution_for_an_unrelated_step_is_rejected() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("sent"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let pending = paused.approval().expect("paused").step_seq;
    let entries = journal.entries(run).len();

    for bogus in [pending + 1, pending + 7, 1] {
        let result = engine.resume(run, Resolution::approve(bogus)).await;
        assert!(
            matches!(
                result,
                Err(crate::Error::StepMismatch { expected: Some(e), got, .. })
                    if e == pending && got == bogus
            ),
            "approving step {bogus} must be refused, got {result:?}"
        );
    }

    assert_eq!(tool.executions(), 0);
    assert_eq!(journal.entries(run).len(), entries, "nothing was appended");
}

/// C3: a crash after the final model response must finish the run from what is
/// recorded, not buy a fresh — and possibly different — turn.
///
/// Before the fix the engine re-asked the model, and the third scripted
/// response here (a tool call the first answer never contained) was executed.
#[tokio::test]
async fn crash_after_final_model_response_finishes_from_the_journal() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({"body": "once"})).with_usage(10, 5),
            ChatResponse::text("saved").with_usage(20, 4),
            // The nondeterministic re-ask: a turn the first answer never had.
            ChatResponse::tool_call("c2", "note", json!({"body": "twice"})),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    journal.fail_append_at(6); // `run_ended` never lands
    engine
        .run(run, "take a note")
        .await
        .expect_err("the injected failure aborts the run");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
            (4, "step_done"),
            (5, "model_response"),
        ],
    );

    let outcome = engine.run(run, "take a note").await.expect("resume");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert!(
        matches!(&outcome, RunOutcome::Done { output: Some(o), .. } if o == "saved"),
        "the recorded answer is the run's answer, got {outcome:?}"
    );
    assert_eq!(llm.turns(), 2, "the model was not asked a third time");
    assert_eq!(llm.remaining(), 1, "the extra scripted turn went unused");
    assert_eq!(tool.executions(), 1, "no second effect");
}

/// H5: a crash between `step_done` and `run_waiting` must not lose the wait.
///
/// Before the fix replay found no `wake_at` anywhere, marked the call done and
/// walked straight on to the model — the delay vanished silently.
#[tokio::test]
async fn crash_between_step_done_and_run_waiting_keeps_the_wait() {
    let wake_at = Utc::now() + ChronoDuration::minutes(5);
    let tool = Arc::new(WaitTool::new(wake_at));
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "sleep", json!({})),
            ChatResponse::text("awake"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    journal.fail_append_at(5); // `run_waiting` never lands
    engine
        .run(run, "wait a while")
        .await
        .expect_err("the injected failure aborts the run");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
            (4, "step_done"),
        ],
    );

    let outcome = engine.run(run, "wait a while").await.expect("resume");

    assert!(
        matches!(&outcome, RunOutcome::Waiting { wake_at: at, .. } if *at == wake_at),
        "the wait must survive the crash, got {outcome:?}"
    );
    assert_eq!(llm.turns(), 1, "the model must not be asked while parked");
    assert_eq!(tool.executions(), 1, "the parking tool did not run again");

    // And the run still finishes normally once the timer fires.
    let step_seq = match &outcome {
        RunOutcome::Waiting { step_seq, .. } => *step_seq,
        other => panic!("expected a wait, got {other:?}"),
    };
    let done = engine
        .resume(run, Resolution::timer(step_seq))
        .await
        .expect("timer fires");
    assert_eq!(done.status(), RunStatus::Done);
}

/// H6: a committed Skip with no `run_ended` must still reach a terminal state.
///
/// Before the fix `resume` answered `NoPendingApproval`, `run` walked into the
/// gated step and returned `CorruptJournal`, and no public API could finish the
/// run at all — the human's decision was durable and unusable.
#[tokio::test]
async fn committed_skip_without_run_ended_still_completes() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("unreachable"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let step = paused.approval().expect("paused").step_seq;

    journal.fail_append_at(5); // `run_ended` never lands
    engine
        .resume(run, Resolution::skip(step))
        .await
        .expect_err("the injected failure aborts the resume");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "approval_requested"),
            (4, "approval_resolved"),
        ],
    );

    let outcome = engine.run(run, "draft").await.expect("the run completes");

    assert_eq!(outcome.status(), RunStatus::Skipped);
    assert_eq!(tool.executions(), 0, "a skip never executes anything");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "approval_requested"),
            (4, "approval_resolved"),
            (5, "run_ended"),
        ],
    );
}

/// H6, expire: the same hole, the other decision.
#[tokio::test]
async fn committed_expire_without_run_ended_still_completes() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("unreachable"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let step = paused.approval().expect("paused").step_seq;

    journal.fail_append_at(5);
    engine
        .resume(run, Resolution::expire(step))
        .await
        .expect_err("the injected failure aborts the resume");

    let outcome = engine.run(run, "draft").await.expect("the run completes");

    assert_eq!(outcome.status(), RunStatus::Expired);
    assert_eq!(tool.executions(), 0);
}

/// H7: a step opened under one implementation must not execute under another.
///
/// The step's `effect_id` was minted for a particular action. Replay preserves
/// only the tool *name*, so a redeployment could otherwise run something else
/// entirely under that identity.
#[tokio::test]
async fn a_changed_tool_cannot_execute_an_open_step() {
    let v1 = Arc::new(CountingTool::new("note").with_version("v1"));
    let journal = crate::testing::MemoryJournal::new();
    let (engine, _llm) = super::engine_over(
        vec![
            ChatResponse::tool_call("c1", "note", json!({})),
            ChatResponse::text("saved"),
        ],
        vec![v1.clone()],
        config(),
        journal.clone(),
    );
    let run = RunId::new();

    journal.fail_append_at(4); // `step_done` never lands: the step stays open
    engine.run(run, "note").await.expect_err("crashes");
    assert_eq!(v1.executions(), 1);

    // The process restarts on a build where `note` does something else.
    let v2 = Arc::new(CountingTool::new("note").with_version("v2"));
    let (redeployed, _llm) = super::engine_over(
        vec![ChatResponse::text("saved")],
        vec![v2.clone()],
        config(),
        journal.clone(),
    );

    let result = redeployed.run(run, "note").await;

    assert!(
        matches!(&result, Err(crate::Error::ToolChanged { step_seq: 3, tool, .. }) if tool == "note"),
        "a changed implementation must not inherit the step, got {result:?}"
    );
    assert_eq!(v2.executions(), 0, "the new implementation never ran");
    // And the refusal is durable: it does not quietly resolve itself on retry.
    assert!(redeployed.run(run, "note").await.is_err());
}

/// H7, the part that matters most: a build that now gates a call must not
/// execute it just because an older build opened the step ungated.
#[tokio::test]
async fn a_tool_that_now_requires_approval_is_not_executed_ungated() {
    let ungated = Arc::new(CountingTool::new("draft"));
    let journal = crate::testing::MemoryJournal::new();
    let (engine, _llm) = super::engine_over(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("sent"),
        ],
        vec![ungated.clone()],
        config(),
        journal.clone(),
    );
    let run = RunId::new();

    journal.fail_append_at(4);
    engine.run(run, "draft").await.expect_err("crashes");

    // Same name, same schema, same version — only the policy changed, so the
    // fingerprint still matches and the gate check is what has to catch this.
    let gated = Arc::new(CountingTool::new("draft").with_approval(true));
    let (redeployed, _llm) = super::engine_over(
        vec![ChatResponse::text("sent")],
        vec![gated.clone()],
        config(),
        journal.clone(),
    );

    let result = redeployed.run(run, "draft").await;

    assert!(
        matches!(
            &result,
            Err(crate::Error::ToolChanged { detail, .. }) if detail.contains("requires approval")
        ),
        "an ungated step must not run under a build that gates it, got {result:?}"
    );
    assert_eq!(gated.executions(), 0);
}

/// C2: approval is not idempotency, and the engine now says so in code.
///
/// A tool that declares itself unsafe to repeat is not blind-retried after a
/// crash. The run stops with the `effect_id` quoted, so the host can reconcile
/// against its own outbox — the thing that would have made this unnecessary.
#[tokio::test]
async fn a_non_idempotent_tool_is_not_blind_retried() {
    let tool = Arc::new(
        CountingTool::mutating("send_email").with_replay_policy(crate::ReplayPolicy::Halt),
    );
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "send_email", json!({"to": "a@b.c"})),
            ChatResponse::text("sent"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "send it").await.expect("pauses");
    let step = paused.approval().expect("paused").step_seq;

    // Approved, executed — and then killed before the result committed. This is
    // exactly the interleaving the old docs claimed approval protected against.
    journal.fail_append_at(6);
    engine
        .resume(run, Resolution::approve(step))
        .await
        .expect_err("crashes after the send");
    assert_eq!(tool.executions(), 1, "the email left once");

    let outcome = engine.run(run, "send it").await.expect("the run resolves");

    assert!(
        matches!(
            &outcome,
            RunOutcome::Failed {
                reason: crate::FailureReason::AmbiguousEffect { step_seq, effect_id, tool: t },
                ..
            } if *step_seq == step
                && *effect_id == crate::effect_id(run, step)
                && t == "send_email"
        ),
        "the ambiguity must reach the host, got {outcome:?}"
    );
    assert_eq!(tool.executions(), 1, "and it did not leave twice");
    assert!(outcome.is_terminal(), "the run does not hang");
}

/// M10: two attempts at one step are handed identical input.
///
/// Anything else and "same `effect_id`" would not imply "same upserted value" —
/// the tool could not write the same row twice even if it wanted to.
#[tokio::test]
async fn every_attempt_at_a_step_sees_identical_input() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({"body": "once"})),
            ChatResponse::text("saved"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    journal.fail_append_at(4); // killed between the effect and `step_done`
    engine.run(run, "note").await.expect_err("crashes");
    engine.run(run, "note").await.expect("resumes");

    let calls = tool.calls();
    assert_eq!(calls.len(), 2, "the step really was executed twice");
    let (first, second) = (&calls[0], &calls[1]);
    assert_eq!(first.id, second.id);
    assert_eq!(first.name, second.name);
    assert_eq!(first.arguments, second.arguments);

    let a = first.context.expect("dispatched by the engine");
    let b = second.context.expect("dispatched by the engine");
    assert_eq!(a.run_id, b.run_id);
    assert_eq!(a.step_seq, b.step_seq);
    assert_eq!(a.effect_id, b.effect_id);
    assert_eq!(
        a.opened_at, b.opened_at,
        "the step's timestamp is fixed when it opens, not regenerated per attempt"
    );
    // Exactly one field may move, and it is the advisory one.
    assert!(!a.replay && b.replay);
    assert_eq!(
        CallContext { replay: true, ..a },
        b,
        "`replay` is the only difference between two attempts at one step"
    );

    // Which is what lets a well-behaved tool answer identically both times.
    let results: Vec<_> = calls
        .iter()
        .map(|c| json!({"effect_id": c.effect_id(), "args": c.arguments}))
        .collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(tool.effects().len(), 1, "one row, two upserts");
}

/// M9: the expiry comparison is floored by the journal, so a lagging clock
/// cannot make an approval that has objectively aged out approvable again.
///
/// The clock cannot be moved inside a test, so the equivalent is asserted from
/// the other side: a journal whose newest entry is already past `expires_at`
/// expires the approval even though the local `Utc::now()` says otherwise.
#[tokio::test]
async fn expiry_is_floored_by_the_newest_journal_entry() {
    use crate::journal::{ApprovalRequested, Entry, EntryKind, ModelResponse, RunStarted};
    use crate::message::StopReason;
    use crate::run::RunInput;

    // Two runs, identical but for when the newest journal entry claims to be.
    async fn attempt(entry_written_at: chrono::DateTime<Utc>) -> RunStatus {
        let tool = Arc::new(CountingTool::mutating("draft"));
        let fingerprint = Some(crate::tool_fingerprint(&*tool));
        let journal = crate::testing::MemoryJournal::new();
        let (engine, _llm) = super::engine_over(
            vec![ChatResponse::text("unused")],
            vec![tool as Arc<dyn crate::Tool>],
            crate::EngineConfig {
                clock_skew_leeway: std::time::Duration::ZERO,
                ..config()
            },
            journal.clone(),
        );
        let run = RunId::new();
        let now = Utc::now();
        let call = crate::message::ToolCall::new("c1", "draft", json!({}));

        journal.force_append(
            run,
            Entry::at(
                1,
                EntryKind::RunStarted,
                &RunStarted {
                    input: RunInput::user("draft"),
                },
                now,
            )
            .expect("builds"),
        );
        journal.force_append(
            run,
            Entry::at(
                2,
                EntryKind::ModelResponse,
                &ModelResponse {
                    turn: 1,
                    text: None,
                    tool_calls: vec![call.clone()],
                    stop_reason: StopReason::ToolUse,
                    usage: Default::default(),
                },
                now,
            )
            .expect("builds"),
        );
        journal.force_append(
            run,
            Entry::at(
                3,
                EntryKind::ApprovalRequested,
                &ApprovalRequested {
                    step_seq: 3,
                    call_id: "c1".to_string(),
                    tool: "draft".to_string(),
                    args: json!({}),
                    args_hash: crate::args_hash(&json!({})),
                    effect_id: crate::effect_id(run, 3),
                    requested_at: now,
                    // Still an hour away by the local clock.
                    expires_at: Some(now + ChronoDuration::hours(1)),
                    tool_fingerprint: fingerprint,
                },
                entry_written_at,
            )
            .expect("builds"),
        );

        engine
            .resume(run, Resolution::approve(3))
            .await
            .expect("resume succeeds")
            .status()
    }

    let now = Utc::now();
    assert_eq!(
        attempt(now).await,
        RunStatus::Done,
        "an approval inside its TTL is approvable"
    );
    assert_eq!(
        attempt(now + ChronoDuration::hours(2)).await,
        RunStatus::Expired,
        "but not once the run's own journal has moved past the deadline"
    );
}

/// The same shape as H6, found by looking for it: a cap breach that committed
/// without its `run_ended` must also reach a terminal state — and must not
/// journal the breach a second time on the way.
#[tokio::test]
async fn committed_cap_breach_without_run_ended_still_completes() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({})).with_usage(40, 10),
            ChatResponse::text("done").with_usage(40, 11),
        ],
        vec![tool.clone()],
        crate::EngineConfig {
            token_budget: 100,
            ..config()
        },
    );
    let run = RunId::new();

    // 1 run_started, 2 model_response, 3 step_started, 4 step_done,
    // 5 model_response, 6 cap_breached, 7 run_ended.
    journal.fail_append_at(7);
    engine.run(run, "overspend").await.expect_err("crashes");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
            (4, "step_done"),
            (5, "model_response"),
            (6, "cap_breached"),
        ],
    );

    let outcome = engine.run(run, "overspend").await.expect("completes");

    assert!(
        matches!(
            &outcome,
            RunOutcome::Failed {
                reason: crate::FailureReason::TokenBudgetExceeded { spent: 101, .. },
                ..
            }
        ),
        "the recorded breach is the run's outcome, got {outcome:?}"
    );
    assert_eq!(
        super::entries_of(&journal, run, crate::EntryKind::CapBreached).len(),
        1,
        "the breach is not journaled twice"
    );
}

/// The escape hatch `Error::AlreadyResolved` relies on: once a decision is
/// durable, `run` alone carries the run to its end. A worker that crashes after
/// recording an approval is never stranded, however it retries.
#[tokio::test]
async fn run_alone_completes_a_run_whose_approval_is_already_recorded() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "draft", json!({})),
            ChatResponse::text("sent"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("pauses");
    let step = pending_step(&paused);

    // The approval commits; the process dies before the tool is dispatched.
    journal.fail_append_at(5);
    engine
        .resume(run, Resolution::approve(step))
        .await
        .expect_err("crashes after recording the decision");
    assert_eq!(tool.executions(), 0);

    // Re-delivering the decision reports that it already landed...
    let again = engine.resume(run, Resolution::approve(step)).await;
    assert!(
        matches!(
            again,
            Err(crate::Error::AlreadyResolved {
                decision: crate::Decision::Approve,
                ..
            })
        ),
        "got {again:?}"
    );
    assert_eq!(tool.executions(), 0, "and still executes nothing");

    // ...and `run` is what carries it forward.
    let outcome = engine.run(run, "draft").await.expect("completes");
    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1);
    assert_eq!(tool.effects().len(), 1);
}

/// A stale timer must not cut short a later, unrelated wait — the same defect
/// as C1, on the other resolution.
#[tokio::test]
async fn a_stale_timer_does_not_wake_a_later_wait() {
    let first_wake = Utc::now() + ChronoDuration::minutes(5);
    let second_wake = Utc::now() + ChronoDuration::minutes(30);
    let early = Arc::new(WaitTool::new(first_wake));
    let late = Arc::new(crate::testing::WaitTool::named("nap", second_wake));
    let (engine, _llm, _journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "sleep", json!({})),
            ChatResponse::tool_call("c2", "nap", json!({})),
            ChatResponse::text("rested"),
        ],
        vec![early.clone(), late.clone()],
        config(),
    );
    let run = RunId::new();

    let parked = engine.run(run, "sleep twice").await.expect("parks");
    let RunOutcome::Waiting { step_seq: a, .. } = parked else {
        panic!("expected a wait, got {parked:?}")
    };
    let woken = engine
        .resume(run, Resolution::timer(a))
        .await
        .expect("the first timer fires");
    let RunOutcome::Waiting {
        step_seq: b,
        wake_at,
        ..
    } = woken
    else {
        panic!("expected a second wait, got {woken:?}")
    };
    assert_ne!(a, b);
    assert_eq!(wake_at, second_wake);

    // The duplicate delivery of the first timer.
    let replayed = engine.resume(run, Resolution::timer(a)).await;

    assert!(
        matches!(
            replayed,
            Err(crate::Error::StepMismatch { expected: Some(e), got, .. }) if e == b && got == a
        ),
        "a stale wake-up must be refused, got {replayed:?}"
    );
    let still = engine.run(run, "unused").await.expect("noop");
    assert_eq!(
        still.status(),
        RunStatus::Waiting,
        "the run is still parked on its second wait"
    );
}
