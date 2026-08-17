//! Gmail's `q` search syntax: a real parser, not a substring check.
//!
//! `messages.list?q=…` is the first call an initial sync makes
//! (`q="newer_than:30d"`), so getting it wrong silently changes what a sync
//! *is*. The parser handles implicit `AND`, explicit `OR`, `-` negation,
//! parentheses, `{…}` grouping, quoted phrases, and the operators below.
//!
//! **Anything it does not recognise becomes a free-text term**, because Gmail
//! never rejects a query — it just searches for the text. There is no published
//! evidence that `messages.list` has *ever* returned `400` for a malformed `q`;
//! the observed answer is `200` with the body `{"resultSizeEstimate": 0}`. The
//! operational consequence is the important part, and this crate reproduces it
//! faithfully: **a malformed query is indistinguishable from an empty inbox.**
//! A client must validate `q` itself, because no error is coming — which matters
//! most for LLM-generated queries, where a hallucinated operator looks exactly
//! like a quiet mailbox.
//!
//! # What is measured, and what is only documented
//!
//! Several of these were settled by a live read-only probe against a real
//! mailbox, scoped to `newer_than:3d` (85 messages) so that counts
//! discriminate. Where the probe and the documentation disagree, **the probe
//! wins** — this is a fidelity simulator, and a simulator that rejects what
//! Gmail accepts teaches the client to avoid perfectly good queries, which is
//! its own kind of wrong.
//!
//! | Behaviour | Status |
//! |---|---|
//! | `newer_than:` accepts `h` (`24h` == `1d`) | **measured**; undocumented, and it works |
//! | `newer_than:` accepts `d`, `m`, `y` | documented **and** measured |
//! | `newer_than:1w`, `newer_than:30` (no unit) | **measured**: return nothing. Refused here |
//! | `\|` is an OR alias — `(a \| b)` == `(a OR b)` | **measured**; documented nowhere |
//! | Operator **names** are case-insensitive (`IS:UNREAD`) | **measured** |
//! | The boolean keyword `or` is case-**sensitive** | **measured**: lowercase `or` matches the literal word |
//! | `from: x` (space after the colon) still filters | **measured**: identical to `from:x` |
//! | `from:` with a genuinely empty argument is a no-op | **measured**: returns the unfiltered baseline |
//! | An unknown operator matches nothing, never a `400` | **measured** (`zzz:qqq` → 0) |
//! | `q=label:` is case-insensitive | **measured** |
//! | A bare date is **midnight Pacific**, not UTC | documented only |
//! | Ranges are half-open `[after, before)` | documented only, via Google's worked example |
//! | `m` = 30 days, `y` = 365 days | **unmeasured** — a stated approximation |
//!
//! Only `MM/DD/YYYY` is refused despite being documented: `03/04/2004` is
//! genuinely ambiguous against `YYYY/MM/DD` and Google publishes no
//! disambiguation rule, so zero results is safer than three wrong months.
//!
//! What is *not* modelled, and is safe not to be: stemming, synonyms, `AROUND`,
//! wildcards, and Gmail's own spam/category classification. Ordering is
//! newest-first, which is what Google's list-messages guide documents; the real
//! sort key looks like message id descending, so two messages delivered in the
//! same second can come back in the opposite order to their timestamps. The
//! simulator's tie-break is id descending for exactly that reason.

use crate::{clock::DAY_MS, label::normalise_label_query};

/// A parsed query. Match it against a [`Searchable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Matches everything — an empty `q`.
    All,
    /// Every child must match.
    And(Vec<Query>),
    /// At least one child must match.
    Or(Vec<Query>),
    /// The child must not match.
    Not(Box<Query>),
    /// A leaf condition.
    Term(Term),
}

/// One leaf condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// `newer_than:30d` — `internalDate` strictly newer than now minus the span.
    NewerThan(i64),
    /// `older_than:1y`.
    OlderThan(i64),
    /// `after:2026/07/01`, or an epoch-seconds value.
    After(i64),
    /// `before:2026/07/01`.
    Before(i64),
    /// `label:` / `in:` — matched against label ids and names, folded.
    Label(String),
    /// `from:`.
    From(String),
    /// `to:`.
    To(String),
    /// `cc:`.
    Cc(String),
    /// `bcc:`.
    Bcc(String),
    /// `subject:`.
    Subject(String),
    /// `filename:`.
    Filename(String),
    /// `list:`.
    List(String),
    /// `rfc822msgid:` — an exact `Message-ID` header match.
    Rfc822MsgId(String),
    /// `is:unread`, `is:read`, `is:starred`, `is:important`.
    Is(State),
    /// `has:attachment`, `has:userlabels`, `has:nouserlabels`.
    Has(Attribute),
    /// `larger:5M` — `sizeEstimate` above this many bytes.
    Larger(u64),
    /// `smaller:5M`.
    Smaller(u64),
    /// A bare word or quoted phrase.
    Text(String),
}

/// The `is:` states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Carries `UNREAD`.
    Unread,
    /// Does not carry `UNREAD`.
    Read,
    /// Carries `STARRED`.
    Starred,
    /// Carries `IMPORTANT`.
    Important,
}

/// The `has:` attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribute {
    /// At least one part Gmail would give an `attachmentId`.
    Attachment,
    /// At least one user label.
    UserLabels,
    /// No user labels.
    NoUserLabels,
}

