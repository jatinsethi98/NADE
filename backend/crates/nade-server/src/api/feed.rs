//! `GET /feed`, `GET /feed/{id}`, and the three state changes under them.
//!
//! The feed is the home screen and "the only place NADE asks for anything"
//! (`API.md` §7). Everything here is about one question: has this decision been
//! recorded exactly once?
//!
//! # The lock order is `feed_items` first, then `agent_runs`
//!
//! Approve, skip and the expiry sweep all take the card's row and then the
//! run's. `DELETE /agents/{id}` takes them in the same order for the same
//! reason (`agents::run::settle_cards_of` runs before `cancel_runs_of`).
//! Reversing it anywhere makes a deadlock reachable, and the transaction
//! Postgres aborts would be a user's Approve.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use nade_agent_sdk::{ApprovalRequest, Seq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{
    agents::resume,
    api::{auth::Auth, cursor, cursor::Keyset, mail::wire_ts},
    error::{ApiError, ApiResult},
    state::AppState,
};

const PAGE_ROWS: usize = 50;
const PAGE: i64 = PAGE_ROWS as i64;

/// The italic line under a settled card (`API.md` §7).
///
/// Server-authored copy, so it is screened by
/// `docs/contract/validate.py::check_no_outbound_copy` and pinned by a test.
/// None of it may promise an outbound action: v1 saves, it never sends.
///
/// The two an **approval** settles to are not here. They are per-tool, so they
/// live in `agents::feed`'s [`GatePresentation`](crate::agents::feed::GatePresentation)
/// table beside the button label they are the answer to — where a tool that
/// gains a gate and no copy fails a test instead of quietly reading
/// "Saved to Notes."
const SKIPPED: &str = "Skipped — nothing was saved.";
/// Shared with `agents::run::settle_cards_of`, which settles a deleted agent's
/// cards before this endpoint can meet one.
pub const AGENT_DELETED: &str = "The agent was deleted, so this was never saved.";
const EXPIRED: &str = "Expired after 7 days — nothing was saved.";

// ------------------------------------------------------------------ wire --

#[derive(Debug, Serialize)]
pub struct FeedItem {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub run_id: Option<Uuid>,
    /// "Exactly the buttons to render, in order" (`API.md` §7).
    pub actions: Vec<String>,
    pub approval_token: Option<Uuid>,
    pub approval_expires_at: Option<String>,
    pub resolved_note: Option<String>,
    pub data: Option<Value>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct FeedResponse {
    pub items: Vec<FeedItem>,
    pub next_cursor: Option<String>,
    pub new_count: i64,
}

#[derive(Debug, Serialize)]
pub struct ApproveResponse {
    pub run_id: Uuid,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct SkipResponse {
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct SeenResponse {
    pub new_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Decision {
    pub approval_token: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct Seen {
    pub ids: Vec<Uuid>,
}

// ------------------------------------------------------------------ rows --

/// One card, as stored. `pub(crate)` because the expiry sweep lives in
/// `agents::expire` and settles rows this module locked and read.
#[derive(Debug, sqlx::FromRow)]
pub struct FeedRow {
    pub id: Uuid,
    pub account_id: Uuid,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) body: Option<String>,
    pub(crate) status: String,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) approval_token: Option<Uuid>,
    pub(crate) approval_expires_at: Option<DateTime<Utc>>,
    pub(crate) resolved_note: Option<String>,
    pub(crate) data: Option<Value>,
    pub(crate) step_seq: Option<i32>,
    pub(crate) created_at: DateTime<Utc>,
}

/// The columns every read of a card selects, in one place.
const COLUMNS: &str = "id, account_id, kind, title, body, status, run_id, approval_token, \
                       approval_expires_at, resolved_note, data, step_seq, created_at";

impl FeedRow {
    /// The wire shape. **One** conversion, shared by all five routes, so
    /// `actions` and the token-nulling rule cannot be right on the list and
    /// wrong on the deep link.
    pub(crate) fn into_wire(self) -> FeedItem {
        let live = self.kind == "approval" && self.status == "new";
        let action = self
            .data
            .as_ref()
            .and_then(|data| data.get("action"))
            .and_then(Value::as_str)
            .unwrap_or("none");

        // `API.md` §7's three branches, derived rather than stored. Storing
        // them would be a second thing to keep true, and the rule is a
        // function of two columns already here.
        // Edit means "approve, then edit the draft it created" — v1's only edit
        // path — so it belongs to a tool that *makes* something editable.
        // `offers_edit` is that fact, stated once, next to the button's label.
        let actions: Vec<String> = if !live {
            Vec::new()
        } else if crate::agents::feed::presentation_or_default(action).offers_edit {
            vec!["approve".into(), "edit".into(), "skip".into()]
        } else {
            vec!["approve".into(), "skip".into()]
        };

        FeedItem {
            id: self.id,
            kind: self.kind,
            title: self.title,
            // `body` is `text` (nullable) in the schema and non-null on the
            // wire. The producers always write one; a row from before they did
            // renders as empty rather than failing the whole page.
            body: self.body.unwrap_or_default(),
            status: self.status,
            run_id: self.run_id,
            actions,
            // Belt and braces: the transactions null the token when they
            // consume it, and this makes the wire rule true even if a row is
            // somehow left holding one.
            approval_token: if live { self.approval_token } else { None },
            approval_expires_at: self.approval_expires_at.map(wire_ts),
            resolved_note: self.resolved_note,
            data: self.data,
            created_at: wire_ts(self.created_at),
        }
    }
}

// ----------------------------------------------------------------- reads --

/// `GET /v1/feed?cursor`
///
/// # Errors
/// `400` on a corrupt cursor; `404` when no mailbox is connected.
pub async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<FeedQuery>,
) -> ApiResult<Json<FeedResponse>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;
    let keyset: Option<Keyset> = query.cursor.as_deref().map(cursor::decode).transpose()?;

