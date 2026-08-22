//! `/agents`, end to end through the router.

use serde_json::{json, Value};
use uuid::Uuid;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::config::Env;
use crate::test_support::{get, response_json, send, test_app, TestApp};

const SENTENCE: &str =
    "When a recruiter emails about a tech role, save the next steps as a note. Ask me first.";

struct Ctx {
    app: TestApp,
    token: String,
    account: Uuid,
    server: MockServer,
}

fn emitted() -> Value {
    json!({
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
    })
}

fn compile_answer(input: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "claude-haiku-4-5-20251001",
        "stop_reason": "tool_use",
        "content": [{"type": "tool_use", "id": "t1", "name": "emit_agent", "input": input}],
        "usage": {"input_tokens": 100, "output_tokens": 40}
    }))
}

/// Exactly one account, so `Auth`'s sole-account fallback (D45) resolves it for
/// a device that was never bound.
async fn ctx() -> Ctx {
    let server = MockServer::start().await;
    let mut app = test_app(Env::Dev).await;
    app.set_llm_base(&server.uri());
    let token = app.device_token().await;
    let account: Uuid = sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind("owner@example.com")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    sqlx::query("insert into settings (account_id) values ($1) on conflict do nothing")
        .bind(account)
        .execute(&app.db.pool)
        .await
        .unwrap();
    Ctx {
        app,
        token,
        account,
        server,
    }
}

async fn mount_compile(ctx: &Ctx, input: Value) {
    Mock::given(method("POST"))
        .and(wm_path("/v1/messages"))
        .respond_with(compile_answer(input))
        .mount(&ctx.server)
        .await;
}

async fn post(ctx: &Ctx, path: &str, body: &Value) -> axum::http::Response<axum::body::Body> {
    send(
        &ctx.app.router,
        "POST",
        path,
        Some(&ctx.token),
        Some(("application/json", body.to_string())),
    )
    .await
}

async fn patch(ctx: &Ctx, path: &str, body: &Value) -> axum::http::Response<axum::body::Body> {
    send(
        &ctx.app.router,
        "PATCH",
        path,
        Some(&ctx.token),
        Some(("application/json", body.to_string())),
    )
    .await
}

async fn create(ctx: &Ctx) -> Value {
    let response = post(ctx, "/v1/agents", &json!({"nl_definition": SENTENCE})).await;
    assert_eq!(response.status(), 200);
    response_json(response).await
}

// -------------------------------------------------------------- create --

#[tokio::test]
async fn a_created_agent_is_always_a_draft_and_carries_its_spans() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;

    let agent = create(&ctx).await;
    // `API.md` §5: "the client cannot ask for anything else, and a draft never
    // runs". Enforced server-side rather than trusted to the caller.
    assert_eq!(agent["status"], json!("draft"));
    assert_eq!(agent["name"], json!("Job Search Tracker"));
    assert_eq!(
        agent["when_span"],
        json!("a recruiter emails about a tech role")
    );
    assert_eq!(agent["do_span"], json!("save the next steps as a note"));
    assert_eq!(agent["trailing"], json!("Ask me first."));
    assert_eq!(agent["compile_error"], Value::Null);
    assert_eq!(agent["trigger_summary"], json!("On new mail"));
    assert_eq!(agent["allowed_tools"], json!(["read_thread", "write_note"]));
    assert_eq!(agent["last_run_at"], Value::Null);
    assert!(agent["spec"].is_object());
}

#[tokio::test]
async fn a_client_cannot_ask_for_a_published_agent() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    // Extra fields are ignored rather than honoured - `status` is a literal in
    // the insert, not a parameter.
    let response = post(
        &ctx,
        "/v1/agents",
        &json!({"nl_definition": SENTENCE, "status": "published"}),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response_json(response).await["status"], json!("draft"));
}

