-- NADE initial schema.
--
-- This is PLAN.md §Postgres schema expanded into valid PostgreSQL 16+ DDL.
-- The plan's shorthand is resolved as follows:
--   `id uuid pk`          -> `id uuid primary key default gen_random_uuid()`
--                            (gen_random_uuid() is core since PG13 - no pgcrypto)
--   `bigint identity pk`  -> `bigint generated always as identity primary key`
--   `references accounts` -> `references accounts(id) on delete cascade`
--   `default now()`       -> `not null default now()`
-- `not null` is added wherever the plan implies a value always exists.
--
-- Idempotency: sqlx's migrator records applied versions in `_sqlx_migrations`
-- and refuses to re-run them, so `just migrate` is a no-op on an already
-- migrated database. The DDL itself is deliberately NOT `if not exists`: a
-- schema drift should fail loudly rather than be silently skipped.

-- ---------------------------------------------------------------- accounts --

create table accounts (
    id         uuid        primary key default gen_random_uuid(),
    email      text        not null unique,
    status     text        not null default 'ok' check (status in ('ok', 'needs_reauth')),
    created_at timestamptz not null default now()
);

create table gmail_tokens (
    account_id    uuid primary key references accounts (id) on delete cascade,
    -- AES-GCM ciphertext, rewritten on EVERY refresh (P2).
    access_token  text,
    access_expiry timestamptz,
    refresh_token text,
    scopes        text[],
    updated_at    timestamptz not null default now()
);

-- ------------------------------------------------------------------- mail --

create table messages (
    id               bigint      generated always as identity primary key,
    account_id       uuid        not null references accounts (id) on delete cascade,
    gmail_id         text        not null,
    thread_id        text        not null,
    history_id       bigint,
    internal_ts      timestamptz,
    from_name        text,
    from_email       text,
    to_json          jsonb       not null default '[]'::jsonb,
    cc_json          jsonb       not null default '[]'::jsonb,
    subject          text,
    snippet          text,
    body_text        text,
    body_html        text,
    label_ids        text[]      not null default '{}',
    has_attachments  boolean     not null default false,
    deleted_at       timestamptz,
    constraint messages_account_id_gmail_id_key unique (account_id, gmail_id)
);

-- There is deliberately **no full-text index here**, and there never will be.
-- `docs/SEARCH.md`: Gmail is the search index and this table is a cache of what
-- we have looked at. The generated `fts` tsvector this schema used to carry
-- indexed the 30-day sync window - ~500 of 63,866 messages, 0.78% - so a search
-- for anything older returned an empty result indistinguishable from "no such
-- mail exists". `GET /v1/search` now calls `users.messages.list`, which covers
-- the whole mailbox, and hydrates the misses into these rows.
--
-- `db::tests::nothing_maintains_a_second_index` enforces the absence.
create index messages_account_ts_idx      on messages (account_id, internal_ts desc);
create index messages_account_thread_idx  on messages (account_id, thread_id);
create index messages_label_ids_idx       on messages using gin (label_ids);

-- Attachment *metadata* only. The bytes are never stored: the proxy endpoint
-- refetches them from Gmail on demand (API.md §2, PLAN.md §Gmail sync 7).
--
-- `att_id` is assigned by the parser, not by Gmail. `format=raw` - the format
-- the sync fetches, because that is the only one our MIME parser can see - does
-- not carry Gmail's `attachmentId` anywhere. See backend/DECISIONS.md D14.
create table attachments (
    id         bigint  generated always as identity primary key,
    message_id bigint  not null references messages (id) on delete cascade,
    att_id     text    not null,
    name       text    not null default '',
    mime       text    not null default 'application/octet-stream',
    size_bytes bigint  not null default 0,
    -- The `Content-ID` header, without the angle brackets. Null for a part that
    -- nothing references.
    content_id text,
    -- True when the HTML references this part with `cid:`.
    inline     boolean not null default false,
    constraint attachments_message_id_att_id_key unique (message_id, att_id)
);

-- ------------------------------------------------------- thread rollups --

