//! Host implementations of the SDK's storage-facing traits.
//!
//! The SDK is generic over [`Journal`](durable_agent::Journal), and its default
//! build still owns no infrastructure — no HTTP, no database, no runtime.
//!
//! # Why this driver stays here now that the SDK ships one
//!
//! `durable-agent` 0.2 added `postgres::PgJournal` behind a `postgres` feature,
//! so this module is no longer the only option. It is still the one in use, for
//! two reasons.
//!
//! The original reason has expired and should not be cited again: while the SDK
//! was a member of this workspace, putting its driver behind a feature was the
//! per-crate feature trap `backend/DECISIONS.md` D27 documents — Cargo unifies
//! features across a workspace, so one crate's choice silently changed
//! another's build. The SDK is an external dependency now and nothing else here
//! depends on it, so that trap cannot reach us.
//!
//! What remains is smaller and real: this table is not the SDK's table. It
//! carries `references agent_runs (id) on delete cascade`, which is what makes a
//! deleted run take its journal with it, and the SDK's `SCHEMA` has no such
//! column — it cannot, being generic. `PgJournal::migrate` is therefore never
//! the right call here; `0001_init.sql` owns the DDL. Swapping to the SDK's
//! driver is a one-line change to the re-export below (the two implementations
//! agree, statement for statement) and the tests in `journal/tests.rs` would
//! cover it unchanged — but it buys deduplication only, and costs a second
//! place to look when the schema and the driver disagree.

pub mod journal;

pub use journal::PgJournal;
