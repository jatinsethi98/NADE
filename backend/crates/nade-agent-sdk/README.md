# nade-agent-sdk

A generic agent engine for Rust: a tool-calling loop that survives being killed.

Three traits describe the world — `Llm` talks to a model, `Tool` does something,
`Journal` remembers — and `Engine` drives them. The crate knows nothing about
your domain, your provider, or your database.

```toml
[dependencies]
nade-agent-sdk = "0.1"
```

## What it guarantees

> **At-least-once execution with stable idempotency keys**, conditional on a
> durable journal and idempotent effects.

Not exactly-once. Nothing that survives a process death can be: at the instant
the process dies, whether the effect landed is a fact about the outside world
that the engine has no record of. What the engine does guarantee is that it
writes down its intention *before* it acts, that a step interrupted between
those two moments is retried, and that every retry is handed the same
identifier and the same input — so an effect written as an upsert on that
identifier collapses the retries into one.

Exactly-once *effects* are therefore available, and they are yours to build:
upsert on `effect_id`, and give the engine a journal that is really durable.
Both conditions are load-bearing, and neither is checkable from inside this
crate.

## Why the journal is not a log

An agent that only computes can be retried freely. An agent that *acts* — writes
a row, files a draft, sends a request — cannot. Kill the process halfway through
a run and the world has changed while the engine has no memory of it; retry, and
it happens twice.

So the journal is written **before** the effect, not after it:

1. append `step_started { step_seq, tool, args, args_hash, effect_id }` and let
   it commit;
2. execute the tool;
3. append `step_done { step_seq, result }`.

On restart the engine reads the journal back and sorts every step into one of
three piles:

| journal shows | replay does |
| --- | --- |
| `step_started` and `step_done` | skips it — the result is already known |
| `step_started`, no `step_done` | **runs it again** |
| neither | runs it for the first time |

The middle row is the whole design. After a crash nothing can tell whether the
effect landed before the process died, so the engine assumes the worst and runs
the step again. The same file is therefore the recovery log *and* the run log a
user reads in the UI — one append-only table, two jobs.

## What that costs you

Re-execution is only safe because every attempt at a step gets the same id:

```text
effect_id = uuid5(EFFECT_NAMESPACE, "<run-id>:<step-seq>")
```

available to a tool as `call.effect_id()` and to a host as the free function
`nade_agent_sdk::effect_id(run_id, seq)`.

**Write your side effects keyed on that id, with an upsert.** The identity of a
row an agent creates must come from the engine, never from a fresh `uuid4()` or
an auto-increment column. Get it right and two attempts collapse into one row.
Get it wrong and you have two.

Every attempt at one step also sees the same *input*: same arguments, same
`effect_id`, same `opened_at`. The only field that moves between attempts is
`call.is_replay()`, and it is advisory — a tool whose effect depends on it is
not idempotent, whatever key it upserts on.

## Approval is not idempotency

`Tool::requires_approval` controls **authorisation**: it answers "is this
allowed to happen?", once, before anything happens. It does not answer "has this
already happened?", and it cannot.

An approved step is fenced and replayed exactly like an unapproved one. The
human says yes; `approval_resolved` commits; `step_started` commits; the tool
sends the email; the process dies before `step_done` commits. On the next run
the journal shows a step that started and never finished, and the engine does
the only safe thing it knows — it runs it again. **The email goes twice.** The
gate was passed before the ambiguity existed.

So an effect with no natural key needs an **outbox**, not an approval. In one
transaction write the effect row keyed on `effect_id` *and* a row saying "this
needs sending"; deliver it from a sweeper that marks it sent. Re-execution
rewrites the same two rows and the sweeper still sends once. Approval decides
*whether*; the outbox decides *how many times*.

If an effect truly cannot be keyed, `Tool::replay_policy` → `ReplayPolicy::Halt`
is the guard rail: the engine refuses to blind-retry the step and stops the run
with `FailureReason::AmbiguousEffect`, quoting the `effect_id` so a human can
reconcile. Damage control, not a fix — it turns a silent double-send into a loud
stop.

## Approval

When `requires_approval` returns `true`, the engine appends
`approval_requested`, returns `RunOutcome::PendingApproval` with everything the
host needs to build its own approval record, and **executes nothing**. The only
route from there to execution is
`Engine::resume(run_id, Resolution::approve(step_seq))`. Calling `run` again does
not open that door, however many times it is called.

Every resolution names the step it settles. Without that binding a delayed
duplicate of an old decision is indistinguishable from a fresh decision about
whatever the run is parked on *now* — and the engine would settle the wrong
step, executing something no human ever saw. A resolution that names the wrong
step is `Error::StepMismatch`; one that names a step already decided is
`Error::AlreadyResolved`, which appends nothing and executes nothing. Treat that
as success: an earlier delivery already won. If the run still needs driving,
call `run`, which resumes from any recorded decision.

