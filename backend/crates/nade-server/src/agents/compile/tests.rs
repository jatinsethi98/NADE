//! The compiler: the validation (pure) and one end-to-end pass over wiremock.

use std::sync::Arc;

use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::config::LlmConfig;
use crate::llm::ledger::SpendGuard;
use crate::test_support::test_db;

const SENTENCE: &str =
    "When a recruiter emails about a tech role, save the next steps as a note. Ask me first.";

fn emitted(overrides: Value) -> Value {
    let mut base = json!({
        "name": "Job Search Tracker",
        "when_span": "a recruiter emails about a tech role",
        "do_span": "save the next steps as a note",
        "trailing": "Ask me first.",
        "trigger_kind": "mail",
        "semantic": "The sender is a recruiter.",
        "from_domains": [],
        "subject_contains": [],
        "label_ids": ["INBOX"],
        "instruction": "Read the thread and extract every concrete next step.",
        "tools": ["read_thread", "write_note"],
        "output_kind": "note"
    });
    for (key, value) in overrides.as_object().unwrap() {
        base[key] = value.clone();
    }
    base
}

// ---------------------------------------------------------- validation --

#[test]
fn a_well_formed_answer_compiles() {
    let out = build(SENTENCE, &emitted(json!({}))).expect("compile");
    assert_eq!(out.name, "Job Search Tracker");
    assert_eq!(out.when_span, "a recruiter emails about a tech role");
    assert_eq!(out.do_span, "save the next steps as a note");
    assert_eq!(out.trailing.as_deref(), Some("Ask me first."));
    assert_eq!(out.allowed_tools, vec!["read_thread", "write_note"]);
}

#[test]
fn the_spec_has_exactly_the_keys_the_contract_names() {
    let out = build(SENTENCE, &emitted(json!({}))).unwrap();
    let spec = &out.spec;
    assert_eq!(spec["version"], json!(1));

    let trigger = spec["trigger"].as_object().unwrap();
    let mut trigger_keys: Vec<&String> = trigger.keys().collect();
    trigger_keys.sort();
    assert_eq!(trigger_keys, vec!["filters", "kind", "semantic"]);

    let filters = spec["trigger"]["filters"].as_object().unwrap();
    let mut filter_keys: Vec<&str> = filters.keys().map(String::as_str).collect();
    filter_keys.sort_unstable();
    // `API.md` §5.1, verbatim. A missing key is a decoder failure on the app
    // side; an extra one is a fixture failure in `validate.py`.
    assert_eq!(
        filter_keys,
        vec![
            "body_contains",
            "from_contains",
            "from_domains",
            "has_attachment",
            "label_ids",
            "newer_than_days",
            "subject_contains",
        ]
    );

    let output = spec["output"].as_object().unwrap();
    let mut output_keys: Vec<&String> = output.keys().collect();
    output_keys.sort();
    assert_eq!(output_keys, vec!["kind", "title_template"]);
}

#[test]
fn the_dev_cap_is_applied_by_the_compiler_and_not_left_to_the_model() {
    let out = build(SENTENCE, &emitted(json!({}))).unwrap();
    assert_eq!(out.spec["trigger"]["filters"]["newer_than_days"], json!(30));
}

#[test]
fn spec_tools_are_a_subset_of_allowed_tools() {
    // `API.md` §5.1's own invariant, and one `validate.py` checks on fixtures.
    let out = build(SENTENCE, &emitted(json!({}))).unwrap();
    let spec_tools: Vec<String> = out.spec["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    for tool in &spec_tools {
        assert!(out.allowed_tools.contains(tool));
    }
}

#[test]
fn a_tool_that_does_not_exist_is_dropped_rather_than_stored() {
    // A stored spec naming `http_fetch` would be served by `GET /agents/{id}`
    // and would fail `validate.py`'s enum on the way out.
    let out = build(
        SENTENCE,
        &emitted(json!({"tools": ["read_thread", "http_fetch", "send_email"]})),
    )
    .unwrap();
    assert_eq!(out.allowed_tools, vec!["read_thread"]);
}

#[test]
fn tools_are_sorted_and_deduplicated() {
    let out = build(
        SENTENCE,
        &emitted(json!({"tools": ["write_note", "read_thread", "write_note"]})),
    )
    .unwrap();
    assert_eq!(out.allowed_tools, vec!["read_thread", "write_note"]);
}

