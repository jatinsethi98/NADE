//! The adapter, in two halves: the translation (pure) and the HTTP (wiremock).
//!
//! The translation tests are the important ones. Each of them corresponds to a
//! way the SDK's conversation model and Anthropic's differ, and every one of
//! those differences is a `400` — or worse, a silent degradation — if it is
//! missed.

use nade_agent_sdk::{ChatRequest, Llm, Message, StopReason, ToolCall, ToolSchema};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::config::LlmConfig;
use crate::test_support::test_db;

// ------------------------------------------------------------- helpers --

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        messages,
        ..ChatRequest::default()
    }
}

fn wire(messages: Vec<Message>) -> WireRequest {
    to_wire(&request(messages), "test-model", 1024).expect("translation")
}

fn blocks(message: &WireMessage) -> &[Value] {
    &message.content
}

fn kinds(message: &WireMessage) -> Vec<&str> {
    message
        .content
        .iter()
        .filter_map(|b| b.get("type").and_then(Value::as_str))
        .collect()
}

// --------------------------------------------------------- translation --

#[test]
fn the_system_message_is_hoisted_out_of_the_conversation() {
    // The SDK carries the system prompt as a `Message::System` and prepends it
    // to every turn; Anthropic has no `system` role and rejects one.
    let out = wire(vec![Message::system("be helpful"), Message::user("hi")]);
    assert_eq!(out.system.as_deref(), Some("be helpful"));
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "user");
}

#[test]
fn several_system_messages_join_in_order() {
    let out = wire(vec![
        Message::system("first"),
        Message::system("second"),
        Message::user("hi"),
    ]);
    assert_eq!(out.system.as_deref(), Some("first\n\nsecond"));
}

#[test]
fn a_blank_system_message_does_not_produce_an_empty_system_field() {
    // EDGE: empty input. An empty `system` string is rejected by the provider,
    // and `Some("")` is the spelling that slips past a `None` check.
    let out = wire(vec![Message::system("   "), Message::user("hi")]);
    assert!(out.system.is_none());
}

#[test]
fn a_conversation_that_opens_on_an_assistant_turn_is_refused() {
    // Anthropic requires the first message to be `user`. Refusing here names
    // the cause; letting it through produces a 400 far from it.
    let err =
        to_wire(&request(vec![Message::assistant("uninvited")]), "m", 16).expect_err("must refuse");
    assert!(matches!(err, Error::Translation(_)), "{err:?}");
    assert!(err.to_string().contains("user turn first"), "{err}");
}

#[test]
fn an_empty_conversation_is_refused() {
    // EDGE: empty input.
    let err = to_wire(&request(vec![]), "m", 16).expect_err("must refuse");
    assert!(matches!(err, Error::Translation(_)));
}

#[test]
fn a_system_only_conversation_is_refused_rather_than_sent_with_no_messages() {
    let err = to_wire(&request(vec![Message::system("alone")]), "m", 16).expect_err("must refuse");
    assert!(matches!(err, Error::Translation(_)));
}

#[test]
fn consecutive_tool_results_coalesce_into_one_user_message() {
    // The rule that is invisible when it is wrong: sending one message per
    // result *succeeds* against the API and quietly teaches the model that
    // parallel tool calls get split up, so it stops making them.
    let out = wire(vec![
        Message::user("go"),
        Message::Assistant {
            text: None,
            tool_calls: vec![
                ToolCall::new("a", "one", json!({})),
                ToolCall::new("b", "two", json!({})),
            ],
        },
        Message::Tool {
            call_id: "a".into(),
            name: "one".into(),
            result: json!({"n": 1}),
            is_error: false,
        },
        Message::Tool {
            call_id: "b".into(),
            name: "two".into(),
            result: json!({"n": 2}),
            is_error: false,
        },
    ]);

    assert_eq!(
        out.messages.len(),
        3,
        "user, assistant, and ONE user of results"
    );
    let results = &out.messages[2];
    assert_eq!(results.role, "user");
    assert_eq!(kinds(results), vec!["tool_result", "tool_result"]);
    assert_eq!(blocks(results)[0]["tool_use_id"], "a");
    assert_eq!(blocks(results)[1]["tool_use_id"], "b");
}

