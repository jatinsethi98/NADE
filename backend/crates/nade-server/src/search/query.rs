//! Validate and normalise a Gmail search query **before** sending it.
//!
//! `docs/SEARCH.md` is the specification. The one fact the whole module exists
//! for: **Gmail never answers a bad `q` with a 400.** It answers `200` with an
//! empty `messages` array, which is byte-identical to a query that genuinely
//! matched nothing. So an unknown operator, a label id where a label name
//! belongs, or `newer_than:1w` all fail *silently*, and neither a person nor a
//! model can tell the difference between "you asked wrong" and "you have no
//! such mail".
//!
//! Every rule below was measured against the live API rather than read out of
//! Google's reference, and the reference is wrong about at least one of them
//! (`after:` is midnight **UTC**, not Pacific).
//!
//! The rules, and the reason each is here:
//!
//! | Rule | What Gmail does | What we do |
//! |---|---|---|
//! | unknown operator | matches nothing | reject, naming the operator |
//! | `label:<id>` | matches nothing | translate the id to its name, or reject |
//! | `newer_than:1w`, `newer_than:7` | matches nothing | reject, offering `7d` |
//! | `after:yesterday` | matches nothing | reject, giving the format |
//! | `is:`/`has:`/`category:` typo | matches nothing | reject, listing the values |
//! | `from:` with no argument | matches **everything** | reject |
//! | lowercase `or` | a literal word, silently ANDed | reject, offering `OR` |
//!
//! Two behaviours are deliberately **not** rejected, because they are things a
//! caller may legitimately mean:
//!
//! * `from: alice` - a space after the colon is tolerated by Gmail and filters
//!   normally, so it is *normalised* to `from:alice` rather than refused.
//! * `in:anywhere` / `in:trash` / `in:spam` - these widen the search past Trash
//!   and Spam, and `includeSpamTrash=false` does not stop them (it gates
//!   neither `q` nor `labelIds`). A caller that types one means it. Keeping
//!   Trash out is a property of *not writing an `in:`*, which is exactly why
//!   [`crate::sync::SyncOptions::from_config`] builds its query from a fixed
//!   template.

use std::collections::BTreeMap;

use crate::error::ApiError;

/// `API.md` §0: `q` is capped at 512 characters.
pub const MAX_QUERY_CHARS: usize = 512;

/// Every operator Gmail's search language actually has. Names fold case
/// (`IS:UNREAD` == `is:unread`), so the lookup is done lowercased.
///
/// Anything not on this list matches nothing, silently - which is the whole
/// reason the list is exhaustive rather than a convenient subset.
const OPERATORS: &[&str] = &[
    "after",
    "bcc",
    "before",
    "category",
    "cc",
    "deliveredto",
    "filename",
    "from",
    "has",
    "in",
    "is",
    "label",
    "larger",
    "list",
    "newer",
    "newer_than",
    "older",
    "older_than",
    "rfc822msgid",
    "size",
    "smaller",
    "subject",
    "to",
];

/// `is:` takes a closed vocabulary; anything else matches nothing.
const IS_VALUES: &[&str] = &[
    "important",
    "muted",
    "read",
    "snoozed",
    "starred",
    "unimportant",
    "unread",
    "unstarred",
];

/// `has:` takes a closed vocabulary, plus the star/marker colours.
const HAS_VALUES: &[&str] = &[
    "attachment",
    "document",
    "drive",
    "nouserlabels",
    "presentation",
    "spreadsheet",
    "userlabels",
    "youtube",
];

/// The suffixes of Gmail's coloured star and marker names (`has:blue-info`).
const HAS_MARKER_SUFFIXES: &[&str] = &["star", "bang", "check", "info", "question", "guillemet"];

/// `category:` takes exactly Gmail's tabs.
const CATEGORY_VALUES: &[&str] = &[
    "forums",
    "primary",
    "promotions",
    "purchases",
    "reservations",
    "social",
    "updates",
];

