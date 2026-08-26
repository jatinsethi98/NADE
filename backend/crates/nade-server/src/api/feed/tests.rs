//! The approval loop, end to end, over a real database.
//!
//! Every test here drives the real engine against a scripted provider, so a
//! card is raised by the code that raises cards and settled by the transaction
//! that settles them. The alternative — hand-inserting a `feed_items` row —
//! would test the endpoint against a world the producer cannot actually create.

use durable_agent::{Journal, RunId};
use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::MockServer;

use crate::test_support::{script, text_turn, tool_turn};
use crate::{
    agents::{expire, resume, run::RunAgentHandler},
    config::Env,
    jobs::{Job, JobContext, Queue},
    runtime::PgJournal,
    test_support::{fixture, post_json_as, response_json, send, test_app, TestApp},
};

// ------------------------------------------------------------- harness --

struct World {
    app: TestApp,
    account: Uuid,
    agent: Uuid,
    run: Uuid,
    token: String,
    #[allow(dead_code)]
    server: MockServer,
}

/// A published agent, one manual run, and a paired device bound to the account.
async fn world(tool: &str, approval_required: bool) -> World {
    let server = MockServer::start().await;
    let mut app = test_app(Env::Dev).await;
    app.set_llm_base(&server.uri());

    let account: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("feed-{}@example.com", Uuid::new_v4()))
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    sqlx::query("insert into settings (account_id) values ($1)")
        .bind(account)
        .execute(&app.db.pool)
        .await
        .unwrap();

    let agent: Uuid = sqlx::query_scalar(
        "insert into agents (account_id, name, nl_definition, spec, allowed_tools, \
                             approval_required, status) \
         values ($1, 'Job Search Tracker', 'when a recruiter emails, note the next steps', \
                 $2, $3, $4, 'published') returning id",
    )
    .bind(account)
    .bind(json!({"instruction": "Take a note about the mailbox."}))
    .bind(vec![tool.to_owned(), "read_thread".to_owned()])
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

    let token = app.device_token().await;
    sqlx::query("update devices set account_id = $1")
        .bind(account)
        .execute(&app.db.pool)
        .await
        .unwrap();

    World {
        app,
        account,
        agent,
        run,
        token,
        server,
    }
}

async fn drive(w: &World, kind: &str, payload: Value) -> anyhow::Result<()> {
    let ctx = JobContext {
        queue: Queue::new(w.app.db.pool.clone(), w.app.config.jobs.clone()),
    };
    let job = Job {
        id: 1,
        kind: kind.to_owned(),
        payload,
        attempts: 0,
    };
    match kind {
        crate::agents::run::KIND => {
            RunAgentHandler::shared(w.app.state.clone())
                .handle(job, ctx)
                .await
        }
        crate::agents::resume::KIND => {
            resume::ResumeRunHandler::shared(w.app.state.clone())
                .handle(job, ctx)
                .await
        }
        other => panic!("no handler for {other}"),
    }
}

/// Park the run on an approval, and return the card the producer raised.
async fn park(w: &World, tool: &str, args: Value) -> Value {
    script(
        &w.server,
        tool_turn(
            tool,
            args,
            Some("Two next steps found — an intro and a portfolio session."),
        ),
        text_turn("done"),
    )
    .await;
    drive(w, crate::agents::run::KIND, json!({"run_id": w.run}))
        .await
        .expect("the run job");
    assert_eq!(status_of(w).await, "pending_approval");
    feed(w).await["items"][0].clone()
}

