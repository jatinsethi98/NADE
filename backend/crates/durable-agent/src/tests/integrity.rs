//! M8: replay validates a journal before it interprets one.
//!
//! Every test here takes a journal that is valid in every respect but one,
//! damages exactly that one thing, and asserts the engine refuses it. The
//! damage is always something that would otherwise let a step reach dispatch
//! under an identity — a `step_seq`, an `effect_id` — that was minted for
//! something else, or under a history with pieces missing.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;

use super::{config, engine_over};
use crate::ids::{args_hash, effect_id, RunId};
use crate::journal::{
    Entry, EntryKind, ModelResponse, RunEnded, RunStarted, RunWoken, StepDone, StepStarted,
};
use crate::message::{ChatResponse, StopReason, ToolCall};
use crate::run::{RunInput, RunStatus};
use crate::testing::{CountingTool, MemoryJournal};
use crate::tool::{tool_fingerprint, ReplayPolicy, Tool};
use crate::Error;

fn note_tool() -> Arc<CountingTool> {
    Arc::new(CountingTool::new("note"))
}

/// A fixed instant so every entry in a hand-built journal is ordered and the
/// clock plays no part in what these tests measure. It is in the *past*: an
/// entry may not claim to have been created ahead of the local clock, and a step
/// may not claim to have opened after the entry recording it.
fn when() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a valid instant")
}

fn at<P: serde::Serialize>(seq: u32, kind: EntryKind, payload: &P) -> Entry {
    Entry::at(seq, kind, payload, when()).expect("entry builds")
}

/// The journal of a run that called `note` once and then answered. Valid.
fn healthy(run: RunId) -> Vec<Entry> {
    let args = json!({"body": "x"});
    let fingerprint = Some(tool_fingerprint(&*note_tool()));
    vec![
        at(
            1,
            EntryKind::RunStarted,
            &RunStarted::new(RunInput::user("take a note")),
        ),
        at(
            2,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 1,
                text: None,
                tool_calls: vec![ToolCall::new("c1", "note", args.clone())],
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
        ),
        at(
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
                opened_at: when(),
                tool_fingerprint: fingerprint,
                replay_policy: ReplayPolicy::Retry,
            },
        ),
        at(
            4,
            EntryKind::StepDone,
            &StepDone {
                step_seq: 3,
                call_id: "c1".to_string(),
                tool: "note".to_string(),
                result: json!({"ok": true}),
                is_error: false,
                truncated: false,
                wake_at: None,
            },
        ),
        at(
            5,
            EntryKind::ModelResponse,
            &ModelResponse {
                turn: 2,
                text: Some("saved".to_string()),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ),
        at(
            6,
            EntryKind::RunEnded,
            &RunEnded {
                status: RunStatus::Done,
                output: Some("saved".to_string()),
                reason: None,
                steps: 1,
                usage: Default::default(),
            },
        ),
    ]
}

/// Load `entries` as `run`'s journal and try to drive it.
async fn drive(run: RunId, entries: Vec<Entry>) -> crate::Result<crate::RunOutcome> {
    let journal = MemoryJournal::new();
    for entry in entries {
        journal.force_append(run, entry);
    }
    let tool = note_tool();
    let (engine, _llm) = engine_over(
        vec![ChatResponse::text("an unscripted turn would panic")],
        vec![tool as Arc<dyn Tool>],
        config(),
        journal,
    );
    engine.run(run, "take a note").await
}

/// Damage the journal in one way and assert the engine says why.
async fn rejects(label: &str, run: RunId, entries: Vec<Entry>, expect: &str) {
    let result = drive(run, entries).await;
    match result {
        Err(Error::CorruptJournal { message, .. }) => assert!(
            message.contains(expect),
            "{label}: expected a message mentioning {expect:?}, got {message:?}"
        ),
        other => panic!("{label}: expected CorruptJournal, got {other:?}"),
    }
}

/// The control: the undamaged journal replays to its recorded outcome.
#[tokio::test]
async fn a_healthy_journal_replays() {
    let run = RunId::new();
    let outcome = drive(run, healthy(run)).await.expect("replays");
    assert_eq!(outcome.status(), RunStatus::Done);
}

#[tokio::test]
async fn a_gap_in_the_sequence_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    entries.remove(2); // seq 3 vanishes; 4 now follows 2
    rejects("gap", run, entries, "expected 3").await;
}

#[tokio::test]
async fn a_journal_that_does_not_start_at_one_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    // A well-formed `run_started`, just not at the sequence it must occupy.
    entries[0] = at(
        2,
        EntryKind::RunStarted,
        &RunStarted::new(RunInput::user("take a note")),
    );
    entries.truncate(1);
    rejects("no seq 1", run, entries, "expected 1").await;
}

#[tokio::test]
async fn a_journal_that_does_not_open_with_run_started_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    entries[0] = at(
        1,
        EntryKind::ModelResponse,
        &ModelResponse {
            turn: 1,
            text: Some("hi".to_string()),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    );
    rejects("wrong opener", run, entries, "expected 'run_started'").await;
}

#[tokio::test]
async fn a_second_run_started_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    entries[1] = at(
        2,
        EntryKind::RunStarted,
        &RunStarted::new(RunInput::user("again")),
    );
    rejects("two starts", run, entries, "second 'run_started'").await;
}