/// The units `newer_than:` / `older_than:` accept. **Not `w`**, and not a bare
/// number: both match nothing.
const AGE_UNITS: [char; 4] = ['h', 'd', 'm', 'y'];

/// The scope-widening `in:` values, which reach past Trash and Spam regardless
/// of `includeSpamTrash=false`.
const SCOPE_VALUES: &[&str] = &["anywhere", "spam", "trash"];

// ------------------------------------------------------------- rejection --

/// A query we refused, and **why** - in words a person can act on and a model
/// can correct itself from.
///
/// The message is the entire point. An agent handed an empty result list cannot
/// tell a mistake from an empty mailbox; an agent handed "unknown operator
/// `labels:`; Gmail has `label:`" fixes its own query and asks again.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Rejection {
    pub message: String,
}

impl Rejection {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<Rejection> for ApiError {
    fn from(rejection: Rejection) -> Self {
        Self::bad_request(rejection.message)
    }
}

/// A query that has been checked and rewritten, ready for `users.messages.list`.
///
/// Constructed only by [`validate`], so "this string was validated" is a type
/// rather than a convention - there is no way to hand an unchecked string to
/// the Gmail client by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedQuery {
    text: String,
    /// True when the query reaches past Trash and Spam. Recorded rather than
    /// refused: `includeSpamTrash=false` does not gate `q`, so this is the only
    /// place that knows.
    widens_scope: bool,
}

impl NormalisedQuery {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// True when an `in:anywhere` / `in:trash` / `in:spam` reaches past the
    /// scope `includeSpamTrash=false` would otherwise imply.
    #[must_use]
    pub const fn widens_scope(&self) -> bool {
        self.widens_scope
    }
}

impl std::fmt::Display for NormalisedQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

// ----------------------------------------------------------------- labels --

/// The account's labels, so `label:Label_25` can be *translated* rather than
/// only complained about. The id is exactly what a program has to hand, so this
/// is the recoverable case and the one worth spending a lookup on.
#[derive(Debug, Clone, Default)]
pub struct LabelIndex {
    /// `label_id` → display name.
    by_id: BTreeMap<String, String>,
}

impl LabelIndex {
    #[must_use]
    pub fn new<I, S>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (S, S)>,
        S: Into<String>,
    {
        Self {
            by_id: pairs
                .into_iter()
                .map(|(id, name)| (id.into(), name.into()))
                .collect(),
        }
    }

    /// No labels known - the pre-sync state, and what the validator gets when
    /// no Gmail account is connected at all.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    fn name_for(&self, id: &str) -> Option<&str> {
        self.by_id.get(id).map(String::as_str)
    }

    /// A name to put in the "did you mean" of a rejection.
    ///
    /// A **user** label, because that is what somebody handing us an id was
    /// reaching for. A system label's name is its id (`INBOX`, `SENT`,
    /// `CATEGORY_PROMOTIONS`), so "the name differs from the id" is exactly the
    /// user-label test without needing the `type` column here. Deterministic -
    /// the map is ordered - so the message is the same on every run.
    fn example(&self) -> Option<&str> {
        self.by_id
            .iter()
            .find(|(id, name)| *id != *name && !name.starts_with("[Gmail]"))
            .map(|(_, name)| name.as_str())
    }
}

// --------------------------------------------------------------- validate --

