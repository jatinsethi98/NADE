//! The mechanical promises, checked mechanically.
//!
//! Three of this crate's rules are the kind that hold on the day they are
//! written and quietly stop holding six commits later, because breaking them
//! looks like a convenience rather than a mistake:
//!
//! * it models **Gmail**, so it must never depend on a NADE crate;
//! * it must never read a wall clock;
//! * it must never use an RNG.
//!
//! A reviewer cannot be relied on to notice a `SystemTime::now()` in a diff.
//! These tests can.

use std::{fs, path::Path};

fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The manifest with its comments stripped.
///
/// The comments deliberately *name* the crates this one must not depend on, so
/// scanning the raw text would fail on the very note explaining the rule.
fn manifest() -> String {
    let raw =
        fs::read_to_string(crate_root().join("Cargo.toml")).expect("the crate has a manifest");
    raw.lines()
        .map(|line| line.split_once('#').map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under `src/`, as `(relative path, contents)`.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                out.push((relative, fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&crate_root().join("src"), crate_root(), &mut out);
    assert!(out.len() > 10, "the walker found almost nothing: {out:?}");
    out
}

#[test]
fn no_nade_dependencies() {
    let manifest = manifest();
    for forbidden in [
        "nade-server",
        "nade_server",
        "nade-agent-sdk",
        "nade_agent_sdk",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "this crate models Gmail, not NADE; it must not depend on {forbidden}"
        );
    }
    for (path, source) in sources() {
        for forbidden in ["nade_server", "nade_agent_sdk"] {
            assert!(!source.contains(forbidden), "{path} refers to {forbidden}");
        }
    }
}

#[test]
fn no_mail_parser_dependency() {
    // The simulator and the code under test must not share a MIME parser. If
    // they did, a bug in that parser would be invisible to both and the tests
    // would agree on the wrong answer.
    let manifest = manifest();
    for forbidden in ["mail-parser", "mailparse", "lettre", "email-parser"] {
        assert!(
            !manifest.contains(forbidden),
            "the simulator must walk MIME itself, not with {forbidden}"
        );
    }
}

#[test]
fn chrono_is_taken_without_its_clock_feature() {
    // This is the compile-time half of the no-wall-clock rule: with
    // `default-features = false` and no `clock` feature, `chrono::Utc::now` is
    // not compiled into the dependency graph at all, so it cannot be called even
    // by accident.
    let manifest = manifest();
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("chrono ="))
        .expect("chrono is a dependency");
    assert!(
        line.contains("default-features = false"),
        "chrono must not bring its `clock` feature: {line}"
    );
    assert!(
        !line.contains("\"clock\""),
        "the `clock` feature would restore Utc::now: {line}"
    );
}

/// A source file with its `#[cfg(test)]` module chopped off.
///
/// The rule is about the **response path**, not about the crate's own tests: a
/// test that times a decoder legitimately calls `Instant::now`, and it can never
/// reach a response. Everything from the first `#[cfg(test)]` onwards is in-file
/// tests, so cutting there scopes the scan to shipped code.
fn library_code(source: &str) -> String {
    source
        .split_once("#[cfg(test)]")
        .map_or(source, |(before, _)| before)
        .to_owned()
}

#[test]
fn no_wall_clock_and_no_rng_in_the_library() {
    // Each needle is something that would silently make responses differ between
    // two runs of the same script.
    const FORBIDDEN: [(&str, &str); 8] = [
        ("SystemTime::now", "reads the wall clock"),
        ("Instant::now", "reads the monotonic clock"),
        ("Utc::now", "reads the wall clock"),
        ("Local::now", "reads the wall clock"),
        ("new_v4", "a random UUID"),
        ("thread_rng", "an RNG"),
        ("rand::", "an RNG"),
        ("DefaultHasher", "not stable across compiler releases"),
    ];

    for (path, source) in sources() {
        let source = library_code(&source);
        for (needle, why) in FORBIDDEN {
            // Prose in a doc comment may name the thing it is promising not to
            // do, so only real code counts.
            let offenders: Vec<&str> = source
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("//") && !trimmed.starts_with("*")
                })
                .filter(|line| line.contains(needle))
                .collect();
            assert!(
                offenders.is_empty(),
                "{path} uses `{needle}` ({why}), which breaks byte-identical replay:\n{}",
                offenders.join("\n")
            );
        }
    }
}

#[test]
fn the_library_forbids_unsafe() {
    let root = fs::read_to_string(crate_root().join("src").join("lib.rs")).expect("lib.rs");
    assert!(root.contains("#![forbid(unsafe_code)]"));
}

#[test]
fn the_criteria_document_exists_and_lists_the_edge_cases() {
    let criteria = fs::read_to_string(crate_root().join("CRITERIA.md"))
        .expect("CRITERIA.md is written before the code, per the execution doctrine");
    // The brief's minimum checklist, each of which must be named in the file.
    for required in [
        "empty mailbox",
        "pagination exactly on a boundary",
        "deleted mid-pagination",
        "Replayed history",
        "far in the future",
        "far in the past",
        "removed twice",
        "Batch of 1",
        "Batch of 100",
        "every sub-request fails",
        "RFC-2047",
        "8 MB",
    ] {
        assert!(
            criteria.to_lowercase().contains(&required.to_lowercase()),
            "CRITERIA.md does not mention {required:?}"
        );
    }
}

#[test]
fn every_public_module_is_documented() {
    // `#![warn(missing_docs)]` catches items; this catches a module that was
    // added to lib.rs without one.
    let root = fs::read_to_string(crate_root().join("src").join("lib.rs")).expect("lib.rs");
    assert!(root.contains("#![warn(missing_docs"));
    for (path, source) in sources() {
        if path.ends_with("lib.rs") {
            continue;
        }
        assert!(
            source.starts_with("//!"),
            "{path} has no module-level documentation"
        );
    }
}
