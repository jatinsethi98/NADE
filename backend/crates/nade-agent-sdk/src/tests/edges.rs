//! Tools that misbehave, and models that ask for things that do not exist.

use std::sync::Arc;

use serde_json::json;

use super::{at, config, engine, only_payload};
use crate::engine::EngineConfig;
use crate::ids::RunId;
use crate::journal::{EntryKind, StepDone};
use crate::message::{ChatResponse, Message};
use crate::run::RunStatus;
use crate::testing::{EchoTool, FailingTool, HugeTool, PanicTool};
use crate::tool::{is_truncated, TRUNCATED_KEY};

/// EDGE E19: a panicking tool must not unwind out of the run — that would strand
/// it with a committed `step_started` and no `step_done`.
#[tokio::test]
async fn panicking_tool_is_caught_and_reported() {
    let (engine, _llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "boom", json!({})),
            ChatResponse::text("recovered"),
        ],
        vec![Arc::new(PanicTool)],
        config(),
    );
    let run = RunId::new();

    let outcome = engine
        .run(run, "explode")
        .await
        .expect("the run survives the panic");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(outcome.stats().steps, 1, "a panic still costs a step");
    let done: StepDone = only_payload(&journal, run, EntryKind::StepDone);
    assert!(done.is_error);
    assert_eq!(at(&done.result, &["error", "code"]), "tool_panicked");
    assert!(
        at(&done.result, &["error", "message"])
            .as_str()
            .expect("a message")
            .contains("boom"),
        "the panic message reaches the model: {}",
        done.result
    );
}

/// EDGE E20: a tool that returns `Err` is a message to the model, not the end
/// of the run.
#[tokio::test]
async fn tool_error_is_reported_to_model() {
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "flaky", json!({})),
            ChatResponse::text("understood"),
        ],
        vec![Arc::new(FailingTool)],
        config(),
    );
    let run = RunId::new();

    let outcome = engine.run(run, "try it").await.expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    let done: StepDone = only_payload(&journal, run, EntryKind::StepDone);
    assert!(done.is_error);
    assert_eq!(at(&done.result, &["error", "code"]), "tool_error");
    assert_eq!(at(&done.result, &["error", "tool"]), "flaky");

    let last = llm.requests().pop().expect("a second turn happened");
    let tool_message = last
        .messages
        .iter()
        .find(|m| m.role() == "tool")
        .expect("tool message");
    assert!(matches!(tool_message, Message::Tool { is_error: true, .. }));
}

/// EDGE E22: the host-enforced allowlist. An invented tool name comes back as a
/// structured error the model can act on — and costs a step, so hallucinating
/// names is not a free retry loop.
#[tokio::test]
async fn unknown_tool_call_is_structured_error_and_counts_a_step() {
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "send_email", json!({"to": "x"})),
            ChatResponse::text("sorry"),
        ],
        vec![Arc::new(EchoTool::new())],
        config(),
    );
    let run = RunId::new();

    let outcome = engine
        .run(run, "do something forbidden")
        .await
        .expect("run succeeds");

    assert_eq!(outcome.status(), RunStatus::Done);
    assert_eq!(
        outcome.stats().steps,
        1,
        "an unknown tool still costs a step"
    );

    let done: StepDone = only_payload(&journal, run, EntryKind::StepDone);
    assert!(done.is_error);
    assert_eq!(at(&done.result, &["error", "code"]), "unknown_tool");
    assert_eq!(at(&done.result, &["error", "tool"]), "send_email");
    assert_eq!(
        at(&done.result, &["error", "available_tools"]),
        &json!(["echo"])
    );

    // The model was never offered the name it invented.
    let requests = llm.requests();
    let names: Vec<&str> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["echo"]);
}

/// EDGE E21 and E5: a 10 MB result is replaced by an explicit envelope, the
/// journal entry stays bounded, and the preview is cut on a character boundary.
#[tokio::test]
async fn oversized_tool_result_is_truncated_with_marker() {
    let limit = 4096;
    let config = EngineConfig {
        max_tool_result_bytes: limit,
        ..config()
    };
    let (engine, llm, journal) = engine(
        vec![
            ChatResponse::tool_call("c1", "firehose", json!({})),
            ChatResponse::text("that was a lot"),
        ],
        vec![Arc::new(HugeTool::ten_megabytes())],
        config,
    );
    let run = RunId::new();

    let outcome = engine.run(run, "flood me").await.expect("run succeeds");
    assert_eq!(outcome.status(), RunStatus::Done);

    let done: StepDone = only_payload(&journal, run, EntryKind::StepDone);
    assert!(done.truncated, "the step records that it was cut");
    assert!(!done.is_error, "truncation is not a tool failure");
    assert!(is_truncated(&done.result));
    assert_eq!(done.result[TRUNCATED_KEY], json!(true));
    assert!(
        done.result["original_bytes"].as_u64().expect("a count") > 10_000_000,
        "the true size is reported, not the kept size"
    );
    assert_eq!(done.result["limit_bytes"], json!(limit));

    // The result honours the cap, and the whole entry stays small.
    let result_bytes = serde_json::to_vec(&done.result).expect("serialises").len();
    assert!(
        result_bytes <= limit,
        "result was {result_bytes} bytes, cap is {limit}"
    );
    let entry = &super::entries_of(&journal, run, EntryKind::StepDone)[0];
    let entry_bytes = serde_json::to_vec(&entry.payload)
        .expect("serialises")
        .len();
    assert!(
        entry_bytes <= limit + 1024,
        "journal entry was {entry_bytes} bytes"
    );

    // EDGE E5: the preview is whole characters, never half a code point.
    let preview = done.result["preview"].as_str().expect("a preview");
    assert!(!preview.is_empty());
    assert!(
        preview.contains('漢') || preview.contains('α'),
        "multi-byte content was kept"
    );
    assert!(!preview.contains('\u{FFFD}'), "no code point was split");

    // And the model saw the envelope, not a fragment pretending to be an answer.
    let last = llm.requests().pop().expect("a second turn happened");
    let tool_message = last
        .messages
        .iter()
        .find(|m| m.role() == "tool")
        .expect("tool message");
    match tool_message {
        Message::Tool { result, .. } => assert!(is_truncated(result)),
        other => panic!("expected a tool message, got {other:?}"),
    }
}
