//! `PgJournal` against a real PostgreSQL, including a real engine driving it.
//!
//! What is deliberately **not** here: replay correctness. The SDK proves
//! crash-at-every-protocol-point, cap boundaries, forgery rejection and
//! deterministic-effect-id collapsing against its own `MemoryJournal`, in about
//! a hundred tests. Re-proving them here would test the engine a second time.
//! These tests are about the things only a *Postgres* journal can get wrong:
//! the timestamp, the sequence conflict, the `jsonb` column, and the fact that
//! the engine can drive this driver at all.

use std::sync::Arc;

use chrono::{DateTime, SubsecRound, TimeZone, Utc};
use nade_agent_sdk::{
    args_hash,
    // `CountingTool` and not a local re-roll of it: it counts executions the
    // same way and additionally keys effects by `effect_id`, which is the
    // instrument this module's "deterministic ids collapse" claim wants.
    testing::{CountingTool, ScriptedLlm},
    ChatResponse,
    Engine,
    EngineConfig,
    Entry,
    EntryKind,
    Error,
    Journal,
    RunId,
    Tool,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::PgJournal;
use crate::test_support::test_db;

/// An `agent_runs` row for the journal's foreign key to point at, and its id as
/// a `RunId`.
async fn a_run(pool: &PgPool) -> RunId {
    let account: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("journal-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("account");
    let agent: Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, allowed_tools) \
         values ($1, 'Test Agent', 'when mail arrives, note it', $2) returning id",
    )
    .bind(account)
    .bind(vec!["read_thread".to_owned(), "write_note".to_owned()])
    .fetch_one(pool)
    .await
    .expect("agent");
    let run: Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, account_id, trigger_kind) \
         values ($1, $2, 'manual') returning id",
    )
    .bind(agent)
    .bind(account)
    .fetch_one(pool)
    .await
    .expect("run");
    RunId::from_uuid(run)
}

fn entry(seq: u32, kind: EntryKind, payload: Value, at: DateTime<Utc>) -> Entry {
    Entry {
        seq,
        kind,
        payload,
        created_at: at.trunc_subsecs(0),
    }
}

// --------------------------------------------------------- the timestamp --

#[tokio::test]
async fn the_stored_timestamp_is_the_engines_and_never_the_databases() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;

    // Years away from `now()`. The column carries `default now()`, so if the
    // insert ever stopped binding this value the row would come back stamped
    // today and this assertion would fail by five years.
    let engine_stamp = Utc.with_ymd_and_hms(2021, 3, 4, 5, 6, 7).unwrap();
    journal
        .append(
            run,
            entry(1, EntryKind::RunStarted, json!({"input": {}}), engine_stamp),
        )
        .await
        .expect("append");

    let loaded = journal.load(run).await.expect("load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded[0].created_at, engine_stamp,
        "run_journal.created_at must be the engine's stamp, byte for byte"
    );
    let drift = (Utc::now() - loaded[0].created_at).num_days().abs();
    assert!(
        drift > 365,
        "the database clock was substituted for the engine's"
    );
}

#[tokio::test]
async fn whole_second_stamps_survive_the_round_trip_exactly() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;

    // `API.md` 6.1: the engine stamps whole seconds so one date formatter
    // serves the whole wire. `timestamptz` has microsecond precision, so a
    // fractional part could only appear if something here added one.
    let at = Utc::now().trunc_subsecs(0);
    journal
        .append(run, entry(1, EntryKind::RunStarted, json!({}), at))
        .await
        .expect("append");
    let loaded = journal.load(run).await.expect("load");
    assert_eq!(loaded[0].created_at, at);
    assert_eq!(loaded[0].created_at.timestamp_subsec_nanos(), 0);
}

// -------------------------------------------------------------- ordering --

#[tokio::test]
async fn entries_load_in_sequence_order_however_they_were_written() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    // Written out of order on purpose: `load` promises `seq` order, and the
    // engine's replay validator refuses a journal with a gap or a jump, so a
    // driver that returned insertion order would corrupt every resumed run.
    for seq in [3_u32, 1, 2] {
        journal
            .append(
                run,
                entry(seq, EntryKind::ModelResponse, json!({"turn": seq}), at),
            )
            .await
            .expect("append");
    }
    let seqs: Vec<u32> = journal
        .load(run)
        .await
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[tokio::test]
async fn an_unknown_run_loads_as_empty_which_is_how_the_engine_spots_a_fresh_one() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    // Not an error: `Journal::load` documents an empty Vec for an unknown run,
    // and `Engine::run` uses exactly that to decide it is starting rather than
    // replaying.
    let loaded = journal.load(RunId::new()).await.expect("load");
    assert!(loaded.is_empty());
}

// -------------------------------------------------------- seq conflicts --

