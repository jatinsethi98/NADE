-- P4 "First runs": what the agent runtime needs that the schema does not have.
--
-- A fifth migration rather than an amendment to 0001: sqlx checksums each
-- migration and refuses a modified one, so editing an applied file makes every
-- already-migrated dev database fail to boot (backend/DECISIONS.md D23).

-- ----------------------------------------------------------------- agents --

-- Four fields `API.md` §5 puts on every full agent object and no column holds.
-- None of them is derivable at read time, which is why they are stored and not
-- computed:
--
--   * `compile_error` records why a sentence did not compile. `API.md` §5 is
--     explicit that a failed compile still creates the agent - "so the user's
--     sentence is never lost" - so the failure has to live somewhere.
--   * `when_span` / `do_span` are the two substrings the builder screen
--     underlines. They are chosen by the compiler from the user's own wording;
--     §5.1's `spec` has no field they could be recovered from.
--   * `trailing_clause` is the leftover sentence ("Ask me before you save.") -
--     again a fragment of the user's wording, not of the spec.
--
-- `docs/contract/validate.py` enforces the invariant these four participate in:
-- the three spans are null exactly when `spec` is null, and `spec` is null XOR
-- `compile_error` is set.
--
-- The wire calls the last one `trailing`; the column does not, because
-- `TRAILING` is a **reserved word** in PostgreSQL (it is part of `trim(trailing
-- ... )`) and a bare `trailing` in a column list is a syntax error. Quoting it
-- everywhere would work and would be forgotten exactly once; the serialiser
-- renames it back for the wire, where the contract's name is what matters.
alter table agents
    add column compile_error   text,
    add column when_span       text,
    add column do_span         text,
    add column trailing_clause text;

-- -------------------------------------------------------------- llm_calls --

-- The spend ledger. Every model call lands here, successful or not, and the
-- $1/day ceiling and the <=20-triaged-messages-per-agent-per-day cap are both
-- reads against it (PLAN.md §Dev caps: "they read the per-account `llm_calls`
-- ledger, not a process-wide counter").
--
-- Written by the *adapter*, not by the engine. Anthropic reports cache reads
-- and writes outside `input_tokens`, and the SDK's `TokenUsage` deliberately
-- carries only two counters, so the adapter is the one place where the true
-- breakdown is visible - and therefore the only place that can price a call.
create table llm_calls (
    id          bigint      generated always as identity primary key,
    account_id  uuid        not null references accounts (id) on delete cascade,
    purpose     text        not null
                check (purpose in ('run', 'compile', 'triage', 'ask')),

    -- `set null`, never `cascade`, on both back-references: deleting an agent
    -- must not erase what it spent. The ceiling is an account-level fact and
    -- has to survive the agent that ran up the bill.
    agent_id    uuid        references agents (id)     on delete set null,
    run_id      uuid        references agent_runs (id) on delete set null,

    model       text        not null,

    -- Four counters because Anthropic bills four things at three prices. At P4
    -- prompt caching is off and the two cache columns are always 0; they exist
    -- so that turning it on later is a code change and not a schema change
    -- discovered after a month of under-counted spend.
    tokens_in          integer not null default 0 check (tokens_in          >= 0),
    tokens_out         integer not null default 0 check (tokens_out         >= 0),
    cache_read_tokens  integer not null default 0 check (cache_read_tokens  >= 0),
    cache_write_tokens integer not null default 0 check (cache_write_tokens >= 0),

    -- Scale 9 = nano-USD, matching `llm::cost`, which does all of its
    -- arithmetic in integer nano-USD so the ceiling is exact at its boundary.
    -- `numeric`, never `double precision`: the whole point is that a ledger
    -- standing at exactly 1.000000000 blocks and one at 0.999999999 does not.
    cost_usd    numeric(14, 9) not null default 0 check (cost_usd >= 0),

    -- False for a call that failed after its retries. Failed calls are still
    -- charged when the provider billed them, and are recorded either way so a
    -- burst of failures is visible rather than invisible.
    ok          boolean     not null,
    error       text,

    created_at  timestamptz not null default now()
);

-- The ceiling's query is "this account, today", newest irrelevant but the
-- range scan is: `where account_id = $1 and created_at >= date_trunc('day', now())`.
create index llm_calls_account_day_idx on llm_calls (account_id, created_at desc);

-- The triage cap is per agent per day, so it needs its own access path. Partial,
-- because triage is one purpose out of four and the other three never use it.
create index llm_calls_agent_day_idx on llm_calls (agent_id, created_at desc)
    where purpose = 'triage';

-- ------------------------------------------------------------- agent_runs --

-- `GET /runs` is newest-first **per account** and optionally filtered by agent.
-- `agent_runs_agent_idx` (0001) serves only the `?agent_id=` form; without this
-- one the unfiltered Run log is a sequential scan plus a sort.
create index agent_runs_account_created_idx on agent_runs (account_id, created_at desc);
