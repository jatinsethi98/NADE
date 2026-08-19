-- P3 — "Mail stays current".
--
-- A second migration rather than an amendment to 0001_init.sql: sqlx records a
-- checksum per migration and refuses a modified one, so editing 0001 would make
-- every already-migrated dev database fail to boot (backend/DECISIONS.md D23).

-- ------------------------------------------------------------- sync_state --

-- P2 wrote `last_history_id` and read it nowhere; `watch_expiry` has never been
-- touched by any code path. P3 makes both load-bearing and adds the state the
-- scheduler and the recovery sweep need.
alter table sync_state
    -- When `users.watch` last succeeded. The daily renewal is measured from
    -- here rather than from `watch_expiry`, so a registration that failed to
    -- refresh is retried on the normal cadence instead of waiting for the
    -- expiry to come within range.
    add column watch_renewed_at timestamptz,
    -- The topic that registration named. Changing NADE_PUSH_TOPIC has to
    -- re-register, and comparing is the only way to notice.
    add column watch_topic      text,
    -- Last *successful* incremental check, whether a webhook or the poll drove
    -- it. This is what the 30-minute fallback measures, so it is stamped even
    -- when the history page was empty: an empty page is a check that changed
    -- nothing, not a check that did not happen.
    add column last_checked_at  timestamptz,
    -- Last verified Pub/Sub push we accepted. Diagnostic only - it is never an
    -- input to due-ness, because a push that arrives and enqueues nothing must
    -- not suppress the poll.
    add column last_webhook_at  timestamptz,
    -- The durable "a reconciliation sweep is owed" marker, holding the stale
    -- cursor that triggered it.
    --
    -- Without it the sweep is unreachable after a crash. Recovery re-runs the
    -- 30-day sync, which durably commits a *fresh* cursor before returning; if
    -- the process dies between that commit and the sweep, the retry asks
    -- `history.list` from the new cursor, gets a 200 instead of the 404 that
    -- enters recovery, and the sweep never runs again. Deleted and moved mail
    -- would stay live indefinitely with nothing to notice.
    --
    -- So it is set in the same transaction that commits the fresh cursor, and
    -- cleared only when the sweep completes. Every incremental and maintenance
    -- run checks it.
    add column reconcile_after  bigint,
    -- "A notification arrived that no walk has covered yet."
    --
    -- The dedupe index below suppresses a second job while the first is still
    -- pending, which is what stops a burst of pushes becoming a burst of walks.
    -- But it also creates a lost wakeup: a change that lands *after* the running
    -- walk has read its last (empty) history page and *before* the job
    -- completes is suppressed at enqueue and invisible to the walk, so it waits
    -- for the 30-minute poll.
    --
    -- Re-reading the cursor cannot see it either, because the webhook
    -- deliberately never writes the notification's own `historyId`. So the
    -- suppressed enqueue records itself here instead, and the walk clears the
    -- flag and loops again rather than finishing.
    add column rerun_requested  boolean     not null default false;

-- ------------------------------------------------------------------- jobs --

-- Collapses a burst of pushes into one pending walk. Nullable, so every P1/P2
-- enqueue site keeps working untouched and only the callers that opt in are
-- deduplicated. The name mirrors `agent_runs.dedupe_key`, which already exists.
alter table jobs add column dedupe_key text;

-- Nulls are distinct in a unique index, so this constrains only the rows that
-- carry a key.
--
-- The predicate deliberately does NOT mention `locked_by`. Excluding locked
-- rows would let a push enqueue while an earlier walk is still running, but
-- `Queue::fail`, `Queue::release` and `Queue::reap_expired_leases` all set
-- `locked_by = null`, so a replacement row inserted during the run would
-- collide the moment the original failed, was released at shutdown, or had its
-- lease reaped - breaking the queue during exactly the outages it exists for.
-- The race that predicate was solving is closed in the walk instead, which
-- re-reads the cursor and keeps going before it returns.
create unique index jobs_pending_dedupe_idx on jobs (dedupe_key)
    where dedupe_key is not null and done_at is null and dead_at is null;
