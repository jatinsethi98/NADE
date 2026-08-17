//! Identifiers and the deterministic effect-id contract.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Position of an entry in a run's journal.
///
/// Sequence numbers start at `1` and increase by one per committed entry, with
/// no gaps. They are small by construction (bounded by
/// [`EngineConfig::max_steps`](crate::EngineConfig::max_steps) times a small
/// constant), so a `u32` is ample and a Postgres `int` column round-trips
/// losslessly.
pub type Seq = u32;

/// The identifier of a single agent run.
///
/// A newtype over [`Uuid`] so it cannot be confused with the other UUIDs in a
/// system (accounts, effects, approval tokens). Serialises transparently as the
/// hyphenated UUID string.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(Uuid);

impl RunId {
    /// Mint a fresh random run id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wrap an existing UUID — for example one already stored by the host.
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// The underlying UUID.
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RunId({})", self.0.hyphenated())
    }
}

impl From<Uuid> for RunId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl From<RunId> for Uuid {
    fn from(id: RunId) -> Self {
        id.0
    }
}

impl FromStr for RunId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::from_str(s)?))
    }
}

/// The UUID v5 namespace all effect ids are minted under.
///
/// **This value is frozen.** Changing it would re-point every deterministic
/// effect id and break replay against journals written by older versions.
pub const EFFECT_NAMESPACE: Uuid =
    Uuid::from_u128(0x6e_61_64_65_5f_65_66_66_65_63_74_5f_6e_73_76_31);

/// The deterministic identifier of the side effect a step is allowed to have.
///
/// `uuid5(EFFECT_NAMESPACE, "<run-id>:<seq>")`, where `seq` is the journal
/// sequence of the entry that *opened* the step — the `step_started` entry for
/// an ordinary call, or the `approval_requested` entry for a gated one.
///
/// # The contract this creates
///
/// A step can be executed more than once: the engine re-executes any step whose
/// `step_started` committed but whose `step_done` did not, because it cannot
/// know whether the effect landed. That is only safe if the effect is
/// **idempotent under this id**. Consumers must therefore write their effect
/// rows with this UUID as the primary key and an upsert
/// (`insert … on conflict (id) do update`), never a plain insert and never a
/// freshly generated id.
///
/// Tools receive the id for the step they are running as
/// [`ToolCall::effect_id`](crate::ToolCall::effect_id); this free function
/// exists so hosts can compute the same value from outside a run — for example
/// to pre-create the row an approval will later fill in.
///
/// ```
/// use nade_agent_sdk::{effect_id, RunId};
/// use uuid::Uuid;
///
/// let run = RunId::from_uuid(Uuid::nil());
/// assert_eq!(effect_id(run, 7), effect_id(run, 7)); // stable
/// assert_ne!(effect_id(run, 7), effect_id(run, 8)); // per-step
/// ```
pub fn effect_id(run: RunId, seq: Seq) -> Uuid {
    // Hyphenated UUIDs are fixed width, so "<uuid>:<seq>" cannot be ambiguous.
    let name = format!("{}:{}", run.as_uuid().hyphenated(), seq);
    Uuid::new_v5(&EFFECT_NAMESPACE, name.as_bytes())
}

/// A stable content hash of a tool call's arguments, as `sha256:<hex>`.
///
/// Stable across processes and platforms: [`serde_json::Value`] keeps object
/// keys in sorted order (this crate deliberately does **not** enable the
/// `preserve_order` feature), so the serialised bytes are canonical.
pub fn args_hash(args: &serde_json::Value) -> String {
    let canonical = serde_json::to_vec(args).unwrap_or_else(|_| b"null".to_vec());
    let digest = Sha256::digest(&canonical);
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest {
        // Two lowercase hex nibbles; avoids pulling in a hex crate.
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}
