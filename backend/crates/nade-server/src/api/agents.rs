//! `/agents` — the builder's whole surface.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    agents::{compile, run, tools::V1_TOOLS},
    api::{auth::Auth, mail::wire_ts},
    error::{ApiError, ApiJson, ApiResult},
    llm::ledger::SpendGuard,
    state::AppState,
};

// ------------------------------------------------------------- the wire --

#[derive(Debug, Serialize)]
pub struct AgentSummary {
    pub id: Uuid,
    pub name: String,
    pub nl_definition: String,
    pub status: String,
    pub trigger_summary: String,
    pub schedule: Option<Value>,
    pub last_run_at: Option<String>,
    pub approval_required: bool,
}

#[derive(Debug, Serialize)]
pub struct AgentDetail {
    #[serde(flatten)]
    pub summary: AgentSummary,
    pub allowed_tools: Vec<String>,
    pub when_span: Option<String>,
    pub do_span: Option<String>,
    pub trailing: Option<String>,
    pub compile_error: Option<String>,
    pub spec: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct AgentsResponse {
    pub agents: Vec<AgentSummary>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAgent {
    pub nl_definition: String,
}

/// Validate a client-supplied `schedule` against `API.md` §5.2.
///
/// This was `Option<Value>` written straight into the `jsonb` column, which is
/// three defects at once:
///
/// * **the app breaks.** `WireSchedule` on the iOS side declares `freq`,
///   `interval`, `byweekday`, `at`, `tz`, `ends` and `runs_done` as
///   non-optional, and a decode failure on one agent fails the whole
///   `GET /agents` body - every agent disappears behind "The server sent
///   something unexpected", not just the broken one;
/// * **`runs_done` became client-writable**, though the doc comment on this
///   very struct said it was "ignored if sent". §5.2 calls it server-maintained
///   and read-only, and `ends.after` counts against it;
/// * **the trigger/schedule invariant breaks.** `validate.py` and the contract
///   tests both enforce "a schedule trigger and a schedule imply each other",
///   and `compile.rs` deliberately refuses to compile a schedule trigger for
///   exactly that reason - then `PATCH` walked around the guard.
///
/// Returns the value to store, with `runs_done` taken from the stored row
/// rather than the request.
fn validate_schedule(input: &Value, stored: Option<&Value>) -> ApiResult<Value> {
    let bad = |why: &str| ApiError::bad_request(format!("The schedule is not valid: {why}."));

    let object = input
        .as_object()
        .ok_or_else(|| bad("it must be an object"))?;

    let freq = object
        .get("freq")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("`freq` is required"))?;
    if !matches!(freq, "day" | "week" | "month") {
        return Err(bad("`freq` must be day, week or month"));
    }

    let interval = object.get("interval").and_then(Value::as_i64).unwrap_or(1);
    if interval < 1 {
        return Err(bad("`interval` must be at least 1"));
    }

    let byweekday: Vec<String> = crate::json::str_array(object.get("byweekday"))
        .into_iter()
        .map(str::to_owned)
        .collect();
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    if !byweekday.is_empty() {
        if freq != "week" {
            return Err(bad("`byweekday` is only allowed when `freq` is week"));
        }
        if let Some(unknown) = byweekday.iter().find(|d| !DAYS.contains(&d.as_str())) {
            return Err(bad(&format!("{unknown:?} is not a weekday")));
        }
    }

    let bymonthday = object.get("bymonthday").and_then(Value::as_i64);
    if let Some(day) = bymonthday {
        if freq != "month" {
            return Err(bad("`bymonthday` is only allowed when `freq` is month"));
        }
        // §5.2 caps a fixed day at 28 "so a monthly schedule never silently
        // skips February"; -1 is the actual last day.
        if day != -1 && !(1..=28).contains(&day) {
            return Err(bad("`bymonthday` must be 1..28, or -1 for the last day"));
        }
    }

    let at = object
        .get("at")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("`at` is required, as HH:MM"))?;
    let valid_time = at.len() == 5
        && at.as_bytes()[2] == b':'
        && at[..2].parse::<u32>().is_ok_and(|h| h < 24)
        && at[3..].parse::<u32>().is_ok_and(|m| m < 60);
    if !valid_time {
        return Err(bad("`at` must be a 24-hour HH:MM time"));
    }

