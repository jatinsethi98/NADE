//! The run job, end to end, over a real database and a fake provider.

use nade_agent_sdk::{Journal, RunId};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::config::Env;
use crate::jobs::{Job, JobContext, Queue};
use crate::test_support::{test_app, TestApp};

// ------------------------------------------------------------- helpers --

struct World {
    app: TestApp,
    account: Uuid,
    agent: Uuid,
    run: Uuid,
    server: MockServer,
}

async fn world(allowed: &[&str], approval_required: bool) -> World {
    let server = MockServer::start().await;
    let mut app = test_app(Env::Dev).await;
    app.set_llm_base(&server.uri());

    let account: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("run-{}@example.com", Uuid::new_v4()))
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    let agent: Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, spec, allowed_tools, \
                             approval_required, status) \
         values ($1, 'Tester', 'when mail arrives, note it', $2, $3, $4, 'published') \
         returning id",
    )
    .bind(account)
    .bind(json!({"instruction": "Take a note about the mailbox."}))
    .bind(allowed.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    .bind(approval_required)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let run: Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, account_id, trigger_kind) \
         values ($1, $2, 'manual') returning id",
    )
    .bind(agent)
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();

    World {
        app,
        account,
        agent,
        run,
        server,
    }
}

fn text_turn(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 100, "output_tokens": 20}
    }))
}

fn tool_turn(name: &str, input: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "tool_use",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": name, "input": input}],
        "usage": {"input_tokens": 100, "output_tokens": 20}
    }))
}

/// Answer with `first` once, then `rest` for everything after.
async fn script(server: &MockServer, first: ResponseTemplate, rest: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(first)
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(rest)
        .mount(server)
        .await;
}

fn job_for(run: Uuid) -> Job {
    Job {
        id: 1,
        kind: KIND.to_owned(),
        payload: json!({"run_id": run}),
        attempts: 0,
    }
}

async fn run_job(w: &World) -> anyhow::Result<()> {
    let handler = RunAgentHandler {
        state: w.app.state.clone(),
    };
    let ctx = JobContext {
        queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
    };
    handler.handle(job_for(w.run), ctx).await
}

async fn status_of(w: &World) -> String {
    sqlx::query_scalar("select status from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap()
}

async fn journal_kinds(w: &World) -> Vec<String> {
    PgJournal::new(w.app.db.pool.clone())
        .load(RunId::from_uuid(w.run))
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str().to_owned())
        .collect()
}

// ------------------------------------------------------------ the loop --

#[tokio::test]
async fn a_run_that_answers_immediately_finishes_and_is_summarised() {
    let w = world(&["read_thread"], false).await;
    script(
        &w.server,
        text_turn("I looked and found nothing."),
        text_turn("x"),
    )
    .await;

    run_job(&w).await.expect("job");

    assert_eq!(status_of(&w).await, "done");
    let (summary, tokens): (Option<String>, i32) =
        sqlx::query_as("select summary, tokens_spent from agent_runs where id = $1")
            .bind(w.run)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(summary.as_deref(), Some("I looked and found nothing."));
    assert_eq!(tokens, 120);
    assert_eq!(
        journal_kinds(&w).await,
        vec!["run_started", "model_response", "run_ended"]
    );
}

#[tokio::test]
async fn a_run_that_calls_a_tool_writes_the_effect_and_journals_the_step() {
    let w = world(&["write_note"], false).await;
    script(
        &w.server,
        tool_turn(
            "write_note",
            json!({"title": "Findings", "body_md": "all quiet"}),
        ),
        text_turn("Saved a note."),
    )
    .await;

    run_job(&w).await.expect("job");

    assert_eq!(status_of(&w).await, "done");
    assert_eq!(
        journal_kinds(&w).await,
        vec![
            "run_started",
            "model_response",
            "step_started",
            "step_done",
            "model_response",
            "run_ended"
        ],
        "the journal must be in the SDK's own vocabulary (API.md 6.1)"
    );

    let (id, title, run_id): (Uuid, String, Option<Uuid>) =
        sqlx::query_as("select id, title, run_id from notes where account_id = $1")
            .bind(w.account)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(title, "Findings");
    assert_eq!(run_id, Some(w.run));
    // API.md 6.2: an agent-written note carries a v5 uuid.
    assert_eq!(id.get_version_num(), 5);
}