#[tokio::test]
async fn a_compile_failure_still_creates_the_agent_and_never_returns_5xx() {
    // `API.md` §5: "the agent is still created as a draft with `spec: null` and
    // `compile_error` set, so the user's sentence is never lost".
    let ctx = ctx().await;
    Mock::given(method("POST"))
        .and(wm_path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .mount(&ctx.server)
        .await;

    let response = post(&ctx, "/v1/agents", &json!({"nl_definition": SENTENCE})).await;
    assert_eq!(
        response.status(),
        200,
        "a compile failure is not an HTTP failure"
    );
    let agent = response_json(response).await;
    assert_eq!(agent["status"], json!("draft"));
    assert_eq!(agent["spec"], Value::Null);
    assert!(agent["compile_error"].is_string());
    // The invariant `validate.py` enforces: with a null spec, all three spans
    // are null too.
    for field in ["when_span", "do_span", "trailing"] {
        assert_eq!(agent[field], Value::Null, "{field}");
    }
    // And the sentence itself survived, which is the whole point.
    assert_eq!(agent["nl_definition"], json!(SENTENCE));
}

#[tokio::test]
async fn an_empty_or_oversized_sentence_is_a_bad_request() {
    // EDGE: empty input, and `API.md` §0's 4 000-character cap.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    for body in [
        json!({"nl_definition": ""}),
        json!({"nl_definition": "   "}),
        json!({"nl_definition": "x".repeat(4_001)}),
        json!({}),
    ] {
        let response = post(&ctx, "/v1/agents", &body).await;
        assert_eq!(response.status(), 400, "{body}");
    }
}

#[tokio::test]
async fn a_sentence_at_exactly_the_cap_is_accepted() {
    // The boundary, in characters as the contract states - not bytes.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let sentence = "\u{00e9}".repeat(4_000);
    let response = post(&ctx, "/v1/agents", &json!({"nl_definition": sentence})).await;
    assert_eq!(response.status(), 200);
}

// ---------------------------------------------------------------- read --

#[tokio::test]
async fn the_list_is_oldest_first_and_carries_no_cursor_field() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let first = create(&ctx).await;
    let second = create(&ctx).await;

    let body = response_json(get(&ctx.app, "/v1/agents", Some(&ctx.token)).await).await;
    let ids: Vec<&str> = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            first["id"].as_str().unwrap(),
            second["id"].as_str().unwrap()
        ]
    );
    // `API.md` §0: a bounded collection carries no cursor field at all,
    // "because inventing one implies a page boundary that will never exist".
    assert!(body.get("next_cursor").is_none(), "{body}");
}

#[tokio::test]
async fn the_list_row_carries_no_detail_only_fields() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    create(&ctx).await;
    let body = response_json(get(&ctx.app, "/v1/agents", Some(&ctx.token)).await).await;
    let row = &body["agents"][0];
    let mut keys: Vec<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "approval_required",
            "id",
            "last_run_at",
            "name",
            "nl_definition",
            "schedule",
            "status",
            "trigger_summary"
        ]
    );
}

#[tokio::test]
async fn an_agent_that_is_not_this_accounts_is_a_404_and_not_a_403() {
    let ctx = ctx().await;
    let response = get(
        &ctx.app,
        &format!("/v1/agents/{}", Uuid::new_v4()),
        Some(&ctx.token),
    )
    .await;
    // 404, so the caller learns nothing about what exists.
    assert_eq!(response.status(), 404);
    assert_eq!(
        response_json(response).await["error"]["code"],
        json!("not_found")
    );
}

// --------------------------------------------------------------- patch --

#[tokio::test]
async fn publishing_a_compiled_agent_works_and_publishing_a_broken_one_does_not() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let agent = create(&ctx).await;
    let id = agent["id"].as_str().unwrap();

    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"status": "published"}),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response_json(response).await["status"], json!("published"));

    // An agent with no spec has no instruction, so a run would do nothing at
    // all. `validate.py` requires it to stay a draft.
    sqlx::query("update agents set spec = null, compile_error = 'broken' where id = $1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(&ctx.app.db.pool)
        .await
        .unwrap();
    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"status": "published"}),
    )
    .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn an_empty_patch_is_a_bad_request() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    let response = patch(&ctx, &format!("/v1/agents/{id}"), &json!({})).await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn a_patch_touches_only_the_fields_it_names() {
    // Two concurrent patches of different fields must not undo each other - the
    // exact defect P3's iOS review found, where both writers derived from one
    // pre-edit object.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"approval_required": false}),
    )
    .await;
    let after = response_json(
        patch(
            &ctx,
            &format!("/v1/agents/{id}"),
            &json!({"status": "published"}),
        )
        .await,
    )
    .await;
    assert_eq!(
        after["approval_required"],
        json!(false),
        "the first patch was undone"
    );
    assert_eq!(after["status"], json!("published"));
    assert_eq!(
        after["name"],
        json!("Job Search Tracker"),
        "an untouched field moved"
    );
}