    let tz = object
        .get("tz")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("`tz` is required"))?;
    if tz.parse::<chrono_tz::Tz>().is_err() {
        return Err(bad(&format!("{tz:?} is not an IANA time zone")));
    }

    let ends = object
        .get("ends")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("`ends` is required"))?;
    match ends.get("kind").and_then(Value::as_str) {
        Some("never") => {}
        Some("on") => {
            let date = ends
                .get("date")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("`ends.kind` is `on`, so `ends.date` is required"))?;
            if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
                return Err(bad("`ends.date` must be YYYY-MM-DD"));
            }
        }
        Some("after") => {
            let count = ends
                .get("count")
                .and_then(Value::as_i64)
                .ok_or_else(|| bad("`ends.kind` is `after`, so `ends.count` is required"))?;
            if count < 1 {
                return Err(bad("`ends.count` must be at least 1"));
            }
        }
        _ => return Err(bad("`ends.kind` must be never, on or after")),
    }

    // Server-maintained: carried over from the stored schedule, never taken
    // from the request.
    let runs_done = stored
        .and_then(|s| s.get("runs_done"))
        .and_then(Value::as_i64)
        .unwrap_or(0);

    Ok(serde_json::json!({
        "freq": freq,
        "interval": interval,
        "byweekday": byweekday,
        "bymonthday": bymonthday.map_or(Value::Null, Value::from),
        "at": at,
        "tz": tz,
        "ends": {
            "kind": ends.get("kind").and_then(Value::as_str).unwrap_or("never"),
            "date": ends.get("date").cloned().unwrap_or(Value::Null),
            "count": ends.get("count").cloned().unwrap_or(Value::Null),
        },
        "runs_done": runs_done,
    }))
}

/// Any subset of the five fields `API.md` §5 names. `runs_done` lives inside
/// `schedule` and is server-maintained; it is **ignored** if sent.
#[derive(Debug, Deserialize)]
pub struct PatchAgent {
    pub nl_definition: Option<String>,
    pub status: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub approval_required: Option<bool>,
    pub schedule: Option<Value>,
}

/// An opening message is a sentence, not a document. The agent's instruction
/// carries the standing task; this is the "run it about *this*" note.
const MAX_RUN_INPUT_BYTES: usize = 4_000;

