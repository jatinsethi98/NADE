# nade-agent-sdk

A generic agent engine for Rust: a tool-calling loop that survives being killed.

Three traits describe the world — `Llm` talks to a model, `Tool` does something,
`Journal` remembers — and `Engine` drives them. The crate knows nothing about
your domain, your provider, or your database.

```toml
[dependencies]
nade-agent-sdk = "0.1"
```

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

Effects with no natural key — an email actually leaving a server — belong behind
`Tool::requires_approval`, where a human is the fence.

## Approval

When `requires_approval` returns `true`, the engine appends
`approval_requested`, returns `RunOutcome::PendingApproval` with everything the
host needs to build its own approval record, and **executes nothing**. The only
route from there to execution is `Engine::resume(run_id, Resolution::Approve)`.
Calling `run` again does not open that door, however many times it is called.

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

## What a consumer must guarantee

1. **Durability** — `Journal::append` returns `Ok` only once the entry would
   survive a crash.
2. **Uniqueness** — a second append at an existing sequence fails with
   `Error::SeqConflict` instead of overwriting. Model it on
   `primary key (run_id, seq)`.
3. **Idempotent effects**, keyed on `call.effect_id()`, upserted.
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