/// Check a raw `q`, and return the query to send to Gmail.
///
/// # Errors
/// [`Rejection`] for anything Gmail would swallow into an empty `200`, with the
/// explanation the caller shows.
pub fn validate(raw: &str, labels: &LabelIndex) -> Result<NormalisedQuery, Rejection> {
    // EDGE (empty input): an empty or whitespace-only `q` is a refusal, not
    // "every message you own".
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Rejection::new("Type something to search for."));
    }
    if trimmed.chars().count() > MAX_QUERY_CHARS {
        return Err(Rejection::new(format!(
            "That search is too long. The limit is {MAX_QUERY_CHARS} characters."
        )));
    }

    let words = split_words(trimmed);
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut widens_scope = false;
    let mut index = 0usize;

    while index < words.len() {
        let word = &words[index];
        index += 1;

        let Some(core) = word.core() else {
            // Pure punctuation - a lone `(`, `)`, `{` or `}`.
            out.push(word.raw.clone());
            continue;
        };

        // A quoted phrase is a literal, whatever is inside it. `"is:unread"`
        // searches for the text, and that is a legitimate thing to want.
        if core.starts_with('"') {
            out.push(word.raw.clone());
            continue;
        }

        let Some((name, value)) = split_operator(core) else {
            check_bare_term(core)?;
            out.push(word.raw.clone());
            continue;
        };

        let lowered = name.to_ascii_lowercase();
        if !OPERATORS.contains(&lowered.as_str()) {
            return Err(unknown_operator(&name, core));
        }

        // A space after the colon is tolerated by Gmail (`from: alice` filters
        // exactly like `from:alice`), so the next word is the argument. An
        // operator with a genuinely **empty** argument is a no-op that matches
        // everything, which is the opposite of what the caller wanted.
        let value = if value.is_empty() {
            match words.get(index) {
                // The **raw** next word, not its peeled core: `from: -bob` is
                // `from:-bob`, and taking the core would quietly drop the
                // negation and search for exactly what the caller excluded.
                // A word carrying its own punctuation is refused rather than
                // glued on, so `(from: )` stays an empty argument.
                Some(next) if is_argument(next) => {
                    index += 1;
                    next.raw.clone()
                }
                _ => {
                    return Err(Rejection::new(format!(
                        "`{lowered}:` has no argument. Gmail treats an operator with an empty \
                         argument as a match-everything no-op rather than an error, so it is \
                         refused here. Write `{lowered}:something`."
                    )))
                }
            }
        } else {
            value.to_owned()
        };

        let value = check_value(&lowered, &value, labels, &mut widens_scope)?;
        out.push(format!("{}{lowered}:{value}{}", word.prefix, word.suffix));
    }

    let text = out.join(" ");
    // Every word was punctuation: `(` `)` and nothing else is not a search.
    if text.trim().chars().all(|c| "(){}+-".contains(c)) {
        return Err(Rejection::new("Type something to search for."));
    }

    Ok(NormalisedQuery { text, widens_scope })
}

fn unknown_operator(name: &str, core: &str) -> Rejection {
    let lowered = name.to_ascii_lowercase();
    let suggestion = OPERATORS
        .iter()
        .find(|known| is_near(&lowered, known))
        .map_or_else(String::new, |known| format!(" Did you mean `{known}:`?"));
    Rejection::new(format!(
        "Unknown operator `{lowered}:`. Gmail answers an unknown operator with an empty result \
         rather than an error, so `{core}` would have looked like \"no such mail\".{suggestion} \
         To search for the text itself, quote it: `\"{core}\"`."
    ))
}

/// One edit apart: a substitution, an insertion, a deletion, or a **swap of
/// two neighbours**. The swap earns its place - `form:` for `from:` is the
/// single most common way to mistype the single most common operator, and it
/// is two substitutions rather than one, so a plain Levenshtein-1 check misses
/// exactly the case worth catching.
///
/// Enough to say "did you mean"; not a spell checker, and deliberately not.
fn is_near(candidate: &str, known: &str) -> bool {
    if candidate == known {
        return true;
    }
    let short: Vec<char>;
    let long: Vec<char>;
    {
        let (a, b): (Vec<char>, Vec<char>) = (candidate.chars().collect(), known.chars().collect());
        if a.len() <= b.len() {
            short = a;
            long = b;
        } else {
            short = b;
            long = a;
        }
    }

    match long.len() - short.len() {
        0 => {
            let differing: Vec<usize> = (0..long.len()).filter(|i| short[*i] != long[*i]).collect();
            match differing.as_slice() {
                // One substitution.
                [_] => true,
                // Two neighbours swapped.
                [first, second] => {
                    *second == first + 1
                        && short[*first] == long[*second]
                        && short[*second] == long[*first]
                }
                _ => false,
            }
        }
        // `long` with one character deleted equals `short`.
        1 => (0..long.len()).any(|skip| {
            long.iter()
                .enumerate()
                .filter(|(index, _)| *index != skip)
                .map(|(_, c)| *c)
                .eq(short.iter().copied())
        }),
        _ => false,
    }
}

