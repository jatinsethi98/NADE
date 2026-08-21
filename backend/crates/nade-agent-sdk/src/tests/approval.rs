//! The approval gate: nothing gated runs without an explicit resolution.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::{assert_layout, config, engine, entries_of, only_payload, pending_step};
use crate::engine::EngineConfig;
use crate::ids::{effect_id, RunId};
use crate::journal::{ApprovalRequested, ApprovalResolved, EntryKind, RunEnded, StepStarted};
use crate::message::{ChatResponse, Message, ToolCall};
use crate::run::{Decision, Resolution, RunOutcome, RunStatus};
use crate::testing::CountingTool;
use crate::Error;

/// Two turns: the model asks for the gated tool, then wraps up after it runs.
fn script() -> Vec<ChatResponse> {
    vec![
        ChatResponse::tool_call("c1", "draft", json!({"to": "a@b.c"})).with_usage(10, 5),
        ChatResponse::text("drafted").with_usage(20, 4),
    ]
}

/// EDGE E27: the round trip the whole feature exists for.
#[tokio::test]
async fn pause_resume_round_trip_approve_completes() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine
        .run(run, "draft a reply")
        .await
        .expect("run succeeds");

    let RunOutcome::PendingApproval { request, .. } = &paused else {
        panic!("expected a pause, got {paused:?}");
    };
    assert_eq!(request.tool, "draft");
    assert_eq!(request.step_seq, 3);
    assert_eq!(request.call.arguments, json!({"to": "a@b.c"}));
    assert!(request.expires_at.is_some());
    assert_eq!(
        tool.executions(),
        0,
        "nothing ran before the human answered"
    );
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "approval_requested"),
        ],
    );

    let done = engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(done.status(), RunStatus::Done);
    assert_eq!(done.stats().steps, 1);
    assert_eq!(tool.executions(), 1);
    assert_eq!(tool.effects().len(), 1);
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "approval_requested"),
            (4, "approval_resolved"),
            (5, "step_started"),
            (6, "step_done"),
            (7, "model_response"),
            (8, "run_ended"),
        ],
    );
}

/// EDGE E38: the effect id quoted at approval time is the one the tool is
/// handed, even though the `step_started` entry lands at a later sequence. The
/// host can therefore pre-create the row the approval will fill in.
#[tokio::test]
async fn approved_step_keeps_its_quoted_effect_id() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let quoted = paused.approval().expect("paused on approval").effect_id;
    assert_eq!(
        quoted,
        effect_id(run, 3),
        "derived from the approval's own seq"
    );

    engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(tool.calls()[0].effect_id(), Some(quoted));
    let started: StepStarted = only_payload(&journal, run, EntryKind::StepStarted);
    assert_eq!(
        started.step_seq, 3,
        "the step keeps the identity it was opened with"
    );
    assert_eq!(started.effect_id, quoted);
    assert_eq!(
        tool.effects().keys().copied().collect::<Vec<_>>(),
        vec![quoted]
    );
}

/// EDGE E25.
#[tokio::test]
async fn skip_path_ends_skipped() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let outcome = engine
        .resume(run, Resolution::skip(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Skipped);
    assert!(outcome.is_terminal());
    assert_eq!(tool.executions(), 0);
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
    let resolved: ApprovalResolved = only_payload(&journal, run, EntryKind::ApprovalResolved);
    assert_eq!(resolved.decision, Decision::Skip);
    assert_eq!(resolved.reason, None);
}

/// EDGE E26.
#[tokio::test]
async fn expire_path_ends_expired() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let outcome = engine
        .resume(run, Resolution::expire(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Expired);
    assert_eq!(tool.executions(), 0);
    let resolved: ApprovalResolved = only_payload(&journal, run, EntryKind::ApprovalResolved);
    assert_eq!(resolved.decision, Decision::Expire);
    let ended: RunEnded = only_payload(&journal, run, EntryKind::RunEnded);
    assert_eq!(ended.status, RunStatus::Expired);
}

/// EDGE E24: the one that must never regress. No number of `run` calls opens
/// the gate; only a resolution does.
#[tokio::test]
async fn approval_gated_tool_never_runs_without_resolution() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    for attempt in 0..5 {
        let outcome = engine.run(run, "draft").await.expect("run succeeds");
        assert_eq!(
            outcome.status(),
            RunStatus::PendingApproval,
            "attempt {attempt} should still be paused"
        );
        assert_eq!(
            tool.executions(),
            0,
            "attempt {attempt} must not execute the tool"
        );
    }

    // And re-running never appends, never re-asks the model, never restarts.
    assert_eq!(journal.entries(run).len(), 3);
    assert_eq!(llm.turns(), 1);
    assert_eq!(entries_of(&journal, run, EntryKind::RunStarted).len(), 1);

    // The only door is a resolution, and skipping keeps it shut.
    let pending: ApprovalRequested = only_payload(&journal, run, EntryKind::ApprovalRequested);
    let skipped = engine
        .resume(run, Resolution::skip(pending.step_seq))
        .await
        .expect("resume succeeds");
    assert_eq!(skipped.status(), RunStatus::Skipped);
    assert_eq!(tool.executions(), 0);
}

