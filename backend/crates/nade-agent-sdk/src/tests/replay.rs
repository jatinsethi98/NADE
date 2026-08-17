//! Crash and replay: the reason the journal exists.
//!
//! Each test kills the run at a different point in the journal-before-effect
//! protocol and then asserts the only thing that matters — how many distinct
//! side effects survived.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;

use super::{assert_layout, config, engine, entries_of, only_payload};
use crate::ids::{effect_id, RunId};
use crate::journal::{EntryKind, RunStarted, RunWaiting, StepStarted};
use crate::message::{ChatResponse, Message};
use crate::run::{Resolution, RunOutcome, RunStatus};
use crate::testing::{CountingTool, WaitTool};

/// One tool call, then a closing answer. The clean journal is:
/// `1 run_started, 2 model_response, 3 step_started, 4 step_done,
///  5 model_response, 6 run_ended`.
fn script() -> Vec<ChatResponse> {
    vec![
        ChatResponse::tool_call("c1", "note", json!({"body": "once"})).with_usage(10, 5),
        ChatResponse::text("saved").with_usage(20, 4),
    ]
}

/// EDGE E8: killed before the fence commits. Nothing ran, so the resumed run
/// starts the step cleanly.
#[tokio::test]
async fn crash_before_step_started_commit_runs_once() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    journal.fail_append_at(3); // the `step_started` write never lands
    let crashed = engine.run(run, "take a note").await;

    assert!(crashed.is_err(), "the injected failure aborts the run");
    assert_eq!(tool.executions(), 0, "the tool is never reached");
    assert_layout(&journal, run, &[(1, "run_started"), (2, "model_response")]);

    let outcome = engine
        .run(run, "take a note")
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1);
    assert_eq!(tool.effects().len(), 1);
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
            (4, "step_done"),
            (5, "model_response"),
            (6, "run_ended"),
        ],
    );
}

/// EDGE E6: killed between the fence and the effect. The journal says a step
/// may have run; it had not. Replay runs it, once.
#[tokio::test]
async fn crash_between_step_started_and_effect_executes_once() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    // The entry commits; the process dies before it learns that it did, and so
    // before the tool is dispatched.
    journal.fail_append_after_commit_at(3);
    let crashed = engine.run(run, "take a note").await;

    assert!(crashed.is_err());
    assert_eq!(tool.executions(), 0, "the effect never happened");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
        ],
    );

    let outcome = engine
        .run(run, "take a note")
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1, "exactly one execution");
    assert_eq!(tool.effects().len(), 1, "exactly one effect");
    // The retry is recorded as a second attempt at the *same* step.
    let started = entries_of(&journal, run, EntryKind::StepStarted);
    assert_eq!(started.len(), 2);
    let attempts: Vec<(u32, u32)> = started
        .iter()
        .map(|e| {
            let p: StepStarted = e.payload_as(run).expect("decodes");
            (p.step_seq, p.attempt)
        })
        .collect();
    assert_eq!(attempts, vec![(3, 1), (3, 2)]);
    assert_eq!(outcome.stats().steps, 1, "a retry is not a second step");
}

/// EDGE E7: the nastiest one. Killed *after* the effect landed and *before*
/// `step_done` committed. Replay cannot know the effect happened, so it runs the
/// tool a second time — and the deterministic id makes that second run land on
/// the first one's row.
#[tokio::test]
async fn crash_between_effect_and_step_done_still_one_effect() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    journal.fail_append_at(4); // the `step_done` write never lands
    let crashed = engine.run(run, "take a note").await;

    assert!(crashed.is_err());
    assert_eq!(tool.executions(), 1, "the effect really did happen");
    assert_eq!(tool.effects().len(), 1);
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
        ],
    );

    let outcome = engine
        .run(run, "take a note")
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    // Two executions...
    assert_eq!(
        tool.executions(),
        2,
        "replay must re-execute: it cannot know"
    );
    // ...one effect. This is the entire point of `uuid5(run_id ‖ step_seq)`.
    let effects = tool.effects();
    assert_eq!(effects.len(), 1, "still exactly one effect");
    let (id, writes) = effects.into_iter().next().expect("one effect");
    assert_eq!(
        id,
        effect_id(run, 3),
        "both attempts used the id derived from the step's seq"
    );
    assert_eq!(writes, 2, "two upserts onto one row");

    // Both dispatches carried the same id, and the second knew it was a retry.
    let calls = tool.calls();
    assert_eq!(calls[0].effect_id(), calls[1].effect_id());
    assert!(!calls[0].is_replay());
    assert!(
        calls[1].is_replay(),
        "the second dispatch is flagged as a replay"
    );
    assert_eq!(outcome.stats().steps, 1);
}