#[tokio::test]
async fn changing_the_sentence_recompiles_the_spec() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let new_sentence = "When a bill arrives, save a note about it.";
    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"nl_definition": new_sentence}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = response_json(response).await;
    assert_eq!(body["nl_definition"], json!(new_sentence));
    // The mock still answers with the old spans, which do not appear in the new
    // sentence - so the recompile is rejected and recorded, and the sentence is
    // still kept. That is the contract working, not a test artefact.
    assert_eq!(body["spec"], Value::Null);
    assert!(body["compile_error"].is_string());
    assert_eq!(
        body["status"],
        json!("draft"),
        "a null spec forces the status back"
    );
}

#[tokio::test]
async fn a_successful_recompile_replaces_the_spec_and_clears_the_error() {
    // The other half of `changing_the_sentence_recompiles_the_spec`, which only
    // exercises the failure branch. All five derived columns move together in
    // one `case when` statement, and the invariant they must land on is
    // "spec null XOR compile_error set" - so the success path needs its own
    // test or half of that statement is never run.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    // Break it first, so there is a compile_error for the success to clear.
    sqlx::query(
        "update agents set spec = null, compile_error = 'stale failure', \
                when_span = null, do_span = null where id = $1",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    // The same sentence the mock's spans come from, so the recompile succeeds.
    let body = response_json(
        patch(
            &ctx,
            &format!("/v1/agents/{id}"),
            &json!({"nl_definition": SENTENCE}),
        )
        .await,
    )
    .await;

    assert!(body["spec"].is_object(), "the new spec was not stored");
    assert_eq!(
        body["compile_error"],
        Value::Null,
        "the old error outlived its spec"
    );
    assert_eq!(
        body["when_span"],
        json!("a recruiter emails about a tech role")
    );
    assert_eq!(body["do_span"], json!("save the next steps as a note"));
    assert_eq!(body["trailing"], json!("Ask me first."));
    assert_eq!(body["nl_definition"], json!(SENTENCE));
}

#[tokio::test]
async fn a_patch_that_does_not_touch_the_sentence_leaves_the_spec_alone() {
    // The `case when $recompiling` guard, from the other direction: a status
    // change must not blank the spec.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let created = create(&ctx).await;
    let id = created["id"].as_str().unwrap().to_owned();

    let after = response_json(
        patch(
            &ctx,
            &format!("/v1/agents/{id}"),
            &json!({"status": "published"}),
        )
        .await,
    )
    .await;
    assert_eq!(after["spec"], created["spec"]);
    assert_eq!(after["when_span"], created["when_span"]);
    assert_eq!(after["trailing"], created["trailing"]);
}

#[tokio::test]
async fn a_tool_this_version_does_not_have_is_refused() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"allowed_tools": ["read_thread", "send_email"]}),
    )
    .await;
    assert_eq!(response.status(), 400);
}

// -------------------------------------------------------------- delete --

#[tokio::test]
async fn deleting_an_agent_answers_204_and_takes_its_runs_with_it() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    let uuid = Uuid::parse_str(&id).unwrap();

    sqlx::query(
        "insert into agent_runs (agent_id, account_id, trigger_kind) values ($1, $2, 'manual')",
    )
    .bind(uuid)
    .bind(ctx.account)
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let response = send(
        &ctx.app.router,
        "DELETE",
        &format!("/v1/agents/{id}"),
        Some(&ctx.token),
        None,
    )
    .await;
    assert_eq!(response.status(), 204);

    let agents: i64 = sqlx::query_scalar("select count(*) from agents where id = $1")
        .bind(uuid)
        .fetch_one(&ctx.app.db.pool)
        .await
        .unwrap();
    assert_eq!(agents, 0);
}

#[tokio::test]
async fn deleting_an_agent_that_is_already_gone_is_a_404() {
    let ctx = ctx().await;
    let response = send(
        &ctx.app.router,
        "DELETE",
        &format!("/v1/agents/{}", Uuid::new_v4()),
        Some(&ctx.token),
        None,
    )
    .await;
    assert_eq!(response.status(), 404);
}

// ------------------------------------------------------------- run now --

