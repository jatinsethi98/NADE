//! Filling a mailbox: from real mail, from the conformance corpus, or by hand.
//!
//! Three sources, one code path:
//!
//! * **the live sample** at `backend/testdata/live/raw/*.eml` — 60 real
//!   messages, gitignored, and *absent on a fresh clone*. Seeding degrades to
//!   `Ok(0)` rather than failing, and the tests that need it skip loudly;
//! * **the MIME conformance corpus** at `backend/testdata/mime/*.eml` — 26 cases
//!   that are committed, so a test may depend on them;
//! * **[`MessageSpec`]**, for hand-written scenarios.
//!
//! Two things make seeding deterministic, and both matter more than they look:
//! directory entries are **sorted** before they are read (`read_dir` order is
//! filesystem order, which differs between machines and between runs), and a
//! message with no usable `Date` header gets a slot derived from its position in
//! that sorted list rather than from the clock.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    clock::DAY_MS,
    error::{Result, SimError},
    mailbox::{date_header_ms, Mailbox},
    message::{MessageSpec, ReceivedAt},
};

/// One message that was seeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seeded {
    /// The file it came from, without its directory.
    pub file: String,
    /// The id it was given in the mailbox.
    pub id: String,
    /// Its `internalDate`.
    pub internal_date_ms: i64,
}

/// How to seed from a directory of `.eml` files.
#[derive(Debug, Clone)]
pub struct SeedOptions {
    /// Labels every seeded message gets.
    pub labels: Vec<String>,
    /// `internalDate` for the first message that has no usable `Date` header.
    pub fallback_base_ms: i64,
    /// How far apart such messages are spaced.
    pub fallback_step_ms: i64,
    /// Stop after this many files.
    pub limit: Option<usize>,
    /// Ignore each file's own `Date` header and lay every message out on the
    /// fallback ladder instead. Useful when a test wants a known, evenly spaced
    /// listing order rather than whatever the real mail happened to say.
    pub ignore_date_headers: bool,
}

impl Default for SeedOptions {
    fn default() -> Self {
        Self {
            labels: vec!["INBOX".to_owned(), "UNREAD".to_owned()],
            // Four weeks before the default clock, so a `newer_than:30d` sync
            // sees the whole corpus.
            fallback_base_ms: crate::clock::DEFAULT_NOW_MS - 28 * DAY_MS,
            fallback_step_ms: 60 * 60 * 1000,
            limit: None,
            ignore_date_headers: false,
        }
    }
}

impl SeedOptions {
    /// Replace the labels every seeded message gets.
    #[must_use]
    pub fn labels<I, S>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    /// Seed at most this many files.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Lay every message out on the fallback ladder.
    #[must_use]
    pub fn evenly_spaced(mut self, base_ms: i64, step_ms: i64) -> Self {
        self.ignore_date_headers = true;
        self.fallback_base_ms = base_ms;
        self.fallback_step_ms = step_ms;
        self
    }
}

/// `backend/testdata` — resolved from this crate's manifest directory, so it
/// works no matter where the test binary is run from.
#[must_use]
pub fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
}

/// `backend/testdata/live/raw` — 60 real messages, gitignored, often absent.
#[must_use]
pub fn live_corpus_dir() -> PathBuf {
    testdata_dir().join("live").join("raw")
}

/// `backend/testdata/mime` — the committed conformance corpus.
#[must_use]
pub fn mime_corpus_dir() -> PathBuf {
    testdata_dir().join("mime")
}