#[tokio::test]
async fn a_gated_tool_parks_the_run_and_records_everything_p5_will_need() {
    let w = world(&["write_note"], true).await;
    script(
        &w.server,
        tool_turn(
            "write_note",
            json!({"title": "Findings", "body_md": "all quiet"}),
        ),
        text_turn("unused"),
    )
    .await;

    run_job(&w).await.expect("job");

    assert_eq!(status_of(&w).await, "pending_approval");
    // Nothing was written: the engine executes nothing behind a gate.
    let notes: i64 = sqlx::query_scalar("select count(*) from notes where account_id = $1")
        .bind(w.account)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 0);

    let pending: Value = sqlx::query_scalar("select pending_action from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    // The **whole** ApprovalRequest. P5's approve transaction needs `step_seq`
    // to build `Resolution::Approve`, and the effect id to publish on the card
    // before the effect exists.
    assert_eq!(pending["tool"], json!("write_note"));
    assert!(pending["step_seq"].is_number(), "{pending}");
    assert!(pending["effect_id"].is_string(), "{pending}");
    assert!(pending["args_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    assert_eq!(
        journal_kinds(&w).await.last().map(String::as_str),
        Some("approval_requested")
    );
}

#[tokio::test]
async fn a_nade_journal_never_records_an_approval_expiry() {
    // `EngineConfig::approval_ttl` must be None: the host's sweep and the
    // approve transaction's 410 own expiry (API.md 7), and
    // `docs/contract/validate.py` forbids the field outright - a fixture that
    // carried it "is describing an engine configured wrong".
    let w = world(&["write_note"], true).await;
    script(
        &w.server,
        tool_turn("write_note", json!({"title": "t", "body_md": "b"})),
        text_turn("unused"),
    )
    .await;
    run_job(&w).await.expect("job");

    let entries = PgJournal::new(w.app.db.pool.clone())
        .load(RunId::from_uuid(w.run))
        .await
        .unwrap();
    let gate = entries
        .iter()
        .find(|e| e.kind.as_str() == "approval_requested")
        .expect("gate");
    assert!(
        gate.payload.get("expires_at").is_none(),
        "approval_ttl leaked into the journal: {}",
        gate.payload
    );
}

#[tokio::test]
async fn a_duplicate_delivery_of_a_finished_run_does_nothing() {
    let w = world(&["write_note"], false).await;
    script(
        &w.server,
        tool_turn("write_note", json!({"title": "t", "body_md": "b"})),
        text_turn("done"),
    )
    .await;

    run_job(&w).await.expect("first");
    let after_first = journal_kinds(&w).await.len();
    run_job(&w).await.expect("second");

    assert_eq!(
        journal_kinds(&w).await.len(),
        after_first,
        "a replay must append nothing"
    );
    let notes: i64 = sqlx::query_scalar("select count(*) from notes where account_id = $1")
        .bind(w.account)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 1, "the effect must not be written twice");
}

#[tokio::test]
async fn a_job_for_a_run_that_no_longer_exists_is_dropped_rather_than_retried() {
    let w = world(&["read_thread"], false).await;
    sqlx::query("delete from agents where id = $1")
        .bind(w.agent)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    // A job outliving its row is ordinary, not an error: retrying it five times
    // and dead-lettering would be noise.
    run_job(&w).await.expect("dropped cleanly");
}

#[tokio::test]
async fn a_job_with_no_run_id_fails_loudly() {
    let w = world(&["read_thread"], false).await;
    let handler = RunAgentHandler {
        state: w.app.state.clone(),
    };
    let ctx = JobContext {
        queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
    };
    let mut job = job_for(w.run);
    job.payload = json!({});
    assert!(handler.handle(job, ctx).await.is_err());
}

// ------------------------------------------------------------- ceiling --

#[tokio::test]
async fn a_ceiling_breach_cancels_the_run_instead_of_retrying_the_job() {
    let mut w = world(&["read_thread"], false).await;
    // Ceiling zero: the adapter refuses before the first call.
    w.app.set_llm_ceiling_nano(0);
    script(&w.server, text_turn("unused"), text_turn("unused")).await;

    // `Ok`, not `Err`. An `Err` here is the queue's signal to retry, and every
    // retry that reached a model call would spend again - the exact path
    // PLAN.md forbids.
    run_job(&w)
        .await
        .expect("a breach must not be handed back to the queue");

    assert_eq!(status_of(&w).await, "failed");
    let error: Option<String> = sqlx::query_scalar("select error from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert!(
        error.unwrap().contains("spend limit"),
        "the row must say why"
    );

    // A loud terminal entry, not a silent stop.
    let kinds = journal_kinds(&w).await;
    assert_eq!(kinds.last().map(String::as_str), Some("run_ended"));
    let entries = PgJournal::new(w.app.db.pool.clone())
        .load(RunId::from_uuid(w.run))
        .await
        .unwrap();
    let ended = entries.last().unwrap();
    assert_eq!(ended.payload["reason"]["cap"], json!("cancelled"));

    assert_eq!(
        w.server.received_requests().await.unwrap().len(),
        0,
        "nothing may be sent"
    );
}