#[tokio::test]
async fn run_now_queues_a_manual_run_even_for_a_draft() {
    // `API.md` §6: "Runs a `draft` agent too - that is what the builder's
    // 'Run once now' is for."
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let response = post(&ctx, &format!("/v1/agents/{id}/run"), &json!({})).await;
    assert_eq!(response.status(), 200);
    let run_id = response_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, kind): (String, String) =
        sqlx::query_as("select status, trigger_kind from agent_runs where id = $1")
            .bind(Uuid::parse_str(&run_id).unwrap())
            .fetch_one(&ctx.app.db.pool)
            .await
            .unwrap();
    assert_eq!(status, "queued");
    assert_eq!(kind, "manual");

    // And a job to drive it, deduplicated on the run.
    let jobs: i64 = sqlx::query_scalar(
        "select count(*) from jobs where kind = 'run_agent' and dedupe_key = $1",
    )
    .bind(format!("run_agent:{run_id}"))
    .fetch_one(&ctx.app.db.pool)
    .await
    .unwrap();
    assert_eq!(jobs, 1);
}

#[tokio::test]
async fn an_agent_that_never_compiled_cannot_be_run() {
    let ctx = ctx().await;
    Mock::given(method("POST"))
        .and(wm_path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ctx.server)
        .await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    let response = post(&ctx, &format!("/v1/agents/{id}/run"), &json!({})).await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn run_now_refuses_once_the_daily_budget_is_gone() {
    // The pre-flight lives here rather than in the job on purpose: refusing in
    // the handler means no run row is created, while refusing in the job would
    // strand a `queued` run with an empty journal - and `Engine::cancel`
    // refuses one of those outright, so there would be no legal way to end it.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    sqlx::query(
        "insert into llm_calls (account_id, purpose, model, cost_usd, ok) \
         values ($1, 'run', 'm', 5.0, true)",
    )
    .bind(ctx.account)
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let response = post(&ctx, &format!("/v1/agents/{id}/run"), &json!({})).await;
    assert_eq!(response.status(), 429);
    assert_eq!(
        response_json(response).await["error"]["code"],
        json!("rate_limited")
    );

    let runs: i64 = sqlx::query_scalar("select count(*) from agent_runs")
        .fetch_one(&ctx.app.db.pool)
        .await
        .unwrap();
    assert_eq!(runs, 0, "no run row may be created when the budget is gone");
}

#[tokio::test]
async fn a_server_with_no_model_reports_upstream_unavailable_rather_than_a_500() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let mut app = ctx.app;
    app.clear_llm_key();
    let response = send(
        &app.router,
        "POST",
        &format!("/v1/agents/{id}/run"),
        Some(&ctx.token),
        Some(("application/json", "{}".to_owned())),
    )
    .await;
    assert_eq!(response.status(), 502);
    assert_eq!(
        response_json(response).await["error"]["code"],
        json!("upstream_unavailable")
    );
}

#[tokio::test]
async fn every_agent_route_is_behind_the_bearer_guard() {
    let ctx = ctx().await;
    let id = Uuid::new_v4();
    for (verb, path) in [
        ("GET", "/v1/agents".to_owned()),
        ("POST", "/v1/agents".to_owned()),
        ("GET", format!("/v1/agents/{id}")),
        ("PATCH", format!("/v1/agents/{id}")),
        ("DELETE", format!("/v1/agents/{id}")),
        ("POST", format!("/v1/agents/{id}/run")),
    ] {
        let response = send(&ctx.app.router, verb, &path, None, None).await;
        assert_eq!(response.status(), 401, "{verb} {path} is not guarded");
    }
}

#[test]
fn trigger_summary_matches_every_agent_fixture() {
    // The guard that was missing. Every agent assertion in `contract_tests.rs`
    // used `shape_of` - key sets and JSON types - so the *string* was never
    // compared, and the server said "Not set up" and "Every week at 08:00"
    // while the fixtures (and therefore the app, and the design screenshots)
    // said "Not set" and "Every weekday at 08:00". Both lanes green, both
    // wrong.
    //
    // Only the **full** fixtures can drive the renderer: a list row carries no
    // `spec` by design (`API.md` §5), which is exactly why the list is checked
    // against them rather than fed through it.
    let full = [
        "agent.json",
        "agent_draft.json",
        "agent_scheduled.json",
        "agent_compile_failed.json",
    ];
    let mut by_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for name in full {
        let agent = crate::test_support::fixture(name);
        let expected = agent["trigger_summary"].as_str().expect("trigger_summary");
        let spec = agent.get("spec").filter(|v| !v.is_null());
        let schedule = agent.get("schedule").filter(|v| !v.is_null());
        assert_eq!(
            super::trigger_summary(spec, schedule),
            expected,
            "{name}: {} renders wrongly",
            agent["name"]
        );
        by_id.insert(
            agent["id"].as_str().unwrap().to_owned(),
            expected.to_owned(),
        );
    }

    // And the list agrees with the detail, agent for agent - the two are
    // rendered by one function, so a divergence would mean the fixtures
    // disagree with each other.
    let list = crate::test_support::fixture("agents.json");
    for row in list["agents"].as_array().expect("agents") {
        let id = row["id"].as_str().unwrap();
        let Some(expected) = by_id.get(id) else {
            continue;
        };
        assert_eq!(
            row["trigger_summary"].as_str().unwrap(),
            expected,
            "the list and the full object disagree for {}",
            row["name"]
        );
    }
    assert_eq!(
        by_id.len(),
        full.len(),
        "a full agent fixture went unchecked"
    );
}