async fn feed(w: &World) -> Value {
    response_json(crate::test_support::get(&w.app, "/v1/feed", Some(&w.token)).await).await
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

async fn resume_jobs(w: &World) -> Vec<Value> {
    sqlx::query_scalar::<_, Value>("select payload from jobs where kind = 'resume_run' order by id")
        .fetch_all(&w.app.db.pool)
        .await
        .unwrap()
}

const NOTE_ARGS: fn() -> Value = || {
    json!({"title": "Kettle — next steps", "body_md": "- intro\n- portfolio",
           "thread_id": "18f2a1b3c4d5e6f7"})
};

// ------------------------------------------------------- raising a card --

#[tokio::test]
async fn parking_a_run_raises_a_card_stamped_by_the_engines_clock() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;

    assert_eq!(card["kind"], "approval");
    assert_eq!(card["status"], "new");
    assert_eq!(card["title"], "Job Search Tracker");
    assert_eq!(
        card["body"],
        "Two next steps found — an intro and a portfolio session."
    );
    assert_eq!(card["actions"], json!(["approve", "skip"]));
    assert!(card["approval_token"].is_string());
    assert_eq!(card["resolved_note"], json!(null));
    assert_eq!(card["data"]["action_label"], "Save note");

    // `docs/contract/validate.py`: the card's `created_at` **is** the gate
    // entry's, and `approval_expires_at` is exactly seven days after it.
    // `ApprovalRequest::requested_at` is the step's `opened_at` and `Entry::new`
    // reads the clock again, so only the entry itself satisfies the rule.
    let gate: (Value, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "select payload, created_at from run_journal \
          where run_id = $1 and kind = 'approval_requested'",
    )
    .bind(w.run)
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();
    assert_eq!(card["created_at"], crate::api::mail::wire_ts(gate.1));
    assert_eq!(
        card["approval_expires_at"],
        crate::api::mail::wire_ts(gate.1 + chrono::Duration::days(7))
    );
    assert_eq!(card["data"]["note_id"], gate.0["effect_id"]);
    assert_eq!(card["data"]["action"], gate.0["tool"]);
}

/// EDGE (duplicate delivery): the producer is reached again for a run that has
/// already parked. A second card would mean a **second live token** for one
/// decision.
///
/// This drives `settle` directly rather than re-delivering the job, and the
/// difference matters: `run_agent` now declines a parked run before the engine
/// (D71), so a second `drive` returns without ever reaching the producer — and
/// this test, written that way, could not have caught the `on conflict` clause
/// or the unique index being removed.
#[tokio::test]
async fn raising_a_card_twice_for_one_step_produces_one_card() {
    let w = world("write_note", true).await;
    let first = park(&w, "write_note", NOTE_ARGS()).await;

    let handler = crate::agents::run::RunAgentHandler::for_tests(w.app.state.clone());
    let pending: Value = sqlx::query_scalar("select pending_action from agent_runs where id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    let request: durable_agent::ApprovalRequest =
        serde_json::from_value(pending).expect("the persisted request");
    let outcome = durable_agent::RunOutcome::PendingApproval {
        run_id: durable_agent::RunId::from_uuid(w.run),
        request: Box::new(request),
        stats: durable_agent::RunStats::default(),
    };

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
    handler
        .settle_for_tests(w.run, &outcome, attempt)
        .await
        .expect("re-raising is idempotent, not an error");

    let page = feed(&w).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1, "one card");
    assert_eq!(
        page["items"][0]["approval_token"], first["approval_token"],
        "and the token it already published, not a second live one"
    );
    assert_eq!(page["new_count"], 1);
}

#[tokio::test]
async fn a_draft_gate_offers_edit_and_flags_a_stranger() {
    let w = world("draft_reply", true).await;
    let card = park(
        &w,
        "draft_reply",
        json!({"to": ["kamran@northbound.co"], "subject": "Re: Thursday",
               "body_text": "Works for me.", "thread_id": "18f28c5d6e7f8a9b"}),
    )
    .await;

    assert_eq!(card["actions"], json!(["approve", "edit", "skip"]));
    assert_eq!(card["data"]["action_label"], "Save draft");
    // `backend/testdata/injection/README.md` finding 10: the card is contained
    // only if it shows who the draft is addressed to and that this mailbox has
    // never written to them.
    assert_eq!(card["data"]["to"], json!(["kamran@northbound.co"]));
    assert_eq!(card["data"]["never_messaged"], json!(true));
}

#[tokio::test]
async fn a_recipient_this_mailbox_has_written_to_is_not_flagged() {
    let w = world("draft_reply", true).await;
    sqlx::query(
        "insert into messages (account_id, gmail_id, thread_id, from_email, to_json) \
         values ($1, 'm1', 't1', 'me@example.com', $2)",
    )
    .bind(w.account)
    .bind(json!(["Kamran@Northbound.co"]))
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    let card = park(
        &w,
        "draft_reply",
        json!({"to": ["kamran@northbound.co"], "subject": "Re: Thursday",
               "body_text": "…", "thread_id": "18f28c5d6e7f8a9b"}),
    )
    .await;
    assert_eq!(
        card["data"]["never_messaged"],
        json!(false),
        "the match is case-insensitive on both sides"
    );
}