    // The page and the badge are independent reads, and this is the home
    // screen's first request — so they go together rather than in turn.
    let sql = format!(
        "select {COLUMNS} from feed_items \
          where account_id = $1 \
            and ($2::timestamptz is null \
                 or (created_at, id) < ($2::timestamptz, $3::uuid)) \
          order by created_at desc, id desc \
          limit $4"
    );
    let page = sqlx::query_as::<_, FeedRow>(&sql)
        .bind(account.id)
        // The same two-liner `runs`, `notes` and `drafts` bind with — the fourth
        // keyset endpoint should not have a fourth spelling.
        .bind(keyset.as_ref().map(|key| key.ts))
        .bind(keyset.as_ref().map(Keyset::uuid).transpose()?)
        // One more than the page, so `take_page` can tell "the last page" from
        // "a page that happens to be full" without a second count.
        .bind(PAGE + 1)
        .fetch_all(&state.pool);

    let (mut rows, new_count) = tokio::try_join!(
        async { page.await.map_err(ApiError::from) },
        new_count(&state, account.id)
    )?;

    let next_cursor = cursor::take_page(&mut rows, PAGE_ROWS, |row| {
        (row.created_at, row.id.to_string())
    });

    Ok(Json(FeedResponse {
        items: rows.into_iter().map(FeedRow::into_wire).collect(),
        next_cursor,
        // The badge, and it is **not** a count of this page: `API.md` §7 says
        // "the number of items with `status: "new"`" for the mailbox.
        new_count,
    }))
}

/// `GET /v1/feed/{id}` — the push deep link.
///
/// # Errors
/// `404` when the card is not this account's.
pub async fn detail(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FeedItem>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;
    let row = load(&state, account.id, id).await?;
    Ok(Json(row.into_wire()))
}

async fn load(state: &AppState, account_id: Uuid, id: Uuid) -> ApiResult<FeedRow> {
    sqlx::query_as::<_, FeedRow>(&format!(
        "select {COLUMNS} from feed_items where id = $1 and account_id = $2"
    ))
    .bind(id)
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)
}