#[test]
fn tool_results_from_separate_turns_do_not_merge_across_the_assistant_turn() {
    let out = wire(vec![
        Message::user("go"),
        Message::Assistant {
            text: None,
            tool_calls: vec![ToolCall::new("a", "one", json!({}))],
        },
        Message::Tool {
            call_id: "a".into(),
            name: "one".into(),
            result: json!(1),
            is_error: false,
        },
        Message::Assistant {
            text: None,
            tool_calls: vec![ToolCall::new("b", "two", json!({}))],
        },
        Message::Tool {
            call_id: "b".into(),
            name: "two".into(),
            result: json!(2),
            is_error: false,
        },
    ]);
    let roles: Vec<&str> = out.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "user", "assistant", "user"]
    );
    assert_eq!(kinds(&out.messages[2]), vec!["tool_result"]);
    assert_eq!(kinds(&out.messages[4]), vec!["tool_result"]);
}

#[test]
fn a_tool_result_is_always_a_string_on_the_wire() {
    // The SDK's `result` is an arbitrary `Value`; Anthropic requires a string
    // or a block list. Sending the raw value is a 400 on the first tool result.
    let out = wire(vec![
        Message::user("go"),
        Message::Assistant {
            text: None,
            tool_calls: vec![ToolCall::new("a", "t", json!({}))],
        },
        Message::Tool {
            call_id: "a".into(),
            name: "t".into(),
            result: json!({"rows": [1, 2, 3]}),
            is_error: false,
        },
    ]);
    let content = &blocks(&out.messages[2])[0]["content"];
    assert!(
        content.is_string(),
        "tool_result.content must be a string, got {content}"
    );
    assert_eq!(content.as_str().unwrap(), r#"{"rows":[1,2,3]}"#);
}

#[test]
fn a_string_result_is_shown_to_the_model_unquoted() {
    let out = wire(vec![
        Message::user("go"),
        Message::Assistant {
            text: None,
            tool_calls: vec![ToolCall::new("a", "t", json!({}))],
        },
        Message::Tool {
            call_id: "a".into(),
            name: "t".into(),
            result: json!("plain text"),
            is_error: false,
        },
    ]);
    // `"plain text"` and not `"\"plain text\""` - a tool that returns prose
    // should not have the model reading JSON quoting.
    assert_eq!(blocks(&out.messages[2])[0]["content"], json!("plain text"));
}

#[test]
fn an_errored_tool_result_carries_its_flag() {
    let out = wire(vec![
        Message::user("go"),
        Message::Assistant {
            text: None,
            tool_calls: vec![ToolCall::new("a", "t", json!({}))],
        },
        Message::Tool {
            call_id: "a".into(),
            name: "t".into(),
            result: json!({"error": "nope"}),
            is_error: true,
        },
    ]);
    assert_eq!(blocks(&out.messages[2])[0]["is_error"], json!(true));
}

#[test]
fn empty_assistant_text_is_dropped_in_both_of_its_spellings() {
    // `None` is the obvious one. `Some("")` is the one that slips through and
    // produces an empty text block, which the provider rejects.
    for text in [None, Some(String::new())] {
        let out = wire(vec![
            Message::user("go"),
            Message::Assistant {
                text,
                tool_calls: vec![ToolCall::new("a", "t", json!({}))],
            },
        ]);
        assert_eq!(kinds(&out.messages[1]), vec!["tool_use"]);
    }
}

#[test]
fn an_assistant_turn_with_neither_prose_nor_calls_is_skipped_entirely() {
    // It has no legal representation - empty content is rejected - and dropping
    // it is safe precisely because it had no tool calls, so nothing later
    // refers back to it.
    let out = wire(vec![
        Message::user("go"),
        Message::Assistant {
            text: None,
            tool_calls: vec![],
        },
        Message::user("still here"),
    ]);
    let roles: Vec<&str> = out.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec!["user", "user"]);
}