/// Everything a query can look at, precomputed once per message.
///
/// Built at insert time so a `list` call does not reparse every message — and,
/// more importantly, so the searchable text is derived from the message exactly
/// once, deterministically, rather than depending on when a query happened to
/// run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Searchable {
    /// Decoded subject, lowercased.
    pub subject: String,
    /// Decoded `From`, lowercased.
    pub from: String,
    /// Decoded `To`, lowercased.
    pub to: String,
    /// Decoded `Cc`, lowercased.
    pub cc: String,
    /// Decoded `Bcc`, lowercased.
    pub bcc: String,
    /// `List-Id` / `List-Unsubscribe`, lowercased.
    pub list: String,
    /// The `Message-ID` header, angle brackets stripped, lowercased.
    pub rfc822_msgid: String,
    /// Attachment filenames, lowercased.
    pub filenames: Vec<String>,
    /// Body text, lowercased, whitespace-collapsed.
    pub body: String,
    /// Whether any part would carry an `attachmentId`.
    pub has_attachment: bool,
}

impl Searchable {
    /// The haystack a bare word searches: subject, sender, recipients, body.
    fn free_text(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.subject, self.from, self.to, self.cc, self.body
        )
    }
}

/// The message facts a query needs that are not text.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// `internalDate`, epoch milliseconds.
    pub internal_date_ms: i64,
    /// The message's labels.
    pub label_ids: &'a [String],
    /// `sizeEstimate`.
    pub size: u64,
    /// Now, from the simulator's clock.
    pub now_ms: i64,
}

impl Query {
    /// Parse a `q` value. Never fails; unknown syntax becomes free text.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let tokens = tokenise(input);
        if tokens.is_empty() {
            return Self::All;
        }
        let mut cursor = Cursor { tokens, at: 0 };
        let parsed = cursor.parse_or();
        parsed.unwrap_or(Self::All)
    }

    /// Does this message match?
    ///
    /// `label_name` resolves a label id to its display name, so `label:receipts`
    /// can match the user label whose id is `Label_4`.
    pub fn matches(
        &self,
        text: &Searchable,
        facts: Facts<'_>,
        label_name: &dyn Fn(&str) -> Option<String>,
    ) -> bool {
        match self {
            Self::All => true,
            Self::And(children) => children
                .iter()
                .all(|child| child.matches(text, facts, label_name)),
            Self::Or(children) => children
                .iter()
                .any(|child| child.matches(text, facts, label_name)),
            Self::Not(child) => !child.matches(text, facts, label_name),
            Self::Term(term) => term.matches(text, facts, label_name),
        }
    }
}

impl Term {
    fn matches(
        &self,
        text: &Searchable,
        facts: Facts<'_>,
        label_name: &dyn Fn(&str) -> Option<String>,
    ) -> bool {
        match self {
            // EDGE: strictly greater-than. A message exactly `30d` old is
            // outside `newer_than:30d`, which is the boundary a 30-day sync
            // window lands on every single day.
            Self::NewerThan(span) => facts.internal_date_ms > facts.now_ms - span,
            Self::OlderThan(span) => facts.internal_date_ms < facts.now_ms - span,
            Self::After(at) => facts.internal_date_ms >= *at,
            Self::Before(at) => facts.internal_date_ms < *at,
            Self::Label(wanted) => facts.label_ids.iter().any(|id| {
                normalise_label_query(id) == *wanted
                    || label_name(id).is_some_and(|name| normalise_label_query(&name) == *wanted)
            }),
            Self::From(value) => text.from.contains(value),
            Self::To(value) => text.to.contains(value),
            Self::Cc(value) => text.cc.contains(value),
            Self::Bcc(value) => text.bcc.contains(value),
            Self::Subject(value) => text.subject.contains(value),
            Self::Filename(value) => text.filenames.iter().any(|name| name.contains(value)),
            Self::List(value) => text.list.contains(value),
            Self::Rfc822MsgId(value) => text.rfc822_msgid == *value,
            Self::Is(state) => match state {
                State::Unread => has(facts.label_ids, "UNREAD"),
                State::Read => !has(facts.label_ids, "UNREAD"),
                State::Starred => has(facts.label_ids, "STARRED"),
                State::Important => has(facts.label_ids, "IMPORTANT"),
            },
            Self::Has(attribute) => match attribute {
                Attribute::Attachment => text.has_attachment,
                Attribute::UserLabels => facts
                    .label_ids
                    .iter()
                    .any(|id| !crate::label::is_system(id)),
                Attribute::NoUserLabels => !facts
                    .label_ids
                    .iter()
                    .any(|id| !crate::label::is_system(id)),
            },
            Self::Larger(size) => facts.size > *size,
            Self::Smaller(size) => facts.size < *size,
            Self::Text(word) => matches_free_text(&text.free_text(), word),
        }
    }
}

fn has(labels: &[String], wanted: &str) -> bool {
    labels.iter().any(|id| id == wanted)
}