#[test]
fn an_agent_with_no_usable_tool_is_refused() {
    // EDGE: empty input. An agent that can do nothing is not an agent, and
    // storing it would produce a run that ends immediately with no explanation.
    for tools in [json!([]), json!(["http_fetch"]), json!("not an array")] {
        let err = build(SENTENCE, &emitted(json!({"tools": tools}))).expect_err("refuse");
        assert!(matches!(err, CompileError::Rejected(_)), "{err:?}");
    }
}

#[test]
fn a_paraphrased_span_is_refused_because_the_builder_cannot_underline_it() {
    // The screen renders "When {when_span}, {do_span}." with both underlined
    // inside the user's own sentence. A paraphrase underlines nothing and the
    // screen silently looks broken.
    let err = build(
        SENTENCE,
        &emitted(json!({"when_span": "someone from a company writes in"})),
    )
    .expect_err("refuse");
    assert!(matches!(err, CompileError::Rejected(_)), "{err:?}");
}

#[test]
fn a_span_the_model_capitalised_is_rescued_rather_than_rejected() {
    // Models routinely capitalise the first word of a clause they quote. The
    // sentence is still the user's, so the original casing is recovered.
    let out = build(
        SENTENCE,
        &emitted(json!({"when_span": "A recruiter emails about a tech role"})),
    )
    .expect("compile");
    assert_eq!(
        out.when_span, "a recruiter emails about a tech role",
        "the stored span must be the sentence's own casing, so it matches when underlined"
    );
    assert!(SENTENCE.contains(&out.when_span));
}

#[test]
fn a_missing_span_is_malformed_rather_than_silently_null() {
    // `validate.py`: with a spec, `when_span` and `do_span` are never null.
    for key in ["when_span", "do_span", "name", "instruction"] {
        let mut answer = emitted(json!({}));
        answer[key] = Value::Null;
        let err = build(SENTENCE, &answer).expect_err("refuse");
        assert!(matches!(err, CompileError::Malformed(_)), "{key}: {err:?}");
    }
}

#[test]
fn a_missing_trailing_clause_is_null_and_that_is_legal() {
    // Unlike the two spans: not every sentence has a closing clause, and
    // `agent_scheduled.json` is a fixture with `trailing: null` and a spec.
    for trailing in [json!(null), json!(""), json!("   ")] {
        let out = build(SENTENCE, &emitted(json!({"trailing": trailing}))).unwrap();
        assert!(out.trailing.is_none());
    }
}

#[test]
fn a_scheduled_trigger_is_refused_until_schedules_exist() {
    // `validate.py` requires `trigger.kind == "schedule"` to imply a non-null
    // `agents.schedule`. Deriving one is P7's job, so storing a schedule
    // trigger now would be a contract violation on every subsequent read.
    let err = build(SENTENCE, &emitted(json!({"trigger_kind": "schedule"}))).expect_err("refuse");
    assert!(matches!(err, CompileError::Rejected(_)), "{err:?}");
}

#[test]
fn an_unknown_trigger_or_output_kind_is_refused() {
    for (key, value) in [("trigger_kind", "telepathy"), ("output_kind", "send_email")] {
        let err = build(SENTENCE, &emitted(json!({key: value}))).expect_err("refuse");
        assert!(matches!(err, CompileError::Rejected(_)), "{key}: {err:?}");
    }
}

#[test]
fn label_ids_default_to_the_inbox_rather_than_to_everything() {
    let out = build(SENTENCE, &emitted(json!({"label_ids": []}))).unwrap();
    assert_eq!(
        out.spec["trigger"]["filters"]["label_ids"],
        json!(["INBOX"])
    );
}

#[test]
fn a_blank_semantic_becomes_null_rather_than_an_empty_string() {
    for semantic in [json!(null), json!(""), json!("  ")] {
        let out = build(SENTENCE, &emitted(json!({"semantic": semantic}))).unwrap();
        assert_eq!(out.spec["trigger"]["semantic"], Value::Null);
    }
}

