//! The paths that work, plus the shapes everything else is asserted against.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::{assert_layout, config, engine, entries_of, only_payload};
use crate::engine::{Engine, EngineConfig};
use crate::ids::{args_hash, effect_id, RunId, EFFECT_NAMESPACE};
use crate::journal::{EntryKind, Journal, ModelResponse, RunStarted, StepDone, StepStarted};
use crate::llm::Llm;
use crate::message::{ChatResponse, Message, StopReason, ToolCall};
use crate::run::{RunInput, RunOutcome, RunStatus};
use crate::testing::{CountingTool, EchoTool, MemoryJournal, ScriptedLlm};
use crate::tool::Tool;
use crate::Error;

#[tokio::test]
async fn plain_answer_completes() {
    let (engine, llm, journal) = engine(
        vec![ChatResponse::text("hello").with_usage(10, 3)],
        vec![],
        config(),
    );
    let run = RunId::new();

    let outcome = engine.run(run, "hi").await.expect("run succeeds");

    assert!(matches!(&outcome, RunOutcome::Done { output: Some(o), .. } if o == "hello"));
    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(outcome.stats().steps, 0);
    assert_eq!(outcome.stats().usage.total(), 13);
    assert_eq!(llm.turns(), 1);
    assert_layout(
        &journal,
        run,
        &[(1, "run_started"), (2, "model_response"), (3, "run_ended")],
    );
}

/// EDGE E37: the journal layout is a contract. A change here is a change to the
/// recovery protocol and to every stored run.
#[tokio::test]
async fn journal_layout_is_stable() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", json!({"body": "x"})).with_usage(10, 5),
            ChatResponse::text("saved").with_usage(20, 4),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let outcome = engine.run(run, "take a note").await.expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(outcome.stats().steps, 1);
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

    // The step is identified by the seq that opened it, and the effect id is
    // derived from exactly that.
    let started: StepStarted = only_payload(&journal, run, EntryKind::StepStarted);
    assert_eq!(started.step_seq, 3);
    assert_eq!(started.attempt, 1);
    assert_eq!(started.effect_id, effect_id(run, 3));
    assert_eq!(started.args_hash, args_hash(&json!({"body": "x"})));

    let done: StepDone = only_payload(&journal, run, EntryKind::StepDone);
    assert_eq!(done.step_seq, 3);
    assert!(!done.is_error);
    assert!(!done.truncated);

    // And the tool was handed that same id.
    assert_eq!(tool.calls()[0].effect_id(), Some(effect_id(run, 3)));
    assert!(!tool.calls()[0].is_replay());
}

/// EDGE E36: `effect_id` is a frozen public contract. If this vector changes,
/// every journal ever written stops replaying onto the same effects.
#[test]
fn effect_id_golden_vector() {
    assert_eq!(
        EFFECT_NAMESPACE.to_string(),
        "6e616465-5f65-6666-6563-745f6e737631"
    );
    let nil = RunId::from_uuid(Uuid::nil());
    assert_eq!(
        effect_id(nil, 1).to_string(),
        "e7293919-5143-5626-903c-d82c4d5be3c8"
    );
    assert_eq!(
        effect_id(nil, 3).to_string(),
        "1ac9540d-d1eb-5c36-8fcf-58ce0a96f997"
    );
    assert_eq!(
        effect_id(nil, 5).to_string(),
        "01ba250f-ac9e-5afd-9b06-f18ce4fd5af0"
    );
}

/// A system prompt frames the run and reaches the model ahead of everything else.
#[tokio::test]
async fn system_prompt_is_prepended_to_every_turn() {
    let (engine, llm, journal) = engine(vec![ChatResponse::text("understood")], vec![], config());
    let run = RunId::new();
    let input = RunInput::user("hi").with_system("be terse");

    engine.run(run, input.clone()).await.expect("run succeeds");

    let requests = llm.requests();
    assert_eq!(
        requests[0].messages,
        vec![Message::system("be terse"), Message::user("hi")]
    );
    // And the whole input is what replay would rebuild from.
    let started: RunStarted = only_payload(&journal, run, EntryKind::RunStarted);
    assert_eq!(started.input, input);
}