#[tokio::test]
async fn a_ceiling_breach_raises_exactly_one_feed_card_however_many_runs_hit_it() {
    let mut w = world(&["read_thread"], false).await;
    w.app.set_llm_ceiling_nano(0);
    script(&w.server, text_turn("unused"), text_turn("unused")).await;

    run_job(&w).await.expect("first");

    // A second run on the same account, same day.
    let second: Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, account_id, trigger_kind) \
         values ($1, $2, 'manual') returning id",
    )
    .bind(w.agent)
    .bind(w.account)
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();
    let handler = RunAgentHandler {
        state: w.app.state.clone(),
    };
    let ctx = JobContext {
        queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
    };
    handler.handle(job_for(second), ctx).await.expect("second");

    let cards: i64 = sqlx::query_scalar(
        "select count(*) from feed_items where account_id = $1 and data->>'reason' = 'spend_ceiling'",
    )
    .bind(w.account)
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();
    assert_eq!(cards, 1, "the user needs telling once, not once per run");
}

#[tokio::test]
async fn an_unreachable_provider_is_handed_back_to_the_queue() {
    let w = world(&["read_thread"], false).await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&w.server)
        .await;

    // Transport, not a decision: later might work, so the job backs off and
    // retries rather than killing the run.
    assert!(run_job(&w).await.is_err());
    assert_eq!(status_of(&w).await, "running", "the run stays claimable");
}

// -------------------------------------------------------------- delete --

#[tokio::test]
async fn deleting_an_agent_settles_a_queued_run_without_the_engine() {
    // `Engine::cancel` refuses an empty journal outright, and a `queued` run
    // has none - which is the ordinary state between being created and being
    // claimed, and so the state a DELETE most often meets.
    let w = world(&["read_thread"], false).await;
    assert_eq!(status_of(&w).await, "queued");

    cancel_runs_of(&w.app.state, w.agent).await.expect("cancel");

    assert_eq!(status_of(&w).await, "failed");
    assert!(
        journal_kinds(&w).await.is_empty(),
        "no journal was invented for it"
    );
}

#[tokio::test]
async fn deleting_an_agent_cancels_a_started_run_through_the_engine() {
    let w = world(&["write_note"], true).await;
    script(
        &w.server,
        tool_turn("write_note", json!({"title": "t", "body_md": "b"})),
        text_turn("unused"),
    )
    .await;
    run_job(&w).await.expect("job");
    assert_eq!(status_of(&w).await, "pending_approval");

    cancel_runs_of(&w.app.state, w.agent).await.expect("cancel");

    assert_eq!(status_of(&w).await, "failed");
    // Through the engine, so the log records its own ending rather than being
    // yanked away by the FK cascade mid-step.
    let kinds = journal_kinds(&w).await;
    assert_eq!(kinds.last().map(String::as_str), Some("run_ended"));
}

#[tokio::test]
async fn a_started_run_is_cancelled_through_the_engine_with_no_provider_configured() {
    let mut w = world(&["write_note"], true).await;
    script(
        &w.server,
        tool_turn("write_note", json!({"title": "t", "body_md": "b"})),
        text_turn("unused"),
    )
    .await;
    run_job(&w).await.expect("job");
    assert_eq!(status_of(&w).await, "pending_approval");

    // The key goes away *after* the run started - a redeploy that dropped the
    // variable, a rotated secret. Cancelling must still go through the engine:
    // a row moved straight to `failed` would carry a status its journal does
    // not support, which `API.md` §6.1 forbids and D57 records.
    w.app.clear_llm_key();
    assert!(
        w.app.state.llm.is_none(),
        "the provider must really be gone"
    );

    cancel_runs_of(&w.app.state, w.agent).await.expect("cancel");

    assert_eq!(status_of(&w).await, "failed");
    let kinds = journal_kinds(&w).await;
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("run_ended"),
        "the journal must record its own ending, provider or no provider"
    );
}

#[tokio::test]
async fn cancelling_leaves_already_finished_runs_alone() {
    let w = world(&["read_thread"], false).await;
    script(&w.server, text_turn("all done"), text_turn("x")).await;
    run_job(&w).await.expect("job");
    assert_eq!(status_of(&w).await, "done");

    cancel_runs_of(&w.app.state, w.agent).await.expect("cancel");
    assert_eq!(
        status_of(&w).await,
        "done",
        "a terminal run is not re-ended"
    );
}

// -------------------------------------------------------------- config --