#[test]
fn an_answer_that_is_not_an_object_is_malformed() {
    for answer in [json!("a string"), json!([1, 2]), json!(null)] {
        assert!(matches!(
            build(SENTENCE, &answer),
            Err(CompileError::Malformed(_))
        ));
    }
}

#[test]
fn a_very_long_name_is_capped_rather_than_stored_whole() {
    let out = build(SENTENCE, &emitted(json!({"name": "n".repeat(5_000)}))).unwrap();
    assert!(out.name.len() <= 80 + 64);
}

#[test]
fn a_unicode_sentence_compiles_and_its_spans_still_match() {
    // EDGE: unicode. `verbatim_span` slices by byte offset after a
    // case-insensitive find, which is only sound because the lowercase and the
    // original agree in length for the scripts we hit here.
    let sentence = "When \u{4f60}\u{597d} arrives, save a note.";
    let answer = emitted(json!({
        "when_span": "\u{4f60}\u{597d} arrives",
        "do_span": "save a note",
        "trailing": null
    }));
    let out = build(sentence, &answer).expect("compile");
    assert_eq!(out.when_span, "\u{4f60}\u{597d} arrives");
}

#[test]
fn the_forced_tool_schema_satisfies_what_strict_mode_demands() {
    // `strict: true` requires `additionalProperties: false` and a `required`
    // list naming every property, or the provider refuses the tool outright.
    let schema = emit_tool_schema();
    assert_eq!(schema["strict"], json!(true));
    let input = &schema["input_schema"];
    assert_eq!(input["additionalProperties"], json!(false));

    let required: Vec<&str> = input["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let properties = input["properties"].as_object().unwrap();
    for name in properties.keys() {
        assert!(
            required.contains(&name.as_str()),
            "{name} is not in `required`"
        );
    }
    assert_eq!(
        required.len(),
        properties.len(),
        "required and properties must agree"
    );
}

#[test]
fn the_system_prompt_tells_the_model_what_v1_cannot_do() {
    // C1/C2 reach into the compiler too: a model that believes it can send mail
    // writes specs that promise it.
    let lowered = SYSTEM.to_lowercase();
    assert!(lowered.contains("never send"), "{SYSTEM}");
    assert!(
        lowered.contains("verbatim"),
        "the spans must be quoted, not paraphrased"
    );
}

// ----------------------------------------------------------------- e2e --

fn tool_answer(input: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_1",
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "tool_use",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "emit_agent", "input": input}],
        "usage": {"input_tokens": 200, "output_tokens": 80}
    }))
}

async fn an_account(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("compile-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("account")
}

/// A guard with a ceiling nothing in these tests reaches.
fn guard(pool: &sqlx::PgPool, account: Uuid) -> SpendGuard {
    SpendGuard::new(pool.clone(), account, 1_000_000_000)
}

fn client_at(base: &str) -> Arc<Client> {
    let cfg = LlmConfig {
        api_base: base.to_owned(),
        ..crate::config::tests::sample_llm()
    };
    Arc::new(Client::new(&cfg).expect("client"))
}

#[tokio::test]
async fn a_sentence_compiles_end_to_end_and_the_call_is_billed() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(tool_answer(emitted(json!({}))))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_at(&server.uri());
    let out = compile(Some(&client), &db.pool, &guard(&db.pool, account), SENTENCE)
        .await
        .expect("compile");
    assert_eq!(out.name, "Job Search Tracker");

    let (purpose, spent): (String, i64) = sqlx::query_as(
        "select purpose, (cost_usd * 1000000000)::bigint from llm_calls where account_id = $1",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(purpose, "compile");
    // 200 input at 1000 nano + 80 output at 5000 nano.
    assert_eq!(spent, 200 * 1_000 + 80 * 5_000);
}

#[tokio::test]
async fn the_request_forces_the_tool_call() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(wiremock::matchers::body_partial_json(
            json!({"tool_choice": {"type": "tool", "name": "emit_agent"}}),
        ))
        .respond_with(tool_answer(emitted(json!({}))))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_at(&server.uri());
    compile(Some(&client), &db.pool, &guard(&db.pool, account), SENTENCE)
        .await
        .expect("compile");
}

