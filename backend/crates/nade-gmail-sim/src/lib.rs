//! A stateful, deterministic, in-process simulator of the Gmail REST API v1.
//!
//! # Why this exists
//!
//! The failures that matter in a mail sync are not malformed requests — they are
//! **stateful**. An incremental history page applied twice. A message deleted
//! between `messages.list` and `messages.get`. A `historyId` that expired and
//! forced a full re-sync. A label removed while a batch was in flight. A thread
//! whose message count changed under an open cursor.
//!
//! A static stub cannot express any of that, because each stub is a fixed
//! response with no memory: the second `history.list` returns the same canned
//! bytes without knowing it is the second, so "applying this page twice changed
//! nothing" becomes an assertion about the stub rather than about the client.
//!
//! This crate holds a [`Mailbox`](mailbox::Mailbox) — messages, labels, threads,
//! a history log, a monotonic `historyId` — and answers every endpoint
//! *consistently from that state*.
//!
//! ```
//! use nade_gmail_sim::{MessageSpec, Simulator};
//!
//! let sim = Simulator::new();
//! let id = sim.insert_message(&MessageSpec::new().subject("Invoice").text("due")).unwrap();
//! let before = sim.history_id();
//!
//! // Every mutation writes exactly one history record and moves `historyId`.
//! sim.mark_read(&id).unwrap();
//! assert!(sim.history_id() > before);
//!
//! // …and a mutation that changes nothing writes nothing.
//! let after = sim.history_id();
//! assert_eq!(sim.mark_read(&id).unwrap(), None);
//! assert_eq!(sim.history_id(), after);
//!
//! // The incremental sync sees the one real change.
//! let page = sim
//!     .handle(&sim.authorized(&format!("/gmail/v1/users/me/history?startHistoryId={before}")))
//!     .json_body();
//! assert_eq!(page["history"].as_array().unwrap().len(), 1);
//! ```
//!
//! # Two transports, one implementation
//!
//! [`Simulator::handle`] is the only place a response is produced. The
//! in-process path calls it directly; [`http::SimServer`] translates an HTTP
//! request into the same call. That is what lets a real `reqwest`-based client be
//! tested **unmodified** — a client tested only against an in-process fake has
//! never had its URL building, query encoding, headers or multipart bodies
//! checked by anything but itself.
//!
//! # Determinism is a hard requirement
//!
//! No wall clock, no RNG, anywhere in the response path. Time is a
//! [`TestClock`] the test advances explicitly; ids, page tokens, MIME
//! boundaries and batch boundaries all come from counters. `chrono` is taken
//! without its `clock` feature, so `Utc::now` is not even compiled in, and
//! `tests/hygiene.rs` scans the sources for the rest. `tests/determinism.rs`
//! pins the promise: the same script against two fresh simulators produces
//! byte-identical responses.
//!
//! # Fidelity beats convenience
//!
//! Where Gmail is awkward, so is this. The awkwardness is the product:
//!
//! * system labels have `name == id`, so `INBOX` is named `INBOX`;
//! * `labels.list` returns **no** counters — those are `labels.get`'s job — and
//!   returns the visibility fields for user labels only;
//! * `historyId` and `internalDate` are **strings**; `sizeEstimate`,
//!   `resultSizeEstimate` and `body.size` are **numbers**;
//! * `historyId` does not increment by one, so a client cannot assume contiguity;
//! * `history.list` returns the mailbox's *current* `historyId` on every page, so
//!   a client that stores it after page one silently skips page two;
//! * `resultSizeEstimate` is an estimate, not a count;
//! * an empty listing omits the `messages` key entirely;
//! * a **trashed** message is still returned by `messages.get` with `TRASH` in
//!   its labels — only a permanently deleted one is a `404`;
//! * `format=raw` carries no `payload` and therefore no `attachmentId` anywhere;
//! * batch sub-responses come back **out of order** by default, because Google
//!   does not guarantee the order and a client must correlate by `Content-ID`.
//!
//! # Where to start
//!
//! | Want to | Look at |
//! |---|---|
//! | drive the world | [`Simulator`] and [`mailbox::Mailbox`] |
//! | write a whole sync story as one value | [`Scenario`] |
//! | make something fail on purpose | [`fault::FaultRule`] |
//! | fill a mailbox from real mail | [`seed`] |
//! | test a real HTTP client | [`http::SimServer`] |
//!
//! Acceptance criteria and the edge-case checklist are in `CRITERIA.md` beside
//! this crate.
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
// `similar_names` fires on pairs like `ct_params`/`cd_params`, where the
// similarity is the point — they are the Content-Type and Content-Disposition
// parameter lists and renaming either makes the code read worse.
#![allow(clippy::module_name_repetitions, clippy::similar_names)]

pub mod api;
pub mod batch;
pub mod clock;
pub mod error;
pub mod fault;
pub mod history;
// The module carries its own `//!` docs; an outer `///` here would move the
// intra-doc-link resolution scope to this file and break every link in them.
#[cfg(feature = "http")]
pub mod http;
pub mod ids;
pub mod label;
pub mod mailbox;
pub mod message;
pub mod mime;
pub mod query;
pub mod render;
pub mod scenario;
pub mod seed;
pub mod simulator;

pub use clock::{Clock, TestClock};
pub use error::{ApiError, SimError};
pub use message::{MessageSpec, StoredMessage};
pub use scenario::{Outcome, Scenario, Step, Target};
pub use simulator::Simulator;