#[test]
fn a_tool_call_with_non_object_arguments_is_refused_rather_than_quietly_reshaped() {
    // The SDK passes a scalar through on purpose ("so the tool can reject it"),
    // but Anthropic requires an object. Substituting `{}` would replay a
    // *different* call than the journal records - exactly the drift the whole
    // journal protocol exists to prevent - so this refuses instead.
    for bad in [json!("a string"), json!([1, 2]), json!(7), json!(null)] {
        let err = to_wire(
            &request(vec![
                Message::user("go"),
                Message::Assistant {
                    text: None,
                    tool_calls: vec![ToolCall::new("a", "t", bad.clone())],
                },
            ]),
            "m",
            16,
        )
        .expect_err("must refuse");
        assert!(matches!(err, Error::Translation(_)), "{bad}: {err:?}");
    }
}

#[test]
fn max_tokens_is_always_sent_because_the_provider_requires_it() {
    let out = wire(vec![Message::user("hi")]);
    assert_eq!(
        out.max_tokens, 1024,
        "the fallback must be used when the SDK sends None"
    );

    let mut req = request(vec![Message::user("hi")]);
    req.max_output_tokens = Some(77);
    assert_eq!(to_wire(&req, "m", 1024).unwrap().max_tokens, 77);
}

#[test]
fn the_model_falls_back_only_when_the_request_does_not_name_one() {
    let mut req = request(vec![Message::user("hi")]);
    assert_eq!(to_wire(&req, "fallback", 16).unwrap().model, "fallback");
    req.model = Some("explicit".into());
    assert_eq!(to_wire(&req, "fallback", 16).unwrap().model, "explicit");
}

#[test]
fn tool_schemas_are_renamed_to_the_providers_field() {
    let mut req = request(vec![Message::user("hi")]);
    req.tools = vec![ToolSchema {
        name: "read_thread".into(),
        description: Some("read one thread".into()),
        parameters: json!({"type": "object", "properties": {}}),
    }];
    let out = to_wire(&req, "m", 16).unwrap();
    assert_eq!(out.tools.len(), 1);
    assert_eq!(out.tools[0]["name"], "read_thread");
    assert_eq!(out.tools[0]["description"], "read one thread");
    // `parameters` in the SDK, `input_schema` on the wire.
    assert_eq!(out.tools[0]["input_schema"]["type"], "object");
}

#[test]
fn unicode_survives_translation() {
    // EDGE: unicode.
    let text = "\u{4f60}\u{597d} \u{1F600} \u{0645}\u{0631}\u{062d}\u{0628}\u{0627}";
    let out = wire(vec![Message::user(text)]);
    assert_eq!(blocks(&out.messages[0])[0]["text"], json!(text));
}

#[test]
fn the_body_serialises_without_the_fields_the_provider_rejects_as_null() {
    let out = wire(vec![Message::user("hi")]);
    let body = serde_json::to_value(&out).unwrap();
    assert!(
        body.get("system").is_none(),
        "an absent system must be omitted, not null"
    );
    assert!(
        body.get("tools").is_none(),
        "an empty tool list must be omitted"
    );
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("temperature").is_none());
}

// ----------------------------------------------------- response parsing --

fn parse(body: Value) -> (ChatResponse, Tokens, String) {
    let wire: WireResponse = serde_json::from_value(body).expect("wire response");
    from_wire(wire).expect("from_wire")
}