#[test]
fn the_schedule_renderer_covers_the_shapes_the_contract_allows() {
    use serde_json::json;
    let at = |freq: &str, days: Value| {
        super::schedule_summary(Some(
            &json!({"freq": freq, "at": "08:00", "byweekday": days}),
        ))
    };
    assert_eq!(
        at("week", json!(["mon", "tue", "wed", "thu", "fri"])),
        "Every weekday at 08:00"
    );
    assert_eq!(at("week", json!(["sat"])), "Every Saturday at 08:00");
    assert_eq!(at("week", json!(["mon", "wed"])), "Every week at 08:00");
    assert_eq!(at("day", json!([])), "Every day at 08:00");
    assert_eq!(at("month", json!([])), "Every month at 08:00");
    // EDGE: a schedule that is absent, or malformed enough to render nothing
    // honest, still produces a sentence rather than an empty cell.
    assert_eq!(super::schedule_summary(None), "On a schedule");
    assert_eq!(super::schedule_summary(Some(&json!({}))), "On a schedule");
    assert_eq!(
        super::schedule_summary(Some(&json!({"freq": "week"}))),
        "Every week"
    );
}

// ------------------------------------------------------------ schedules --

#[tokio::test]
async fn a_schedule_cannot_be_attached_to_an_agent_that_is_not_on_one() {
    // `validate.py` and the contract tests both enforce "a schedule trigger and
    // a schedule imply each other", and `compile.rs` refuses to compile a
    // schedule trigger for that reason - so `PATCH` must not walk around it.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"schedule": {
            "freq": "week", "interval": 1, "byweekday": ["mon"],
            "at": "08:00", "tz": "America/Phoenix",
            "ends": {"kind": "never"}, "runs_done": 0
        }}),
    )
    .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn a_malformed_schedule_is_refused_rather_than_stored() {
    // Stored raw, one bad schedule fails the decode of the **whole**
    // `GET /agents` body on the app side - every agent disappears, not just the
    // broken one.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    // Give it a schedule trigger so the refusal is the schedule's, not the
    // trigger's.
    sqlx::query(
        "update agents set spec = jsonb_set(spec, '{trigger,kind}', '\"schedule\"') where id = $1",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let good = json!({
        "freq": "week", "interval": 1, "byweekday": ["mon", "tue", "wed", "thu", "fri"],
        "at": "08:00", "tz": "America/Phoenix", "ends": {"kind": "never"}, "runs_done": 0
    });
    let mut bad = Vec::new();
    for (field, value) in [
        ("freq", json!("fortnight")),
        ("interval", json!(0)),
        ("at", json!("25:00")),
        ("at", json!("8:00")),
        ("tz", json!("Mars/Olympus")),
        ("byweekday", json!(["funday"])),
        ("ends", json!({"kind": "after"})),
        ("ends", json!({"kind": "on"})),
        ("ends", json!({"kind": "whenever"})),
    ] {
        let mut schedule = good.clone();
        schedule[field] = value;
        bad.push(schedule);
    }
    bad.push(json!(42));
    bad.push(json!("a schedule"));
    // `bymonthday` on a weekly schedule, and out of §5.2's 1..28 range.
    let mut monthly = good.clone();
    monthly["bymonthday"] = json!(31);
    bad.push(monthly);

    for schedule in bad {
        let response = patch(
            &ctx,
            &format!("/v1/agents/{id}"),
            &json!({"schedule": schedule}),
        )
        .await;
        assert_eq!(response.status(), 400, "accepted {schedule}");
    }

    // And the good one is accepted, so the test is not passing vacuously.
    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"schedule": good}),
    )
    .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn runs_done_is_server_maintained_and_a_client_cannot_reset_it() {
    // §5.2: "server-maintained, read-only". `ends.after.count` is compared
    // against it, so a client that could zero it could run an agent forever.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    let uuid = Uuid::parse_str(&id).unwrap();
    sqlx::query(
        "update agents set spec = jsonb_set(spec, '{trigger,kind}', '\"schedule\"'), \
                        schedule = $2 where id = $1",
    )
    .bind(uuid)
    .bind(json!({
        "freq": "day", "interval": 1, "byweekday": [], "bymonthday": null,
        "at": "08:00", "tz": "America/Phoenix",
        "ends": {"kind": "after", "date": null, "count": 10}, "runs_done": 7
    }))
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let body = response_json(
        patch(
            &ctx,
            &format!("/v1/agents/{id}"),
            &json!({"schedule": {
                "freq": "day", "interval": 1, "byweekday": [],
                "at": "09:00", "tz": "America/Phoenix",
                "ends": {"kind": "after", "count": 10},
                "runs_done": 0
            }}),
        )
        .await,
    )
    .await;
    assert_eq!(
        body["schedule"]["at"],
        json!("09:00"),
        "the edit did not apply"
    );
    assert_eq!(
        body["schedule"]["runs_done"],
        json!(7),
        "a client reset the counter `ends.after` is measured against"
    );
}

