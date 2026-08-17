//! The public types claim to be serde-serialisable and provider-neutral. This
//! is where that claim is checked, because a host stores every one of them.

use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use uuid::Uuid;

use crate::ids::{effect_id, RunId};
use crate::journal::{
    ApprovalRequested, CapBreached, Entry, EntryKind, ModelResponse, RunEnded, RunStarted,
    RunWaiting, StepDone, StepStarted,
};
use crate::message::{
    CallContext, ChatRequest, ChatResponse, Message, StopReason, TokenUsage, ToolCall, ToolSchema,
};
use crate::run::{
    ApprovalRequest, Decision, FailureReason, Resolution, RunInput, RunOutcome, RunStats, RunStatus,
};

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(value).expect("serialises");
    let decoded: T = serde_json::from_str(&encoded).expect("deserialises");
    assert_eq!(
        &decoded, value,
        "round trip changed the value; wire form was {encoded}"
    );
    decoded
}

fn sample_call() -> ToolCall {
    ToolCall {
        id: "c1".to_string(),
        name: "write_note".to_string(),
        arguments: json!({"body": "café 🙂", "n": 3}),
        context: Some(CallContext {
            run_id: RunId::from_uuid(Uuid::nil()),
            step_seq: 3,
            effect_id: effect_id(RunId::from_uuid(Uuid::nil()), 3),
            replay: true,
            dispatched_at: Utc::now(),
        }),
    }
}

#[test]
fn conversation_types_round_trip() {
    round_trip(&Message::system("be brief"));
    round_trip(&Message::user("hi"));
    round_trip(&Message::Assistant {
        text: Some("thinking".to_string()),
        tool_calls: vec![sample_call()],
    });
    round_trip(&Message::Assistant {
        text: None,
        tool_calls: vec![],
    });
    round_trip(&Message::Tool {
        call_id: "c1".to_string(),
        name: "write_note".to_string(),
        result: json!({"ok": true}),
        is_error: false,
    });

    round_trip(&ChatRequest {
        messages: vec![Message::user("hi")],
        tools: vec![ToolSchema {
            name: "write_note".to_string(),
            description: Some("writes".to_string()),
            parameters: json!({"type": "object"}),
        }],
        model: Some("some-model".to_string()),
        max_output_tokens: Some(512),
        temperature: Some(0.2),
    });

    for stop in [
        StopReason::EndTurn,
        StopReason::ToolUse,
        StopReason::MaxTokens,
        StopReason::StopSequence,
        StopReason::ContentFilter,
        StopReason::Other("provider_specific".to_string()),
    ] {
        round_trip(&ChatResponse {
            text: Some("x".to_string()),
            tool_calls: vec![sample_call()],
            stop_reason: stop,
            usage: TokenUsage {
                input_tokens: 7,
                output_tokens: 11,
            },
        });
    }

    // The role tag is what a journal reader and a provider adapter key off.
    let encoded = serde_json::to_value(Message::user("hi")).expect("serialises");
    assert_eq!(encoded, json!({"role": "user", "text": "hi"}));
}