#[test]
fn text_and_tool_calls_come_back_off_the_same_turn() {
    let (chat, _, model) = parse(json!({
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "tool_use",
        "content": [
            {"type": "text", "text": "let me look"},
            {"type": "tool_use", "id": "toolu_1", "name": "read_thread", "input": {"thread_id": "t1"}}
        ],
        "usage": {"input_tokens": 10, "output_tokens": 3}
    }));
    assert_eq!(chat.text.as_deref(), Some("let me look"));
    assert_eq!(chat.tool_calls.len(), 1);
    assert_eq!(chat.tool_calls[0].id, "toolu_1");
    assert_eq!(chat.tool_calls[0].name, "read_thread");
    assert_eq!(chat.tool_calls[0].arguments, json!({"thread_id": "t1"}));
    assert_eq!(chat.stop_reason, StopReason::ToolUse);
    assert_eq!(model, "claude-haiku-4-5-20251001");
}

#[test]
fn every_stop_reason_maps_and_the_unknown_ones_do_not_vanish() {
    let cases = [
        (json!("end_turn"), StopReason::EndTurn),
        (json!("tool_use"), StopReason::ToolUse),
        (json!("max_tokens"), StopReason::MaxTokens),
        (json!("stop_sequence"), StopReason::StopSequence),
        // Anything the provider ships later must survive as data rather than
        // being guessed at or silently dropped.
        (json!("refusal"), StopReason::Other("refusal".into())),
        (json!("pause_turn"), StopReason::Other("pause_turn".into())),
        (json!(null), StopReason::EndTurn),
    ];
    for (raw, expected) in cases {
        let (chat, _, _) = parse(json!({
            "stop_reason": raw, "content": [{"type": "text", "text": "x"}], "usage": {}
        }));
        assert_eq!(chat.stop_reason, expected, "{raw}");
    }
}

#[test]
fn cache_counters_are_kept_for_the_ledger_and_folded_into_the_budget() {
    let (chat, tokens, _) = parse(json!({
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "x"}],
        "usage": {
            "input_tokens": 10, "output_tokens": 2,
            "cache_creation_input_tokens": 100, "cache_read_input_tokens": 1000
        }
    }));
    // The full breakdown survives for pricing...
    assert_eq!(tokens.cache_write, 100);
    assert_eq!(tokens.cache_read, 1000);
    // ...while the engine's two-counter budget sees every input token that was
    // actually processed.
    assert_eq!(chat.usage.input_tokens, 1110);
    assert_eq!(chat.usage.output_tokens, 2);
}

#[test]
fn unknown_content_blocks_are_ignored_rather_than_fatal() {
    // A thinking block, or whatever ships next, must not break every run.
    let (chat, _, _) = parse(json!({
        "stop_reason": "end_turn",
        "content": [
            {"type": "thinking", "thinking": "..."},
            {"type": "text", "text": "answer"},
            {"type": "something_new", "data": 1}
        ],
        "usage": {}
    }));
    assert_eq!(chat.text.as_deref(), Some("answer"));
}

#[test]
fn a_turn_with_no_prose_reports_none_rather_than_an_empty_string() {
    let (chat, _, _) = parse(json!({
        "stop_reason": "tool_use",
        "content": [{"type": "tool_use", "id": "t", "name": "x", "input": {}}],
        "usage": {}
    }));
    assert!(chat.text.is_none());
}

#[test]
fn a_tool_use_block_with_no_name_is_malformed_rather_than_a_call_to_nothing() {
    let wire: WireResponse = serde_json::from_value(json!({
        "stop_reason": "tool_use",
        "content": [{"type": "tool_use", "id": "t", "input": {}}],
        "usage": {}
    }))
    .unwrap();
    assert!(matches!(from_wire(wire), Err(Error::Malformed(_))));
}

#[test]
fn a_tool_use_block_with_no_input_becomes_an_empty_object_not_null() {
    let (chat, _, _) = parse(json!({
        "stop_reason": "tool_use",
        "content": [{"type": "tool_use", "id": "t", "name": "x"}],
        "usage": {}
    }));
    assert_eq!(chat.tool_calls[0].arguments, json!({}));
}

// --------------------------------------------------------------- HTTP --

fn config_at(base: &str) -> LlmConfig {
    LlmConfig {
        api_base: base.to_owned(),
        ..crate::config::tests::sample_llm()
    }
}