#[derive(Debug, Deserialize)]
pub struct RunNow {
    pub input: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunAccepted {
    pub run_id: Uuid,
}

// -------------------------------------------------------------- the row --

#[derive(Debug, sqlx::FromRow)]
struct AgentRow {
    id: Uuid,
    name: String,
    nl_definition: String,
    status: String,
    spec: Option<Value>,
    allowed_tools: Vec<String>,
    approval_required: bool,
    schedule: Option<Value>,
    when_span: Option<String>,
    do_span: Option<String>,
    trailing_clause: Option<String>,
    compile_error: Option<String>,
    last_run_at: Option<DateTime<Utc>>,
}

/// Every column the two views need, plus the newest run's timestamp.
///
/// `last_run_at` is a join and not a column: `validate.py` requires it to equal
/// the newest run's `created_at`, and a cached copy would be one more thing that
/// can disagree with the table it summarises.
const SELECT: &str = "select a.id, a.name, a.nl_definition, a.status, a.spec, a.allowed_tools, \
                             a.approval_required, a.schedule, a.when_span, a.do_span, \
                             a.trailing_clause, a.compile_error, \
                             (select max(r.created_at) from agent_runs r where r.agent_id = a.id) \
                               as last_run_at \
                        from agents a";

/// The human string the list row shows. Server-rendered, so every client says
/// the same thing about the same agent.
fn trigger_summary(spec: Option<&Value>, schedule: Option<&Value>) -> String {
    let Some(spec) = spec else {
        // A sentence that did not compile still has to render as *something*
        // honest; `compile_error` carries the detail. The exact words come from
        // `agents.json` / `agent_compile_failed.json`, which is what the app
        // renders and what the design screenshots show.
        return "Not set".to_owned();
    };
    match spec
        .get("trigger")
        .and_then(|t| t.get("kind"))
        .and_then(Value::as_str)
    {
        Some("mail") => "On new mail".to_owned(),
        Some("schedule") => schedule_summary(schedule),
        _ => "Manual only".to_owned(),
    }
}

/// Render a schedule the way the contract fixtures do.
///
/// `agents.json` and `agent_scheduled.json` both say **"Every weekday at
/// 08:00"** for `freq: "week"` with `byweekday: [mon..fri]`. A naive
/// `"Every {freq} at {at}"` produces "Every week at 08:00" - which no test
/// caught, because every agent assertion used `shape_of` (key sets and JSON
/// types) and never compared the string. Both lanes were green while
/// disagreeing about a line `DESIGN.md` puts in accent type on every row of 1b.
fn schedule_summary(schedule: Option<&Value>) -> String {
    const WEEKDAYS: [&str; 5] = ["mon", "tue", "wed", "thu", "fri"];

    let Some(schedule) = schedule else {
        return "On a schedule".to_owned();
    };
    let at = schedule.get("at").and_then(Value::as_str).unwrap_or("");
    let freq = schedule.get("freq").and_then(Value::as_str).unwrap_or("");
    let days: Vec<&str> = crate::json::str_array(schedule.get("byweekday"));

    let cadence = match freq {
        "day" => "Every day".to_owned(),
        "month" => "Every month".to_owned(),
        "week" if days == WEEKDAYS => "Every weekday".to_owned(),
        "week" if days.len() == 1 => format!("Every {}", long_day(days[0])),
        "week" => "Every week".to_owned(),
        _ => return "On a schedule".to_owned(),
    };
    if at.is_empty() {
        cadence
    } else {
        format!("{cadence} at {at}")
    }
}

fn long_day(short: &str) -> &str {
    match short {
        "mon" => "Monday",
        "tue" => "Tuesday",
        "wed" => "Wednesday",
        "thu" => "Thursday",
        "fri" => "Friday",
        "sat" => "Saturday",
        "sun" => "Sunday",
        other => other,
    }
}

impl AgentRow {
    fn into_summary(self) -> AgentSummary {
        let trigger_summary = trigger_summary(self.spec.as_ref(), self.schedule.as_ref());
        Self::summary_with(self, trigger_summary)
    }

    /// The summary, given a `trigger_summary` already rendered from the spec.
    ///
    /// Split out so [`Self::into_detail`] can render the summary *before* it
    /// moves the spec, instead of deep-cloning the spec to work around the
    /// move — which it did, along with four `String`s and a `Vec<String>`, on
    /// every `GET /agents/{id}`, `POST /agents` and `PATCH /agents/{id}`.
    fn summary_with(self, trigger_summary: String) -> AgentSummary {
        AgentSummary {
            id: self.id,
            name: self.name,
            nl_definition: self.nl_definition,
            status: self.status,
            trigger_summary,
            schedule: self.schedule,
            last_run_at: self.last_run_at.map(wire_ts),
            approval_required: self.approval_required,
        }
    }