-- The mail list reads threads, not messages, so the rollup the list needs is
-- maintained at ingest rather than aggregated per request. Two tables rather
-- than one because the list is *filtered by label and ordered by time*, and a
-- single btree over (account_id, label_id, last_ts desc, thread_id desc) is
-- exactly the keyset walk `GET /mailboxes/{id}/threads` performs.
-- backend/DECISIONS.md D13.
create table threads (
    account_id uuid        not null references accounts (id) on delete cascade,
    thread_id  text        not null,
    -- Newest message in the thread, inside the synced window.
    last_ts    timestamptz not null,
    subject    text        not null default '',
    snippet    text        not null default '',
    from_name  text        not null default '',
    from_email text        not null default '',
    -- True when ANY message in the thread is unread (API.md §2).
    unread     boolean     not null default false,
    msg_count  integer     not null default 0,
    updated_at timestamptz not null default now(),
    primary key (account_id, thread_id)
);

create index threads_keyset_idx on threads (account_id, last_ts desc, thread_id desc);

create table thread_labels (
    account_id uuid        not null,
    thread_id  text        not null,
    label_id   text        not null,
    -- Denormalised from `threads` so the index below is a total keyset order
    -- and the list never has to sort.
    last_ts    timestamptz not null,
    unread     boolean     not null default false,
    primary key (account_id, thread_id, label_id),
    foreign key (account_id, thread_id)
        references threads (account_id, thread_id) on delete cascade
);

create index thread_labels_keyset_idx
    on thread_labels (account_id, label_id, last_ts desc, thread_id desc);

create table labels (
    account_id uuid not null references accounts (id) on delete cascade,
    label_id   text not null,
    name       text,
    type       text,
    primary key (account_id, label_id)
);

create table sync_state (
    account_id      uuid primary key references accounts (id) on delete cascade,
    last_history_id bigint,
    watch_expiry    timestamptz,
    updated_at      timestamptz not null default now()
);

-- ----------------------------------------------------------------- agents --

create table agents (
    id                uuid        primary key default gen_random_uuid(),
    account_id        uuid        not null references accounts (id) on delete cascade,
    name              text        not null,
    nl_definition     text        not null,
    spec              jsonb,
    status            text        not null default 'draft'
                      check (status in ('draft', 'published', 'paused')),
    allowed_tools     text[]      not null default '{}',
    approval_required boolean     not null default true,
    triage_model      text,
    run_model         text,
    -- Recurrence model, PLAN.md §Product contract §Schedule model.
    schedule          jsonb,
    -- Derived from `schedule` with chrono-tz (P7); never set by the client.
    next_run_at       timestamptz,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now()
);

create index agents_account_idx     on agents (account_id);
create index agents_next_run_at_idx on agents (next_run_at) where next_run_at is not null;

create table agent_runs (
    id                   uuid        primary key default gen_random_uuid(),
    agent_id             uuid        not null references agents (id) on delete cascade,
    account_id           uuid        not null references accounts (id) on delete cascade,
    trigger_kind         text        not null check (trigger_kind in ('mail', 'schedule', 'manual')),
    trigger_ref          text,
    -- Nulls are distinct in a unique index, so unscheduled/manual runs may all
    -- leave this null while triggered runs stay exactly-once.
    dedupe_key           text        unique,
    status               text        not null default 'queued'
                         check (status in ('queued', 'running', 'pending_approval',
                                           'waiting', 'done', 'failed', 'expired', 'skipped')),
    pending_action       jsonb,
    approval_token       uuid,
    approval_expires_at  timestamptz,
    wake_at              timestamptz,
    tokens_spent         integer     not null default 0,
    attempt              integer     not null default 0,
    error                text,
    created_at           timestamptz not null default now(),
    updated_at           timestamptz not null default now()
);

create index agent_runs_status_wake_idx on agent_runs (status, wake_at);
create index agent_runs_agent_idx       on agent_runs (agent_id, created_at desc);

-- Append-only. Both the crash-recovery log and the UI's run log.
create table run_journal (
    run_id     uuid        not null references agent_runs (id) on delete cascade,
    seq        integer     not null,
    kind       text        not null,
    payload    jsonb       not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    primary key (run_id, seq)
);

-- ------------------------------------------------------ agent side effects --