#[tokio::test]
async fn a_duplicate_seq_is_a_typed_conflict_and_not_a_generic_storage_error() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    journal
        .append(run, entry(1, EntryKind::RunStarted, json!({}), at))
        .await
        .unwrap();
    let again = journal
        .append(run, entry(1, EntryKind::ModelResponse, json!({}), at))
        .await;

    // The engine reads `SeqConflict` as "another writer got there first" and
    // any other journal error as "storage is broken". Collapsing the two would
    // make a lost race look like a corrupt database.
    match again {
        Err(Error::SeqConflict { run: r, seq }) => {
            assert_eq!(r, run);
            assert_eq!(seq, 1);
        }
        other => panic!("expected SeqConflict, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_append_leaves_nothing_behind() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    journal
        .append(
            run,
            entry(1, EntryKind::RunStarted, json!({"first": true}), at),
        )
        .await
        .unwrap();
    let _ = journal
        .append(
            run,
            entry(1, EntryKind::RunEnded, json!({"second": true}), at),
        )
        .await;

    let loaded = journal.load(run).await.unwrap();
    assert_eq!(loaded.len(), 1, "the rejected append must not have landed");
    assert_eq!(loaded[0].payload, json!({"first": true}));
}

#[tokio::test]
async fn a_seq_beyond_the_columns_range_is_refused_rather_than_wrapping_negative() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    // `Seq` is a u32 and the column is `integer`. An unchecked cast would store
    // a *negative* seq in the primary key and silently invert the ordering the
    // entire replay protocol depends on. Unreachable in practice - max_steps is
    // 12 - which is exactly why it is worth pinning.
    let result = journal
        .append(
            run,
            entry(u32::MAX, EntryKind::ModelResponse, json!({}), at),
        )
        .await;
    assert!(
        matches!(result, Err(Error::Journal { .. })),
        "got {result:?}"
    );
    assert!(journal.load(run).await.unwrap().is_empty());
}

// --------------------------------------------------------------- payloads --

#[tokio::test]
async fn every_payload_shape_the_engine_writes_survives_the_jsonb_round_trip() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    // `jsonb` is not a byte store: it re-sorts keys, drops duplicates and
    // stores numbers as `numeric`. The engine hashes tool arguments with
    // `args_hash` and its replay validator refuses a journal whose hash does
    // not match its arguments, so a payload that changed shape in transit would
    // make every resumed run unresumable.
    let payloads = [
        json!({"b": 1, "a": 2, "z": {"nested": [1, 2, 3]}}),
        json!({"float": 1.5, "negative": -0.25, "zero": 0.0}),
        json!({"big": 9_007_199_254_740_991_i64, "small": -9_007_199_254_740_991_i64}),
        json!({"unicode": "\u{4f60}\u{597d} \u{1F600} \u{0645}\u{0631}\u{062d}\u{0628}\u{0627}"}),
        json!({"empty_object": {}, "empty_array": [], "null": null, "bool": true}),
        json!({"deep": {"a": {"b": {"c": {"d": {"e": "bottom"}}}}}}),
        json!({"quotes": "she said \"hi\"", "backslash": "a\\b", "newline": "a\nb", "tab": "a\tb"}),
    ];

    for (index, payload) in payloads.iter().enumerate() {
        let seq = u32::try_from(index).unwrap() + 1;
        journal
            .append(run, entry(seq, EntryKind::StepStarted, payload.clone(), at))
            .await
            .expect("append");
    }

    let loaded = journal.load(run).await.expect("load");
    for (index, payload) in payloads.iter().enumerate() {
        assert_eq!(
            &loaded[index].payload, payload,
            "payload {index} changed in transit"
        );
        // The property that actually matters downstream: the hash the engine
        // recomputes on replay is the hash it wrote.
        assert_eq!(
            args_hash(&loaded[index].payload),
            args_hash(payload),
            "args_hash for payload {index} did not survive jsonb"
        );
    }
}

#[test]
fn serde_json_still_orders_object_keys_canonically() {
    // `args_hash` hashes the serialisation of a `Value`, and `Value` is a
    // `BTreeMap` only while `serde_json/preserve_order` is off. Any dependency
    // that turns that feature on has it unified across the whole workspace -
    // `backend/DECISIONS.md` D27's exact trap - and every `args_hash` in every
    // stored journal would silently stop matching.
    let value: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"a":2,"b":1}"#,
        "serde_json/preserve_order has been enabled somewhere in the workspace"
    );
}

#[tokio::test]
async fn a_nul_byte_in_a_payload_is_refused_by_the_column_which_is_why_tools_strip_it() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    // PostgreSQL `jsonb` rejects a NUL inside a string outright. Hostile mail
    // text reaches the journal through a tool result, so this is a reachable
    // input and not a theoretical one. The fix lives in the tools, which
    // sanitise their own results; this test pins the reason they have to.
    let hostile = format!("before{}after", '\u{0000}');
    let result = journal
        .append(
            run,
            entry(1, EntryKind::StepDone, json!({"result": hostile}), at),
        )
        .await;
    assert!(
        result.is_err(),
        "if jsonb ever starts accepting NUL, the sanitiser in agents::tools can be revisited"
    );
}