// ----------------------------------------------------------- invariants --

#[tokio::test]
async fn allowed_tools_cannot_be_narrowed_below_what_the_spec_needs() {
    // `API.md` §5.1's own invariant. Narrowing it alone leaves a row that
    // `validate.py` and the contract tests both reject.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"allowed_tools": ["read_thread"]}),
    )
    .await;
    assert_eq!(response.status(), 400, "spec.tools needs write_note too");

    // Widening is fine.
    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"allowed_tools": ["read_thread", "write_note", "search_mail"]}),
    )
    .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_patch_may_fix_the_sentence_and_publish_in_one_request() {
    // The status check reads the spec that will be **stored**. Reading the
    // pre-patch spec refused a request that repairs and publishes at once.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    sqlx::query(
        "update agents set spec = null, compile_error = 'was broken', \
                        when_span = null, do_span = null where id = $1",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let body = response_json(
        patch(
            &ctx,
            &format!("/v1/agents/{id}"),
            &json!({"nl_definition": SENTENCE, "status": "published"}),
        )
        .await,
    )
    .await;
    assert_eq!(body["status"], json!("published"));
    assert!(body["spec"].is_object());
    assert_eq!(body["compile_error"], Value::Null);
}

#[tokio::test]
async fn the_over_budget_refusal_says_when_to_come_back() {
    // `API.md` §0 makes `Retry-After` part of what `rate_limited` means, and
    // the app already reads it.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();
    sqlx::query(
        "insert into llm_calls (account_id, purpose, model, cost_usd, ok) \
         values ($1, 'run', 'm', 5.0, true)",
    )
    .bind(ctx.account)
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let response = post(&ctx, &format!("/v1/agents/{id}/run"), &json!({})).await;
    assert_eq!(response.status(), 429);
    let header = response
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .expect("Retry-After must be set on a rate_limited response");
    assert!(
        header > 0 && header <= 24 * 3600,
        "implausible Retry-After: {header}"
    );
}