/// The one that matters most: an entry claiming an effect id its step does not
/// derive would put a foreign — or colliding — identity on a side effect.
#[tokio::test]
async fn a_foreign_effect_id_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut payload: StepStarted = entries[2].payload_as(run).expect("decodes");
    payload.effect_id = effect_id(run, 99); // some other step's identity
    entries[2] = at(3, EntryKind::StepStarted, &payload);
    rejects("foreign effect id", run, entries, "claims effect id").await;
}

/// The collision case, stated separately because it is the dangerous one: two
/// steps pointing at one effect id would silently merge two actions into one
/// row.
#[tokio::test]
async fn a_colliding_effect_id_is_refused() {
    let run = RunId::new();
    let other = RunId::new();
    let mut entries = healthy(run);
    let mut payload: StepStarted = entries[2].payload_as(run).expect("decodes");
    payload.effect_id = effect_id(other, 3); // another run's effect
    entries[2] = at(3, EntryKind::StepStarted, &payload);
    rejects("colliding effect id", run, entries, "claims effect id").await;
}

#[tokio::test]
async fn an_args_hash_that_does_not_match_its_arguments_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut payload: StepStarted = entries[2].payload_as(run).expect("decodes");
    payload.args_hash = args_hash(&json!({"body": "something else"}));
    entries[2] = at(3, EntryKind::StepStarted, &payload);
    rejects("bad args hash", run, entries, "claims args_hash").await;
}

#[tokio::test]
async fn an_opening_entry_whose_step_seq_is_not_its_own_seq_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut payload: StepStarted = entries[2].payload_as(run).expect("decodes");
    payload.step_seq = 2;
    payload.effect_id = effect_id(run, 2); // consistent, but still not its seq
    entries[2] = at(3, EntryKind::StepStarted, &payload);
    rejects("step_seq mismatch", run, entries, "claims step_seq").await;
}

#[tokio::test]
async fn a_retry_that_disagrees_with_the_original_step_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut retry: StepStarted = entries[2].payload_as(run).expect("decodes");
    retry.attempt = 2;
    retry.args = json!({"body": "a different instruction"});
    retry.args_hash = args_hash(&retry.args);
    // Drop `step_done` so the step is legitimately open, then retry it wrongly.
    entries.truncate(3);
    entries.push(at(4, EntryKind::StepStarted, &retry));
    rejects(
        "retry drift",
        run,
        entries,
        "disagrees with the entry that opened",
    )
    .await;
}

#[tokio::test]
async fn a_step_done_for_a_step_that_was_never_opened_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut payload: StepDone = entries[3].payload_as(run).expect("decodes");
    payload.call_id = "never_opened".to_string();
    entries[3] = at(4, EntryKind::StepDone, &payload);
    rejects("orphan step_done", run, entries, "never opened").await;
}

#[tokio::test]
async fn a_step_closed_twice_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let payload: StepDone = entries[3].payload_as(run).expect("decodes");
    entries.truncate(4);
    entries.push(at(5, EntryKind::StepDone, &payload));
    rejects("double close", run, entries, "twice").await;
}

#[tokio::test]
async fn an_entry_after_run_ended_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    entries.push(at(
        7,
        EntryKind::ModelResponse,
        &ModelResponse {
            turn: 3,
            text: Some("an afterthought".to_string()),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    ));
    rejects("post-terminal entry", run, entries, "follows 'run_ended'").await;
}

#[tokio::test]
async fn a_wake_up_with_no_wait_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    entries.truncate(4);
    entries.push(at(
        5,
        EntryKind::RunWoken,
        &RunWoken {
            step_seq: 3,
            woken_at: Utc::now(),
        },
    ));
    rejects("unpaired wake", run, entries, "not parked").await;
}

#[tokio::test]
async fn a_model_turn_out_of_order_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut payload: ModelResponse = entries[4].payload_as(run).expect("decodes");
    payload.turn = 7;
    entries[4] = at(5, EntryKind::ModelResponse, &payload);
    rejects("turn skew", run, entries, "is turn 7").await;
}

#[tokio::test]
async fn a_step_for_a_call_no_model_turn_produced_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    let mut started: StepStarted = entries[2].payload_as(run).expect("decodes");
    let mut done: StepDone = entries[3].payload_as(run).expect("decodes");
    started.call_id = "invented".to_string();
    done.call_id = "invented".to_string();
    entries[2] = at(3, EntryKind::StepStarted, &started);
    entries[3] = at(4, EntryKind::StepDone, &done);
    rejects("phantom call", run, entries, "which no model turn produced").await;
}

/// An unknown entry kind is readable — that is what `EntryKind::Other` is for —
/// but never *interpreted*, because guessing at its meaning is how a step gets
/// skipped or repeated.
#[tokio::test]
async fn an_unknown_entry_kind_is_refused() {
    let run = RunId::new();
    let mut entries = healthy(run);
    entries.truncate(4);
    entries.push(at(
        5,
        EntryKind::Other("something_from_the_future".to_string()),
        &json!({}),
    ));
    rejects("future kind", run, entries, "does not understand").await;
}