#[tokio::test]
async fn an_agents_own_model_overrides_the_servers_default() {
    let w = world(&["read_thread"], false).await;
    sqlx::query("update agents set run_model = 'claude-opus-4-8' where id = $1")
        .bind(w.agent)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(wiremock::matchers::body_partial_json(
            json!({"model": "claude-opus-4-8"}),
        ))
        .respond_with(text_turn("done"))
        .expect(1)
        .mount(&w.server)
        .await;

    run_job(&w).await.expect("job");
    assert_eq!(status_of(&w).await, "done");
}

#[tokio::test]
async fn the_step_cap_actually_bounds_a_looping_model() {
    // The previous version of this test asserted only that two config fields
    // held their defaults - it never touched `engine_for`, so `EngineConfig`
    // could have been built from anything and it would still have passed. This
    // one drives a model that never stops calling tools and checks the engine
    // stopped it.
    let mut w = world(&["read_thread"], false).await;
    w.app.set_llm_ceiling_nano(i64::MAX);
    assert_eq!(
        w.app.config.llm.run_max_steps, 12,
        "PLAN.md fixes max_steps at 12"
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(tool_turn("read_thread", json!({"thread_id": "nope"})))
        .mount(&w.server)
        .await;

    run_job(&w).await.expect("job");

    assert_eq!(status_of(&w).await, "failed");
    let entries = PgJournal::new(w.app.db.pool.clone())
        .load(RunId::from_uuid(w.run))
        .await
        .unwrap();
    let ended = entries.last().expect("run_ended");
    assert_eq!(ended.payload["reason"]["cap"], json!("step_cap_exceeded"));
    assert_eq!(ended.payload["reason"]["limit"], json!(12));
    assert_eq!(ended.payload["status"], json!("failed"));
}

#[tokio::test]
async fn a_permanently_refused_call_cancels_the_run_instead_of_retrying_it() {
    // A bad key or a retired model id will answer the same way forever.
    // Returning `Err` hands the job back to the queue, which burns five
    // attempts and an hour of backoff and leaves the run non-terminal
    // throughout.
    let w = world(&["read_thread"], false).await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"type": "error", "error": {"message": "invalid x-api-key"}})),
        )
        .expect(1)
        .mount(&w.server)
        .await;

    run_job(&w)
        .await
        .expect("a permanent refusal must not be retried");

    assert_eq!(status_of(&w).await, "failed");
    let error: Option<String> = sqlx::query_scalar("select error from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert!(error.unwrap().contains("refused"), "the row must say why");
    assert_eq!(
        journal_kinds(&w).await.last().map(String::as_str),
        Some("run_ended")
    );
}

#[tokio::test]
async fn a_run_is_billed_to_its_agent_and_its_run() {
    let w = world(&["read_thread"], false).await;
    script(&w.server, text_turn("done"), text_turn("x")).await;
    run_job(&w).await.expect("job");

    let (agent_id, run_id, purpose): (Option<Uuid>, Option<Uuid>, String) =
        sqlx::query_as("select agent_id, run_id, purpose from llm_calls where account_id = $1")
            .bind(w.account)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(agent_id, Some(w.agent));
    assert_eq!(run_id, Some(w.run));
    assert_eq!(purpose, "run");
}

#[test]
fn the_dedupe_key_is_stable_and_scoped_to_one_run() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    assert_eq!(dedupe_key(a), dedupe_key(a));
    assert_ne!(dedupe_key(a), dedupe_key(b));
    assert!(dedupe_key(a).starts_with(KIND));
}

/// `TERMINAL_STATUSES` is the host's copy of a fact the SDK owns. Two lists,
/// one authority: if `RunStatus` ever grows a state, or changes a spelling,
/// this fails rather than the SQL quietly leaving rows behind.
#[test]
fn terminal_statuses_match_the_sdk() {
    use nade_agent_sdk::RunStatus;

    let sdk: Vec<&str> = [
        RunStatus::Done,
        RunStatus::Failed,
        RunStatus::PendingApproval,
        RunStatus::Waiting,
        RunStatus::Skipped,
        RunStatus::Expired,
    ]
    .into_iter()
    .filter(RunStatus::is_terminal)
    .map(|status| status.as_str())
    .collect();

    let mut ours = super::TERMINAL_STATUSES.to_vec();
    let mut theirs = sdk.clone();
    ours.sort_unstable();
    theirs.sort_unstable();
    assert_eq!(
        ours, theirs,
        "the host's terminal list drifted from the SDK's"
    );

    // And the helper both SQL predicates share agrees with both lists.
    for status in &sdk {
        assert!(super::is_terminal(status), "{status} should be terminal");
    }
    for status in [RunStatus::PendingApproval, RunStatus::Waiting] {
        assert!(
            !super::is_terminal(status.as_str()),
            "{} is not terminal",
            status.as_str()
        );
    }
}