    fn into_detail(mut self) -> AgentDetail {
        // Render first, then move each field exactly once. Nothing is cloned.
        let trigger_summary = trigger_summary(self.spec.as_ref(), self.schedule.as_ref());
        let detail = (
            std::mem::take(&mut self.allowed_tools),
            self.when_span.take(),
            self.do_span.take(),
            self.trailing_clause.take(),
            self.compile_error.take(),
            self.spec.take(),
        );
        AgentDetail {
            summary: self.summary_with(trigger_summary),
            allowed_tools: detail.0,
            when_span: detail.1,
            do_span: detail.2,
            trailing: detail.3,
            compile_error: detail.4,
            spec: detail.5,
        }
    }
}

/// The account's ledger, its ceiling, and a flag nothing on this path reads.
fn spend_guard(state: &AppState, account_id: Uuid) -> SpendGuard {
    SpendGuard::new(
        state.pool.clone(),
        account_id,
        state.config.llm.daily_ceiling_nano_usd,
    )
}

/// One sentence for every route that runs out of budget, so the app can match
/// on it once - and a `Retry-After` that says when, because `API.md` §0 makes
/// the header part of what `rate_limited` means.
fn over_budget() -> ApiError {
    ApiError::new(
        crate::error::ErrorCode::RateLimited,
        "Your agents have used today's AI budget. They will start again tomorrow.",
    )
    .retry_after(seconds_until_utc_midnight())
}

/// How long until the ledger's day rolls over. The budget resets at midnight
/// UTC (`llm::ledger`), so that is the honest answer to "when can I retry".
fn seconds_until_utc_midnight() -> u64 {
    use chrono::Timelike;
    let now = chrono::Utc::now();
    let elapsed =
        u64::from(now.hour()) * 3_600 + u64::from(now.minute()) * 60 + u64::from(now.second());
    // Saturating, and never zero: a caller told to retry in 0 seconds retries
    // immediately and gets the same answer.
    (24 * 3_600 - elapsed).max(1)
}

async fn load(state: &AppState, account_id: Uuid, id: Uuid) -> ApiResult<AgentRow> {
    // Account-scoped in the predicate: another account's agent is a 404, so the
    // caller learns nothing about what exists.
    sqlx::query_as::<_, AgentRow>(&format!("{SELECT} where a.id = $1 and a.account_id = $2"))
        .bind(id)
        .bind(account_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)
}

// ----------------------------------------------------------- handlers --

/// `GET /v1/agents` — **not** paginated, oldest first.
///
/// `API.md` §0: a bounded collection carries no cursor field at all, "because
/// inventing one implies a page boundary that will never exist". Oldest first
/// so the list does not reshuffle under the user every time an agent runs.
pub async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<AgentsResponse>> {
    let Some(account) = auth.account else {
        return Ok(Json(AgentsResponse { agents: Vec::new() }));
    };
    let rows: Vec<AgentRow> = sqlx::query_as(&format!(
        "{SELECT} where a.account_id = $1 order by a.created_at asc, a.id asc"
    ))
    .bind(account.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(AgentsResponse {
        agents: rows.into_iter().map(AgentRow::into_summary).collect(),
    }))
}

/// `POST /v1/agents` — compile a sentence, store it as a draft.
///
/// **A compile failure is not an HTTP failure.** `API.md` §5: the agent is
/// still created, as a `draft`, with `spec: null` and `compile_error` set, "so
/// the user's sentence is never lost". This handler therefore returns `200`
/// for every outcome except a malformed request or a database failure.
pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    ApiJson(body): ApiJson<CreateAgent>,
) -> ApiResult<Json<AgentDetail>> {
    let account = auth.account.ok_or_else(|| {
        ApiError::bad_request("Connect a Gmail account before creating an agent.")
    })?;

    let sentence = body.nl_definition.trim();
    let guard = spend_guard(&state, account.id);
    // Concurrently: the compile is a multi-second provider call and the
    // account's default depends on nothing it produces.
    //
    // The default is the one the `settings` row is born with (D46), so there is
    // a value to read and nothing to invent.
    let (compiled, approval_default) = tokio::join!(
        compile::compile(state.llm.as_ref(), &state.pool, &guard, sentence),
        sqlx::query_scalar::<_, bool>(
            "select approval_required_default from settings where account_id = $1"
        )
        .bind(account.id)
        .fetch_optional(&state.pool),
    );
    let approval_default = approval_default?.unwrap_or(true);

    // A ceiling breach is not a fact about the sentence, so it is not stored as
    // one: the client keeps its text and is told to come back tomorrow. An
    // empty or over-long sentence is a fact about the *request*, and answers
    // `400`. Every other compile failure still creates the agent, per
    // `API.md` §5.
    if matches!(compiled, Err(compile::CompileError::CeilingReached)) {
        return Err(over_budget());
    }
    if let Err(err) = &compiled {
        if err.is_bad_request() {
            return Err(ApiError::bad_request(err.to_string()));
        }
    }

    // One call, not one per arm: `insert_agent` exists precisely to be the
    // single site that keeps `spec` XOR `compile_error` true, and calling it
    // twice with the same six arguments gave that job back to the caller.
    let compile_error = compiled.as_ref().err().map(ToString::to_string);
    let id = insert_agent(
        &state,
        account.id,
        sentence,
        compiled.as_ref().ok(),
        compile_error.as_deref(),
        approval_default,
    )
    .await?;

    let row = load(&state, account.id, id).await?;
    Ok(Json(row.into_detail()))
}