#[test]
fn journal_payloads_round_trip() {
    let run = RunId::from_uuid(Uuid::nil());
    let now = Utc::now();

    round_trip(&RunStarted {
        input: RunInput::user("hi").with_system("be brief"),
    });
    round_trip(&ModelResponse {
        turn: 1,
        text: None,
        tool_calls: vec![sample_call()],
        stop_reason: StopReason::ToolUse,
        usage: TokenUsage::default(),
    });
    round_trip(&StepStarted {
        step_seq: 3,
        call_id: "c1".to_string(),
        tool: "write_note".to_string(),
        args: json!({"body": "漢字"}),
        args_hash: "sha256:00".to_string(),
        effect_id: effect_id(run, 3),
        attempt: 2,
    });
    round_trip(&StepDone {
        step_seq: 3,
        call_id: "c1".to_string(),
        tool: "write_note".to_string(),
        result: json!({"id": 1}),
        is_error: false,
        truncated: true,
    });
    round_trip(&ApprovalRequested {
        step_seq: 3,
        call_id: "c1".to_string(),
        tool: "write_note".to_string(),
        args: json!({}),
        args_hash: "sha256:00".to_string(),
        effect_id: effect_id(run, 3),
        requested_at: now,
        expires_at: Some(now + ChronoDuration::days(7)),
    });
    round_trip(&RunWaiting {
        step_seq: 3,
        wake_at: now,
    });
    round_trip(&CapBreached {
        reason: FailureReason::TokenBudgetExceeded {
            limit: 50_000,
            spent: 50_001,
        },
    });
    round_trip(&RunEnded {
        status: RunStatus::Failed,
        output: None,
        reason: Some(FailureReason::StepCapExceeded {
            limit: 12,
            taken: 12,
        }),
        steps: 12,
        usage: TokenUsage::default(),
    });

    // And the envelope itself, which is what a driver actually stores.
    let entry = Entry::new(3, EntryKind::StepStarted, &json!({"step_seq": 3})).expect("builds");
    let decoded = round_trip(&entry);
    assert_eq!(decoded.kind, EntryKind::StepStarted);
    let encoded = serde_json::to_value(&entry).expect("serialises");
    assert_eq!(
        encoded["kind"],
        json!("step_started"),
        "kind is a bare string column"
    );

    // A kind from a future version survives a read instead of failing to parse.
    let future: EntryKind = serde_json::from_value(json!("something_new")).expect("parses");
    assert_eq!(future, EntryKind::Other("something_new".to_string()));
}

#[test]
fn outcomes_and_resolutions_round_trip() {
    let run = RunId::from_uuid(Uuid::nil());
    let stats = RunStats {
        steps: 2,
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        },
        last_seq: 9,
    };

    round_trip(&RunOutcome::Done {
        run_id: run,
        output: Some("done".to_string()),
        stats,
    });
    round_trip(&RunOutcome::PendingApproval {
        run_id: run,
        request: Box::new(ApprovalRequest {
            step_seq: 3,
            tool: "write_note".to_string(),
            call: sample_call(),
            args_hash: "sha256:00".to_string(),
            effect_id: effect_id(run, 3),
            requested_at: Utc::now(),
            expires_at: None,
        }),
        stats,
    });
    round_trip(&RunOutcome::Waiting {
        run_id: run,
        wake_at: Utc::now(),
        stats,
    });
    round_trip(&RunOutcome::Failed {
        run_id: run,
        reason: FailureReason::StepCapExceeded {
            limit: 12,
            taken: 12,
        },
        stats,
    });
    round_trip(&RunOutcome::Skipped { run_id: run, stats });
    round_trip(&RunOutcome::Expired { run_id: run, stats });

    for resolution in [
        Resolution::Approve,
        Resolution::Skip,
        Resolution::Expire,
        Resolution::Timer,
    ] {
        round_trip(&resolution);
    }
    for decision in [Decision::Approve, Decision::Skip, Decision::Expire] {
        round_trip(&decision);
        assert_eq!(
            serde_json::to_value(decision).expect("serialises"),
            json!(decision.as_str())
        );
    }
    for status in [
        RunStatus::Done,
        RunStatus::Failed,
        RunStatus::PendingApproval,
        RunStatus::Waiting,
        RunStatus::Skipped,
        RunStatus::Expired,
    ] {
        round_trip(&status);
        assert_eq!(
            serde_json::to_value(status).expect("serialises"),
            json!(status.as_str()),
            "the status tag must match the database column"
        );
    }

    // The outcome's own tag is the same vocabulary.
    let encoded =
        serde_json::to_value(RunOutcome::Skipped { run_id: run, stats }).expect("serialises");
    assert_eq!(encoded["status"], json!("skipped"));
}

/// `RunId` is a newtype but must look exactly like a UUID on the wire, because a
/// host stores it in a `uuid` column.
#[test]
fn run_id_is_transparent_on_the_wire() {
    let run = RunId::new();
    let encoded = serde_json::to_value(run).expect("serialises");
    assert_eq!(encoded, json!(run.as_uuid()));
    assert_eq!(run.to_string(), run.as_uuid().hyphenated().to_string());
    assert_eq!(run.to_string().parse::<RunId>().expect("parses"), run);
}