// ----------------------------------------------------------- approving --

/// `API.md` §7's five writes, and the sixth thing that must **not** happen.
#[tokio::test]
async fn approve_lands_all_five_writes_and_leaves_the_journal_alone() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    let before = journal_kinds(&w).await;

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = response_json(response).await;
    assert_eq!(body["status"], "queued");
    assert_eq!(body["run_id"], json!(w.run));

    // 1. the token is consumed.
    let token: Option<Uuid> =
        sqlx::query_scalar("select approval_token from feed_items where id = $1")
            .bind(Uuid::parse_str(card["id"].as_str().unwrap()).unwrap())
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert!(token.is_none());

    // 2. the run is queued, with its pending action intact.
    assert_eq!(status_of(&w).await, "queued");
    let pending: Option<Value> =
        sqlx::query_scalar("select pending_action from agent_runs where id = $1")
            .bind(w.run)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(pending.expect("kept")["tool"], "write_note");

    // 3. the card is resolved, with its note.
    let page = feed(&w).await;
    assert_eq!(page["items"][0]["status"], "resolved");
    assert_eq!(page["items"][0]["resolved_note"], "Saved to Notes.");
    assert_eq!(page["items"][0]["actions"], json!([]));
    assert_eq!(page["items"][0]["approval_token"], json!(null));
    // Deliberately kept: it is what lets a finished card say *when* it expired.
    assert!(page["items"][0]["approval_expires_at"].is_string());
    assert_eq!(page["new_count"], 0);

    // 4. the audit row names the device, not the system.
    let (actor, subject): (String, Value) =
        sqlx::query_as("select actor, subject from audit_log where action = 'feed.approve'")
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_ne!(actor, "system");
    assert_eq!(subject["run_id"], json!(w.run));
    assert!(subject["step_seq"].is_number());

    // 5. the resume job is enqueued, naming the step.
    let jobs = resume_jobs(&w).await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["decision"], "approve");
    assert_eq!(jobs[0]["step_seq"], subject["step_seq"]);

    // And the sixth: **the transaction never touches `run_journal`.** The
    // journal has one author, and `approval_resolved` arrives only when the
    // engine writes it (`API.md` §6.1).
    assert_eq!(journal_kinds(&w).await, before);
}

#[tokio::test]
async fn the_resume_job_carries_an_approval_to_done_and_writes_the_effect() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    approve(&w, &card).await;

    let job = resume_jobs(&w).await.remove(0);
    drive(&w, crate::agents::resume::KIND, job)
        .await
        .expect("the resume job");

    assert_eq!(status_of(&w).await, "done");
    let kinds = journal_kinds(&w).await;
    assert!(kinds.contains(&"approval_resolved".to_owned()), "{kinds:?}");
    assert_eq!(kinds.last().unwrap(), "run_ended");

    // The note landed under the id the card published, before it existed.
    let (id, title): (Uuid, String) =
        sqlx::query_as("select id, title from notes where run_id = $1")
            .bind(w.run)
            .fetch_one(&w.app.db.pool)
            .await
            .unwrap();
    assert_eq!(json!(id), card["data"]["note_id"]);
    assert_eq!(title, "Kettle — next steps");
    // `API.md` §6.2: an agent-written effect carries a v5 uuid.
    assert_eq!(id.get_version_num(), 5);
}

/// **One card per decision, and the resume must not raise a second.**
///
/// The first cut of `settle` called `raise_run_info` on every `Done`, so the
/// headline flow — card, approve, resume executes the note, `Done` — produced
/// a *second* card saying "The agent saved a note." beside the first saying
/// "Saved to Notes.", and `new_count` went back up the moment the user cleared
/// it. `docs/contract/feed.json` has exactly one card per run.
///
/// The old test asserted `new_count == 0` **before** the resume job ran, which
/// is the one moment the bug is invisible.
#[tokio::test]
async fn approving_and_resuming_leaves_exactly_one_card() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    approve(&w, &card).await;

    let job = resume_jobs(&w).await.remove(0);
    drive(&w, crate::agents::resume::KIND, job)
        .await
        .expect("the resume job");
    assert_eq!(status_of(&w).await, "done");

    let page = feed(&w).await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "one decision, one card: {items:#?}");
    assert_eq!(items[0]["id"], card["id"]);
    assert_eq!(items[0]["status"], "resolved");
    assert_eq!(
        page["new_count"], 0,
        "the badge must not climb back after the user has cleared it"
    );
}

