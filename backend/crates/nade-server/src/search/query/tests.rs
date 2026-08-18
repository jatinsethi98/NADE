//! Every rejection, and the explanation it produces.
//!
//! The explanations are asserted rather than merely the refusals. That is the
//! whole contract of this module: an agent handed an empty list cannot tell a
//! mistake from an empty mailbox, and an agent handed "unknown operator
//! `labels:`; did you mean `label:`?" fixes its own query. A rejection with a
//! useless message is a bug even though the `Err` is right.

use super::*;

/// The account this project actually runs against.
fn labels() -> LabelIndex {
    LabelIndex::new([
        ("INBOX", "INBOX"),
        ("UNREAD", "UNREAD"),
        ("SENT", "SENT"),
        ("CATEGORY_PROMOTIONS", "CATEGORY_PROMOTIONS"),
        ("Label_12", "To Reply"),
        ("Label_8725352880648854357", "Subscriptions"),
        ("Label_99", "[Gmail]All Mail"),
    ])
}

fn ok(raw: &str) -> String {
    validate(raw, &labels())
        .unwrap_or_else(|error| panic!("{raw:?} should be accepted: {}", error.message))
        .as_str()
        .to_owned()
}

/// Refuse `raw`, and require the message to contain every fragment - so the
/// explanation is asserted, not just the refusal.
fn refused(raw: &str, fragments: &[&str]) -> String {
    let message = validate(raw, &labels())
        .expect_err(&format!(
            "{raw:?} must be refused: Gmail answers it with an empty 200, so passing it through \
             is indistinguishable from an empty mailbox"
        ))
        .message;
    for fragment in fragments {
        assert!(
            message.contains(fragment),
            "the explanation for {raw:?} must contain {fragment:?}, got:\n  {message}"
        );
    }
    message
}

// ------------------------------------------------------------- the basics --

#[test]
fn a_plain_query_passes_through_unchanged() {
    assert_eq!(ok("invoice"), "invoice");
    assert_eq!(ok("staff product designer"), "staff product designer");
    assert_eq!(ok("  invoice  "), "invoice");
    // Unicode is a search term like any other.
    assert_eq!(ok("配送のお知らせ"), "配送のお知らせ");
    assert_eq!(ok("Rechnung für Café"), "Rechnung für Café");
}

/// EDGE (empty input): an empty `q` must not mean "every message you own".
#[test]
fn an_empty_query_is_refused() {
    for raw in ["", "   ", "\t\n", "()", "{}", "-"] {
        let error = validate(raw, &labels()).expect_err(&format!("{raw:?}"));
        assert_eq!(error.message, "Type something to search for.", "{raw:?}");
    }
}

#[test]
fn an_oversized_query_is_refused() {
    let long = "a".repeat(MAX_QUERY_CHARS + 1);
    refused(&long, &["too long", "512"]);
    // Exactly at the cap is fine, and the cap counts characters rather than
    // bytes: 512 emoji is 2 048 bytes and still a legal query.
    assert!(validate(&"a".repeat(MAX_QUERY_CHARS), &labels()).is_ok());
    assert!(validate(&"🚀".repeat(MAX_QUERY_CHARS), &labels()).is_ok());
}

#[test]
fn known_operators_are_accepted_and_case_folded() {
    // MEASURED: operator *names* fold case. `IS:UNREAD` is `is:unread`.
    assert_eq!(ok("IS:UNREAD"), "is:unread");
    assert_eq!(ok("From:priya@kettle.com"), "from:priya@kettle.com");
    assert_eq!(ok("NEWER_THAN:30d"), "newer_than:30d");
    assert_eq!(
        ok("from:priya@kettle.com has:attachment newer_than:30d"),
        "from:priya@kettle.com has:attachment newer_than:30d"
    );
    // Negation, grouping and the OR braces survive the round trip.
    assert_eq!(ok("-from:noreply@x.com"), "-from:noreply@x.com");
    assert_eq!(
        ok("{from:a@x.com from:b@x.com}"),
        "{from:a@x.com from:b@x.com}"
    );
    assert_eq!(ok("from:(alice)"), "from:(alice)");
}

// -------------------------------------------------------- unknown operator --