-- `id` is uuid5(run_id || seq) when agent-written, so a replayed step upserts
-- instead of duplicating (PLAN.md §Exactly-once side effects).
create table notes (
    id         uuid        primary key,
    account_id uuid        not null references accounts (id) on delete cascade,
    run_id     uuid        references agent_runs (id) on delete set null,
    thread_id  text,
    title      text        not null,
    body_md    text        not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index notes_account_updated_idx on notes (account_id, updated_at desc);

-- Same deterministic-id rule as `notes`.
create table drafts (
    id         uuid        primary key,
    account_id uuid        not null references accounts (id) on delete cascade,
    run_id     uuid        references agent_runs (id) on delete set null,
    -- API.md §3 gives a draft row a `thread_id: string|null`; without it a draft
    -- has no way back to the thread it answers.
    thread_id  text,
    to_json    jsonb       not null default '[]'::jsonb,
    subject    text,
    body_text  text        not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index drafts_account_updated_idx on drafts (account_id, updated_at desc);

create table feed_items (
    id            uuid        primary key default gen_random_uuid(),
    account_id    uuid        not null references accounts (id) on delete cascade,
    run_id        uuid        references agent_runs (id) on delete set null,
    kind          text        not null check (kind in ('approval', 'info')),
    title         text        not null,
    body          text,
    data          jsonb,
    status        text        not null default 'new'
                  check (status in ('new', 'resolved', 'skipped', 'expired')),
    resolved_note text,
    created_at    timestamptz not null default now()
);

create index feed_items_account_status_created_idx
    on feed_items (account_id, status, created_at desc);

-- --------------------------------------------------------------- settings --

-- Exactly one row, ever. `singleton` is a generated-by-default column pinned to
-- `true` by a check and made unique, which is the cheapest way to say "one row"
-- in DDL that the database itself enforces - `GET /settings` can then read it
-- without an account id and `PATCH` can upsert on the constraint.
create table settings (
    singleton                 boolean     primary key default true
                              check (singleton),
    approval_required_default boolean     not null default true,
    updated_at                timestamptz not null default now()
);

insert into settings (singleton) values (true);

-- ------------------------------------------------------- audit & devices --

-- account_id is nullable: system-level events (a dead-lettered job, say)
-- belong to no mailbox.
create table audit_log (
    id         bigint      generated always as identity primary key,
    account_id uuid        references accounts (id) on delete set null,
    actor      text        not null,
    action     text        not null,
    subject    jsonb,
    created_at timestamptz not null default now()
);

create index audit_log_created_idx on audit_log (created_at desc);

-- account_id is nullable: a device pairs before any Gmail account is linked
-- (pairing is P1, OAuth is P2).
create table devices (
    id           uuid        primary key default gen_random_uuid(),
    account_id   uuid        references accounts (id) on delete cascade,
    apns_token   text        unique,
    -- sha256(bearer token), lowercase hex. The plaintext token is returned to
    -- the device exactly once and never stored.
    bearer_hash  text        not null unique,
    device_name  text        not null,
    last_seen_at timestamptz,
    created_at   timestamptz not null default now(),
    -- Tokens are revocable (PLAN.md §Auth bootstrap) without deleting history.
    revoked_at   timestamptz
);

-- ------------------------------------------------------------------- jobs --

create table jobs (
    id               bigint      generated always as identity primary key,
    kind             text        not null,
    payload          jsonb       not null default '{}'::jsonb,
    run_after        timestamptz not null default now(),
    attempts         integer     not null default 0,
    locked_by        text,
    -- Heartbeat every 60 s extends this; the reaper reclaims only expired
    -- leases and never touches a live one.
    lease_expires_at timestamptz,
    done_at          timestamptz,
    error            text,
    created_at       timestamptz not null default now(),
    -- Terminal dead-letter marker (see backend/DECISIONS.md D3). `done_at`
    -- stays null - the job never succeeded - but `dead_at` removes the row from
    -- both the claim predicate and the partial index below, so it is
    -- claimable-never.
    dead_at          timestamptz
);

-- The claim query is
--   select id from jobs
--    where done_at is null and dead_at is null
--      and run_after <= now()
--      and (lease_expires_at is null or lease_expires_at < now())
--      and kind = any($kinds)
--    order by run_after
--    for update skip locked limit 1
-- so this partial index over the pending set, ordered by run_after, is exactly
-- what it walks. Verified by jobs::tests::claim_query_uses_an_index_not_a_seq_scan.
create index jobs_ready_idx on jobs (run_after) where done_at is null and dead_at is null;