/// Seed every `.eml` in a directory, sorted by file name.
///
/// A missing directory is **not** an error: it returns an empty list, because
/// the live corpus is gitignored and a fresh clone must still build and test.
///
/// A file whose name is 16 hexadecimal digits keeps that name as its message id
/// — the live corpus's file names *are* the real Gmail ids, so seeding from it
/// reproduces a real mailbox's identifiers exactly. Anything else gets an
/// allocated id.
///
/// # Errors
/// [`SimError::Seed`] for an I/O failure that is not "no such directory", and
/// whatever [`Mailbox::insert_message`] returns.
pub fn seed_from_eml_dir(
    mailbox: &mut Mailbox,
    dir: &Path,
    options: &SeedOptions,
) -> Result<Vec<Seeded>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(SimError::Seed {
                path: dir.display().to_string(),
                source,
            })
        }
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("eml"))
        })
        .collect();
    // `read_dir` order is the filesystem's, which differs between machines.
    // Sorting is what makes seeding reproducible at all.
    files.sort();
    if let Some(limit) = options.limit {
        files.truncate(limit);
    }

    let mut out = Vec::with_capacity(files.len());
    for (index, path) in files.iter().enumerate() {
        let raw = fs::read(path).map_err(|source| SimError::Seed {
            path: path.display().to_string(),
            source,
        })?;
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        #[allow(clippy::cast_possible_wrap)]
        let ladder = options
            .fallback_base_ms
            .saturating_add(options.fallback_step_ms * index as i64);
        let internal_date_ms = if options.ignore_date_headers {
            ladder
        } else {
            date_header_ms(&raw).unwrap_or(ladder)
        };

        let mut spec = MessageSpec::from_raw(raw)
            .labels(options.labels.clone())
            .received_at(ReceivedAt::AtMs(internal_date_ms));
        if looks_like_a_gmail_id(&stem) {
            spec = spec.id(&stem);
        }
        let id = mailbox.insert_message(&spec, internal_date_ms)?;
        out.push(Seeded {
            file,
            id,
            internal_date_ms,
        });
    }
    Ok(out)
}

/// Seed from the live sample. Empty when the corpus is not on this machine.
///
/// # Errors
/// As [`seed_from_eml_dir`].
pub fn seed_live(mailbox: &mut Mailbox, options: &SeedOptions) -> Result<Vec<Seeded>> {
    seed_from_eml_dir(mailbox, &live_corpus_dir(), options)
}

/// Seed from the committed MIME conformance corpus.
///
/// # Errors
/// As [`seed_from_eml_dir`].
pub fn seed_mime_corpus(mailbox: &mut Mailbox, options: &SeedOptions) -> Result<Vec<Seeded>> {
    seed_from_eml_dir(mailbox, &mime_corpus_dir(), options)
}