/// One insert for both outcomes, so the `spec` XOR `compile_error` invariant
/// cannot be violated by one of two code paths forgetting it.
async fn insert_agent(
    state: &AppState,
    account_id: Uuid,
    sentence: &str,
    compiled: Option<&compile::Compiled>,
    compile_error: Option<&str>,
    approval_required: bool,
) -> ApiResult<Uuid> {
    debug_assert_eq!(
        compiled.is_none(),
        compile_error.is_some(),
        "exactly one of spec and compile_error is set"
    );
    let name = compiled.map_or_else(|| fallback_name(sentence), |c| c.name.clone());
    let id: Uuid = sqlx::query_scalar(
        // `status` is not a parameter: `API.md` §5 says a new agent is *always*
        // a draft and "the client cannot ask for anything else", so it is
        // literal here rather than trusted to a caller.
        "insert into agents (account_id, name, nl_definition, spec, allowed_tools, \
                             approval_required, status, when_span, do_span, trailing_clause, \
                             compile_error) \
         values ($1, $2, $3, $4, $5, $6, 'draft', $7, $8, $9, $10) returning id",
    )
    .bind(account_id)
    .bind(name)
    .bind(sentence)
    .bind(compiled.map(|c| c.spec.clone()))
    .bind(compiled.map_or_else(Vec::new, |c| c.allowed_tools.clone()))
    .bind(approval_required)
    .bind(compiled.map(|c| c.when_span.clone()))
    .bind(compiled.map(|c| c.do_span.clone()))
    .bind(compiled.and_then(|c| c.trailing.clone()))
    .bind(compile_error)
    .fetch_one(&state.pool)
    .await?;
    Ok(id)
}

/// A name for an agent whose sentence did not compile, so the list is still
/// readable. The first few words of what the user actually wrote.
fn fallback_name(sentence: &str) -> String {
    let words: Vec<&str> = sentence.split_whitespace().take(6).collect();
    let joined = words.join(" ");
    crate::agents::tools::cap(
        if joined.is_empty() {
            "New agent"
        } else {
            &joined
        },
        80,
    )
}

/// `GET /v1/agents/{id}`.
pub async fn detail(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AgentDetail>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;
    Ok(Json(load(&state, account.id, id).await?.into_detail()))
}