/// Killed before the model's own turn was journaled: the turn is simply bought
/// again. Nothing was executed, so nothing can be duplicated — the cost of a
/// crash here is one wasted model call, never a repeated effect.
#[tokio::test]
async fn crash_before_model_response_commit_re_asks_the_model() {
    let tool = Arc::new(CountingTool::new("note"));
    let mut responses = script();
    // One extra copy of the first turn: the crash throws the first away.
    responses.insert(0, responses[0].clone());
    let (engine, llm, journal) = engine(responses, vec![tool.clone()], config());
    let run = RunId::new();

    journal.fail_append_at(2);
    engine
        .run(run, "take a note")
        .await
        .expect_err("the injected failure aborts the run");

    assert_layout(&journal, run, &[(1, "run_started")]);
    assert_eq!(llm.turns(), 1);

    let outcome = engine
        .run(run, "take a note")
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(llm.turns(), 3, "the lost turn was bought again");
    assert_eq!(
        tool.executions(),
        1,
        "but the effect still happened exactly once"
    );
    assert_eq!(tool.effects().len(), 1);
}

/// EDGE E9: a worker that retries `run` does not restart the run, and the input
/// recorded at the start wins over whatever the retry passes in.
#[tokio::test]
async fn duplicate_run_replays_instead_of_restarting() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, llm, journal) = engine(script(), vec![tool.clone()], config());
    let run = RunId::new();

    journal.fail_append_at(3);
    engine
        .run(run, "the original instruction")
        .await
        .expect_err("crashes");

    let outcome = engine
        .run(run, "a completely different instruction")
        .await
        .expect("resume succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(
        entries_of(&journal, run, EntryKind::RunStarted).len(),
        1,
        "started once"
    );
    let started: RunStarted = only_payload(&journal, run, EntryKind::RunStarted);
    assert_eq!(
        started.input.messages,
        vec![Message::user("the original instruction")]
    );

    // The rebuilt conversation still opens with the original instruction, and
    // the model was never asked to redo the turn it had already answered.
    let last = llm.requests().pop().expect("a turn happened");
    assert_eq!(last.messages[0], Message::user("the original instruction"));
    assert_eq!(llm.turns(), 2, "one turn before the crash, one after");
    assert_eq!(tool.executions(), 1);
}

/// EDGE E30: `running → waiting(wake_at) --timer--> running`.
#[tokio::test]
async fn waiting_then_timer_resumes_and_completes() {
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

    let parked = engine.run(run, "wait a while").await.expect("run succeeds");

    assert!(matches!(&parked, RunOutcome::Waiting { wake_at: at, .. } if *at == wake_at));
    assert_eq!(parked.status(), RunStatus::Waiting);
    assert_eq!(llm.turns(), 1, "the model is not asked again while parked");
    let waiting: RunWaiting = only_payload(&journal, run, EntryKind::RunWaiting);
    assert_eq!(waiting.step_seq, 3);
    assert_eq!(waiting.wake_at, wake_at);

    // Re-running a parked run changes nothing.
    let still = engine.run(run, "wait").await.expect("noop");
    assert_eq!(still.status(), RunStatus::Waiting);
    assert_eq!(tool.executions(), 1);

    let outcome = engine
        .resume(run, Resolution::Timer)
        .await
        .expect("timer fires");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.executions(), 1, "the parking tool is not re-run");
    assert_layout(
        &journal,
        run,
        &[
            (1, "run_started"),
            (2, "model_response"),
            (3, "step_started"),
            (4, "step_done"),
            (5, "run_waiting"),
            (6, "run_woken"),
            (7, "model_response"),
            (8, "run_ended"),
        ],
    );

    // The control envelope is stripped: the model sees the tool's own result.
    let final_request = llm.requests().pop().expect("a second turn happened");
    let tool_message = final_request
        .messages
        .iter()
        .find(|m| m.role() == "tool")
        .expect("tool message");
    match tool_message {
        Message::Tool { result, .. } => {
            assert_eq!(result, &json!({"parked": true}));
            assert!(result.get(crate::control::CONTROL_KEY).is_none());
        }
        other => panic!("expected a tool message, got {other:?}"),
    }
}
