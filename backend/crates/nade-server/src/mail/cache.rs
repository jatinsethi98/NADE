//! Filling the cache from Gmail, on demand.
//!
//! `docs/SEARCH.md`: **Gmail is the index, Postgres is a cache of what we have
//! looked at.** That trade buys search over the whole mailbox from a store a
//! thousandth its size, and it costs exactly one thing - the cache is allowed to
//! be *incomplete*, so anything that reads it has to know when it is looking at
//! a fragment.
//!
//! Two callers fill it. Search stores the messages a query matched. Opening a
//! thread completes that thread, because a search that matched one message of a
//! ten-message conversation cached exactly one message of it, and serving those
//! nine absences as "the conversation" is how a person - or an agent under a
//! token budget - reasons confidently from mail that is not there.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::{
    error::ApiResult,
    gmail::client::{BatchOutcome, GmailClient, MAX_BATCH},
    mail::parse,
    state::AppState,
    sync::store,
};

/// Fetch any of `ids` we do not already hold, parse it, store it, and refresh
/// the rollups of every thread touched.
///
/// A whole batch failing is an error - a page missing ten of its results is not
/// a result. A *single* sub-request failing does not sink the rest, but it is
/// **returned** rather than only logged: the caller is the only one that knows
/// whether an absent message is survivable, and an id that quietly disappears
/// here becomes a short page the reader cannot tell from "no more results".
///
/// Returns the ids it could not resolve.
pub async fn fetch_missing(
    state: &AppState,
    account_id: Uuid,
    client: &GmailClient,
    ids: &[String],
) -> ApiResult<Vec<String>> {
    // EDGE (empty input): nothing matched, so nothing is fetched.
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut unresolved: Vec<String> = Vec::new();

    // EDGE (duplicate delivery): Gmail can list one id twice across a thread and
    // a query; fetching it twice would be waste, not corruption, but the `any()`
    // below is cheaper with it deduped.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let wanted: Vec<String> = ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect();

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
        return Ok(unresolved);
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
                            tracing::warn!(%gmail_id, %error, "unparseable message; caching metadata only");
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
                    tracing::debug!(%gmail_id, "message vanished between list and get");
                }
                // A throttle or a 5xx. The client already decided which
                // failures are worth asking about again (`gmail::types::
                // is_transient`), and throwing that answer away is how the
                // first live sync lost 77 messages to a transient 429.
                BatchOutcome::Failed {
                    gmail_id,
                    status,
                    detail,
                    transient,
                } => {
                    tracing::warn!(%gmail_id, status, %detail, transient, "could not hydrate a message");
                    unresolved.push(gmail_id);
                }
            }
        }

        for thread_id in &threads {
            let mut tx = state.pool.begin().await?;
            store::refresh_thread(&mut tx, account_id, thread_id).await?;
            tx.commit().await?;
        }
    }

    Ok(unresolved)
}

/// What a thread read got: the whole conversation, or as much of it as we hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    /// Every message Gmail has for this thread is in the cache.
    Complete,
    /// Some are missing and we could not get them. The response must say so.
    Partial,
}

impl Completeness {
    #[must_use]
    pub const fn is_partial(self) -> bool {
        matches!(self, Self::Partial)
    }
}

/// Make sure we hold every message in `thread_id`, fetching what is missing.
///
/// Cheap after the first time: the `complete` flag means an opened thread is
/// one local read forever after, and a thread the sync already holds whole is
/// confirmed once and never asked about again.
///
/// Never fails the request. A thread we cannot complete is still worth showing;
/// what would be wrong is showing it *as if* it were whole.
pub async fn ensure_thread_complete(
    state: &AppState,
    account_id: Uuid,
    thread_id: &str,
) -> ApiResult<Completeness> {
    let already: Option<bool> = sqlx::query_scalar(
        "select complete from threads where account_id = $1 and thread_id = $2",
    )
    .bind(account_id)
    .bind(thread_id)
    .fetch_optional(&state.pool)
    .await?;
    if already == Some(true) {
        return Ok(Completeness::Complete);
    }

    let client = state.gmail.client_for(account_id);
    let thread = match client.get_thread_minimal(thread_id).await {
        Ok(thread) => thread,
        Err(error) => {
            // EDGE (429/timeout, dead credential, offline): the cached rows are
            // still the user's mail. Serve them, flagged.
            tracing::warn!(%thread_id, %error, "could not ask Gmail for the whole thread");
            return Ok(Completeness::Partial);
        }
    };

    let ids: Vec<String> = thread.messages.iter().map(|m| m.id.clone()).collect();
    // EDGE (empty input): Gmail knows the thread but listed no messages. Nothing
    // to fetch, and nothing to claim - `complete` stays false so the next open
    // asks again rather than freezing an empty answer.
    if ids.is_empty() {
        return Ok(Completeness::Partial);
    }

    fetch_missing(state, account_id, &client, &ids).await?;

    // Trust the count, not the fetch: a sub-request can fail silently inside a
    // 200 batch, and `complete` must mean "verified", not "attempted".
    let held: i64 = sqlx::query_scalar(
        "select count(*) from messages \
          where account_id = $1 and gmail_id = any($2) and deleted_at is null",
    )
    .bind(account_id)
    .bind(&ids)
    .fetch_one(&state.pool)
    .await?;

    if held < i64::try_from(ids.len()).unwrap_or(i64::MAX) {
        tracing::warn!(
            %thread_id, held, wanted = ids.len(),
            "thread is still short after hydration"
        );
        return Ok(Completeness::Partial);
    }

    // `msg_count` now means the same thing Gmail means by it.
    sqlx::query(
        "update threads set complete = true, msg_count = $3, updated_at = now() \
          where account_id = $1 and thread_id = $2",
    )
    .bind(account_id)
    .bind(thread_id)
    .bind(i32::try_from(ids.len()).unwrap_or(i32::MAX))
    .execute(&state.pool)
    .await?;

    Ok(Completeness::Complete)
}