#[tokio::test]
async fn creating_an_agent_is_refused_once_the_budget_is_gone() {
    // The compile path spends real money and had no ceiling at all: an
    // authenticated caller could loop `POST /agents` without limit.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    sqlx::query(
        "insert into llm_calls (account_id, purpose, model, cost_usd, ok) \
         values ($1, 'compile', 'm', 5.0, true)",
    )
    .bind(ctx.account)
    .execute(&ctx.app.db.pool)
    .await
    .unwrap();

    let response = post(&ctx, "/v1/agents", &json!({"nl_definition": SENTENCE})).await;
    assert_eq!(response.status(), 429);
    // And no agent row was created to carry a bogus compile_error.
    let agents: i64 = sqlx::query_scalar("select count(*) from agents")
        .fetch_one(&ctx.app.db.pool)
        .await
        .unwrap();
    assert_eq!(agents, 0);
    // Nor was the model called.
    assert_eq!(ctx.server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_run_input_with_a_nul_does_not_strand_a_queued_run() {
    // The row and the job commit together now, and the input is stripped - so
    // neither half can fail and leave a `queued` run that nothing will move and
    // `Engine::cancel` refuses to end.
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let hostile = format!("look at{}this", '\u{0000}');
    let response = post(
        &ctx,
        &format!("/v1/agents/{id}/run"),
        &json!({"input": hostile}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let run_id = response_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let jobs: i64 = sqlx::query_scalar(
        "select count(*) from jobs where kind = 'run_agent' and dedupe_key = $1",
    )
    .bind(format!("run_agent:{run_id}"))
    .fetch_one(&ctx.app.db.pool)
    .await
    .unwrap();
    assert_eq!(jobs, 1, "the run has no job to drive it");
}

#[tokio::test]
async fn an_enormous_run_input_is_capped_before_it_reaches_the_prompt() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let id = create(&ctx).await["id"].as_str().unwrap().to_owned();

    let response = post(
        &ctx,
        &format!("/v1/agents/{id}/run"),
        &json!({"input": "x".repeat(500_000)}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let run_id = response_json(response).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let payload: Value = sqlx::query_scalar("select payload from jobs where dedupe_key = $1")
        .bind(format!("run_agent:{run_id}"))
        .fetch_one(&ctx.app.db.pool)
        .await
        .unwrap();
    let stored = payload["input"].as_str().unwrap();
    assert!(
        stored.len() < 5_000,
        "an unbounded input reached the job: {}",
        stored.len()
    );
}

/// The `nl_definition` bounds moved into `compile::compile`, which is the one
/// place every compile passes through — the same argument that put the spend
/// ceiling there. These assert the outcome the handlers must still produce, and
/// they are new: before the move, both bounds were enforced by two copies of an
/// `if` that no test exercised.
#[tokio::test]
async fn the_input_cap_is_enforced_and_no_agent_is_created() {
    let ctx = ctx().await;
    // No mock is mounted, which is half the point: a refused sentence must not
    // reach the provider at all. `MockServer` fails an unmatched request, so a
    // call that escaped would surface here rather than pass quietly.
    let long = "a".repeat(4_001);

    for body in [
        json!({"nl_definition": ""}),
        json!({"nl_definition": "   "}),
    ] {
        let response = post(&ctx, "/v1/agents", &body).await;
        assert_eq!(response.status(), 400, "empty sentence");
        let json = response_json(response).await;
        assert_eq!(json["error"]["code"], "bad_request");
        assert_eq!(
            json["error"]["message"],
            "Describe what the agent should do."
        );
    }

    let response = post(&ctx, "/v1/agents", &json!({"nl_definition": long})).await;
    assert_eq!(response.status(), 400, "4001 characters");
    let json = response_json(response).await;
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("too long"),
        "{json}"
    );

    // A `400` is a refusal, not a draft: nothing is stored.
    let count: i64 = sqlx::query_scalar("select count(*) from agents where account_id = $1")
        .bind(ctx.account)
        .fetch_one(&ctx.app.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "a refused sentence must not create an agent");
}

/// EDGE (unicode): the cap is characters, as `API.md` §0 states. Counting bytes
/// would reject a sentence a third this length written in Japanese.
#[tokio::test]
async fn the_input_cap_counts_characters_and_not_bytes() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;

    // 2 000 characters, ~6 000 bytes - comfortably under the character cap and
    // comfortably over it in bytes.
    let sentence = "あ".repeat(2_000);
    let response = post(&ctx, "/v1/agents", &json!({"nl_definition": sentence})).await;
    assert_eq!(
        response.status(),
        200,
        "2000 characters is under the 4000-character cap"
    );
}

#[tokio::test]
async fn a_patch_is_bound_by_the_same_input_cap() {
    let ctx = ctx().await;
    mount_compile(&ctx, emitted()).await;
    let agent = create(&ctx).await;
    let id = agent["id"].as_str().unwrap().to_owned();

    let response = patch(
        &ctx,
        &format!("/v1/agents/{id}"),
        &json!({"nl_definition": "a".repeat(4_001)}),
    )
    .await;
    assert_eq!(response.status(), 400);

    // And the stored sentence is untouched.
    let stored: String = sqlx::query_scalar("select nl_definition from agents where id = $1::uuid")
        .bind(&id)
        .fetch_one(&ctx.app.db.pool)
        .await
        .unwrap();
    assert_eq!(stored, SENTENCE);
}
