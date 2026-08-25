-- P5 "The loop closes": the approval capability, and what the mail list and
-- the thread screen read back off a feed item.
--
-- A sixth migration rather than an amendment to 0001: sqlx checksums each
-- migration and refuses a modified one, so editing an applied file makes every
-- already-migrated dev database fail to boot (backend/DECISIONS.md D23).

-- ------------------------------------------------------------ feed_items --

-- **The approval capability belongs to the card, not to the run.**
--
-- 0001 put `approval_token` and `approval_expires_at` on `agent_runs`, where
-- nothing ever read or wrote them. Using them now would be wrong three times:
--
--   * a run can pause more than once - `max_steps` is 12, and an agent holding
--     both `write_note` and `draft_reply` can be gated, approved, resumed and
--     gated again - so the second pause would overwrite the first card's token
--     and deadline;
--   * `API.md` §7 keeps `approval_expires_at` non-null on a resolved or expired
--     card, "what lets an expired card say *when* it expired". Read through the
--     run, that value changes under the finished card;
--   * the SDK says so outright (`run.rs`, `Resolution`'s docs): "a host that
--     issues its own single-use token per approval (NADE's `approval_token`,
--     one per feed item) should store `step_seq` beside it and pass it back".
--
-- `step_seq` is stored rather than re-read from `agent_runs.pending_action`
-- because that column holds only the *current* pause. It is what addresses the
-- decision to exactly one step, and it is half of what makes a stale card's
-- token refuse rather than land on whatever is open now.
alter table feed_items
    add column approval_token      uuid,
    add column approval_expires_at timestamptz,
    add column step_seq            integer,
    -- `API.md` §2's `agent_note`, and the thread a card belongs to.
    --
    -- Not `data->>'agent_note'`, which is what `api/mail.rs` was reading and
    -- what a P5 comment there promised to write. `data` is served **verbatim**
    -- and `docs/contract/validate.py`'s `OBJ` is an exact key set - "a missing
    -- key and an extra key are both violations" - so a smuggled `agent_note`
    -- would have put the first live thread list in breach of its own contract
    -- test. `thread_id` as a column also gives the join an index, which
    -- `data->>'thread_id' = any($2)` can never use.
    add column thread_id           text,
    add column agent_note          text,

    -- **Why the system cards' `reason` had to come out of `data`.**
    --
    -- Two writers - the spend-ceiling notice (`agents/run.rs`) and the
    -- needs-reauth notice (`gmail/oauth.rs`) - put `"reason": "..."` inside
    -- `data`, and both used `data->>'reason'` as the guard that stops a second
    -- card being raised. `data` is served **verbatim** by `GET /feed`, and
    -- `FEED_DATA`'s `none` shape in `docs/contract/validate.py` is an exact key
    -- set, so every one of those rows was a contract violation waiting for the
    -- day `/feed` was mounted. It moves to a column, where it is a server fact
    -- and not a wire one.
    add column reason              text,

    -- `POST /feed/seen` is "a best-effort read receipt fired as the user
    -- scrolls" (`API.md` §7). The needs-reauth card is the only surface that
    -- says sync has stopped, and clearing it by scrolling past it would leave a
    -- dead mailbox with nothing on screen - while `save_consent`'s reconciler
    -- only matches `status = 'new'`, so it would never come back either.
    add column dismissible         boolean not null default true;

-- The two live writers' rows, carried over rather than left in breach. A dev
-- database that has ever hit the ceiling or re-consented holds them today.
update feed_items
   set reason        = data->>'reason',
       dismissible   = (data->>'reason') is distinct from 'needs_reauth',
       data          = data - 'reason',
       -- `API.md` §7: `resolved_note` is null for every `info` item. The
       -- reconnect path was writing 'Reconnected.' onto one.
       resolved_note = case when kind = 'info' then null else resolved_note end
 where data ? 'reason' or (kind = 'info' and resolved_note is not null);

-- The guards both writers use, and the seen endpoint's predicate.
create index feed_items_reason_idx on feed_items (account_id, reason)
    where reason is not null and status = 'new';

-- One card per gated step, so re-raising after a job retry is an upsert and
-- not a second card with a second token. `run_id` is nullable (the FK is
-- `on delete set null`), and both halves must be present for the constraint to
-- mean anything - a partial index is how that is said.
create unique index feed_items_run_step_idx on feed_items (run_id, step_seq)
    where run_id is not null and step_seq is not null;

-- The token *is* the capability, so two cards must never share one. Uniqueness
-- only, with no reader: approve/skip select by (id, account_id) and compare the
-- token in Rust, so nothing here is a lookup path.
create unique index feed_items_token_idx on feed_items (approval_token)
    where approval_token is not null;

-- The thread screen asks each run for its newest card. `feed_items_run_step_idx`
-- above cannot serve it: that index is partial on `step_seq is not null`, and
-- an `info` card has no step - so a query keyed on `run_id` alone cannot prove
-- the predicate and falls back to a sequential scan of the whole table, once
-- per run on the screen. This one also covers `abandon` and `settle_cards_of`.
create index feed_items_run_created_idx on feed_items (run_id, created_at desc)
    where run_id is not null;

-- `GET /feed` is the whole feed for an account, newest first, keyset on
-- `(created_at, id)`. The 0001 index leads with `status`, which that query does
-- not filter on, so it cannot serve it.
create index feed_items_account_created_idx
    on feed_items (account_id, created_at desc, id desc);

-- The mail list's `agent_note` join and the thread screen's agent cards.
create index feed_items_thread_idx on feed_items (account_id, thread_id)
    where thread_id is not null;

-- The hourly expiry sweep: the whole predicate, so it is an index-only walk of
-- a set that is normally empty.
create index feed_items_expiry_idx on feed_items (approval_expires_at)
    where kind = 'approval' and status = 'new';

-- --------------------------------------------------------- notes, drafts --

-- The other two branches of the thread screen's union. 0001 indexes both tables
-- on `(account_id, updated_at desc)`, which leads with the wrong column for
-- "what did agents write about *this thread*" - so both were scanned in full on
-- every thread open.
create index notes_account_thread_idx on notes (account_id, thread_id)
    where thread_id is not null;
create index drafts_account_thread_idx on drafts (account_id, thread_id)
    where thread_id is not null;

-- ------------------------------------------------------------- agent_runs --

-- Dead since 0001 and, per the comment above, they must stay dead: leaving them
-- is leaving a second place to look for the same fact, and the wrong one.
alter table agent_runs
    drop column approval_token,
    drop column approval_expires_at;

-- The thread screen's agent cards ask "which runs fired on this thread", and
-- `trigger_ref` is a *message* id (`API.md` §6), so the answer is a join
-- through `messages`. Without this the join scans every run the account has.
create index agent_runs_trigger_ref_idx on agent_runs (account_id, trigger_ref)
    where trigger_ref is not null;

-- The per-agent daily cap on mail-triggered runs (PLAN.md §Dev caps). The 0001
-- index is `(agent_id, created_at desc)` and serves the range; the partial
-- predicate keeps this one to the rows the cap actually counts.
create index agent_runs_agent_mail_day_idx on agent_runs (agent_id, created_at desc)
    where trigger_kind = 'mail';
