//! Caps, at the boundary and one past it.

use std::sync::Arc;

use serde_json::json;

use super::{config, engine, entries_of, only_payload};
use crate::engine::EngineConfig;
use crate::ids::RunId;
use crate::journal::{CapBreached, EntryKind, RunEnded};
use crate::message::ChatResponse;
use crate::run::{FailureReason, RunOutcome, RunStatus};
use crate::testing::CountingTool;

fn call() -> ChatResponse {
    ChatResponse::tool_call("", "note", json!({})).with_usage(1, 1)
}

/// EDGE E14: exactly `max_steps` steps is fine.
#[tokio::test]
async fn step_cap_boundary_exactly_at_limit_succeeds() {
    let tool = Arc::new(CountingTool::new("note"));
    let config = EngineConfig {
        max_steps: 3,
        ..config()
    };
    let (engine, _llm, _journal) = engine(
        vec![call(), call(), call(), ChatResponse::text("done")],
        vec![tool.clone()],
        config,
    );

    let outcome = engine
        .run(RunId::new(), "loop")
        .await
        .expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(outcome.stats().steps, 3);
    assert_eq!(tool.executions(), 3);
}

/// EDGE E15: one step past the cap fails, and the breach is journaled.
#[tokio::test]
async fn step_cap_one_over_limit_fails() {
    let tool = Arc::new(CountingTool::new("note"));
    let config = EngineConfig {
        max_steps: 3,
        ..config()
    };
    let (engine, llm, journal) = engine(
        vec![
            call(),
            call(),
            call(),
            call(),
            ChatResponse::text("unreachable"),
        ],
        vec![tool.clone()],
        config,
    );
    let run = RunId::new();

    let outcome = engine
        .run(run, "loop")
        .await
        .expect("run completes with a failure");

    assert!(matches!(
        &outcome,
        RunOutcome::Failed {
            reason: FailureReason::StepCapExceeded { limit: 3, taken: 3 },
            ..
        }
    ));
    assert_eq!(tool.executions(), 3, "the fourth step never opened");
    assert_eq!(llm.turns(), 4);

    let breach: CapBreached = only_payload(&journal, run, EntryKind::CapBreached);
    assert!(matches!(
        breach.reason,
        FailureReason::StepCapExceeded { limit: 3, taken: 3 }
    ));
    let ended: RunEnded = only_payload(&journal, run, EntryKind::RunEnded);
    assert_eq!(ended.status, RunStatus::Failed);
    assert_eq!(ended.steps, 3);

    // The failure is terminal: re-running reads it back rather than retrying.
    let entries = journal.entries(run).len();
    let again = engine.run(run, "loop").await.expect("noop");
    assert_eq!(again.status(), RunStatus::Failed);
    assert_eq!(journal.entries(run).len(), entries);
}

/// EDGE E23: a model that would call the same tool forever is stopped by the
/// step cap and nothing else.
#[tokio::test]
async fn model_looping_forever_is_stopped_by_step_cap() {
    let tool = Arc::new(CountingTool::new("note"));
    let config = EngineConfig {
        max_steps: 4,
        ..config()
    };
    let (engine, _llm, _journal) = engine(vec![call(); 50], vec![tool.clone()], config);

    let outcome = engine
        .run(RunId::new(), "spin")
        .await
        .expect("run completes");

    assert!(matches!(
        &outcome,
        RunOutcome::Failed {
            reason: FailureReason::StepCapExceeded { limit: 4, .. },
            ..
        }
    ));
    assert_eq!(tool.executions(), 4);
}

/// EDGE E16: spending exactly the budget is allowed.
#[tokio::test]
async fn token_budget_boundary_exactly_at_limit_succeeds() {
    let tool = Arc::new(CountingTool::new("note"));
    let config = EngineConfig {
        token_budget: 100,
        ..config()
    };
    let (engine, _llm, _journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({})).with_usage(40, 10),
            ChatResponse::text("done").with_usage(40, 10),
        ],
        vec![tool.clone()],
        config,
    );

    let outcome = engine
        .run(RunId::new(), "spend it all")
        .await
        .expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(outcome.stats().usage.total(), 100);
}

/// EDGE E17: one token past the budget fails, and the spend that caused it is
/// journaled before the breach is.
#[tokio::test]
async fn token_budget_one_over_limit_fails() {
    let tool = Arc::new(CountingTool::new("note"));
    let config = EngineConfig {
        token_budget: 100,
        ..config()
    };
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({})).with_usage(40, 10),
            ChatResponse::text("done").with_usage(40, 11),
        ],
        vec![tool.clone()],
        config,
    );
    let run = RunId::new();

    let outcome = engine
        .run(run, "overspend")
        .await
        .expect("run completes with a failure");

    assert!(matches!(
        &outcome,
        RunOutcome::Failed {
            reason: FailureReason::TokenBudgetExceeded {
                limit: 100,
                spent: 101
            },
            ..
        }
    ));
    assert_eq!(outcome.stats().usage.total(), 101);
    assert_eq!(
        entries_of(&journal, run, EntryKind::ModelResponse).len(),
        2,
        "both turns recorded"
    );
    let breach: CapBreached = only_payload(&journal, run, EntryKind::CapBreached);
    assert!(matches!(
        breach.reason,
        FailureReason::TokenBudgetExceeded { spent: 101, .. }
    ));
}

/// EDGE E18: a run with nothing left to spend does not call the model at all.
#[tokio::test]
async fn token_budget_blocks_next_turn_when_exhausted() {
    let tool = Arc::new(CountingTool::new("note"));
    let config = EngineConfig {
        token_budget: 50,
        ..config()
    };
    let (engine, llm, _journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({})).with_usage(40, 10),
            ChatResponse::text("never asked for").with_usage(1, 1),
        ],
        vec![tool.clone()],
        config,
    );

    let outcome = engine
        .run(RunId::new(), "one turn only")
        .await
        .expect("run completes");

    assert!(matches!(
        &outcome,
        RunOutcome::Failed {
            reason: FailureReason::TokenBudgetExceeded {
                limit: 50,
                spent: 50
            },
            ..
        }
    ));
    assert_eq!(llm.turns(), 1, "the second turn was refused, not attempted");
    assert_eq!(llm.remaining(), 1, "its scripted response went unused");
    assert_eq!(tool.executions(), 1, "the first step still ran");
}