/// EDGE E40: a paused, parked or finished run is returned as-is.
#[tokio::test]
async fn run_on_paused_run_is_noop() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");
    let entries = journal.entries(run).len();
    let turns = llm.turns();

    let again = engine
        .run(run, "a completely different instruction")
        .await
        .expect("noop");

    assert_eq!(again.status(), RunStatus::Done);
    assert_eq!(journal.entries(run).len(), entries, "nothing appended");
    assert_eq!(llm.turns(), turns, "the model was not asked again");
    assert_eq!(tool.executions(), 1, "the tool did not run again");
}

/// EDGE E11: the same approval delivered twice — a button pressed twice, a
/// webhook redelivered — must not produce a second effect.
#[tokio::test]
async fn double_approve_executes_tool_once() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let step = pending_step(&paused);
    let first = engine
        .resume(run, Resolution::approve(step))
        .await
        .expect("first approve");
    let entries = journal.entries(run).len();

    let second = engine
        .resume(run, Resolution::approve(step))
        .await
        .expect("second approve");

    assert_eq!(first.status(), RunStatus::Done);
    assert_eq!(second.status(), RunStatus::Done);
    assert_eq!(second.stats(), first.stats());
    assert_eq!(tool.executions(), 1);
    assert_eq!(tool.effects().len(), 1);
    assert_eq!(
        journal.entries(run).len(),
        entries,
        "the replay appended nothing"
    );
}

/// EDGE E10: resolving a finished run at all — approve, skip or expire — is a
/// read, not a second run.
#[tokio::test]
async fn duplicate_resume_of_finished_run_is_noop() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let step = pending_step(&paused);
    engine
        .resume(run, Resolution::skip(step))
        .await
        .expect("skip");
    let entries = journal.entries(run).len();

    for resolution in [
        Resolution::approve(step),
        Resolution::skip(step),
        Resolution::expire(step),
        Resolution::timer(step),
    ] {
        let outcome = engine
            .resume(run, resolution)
            .await
            .expect("resume is a read");
        assert_eq!(
            outcome.status(),
            RunStatus::Skipped,
            "{resolution:?} must not change anything"
        );
    }

    assert_eq!(journal.entries(run).len(), entries);
    assert_eq!(tool.executions(), 0);
}

/// EDGE E12: an approval that aged out is refused, and refused *before* the
/// tool is dispatched.
#[tokio::test]
async fn approve_after_expiry_expires_run() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let config = EngineConfig {
        approval_ttl: Some(Duration::ZERO),
        clock_skew_leeway: Duration::ZERO,
        ..config()
    };
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config);
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let expires_at = paused
        .approval()
        .expect("paused")
        .expires_at
        .expect("a ttl was set");
    // Stamps are whole seconds (`Entry::new`), so a zero TTL expires only once
    // the clock crosses the *next* second boundary — sleep just past it.
    let to_boundary =
        1_000_000_000u64.saturating_sub(u64::from(chrono::Utc::now().timestamp_subsec_nanos()));
    tokio::time::sleep(Duration::from_nanos(to_boundary + 50_000_000)).await;
    assert!(
        chrono::SubsecRound::trunc_subsecs(chrono::Utc::now(), 0) > expires_at,
        "the approval really is stale at second granularity"
    );

    let outcome = engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Expired);
    assert_eq!(
        tool.executions(),
        0,
        "an expired approval must not execute anything"
    );
    let resolved: ApprovalResolved = only_payload(&journal, run, EntryKind::ApprovalResolved);
    assert_eq!(resolved.decision, Decision::Expire);
    assert_eq!(resolved.reason.as_deref(), Some("ttl_expired"));
}

/// A TTL or a leeway too large for `chrono` must clamp, not panic. `chrono`'s
/// `Add` panics on overflow, so this is a real way to take a server down with a
/// config value.
#[tokio::test]
async fn absurd_ttl_and_leeway_clamp_instead_of_panicking() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let config = EngineConfig {
        approval_ttl: Some(Duration::MAX),
        clock_skew_leeway: Duration::MAX,
        ..config()
    };
    let (engine, _llm, _journal) = engine(script(), vec![tool.clone()], config);
    let run = RunId::new();

    let paused = engine
        .run(run, "draft")
        .await
        .expect("run survives the config");
    assert!(paused.approval().expect("paused").expires_at.is_some());

    let outcome = engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume survives the config");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1);
}