#[tokio::test]
async fn an_unrecognised_kind_round_trips_instead_of_failing_to_load() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    // A journal written by a newer build must still *load*; refusing it is the
    // engine's job, with a typed error, not this driver's job with a decode
    // failure. `Engine::cancel` also needs the journal to load.
    journal
        .append(
            run,
            entry(
                1,
                EntryKind::Other("from_the_future".to_owned()),
                json!({}),
                at,
            ),
        )
        .await
        .expect("append");
    let loaded = journal.load(run).await.expect("load");
    assert_eq!(
        loaded[0].kind,
        EntryKind::Other("from_the_future".to_owned())
    );
    assert_eq!(loaded[0].kind.as_str(), "from_the_future");
}

#[tokio::test]
async fn journals_are_scoped_to_their_run() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let one = a_run(&db.pool).await;
    let two = a_run(&db.pool).await;
    let at = Utc::now().trunc_subsecs(0);

    journal
        .append(
            one,
            entry(1, EntryKind::RunStarted, json!({"run": "one"}), at),
        )
        .await
        .unwrap();
    journal
        .append(
            two,
            entry(1, EntryKind::RunStarted, json!({"run": "two"}), at),
        )
        .await
        .unwrap();

    assert_eq!(
        journal.load(one).await.unwrap()[0].payload,
        json!({"run": "one"})
    );
    assert_eq!(
        journal.load(two).await.unwrap()[0].payload,
        json!({"run": "two"})
    );
}

// -------------------------------------------- the engine over this driver --

fn config() -> EngineConfig {
    EngineConfig {
        approval_ttl: None,
        ..EngineConfig::default()
    }
}

#[tokio::test]
async fn a_real_engine_drives_this_driver_end_to_end() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;

    let llm = ScriptedLlm::new(vec![
        ChatResponse::tool_call("c1", "count", json!({})),
        ChatResponse::text("done"),
    ]);
    let tool = Arc::new(CountingTool::new("count"));
    let engine = Engine::new(
        llm,
        vec![tool.clone() as Arc<dyn Tool>],
        journal.clone(),
        config(),
    )
    .expect("engine");

    let outcome = engine.run(run, "go").await.expect("run");
    assert!(outcome.is_terminal(), "got {outcome:?}");

    // The journal in PostgreSQL is the one the engine wrote, in the SDK's own
    // vocabulary (`API.md` 6.1) - not a translation of it.
    let kinds: Vec<String> = journal
        .load(run)
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str().to_owned())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "run_started",
            "model_response",
            "step_started",
            "step_done",
            "model_response",
            "run_ended",
        ]
    );
    assert_eq!(tool.executions(), 1);
}

#[tokio::test]
async fn a_duplicate_job_delivery_replays_from_postgres_rather_than_running_again() {
    let db = test_db().await;
    let journal = PgJournal::new(db.pool.clone());
    let run = a_run(&db.pool).await;
    let tool = Arc::new(CountingTool::new("count"));

    let first = Engine::new(
        ScriptedLlm::new(vec![
            ChatResponse::tool_call("c1", "count", json!({})),
            ChatResponse::text("done"),
        ]),
        vec![tool.clone() as Arc<dyn Tool>],
        journal.clone(),
        config(),
    )
    .unwrap();
    first.run(run, "go").await.expect("first run");
    let after_first = journal.load(run).await.unwrap().len();

    // A second worker picks the same job up. Its script is empty on purpose:
    // `ScriptedLlm` panics if asked for a turn it does not have, so anything
    // that consulted the model again would fail loudly rather than silently.
    let second = Engine::new(
        ScriptedLlm::new(vec![]),
        vec![tool.clone() as Arc<dyn Tool>],
        journal.clone(),
        config(),
    )
    .unwrap();
    let outcome = second.run(run, "go").await.expect("replay");

    assert!(outcome.is_terminal());
    assert_eq!(
        journal.load(run).await.unwrap().len(),
        after_first,
        "a replay must append nothing to a finished run"
    );
    assert_eq!(tool.executions(), 1, "the tool must not run a second time");
}

#[tokio::test]
async fn the_journal_survives_being_reopened_by_a_different_driver_instance() {
    let db = test_db().await;
    let run = a_run(&db.pool).await;

    // The realistic crash shape: the process dies, a new one starts, and a new
    // `PgJournal` over a new pool loads what the old one wrote.
    {
        let journal = PgJournal::new(db.pool.clone());
        let engine = Engine::new(
            ScriptedLlm::new(vec![ChatResponse::text("only turn")]),
            Vec::<Arc<dyn Tool>>::new(),
            journal,
            config(),
        )
        .unwrap();
        engine.run(run, "go").await.unwrap();
    }

    let reopened = PgJournal::new(db.pool.clone());
    let loaded = reopened.load(run).await.unwrap();
    assert_eq!(loaded.first().map(|e| e.kind.as_str()), Some("run_started"));
    assert_eq!(loaded.last().map(|e| e.kind.as_str()), Some("run_ended"));
    // Contiguous from 1, which is what the engine's replay validator demands.
    for (index, entry) in loaded.iter().enumerate() {
        assert_eq!(entry.seq, u32::try_from(index).unwrap() + 1);
    }
}
