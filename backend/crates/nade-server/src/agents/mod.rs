//! The agent runtime: the four tools, the spec compiler, the mail trigger, and
//! the jobs that drive one run from its first turn to its last.

pub mod compile;
pub mod expire;
pub mod feed;
pub mod fence;
pub mod resume;
pub mod run;
pub mod spec;
pub mod tools;
pub mod triage;

/// One `audit_log` row, best-effort.
///
/// Every job in this module writes audit rows, and each had grown its own
/// wrapper around the same `insert … values ($1, 'system', $2, $3)`: `triage`'s
/// took a `Value`, `resume`'s resolved an account and built its own `subject`,
/// `run`'s built a different `subject` from a `RunRow`, and `expire` wrote the
/// statement inline. Four spellings of one row, and the next writer picks
/// whichever it happens to find.
///
/// The `subject` stays the caller's — it is the only part that genuinely
/// differs — and so does the failure policy: an audit row is a record of what
/// happened, never a reason to fail what happened. `let _ =` is deliberate.
pub(crate) async fn audit(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    action: &str,
    subject: serde_json::Value,
) {
    let _ = sqlx::query(
        "insert into audit_log (account_id, actor, action, subject) values ($1, 'system', $2, $3)",
    )
    .bind(account_id)
    .bind(action)
    .bind(subject)
    .execute(pool)
    .await;
}

/// The injection corpus, driven through this crate's own pipeline.
///
/// Lives beside the modules it exercises rather than under any one of them: it
/// spans `mail::parse`, `triage`, `run`, `tools` and `feed`, and belongs to
/// none.
#[cfg(test)]
mod redteam_tests;