fn answer(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_1",
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 12, "output_tokens": 4}
    }))
}

async fn adapter_over(server: &MockServer, pool: &sqlx::PgPool, account: Uuid) -> Adapter {
    let client = Arc::new(Client::new(&config_at(&server.uri())).expect("client"));
    let guard = SpendGuard::new(pool.clone(), account, 1_000_000_000);
    Adapter::new(
        client,
        pool.clone(),
        guard,
        Purpose::Run,
        "claude-haiku-4-5",
    )
}

async fn an_account(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("llm-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("account")
}

#[tokio::test]
async fn a_successful_call_answers_and_lands_in_the_ledger() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(answer("hello"))
        .expect(1)
        .mount(&server)
        .await;

    let adapter = adapter_over(&server, &db.pool, account).await;
    let out = adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect("chat");
    assert_eq!(out.text.as_deref(), Some("hello"));

    let (model, tokens_in, tokens_out, ok): (String, i32, i32, bool) = sqlx::query_as(
        "select model, tokens_in, tokens_out, ok from llm_calls where account_id = $1",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    // The model the provider says answered, not the alias we asked with.
    assert_eq!(model, "claude-haiku-4-5-20251001");
    assert_eq!((tokens_in, tokens_out, ok), (12, 4, true));
    // 12 input at 1000 nano + 4 output at 5000 nano.
    assert_eq!(
        crate::llm::ledger::spent_today_nano(&db.pool, account)
            .await
            .unwrap(),
        12 * 1_000 + 4 * 5_000
    );
}

#[tokio::test]
async fn a_permanent_status_is_not_retried() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    // `expect(1)` is the assertion: a 400 will never answer differently, and
    // retrying it burns the job's attempts on a certainty.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = config_at(&server.uri());
    cfg.max_attempts = 5;
    let client = Arc::new(Client::new(&cfg).unwrap());
    let guard = SpendGuard::new(db.pool.clone(), account, 1_000_000_000);
    let adapter = Adapter::new(
        client,
        db.pool.clone(),
        guard,
        Purpose::Run,
        "claude-haiku-4-5",
    );

    let err = adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("rejected the request"), "{err}");
}

#[tokio::test]
async fn a_permanent_error_surfaces_the_providers_own_message() {
    let cfg = config_at("http://127.0.0.1:1");
    let _ = Client::new(&cfg).expect("client builds");
    // The parsing half, without a server: the provider's `error.message` is
    // what a human needs, not the raw envelope.
    let body = json!({"type": "error", "error": {"message": "model not found"}});
    let parsed: WireError = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.error.message, "model not found");
}

#[tokio::test]
async fn a_retryable_status_is_retried_and_then_succeeds() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;

    // wiremock matches most-recently-mounted first for equal specificity, so
    // mount the failure with an explicit call budget and let the success take
    // the rest.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_string("overloaded"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(answer("second time lucky"))
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = config_at(&server.uri());
    cfg.max_attempts = 3;
    let client = Arc::new(Client::new(&cfg).unwrap());
    let guard = SpendGuard::new(db.pool.clone(), account, 1_000_000_000);
    let adapter = Adapter::new(
        client,
        db.pool.clone(),
        guard,
        Purpose::Run,
        "claude-haiku-4-5",
    );

    let out = adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect("chat");
    assert_eq!(out.text.as_deref(), Some("second time lucky"));
}

#[tokio::test]
async fn exhausted_retries_report_how_many_were_made() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(2)
        .mount(&server)
        .await;

    let mut cfg = config_at(&server.uri());
    cfg.max_attempts = 2;
    let client = Arc::new(Client::new(&cfg).unwrap());
    let guard = SpendGuard::new(db.pool.clone(), account, 1_000_000_000);
    let adapter = Adapter::new(
        client,
        db.pool.clone(),
        guard,
        Purpose::Run,
        "claude-haiku-4-5",
    );

    let err = adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("2 attempts"), "{err}");
}