/// EDGE E1: an empty tool set is legal. The model is told it has no tools.
#[tokio::test]
async fn empty_tool_list_completes() {
    let (engine, llm, _journal) =
        engine(vec![ChatResponse::text("nothing to do")], vec![], config());

    let outcome = engine.run(RunId::new(), "hi").await.expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert!(llm.requests()[0].tools.is_empty(), "no tools advertised");
    assert!(engine.tools().is_empty());
}

/// EDGE E2: a model that says nothing has finished, not failed.
#[tokio::test]
async fn empty_model_output_is_done_with_no_output() {
    let (engine, _llm, _journal) = engine(
        vec![ChatResponse {
            text: None,
            ..ChatResponse::text("")
        }],
        vec![],
        config(),
    );

    let outcome = engine.run(RunId::new(), "hi").await.expect("run succeeds");

    assert!(matches!(outcome, RunOutcome::Done { output: None, .. }));
}

/// EDGE E3: whitespace-only prose is trimmed away in the outcome, but the raw
/// text stays in the journal.
#[tokio::test]
async fn whitespace_model_output_is_trimmed_to_none() {
    let (engine, _llm, journal) = engine(vec![ChatResponse::text("  \n\t  ")], vec![], config());
    let run = RunId::new();

    let outcome = engine.run(run, "hi").await.expect("run succeeds");

    assert!(matches!(outcome, RunOutcome::Done { output: None, .. }));
    let response: ModelResponse = only_payload(&journal, run, EntryKind::ModelResponse);
    assert_eq!(
        response.text.as_deref(),
        Some("  \n\t  "),
        "raw text is preserved"
    );
}

/// EDGE E35: `stop_reason: tool_use` with no calls ends the run instead of
/// looping on empty turns.
#[tokio::test]
async fn tool_use_stop_reason_without_calls_ends_the_run() {
    let (engine, llm, _journal) = engine(
        vec![ChatResponse {
            text: Some("I meant to call something".to_string()),
            tool_calls: vec![],
            stop_reason: StopReason::ToolUse,
            usage: Default::default(),
        }],
        vec![Arc::new(EchoTool::new())],
        config(),
    );

    let outcome = engine.run(RunId::new(), "hi").await.expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(llm.turns(), 1, "no second turn was asked for");
}

/// EDGE E4: unicode survives the journal byte for byte, and the argument hash
/// is stable across processes.
#[tokio::test]
async fn unicode_args_and_results_round_trip() {
    let args = json!({
        "note": "café 漢字 🙂 שלום",
        "n": 1,
        "nested": {"z": true, "a": [1, "é"]},
    });
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "note", args.clone()),
            ChatResponse::text("done ✅"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let outcome = engine
        .run(run, "unicode please")
        .await
        .expect("run succeeds");

    assert!(matches!(&outcome, RunOutcome::Done { output: Some(o), .. } if o == "done ✅"));
    assert_eq!(
        tool.calls()[0].arguments,
        args,
        "tool sees the arguments verbatim"
    );

    let started: StepStarted = only_payload(&journal, run, EntryKind::StepStarted);
    assert_eq!(started.args, args, "journal stores the arguments verbatim");
    // Pinned: canonical JSON is compact with sorted keys and raw UTF-8, so the
    // same arguments hash the same on any machine.
    assert_eq!(
        started.args_hash,
        "sha256:d751c379d290b1e6fbda49ac1d5e60238175f2c97680b47a094f9ff2f1a367c6"
    );

    let done: StepDone = only_payload(&journal, run, EntryKind::StepDone);
    assert_eq!(done.result["args"], args);

    // And the model was shown the result unmangled.
    let last = llm.requests().pop().expect("a second turn happened");
    let tool_message = last
        .messages
        .iter()
        .find(|m| m.role() == "tool")
        .expect("tool message");
    match tool_message {
        Message::Tool { result, .. } => assert_eq!(result["args"], args),
        other => panic!("expected a tool message, got {other:?}"),
    }
}

