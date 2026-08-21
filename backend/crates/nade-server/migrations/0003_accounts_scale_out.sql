-- Any number of accounts on one server, each device bound to its account.
--
-- A third migration rather than an amendment to 0001/0002: sqlx records a
-- checksum per migration and refuses a modified one, so editing an applied
-- file makes every already-migrated dev database fail to boot
-- (backend/DECISIONS.md D23).

-- --------------------------------------------------------------- settings --

-- `settings` was a DDL-enforced singleton - one row for the whole server -
-- which encoded "one server serves one mailbox" into the schema. Resolution is
-- per-account now (backend/DECISIONS.md D45), so the setting has to be too:
-- two users on one server must be able to disagree about whether approvals are
-- required by default.
--
-- The old value is carried over to every existing account rather than reset:
-- the migration must not silently flip a mailbox back to requiring approvals
-- it had turned off (or the reverse).
alter table settings rename to settings_singleton;

create table settings (
    account_id                uuid        primary key references accounts (id) on delete cascade,
    approval_required_default boolean     not null default true,
    updated_at                timestamptz not null default now()
);

insert into settings (account_id, approval_required_default)
select id,
       coalesce((select approval_required_default from settings_singleton), true)
  from accounts;

drop table settings_singleton;

-- New accounts get their row where accounts are created: the consent path
-- (`TokenStore::save_consent`) inserts it in the same transaction as the
-- `accounts` row, so `GET /settings` (P4) never has to invent a default at
-- read time.

-- --------------------------------------------------------------- messages --

-- The GIN over `label_ids` was insurance for a query that never arrived: no
-- statement in the crate uses a GIN-servable operator (`@>`, `&&`, `<@`) on
-- the column - label reads go through `thread_labels`, which has its own
-- keyset index. An unused GIN is not free: every message upsert pays its
-- maintenance, on the hottest write path the sync has.
drop index messages_label_ids_idx;

-- ------------------------------------------------------- wire fields (P4) --

-- `API.md` §3: a note carries an unread marker the Notes tab renders.
alter table notes add column unread boolean not null default true;

-- `API.md` §6: the one-line summary a run reports, rendered on agent cards.
-- Nullable: a run that died before its first step has nothing to say.
alter table agent_runs add column summary text;