async fn approve(w: &World, card: &Value) -> Value {
    response_json(
        post_json_as(
            &w.app,
            &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
            &json!({"approval_token": card["approval_token"]}),
            Some(&w.token),
        )
        .await,
    )
    .await
}

/// EDGE (duplicate delivery): the same tap twice, or the same push action
/// delivered twice. `API.md` §7: "**Clients treat this as success**".
#[tokio::test]
async fn a_replayed_approval_is_409_token_consumed() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    approve(&w, &card).await;

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 409);
    assert_eq!(
        response_json(response).await,
        fixture("error_token_consumed.json")
    );

    // And exactly one resume job: a replay must not enqueue a second decision.
    assert_eq!(resume_jobs(&w).await.len(), 1);
}

#[tokio::test]
async fn skipping_after_approving_is_also_token_consumed() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    approve(&w, &card).await;

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/skip", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 409);
}

#[tokio::test]
async fn a_wrong_token_is_401_and_changes_nothing() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
        &json!({"approval_token": Uuid::new_v4()}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 401);
    assert_eq!(status_of(&w).await, "pending_approval");
    assert!(resume_jobs(&w).await.is_empty());
    assert_eq!(feed(&w).await["items"][0]["status"], "new");
}

#[tokio::test]
async fn another_accounts_card_is_404_not_401() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;

    // A second account, and a device bound to it.
    let other: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind("other@example.com")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    let intruder = w.app.device_token().await;
    // The device this call just paired is the newest row; binding by id keeps
    // the test out of the token-hashing internals.
    sqlx::query(
        "update devices set account_id = $1 \
          where id = (select id from devices order by created_at desc limit 1)",
    )
    .bind(other)
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&intruder),
    )
    .await;
    assert_eq!(
        response.status(),
        404,
        "a token is not a capability across accounts"
    );
    assert_eq!(status_of(&w).await, "pending_approval");
}

// -------------------------------------------------------------- skipping --

#[tokio::test]
async fn skip_settles_the_card_and_the_run_and_the_engine_closes_the_journal() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    let before = journal_kinds(&w).await;

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/skip", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response_json(response).await, fixture("skip.json"));

    assert_eq!(status_of(&w).await, "skipped");
    let page = feed(&w).await;
    assert_eq!(page["items"][0]["status"], "skipped");
    assert_eq!(
        page["items"][0]["resolved_note"],
        "Skipped — nothing was saved."
    );
    assert_eq!(journal_kinds(&w).await, before, "the tx never journals");

    // **The run is terminal before the engine has written anything**, which is
    // why `resume_run` must not carry `run_agent`'s `is_terminal` guard. If it
    // did, this job would be a no-op and the journal would never close.
    let job = resume_jobs(&w).await.remove(0);
    assert_eq!(job["decision"], "skip");
    drive(&w, crate::agents::resume::KIND, job)
        .await
        .expect("the settle job");

    let kinds = journal_kinds(&w).await;
    assert!(kinds.contains(&"approval_resolved".to_owned()), "{kinds:?}");
    assert_eq!(kinds.last().unwrap(), "run_ended");
    assert_eq!(status_of(&w).await, "skipped");
    let notes: i64 = sqlx::query_scalar("select count(*) from notes where run_id = $1")
        .bind(w.run)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(notes, 0, "a skip writes nothing");
}

// -------------------------------------------------------------- expiry --

