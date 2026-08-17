//! Test doubles: a scripted model, an in-memory journal that can be made to
//! fail on cue, and a handful of tools that misbehave in specific ways.
//!
//! Available to this crate's own tests unconditionally, and to downstream
//! crates behind the `testing` feature:
//!
//! ```toml
//! [dev-dependencies]
//! nade-agent-sdk = { version = "0.1", features = ["testing"] }
//! ```
//!
//! None of it is wired into the default build, so a production dependency never
//! carries it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::ids::{RunId, Seq};
use crate::journal::{Entry, Journal};
use crate::llm::Llm;
use crate::message::{ChatRequest, ChatResponse, ToolCall};
use crate::tool::{control, Tool};

/// A model that reads from a script.
///
/// Hands out the responses it was built with, in order. Asking for a turn that
/// was not scripted **panics** rather than returning an error: a run that took
/// more turns than the test author expected is a bug in the engine, and a quiet
/// `Err` would let it hide as a "the model was unreachable" path.
#[derive(Debug)]
pub struct ScriptedLlm {
    responses: Mutex<std::collections::VecDeque<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedLlm {
    /// Build a model that will answer with `responses`, in order.
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Every request the engine has made so far.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    /// How many turns the engine has asked for.
    pub fn turns(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }

    /// How many scripted responses are still unused.
    pub fn remaining(&self) -> usize {
        self.responses.lock().expect("responses lock").len()
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        self.requests.lock().expect("requests lock").push(req);
        let next = self.responses.lock().expect("responses lock").pop_front();
        match next {
            Some(response) => Ok(response),
            None => panic!(
                "ScriptedLlm ran out of responses: the engine asked for turn {} but the script \
                 has none left. An unscripted turn means the engine looped further than the test \
                 expected.",
                self.turns()
            ),
        }
    }
}

/// Where an injected append failure strikes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailMode {
    /// The entry never lands. Models a process that died before the write
    /// reached durable storage.
    BeforeCommit,
    /// The entry lands, then the call fails anyway. Models a process that died
    /// after the commit but before it learned the commit succeeded — the nastier
    /// of the two, because the journal moves on without the engine.
    AfterCommit,
}

#[derive(Clone, Copy, Debug)]
struct FailPoint {
    seq: Seq,
    mode: FailMode,
}

/// An in-memory [`Journal`] that enforces the `(run_id, seq)` primary key and
/// can be made to fail an append at a chosen sequence.
#[derive(Clone, Debug, Default)]
pub struct MemoryJournal {
    entries: Arc<Mutex<HashMap<RunId, Vec<Entry>>>>,
    fail_point: Arc<Mutex<Option<FailPoint>>>,
    appends: Arc<AtomicU32>,
}

impl MemoryJournal {
    /// An empty journal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every entry recorded for `run`.
    pub fn entries(&self, run: RunId) -> Vec<Entry> {
        self.entries
            .lock()
            .expect("journal lock")
            .get(&run)
            .cloned()
            .unwrap_or_default()
    }

    /// `(seq, kind)` for every entry of `run` — the shape assertions read from.
    pub fn layout(&self, run: RunId) -> Vec<(Seq, String)> {
        self.entries(run)
            .into_iter()
            .map(|e| (e.seq, e.kind.to_string()))
            .collect()
    }

    /// How many appends have been attempted across all runs, failures included.
    pub fn append_attempts(&self) -> u32 {
        self.appends.load(Ordering::SeqCst)
    }

    /// Fail the next append at `seq` **before** the entry is stored, once.
    pub fn fail_append_at(&self, seq: Seq) {
        *self.fail_point.lock().expect("fail lock") = Some(FailPoint {
            seq,
            mode: FailMode::BeforeCommit,
        });
    }

    /// Store the entry at `seq` and then fail the call anyway, once.
    pub fn fail_append_after_commit_at(&self, seq: Seq) {
        *self.fail_point.lock().expect("fail lock") = Some(FailPoint {
            seq,
            mode: FailMode::AfterCommit,
        });
    }

    /// Forget any armed failure.
    pub fn clear_failures(&self) {
        *self.fail_point.lock().expect("fail lock") = None;
    }

    /// Store an entry without going through [`Journal::append`], bypassing the
    /// armed failure. Used to build a journal by hand.
    pub fn force_append(&self, run: RunId, entry: Entry) {
        self.entries
            .lock()
            .expect("journal lock")
            .entry(run)
            .or_default()
            .push(entry);
    }
}

#[async_trait]
impl Journal for MemoryJournal {
    async fn append(&self, run: RunId, entry: Entry) -> Result<Seq> {
        self.appends.fetch_add(1, Ordering::SeqCst);

        let armed = {
            let mut guard = self.fail_point.lock().expect("fail lock");
            match *guard {
                Some(point) if point.seq == entry.seq => {
                    *guard = None;
                    Some(point.mode)
                }
                _ => None,
            }
        };

        if armed == Some(FailMode::BeforeCommit) {
            return Err(Error::journal(format!(
                "injected failure before committing seq {}",
                entry.seq
            )));
        }

        {
            let mut store = self.entries.lock().expect("journal lock");
            let run_entries = store.entry(run).or_default();
            if run_entries.iter().any(|e| e.seq == entry.seq) {
                return Err(Error::SeqConflict {
                    run,
                    seq: entry.seq,
                });
            }
            run_entries.push(entry.clone());
            run_entries.sort_by_key(|e| e.seq);
        }

        if armed == Some(FailMode::AfterCommit) {
            return Err(Error::journal(format!(
                "injected failure after committing seq {}",
                entry.seq
            )));
        }

        Ok(entry.seq)
    }