#[tokio::test]
async fn an_answer_with_no_tool_call_is_malformed_not_a_panic() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "m", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "I would rather not"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .mount(&server)
        .await;

    let client = client_at(&server.uri());
    let err = compile(Some(&client), &db.pool, &guard(&db.pool, account), SENTENCE)
        .await
        .expect_err("refuse");
    assert!(matches!(err, CompileError::Malformed(_)), "{err:?}");
}

#[tokio::test]
async fn an_unreachable_provider_is_recorded_and_reported_as_unavailable() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&server)
        .await;

    let client = client_at(&server.uri());
    let err = compile(Some(&client), &db.pool, &guard(&db.pool, account), SENTENCE)
        .await
        .expect_err("must fail");
    assert!(matches!(err, CompileError::Unavailable), "{err:?}");

    // The failure is a value the caller stores, and it still cost something to
    // find out - so it is in the ledger.
    let rows: i64 = sqlx::query_scalar("select count(*) from llm_calls where account_id = $1")
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn a_server_with_no_model_configured_says_so_rather_than_failing_obscurely() {
    let db = test_db().await;
    let account = an_account(&db.pool).await;
    let err = compile(None, &db.pool, &guard(&db.pool, account), SENTENCE)
        .await
        .expect_err("must fail");
    assert!(matches!(err, CompileError::NotConfigured));
    // Every variant is stored in `agents.compile_error` verbatim, so it has to
    // read as an explanation to a person.
    assert!(err.to_string().contains("no AI model configured"), "{err}");
}

#[test]
fn every_compile_error_reads_as_a_sentence_a_person_could_act_on() {
    let errors = [
        CompileError::NotConfigured,
        CompileError::Unavailable,
        CompileError::Malformed("x".into()),
        CompileError::Rejected("y".into()),
    ];
    for err in errors {
        let text = err.to_string();
        assert!(
            text.ends_with('.') || text.ends_with(':') || text.contains(": "),
            "{text}"
        );
        assert!(text.chars().next().unwrap().is_uppercase(), "{text}");
        for leak in ["unwrap", "sqlx", "Traceback", "None)", "Err("] {
            assert!(!text.contains(leak), "{text} leaks an internal");
        }
    }
}

#[test]
fn a_unicode_sentence_cannot_panic_the_span_search() {
    // `to_lowercase` is not byte-length preserving, so the obvious
    // "lowercase, find, slice the original" spelling panics on real input.
    // These three cases were measured against that spelling: two panicked and
    // one silently returned a span one character short. All of them arrive
    // through a 4 000-character user string on `POST /agents`, and a panic
    // there becomes a 500 - which loses the sentence, the one thing API.md 5
    // says must never happen.
    let cases = [
        (
            "When \u{1E9E}IG news arrives, save a note.",
            "\u{00DF}ig news",
        ),
        ("\u{0130}x", "X"),
        ("\u{0130}\u{00C9} save this", "\u{00E9} save"),
    ];
    for (sentence, span) in cases {
        let answer = emitted(json!({"when_span": span, "do_span": span, "trailing": null}));
        // Must not panic. Either outcome is acceptable; a crash is not.
        let out = build(sentence, &answer);
        if let Ok(compiled) = out {
            // If it did match, the stored span must be a real slice of the
            // sentence - that is the whole contract the builder relies on.
            assert!(
                sentence.contains(&compiled.when_span),
                "{:?} is not a substring of {:?}",
                compiled.when_span,
                sentence
            );
        }
    }
}

#[test]
fn a_case_insensitive_match_returns_the_sentences_own_characters() {
    // The rescue path, in the ordinary ASCII case it exists for.
    let out = build(
        "When A Recruiter Emails about a tech role, save the next steps as a note.",
        &emitted(json!({
            "when_span": "a recruiter emails about a tech role",
            "do_span": "save the next steps as a note",
            "trailing": null
        })),
    )
    .expect("compile");
    assert_eq!(out.when_span, "A Recruiter Emails about a tech role");
}

#[test]
fn the_span_search_finds_nothing_in_an_empty_needle_or_haystack() {
    // EDGE: empty input.
    assert_eq!(find_case_insensitive("abc", ""), None);
    assert_eq!(find_case_insensitive("", "abc"), None);
    assert_eq!(find_case_insensitive("abc", "abc"), Some((0, 3)));
    assert_eq!(find_case_insensitive("xxabc", "ABC"), Some((2, 5)));
}
