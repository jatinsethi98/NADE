//! Search, delegated to Gmail. `docs/SEARCH.md` is the specification.
//!
//! **Gmail is the search index. Postgres is a cache of what we have looked at.**
//!
//! The `messages` table used to carry a generated `tsvector` over the 30-day
//! sync window. It indexed ~500 of 63,866 messages - 0.78% - so a search for an
//! invoice from March returned zero rows, which is indistinguishable from "no
//! such mail exists". The user was not told their archive was never indexed;
//! they were told, silently, that their memory was wrong.
//!
//! So a search now:
//!
//! 1. **validates the query** ([`query`]), because Gmail answers a malformed
//!    `q` with an empty `200` and never a `400`;
//! 2. asks `users.messages.list` for one page of ids over the **whole**
//!    mailbox;
//! 3. hydrates those ids from `messages`, fetching the misses from Gmail in
//!    batches and storing them, so the cache warms as a side effect;
//! 4. returns the thread rows **in Gmail's order**. Google's ranking is the
//!    product here; re-sorting by date would throw it away.
//!
//! One path, one shape. `GET /v1/search` is a thin wrapper over [`search`], and
//! P4's `search_mail` tool calls the same function rather than growing a second
//! search with a second set of traps.

pub mod query;

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::{
    api::{
        cursor,
        mail::{agent_notes, ThreadRow, ThreadSummary},
    },
    error::ApiResult,
    gmail::{client::GmailClient, types::MessageRef},
    mail::cache,
    state::AppState,
    sync::store,
};

pub use query::{LabelIndex, NormalisedQuery, Rejection, MAX_QUERY_CHARS};

/// How many message ids one page asks Gmail for.
///
/// `API.md` §0 fixes `GET /search` at 50 per page. Those 50 messages collapse
/// into *at most* 50 threads, and a page whose results are all cache misses
/// costs 5 (`messages.list`) + 50 x 5 (`messages.get`) = 255 quota units and
/// five sequential batches. A warm page costs 5 units and one round trip.
pub const PAGE_SIZE: usize = 50;

/// One page of results, in Gmail's relevance order.
#[derive(Debug)]
pub struct SearchPage {
    pub threads: Vec<ThreadSummary>,
    /// Gmail's `nextPageToken`, wrapped in our opaque cursor.
    pub next_cursor: Option<String>,
}

impl SearchPage {
    /// `API.md` §0: an empty collection is an empty array **and**
    /// `next_cursor: null` - never a 404.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            threads: Vec::new(),
            next_cursor: None,
        }
    }
}

/// Search the whole mailbox.
///
/// The single entry point: `GET /v1/search` and (from P4) the agents'
/// `search_mail` tool both come through here, so the validation, the
/// hydration, the ordering and the cursor cannot diverge between them.
///
/// `account_id` is the **resolved** account - the handler's [`crate::api::
/// auth::Auth`], or the agent run's own account at P4 - never a fresh "the
/// account" lookup, which no longer exists (backend/DECISIONS.md D45).
///
/// # Errors
/// * `400 bad_request` - the query was refused ([`query::validate`] says why),
///   or the cursor is not one of ours.
/// * `409 needs_reauth` / `502 upstream_unavailable` - Gmail. Search is a
///   network call now, and it says so rather than serving a partial page from
///   the cache and calling it a result.
pub async fn search(
    state: &AppState,
    account_id: Option<Uuid>,
    raw_query: &str,
    cursor: Option<&str>,
) -> ApiResult<SearchPage> {
    let Some(account_id) = account_id else {
        // No Gmail resolved. Validate anyway: "nothing is connected" must not
        // turn a malformed query into an empty page, which is the exact
        // ambiguity this module exists to remove.
        let valid = query::validate(raw_query, &LabelIndex::empty())?;
        // EDGE (cursor): and a bad cursor is still a 400 here, so a client does
        // not learn a different answer depending on whether mail is connected.
        cursor
            .map(|raw| cursor::decode_page_token(raw, valid.as_str()))
            .transpose()?;
        return Ok(SearchPage::empty());
    };

    let labels = label_index(state, account_id).await?;
    let valid = query::validate(raw_query, &labels)?;
    // Opaque in, opaque out, and a corrupt one is a 400 rather than a silent
    // reset to page one - the same rule as the keyset cursors (`API.md` §0).
    // After validation, because the cursor is bound to the *canonical* query:
    // `from:A  OR  from:B` and `from:a or from:b` are one search, and a cursor
    // must survive the user retyping it.
    let page_token = cursor
        .map(|raw| cursor::decode_page_token(raw, valid.as_str()))
        .transpose()?;
    tracing::debug!(
        query = valid.as_str(),
        widens_scope = valid.widens_scope(),
        "searching gmail"
    );

    let client = state.gmail.client_for(account_id);
    let listed = client
        .list_message_page(valid.as_str(), page_token.as_deref(), PAGE_SIZE)
        .await
        .map_err(crate::api::mail::upstream)?;

    hydrate(state, account_id, &client, &listed.messages).await?;

    let order = thread_order(&listed.messages);
    let rows = rows_in_gmail_order(state, account_id, &order).await?;
    let notes = agent_notes(state, account_id, &rows).await?;

    Ok(SearchPage {
        threads: rows
            .into_iter()
            .map(|row| {
                let note = notes.get(row.thread_id()).cloned();
                row.into_summary(note)
            })
            .collect(),
        next_cursor: listed
            .next_page_token
            .as_deref()
            .map(|token| cursor::encode_page_token(token, valid.as_str())),
    })
}