#[tokio::test]
async fn the_sweep_ages_a_card_and_its_run_out_together() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;

    assert!(!expire::due(&w.app.db.pool).await.unwrap(), "not yet");
    sqlx::query("update feed_items set approval_expires_at = now() - interval '1 second'")
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    assert!(expire::due(&w.app.db.pool).await.unwrap());

    assert_eq!(expire::sweep(&w.app.state).await.unwrap(), 1);

    let page = feed(&w).await;
    assert_eq!(page["items"][0]["status"], "expired");
    assert_eq!(
        page["items"][0]["resolved_note"],
        "Expired after 7 days — nothing was saved."
    );
    assert_eq!(page["items"][0]["approval_token"], json!(null));
    assert_eq!(page["new_count"], 0);
    // Both, in the same transaction: `validate.py` ties an expired card to an
    // expired run, and a sweep that moved one would leave them disagreeing for
    // as long as the job took — or for ever, if it dead-lettered.
    assert_eq!(status_of(&w).await, "expired");

    let job = resume_jobs(&w).await.remove(0);
    assert_eq!(job["decision"], "expire");
    drive(&w, crate::agents::resume::KIND, job)
        .await
        .expect("the expiry job");
    assert_eq!(journal_kinds(&w).await.last().unwrap(), "run_ended");

    // EDGE (duplicate delivery): a second sweep finds nothing.
    assert_eq!(expire::sweep(&w.app.state).await.unwrap(), 0);
    let _ = card;
}

/// EDGE (expiry, one second either side of the deadline). One second, actually
/// — the "early" half read `+ interval '1 hour'`, which is not a boundary.
#[tokio::test]
async fn approving_one_second_late_is_410_and_one_second_early_succeeds() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    let id = card["id"].as_str().unwrap().to_owned();

    sqlx::query("update feed_items set approval_expires_at = now() + interval '1 second'")
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    let early = post_json_as(
        &w.app,
        &format!("/v1/feed/{id}/approve"),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(early.status(), 200, "inside the deadline");

    // A second card, past its deadline.
    let w2 = world("write_note", true).await;
    let late_card = park(&w2, "write_note", NOTE_ARGS()).await;
    sqlx::query("update feed_items set approval_expires_at = now() - interval '1 second'")
        .execute(&w2.app.db.pool)
        .await
        .unwrap();
    let late = post_json_as(
        &w2.app,
        &format!("/v1/feed/{}/approve", late_card["id"].as_str().unwrap()),
        &json!({"approval_token": late_card["approval_token"]}),
        Some(&w2.token),
    )
    .await;
    assert_eq!(late.status(), 410);
    assert_eq!(
        response_json(late).await,
        fixture("error_approval_expired.json")
    );
    // The refusal is not passive: it settles the card and the run.
    assert_eq!(status_of(&w2).await, "expired");
    assert_eq!(feed(&w2).await["items"][0]["status"], "expired");
}

// ------------------------------------------------- a run that moved on --

/// The card names a step. A decision must address exactly that step, or the
/// answer to one question would be applied to another.
#[tokio::test]
async fn a_card_whose_run_is_parked_on_a_different_step_is_409_conflict() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;

    sqlx::query("update feed_items set step_seq = step_seq + 10")
        .execute(&w.app.db.pool)
        .await
        .unwrap();

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 409);
    assert_eq!(
        response_json(response).await,
        fixture("error_conflict.json")
    );
    assert!(resume_jobs(&w).await.is_empty());
}