/// `PATCH /v1/agents/{id}`.
pub async fn patch(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<PatchAgent>,
) -> ApiResult<Json<AgentDetail>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;
    // Establish it is ours before spending a compile on it.
    let existing = load(&state, account.id, id).await?;

    if body.nl_definition.is_none()
        && body.status.is_none()
        && body.allowed_tools.is_none()
        && body.approval_required.is_none()
        && body.schedule.is_none()
    {
        return Err(ApiError::bad_request("Send at least one field to change."));
    }

    if let Some(status) = body.status.as_deref() {
        if !matches!(status, "draft" | "published" | "paused") {
            return Err(ApiError::bad_request(
                "Status must be one of draft, published or paused.",
            ));
        }
    }

    if let Some(tools) = body.allowed_tools.as_ref() {
        if let Some(unknown) = tools.iter().find(|t| !V1_TOOLS.contains(&t.as_str())) {
            return Err(ApiError::bad_request(format!(
                "{unknown:?} is not a tool this version has."
            )));
        }
    }

    // A schedule only means anything on a schedule-triggered agent, and
    // `validate.py` plus the contract tests both enforce that the two imply
    // each other. P4's compiler never emits a schedule trigger, so in practice
    // this refuses every schedule today - through the rule that will still be
    // right when P7 derives `next_run_at` from one.
    let schedule = match body.schedule.as_ref() {
        None => None,
        Some(input) => {
            let kind = existing
                .spec
                .as_ref()
                .and_then(|spec| spec.get("trigger"))
                .and_then(|trigger| trigger.get("kind"))
                .and_then(Value::as_str);
            if kind != Some("schedule") {
                return Err(ApiError::bad_request(
                    "This agent is not on a schedule, so it cannot be given one.",
                ));
            }
            Some(validate_schedule(input, existing.schedule.as_ref())?)
        }
    };

    // Changing the sentence recompiles the spec (`API.md` §5). A failure is
    // recorded the same way creation records one - the sentence is kept.
    let recompiled = match body.nl_definition.as_deref().map(str::trim) {
        None => None,
        Some(sentence) => {
            let guard = spend_guard(&state, account.id);
            let outcome = compile::compile(state.llm.as_ref(), &state.pool, &guard, sentence).await;
            if matches!(outcome, Err(compile::CompileError::CeilingReached)) {
                return Err(over_budget());
            }
            if let Err(err) = &outcome {
                if err.is_bad_request() {
                    return Err(ApiError::bad_request(err.to_string()));
                }
            }
            Some((sentence.to_owned(), outcome))
        }
    };

    // `coalesce` per column so two concurrent patches of different fields
    // cannot undo each other - the defect P3's iOS review found in exactly this
    // shape, where both writers derived from the same pre-edit object.
    let (mut sentence, mut spec, mut when_span, mut do_span) = (None, None, None, None);
    let (mut trailing, mut compile_error, mut compiled_tools) = (None, None, None);
    if let Some((text, outcome)) = recompiled {
        sentence = Some(text);
        match outcome {
            Ok(c) => {
                spec = Some(c.spec);
                when_span = Some(c.when_span);
                do_span = Some(c.do_span);
                trailing = c.trailing;
                compiled_tools = Some(c.allowed_tools);
            }
            Err(err) => compile_error = Some(err.to_string()),
        }
    }
    let tools = body.allowed_tools.clone().or(compiled_tools);

    let recompiling = sentence.is_some();
    // A spec that is now null forces the status back to draft, or the row would
    // violate `validate.py`'s "failed to compile, so it must still be a draft".
    // Derived from `compile_error` rather than carried alongside it as its own
    // boolean, which was a second thing to keep in step by hand.
    let status = if compile_error.is_some() {
        Some("draft".to_owned())
    } else {
        body.status.clone()
    };

    // `validate.py`: an agent with no spec must still be a draft - publishing
    // one would produce a run with no instruction at all. Checked against the
    // spec that will be **stored**: a patch that fixes the sentence and
    // publishes in the same request is legitimate, and checking the pre-patch
    // spec refused it.
    if let Some(wanted) = status.as_deref() {
        let will_have_spec = if recompiling {
            spec.is_some()
        } else {
            existing.spec.is_some()
        };
        if wanted != "draft" && !will_have_spec {
            return Err(ApiError::bad_request(
                "This agent has not been set up yet, so it cannot be published.",
            ));
        }
    }

    // `API.md` §5.1: "`spec.tools` must be a subset of `allowed_tools`". Checked
    // against the spec that will be *stored*, not the one that was there before
    // - a recompile changes both halves at once, and narrowing `allowed_tools`
    // alone would leave a row `validate.py` and the contract tests both reject.
    let effective_spec = if recompiling {
        spec.as_ref()
    } else {
        existing.spec.as_ref()
    };
    let effective_tools = tools.as_ref().unwrap_or(&existing.allowed_tools);
    if let Some(spec) = effective_spec {
        let needed: Vec<&str> = crate::json::str_array(spec.get("tools"));
        if let Some(missing) = needed
            .iter()
            .find(|tool| !effective_tools.iter().any(|allowed| allowed == *tool))
        {
            return Err(ApiError::bad_request(format!(
                "This agent still needs {missing:?}, so it cannot be removed from its tools."
            )));
        }
    }

    sqlx::query(
        "update agents set \
            nl_definition = coalesce($3, nl_definition), \
            status = coalesce($4, status), \
            allowed_tools = coalesce($5, allowed_tools), \
            approval_required = coalesce($6, approval_required), \
            schedule = case when $7::jsonb is null then schedule else $7::jsonb end, \
            spec = case when $8 then $9::jsonb else spec end, \
            when_span = case when $8 then $10::text else when_span end, \
            do_span = case when $8 then $11::text else do_span end, \
            trailing_clause = case when $8 then $12::text else trailing_clause end, \
            compile_error = case when $8 then $13::text else compile_error end, \
            updated_at = now() \
          where id = $1 and account_id = $2",
    )
    .bind(id)
    .bind(account.id)
    .bind(sentence)
    .bind(status)
    .bind(tools)
    .bind(body.approval_required)
    .bind(schedule)
    .bind(recompiling)
    .bind(spec)
    .bind(when_span)
    .bind(do_span)
    .bind(trailing)
    .bind(compile_error)
    .execute(&state.pool)
    .await?;

    Ok(Json(load(&state, account.id, id).await?.into_detail()))
}