/// MEASURED: an unknown operator matches nothing, silently. It is never a 400
/// from Gmail, so it has to be one from us.
#[test]
fn an_unknown_operator_is_refused_by_name() {
    let message = refused("labels:Subscriptions", &["Unknown operator", "`labels:`"]);
    assert!(
        message.contains("`label:`"),
        "a one-letter typo should suggest the real operator:\n  {message}"
    );

    refused("form:priya@kettle.com", &["Unknown operator", "`from:`"]);
    refused("wibble:wobble", &["Unknown operator", "`wibble:`"]);
    // The suggestion is optional; the naming is not.
    refused("zzzzzzz:x", &["Unknown operator", "`zzzzzzz:`"]);
}

/// A URL is the honest false positive of "anything before a colon is an
/// operator", so the message has to carry the way out.
#[test]
fn a_url_is_refused_with_the_way_out() {
    let message = refused(
        "https://example.com/invoice",
        &["Unknown operator", "quote"],
    );
    assert!(
        message.contains("\"https://example.com/invoice\""),
        "the message must show the quoted form that works:\n  {message}"
    );
    // And that form really does work.
    assert_eq!(
        ok("\"https://example.com/invoice\""),
        "\"https://example.com/invoice\""
    );
}

/// A colon that is not an operator at all must not be dragged into this.
#[test]
fn a_colon_inside_a_term_is_not_an_operator() {
    // The name must be a plain word, so neither of these is a candidate.
    assert_eq!(ok("12:30"), "12:30");
    assert_eq!(ok("a.b:c"), "a.b:c");
    assert_eq!(ok(":leading"), ":leading");
}

// ------------------------------------------------------- label id vs name --

/// MEASURED: `label:Subscriptions` returns thousands;
/// `label:Label_8725352880648854357` returns **zero**. The id is exactly what a
/// program has to hand, so this is the likely mistake - and the recoverable
/// one, so it is translated rather than only refused.
#[test]
fn a_label_id_is_translated_to_its_name() {
    assert_eq!(
        ok("label:Label_8725352880648854357"),
        "label:Subscriptions",
        "an id we know is translated, not refused - it is the recoverable case"
    );
    // A name with a space has to be quoted or Gmail reads the second word as a
    // separate term.
    assert_eq!(ok("label:Label_12"), "label:\"To Reply\"");
    assert_eq!(
        ok("label:Label_12 is:unread"),
        "label:\"To Reply\" is:unread",
        "translation must not eat the rest of the query"
    );
    // `in:` takes a label too, and takes it by name.
    assert_eq!(ok("in:Label_12"), "in:\"To Reply\"");

    // A system label's name *is* its id, so nothing changes and nothing breaks.
    assert_eq!(ok("label:INBOX"), "label:INBOX");
    // A name is already a name.
    assert_eq!(ok("label:Subscriptions"), "label:Subscriptions");
    assert_eq!(ok("label:\"To Reply\""), "label:\"To Reply\"");
}

#[test]
fn an_unknown_label_id_is_refused_with_a_real_name_to_use() {
    let message = refused("label:Label_404", &["label id", "name", "Label_404"]);
    assert!(
        message.contains("Subscriptions") || message.contains("To Reply"),
        "the message should offer a label this account actually has:\n  {message}"
    );

    // With no labels known at all, it still explains rather than guessing.
    let message = validate("label:Label_404", &LabelIndex::empty())
        .unwrap_err()
        .message;
    assert!(message.contains("mailbox list"), "{message}");

    // Anything that is not id-shaped is a name we simply have not synced yet,
    // and refusing it would refuse a legitimate search.
    assert_eq!(ok("label:holiday-2027"), "label:holiday-2027");
}

// ------------------------------------------------------------- date units --