/// `feed_items.run_id` is `on delete set null`, so deleting the agent can leave
/// a card holding a live token with nothing to approve. `DELETE /agents/{id}`
/// settles those first; this is the race where it commits between the client's
/// read and its tap.
#[tokio::test]
async fn approving_a_card_whose_agent_was_deleted_is_410_gone() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;

    sqlx::query("update feed_items set run_id = null")
        .execute(&w.app.db.pool)
        .await
        .unwrap();

    let response = post_json_as(
        &w.app,
        &format!("/v1/feed/{}/approve", card["id"].as_str().unwrap()),
        &json!({"approval_token": card["approval_token"]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 410);
    assert_eq!(response_json(response).await, fixture("error_gone.json"));
    // And the card is settled rather than left holding a token for ever.
    assert_eq!(feed(&w).await["items"][0]["status"], "skipped");
    assert_eq!(feed(&w).await["new_count"], 0);
}

#[tokio::test]
async fn deleting_an_agent_settles_its_live_cards() {
    let w = world("write_note", true).await;
    park(&w, "write_note", NOTE_ARGS()).await;

    let response = send(
        &w.app.router,
        "DELETE",
        &format!("/v1/agents/{}", w.agent),
        Some(&w.token),
        None,
    )
    .await;
    assert_eq!(response.status(), 204);

    let page = feed(&w).await;
    assert_eq!(page["items"][0]["status"], "skipped");
    assert_eq!(
        page["items"][0]["resolved_note"],
        "The agent was deleted, so this was never saved."
    );
    assert_eq!(page["items"][0]["approval_token"], json!(null));
    assert_eq!(page["new_count"], 0, "the badge cannot be stuck above zero");
}

// ---------------------------------------------------------- the reader --

#[tokio::test]
async fn the_deep_link_returns_the_same_item_as_the_list() {
    let w = world("write_note", true).await;
    let card = park(&w, "write_note", NOTE_ARGS()).await;
    let single = response_json(
        crate::test_support::get(
            &w.app,
            &format!("/v1/feed/{}", card["id"].as_str().unwrap()),
            Some(&w.token),
        )
        .await,
    )
    .await;
    assert_eq!(
        single, card,
        "`GET /feed/{{id}}` returns the same item shape"
    );
}

#[tokio::test]
async fn an_unknown_card_is_404_and_a_corrupt_cursor_is_400() {
    let w = world("write_note", true).await;
    let missing = crate::test_support::get(
        &w.app,
        &format!("/v1/feed/{}", Uuid::new_v4()),
        Some(&w.token),
    )
    .await;
    assert_eq!(missing.status(), 404);

    let bad =
        crate::test_support::get(&w.app, "/v1/feed?cursor=not-a-cursor", Some(&w.token)).await;
    assert_eq!(bad.status(), 400, "never a silent reset to page one");
}

#[tokio::test]
async fn an_empty_feed_matches_the_fixture() {
    let w = world("write_note", true).await;
    let page = feed(&w).await;
    assert_eq!(page, fixture("feed_empty.json"));
}

/// EDGE (pagination boundary): a card inserted mid-scroll can neither duplicate
/// a row nor skip one, which is what the keyset cursor is for (D52).
#[tokio::test]
async fn the_feed_pages_by_keyset_and_a_new_card_cannot_duplicate_a_row() {
    let w = world("write_note", true).await;
    for i in 0..55 {
        sqlx::query(
            "insert into feed_items (account_id, kind, title, body, data, status, created_at) \
             values ($1, 'info', 'Agent', 'body', $2::jsonb, 'new', \
                     now() - make_interval(mins => $3::int))",
        )
        .bind(w.account)
        .bind(crate::agents::feed::info_data(None, None, None))
        .bind(i)
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    }

    let first = feed(&w).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    assert!(first["next_cursor"].is_string());
    assert_eq!(
        first["new_count"], 55,
        "the badge is the mailbox, not the page"
    );

    // A card arrives between the two requests.
    sqlx::query(
        "insert into feed_items (account_id, kind, title, body, data, status) \
         values ($1, 'info', 'Agent', 'interloper', $2::jsonb, 'new')",
    )
    .bind(w.account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    let second = response_json(
        crate::test_support::get(
            &w.app,
            &format!("/v1/feed?cursor={}", first["next_cursor"].as_str().unwrap()),
            Some(&w.token),
        )
        .await,
    )
    .await;
    assert_eq!(second["items"].as_array().unwrap().len(), 5);
    assert_eq!(second["next_cursor"], json!(null), "the last page");

    let mut ids: Vec<&str> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["items"].as_array().unwrap())
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "no row appeared twice");
}

// ------------------------------------------------------------- /feed/seen --