/// The account's labels, so `label:Label_25` can be translated into
/// `label:"To Reply"` rather than only complained about.
async fn label_index(state: &AppState, account_id: Uuid) -> ApiResult<LabelIndex> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select label_id, coalesce(name, label_id) from labels where account_id = $1",
    )
    .bind(account_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(LabelIndex::new(rows))
}

/// Gmail's result order, collapsed to threads, first appearance wins.
fn thread_order(refs: &[MessageRef]) -> Vec<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut order: Vec<String> = Vec::with_capacity(refs.len());
    for entry in refs {
        // Ingest falls back to the message id when Gmail sends no `threadId`;
        // do the same, so a thread row is findable either way.
        let thread_id = entry.thread_id.as_deref().unwrap_or(&entry.id);
        if seen.insert(thread_id) {
            order.push(thread_id.to_owned());
        }
    }
    order
}

/// Fetch anything Gmail returned that we do not already hold, and store it.
///
/// The cache warms as a side effect: a message found by search is a message
/// opening the thread will not have to fetch again. Only the *matching*
/// messages, though - completing the conversation around them is the thread
/// endpoint's job (`crate::mail::cache::ensure_thread_complete`).
async fn hydrate(
    state: &AppState,
    account_id: Uuid,
    client: &GmailClient,
    refs: &[MessageRef],
) -> ApiResult<()> {
    let ids: Vec<String> = refs.iter().map(|entry| entry.id.clone()).collect();
    let unresolved = cache::fetch_missing(state, account_id, client, &ids).await?;

    // A result Gmail matched but we could not fetch is dropped from the page by
    // `rows_in_gmail_order`, and a short page is indistinguishable from "there
    // were fewer matches". `ThreadsResponse` has nowhere to say so yet, so at
    // minimum the loss is recorded rather than silent.
    if !unresolved.is_empty() {
        tracing::warn!(
            count = unresolved.len(),
            "search results Gmail matched but we could not hydrate"
        );
        store::audit(
            &state.pool,
            account_id,
            "search_hydration_incomplete",
            serde_json::json!({ "unresolved": unresolved }),
        )
        .await
        .ok();
    }
    Ok(())
}

/// The thread rows for `order`, **in `order`** - Gmail's relevance ranking,
/// which is the whole reason for delegating search in the first place.
async fn rows_in_gmail_order(
    state: &AppState,
    account_id: Uuid,
    order: &[String],
) -> ApiResult<Vec<ThreadRow>> {
    if order.is_empty() {
        return Ok(Vec::new());
    }

    let mut found = fetch_rows(state, account_id, order).await?;

    // A thread Gmail returned that has no rollup. `hydrate` refreshes anything
    // it stored, so this only happens when the message was already cached but
    // its rollup was not - and dropping a genuine result to preserve that
    // inconsistency would be the same silence this module exists to remove.
    let missing: Vec<String> = order
        .iter()
        .filter(|thread_id| !found.contains_key(*thread_id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        for thread_id in &missing {
            let mut tx = state.pool.begin().await?;
            store::refresh_thread(&mut tx, account_id, thread_id).await?;
            tx.commit().await?;
        }
        found = fetch_rows(state, account_id, order).await?;
    }

    Ok(order
        .iter()
        .filter_map(|thread_id| found.remove(thread_id))
        .collect())
}

async fn fetch_rows(
    state: &AppState,
    account_id: Uuid,
    order: &[String],
) -> ApiResult<HashMap<String, ThreadRow>> {
    let rows: Vec<ThreadRow> = sqlx::query_as(
        "select t.thread_id, t.subject, t.snippet, t.from_name, t.from_email, \
                t.last_ts, t.unread, t.msg_count \
           from threads t \
          where t.account_id = $1 and t.thread_id = any($2)",
    )
    .bind(account_id)
    .bind(order)
    .fetch_all(&state.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| (row.thread_id().to_owned(), row))
        .collect())
}

#[cfg(test)]
mod tests;