/// MEASURED: `newer_than:` supports `h`, `d`, `m`, `y`. It does **not**
/// support `w`, and it does not accept a bare number. Both match nothing.
#[test]
fn unsupported_age_units_are_refused_with_the_supported_one() {
    let message = refused("newer_than:1w", &["Weeks are not a unit", "newer_than:7d"]);
    assert!(
        message.contains("matches nothing"),
        "the message must say why silence was the alternative:\n  {message}"
    );
    refused("newer_than:2w", &["newer_than:14d"]);
    refused("older_than:3w", &["older_than:21d"]);

    let message = refused("newer_than:7", &["bare number", "newer_than:7d"]);
    assert!(message.contains('h') && message.contains('y'), "{message}");

    refused("newer_than:7s", &["`s` is not a unit"]);
    refused("newer_than:7dd", &["`dd` is not a unit"]);
    refused("newer_than:yesterday", &["number followed by"]);

    for good in [
        "newer_than:6h",
        "newer_than:30d",
        "newer_than:1m",
        "older_than:2y",
    ] {
        assert_eq!(ok(good), good);
    }
}

/// A bare date is midnight **UTC**, despite Google's documentation saying
/// Pacific - measured. The format is the part we can enforce.
#[test]
fn an_unparseable_date_is_refused_with_the_format() {
    let message = refused("after:yesterday", &["YYYY/MM/DD", "midnight UTC"]);
    assert!(message.contains("matches nothing"), "{message}");
    refused("after:2026", &["YYYY/MM/DD"]);
    refused("before:08/10/2026", &["YYYY/MM/DD"]);
    refused("after:2026/13/01", &["not a real date"]);
    refused("after:2026/08/32", &["not a real date"]);

    assert_eq!(ok("after:2026/08/10"), "after:2026/08/10");
    assert_eq!(ok("before:2026-08-10"), "before:2026-08-10");
    assert_eq!(ok("older:2026/08/10"), "older:2026/08/10");
    // Epoch seconds are unambiguous and Gmail takes them.
    assert_eq!(ok("after:1786950724"), "after:1786950724");
}

// ------------------------------------------------------- closed vocabularies --

#[test]
fn an_unknown_value_for_a_closed_operator_is_refused_with_the_list() {
    let message = refused("is:unseen", &["`is:unseen`", "`unread`", "`starred`"]);
    assert!(message.contains("matches nothing"), "{message}");

    refused("category:newsletters", &["`promotions`", "`updates`"]);
    refused("has:pdf", &["`attachment`", "yellow-star"]);

    for good in [
        "is:unread",
        "is:starred",
        "category:promotions",
        "has:attachment",
        "has:userlabels",
        "has:yellow-star",
        "has:blue-info",
    ] {
        assert_eq!(ok(good), good);
    }
    // Values fold case too.
    assert_eq!(ok("is:UNREAD"), "is:unread");
}

#[test]
fn a_size_must_be_a_size() {
    for good in ["larger:10m", "smaller:500k", "size:1048576"] {
        assert_eq!(ok(good), good);
    }
    refused("larger:big", &["not a size", "larger:10m"]);
    refused("larger:10gb", &["not a size"]);
}

// ------------------------------------------------- colons, spaces, emptiness --

/// MEASURED: a space after the colon is tolerated - `from: x` filters exactly
/// like `from:x` - so it is normalised rather than refused.
#[test]
fn a_space_after_the_colon_is_normalised_rather_than_refused() {
    assert_eq!(ok("from: priya@kettle.com"), "from:priya@kettle.com");
    assert_eq!(ok("subject: invoice"), "subject:invoice");
    assert_eq!(
        ok("from: priya@kettle.com is:unread"),
        "from:priya@kettle.com is:unread"
    );
    // A label id offered across the space is still translated.
    assert_eq!(ok("label: Label_12"), "label:\"To Reply\"");

    // The argument is taken **whole**. Peeling it first would drop the `-` and
    // search for exactly what the caller asked to exclude.
    assert_eq!(ok("subject: -draft"), "subject:-draft");
}

/// MEASURED: an operator with a genuinely **empty** argument is a no-op that
/// matches *everything*, which is the exact opposite of what the caller meant
/// and is invisible in the result.
#[test]
fn an_empty_argument_is_refused_because_it_matches_everything() {
    for raw in ["from:", "subject:", "newer_than:"] {
        let operator = raw.trim_end_matches(':');
        let message = refused(raw, &["no argument", "match-everything"]);
        assert!(message.contains(operator), "{message}");
    }

    // The next word is only the argument if it *could* be one. A second
    // operator after the colon means the first was left empty.
    refused("from: is:unread", &["`from:` has no argument"]);
    refused("newer_than:30d from:", &["`from:` has no argument"]);
    // And a closing bracket is not an argument either.
    refused("(from: )", &["`from:` has no argument"]);
}

