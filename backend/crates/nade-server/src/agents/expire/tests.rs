//! The sweep: what is due, and what a sweep leaves behind.

use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{config::Env, test_support::test_app};

/// A card with a deadline, and no run behind it.
///
/// The run-backed path is covered end to end in `api::feed::tests`; this module
/// is about the sweep's own arithmetic and its batching.
async fn card(pool: &sqlx::PgPool, account: Uuid, overdue: bool) -> Uuid {
    sqlx::query_scalar(
        "insert into feed_items \
             (account_id, kind, title, body, data, status, approval_token, approval_expires_at) \
         values ($1, 'approval', 'Tester', 'Save it?', $2::jsonb, 'new', gen_random_uuid(), \
                 now() + case when $3 then interval '-1 second' else interval '1 day' end) \
         returning id",
    )
    .bind(account)
    .bind(json!({
        "action": "write_note", "action_label": "Save note",
        "note_title": "Kettle", "note_id": Uuid::nil(), "thread_id": null
    }))
    .bind(overdue)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn account_of(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar("insert into accounts (email) values ($1) returning id")
        .bind(format!("expire-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn nothing_is_due_until_a_deadline_passes() {
    let app = test_app(Env::Dev).await;
    let account = account_of(&app.db.pool).await;

    assert!(!due(&app.db.pool).await.unwrap(), "an empty feed");
    card(&app.db.pool, account, false).await;
    assert!(!due(&app.db.pool).await.unwrap(), "a live approval");

    card(&app.db.pool, account, true).await;
    assert!(due(&app.db.pool).await.unwrap());
}

#[tokio::test]
async fn a_settled_card_is_never_due_again() {
    let app = test_app(Env::Dev).await;
    let account = account_of(&app.db.pool).await;
    let id = card(&app.db.pool, account, true).await;

    assert_eq!(sweep(&app.state).await.unwrap(), 1);
    assert!(!due(&app.db.pool).await.unwrap());
    // EDGE (duplicate delivery): the ticker may enqueue a second sweep before
    // the first one's job is marked done.
    assert_eq!(sweep(&app.state).await.unwrap(), 0);

    let (status, token, note): (String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "select status, approval_token, resolved_note from feed_items where id = $1",
    )
    .bind(id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(status, "expired");
    assert!(
        token.is_none(),
        "the capability is spent, not merely hidden"
    );
    assert_eq!(
        note.as_deref(),
        Some("Expired after 7 days — nothing was saved.")
    );
}

/// EDGE (expiry, at the boundary): `<= now()`, so a deadline that has just
/// arrived is due and one a second away is not.
#[tokio::test]
async fn the_boundary_is_inclusive() {
    let app = test_app(Env::Dev).await;
    let account = account_of(&app.db.pool).await;
    let id = card(&app.db.pool, account, false).await;

    sqlx::query("update feed_items set approval_expires_at = now() where id = $1")
        .bind(id)
        .execute(&app.db.pool)
        .await
        .unwrap();
    assert!(due(&app.db.pool).await.unwrap());
    assert_eq!(sweep(&app.state).await.unwrap(), 1);
}

#[tokio::test]
async fn the_sweep_audits_every_card_it_ages_out() {
    let app = test_app(Env::Dev).await;
    let account = account_of(&app.db.pool).await;
    for _ in 0..3 {
        card(&app.db.pool, account, true).await;
    }

    assert_eq!(sweep(&app.state).await.unwrap(), 3);
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from audit_log where action = 'feed.expire' and account_id = $1",
    )
    .bind(account)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(audited, 3);
}

#[tokio::test]
async fn a_live_approval_is_left_alone() {
    let app = test_app(Env::Dev).await;
    let account = account_of(&app.db.pool).await;
    let live = card(&app.db.pool, account, false).await;
    card(&app.db.pool, account, true).await;

    assert_eq!(sweep(&app.state).await.unwrap(), 1);
    let status: String = sqlx::query_scalar("select status from feed_items where id = $1")
        .bind(live)
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(status, "new");
}

/// An `info` card has no deadline and nothing to expire, and a resolved
/// approval keeps its `approval_expires_at` for ever — "what lets an expired
/// card say *when* it expired". Neither may be swept.
#[tokio::test]
async fn info_cards_and_already_settled_approvals_are_not_swept() {
    let app = test_app(Env::Dev).await;
    let account = account_of(&app.db.pool).await;

    sqlx::query(
        "insert into feed_items (account_id, kind, title, body, data, status) \
         values ($1, 'info', 'Agent', 'Saved a note.', $2::jsonb, 'new')",
    )
    .bind(account)
    .bind(crate::agents::feed::info_data(None, None, None))
    .execute(&app.db.pool)
    .await
    .unwrap();

    let settled = card(&app.db.pool, account, true).await;
    sqlx::query("update feed_items set status = 'resolved', approval_token = null where id = $1")
        .bind(settled)
        .execute(&app.db.pool)
        .await
        .unwrap();

    assert!(!due(&app.db.pool).await.unwrap());
    assert_eq!(sweep(&app.state).await.unwrap(), 0);
}
