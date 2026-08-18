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
    gmail::{
        client::{BatchOutcome, GmailClient, MAX_BATCH},
        types::MessageRef,
    },
    mail::parse,
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
/// # Errors
/// * `400 bad_request` - the query was refused ([`query::validate`] says why),
///   or the cursor is not one of ours.
/// * `409 needs_reauth` / `502 upstream_unavailable` - Gmail. Search is a
///   network call now, and it says so rather than serving a partial page from
///   the cache and calling it a result.
pub async fn search(
    state: &AppState,
    raw_query: &str,
    cursor: Option<&str>,
) -> ApiResult<SearchPage> {
    // Opaque in, opaque out, and a corrupt one is a 400 rather than a silent
    // reset to page one - the same rule as the keyset cursors (`API.md` §0).
    let page_token = cursor.map(cursor::decode_page_token).transpose()?;

    let Some(account) = state.account().await? else {
        // No Gmail connected. Validate anyway: "nothing is connected" must not
        // turn a malformed query into an empty page, which is the exact
        // ambiguity this module exists to remove.
        query::validate(raw_query, &LabelIndex::empty())?;
        return Ok(SearchPage::empty());
    };

    let labels = label_index(state, account.id).await?;
    let valid = query::validate(raw_query, &labels)?;
    tracing::debug!(
        query = valid.as_str(),
        widens_scope = valid.widens_scope(),
        "searching gmail"
    );

    let client = state.gmail.client_for(account.id);
    let listed = client
        .list_message_page(valid.as_str(), page_token.as_deref(), PAGE_SIZE)
        .await
        .map_err(crate::api::mail::upstream)?;

    hydrate(state, account.id, &client, &listed.messages).await?;

    let order = thread_order(&listed.messages);
    let rows = rows_in_gmail_order(state, account.id, &order).await?;
    let notes = agent_notes(state, account.id, &rows).await?;

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
            .map(cursor::encode_page_token),
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
/// opening the thread will not have to fetch again.
///
/// A whole batch failing is a `502` - a page missing ten of its results is not
/// a result. A *single* sub-request failing is logged and skipped, because
/// losing one row is better than losing the other forty-nine.
async fn hydrate(
    state: &AppState,
    account_id: Uuid,
    client: &GmailClient,
    refs: &[MessageRef],
) -> ApiResult<()> {
    let mut wanted: Vec<String> = Vec::with_capacity(refs.len());
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for entry in refs {
        if seen.insert(&entry.id) {
            wanted.push(entry.id.clone());
        }
    }
    // EDGE (empty input): nothing matched, so nothing is fetched.
    if wanted.is_empty() {
        return Ok(());
    }

    let cached: Vec<String> = sqlx::query_scalar(
        "select gmail_id from messages \
          where account_id = $1 and gmail_id = any($2) and deleted_at is null",
    )
    .bind(account_id)
    .bind(&wanted)
    .fetch_all(&state.pool)
    .await?;
    let cached: BTreeSet<String> = cached.into_iter().collect();

    let misses: Vec<String> = wanted
        .into_iter()
        .filter(|id| !cached.contains(id))
        .collect();
    if misses.is_empty() {
        return Ok(());
    }

    for chunk in misses.chunks(MAX_BATCH) {
        let outcomes = client
            .batch_get_raw(chunk)
            .await
            .map_err(crate::api::mail::upstream)?;

        let mut threads: BTreeSet<String> = BTreeSet::new();
        for outcome in outcomes {
            match outcome {
                BatchOutcome::Fetched {
                    gmail_id,
                    message,
                    raw,
                } => {
                    // Exactly the sync's rule: an unparseable message becomes a
                    // metadata-only row rather than a hole.
                    let row = match parse::parse(&raw, &gmail_id) {
                        Ok(parsed) => store::IngestRow::parsed(&message, parsed),
                        Err(error) => {
                            tracing::warn!(%gmail_id, %error, "unparseable search result; caching metadata only");
                            store::IngestRow::metadata_only(&message)
                        }
                    };
                    let mut tx = state.pool.begin().await?;
                    store::upsert_message(&mut tx, account_id, &row).await?;
                    tx.commit().await?;
                    threads.insert(row.thread_id.clone());
                }
                // Deleted between the list and the get. Normal; it simply does
                // not appear.
                BatchOutcome::Gone { gmail_id } => {
                    tracing::debug!(%gmail_id, "search result vanished between list and get");
                }
                BatchOutcome::Failed {
                    gmail_id,
                    status,
                    detail,
                    ..
                } => {
                    tracing::warn!(%gmail_id, status, %detail, "could not hydrate a search result");
                }
            }
        }

        for thread_id in &threads {
            let mut tx = state.pool.begin().await?;
            store::refresh_thread(&mut tx, account_id, thread_id).await?;
            tx.commit().await?;
        }
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