#[tokio::test]
async fn seen_resolves_info_items_and_ignores_everything_else() {
    let w = world("write_note", true).await;
    let approval = park(&w, "write_note", NOTE_ARGS()).await;
    let info: Uuid = sqlx::query_scalar(
        "insert into feed_items (account_id, kind, title, body, data, status) \
         values ($1, 'info', 'Agent', 'Saved a note.', $2::jsonb, 'new') returning id",
    )
    .bind(w.account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();

    assert_eq!(feed(&w).await["new_count"], 2);

    let response = post_json_as(
        &w.app,
        "/v1/feed/seen",
        // An approval, an unknown id, and the info item. Only the last moves.
        &json!({"ids": [info, approval["id"], Uuid::new_v4()]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response_json(response).await["new_count"], 1);
    assert_eq!(feed(&w).await["new_count"], 1);

    // EDGE (empty input): a best-effort receipt with nothing to report.
    let empty = post_json_as(&w.app, "/v1/feed/seen", &json!({"ids": []}), Some(&w.token)).await;
    assert_eq!(response_json(empty).await["new_count"], 1);
}

/// The needs-reauth card is the only surface that says sync has stopped, and
/// `save_consent`'s reconciler only matches `status = 'new'` — so a receipt
/// fired by scrolling past it would clear it for ever.
#[tokio::test]
async fn seen_cannot_dismiss_the_card_that_says_gmail_is_dead() {
    let w = world("write_note", true).await;
    let sticky: Uuid = sqlx::query_scalar(
        "insert into feed_items \
             (account_id, kind, title, body, data, status, reason, dismissible) \
         values ($1, 'info', 'Gmail', 'NADE lost access to your Gmail.', $2::jsonb, \
                 'new', 'needs_reauth', false) returning id",
    )
    .bind(w.account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();

    let response = post_json_as(
        &w.app,
        "/v1/feed/seen",
        &json!({"ids": [sticky]}),
        Some(&w.token),
    )
    .await;
    assert_eq!(response_json(response).await["new_count"], 1);
    let status: String = sqlx::query_scalar("select status from feed_items where id = $1")
        .bind(sticky)
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(status, "new");
}

// ------------------------------------------- the un-gated `info` card --

#[tokio::test]
async fn an_agent_that_needs_no_approval_still_says_what_it_did() {
    let w = world("write_note", false).await;
    script(
        &w.server,
        tool_turn(
            "write_note",
            NOTE_ARGS(),
            Some("Two next steps found — an intro and a portfolio session."),
        ),
        text_turn("Saved the next steps."),
    )
    .await;
    drive(&w, crate::agents::run::KIND, json!({"run_id": w.run}))
        .await
        .expect("the run job");

    assert_eq!(status_of(&w).await, "done");
    let page = feed(&w).await;
    assert_eq!(page["items"][0]["kind"], "info");
    assert_eq!(page["items"][0]["actions"], json!([]));
    assert_eq!(page["items"][0]["approval_token"], json!(null));
    assert_eq!(page["items"][0]["approval_expires_at"], json!(null));
    assert_eq!(page["items"][0]["resolved_note"], json!(null));
    assert_eq!(page["items"][0]["data"]["action"], "none");
    assert!(page["items"][0]["data"]["note_id"].is_string());
}

#[tokio::test]
async fn a_run_that_wrote_nothing_raises_no_card() {
    let w = world("read_thread", false).await;
    script(&w.server, text_turn("Nothing to do."), text_turn("done")).await;
    drive(&w, crate::agents::run::KIND, json!({"run_id": w.run}))
        .await
        .expect("the run job");

    assert_eq!(status_of(&w).await, "done");
    assert_eq!(feed(&w).await, fixture("feed_empty.json"));
}

// -------------------------------------------------- the system's cards --

/// **The regression guard for a bug that had already shipped.**
///
/// `agents::run::raise_spend_ceiling_notice` and `gmail::oauth`'s needs-reauth
/// notice both wrote `data.reason`, a fifth key `FEED_DATA`'s exact key set
/// forbids, and the reconnect path set `resolved_note` on a `kind: "info"`
/// card, which `API.md` §7 forbids outright. Neither could be noticed while
/// `/feed` was unmounted: no test served them, and `validate.py` only ever saw
/// the generated fixtures. This drives the **real writers** and checks what
/// `GET /feed` actually returns.
#[tokio::test]
async fn every_card_the_system_raises_for_itself_is_contract_shaped() {
    let w = world("write_note", true).await;

    crate::agents::run::raise_spend_ceiling_notice(&w.app.db.pool, w.account).await;
    sqlx::query(
        "insert into feed_items \
             (account_id, kind, title, body, data, status, reason, dismissible) \
         values ($1, 'info', 'Gmail', 'NADE lost access to your Gmail.', $2::jsonb, \
                 'new', 'needs_reauth', false)",
    )
    .bind(w.account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    let shape = fixture("feed_item_info.json");
    for item in feed(&w).await["items"].as_array().unwrap() {
        // Exactly `API.md` §7.1's third shape — no more keys, no fewer. The
        // crate's own comparator rather than a hand-rolled key-set diff: this
        // one recurses and checks *types*, so a `note_id` that had become a
        // number would fail here instead of passing.
        crate::api::contract_tests::assert_same_shape(
            &item["data"],
            &shape["data"],
            "feed_item_info.data",
        );
        assert_eq!(item["data"]["action"], "none", "{item}");

        // §7: an `info` item has no deadline, no buttons, no token, and no
        // italic line.
        assert_eq!(item["approval_expires_at"], json!(null), "{item}");
        assert_eq!(item["approval_token"], json!(null), "{item}");
        assert_eq!(item["actions"], json!([]), "{item}");
        assert_eq!(item["resolved_note"], json!(null), "{item}");
        assert!(
            item["body"].as_str().is_some_and(|b| !b.is_empty()),
            "{item}"
        );
    }

    // EDGE (duplicate delivery): a second run hitting the same ceiling on the
    // same day must not raise a second card.
    crate::agents::run::raise_spend_ceiling_notice(&w.app.db.pool, w.account).await;
    assert_eq!(feed(&w).await["items"].as_array().unwrap().len(), 2);
}

/// Re-consent resolves the card and writes **no** italic line: `API.md` §7 says
/// `resolved_note` is null for every `info` item, and the card resolving is the
/// message — the user has just come back from Google's consent screen.
#[tokio::test]
async fn reconnecting_resolves_the_gmail_card_without_a_note() {
    let w = world("write_note", true).await;
    sqlx::query(
        "insert into feed_items \
             (account_id, kind, title, body, data, status, reason, dismissible) \
         values ($1, 'info', 'Gmail', 'NADE lost access.', $2::jsonb, 'new', \
                 'needs_reauth', false)",
    )
    .bind(w.account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    // Exactly what `TokenStore::save_consent` runs.
    sqlx::query(
        "update feed_items set status = 'resolved' \
          where account_id = $1 and kind = 'info' and status = 'new' \
            and reason = 'needs_reauth'",
    )
    .bind(w.account)
    .execute(&w.app.db.pool)
    .await
    .unwrap();

    let item = &feed(&w).await["items"][0];
    assert_eq!(item["status"], "resolved");
    assert_eq!(item["resolved_note"], json!(null));
}

// ---------------------------------------------------------- the plan --

/// D13's rule, applied to the feed: an index the query cannot use is not an
/// index. `thread_list_query_uses_an_index` is the precedent; the failure it
/// prevents is a `Sort` node over the whole table on the home screen.
#[tokio::test]
async fn the_feed_query_walks_an_index_and_never_sorts() {
    let w = world("write_note", true).await;
    sqlx::query(
        "insert into feed_items (account_id, kind, title, body, data, status, created_at) \
         select $1, 'info', 'Agent', 'body', $2::jsonb, 'new', \
                now() - (g || ' seconds')::interval \
           from generate_series(1, 20000) g",
    )
    .bind(w.account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .execute(&w.app.db.pool)
    .await
    .unwrap();
    sqlx::query("analyze feed_items")
        .execute(&w.app.db.pool)
        .await
        .unwrap();

    let plan: Value = sqlx::query_scalar(
        "explain (format json) \
         select id, account_id, kind, title, body, status, run_id, approval_token, \
                approval_expires_at, resolved_note, data, step_seq, created_at \
           from feed_items \
          where account_id = $1 \
            and ($2::timestamptz is null \
                 or (created_at, id) < ($2::timestamptz, $3::uuid)) \
          order by created_at desc, id desc \
          limit $4",
    )
    .bind(w.account)
    .bind(Option::<chrono::DateTime<chrono::Utc>>::None)
    .bind(Option::<Uuid>::None)
    .bind(51i64)
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();

    let rendered = plan.to_string();
    println!("feed EXPLAIN: {rendered}");
    assert!(
        rendered.contains("feed_items_account_created_idx"),
        "the feed must walk its own keyset index: {rendered}"
    );
    assert!(!rendered.contains("Seq Scan"), "{rendered}");
    assert!(
        !rendered.contains("\"Sort\""),
        "the index provides the order; a Sort node means it did not: {rendered}"
    );
}