// ------------------------------------------------------------ boolean case --

/// MEASURED: operator names fold case, but the boolean `OR` does not.
/// Lowercase `or` is matched as the literal three-letter word and silently
/// ANDed with the terms either side, so `invoice or receipt` asks for messages
/// containing all three words - and finds none.
#[test]
fn a_lowercase_or_is_refused_because_gmail_reads_it_as_a_word() {
    for raw in [
        "invoice or receipt",
        "invoice Or receipt",
        "invoice oR receipt",
    ] {
        let message = refused(raw, &["boolean OR", "`OR`"]);
        assert!(
            message.contains("literal word"),
            "the message must say what Gmail actually does with it:\n  {message}"
        );
    }

    // Uppercase is the real operator and passes through.
    assert_eq!(ok("invoice OR receipt"), "invoice OR receipt");
    // And quoting is the way to search for the word itself.
    assert_eq!(ok("\"or\""), "\"or\"");
    // `AND` is implicit in Gmail and lowercase `and` in a sentence is far more
    // often incidental than intended, so it is left alone - unlike `or`, where
    // the literal reading silently empties the result.
    assert_eq!(ok("hotels and flights"), "hotels and flights");
}

// ---------------------------------------------------------------- scoping --

/// MEASURED: `includeSpamTrash=false` gates neither `q` nor `labelIds`.
/// `in:anywhere`, `in:trash` and `in:spam` widen the scope straight past it.
/// A caller that types one means it, so they are recorded rather than refused -
/// keeping Trash out is a property of *not writing an `in:`*.
#[test]
fn scope_operators_are_allowed_and_flagged() {
    for raw in ["in:anywhere", "in:trash", "in:spam", "IN:ANYWHERE"] {
        let valid = validate(raw, &labels()).unwrap();
        assert!(
            valid.widens_scope(),
            "{raw} reaches past Trash and Spam whatever includeSpamTrash says"
        );
    }
    assert!(!validate("in:inbox", &labels()).unwrap().widens_scope());
    assert!(!validate("invoice", &labels()).unwrap().widens_scope());
    // A query is flagged if any part of it widens.
    assert!(validate("invoice in:anywhere", &labels())
        .unwrap()
        .widens_scope());
}

// ------------------------------------------------------------- quoting --

#[test]
fn a_quoted_phrase_is_a_literal_and_is_never_parsed_as_an_operator() {
    assert_eq!(ok("\"is:unread\""), "\"is:unread\"");
    assert_eq!(ok("subject:\"two words\""), "subject:\"two words\"");
    assert_eq!(ok("\"wibble:wobble\""), "\"wibble:wobble\"");
    // An unterminated quote swallows the rest, which is what Gmail does too -
    // and must not panic or split mid-character.
    assert_eq!(ok("\"unterminated 配送"), "\"unterminated 配送");
}

// ------------------------------------------------------- the helper itself --

#[test]
fn the_near_miss_suggestion_does_not_suggest_nonsense() {
    assert!(is_near("labels", "label"));
    assert!(is_near("form", "from"));
    assert!(is_near("subjec", "subject"));
    assert!(is_near("froms", "from"));
    assert!(!is_near("wibble", "label"));
    assert!(!is_near("https", "has"));
    // Never panics on unicode, whatever the byte lengths.
    assert!(!is_near("配送", "label"));
}

#[test]
fn words_are_split_on_whitespace_but_not_inside_quotes() {
    let words = split_words("from:a \"two words\" -label:x)");
    assert_eq!(words.len(), 3);
    assert_eq!(words[0].core(), Some("from:a"));
    assert_eq!(words[1].core(), Some("\"two words\""));
    assert_eq!(words[2].prefix, "-");
    assert_eq!(words[2].core(), Some("label:x"));
    assert_eq!(words[2].suffix, ")");

    // Pure punctuation carries no core and is passed straight through.
    let words = split_words("( )");
    assert_eq!(words[0].core(), None);
}