/// EDGE E13: clock skew. A resolver whose clock runs ahead of the writer's must
/// not throw away a decision a human made in time.
#[tokio::test]
async fn approve_within_clock_skew_leeway_succeeds() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let config = EngineConfig {
        approval_ttl: Some(Duration::ZERO),
        clock_skew_leeway: Duration::from_secs(300),
        ..config()
    };
    let (engine, _llm, _journal) = engine(script(), vec![tool.clone()], config);
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    tokio::time::sleep(Duration::from_millis(20)).await;

    let outcome = engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1);
}

/// EDGE E42: no TTL means no expiry, however long the human takes.
#[tokio::test]
async fn approval_without_ttl_never_expires() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let config = EngineConfig {
        approval_ttl: None,
        ..config()
    };
    let (engine, _llm, _journal) = engine(script(), vec![tool.clone()], config);
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    assert_eq!(paused.approval().expect("paused").expires_at, None);
    tokio::time::sleep(Duration::from_millis(20)).await;

    let outcome = engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1);
}

/// EDGE E28 and E29: resolving something that is not pending is a typed error,
/// and leaves the journal untouched.
#[tokio::test]
async fn resume_without_pending_approval_errors() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({})),
            ChatResponse::text("ok"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    // A run that has never been started.
    assert!(matches!(
        engine.resume(run, Resolution::approve(3)).await,
        Err(Error::NoPendingApproval(_))
    ));
    assert!(matches!(
        engine.resume(run, Resolution::timer(3)).await,
        Err(Error::NotWaiting(_))
    ));

    // A run that crashed mid-step, so it is running rather than paused.
    journal.fail_append_at(4);
    engine
        .run(run, "note")
        .await
        .expect_err("the injected failure aborts the run");
    let entries = journal.entries(run).len();

    assert!(matches!(
        engine.resume(run, Resolution::approve(3)).await,
        Err(Error::NoPendingApproval(_))
    ));
    assert!(matches!(
        engine.resume(run, Resolution::timer(3)).await,
        Err(Error::NotWaiting(_))
    ));
    assert_eq!(
        journal.entries(run).len(),
        entries,
        "a rejected resume appends nothing"
    );
}

#[tokio::test]
async fn timer_resume_when_not_waiting_errors() {
    let tool = Arc::new(CountingTool::mutating("draft"));
    let (engine, _llm, journal) = engine(script(), vec![tool], config());
    let run = RunId::new();

    let paused = engine.run(run, "draft").await.expect("run succeeds");
    let entries = journal.entries(run).len();

    let result = engine
        .resume(run, Resolution::timer(pending_step(&paused)))
        .await;

    assert!(matches!(result, Err(Error::NotWaiting(_))));
    assert_eq!(journal.entries(run).len(), entries);
}

/// EDGE E34: a turn with several calls, one of them gated. Everything before
/// the gate runs, the run pauses, and everything after it waits for the human —
/// then the whole turn finishes in the order the model asked for.
#[tokio::test]
async fn multi_call_turn_pauses_midway_and_resumes_in_order() {
    let first = Arc::new(CountingTool::new("read"));
    let gated = Arc::new(CountingTool::mutating("draft"));
    let last = Arc::new(CountingTool::new("note"));
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_calls(vec![
                ToolCall::new("c1", "read", json!({"i": 1})),
                ToolCall::new("c2", "draft", json!({"i": 2})),
                ToolCall::new("c3", "note", json!({"i": 3})),
            ]),
            ChatResponse::text("all done"),
        ],
        vec![first.clone(), gated.clone(), last.clone()],
        config(),
    );
    let run = RunId::new();

    let paused = engine
        .run(run, "do three things")
        .await
        .expect("run succeeds");

    assert_eq!(paused.status(), RunStatus::PendingApproval);
    assert_eq!(first.executions(), 1, "the call before the gate ran");
    assert_eq!(gated.executions(), 0);
    assert_eq!(last.executions(), 0, "the call after the gate did not");
    let requested: ApprovalRequested = only_payload(&journal, run, EntryKind::ApprovalRequested);
    assert_eq!(requested.step_seq, 5);
    assert_eq!(requested.call_id, "c2");

    let done = engine
        .resume(run, Resolution::approve(pending_step(&paused)))
        .await
        .expect("resume succeeds");

    assert_eq!(done.status(), RunStatus::Done);
    assert_eq!(done.stats().steps, 3);
    assert_eq!(
        (first.executions(), gated.executions(), last.executions()),
        (1, 1, 1)
    );
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
            (4, "step_done"),
            (5, "approval_requested"),
            (6, "approval_resolved"),
            (7, "step_started"),
            (8, "step_done"),
            (9, "step_started"),
            (10, "step_done"),
            (11, "model_response"),
            (12, "run_ended"),
        ],
    );

    // The model sees the three results in the order it asked for them.
    let final_request = llm.requests().pop().expect("a second turn happened");
    let ids: Vec<String> = final_request
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["c1", "c2", "c3"]);
}
