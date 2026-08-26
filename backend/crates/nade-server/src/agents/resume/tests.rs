//! The resume job, and the interleavings it has to survive.

use durable_agent::{Journal, RunId};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::MockServer;

use super::*;
use crate::{
    agents::run::RunAgentHandler,
    config::Env,
    jobs::{Job, JobContext, Queue},
    runtime::PgJournal,
    test_support::{test_app, TestApp},
};

struct World {
    app: TestApp,
    account: Uuid,
    run: Uuid,
    #[allow(dead_code)]
    server: MockServer,
}

async fn world(approval_required: bool) -> World {
    let server = MockServer::start().await;
    let mut app = test_app(Env::Dev).await;
    app.set_llm_base(&server.uri());

    let account: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("resume-{}@example.com", Uuid::new_v4()))
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    let agent: Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, spec, allowed_tools, \
                             approval_required, status) \
         values ($1, 'Tester', 'note it', $2, $3, $4, 'published') returning id",
    )
    .bind(account)
    .bind(json!({"instruction": "Take a note."}))
    .bind(vec!["write_note".to_owned()])
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
        run,
        server,
    }
}

/// The one conversation every test here drives: gate on a note, then finish.
async fn script(w: &World) {
    crate::test_support::script(
        &w.server,
        crate::test_support::tool_turn(
            "write_note",
            json!({"title": "Kettle", "body_md": "- intro"}),
            Some("Two next steps found."),
        ),
        crate::test_support::text_turn("Saved."),
    )
    .await;
}

async fn run_job(w: &World) -> anyhow::Result<()> {
    RunAgentHandler::shared(w.app.state.clone())
        .handle(
            Job {
                id: 1,
                kind: crate::agents::run::KIND.to_owned(),
                payload: json!({"run_id": w.run}),
                attempts: 0,
            },
            JobContext {
                queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
            },
        )
        .await
}

async fn resume_job(w: &World, step_seq: u32, decision: &str) -> anyhow::Result<()> {
    ResumeRunHandler::shared(w.app.state.clone())
        .handle(
            Job {
                id: 2,
                kind: KIND.to_owned(),
                payload: json!({"run_id": w.run, "step_seq": step_seq, "decision": decision}),
                attempts: 0,
            },
            JobContext {
                queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
            },
        )
        .await
}