async fn new_count(state: &AppState, account_id: Uuid) -> ApiResult<i64> {
    let count: i64 = sqlx::query_scalar(
        "select count(*) from feed_items where account_id = $1 and status = 'new'",
    )
    .bind(account_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(count)
}

// --------------------------------------------------------------- decision --

/// What [`take`] found.
///
/// Two of the refusals **write** before they refuse — an expired card settles
/// itself, and a card whose agent is gone is settled rather than left holding a
/// live token — and a handler that simply returned `Err` would roll both back
/// with the transaction. That is not a hypothetical: it is what the first cut
/// of this module did, and the two tests that caught it are
/// `approving_one_second_late_…` and `approving_a_card_whose_agent_was_deleted_…`.
enum Taken {
    /// The card and its run agree; the decision may proceed.
    Ready(Box<(FeedRow, Pending)>),
    /// The card was settled inside the transaction. **Commit, then refuse.**
    Settled(ApiError),
}

/// What a card is asking about, once its row and its run agree.
struct Pending {
    run_id: Uuid,
    step_seq: Seq,
    request: ApprovalRequest,
}

/// Lock the card, then the run, and answer every way this can go wrong.
///
/// The decision table, in the order it is evaluated — each branch is a case a
/// client actually meets:
///
/// | state | answer |
/// |---|---|
/// | not this account's | `404 not_found` |
/// | not an approval | `400 bad_request` |
/// | already `resolved` or `skipped` | `409 token_consumed` — **the client treats this as success**: an earlier attempt won |
/// | already `expired` | `410 approval_expired` |
/// | wrong token | `401 unauthorized` |
/// | past its deadline | expire it, then `410 approval_expired` |
/// | its run was deleted | settle the card, then `410 gone` |
/// | the run has moved on | `409 conflict` |
///
/// The deadline is checked **here** and nowhere else. `EngineConfig::approval_ttl`
/// is `None` on purpose (`API.md` §6.1), so the engine will happily execute a
/// week-old approval; the host owns expiry, in this function and in the sweep.
async fn take(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    id: Uuid,
    token: Uuid,
) -> ApiResult<Taken> {
    let card = sqlx::query_as::<_, FeedRow>(&format!(
        "select {COLUMNS} from feed_items where id = $1 and account_id = $2 for update"
    ))
    .bind(id)
    .bind(account_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(ApiError::not_found)?;

    if card.kind != "approval" {
        return Err(ApiError::bad_request(
            "That card has nothing to approve.".to_owned(),
        ));
    }

    // **Before the status**, because the status cannot tell these apart.
    // `settle_cards_of` writes a deleted agent's cards to `skipped`, and a
    // status check that ran first answered `409 token_consumed` — which
    // `API.md` §7 and the client both read as *success*. The user would be told
    // their approval had already been recorded when nothing was saved, on the
    // ordinary path rather than in a race. `feed_items.run_id` is
    // `on delete set null`, so a null run is exactly "the agent is gone".
    if card.run_id.is_none() {
        if card.status == "new" {
            settle_card(tx, card.id, "skipped", AGENT_DELETED).await?;
            return Ok(Taken::Settled(ApiError::gone()));
        }
        return Err(ApiError::gone());
    }

    match card.status.as_str() {
        "new" => {}
        "expired" => return Err(ApiError::approval_expired()),
        // `resolved` or `skipped`: somebody already answered. Which of the two
        // does not matter to the client, and the contract says so — "it means
        // an earlier attempt already won".
        _ => return Err(ApiError::token_consumed()),
    }
    if card.approval_token != Some(token) {
        return Err(ApiError::unauthorized());
    }

    if let Some(expires_at) = card.approval_expires_at {
        // `>=`, matching the sweep's `<= now()`. At exactly `expires_at` a
        // strict comparison here let an approve succeed on a card the sweep
        // considered due, and nothing said which won.
        if Utc::now() >= expires_at {
            expire_card(tx, &card).await?;
            return Ok(Taken::Settled(ApiError::approval_expired()));
        }
    }

    // Checked above, before the status.
    let run_id = card.run_id.expect("a null run_id answers `gone` above");

    let run: Option<(String, Option<Value>)> =
        sqlx::query_as("select status, pending_action from agent_runs where id = $1 for update")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((status, pending_action)) = run else {
        settle_card(tx, card.id, "skipped", AGENT_DELETED).await?;
        return Ok(Taken::Settled(ApiError::gone()));
    };

    let request: Option<ApprovalRequest> =
        pending_action.and_then(|value| serde_json::from_value::<ApprovalRequest>(value).ok());
    let Some(request) = request else {
        return Err(ApiError::conflict());
    };
    // The card names a step; the run is parked on a step. A card whose run has
    // moved on to a *different* approval must not resolve the new one — the
    // SDK's `Resolution` carries `step_seq` for exactly this, and refusing here
    // means the user is told rather than surprised.
    let card_step = card.step_seq.map(|seq| seq as Seq);
    if status != "pending_approval" || card_step != Some(request.step_seq) {
        return Err(ApiError::conflict());
    }

    let pending = Pending {
        run_id,
        step_seq: request.step_seq,
        request,
    };
    Ok(Taken::Ready(Box::new((card, pending))))
}

/// Commit a settling refusal, or hand a plain one back untouched.
async fn refuse(tx: Transaction<'_, Postgres>, error: ApiError) -> ApiError {
    if let Err(commit) = tx.commit().await {
        return ApiError::internal("committing a settled refusal", &commit);
    }
    error
}

/// `POST /v1/feed/{id}/approve`
///
/// **One transaction, five writes** (`API.md` §7): consume the token → move the
/// run `pending_approval → queued` with its `pending_action` intact → resolve
/// the card with its note → write the audit row → enqueue the resume job.
///
/// **The transaction never touches `run_journal`.** The journal has one author,
/// the engine (`API.md` §6.1), so the approval reaches it only when the resume
/// job runs `Engine::resume` and appends `approval_resolved`. Writing it here
/// would append an entry replay refuses as corrupt.
///
/// # Errors
/// See [`take`].
pub async fn approve(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    crate::error::ApiJson(body): crate::error::ApiJson<Decision>,
) -> ApiResult<Json<ApproveResponse>> {
    let run_id = decide(
        &state,
        &auth,
        id,
        body.approval_token,
        resume::Decision::Approve,
    )
    .await?;
    Ok(Json(ApproveResponse {
        run_id,
        status: "queued".to_owned(),
    }))
}

/// The transaction both decisions run, keyed by which one it is.
///
/// `approve` and `skip` differ in three strings and a response type; they had
/// been the same forty-five lines twice, which meant the "one transaction, five
/// writes" invariant lived in two places and the commit-then-refuse dance —
/// the thing `Taken` and its two named tests exist for — was spelled out twice.
/// The next write this loop grows should be one edit.
///
/// # Errors
/// See [`take`].
async fn decide(
    state: &AppState,
    auth: &Auth,
    id: Uuid,
    token: Uuid,
    decision: resume::Decision,
) -> ApiResult<Uuid> {
    let account_id = auth.account.as_ref().ok_or_else(ApiError::not_found)?.id;
    let mut tx = state.pool.begin().await?;
    let (card, pending) = match take(&mut tx, account_id, id, token).await {
        Ok(Taken::Ready(pair)) => *pair,
        Ok(Taken::Settled(error)) => return Err(refuse(tx, error).await),
        Err(error) => return Err(error),
    };

    let (run_status, card_status, note) = match decision {
        // The note is per-tool, so it comes from the same table the card's
        // button label did.
        resume::Decision::Approve => (
            "queued",
            "resolved",
            crate::agents::feed::presentation_or_default(&pending.request.tool).resolved_note,
        ),
        resume::Decision::Skip => ("skipped", "skipped", SKIPPED),
        // Unreachable from HTTP — expiry arrives through `expire_card`, which
        // owns its own lock — but stating it keeps this match total, and a
        // total match is what stops a fourth decision defaulting to a note.
        resume::Decision::Expire => ("expired", "expired", EXPIRED),
    };

    sqlx::query(
        "update agent_runs set status = $2, updated_at = now() \
          where id = $1 and status = 'pending_approval'",
    )
    .bind(pending.run_id)
    .bind(run_status)
    .execute(&mut *tx)
    .await?;

    settle_card(&mut tx, card.id, card_status, note).await?;
    audit(
        &mut tx,
        account_id,
        auth,
        &format!("feed.{}", decision.as_str()),
        &card,
        &pending,
    )
    .await?;
    resume::enqueue_in(&mut *tx, pending.run_id, pending.step_seq, decision)
        .await
        .map_err(|err| ApiError::internal("enqueueing the resume job", &err))?;

    tx.commit().await?;
    Ok(pending.run_id)
}

/// `POST /v1/feed/{id}/skip`
///
/// The same transaction, and the same rule about the journal. The run moves to
/// `skipped` here because `API.md` §7 says it does — which makes the row
/// terminal *before* the engine has written `run_ended`, and is why
/// `resume_run` must not carry `run_agent`'s `is_terminal` guard.
///
/// # Errors
/// See [`take`].
pub async fn skip(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    crate::error::ApiJson(body): crate::error::ApiJson<Decision>,
) -> ApiResult<Json<SkipResponse>> {
    decide(
        &state,
        &auth,
        id,
        body.approval_token,
        resume::Decision::Skip,
    )
    .await?;
    Ok(Json(SkipResponse {
        status: "skipped".to_owned(),
    }))
}

/// `POST /v1/feed/seen`
///
/// "Marks `kind: "info"` items as `resolved`. Without it `new_count` would
/// never fall, because info items have nothing to approve. Ids that are
/// approvals, or already resolved, or unknown, are ignored rather than
/// erroring — this is a best-effort read receipt fired as the user scrolls."
///
/// `dismissible = false` is the one addition to that rule, and the needs-reauth
/// card is why: it is the only surface that says sync has stopped, and a scroll
/// past it must not be what clears it.
///
/// # Errors
/// `404` when no mailbox is connected.
pub async fn seen(
    State(state): State<AppState>,
    auth: Auth,
    crate::error::ApiJson(body): crate::error::ApiJson<Seen>,
) -> ApiResult<Json<SeenResponse>> {
    let account = auth.account.ok_or_else(ApiError::not_found)?;

    // EDGE (empty input): nothing to mark, and no reason to ask the database
    // to prove it. The count still has to be honest.
    if !body.ids.is_empty() {
        sqlx::query(
            "update feed_items set status = 'resolved' \
              where account_id = $1 and id = any($2) \
                and kind = 'info' and status = 'new' and dismissible",
        )
        .bind(account.id)
        .bind(&body.ids)
        .execute(&state.pool)
        .await?;
    }

    Ok(Json(SeenResponse {
        new_count: new_count(&state, account.id).await?,
    }))
}

// ------------------------------------------------------------- internals --

/// Consume the card: its status, its note, and its token in one statement.
///
/// The token is nulled here rather than left to expire, because it is a
/// **capability**: `API.md` §7's rule that `approval_token` is non-null only
/// while an approval is `new` is a statement about what exists, not only about
/// what is serialised.
async fn settle_card(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    status: &str,
    note: &str,
) -> ApiResult<()> {
    sqlx::query(
        "update feed_items set status = $2, resolved_note = $3, approval_token = null \
          where id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(note)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Age a card out, and its run with it.
///
/// Both, always: `docs/contract/validate.py` ties an `expired` card to an
/// `expired` run, and a sweep that moved only one would leave the pair saying
/// different things for as long as the resume job took — or for ever, if it
/// dead-lettered.
///
/// # Errors
/// Returns an error if a statement fails.
pub async fn expire_card(tx: &mut Transaction<'_, Postgres>, card: &FeedRow) -> ApiResult<()> {
    settle_card(tx, card.id, "expired", EXPIRED).await?;
    if let (Some(run_id), Some(step_seq)) = (card.run_id, card.step_seq) {
        sqlx::query(
            "update agent_runs set status = 'expired', updated_at = now() \
              where id = $1 and status = 'pending_approval'",
        )
        .bind(run_id)
        .execute(&mut **tx)
        .await?;
        resume::enqueue_in(&mut **tx, run_id, step_seq as Seq, resume::Decision::Expire)
            .await
            .map_err(|err| ApiError::internal("enqueueing the expiry job", &err))?;
    }
    Ok(())
}

/// One overdue card by id, locked, re-checked.
///
/// The sweep lists ids in one short transaction and settles each in its own, so
/// this is where the card's state is confirmed: between the listing and the
/// settle, a person may have answered it.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn claim_expired_one(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> ApiResult<Option<FeedRow>> {
    let row = sqlx::query_as::<_, FeedRow>(&format!(
        "select {COLUMNS} from feed_items \
          where id = $1 and kind = 'approval' and status = 'new' \
            and approval_expires_at <= now() \
          for update"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row)
}

/// The ids of the overdue cards, for the sweep's listing pass.
///
/// **Ids, not rows.** The sweep keeps one field of each and settles each card in
/// its own transaction, so decoding thirteen columns — `body`, `title` and a
/// `jsonb` `data` among them — for up to `BATCH` rows was work thrown away on
/// the next line. `feed_items_expiry_idx` covers the whole predicate, so this is
/// an index-only walk of a normally-empty set.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn claim_expired(pool: &sqlx::PgPool, limit: i64) -> ApiResult<Vec<Uuid>> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "select id from feed_items \
          where kind = 'approval' and status = 'new' and approval_expires_at <= now() \
          order by approval_expires_at \
          limit $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    auth: &Auth,
    action: &str,
    card: &FeedRow,
    pending: &Pending,
) -> ApiResult<()> {
    sqlx::query(
        "insert into audit_log (account_id, actor, action, subject) values ($1, $2, $3, $4)",
    )
    .bind(account_id)
    // The **device**, not `'system'`: this is the one class of write a person
    // asked for, and an audit log that cannot tell those apart is not one.
    .bind(auth.device.id.to_string())
    .bind(action)
    .bind(serde_json::json!({
        "feed_item_id": card.id,
        "run_id": pending.run_id,
        "step_seq": pending.step_seq,
        "tool": pending.request.tool,
        "effect_id": pending.request.effect_id,
    }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