/// Gmail matches whole words, not substrings: `cat` does not find `category`.
///
/// A quoted phrase (one containing a space) is matched as a substring, which is
/// what quoting means. Otherwise the haystack is tokenised the way an index
/// would: `billing@stripe.com` yields `billing`, `stripe.com`, `stripe` and
/// `com`, so all four find it and `strip` finds none of them.
///
/// This is an approximation of Gmail's index — no stemming, no synonyms — and it
/// errs towards *stricter*, so a client cannot pass a test by accident on a
/// partial word.
fn matches_free_text(haystack: &str, needle: &str) -> bool {
    const JOINERS: [char; 3] = ['.', '-', '_'];
    if needle.is_empty() {
        return true;
    }
    if needle.contains(' ') {
        return haystack.contains(needle);
    }
    haystack
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && !JOINERS.contains(&c))
        .map(|word| word.trim_matches(|c| JOINERS.contains(&c) || c == '\''))
        .any(|word| word == needle || word.split(JOINERS).any(|piece| piece == needle))
}

// ---------------------------------------------------------------------------
// Tokenising and parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Or,
    Minus,
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
}

fn tokenise(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let current = chars[index];
        match current {
            c if c.is_whitespace() => index += 1,
            '(' => {
                out.push(Token::OpenParen);
                index += 1;
            }
            ')' => {
                out.push(Token::CloseParen);
                index += 1;
            }
            '{' => {
                out.push(Token::OpenBrace);
                index += 1;
            }
            '}' => {
                out.push(Token::CloseBrace);
                index += 1;
            }
            '-' => {
                // A leading `-` negates; one inside a word is part of it.
                out.push(Token::Minus);
                index += 1;
            }
            _ => {
                let (word, next) = read_word(&chars, index);
                index = next;
                // MEASURED, and two different rules that coexist:
                //
                // * the **boolean keyword** `OR` is case-**sensitive** — a
                //   lowercase `or` is matched as the literal word, so
                //   lowercasing a user's query silently changes its meaning;
                // * `|` really is an OR alias. It is documented nowhere, and
                //   Gmail implements it: `(from:a | from:b)` returns exactly
                //   what `(from:a OR from:b)` returns. Undocumented is not the
                //   same as absent, and for a fidelity simulator the observed
                //   behaviour wins.
                //
                // Operator *names* are a third case, handled in `parse_term`:
                // those are case-insensitive (`IS:UNREAD` == `is:unread`).
                if word == "OR" || word == "|" {
                    out.push(Token::Or);
                } else if word == "AND" {
                    // Documented, but implicit already; drop it.
                } else if !word.is_empty() {
                    out.push(Token::Word(word));
                }
            }
        }
    }
    out
}

/// Read one word, honouring quotes so `subject:"a b"` stays a single token.
fn read_word(chars: &[char], start: usize) -> (String, usize) {
    let mut out = String::new();
    let mut index = start;
    let mut quoted = false;
    while index < chars.len() {
        let current = chars[index];
        if current == '"' {
            quoted = !quoted;
            index += 1;
            continue;
        }
        if !quoted && (current.is_whitespace() || "(){}".contains(current)) {
            break;
        }
        out.push(current);
        index += 1;
    }
    (out, index)
}

struct Cursor {
    tokens: Vec<Token>,
    at: usize,
}

impl Cursor {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn parse_or(&mut self) -> Option<Query> {
        let mut branches = vec![self.parse_and()?];
        while matches!(self.peek(), Some(Token::Or)) {
            self.at += 1;
            match self.parse_and() {
                Some(next) => branches.push(next),
                // EDGE: a trailing `OR` with nothing after it. Gmail tolerates
                // it; so does this, by ignoring the dangling operator.
                None => break,
            }
        }
        Some(if branches.len() == 1 {
            branches.remove(0)
        } else {
            Query::Or(branches)
        })
    }

    fn parse_and(&mut self) -> Option<Query> {
        let mut parts = Vec::new();
        loop {
            match self.peek() {
                None | Some(Token::Or | Token::CloseParen | Token::CloseBrace) => break,
                _ => {}
            }
            let Some(next) = self.parse_unary() else {
                break;
            };
            parts.push(next);
        }
        match parts.len() {
            0 => None,
            1 => Some(parts.remove(0)),
            _ => Some(Query::And(parts)),
        }
    }