/// `DELETE /v1/agents/{id}` → `204`.
///
/// Non-terminal runs are cancelled **first** (`API.md` §5): the FK cascade
/// removes `run_journal` with the agent, so a run left in flight would have its
/// log yanked out from under it mid-step. Feed items already raised stay
/// readable — they reference the run with `on delete set null`.
pub async fn delete(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;
    load(&state, account.id, id).await?;

    run::cancel_runs_of(&state, id)
        .await
        .map_err(|err| ApiError::internal("cancelling an agent's runs", &err))?;

    sqlx::query("delete from agents where id = $1 and account_id = $2")
        .bind(id)
        .bind(account.id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/agents/{id}/run` — the builder's "Run once now".
///
/// Runs a `draft` agent too, and does **not** advance `runs_done`
/// (`API.md` §6).
pub async fn run_now(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<RunNow>,
) -> ApiResult<Json<RunAccepted>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;
    let agent = load(&state, account.id, id).await?;

    if agent.spec.is_none() {
        return Err(ApiError::bad_request(
            "This agent has not been set up yet, so there is nothing to run.",
        ));
    }
    if state.llm.is_none() {
        return Err(ApiError::of(crate::error::ErrorCode::UpstreamUnavailable));
    }

    // The ceiling is checked **here** rather than in the job. Refusing in the
    // handler means no run row is created; refusing in the job would strand a
    // `queued` run with an empty journal, and `Engine::cancel` refuses one of
    // those outright - there would be no legal way to end it.
    if spend_guard(&state, account.id).check().await?.is_err() {
        run::raise_spend_ceiling_notice(&state.pool, account.id).await;
        return Err(over_budget());
    }

    // Sanitised and capped before it goes anywhere. Every other text field P4
    // added is; this one reaches the model's opening message *and* the job
    // payload *and* `run_started` in the journal, so it is the worst one to
    // leave raw:
    //
    // * a NUL fails the `jsonb` insert - and the run row was written first, so
    //   the failure stranded a `queued` run with no job to drive it and no
    //   journal for `Engine::cancel` to work from;
    // * an unbounded body spends the whole 50k per-run token budget in one
    //   turn, because the engine checks the budget *before* a turn against the
    //   usage accumulated so far, which on turn one is zero.
    let input = body
        .input
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            crate::agents::tools::cap(
                &crate::agents::fence::strip_control_characters(value),
                MAX_RUN_INPUT_BYTES,
            )
        });

    // The run row and its job commit together. Written apart, a failed enqueue
    // leaves a `queued` run nothing will ever move, and `Engine::cancel`
    // refuses an empty journal outright.
    let mut tx = state.pool.begin().await?;
    let run_id: Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, account_id, trigger_kind) \
         values ($1, $2, 'manual') returning id",
    )
    .bind(id)
    .bind(account.id)
    .fetch_one(&mut *tx)
    .await?;

    // Through the queue's own helper, in this transaction. The `on conflict`
    // target has to restate the partial index's predicate verbatim for
    // PostgreSQL to infer the index, and that is not a sentence to keep a
    // second copy of in an HTTP handler.
    run::enqueue_in(&mut *tx, run_id, input.as_deref()).await?;
    tx.commit().await?;

    Ok(Json(RunAccepted { run_id }))
}

#[cfg(test)]
mod tests;
