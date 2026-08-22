//! Host implementations of the SDK's storage-facing traits.
//!
//! The SDK is generic over [`Journal`](nade_agent_sdk::Journal) and owns no
//! infrastructure — no HTTP, no database, no runtime, per its own manifesto.
//! The Postgres driver for it therefore lives here, in the host, rather than
//! behind a `--features postgres` flag on the SDK, which is the exact per-crate
//! feature trap `backend/DECISIONS.md` D27 documents.

pub mod journal;

pub use journal::PgJournal;