async fn status_of(w: &World) -> String {
    sqlx::query_scalar("select status from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap()
}

async fn pending_request(w: &World) -> durable_agent::ApprovalRequest {
    let pending: Value = sqlx::query_scalar("select pending_action from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    serde_json::from_value(pending).expect("the persisted ApprovalRequest")
}

async fn pending_step(w: &World) -> u32 {
    let pending: Value = sqlx::query_scalar("select pending_action from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    pending["step_seq"].as_u64().unwrap() as u32
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

// ------------------------------------------------------------- the keys --

#[test]
fn the_dedupe_key_names_the_run_and_the_step() {
    // Not the run alone: a run can pause more than once, and the second
    // decision must not be suppressed while the first job is still pending.
    assert_ne!(
        dedupe_key(Uuid::nil(), 6),
        dedupe_key(Uuid::nil(), 10),
        "one job per decision"
    );
    // And never `run_agent`'s key, which is what makes the enqueue safe when a
    // stale run job is still pending.
    assert_ne!(
        dedupe_key(Uuid::nil(), 6),
        crate::agents::run::dedupe_key(Uuid::nil())
    );
}

#[tokio::test]
async fn a_suppressed_enqueue_fails_the_transaction_rather_than_vanishing() {
    let w = world(true).await;
    enqueue_in(&w.app.db.pool, w.run, 6, Decision::Approve)
        .await
        .expect("the first lands");
    let second = enqueue_in(&w.app.db.pool, w.run, 6, Decision::Approve).await;
    assert!(
        second.is_err(),
        "a swallowed enqueue is the bug this module exists to avoid"
    );
}

// ------------------------------------------------------ the interleavings --

/// **The race that made a status predicate necessary.**
///
/// `Engine::run` answers a parked run by replay, returning `PendingApproval`
/// without appending anything, so the outcome a stale `run_agent` job holds can
/// be settled *after* the approval has already been granted and carried out.
/// Unguarded, that settle put the run back to `pending_approval` for ever, with
/// the note already written and the card already resolved.
#[tokio::test]
async fn a_stale_run_outcome_cannot_undo_a_decision_that_already_landed() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    assert_eq!(status_of(&w).await, "pending_approval");
    let step = pending_step(&w).await;

    // The user approves, and the resume job carries the run to `done`.
    sqlx::query("update agent_runs set status = 'queued' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    resume_job(&w, step, "approve")
        .await
        .expect("the resume job");
    assert_eq!(status_of(&w).await, "done");

    // Now the stale delivery arrives.
    run_job(&w).await.expect("a stale delivery is not an error");

    assert_eq!(
        status_of(&w).await,
        "done",
        "a replayed outcome must not resurrect a settled run"
    );
    let notes: i64 = sqlx::query_scalar("select count(*) from notes where run_id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 1, "and must not write the effect twice");
}

/// **The interleaving the status guard is actually for**, and which the early
/// return does not cover.
///
/// A stale `run_agent` job can be past the early return — it loaded the run
/// while it was still `queued` — and then hold a replayed `PendingApproval`
/// while the resume job claims the run and sets it `running`. A guard of
/// `status = 'running'` alone matches the *resume's* own write, so the stale
/// settle lands: the run goes back to `pending_approval` with the answered
/// request, the card stays `resolved`, and the early return then guarantees
/// `run_agent` will never touch it again. Stranded, permanently.
///
/// `attempt` is what tells the two apart, because the claim that sets
/// `running` is the same statement that bumps it.
#[tokio::test]
async fn a_stale_outcome_held_across_the_decision_cannot_settle() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");

    // The stale job: it claims the run and holds an outcome.
    sqlx::query("update agent_runs set status = 'queued' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    let handler = RunAgentHandler::for_tests(w.app.state.clone());
    let stale_attempt = handler
        .claim_for_tests(w.run)
        .await
        .unwrap()
        .expect("the stale job claims it");
    let stale_outcome = durable_agent::RunOutcome::PendingApproval {
        run_id: RunId::from_uuid(w.run),
        request: Box::new(pending_request(&w).await),
        stats: durable_agent::RunStats::default(),
    };

    // Meanwhile the decision lands and the resume job claims the run, which
    // bumps `attempt` past the stale one. The resume is **still running** at
    // this point, which is the whole difficulty: a guard of `status =
    // 'running'` alone matches the resume's own write.
    sqlx::query("update agent_runs set status = 'queued' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    let live_attempt = handler
        .claim_for_tests(w.run)
        .await
        .unwrap()
        .expect("the resume job claims it");
    assert_ne!(live_attempt, stale_attempt, "a claim stamps a new attempt");
    assert_eq!(status_of(&w).await, "running");

    let cards_before: i64 = sqlx::query_scalar("select count(*) from feed_items")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    let tokens_before: i32 =
        sqlx::query_scalar("select tokens_spent from agent_runs where id = $1")
            .bind(w.run)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();

    // And now the stale settle arrives, against a row that is `running` and
    // owned by somebody else.
    handler
        .settle_for_tests(w.run, &stale_outcome, stale_attempt)
        .await
        .expect("a refused settle is not an error");

    assert_eq!(
        status_of(&w).await,
        "running",
        "a settle from a superseded claim must not park the run"
    );
    // Nothing of the row moved: not the status, and not the counters the same
    // statement writes.
    let tokens_after: i32 = sqlx::query_scalar("select tokens_spent from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        tokens_after, tokens_before,
        "the refused write touched nothing"
    );
    let cards_after: i64 = sqlx::query_scalar("select count(*) from feed_items")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        cards_after, cards_before,
        "and must not raise a second card"
    );

    // The live claim still settles, which is the other half: the guard refuses
    // a superseded writer, not every writer.
    handler
        .settle_for_tests(w.run, &stale_outcome, live_attempt)
        .await
        .expect("the owner settles");
    assert_eq!(status_of(&w).await, "pending_approval");
}

/// The terminal branch's own guard, which the attempt stamp deliberately does
/// **not** cover.
///
/// A run reaches a terminal status exactly once, and the skip and expiry
/// transactions write one *before* the engine records it (`API.md` §7). So a
/// settle that arrives afterwards — a stale job holding a `Failed`, say — must
/// not overwrite the answer the user was already given.
#[tokio::test]
async fn a_settle_cannot_overwrite_a_terminal_status() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");

    let handler = RunAgentHandler::for_tests(w.app.state.clone());
    sqlx::query("update agent_runs set status = 'queued' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    let attempt = handler
        .claim_for_tests(w.run)
        .await
        .unwrap()
        .expect("claimed");

    // Exactly what `POST /feed/{id}/skip` writes, before its job runs.
    sqlx::query("update agent_runs set status = 'skipped' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();

    let stale = durable_agent::RunOutcome::Failed {
        run_id: RunId::from_uuid(w.run),
        reason: durable_agent::FailureReason::Cancelled {
            detail: "a stale job's idea of what happened".to_owned(),
        },
        stats: durable_agent::RunStats::default(),
    };
    handler
        .settle_for_tests(w.run, &stale, attempt)
        .await
        .expect("a refused settle is not an error");

    assert_eq!(
        status_of(&w).await,
        "skipped",
        "the user's answer must not be overwritten by a late outcome"
    );
    let error: Option<String> = sqlx::query_scalar("select error from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert!(error.is_none(), "and nothing of it may land: {error:?}");
}

/// The cheaper half of the same defence: a parked run belongs to `resume_run`,
/// so `run_agent` never even reaches the engine for one.
#[tokio::test]
async fn the_run_job_declines_a_parked_run_outright() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    let before = journal_kinds(&w).await;

    let calls_before = w.server.received_requests().await.unwrap_or_default().len();
    run_job(&w).await.expect("a second delivery");

    assert_eq!(journal_kinds(&w).await, before, "nothing was appended");
    assert_eq!(status_of(&w).await, "pending_approval");
    // The assertion that tells "declined" apart from "did the same work
    // again": a parked run must not reach the engine, and reaching the engine
    // is visible as a model call.
    assert_eq!(
        w.server.received_requests().await.unwrap_or_default().len(),
        calls_before,
        "a parked run must not be driven"
    );
}

/// EDGE (duplicate delivery): the same decision delivered twice. The SDK
/// answers `AlreadyResolved`, and an earlier delivery having won is success.
#[tokio::test]
async fn a_replayed_decision_is_success_not_a_retry() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    let step = pending_step(&w).await;

    resume_job(&w, step, "approve").await.expect("first");
    let kinds = journal_kinds(&w).await;

    resume_job(&w, step, "approve")
        .await
        .expect("a duplicate delivery is not a failure");
    assert_eq!(journal_kinds(&w).await, kinds, "and appends nothing new");

    let notes: i64 = sqlx::query_scalar("select count(*) from notes where run_id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 1);
}

/// A decision naming a step the run is not parked on must never be applied to
/// whatever *is* open. `Resolution` carries `step_seq` for exactly this.
#[tokio::test]
async fn a_decision_for_the_wrong_step_is_refused_and_audited() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    let step = pending_step(&w).await;

    resume_job(&w, step + 7, "approve")
        .await
        .expect("a mismatch is handled, not retried for ever");

    assert_eq!(status_of(&w).await, "pending_approval", "still waiting");
    let notes: i64 = sqlx::query_scalar("select count(*) from notes")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 0, "nothing was executed");
    let audited: i64 =
        sqlx::query_scalar("select count(*) from audit_log where action = 'resume_step_mismatch'")
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

/// **Skip's terminal-before-the-journal problem, from the job's side.**
///
/// `API.md` §7 has the skip transaction write `skipped` before the engine has
/// recorded anything. `run_agent` opens with an `is_terminal` guard; copying it
/// here would make skip a silent no-op and leave the journal open for ever.
#[tokio::test]
async fn a_terminal_row_does_not_stop_the_engine_closing_the_journal() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    let step = pending_step(&w).await;

    // Exactly what the skip transaction does.
    sqlx::query("update agent_runs set status = 'skipped' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();

    resume_job(&w, step, "skip").await.expect("the settle job");

    let kinds = journal_kinds(&w).await;
    assert!(kinds.contains(&"approval_resolved".to_owned()), "{kinds:?}");
    assert_eq!(kinds.last().unwrap(), "run_ended");
    assert_eq!(status_of(&w).await, "skipped");
}

/// **The approval that would otherwise vanish.**
///
/// The approve transaction commits the user's answer and then depends on this
/// job. Five failures later the queue gives up — and the run used to sit
/// `queued` for ever, with the card still reading "Saved to Notes." about a
/// note that was never written. `run_agent` has a different dedupe key,
/// nothing re-enqueues it, and there is no stuck-run reaper.
#[tokio::test]
async fn a_dead_lettered_decision_ends_the_run_and_corrects_the_card() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    let step = pending_step(&w).await;

    // What the approve transaction leaves behind.
    sqlx::query("update agent_runs set status = 'queued' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    sqlx::query(
        "update feed_items set status = 'resolved', approval_token = null, \
                resolved_note = 'Saved to Notes.' where run_id = $1",
    )
    .bind(w.run)
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    ResumeRunHandler::shared(w.app.state.clone())
        .on_dead_letter(
            Job {
                id: 9,
                kind: KIND.to_owned(),
                payload: json!({"run_id": w.run, "step_seq": step, "decision": "approve"}),
                attempts: 5,
            },
            JobContext {
                queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
            },
        )
        .await;

    // The run is terminal, through the journal rather than by a bare row write.
    assert_eq!(status_of(&w).await, "failed");
    assert_eq!(journal_kinds(&w).await.last().unwrap(), "run_ended");

    // And the card stops claiming the note was saved.
    let note: Option<String> =
        sqlx::query_scalar("select resolved_note from feed_items where run_id = $1")
            .bind(w.run)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(
        note.as_deref(),
        Some("The agent couldn't finish this, so nothing was saved.")
    );

    let audited: i64 =
        sqlx::query_scalar("select count(*) from audit_log where action = 'run_abandoned'")
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

/// EDGE (duplicate delivery, at the end of a life): a dead letter for a run
/// that already finished changes nothing.
#[tokio::test]
async fn a_dead_letter_for_a_finished_run_is_a_no_op() {
    let w = world(true).await;
    script(&w).await;
    run_job(&w).await.expect("the run job");
    let step = pending_step(&w).await;
    sqlx::query("update agent_runs set status = 'queued' where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    resume_job(&w, step, "approve").await.expect("resumed");
    assert_eq!(status_of(&w).await, "done");
    let before = journal_kinds(&w).await;

    ResumeRunHandler::shared(w.app.state.clone())
        .on_dead_letter(
            Job {
                id: 9,
                kind: KIND.to_owned(),
                payload: json!({"run_id": w.run, "step_seq": step, "decision": "approve"}),
                attempts: 5,
            },
            JobContext {
                queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
            },
        )
        .await;

    assert_eq!(status_of(&w).await, "done", "a finished run is left alone");
    assert_eq!(journal_kinds(&w).await, before);
}

#[tokio::test]
async fn a_run_that_is_gone_drops_the_decision() {
    let w = world(true).await;
    sqlx::query("delete from agent_runs where id = $1")
        .bind(w.run)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    resume_job(&w, 6, "approve")
        .await
        .expect("a job outliving its row is ordinary");
}

#[tokio::test]
async fn a_malformed_payload_is_an_error_the_queue_can_see() {
    let w = world(true).await;
    for payload in [
        json!({}),
        json!({"run_id": w.run}),
        json!({"run_id": w.run, "step_seq": 6}),
        json!({"run_id": w.run, "step_seq": 6, "decision": "telepathy"}),
        json!({"run_id": "not-a-uuid", "step_seq": 6, "decision": "approve"}),
    ] {
        let result = ResumeRunHandler::shared(w.app.state.clone())
            .handle(
                Job {
                    id: 3,
                    kind: KIND.to_owned(),
                    payload: payload.clone(),
                    attempts: 0,
                },
                JobContext {
                    queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
                },
            )
            .await;
        assert!(result.is_err(), "{payload}");
    }
    let _ = w.account;
}