/// 16 lowercase hex digits, which is what a Gmail message id looks like.
fn looks_like_a_gmail_id(stem: &str) -> bool {
    stem.len() == 16 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Query;

    #[test]
    fn a_missing_directory_seeds_nothing_and_does_not_fail() {
        let mut mailbox = Mailbox::new("me@example.com");
        let seeded = seed_from_eml_dir(
            &mut mailbox,
            Path::new("/definitely/not/here"),
            &SeedOptions::default(),
        )
        .expect("a missing corpus must degrade, not fail");
        assert!(seeded.is_empty());
        assert_eq!(mailbox.message_count(), 0);
    }

    #[test]
    fn the_mime_corpus_seeds_and_every_case_survives_every_format() {
        let mut mailbox = Mailbox::new("me@example.com");
        let seeded = seed_mime_corpus(&mut mailbox, &SeedOptions::default()).unwrap();
        assert_eq!(seeded.len(), 26, "the corpus is committed; it must be here");
        assert_eq!(mailbox.message_count(), 26);

        for entry in &seeded {
            let message = mailbox.message(&entry.id).expect("seeded message");
            // Every format renders without panicking, and `raw` is exact.
            for format in [
                crate::render::Format::Minimal,
                crate::render::Format::Metadata,
                crate::render::Format::Full,
                crate::render::Format::Raw,
            ] {
                let body = crate::render::message_json(message, format, &[]);
                assert_eq!(body["id"], entry.id, "{}", entry.file);
            }
            let raw = crate::render::message_json(message, crate::render::Format::Raw, &[]);
            let decoded = crate::ids::b64url_decode(raw["raw"].as_str().unwrap()).unwrap();
            assert_eq!(decoded, message.raw, "{} must round-trip", entry.file);
        }
    }

    #[test]
    fn seeding_is_byte_identical_between_two_mailboxes() {
        let seed = || {
            let mut mailbox = Mailbox::new("me@example.com");
            let seeded = seed_mime_corpus(&mut mailbox, &SeedOptions::default()).unwrap();
            (mailbox, seeded)
        };
        let (first_box, first) = seed();
        let (second_box, second) = seed();
        assert_eq!(first, second, "ids and dates must not depend on the run");
        let ids = |mailbox: &Mailbox| -> Vec<String> {
            mailbox
                .messages_newest_first()
                .iter()
                .map(|message| message.id.clone())
                .collect()
        };
        assert_eq!(ids(&first_box), ids(&second_box));
    }

    #[test]
    fn a_message_with_no_date_header_lands_on_the_fallback_ladder() {
        // Corpus case 13 has no `Date:` at all and case 14's is unparseable.
        let mut mailbox = Mailbox::new("me@example.com");
        let seeded = seed_mime_corpus(&mut mailbox, &SeedOptions::default()).unwrap();
        for name in ["13-no-date-header.eml", "14-malformed-date.eml"] {
            let entry = seeded
                .iter()
                .find(|entry| entry.file == name)
                .unwrap_or_else(|| panic!("{name} is missing from the corpus"));
            let message = mailbox.message(&entry.id).unwrap();
            assert!(
                message.internal_date_ms > 0,
                "{name} must still have an internalDate"
            );
            assert_eq!(message.internal_date_ms, entry.internal_date_ms);
        }
    }

    #[test]
    fn a_gmail_shaped_file_name_becomes_the_message_id() {
        assert!(looks_like_a_gmail_id("1a002c5b7030af82"));
        assert!(!looks_like_a_gmail_id("01-plain-ascii"));
        assert!(!looks_like_a_gmail_id("1a002c5b7030af8"));
        assert!(!looks_like_a_gmail_id("1a002c5b7030af82x"));
    }

    /// The live corpus is gitignored. Absent is a skip, never a failure.
    #[test]
    fn the_live_corpus_seeds_when_it_is_present() {
        let mut mailbox = Mailbox::new("me@example.com");
        let seeded = seed_live(&mut mailbox, &SeedOptions::default()).unwrap();
        if seeded.is_empty() {
            eprintln!(
                "skipping: no live corpus at {} (gitignored; run backend/testdata/fetch_live.sh)",
                live_corpus_dir().display()
            );
            return;
        }
        // Real Gmail ids came through untouched.
        for entry in &seeded {
            assert!(looks_like_a_gmail_id(&entry.id), "{}", entry.file);
            assert_eq!(entry.file, format!("{}.eml", entry.id));
        }
        // …and a 30-day window from the default clock sees them all.
        let now = crate::clock::DEFAULT_NOW_MS;
        let found = mailbox.search(&Query::parse("newer_than:30d"), now, false, &[]);
        assert_eq!(
            found.len(),
            seeded.len(),
            "the live sample should sit inside a 30-day window ending at the default clock"
        );
    }

    #[test]
    fn evenly_spaced_seeding_ignores_the_date_headers() {
        let mut mailbox = Mailbox::new("me@example.com");
        let options = SeedOptions::default().evenly_spaced(1_000_000, 1_000).limit(5);
        let seeded = seed_mime_corpus(&mut mailbox, &options).unwrap();
        assert_eq!(seeded.len(), 5);
        let dates: Vec<i64> = seeded.iter().map(|entry| entry.internal_date_ms).collect();
        assert_eq!(dates, [1_000_000, 1_001_000, 1_002_000, 1_003_000, 1_004_000]);
    }

    #[test]
    fn seeded_labels_are_configurable() {
        let mut mailbox = Mailbox::new("me@example.com");
        let options = SeedOptions::default()
            .labels(["INBOX", "CATEGORY_PROMOTIONS"])
            .limit(2);
        let seeded = seed_mime_corpus(&mut mailbox, &options).unwrap();
        for entry in &seeded {
            let message = mailbox.message(&entry.id).unwrap();
            assert_eq!(message.label_ids, ["INBOX", "CATEGORY_PROMOTIONS"]);
        }
    }
}