    fn parse_unary(&mut self) -> Option<Query> {
        if matches!(self.peek(), Some(Token::Minus)) {
            self.at += 1;
            // EDGE: a lone `-`. Without this guard it would recurse to `None`
            // and the `parse_and` loop would spin forever on the same token.
            let inner = self.parse_unary()?;
            return Some(Query::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<Query> {
        match self.peek().cloned() {
            Some(Token::OpenParen) => {
                self.at += 1;
                let inner = self.parse_or();
                if matches!(self.peek(), Some(Token::CloseParen)) {
                    self.at += 1;
                }
                inner
            }
            // `{a b}` is Gmail's OR grouping.
            Some(Token::OpenBrace) => {
                self.at += 1;
                let mut branches = Vec::new();
                while !matches!(self.peek(), None | Some(Token::CloseBrace)) {
                    match self.parse_unary() {
                        Some(next) => branches.push(next),
                        None => break,
                    }
                }
                if matches!(self.peek(), Some(Token::CloseBrace)) {
                    self.at += 1;
                }
                match branches.len() {
                    0 => None,
                    1 => Some(branches.remove(0)),
                    _ => Some(Query::Or(branches)),
                }
            }
            Some(Token::Word(word)) => {
                self.at += 1;
                // MEASURED: `from: chase.com` returns exactly what
                // `from:chase.com` returns. Gmail skips the whitespace and still
                // applies the operator, so the tokeniser's split must be undone.
                if let Some((operator, argument)) = word.split_once(':') {
                    if argument.is_empty() && !operator.is_empty() {
                        // …but only a *plain* word is swallowed as the argument.
                        // `from: is:unread` must leave `is:unread` as its own
                        // filter rather than searching for a sender literally
                        // called "is:unread". Unmeasured, and the only reading
                        // that keeps both halves of the query meaningful.
                        if let Some(Token::Word(next)) = self.peek().cloned() {
                            if !next.contains(':') {
                                self.at += 1;
                                return Some(Query::Term(parse_term(&format!("{word}{next}"))));
                            }
                        }
                        // MEASURED: `newer_than:3d from:` returns the full
                        // `newer_than:3d` baseline — an operator with a
                        // genuinely empty argument is a no-op, not a filter and
                        // not a search for the word.
                        return Some(Query::All);
                    }
                }
                Some(Query::Term(parse_term(&word)))
            }
            Some(Token::CloseParen | Token::CloseBrace) => {
                // Unbalanced. Consume it so the caller cannot loop.
                self.at += 1;
                None
            }
            Some(Token::Minus | Token::Or) | None => None,
        }
    }
}

fn parse_term(word: &str) -> Term {
    let Some((operator, value)) = word.split_once(':') else {
        return Term::Text(word.to_ascii_lowercase());
    };
    let value = value.trim_matches('"');
    // EDGE: `from: x` — a space after the colon. The tokeniser has already split
    // that into `from:` and `x`, and Gmail treats a bare `from:` as free text
    // rather than as an operator with an empty argument. Without this, an empty
    // argument would `contains("")` and match **every** message, turning a
    // client's stray space into a silently unfiltered sync.
    if value.is_empty() {
        return Term::Text(word.to_ascii_lowercase());
    }
    let folded = value.to_ascii_lowercase();
    match operator.to_ascii_lowercase().as_str() {
        "newer_than" => span_ms(&folded).map_or_else(fallback(word), Term::NewerThan),
        "older_than" => span_ms(&folded).map_or_else(fallback(word), Term::OlderThan),
        "after" | "newer" => date_ms(&folded).map_or_else(fallback(word), Term::After),
        "before" | "older" => date_ms(&folded).map_or_else(fallback(word), Term::Before),
        "label" | "in" => Term::Label(normalise_label_query(value)),
        "from" => Term::From(folded),
        "to" => Term::To(folded),
        "cc" => Term::Cc(folded),
        "bcc" => Term::Bcc(folded),
        "subject" => Term::Subject(folded),
        "filename" => Term::Filename(folded),
        "list" => Term::List(folded),
        "rfc822msgid" => Term::Rfc822MsgId(folded.trim_matches(['<', '>']).to_owned()),
        "is" => match folded.as_str() {
            "unread" => Term::Is(State::Unread),
            "read" => Term::Is(State::Read),
            "starred" => Term::Is(State::Starred),
            "important" => Term::Is(State::Important),
            _ => Term::Text(word.to_ascii_lowercase()),
        },
        "has" => match folded.as_str() {
            "attachment" => Term::Has(Attribute::Attachment),
            "userlabels" => Term::Has(Attribute::UserLabels),
            "nouserlabels" => Term::Has(Attribute::NoUserLabels),
            _ => Term::Text(word.to_ascii_lowercase()),
        },
        "larger" | "size" => byte_size(&folded).map_or_else(fallback(word), Term::Larger),
        "smaller" => byte_size(&folded).map_or_else(fallback(word), Term::Smaller),
        // Gmail's documented tab categories. `category:primary` maps to
        // `CATEGORY_PERSONAL` — Gmail has no `CATEGORY_PRIMARY` label at all,
        // and `reservations`/`purchases` are documented `q` values with **no**
        // label id, so `q` is the only way to ask for them.
        "category" => category_label(&folded).map_or_else(fallback(word), Term::Label),
        // EDGE: an operator Gmail knows and this does not (`deliveredto:`,
        // `header:`, `AROUND`, …), or an unknown one entirely. Free text, never
        // an error. It matches nothing, which is also what an unindexed operator
        // does — and which is indistinguishable from an empty inbox, exactly as
        // it is against the real API.
        _ => Term::Text(word.to_ascii_lowercase()),
    }
}

/// Turn a failed operator parse back into a free-text term.
fn fallback(word: &str) -> impl FnOnce(()) -> Term + '_ {
    move |()| Term::Text(word.to_ascii_lowercase())
}

/// `24h`, `30d`, `6m`, `1y` — and **nothing else**.
///
/// Google documents three units for `newer_than:`/`older_than:` — `d`, `m`, `y`
/// ([Gmail search operators][ops]) — but a live probe against a real mailbox
/// settles the rest, and the docs are incomplete in both directions:
///
/// | Unit | Status |
/// |---|---|
/// | `d`, `m`, `y` | documented **and** measured |
/// | `h` | **undocumented, and it works** — `newer_than:24h` returns exactly what `newer_than:1d` returns |
/// | `w` | measured: returns **nothing**. Not supported |
/// | bare number, no unit | measured: returns **nothing**. Not supported |
///
/// `w` and a bare number therefore fall back to free text and match nothing,
/// which is what the real API does with them. Accepting them would hand back a
/// plausible-looking window that Gmail never produces.
///
/// `m` is 30 days and `y` is 365. Whether Gmail means calendar months and years
/// is still unmeasured; this is a stated approximation, and it is why NADE's own
/// sync should compute its window and pass `after:`/`before:` in epoch seconds
/// rather than lean on `newer_than:` at all.
///
/// [ops]: https://support.google.com/mail/answer/7190
fn span_ms(value: &str) -> Result<i64, ()> {
    let (digits, unit) = value.split_at(value.len().saturating_sub(1));
    let multiplier = match unit {
        "h" => 3_600_000,
        "d" => DAY_MS,
        "m" => 30 * DAY_MS,
        "y" => 365 * DAY_MS,
        _ => return Err(()),
    };
    digits
        .parse::<i64>()
        .ok()
        .filter(|count| *count >= 0)
        .and_then(|count| count.checked_mul(multiplier))
        .ok_or(())
}

/// Gmail interprets a bare date as **midnight Pacific**, not midnight UTC.
///
/// Straight from the API's own filtering guide: *"All dates used in the search
/// query are interpreted as midnight on that date in the PST timezone. To
/// specify accurate dates for other timezones pass the value in seconds
/// instead."*
///
/// Modelling UTC here would put every date bound eight hours out, and a
/// 30-day-window sync that straddles a boundary would disagree with production
/// by a whole day's mail at the edge.
///
/// **Fixed UTC−8**, because that is what Google's word "PST" literally says.
/// Google never states whether it means the fixed offset or DST-aware
/// `America/Los_Angeles`, so between mid-March and early November there is a
/// documented-ambiguity window of one hour that no simulator can resolve. The
/// safe client behaviour — and the one NADE should adopt — is to pass epoch
/// seconds, which this also accepts and which has no ambiguity at all.
const PACIFIC_OFFSET_MS: i64 = 8 * 60 * 60 * 1000;

/// `YYYY/MM/DD`, `YYYY-MM-DD` (midnight Pacific), or epoch seconds (exact).
///
/// `MM/DD/YYYY` is *also* documented by Google, and is deliberately **not**
/// accepted: `03/04/2004` is genuinely ambiguous between the two documented
/// orders and Google publishes no disambiguation rule. Refusing it makes a
/// client that sends it get zero results here — loud and safe — rather than a
/// confidently wrong three months.
fn date_ms(value: &str) -> Result<i64, ()> {
    let parts: Vec<&str> = value.split(['/', '-']).collect();
    if parts.len() == 3 {
        let year: i32 = parts[0].parse().map_err(|_| ())?;
        // Only the unambiguous year-first order.
        if parts[0].len() != 4 {
            return Err(());
        }
        let month: u32 = parts[1].parse().map_err(|_| ())?;
        let day: u32 = parts[2].parse().map_err(|_| ())?;
        let date = chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or(())?;
        return date
            .and_hms_opt(0, 0, 0)
            .map(|at| at.and_utc().timestamp_millis() + PACIFIC_OFFSET_MS)
            .ok_or(());
    }
    // Epoch seconds: exact, timezone-free, and what Google tells you to use.
    value
        .parse::<i64>()
        .map_err(|_| ())
        .and_then(|seconds| seconds.checked_mul(1_000).ok_or(()))
}

/// Map a documented `category:` value onto the label it means.
fn category_label(value: &str) -> Result<String, ()> {
    match value {
        // There is no `CATEGORY_PRIMARY`. Google's docs call it the "Personal
        // tab" and the UI calls it "Primary".
        "primary" | "personal" => Ok("category_personal".to_owned()),
        "social" | "promotions" | "updates" | "forums" => Ok(format!("category_{value}")),
        // Documented `q` values with **no** label id at all, so they can never
        // match a label the mailbox holds. Left as free text by the caller.
        _ => Err(()),
    }
}

/// `10M`, `500K`, `1024`.
fn byte_size(value: &str) -> Result<u64, ()> {
    let (digits, multiplier) = match value.chars().last() {
        Some('k') => (&value[..value.len() - 1], 1_024),
        Some('m') => (&value[..value.len() - 1], 1_024 * 1_024),
        _ => (value, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(multiplier))
        .ok_or(())
}

// ---------------------------------------------------------------------------
// RFC 2047, for the search index only
// ---------------------------------------------------------------------------

/// Decode RFC 2047 encoded-words, for **search indexing only**.
///
/// The wire never sees this: `payload.headers[].value` keeps its `=?utf-8?B?…?=`
/// form, exactly as Gmail returns it, so a client's own decoder still has to
/// work. But Gmail's *index* obviously holds decoded text — searching
/// `subject:Café` finds a message whose subject was sent encoded — and the
/// simulator has to agree with that or every unicode search test is a lie.
///
/// Charsets other than UTF-8 are decoded as windows-1252, which is what
/// `docs/PARSER.md` establishes senders actually mean by `iso-8859-1`.
#[must_use]
pub fn decode_rfc2047(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("=?") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(decoded) = decode_one_word(after) else {
            out.push_str("=?");
            rest = after;
            continue;
        };
        out.push_str(&decoded.0);
        rest = decoded.1;
        // Adjacent encoded words are joined with no space, per RFC 2047 §6.2:
        // the whitespace between them is not part of the text.
        let trimmed = rest.trim_start();
        if trimmed.starts_with("=?") && rest.len() != trimmed.len() {
            rest = trimmed;
        }
    }
    out.push_str(rest);
    out
}

fn decode_one_word(after: &str) -> Option<(String, &str)> {
    let end = after.find("?=")?;
    let word = &after[..end];
    let tail = &after[end + 2..];
    let mut fields = word.splitn(3, '?');
    let charset = fields.next()?.to_ascii_lowercase();
    let encoding = fields.next()?.to_ascii_lowercase();
    let payload = fields.next()?;
    let bytes = match encoding.as_str() {
        "b" => crate::mime::decode_base64_lenient(payload.as_bytes()),
        "q" => crate::mime::decode_quoted_printable(payload.replace('_', " ").as_bytes()),
        _ => return None,
    };
    let text = if charset.starts_with("utf") {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        // windows-1252 for everything else. Good enough for an index, and
        // right far more often than latin-1 would be.
        bytes.iter().map(|byte| cp1252(*byte)).collect()
    };
    Some((text, tail))
}

/// The 27 windows-1252 code points that differ from latin-1.
fn cp1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}',
        '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}',
        '\u{9D}', '\u{017E}', '\u{0178}',
    ];
    if (0x80..0xA0).contains(&byte) {
        HIGH[usize::from(byte - 0x80)]
    } else {
        char::from(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_785_542_400_000;

    fn text() -> Searchable {
        Searchable {
            subject: "your july invoice".to_owned(),
            from: "billing <billing@stripe.com>".to_owned(),
            to: "me@example.com".to_owned(),
            body: "thanks for your business. total 42 usd.".to_owned(),
            filenames: vec!["invoice.pdf".to_owned()],
            has_attachment: true,
            rfc822_msgid: "abc@stripe.com".to_owned(),
            ..Searchable::default()
        }
    }

    fn facts(labels: &[String], age_days: i64) -> Facts<'_> {
        Facts {
            internal_date_ms: NOW - age_days * DAY_MS,
            label_ids: labels,
            size: 4_096,
            now_ms: NOW,
        }
    }

    fn names(id: &str) -> Option<String> {
        match id {
            "Label_4" => Some("Receipts".to_owned()),
            "Label_5" => Some("Work Stuff".to_owned()),
            _ => None,
        }
    }

    fn hit(query: &str, labels: &[&str], age_days: i64) -> bool {
        let labels: Vec<String> = labels.iter().map(|s| (*s).to_owned()).collect();
        Query::parse(query).matches(&text(), facts(&labels, age_days), &names)
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(Query::parse(""), Query::All);
        assert_eq!(Query::parse("   "), Query::All);
        assert!(hit("", &[], 900));
    }

    #[test]
    fn newer_than_is_the_thirty_day_window() {
        assert!(hit("newer_than:30d", &[], 29));
        assert!(!hit("newer_than:30d", &[], 31));
    }

    #[test]
    fn newer_than_excludes_the_exact_boundary() {
        // EDGE: `internalDate == now - 30d` is *not* newer than 30 days.
        let labels: Vec<String> = Vec::new();
        let exactly = Facts {
            internal_date_ms: NOW - 30 * DAY_MS,
            label_ids: &labels,
            size: 1,
            now_ms: NOW,
        };
        assert!(!Query::parse("newer_than:30d").matches(&text(), exactly, &names));
        let one_ms_inside = Facts {
            internal_date_ms: NOW - 30 * DAY_MS + 1,
            ..exactly
        };
        assert!(Query::parse("newer_than:30d").matches(&text(), one_ms_inside, &names));
    }

    #[test]
    fn only_the_three_documented_span_units_are_accepted() {
        // Google documents exactly `d`, `m`, `y`.
        assert_eq!(span_ms("30d"), Ok(30 * DAY_MS));
        assert_eq!(span_ms("6m"), Ok(180 * DAY_MS));
        assert_eq!(span_ms("1y"), Ok(365 * DAY_MS));

        // MEASURED: `h` is undocumented and works — `24h` == `1d`.
        assert_eq!(span_ms("24h"), Ok(DAY_MS));
        assert_eq!(span_ms("1h"), Ok(3_600_000));

        // MEASURED: `w` and a bare number return nothing from the real API.
        // Accepting them would hand back a window Gmail never produces.
        for unsupported in ["2w", "7", "30D", "30 d", ""] {
            assert!(
                span_ms(unsupported).is_err(),
                "{unsupported:?} is not a supported span"
            );
        }
        assert!(span_ms("abc").is_err());
        assert!(span_ms("-3d").is_err());
        assert!(span_ms("99999999999999999999d").is_err());
    }

    #[test]
    fn an_undocumented_span_unit_degrades_to_free_text_and_matches_nothing() {
        // The whole point: `newer_than:1w` must not quietly behave like a
        // seven-day window. It becomes a search term, finds nothing, and the
        // client notices.
        assert_eq!(
            Query::parse("newer_than:1w"),
            Query::Term(Term::Text("newer_than:1w".to_owned()))
        );
        assert!(!hit("newer_than:1w", &[], 1));
        assert!(
            hit("newer_than:7d", &[], 1),
            "the documented form still works"
        );
    }

    #[test]
    fn label_matches_system_ids_and_user_names() {
        assert!(hit("label:inbox", &["INBOX"], 1));
        assert!(hit("label:INBOX", &["INBOX"], 1));
        assert!(hit("label:receipts", &["Label_4"], 1), "by display name");
        assert!(hit("label:work-stuff", &["Label_5"], 1), "hyphen for space");
        assert!(!hit("label:inbox", &["SPAM"], 1));
        assert!(hit("in:inbox", &["INBOX"], 1), "in: is a synonym");
    }

    #[test]
    fn from_and_is_unread_combine_with_implicit_and() {
        assert!(hit("from:stripe is:unread", &["UNREAD"], 1));
        assert!(!hit("from:stripe is:unread", &[], 1), "not unread");
        assert!(
            !hit("from:paypal is:unread", &["UNREAD"], 1),
            "wrong sender"
        );
        assert!(hit("is:read", &["INBOX"], 1));
    }

    #[test]
    fn negation_works_on_terms_and_on_groups() {
        assert!(hit("-from:paypal", &[], 1));
        assert!(!hit("-from:stripe", &[], 1));
        assert!(!hit("-label:inbox", &["INBOX"], 1));
        assert!(hit("-(from:paypal OR label:spam)", &["INBOX"], 1));
    }

    #[test]
    fn or_works_both_ways_it_is_spelled() {
        assert!(hit("from:paypal OR from:stripe", &[], 1));
        assert!(hit("{from:paypal from:stripe}", &[], 1));
        assert!(!hit("from:paypal OR from:amazon", &[], 1));
    }

    #[test]
    fn the_boolean_or_is_case_sensitive_but_operator_names_are_not() {
        // MEASURED, and these are two different rules that coexist.

        // The boolean keyword: lowercase `or` is the literal word, which is why
        // lowercasing a user's query silently changes what it means.
        assert_eq!(
            Query::parse("a or b"),
            Query::And(vec![
                Query::Term(Term::Text("a".to_owned())),
                Query::Term(Term::Text("or".to_owned())),
                Query::Term(Term::Text("b".to_owned())),
            ])
        );

        // Operator names, by contrast, fold case — `IS:UNREAD` == `is:unread`.
        assert_eq!(Query::parse("IS:UNREAD"), Query::parse("is:unread"));
        assert!(hit("IS:UNREAD", &["UNREAD"], 1));
        assert_eq!(Query::parse("FROM:Stripe"), Query::parse("from:stripe"));
        assert_eq!(
            Query::parse("Newer_Than:30d"),
            Query::parse("newer_than:30d")
        );

        // MEASURED: `|` really is an OR alias, documented nowhere.
        assert_eq!(
            Query::parse("(from:paypal | from:stripe)"),
            Query::parse("(from:paypal OR from:stripe)")
        );
        assert!(hit("from:paypal | from:stripe", &[], 1));

        // `AND` is documented, and redundant.
        assert!(hit("from:stripe AND is:unread", &["UNREAD"], 1));
    }

    #[test]
    fn a_space_after_the_colon_still_applies_the_operator() {
        // MEASURED: `from: chase.com` returns exactly what `from:chase.com`
        // returns — Gmail skips the whitespace and still filters. An earlier
        // draft made this free text, which was stricter than Gmail and would
        // have taught a client to avoid a query that works fine.
        assert_eq!(Query::parse("from: stripe"), Query::parse("from:stripe"));
        assert!(hit("from: stripe", &[], 1));
        assert!(!hit("from: paypal", &[], 1), "it really is still a filter");
        assert_eq!(
            Query::parse("subject: invoice"),
            Query::parse("subject:invoice")
        );
    }

    #[test]
    fn an_operator_with_a_genuinely_empty_argument_is_a_no_op() {
        // MEASURED: `newer_than:3d from:` returns the full `newer_than:3d`
        // baseline, so a dangling operator filters nothing at all.
        assert_eq!(Query::parse("from:"), Query::All);
        assert!(hit("from:", &[], 900));
        // …and it must not swallow a following operator as its argument.
        assert!(hit("from: is:unread", &["UNREAD"], 1));
        assert!(!hit("from: is:unread", &[], 1));
    }

    #[test]
    fn category_maps_onto_the_label_gmail_actually_uses() {
        // There is no CATEGORY_PRIMARY; the label is CATEGORY_PERSONAL.
        assert!(hit("category:primary", &["CATEGORY_PERSONAL"], 1));
        assert!(!hit("category:primary", &["CATEGORY_PROMOTIONS"], 1));
        assert!(hit("category:promotions", &["CATEGORY_PROMOTIONS"], 1));
        assert!(hit("category:updates", &["CATEGORY_UPDATES"], 1));
        // `reservations` and `purchases` are documented `q` values with no label
        // id at all, so they can never match a label the mailbox holds.
        assert!(!hit("category:reservations", &["CATEGORY_UPDATES"], 1));
        assert_eq!(
            Query::parse("category:purchases"),
            Query::Term(Term::Text("category:purchases".to_owned()))
        );
    }

    #[test]
    fn quoted_phrases_stay_one_term() {
        assert!(hit("subject:\"july invoice\"", &[], 1));
        assert!(!hit("subject:\"august invoice\"", &[], 1));
        assert!(hit("\"thanks for your business\"", &[], 1));
    }

    #[test]
    fn bare_words_match_whole_words_only() {
        assert!(hit("invoice", &[], 1));
        assert!(hit("stripe.com", &[], 1));
        assert!(
            !hit("invoic", &[], 1),
            "a prefix must not match; Gmail indexes words"
        );
    }

    #[test]
    fn has_and_size_and_filename() {
        assert!(hit("has:attachment", &[], 1));
        assert!(hit("filename:invoice.pdf", &[], 1));
        assert!(hit("filename:pdf", &[], 1));
        assert!(hit("larger:1k", &[], 1));
        assert!(!hit("larger:1m", &[], 1));
        assert!(hit("smaller:1m", &[], 1));
        assert!(hit("has:nouserlabels", &["INBOX"], 1));
        assert!(hit("has:userlabels", &["Label_4"], 1));
    }

    #[test]
    fn rfc822msgid_is_exact_and_bracket_tolerant() {
        assert!(hit("rfc822msgid:abc@stripe.com", &[], 1));
        assert!(hit("rfc822msgid:<abc@stripe.com>", &[], 1));
        assert!(!hit("rfc822msgid:abc", &[], 1));
    }

    #[test]
    fn a_bare_date_means_midnight_pacific_not_midnight_utc() {
        // Google: "All dates used in the search query are interpreted as
        // midnight on that date in the PST timezone." Eight hours, and a
        // 30-day window that straddles the boundary disagrees by a day's mail.
        const UTC_MIDNIGHT: i64 = 1_785_542_400_000; // 2026-08-01T00:00:00Z
        assert_eq!(date_ms("2026/08/01"), Ok(UTC_MIDNIGHT + 8 * 3_600_000));
        assert_eq!(date_ms("2026-08-01"), Ok(UTC_MIDNIGHT + 8 * 3_600_000));

        // Epoch seconds are exact and timezone-free — what Google tells you to
        // send, and the only form with no ambiguity at all.
        assert_eq!(date_ms("1785542400"), Ok(UTC_MIDNIGHT));

        // `MM/DD/YYYY` is also documented by Google and is refused here on
        // purpose: `03/04/2004` cannot be disambiguated and Google publishes no
        // rule. Zero results is loud; three wrong months is not.
        assert!(date_ms("08/01/2026").is_err());
        assert!(date_ms("03/04/2004").is_err());

        assert!(date_ms("2026/13/01").is_err());
        assert!(date_ms("nope").is_err());
    }

    #[test]
    fn a_date_range_is_half_open() {
        // Forced by Google's own worked example: `after:2014/01/01
        // before:2014/02/01` "retrieves all messages sent in January 2014", so
        // `after` includes its day and `before` excludes its own.
        let labels: Vec<String> = Vec::new();
        let boundary = date_ms("2026/08/01").unwrap();
        let at = |ms: i64| Facts {
            internal_date_ms: ms,
            label_ids: &labels,
            size: 1,
            now_ms: NOW,
        };
        let after = Query::parse("after:2026/08/01");
        assert!(after.matches(&text(), at(boundary), &names), "inclusive");
        assert!(!after.matches(&text(), at(boundary - 1), &names));

        let before = Query::parse("before:2026/08/01");
        assert!(!before.matches(&text(), at(boundary), &names), "exclusive");
        assert!(before.matches(&text(), at(boundary - 1), &names));
    }

    #[test]
    fn an_unknown_operator_is_free_text_not_an_error() {
        // `deliveredto:` is real Gmail syntax this simulator does not index;
        // `wibble:` is not syntax at all. Neither may be an error.
        assert!(!hit("deliveredto:me@example.com", &[], 1));
        assert!(!hit("wibble:wobble", &[], 1));
        assert_eq!(
            Query::parse("wibble:wobble"),
            Query::Term(Term::Text("wibble:wobble".to_owned()))
        );
    }

    #[test]
    fn malformed_queries_terminate_instead_of_looping() {
        // Each of these used to be an infinite loop in a draft of the parser.
        for query in [
            "-",
            "--",
            "(",
            ")",
            "()",
            "{",
            "}",
            "{}",
            "OR",
            "OR OR",
            "a OR",
            "((((",
            "))))",
            "-()",
            "\"",
            "\"unclosed",
            "a -",
            "{a",
            "a}",
        ] {
            let parsed = Query::parse(query);
            let labels: Vec<String> = Vec::new();
            let _ = parsed.matches(&text(), facts(&labels, 1), &names);
        }
    }

    #[test]
    fn rfc2047_decodes_for_the_index() {
        assert_eq!(decode_rfc2047("=?UTF-8?B?Q2Fmw6k=?="), "Café");
        assert_eq!(decode_rfc2047("=?utf-8?Q?Caf=C3=A9?="), "Café");
        assert_eq!(decode_rfc2047("=?utf-8?Q?a_b?="), "a b");
        assert_eq!(decode_rfc2047("plain"), "plain");
        assert_eq!(decode_rfc2047("a =?UTF-8?B?Yg==?= c"), "a b c");
        // Adjacent encoded words join with no space (RFC 2047 §6.2).
        assert_eq!(decode_rfc2047("=?UTF-8?B?SGVs?= =?UTF-8?B?bG8=?="), "Hello");
        // windows-1252, not latin-1: 0x92 is a right single quote.
        assert_eq!(decode_rfc2047("=?iso-8859-1?Q?Don=92t?="), "Don\u{2019}t");
    }

    #[test]
    fn rfc2047_survives_junk_without_panicking() {
        assert_eq!(decode_rfc2047("=?"), "=?");
        assert_eq!(decode_rfc2047("=?utf-8?"), "=?utf-8?");
        assert_eq!(decode_rfc2047("=?utf-8?x?zz?="), "=?utf-8?x?zz?=");
        assert_eq!(decode_rfc2047("=?=?=?="), "=?=?=?=");
    }
}