#[tokio::test]
async fn a_failed_call_still_writes_a_ledger_row() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_string("nope"))
        .mount(&server)
        .await;

    let adapter = adapter_over(&server, &db.pool, account).await;
    let _ = adapter.chat(request(vec![Message::user("hi")])).await;

    // A burst of failures must be visible in the ledger rather than absent from
    // it - an invisible failure is how a ceiling stops binding.
    let rows: i64 =
        sqlx::query_scalar("select count(*) from llm_calls where account_id = $1 and not ok")
            .bind(account)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn a_body_that_does_not_parse_is_recorded_and_reported() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&server)
        .await;

    let adapter = adapter_over(&server, &db.pool, account).await;
    let err = adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("could not be parsed"), "{err}");
    let rows: i64 = sqlx::query_scalar("select count(*) from llm_calls where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "a malformed answer still cost money");
}

#[tokio::test]
async fn the_ceiling_stops_the_call_before_it_is_made() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    // No mock is mounted: any request at all would 404 and fail differently.
    // The point is that the adapter never reaches the network.
    let client = Arc::new(Client::new(&config_at(&server.uri())).unwrap());
    // Ceiling zero, ledger empty: `spent (0) >= ceiling (0)`.
    let guard = SpendGuard::new(db.pool.clone(), account, 0);
    let adapter = Adapter::new(
        client,
        db.pool.clone(),
        guard.clone(),
        Purpose::Run,
        "claude-haiku-4-5",
    );

    let err = adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect_err("must refuse");
    assert!(err.to_string().contains("spend ceiling"), "{err}");
    assert!(
        guard.tripped(),
        "the handler reads this flag to choose Engine::cancel over a job retry"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "no request may be sent"
    );
}

#[tokio::test]
async fn the_guard_the_adapter_exposes_is_the_same_one_it_trips() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    let client = Arc::new(Client::new(&config_at(&server.uri())).unwrap());
    let guard = SpendGuard::new(db.pool.clone(), account, 0);
    let adapter = Adapter::new(client, db.pool.clone(), guard, Purpose::Run, "m");

    // The handler clones this *before* moving the adapter into `Engine::new`,
    // which takes its `Llm` by value. If these were separate flags the breach
    // would be invisible and the job would retry - and re-spend.
    let observed = adapter.guard();
    let _ = adapter.chat(request(vec![Message::user("hi")])).await;
    assert!(observed.tripped());
}

#[tokio::test]
async fn the_request_carries_the_headers_the_api_requires() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(wiremock::matchers::header(
            "x-api-key",
            "test-key-not-a-real-one",
        ))
        .and(wiremock::matchers::header("anthropic-version", API_VERSION))
        .respond_with(answer("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let adapter = adapter_over(&server, &db.pool, account).await;
    adapter
        .chat(request(vec![Message::user("hi")]))
        .await
        .expect("chat");
}

#[test]
fn a_client_cannot_be_built_without_a_key() {
    let mut cfg = config_at("http://127.0.0.1:1");
    cfg.api_key = None;
    assert!(matches!(Client::new(&cfg), Err(Error::NotConfigured)));
}

#[test]
#[should_panic(expected = "pointed at the real API")]
fn a_test_cannot_accidentally_build_a_client_aimed_at_the_real_api() {
    // `backend/justfile` sets `dotenv-load := true` and `backend/.env` holds a
    // live key, so without this guard a test that forgot `set_llm_base` would
    // bill real money - on whichever machine happens to have a key, and nowhere
    // else, which is the worst possible way to find out.
    let _ = Client::new(&config_at(crate::llm::DEFAULT_API_BASE));
}

#[test]
fn only_the_statuses_worth_retrying_are_retried() {
    for status in [429, 500, 502, 503, 529] {
        assert!(is_retryable(status), "{status} should be retried");
    }
    for status in [400, 401, 403, 404, 413, 422] {
        assert!(!is_retryable(status), "{status} must not be retried");
    }
}
