//! The protocol defects an adversarial review found, one reproduction each.
//!
//! Every test in this file failed before the fix it names. They are kept
//! together because they are all the same shape: a decision or a fact commits,
//! the process dies, and the engine must not invent a different future.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;

use super::{assert_layout, config, engine};
use crate::ids::RunId;
use crate::message::ChatResponse;
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