    async fn load(&self, run: RunId) -> Result<Vec<Entry>> {
        Ok(self.entries(run))
    }
}

/// A tool that hands its arguments straight back.
#[derive(Debug)]
pub struct EchoTool {
    name: String,
}

impl EchoTool {
    /// An echo tool called `echo`.
    pub fn new() -> Self {
        Self {
            name: "echo".to_string(),
        }
    }

    /// An echo tool under a chosen name.
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}})
    }

    fn description(&self) -> Option<&str> {
        Some("Returns its arguments unchanged.")
    }

    async fn execute(&self, call: &ToolCall) -> Result<Value> {
        Ok(json!({"echo": call.arguments}))
    }
}

/// A tool that records what it did, so a test can prove how often it really ran
/// and how many distinct effects that produced.
///
/// [`CountingTool::executions`] counts calls to `execute`. [`CountingTool::effects`]
/// counts **rows**: every execution upserts into a map keyed by
/// [`ToolCall::effect_id`], exactly as a consumer is required to. A re-executed
/// step therefore shows up as two executions and one effect, which is the whole
/// point of the deterministic id.
#[derive(Debug)]
pub struct CountingTool {
    name: String,
    requires_approval: bool,
    executions: Arc<AtomicU32>,
    effects: Arc<Mutex<HashMap<Uuid, u32>>>,
    calls: Arc<Mutex<Vec<ToolCall>>>,
}

impl CountingTool {
    /// A counting tool that runs without asking.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            requires_approval: false,
            executions: Arc::new(AtomicU32::new(0)),
            effects: Arc::new(Mutex::new(HashMap::new())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A counting tool that is gated behind human approval — a stand-in for any
    /// mutating capability.
    pub fn mutating(name: impl Into<String>) -> Self {
        Self {
            requires_approval: true,
            ..Self::new(name)
        }
    }

    /// How many times `execute` was actually called.
    pub fn executions(&self) -> u32 {
        self.executions.load(Ordering::SeqCst)
    }

    /// The upsert store: effect id to the number of writes that landed on it.
    ///
    /// Its length is the number of distinct side effects the run produced.
    pub fn effects(&self) -> HashMap<Uuid, u32> {
        self.effects.lock().expect("effects lock").clone()
    }

    /// Every call this tool received, in order.
    pub fn calls(&self) -> Vec<ToolCall> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"note": {"type": "string"}}})
    }

    fn requires_approval(&self, _call: &ToolCall) -> bool {
        self.requires_approval
    }

    async fn execute(&self, call: &ToolCall) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().expect("calls lock").push(call.clone());

        // Exactly what a real consumer must do: key the effect on the id the
        // engine handed over, and upsert.
        let id = call
            .effect_id()
            .ok_or_else(|| Error::tool("no effect id on the call"))?;
        let writes = {
            let mut effects = self.effects.lock().expect("effects lock");
            let counter = effects.entry(id).or_insert(0);
            *counter += 1;
            *counter
        };

        Ok(json!({"effect_id": id, "writes": writes, "args": call.arguments}))
    }
}

/// A tool that panics instead of returning.
#[derive(Debug)]
pub struct PanicTool;

#[async_trait]
impl Tool for PanicTool {
    fn name(&self) -> &str {
        "boom"
    }

    fn schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _call: &ToolCall) -> Result<Value> {
        panic!("boom: this tool always panics");
    }
}

/// A tool that returns a [`crate::Error::Tool`].
#[derive(Debug)]
pub struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "flaky"
    }

    fn schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _call: &ToolCall) -> Result<Value> {
        Err(Error::tool("upstream said no"))
    }
}

/// A tool that returns a result far larger than any sane cap.
#[derive(Debug)]
pub struct HugeTool {
    bytes: usize,
}

impl HugeTool {
    /// A tool whose result carries `bytes` of payload.
    pub fn new(bytes: usize) -> Self {
        Self { bytes }
    }

    /// A tool that returns roughly 10 MB.
    pub fn ten_megabytes() -> Self {
        Self::new(10 * 1024 * 1024)
    }
}

#[async_trait]
impl Tool for HugeTool {
    fn name(&self) -> &str {
        "firehose"
    }

    fn schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _call: &ToolCall) -> Result<Value> {
        // Multi-byte characters on purpose: the truncation must land on a
        // character boundary, not halfway through one.
        let unit = "αβγδε漢字🙂";
        let repeats = self.bytes / unit.len() + 1;
        Ok(json!({"blob": unit.repeat(repeats)}))
    }
}

/// A tool that parks the run on a timer.
#[derive(Debug)]
pub struct WaitTool {
    wake_at: DateTime<Utc>,
    executions: Arc<AtomicU32>,
}

impl WaitTool {
    /// A tool that parks the run until `wake_at`.
    pub fn new(wake_at: DateTime<Utc>) -> Self {
        Self {
            wake_at,
            executions: Arc::new(AtomicU32::new(0)),
        }
    }

    /// How many times it ran.
    pub fn executions(&self) -> u32 {
        self.executions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &str {
        "sleep"
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"until": {"type": "string"}}})
    }

    async fn execute(&self, _call: &ToolCall) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(control::wait_until(self.wake_at, json!({"parked": true})))
    }
}