/// A bare word. The only trap here is a boolean the caller wrote in the wrong
/// case.
fn check_bare_term(core: &str) -> Result<(), Rejection> {
    // Operator *names* fold case; the boolean `OR` does not. Lowercase `or` is
    // matched as the literal three-letter word and silently ANDed with
    // everything either side of it, so `invoice or receipt` asks for messages
    // containing all three words and usually finds none.
    if core.eq_ignore_ascii_case("or") && core != "OR" {
        return Err(Rejection::new(format!(
            "`{core}` is not Gmail's boolean OR - only uppercase `OR` is, and `{core}` is matched \
             as the literal word instead, narrowing the search to nothing. Write `OR`, or quote \
             `\"{core}\"` to search for the word."
        )));
    }
    Ok(())
}

/// Could this word be the argument of the operator before it?
///
/// A second operator is not - `from: to:bob` means `from:` was left empty - and
/// neither is anything carrying **grouping** punctuation, because `from: (a b)`
/// opens a group rather than naming a sender.
///
/// A leading `-` or `+` is *not* grouping punctuation and stays: `from: -bob`
/// is `from:-bob`, and dropping the `-` would search for precisely what the
/// caller asked to exclude.
fn is_argument(next: &Word) -> bool {
    match next.core() {
        Some(core) => !next.raw.contains(['(', ')', '{', '}']) && split_operator(core).is_none(),
        None => false,
    }
}

/// Per-operator value rules. Returns the value to send, which may differ from
/// the one supplied - that is how a label id becomes a label name.
fn check_value(
    operator: &str,
    value: &str,
    labels: &LabelIndex,
    widens_scope: &mut bool,
) -> Result<String, Rejection> {
    match operator {
        "newer_than" | "older_than" => check_age(operator, value).map(|()| value.to_owned()),
        "after" | "before" | "newer" | "older" => {
            check_date(operator, value).map(|()| value.to_owned())
        }
        "label" => check_label(operator, value, labels),
        "in" => {
            if SCOPE_VALUES.contains(&value.to_ascii_lowercase().as_str()) {
                *widens_scope = true;
                return Ok(value.to_owned());
            }
            check_label(operator, value, labels)
        }
        // The closed vocabularies are emitted lowercased. Gmail folds them
        // anyway, and a canonical query is one a per-account result cache could
        // key on without `is:UNREAD` and `is:unread` being two entries.
        "is" => check_vocabulary(operator, value, IS_VALUES).map(|()| value.to_ascii_lowercase()),
        "category" => {
            check_vocabulary(operator, value, CATEGORY_VALUES).map(|()| value.to_ascii_lowercase())
        }
        "has" => check_has(value).map(|()| value.to_ascii_lowercase()),
        "size" | "larger" | "smaller" => check_size(operator, value).map(|()| value.to_owned()),
        _ => Ok(value.to_owned()),
    }
}

/// `newer_than:` / `older_than:` take `<number><unit>`, unit ∈ h d m y.
///
/// `w` is the one people reach for and the one Gmail does not have, and a bare
/// number is what a program produces. Both match nothing.
fn check_age(operator: &str, value: &str) -> Result<(), Rejection> {
    let refuse = |detail: String| {
        Err(Rejection::new(format!(
            "`{operator}:{value}` is not a duration Gmail understands. {detail} Gmail matches \
             nothing rather than reporting the mistake, so it is refused here."
        )))
    };

    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    let unit: String = value.chars().skip(digits.chars().count()).collect();

    if digits.is_empty() {
        return refuse(format!(
            "It must be a number followed by `h`, `d`, `m` or `y` - for example `{operator}:7d`."
        ));
    }
    if unit.is_empty() {
        return refuse(format!(
            "A bare number has no unit; write `{operator}:{digits}d` for days, or `h`, `m`, `y`."
        ));
    }
    if unit.eq_ignore_ascii_case("w") {
        // The measured one: weeks are simply not a unit, so we do the
        // arithmetic in the message rather than leaving it as an exercise.
        let days = digits.parse::<u64>().unwrap_or(1).saturating_mul(7);
        return refuse(format!(
            "Weeks are not a unit Gmail supports; use `{operator}:{days}d`."
        ));
    }
    let mut unit_chars = unit.chars();
    match (unit_chars.next(), unit_chars.next()) {
        (Some(single), None) if AGE_UNITS.contains(&single.to_ascii_lowercase()) => Ok(()),
        _ => refuse(format!(
            "`{unit}` is not a unit; the units are `h` (hours), `d` (days), `m` (months) and \
             `y` (years)."
        )),
    }
}