```text
queued → running → done | failed
running → pending_approval --approve--> queued → running
                           --skip----->  skipped
                           --expire--->  expired
running → waiting(wake_at) --timer---->  running
```

## Caps

`EngineConfig` bounds three things; a breach ends the run as
`RunOutcome::Failed` and is journaled:

- `max_steps` (default 12) — tool steps per run, which is also what stops a
  model looping on one tool forever;
- `token_budget` (default 50 000) — total spend across turns;
- `max_tool_result_bytes` (default 16 KiB) — past which a result is replaced by
  an explicit truncation envelope, never quietly shortened.

Transport problems are not cap breaches. An unreachable model or a journal that
will not commit comes back as `Err`, leaves the run exactly where its journal
says it is, and is the host's to retry.

## Usage

```rust
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nade_agent_sdk::{
    ChatRequest, ChatResponse, Engine, EngineConfig, Entry, Error, Journal, Llm, Result,
    RunId, RunStatus, Seq, Tool, ToolCall,
};
use serde_json::{json, Value};

struct Model;
#[async_trait]
impl Llm for Model {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        Ok(if req.messages.iter().any(|m| m.role() == "tool") {
            ChatResponse::text("noted").with_usage(30, 4)
        } else {
            ChatResponse::tool_call("c1", "write_note", json!({"body": "hello"}))
        })
    }
}

struct WriteNote;
#[async_trait]
impl Tool for WriteNote {
    fn name(&self) -> &str { "write_note" }
    fn schema(&self) -> Value { json!({"type": "object"}) }
    async fn execute(&self, call: &ToolCall) -> Result<Value> {
        // `insert … on conflict (id) do update`, keyed on exactly this id.
        Ok(json!({ "note_id": call.effect_id() }))
    }
}

// NOT durable: a `Vec` behind a mutex makes none of the guarantees above.
#[derive(Default)]
struct Log(Mutex<Vec<Entry>>);
#[async_trait]
impl Journal for Log {
    async fn append(&self, run: RunId, entry: Entry) -> Result<Seq> {
        let mut log = self.0.lock().expect("lock");
        if log.iter().any(|e| e.seq == entry.seq) {
            return Err(Error::SeqConflict { run, seq: entry.seq });
        }
        log.push(entry.clone());
        Ok(entry.seq)
    }
    async fn load(&self, _run: RunId) -> Result<Vec<Entry>> {
        Ok(self.0.lock().expect("lock").clone())
    }
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() -> Result<()> {
let tools = vec![Arc::new(WriteNote) as Arc<dyn Tool>];
let engine = Engine::new(Model, tools, Log::default(), EngineConfig::default())?;

let outcome = engine.run(RunId::new(), "write me a note").await?;
assert_eq!(outcome.status(), RunStatus::Done);
# Ok(())
# }
```

## Durability is yours

**This crate contains no durable journal.** No fsync, no WAL, no transaction —
there is nothing here that could be. The only implementation that ships is
`testing::MemoryJournal`, a `HashMap` behind a mutex, exactly as durable as the
process.

`Journal::append` returning `Ok` is a promise that the entry would survive the
process being killed at the next instruction. An implementation that returns
before the write is durable does not make the guarantee weaker — it removes it,
silently, from every run: the fence stops fencing, and a step whose effect
landed comes back as a step that was never opened. The bug never shows up in
testing, because it only exists in the crash.

Concretely: a committed transaction with `synchronous_commit = on`, an `fsync`ed
file whose containing directory is also `fsync`ed, or a quorum-acknowledged
write. And `load` must return every committed entry — a stale replica read is a
correctness bug of the same class as a lost write.

## What a consumer must guarantee

The guarantee at the top of this file is conditional on all four. Miss one and
what is left is a tool-calling loop with a nice log.

1. **Durability** — `Journal::append` returns `Ok` only once the entry would
   survive a crash. This is the load-bearing one.
2. **Uniqueness** — a second append at an existing sequence fails with
   `Error::SeqConflict` instead of overwriting. Model it on
   `primary key (run_id, seq)`.
3. **Idempotent effects**, keyed on `call.effect_id()`, upserted — including
   effects behind an approval gate, which the gate does nothing to protect.
4. **One driver per run** — two workers on one run id will collide on the
   journal's primary key, but that should be the backstop, not the plan. Hold a
   lease.

## Testing

The `testing` feature exposes the doubles this crate tests itself with: a
`ScriptedLlm`, a `MemoryJournal` that can be told to fail an append at a chosen
sequence — before or after it commits — and tools that panic, overflow, or count
their own executions.

```toml
[dev-dependencies]
nade-agent-sdk = { version = "0.1", features = ["testing"] }
```

## License

MIT OR Apache-2.0.