/// EDGE E41: a model that emits a scalar or an array instead of an object is
/// passed through so the tool can complain in terms the model understands.
#[tokio::test]
async fn non_object_arguments_are_passed_through() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_calls(vec![
                ToolCall::new("c1", "note", json!("just a string")),
                ToolCall::new("c2", "note", json!([1, 2, 3])),
            ]),
            ChatResponse::text("ok"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    let outcome = engine.run(run, "weird args").await.expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(tool.calls()[0].arguments, json!("just a string"));
    assert_eq!(tool.calls()[1].arguments, json!([1, 2, 3]));
    let started = entries_of(&journal, run, EntryKind::StepStarted);
    assert_eq!(started.len(), 2);
}

/// EDGE E33: blank and repeated call ids are normalised once, before anything
/// is journaled, so replay always matches results back to calls.
#[tokio::test]
async fn duplicate_and_empty_call_ids_are_normalised() {
    let tool = Arc::new(CountingTool::new("note"));
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_calls(vec![
                ToolCall::new("  ", "note", json!({"i": 0})),
                ToolCall::new("dup", "note", json!({"i": 1})),
                ToolCall::new("dup", "note", json!({"i": 2})),
            ]),
            ChatResponse::text("ok"),
        ],
        vec![tool.clone()],
        config(),
    );
    let run = RunId::new();

    engine.run(run, "three calls").await.expect("run succeeds");

    let response: ModelResponse = super::entries_of(&journal, run, EntryKind::ModelResponse)
        .remove(0)
        .payload_as(run)
        .expect("payload decodes");
    let ids: Vec<&str> = response.tool_calls.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["call_1_0", "dup", "call_1_2"]);

    let dones = entries_of(&journal, run, EntryKind::StepDone);
    let mut done_ids: Vec<String> = dones
        .iter()
        .map(|e| e.payload_as::<StepDone>(run).expect("decodes").call_id)
        .collect();
    done_ids.sort();
    assert_eq!(done_ids, vec!["call_1_0", "call_1_2", "dup"]);
    assert_eq!(tool.executions(), 3);
    assert_eq!(
        tool.effects().len(),
        3,
        "three distinct steps, three distinct effects"
    );
}

/// EDGE E32: two tools cannot share a name.
#[test]
fn duplicate_tool_names_rejected() {
    let result = Engine::new(
        ScriptedLlm::new(vec![]),
        vec![
            Arc::new(EchoTool::named("same")) as Arc<dyn Tool>,
            Arc::new(EchoTool::named("same")) as Arc<dyn Tool>,
        ],
        MemoryJournal::new(),
        EngineConfig::default(),
    );

    assert!(matches!(result, Err(Error::DuplicateTool(name)) if name == "same"));
}

/// EDGE E31: the journal's primary key is what stops two workers from driving
/// one run into two different futures. The engine surfaces the conflict as an
/// error — the crash-replay tests cover that propagation path.
#[tokio::test]
async fn seq_conflict_is_detected() {
    let journal = MemoryJournal::new();
    let run = RunId::new();
    let entry = crate::journal::Entry::new(1, EntryKind::RunStarted, &json!({"input": {}}))
        .expect("entry builds");

    assert_eq!(
        journal
            .append(run, entry.clone())
            .await
            .expect("first append"),
        1
    );
    let second = journal.append(run, entry).await;

    assert!(matches!(second, Err(Error::SeqConflict { seq: 1, .. })));
}

/// A8: a server must be able to hold the engine in an `Arc` and use it from a
/// spawned task, through the blanket `Arc<dyn …>` impls of all three traits.
#[tokio::test(flavor = "multi_thread")]
async fn engine_is_send_sync_and_static() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_sync_static::<Engine<Arc<ScriptedLlm>, MemoryJournal>>();
    assert_send_sync_static::<Engine<Arc<dyn Llm>, Arc<dyn Journal>>>();

    let llm: Arc<dyn Llm> = Arc::new(ScriptedLlm::new(vec![ChatResponse::text("hi")]));
    let journal: Arc<dyn Journal> = Arc::new(MemoryJournal::new());
    let engine = Arc::new(
        Engine::new(
            llm,
            vec![Arc::new(EchoTool::new()) as Arc<dyn Tool>],
            journal,
            config(),
        )
        .expect("engine builds"),
    );

    let handle = tokio::spawn({
        let engine = Arc::clone(&engine);
        async move { engine.run(RunId::new(), "hi").await }
    });

    let outcome = handle.await.expect("task joins").expect("run succeeds");
    assert_eq!(outcome.status(), RunStatus::Done);
}