/// `after:` / `before:` / `newer:` / `older:` take `YYYY/MM/DD` or epoch
/// seconds.
///
/// The semantics are worth knowing even when the value is legal:
/// `after:2026/08/10` is midnight **UTC**, despite Google's documentation
/// saying Pacific - measured. `newer_than:1d`, by contrast, is a rolling
/// instant: exactly `after:<now - 24h>`.
fn check_date(operator: &str, value: &str) -> Result<(), Rejection> {
    // Epoch seconds. Gmail accepts these and they are unambiguous.
    if value.len() >= 9 && value.chars().all(|c| c.is_ascii_digit()) {
        return Ok(());
    }

    let parts: Vec<&str> = value.split(['/', '-']).collect();
    let shaped = parts.len() == 3
        && parts[0].len() == 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    if !shaped {
        return Err(Rejection::new(format!(
            "`{operator}:{value}` is not a date Gmail understands. Use `{operator}:YYYY/MM/DD` \
             (midnight UTC) or epoch seconds. Gmail matches nothing rather than reporting the \
             mistake, so it is refused here."
        )));
    }

    let month: u32 = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(Rejection::new(format!(
            "`{operator}:{value}` is not a real date. The order is `YYYY/MM/DD`."
        )));
    }
    Ok(())
}

/// `label:` and `in:` take a label **name**. Handed an id - which is exactly
/// what a program has to hand - Gmail matches nothing.
fn check_label(operator: &str, value: &str, labels: &LabelIndex) -> Result<String, Rejection> {
    let bare = value.trim_matches('"');
    if bare.is_empty() {
        return Err(Rejection::new(format!(
            "`{operator}:` has no argument. Write `{operator}:` followed by the label's name."
        )));
    }

    // The recoverable case: we know this id, so translate rather than complain.
    if let Some(name) = labels.name_for(bare) {
        if name != bare {
            return Ok(quote_if_needed(name));
        }
        return Ok(value.to_owned());
    }

    // An id we have never seen. `Label_<anything>` is Gmail's own user-label id
    // shape and is never a label a person named.
    if bare.starts_with("Label_") {
        let hint = labels.example().map_or_else(
            || " Use the name shown in the mailbox list.".to_owned(),
            |example| format!(" Did you mean `{operator}:{}`?", quote_if_needed(example)),
        );
        return Err(Rejection::new(format!(
            "`{operator}:{bare}` is a label id, and `{operator}:` takes the label's **name**. \
             Gmail matches nothing for an id rather than reporting the mistake.{hint}"
        )));
    }

    Ok(value.to_owned())
}

fn check_vocabulary(operator: &str, value: &str, allowed: &[&str]) -> Result<(), Rejection> {
    if allowed.contains(&value.to_ascii_lowercase().as_str()) {
        return Ok(());
    }
    Err(Rejection::new(format!(
        "`{operator}:{value}` is not one Gmail knows. The values are {}. Gmail matches nothing \
         for an unknown one rather than reporting the mistake, so it is refused here.",
        list(allowed)
    )))
}

fn check_has(value: &str) -> Result<(), Rejection> {
    let lowered = value.to_ascii_lowercase();
    if HAS_VALUES.contains(&lowered.as_str()) {
        return Ok(());
    }
    // The coloured stars and markers: `has:blue-info`, `has:yellow-bang`.
    if let Some((colour, marker)) = lowered.split_once('-') {
        if !colour.is_empty() && HAS_MARKER_SUFFIXES.contains(&marker) {
            return Ok(());
        }
    }
    Err(Rejection::new(format!(
        "`has:{value}` is not one Gmail knows. The values are {}, plus the coloured stars and \
         markers such as `has:yellow-star`. Gmail matches nothing for an unknown one rather than \
         reporting the mistake, so it is refused here.",
        list(HAS_VALUES)
    )))
}

fn check_size(operator: &str, value: &str) -> Result<(), Rejection> {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    let unit: String = value.chars().skip(digits.chars().count()).collect();
    let unit_ok = unit.is_empty() || matches!(unit.to_ascii_lowercase().as_str(), "k" | "m");
    if digits.is_empty() || !unit_ok {
        return Err(Rejection::new(format!(
            "`{operator}:{value}` is not a size. Write a number of bytes, optionally with `k` or \
             `m` - for example `{operator}:10m`."
        )));
    }
    Ok(())
}

fn list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A label name with a space in it needs quoting, or Gmail reads the second
/// word as a separate term.
fn quote_if_needed(name: &str) -> String {
    if name.chars().any(char::is_whitespace) {
        format!("\"{}\"", name.replace('"', ""))
    } else {
        name.to_owned()
    }
}

// -------------------------------------------------------------- tokenising --

/// One whitespace-separated word, with its grouping punctuation peeled off so
/// the middle can be classified and rewritten and then put back exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    raw: String,
    prefix: String,
    body: String,
    suffix: String,
}

impl Word {
    /// The part that carries meaning, or `None` when the word was nothing but
    /// punctuation.
    fn core(&self) -> Option<&str> {
        (!self.body.is_empty()).then_some(self.body.as_str())
    }
}

/// Split on whitespace, but keep a double-quoted span together: `subject:"two
/// words"` is one word, and so is `"two words"`.
///
/// An unterminated quote swallows the rest of the query, which is what Gmail
/// does with it too.
fn split_words(text: &str) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;

    for character in text.chars() {
        if character == '"' {
            quoted = !quoted;
            current.push(character);
            continue;
        }
        if character.is_whitespace() && !quoted {
            if !current.is_empty() {
                words.push(peel(std::mem::take(&mut current)));
            }
            continue;
        }
        current.push(character);
    }
    if !current.is_empty() {
        words.push(peel(current));
    }
    words
}

/// Peel the leading `-`/`+`/`(`/`{` and trailing `)`/`}` off a word.
fn peel(raw: String) -> Word {
    let leading = raw
        .chars()
        .take_while(|c| matches!(c, '-' | '+' | '(' | '{'))
        .count();
    let head: String = raw.chars().take(leading).collect();
    let rest: String = raw.chars().skip(leading).collect();

    let trailing = rest
        .chars()
        .rev()
        .take_while(|c| matches!(c, ')' | '}'))
        .count();
    let keep = rest.chars().count() - trailing;
    let body: String = rest.chars().take(keep).collect();
    let tail: String = rest.chars().skip(keep).collect();

    Word {
        raw,
        prefix: head,
        body,
        suffix: tail,
    }
}

/// `name:value`, or `None` when the word is a plain term.
///
/// The name must look like an operator name: an ASCII letter first, then
/// letters, digits or `_`. That keeps `12:30` and `a.b:c` out of the operator
/// path entirely - no Gmail operator starts with a digit - while
/// `https://example.com` still lands in the "unknown operator" message with a
/// "quote it" way out, rather than being sent to Gmail to match nothing.
fn split_operator(core: &str) -> Option<(String, String)> {
    let (name, value) = core.split_once(':')?;
    if !name.starts_with(|c: char| c.is_ascii_alphabetic())
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((name.to_owned(), value.to_owned()))
}

#[cfg(test)]
mod tests;
