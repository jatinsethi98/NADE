# Mail parsing — decided, measured, and already proven

P2 implements `backend/crates/nade-server/src/mail/parse.rs`. Everything below
was settled before that phase started, by building a throwaway probe and
running it against both the conformance corpus (`backend/testdata/mime`, 26
cases) and 60 real messages from the live account. **The probe reached 26/26 and
60/60.** This document is the recipe it validated, so P2 implements a known-good
design rather than rediscovering these three traps.

## The stack

| Concern | Choice | Why |
|---|---|---|
| MIME parsing | **`mail-parser` 0.11** | Handles RFC 2047 in all its forms, quoted-printable, base64, folded headers, nested multipart, RFC-2047-encoded attachment filenames, group addresses, missing and malformed dates — verified, not assumed. Actively maintained. |
| HTML → text | **our own, on `lol_html` 3.0** | See below. `html2text` was measured and rejected. |
| Charset | `mail-parser`'s own, plus a header pre-pass | See trap 2. |

`html2text` is **not** a dependency. Do not add it.

## Why we write our own HTML→text

47% of real messages in this account have **no `text/plain` part at all**, so
this conversion is not a fallback — it produces the list snippet, the search
index, and the text every agent reads under a 50k-token budget. Measured over
the 28 html-only messages in the live sample:

| Extractor | Output chars | Words fused across block boundaries | Junk chars (box-drawing, zero-width) |
|---|---|---|---|
| `mail-parser`'s built-in `body_text()` | 41,193 | 112 | 1,002 |
| `html2text` (trivial decorator, its cleanest mode) | 307,500 | 55 | 64,895 |
| **ours** | **47,577** | **46** | **0** |

`html2text` renders for a terminal: it draws tables in box-drawing characters
and pads to a column width. Marketing email *is* tables, so 21% of its output
is border glyphs even in its plainest mode, and it emits 6.5× the text. That is
poison for a tsvector and for a token budget.

`mail-parser`'s built-in extraction is clean and compact but runs words
together across block boundaries — it produces `Sign in to Claude.aiClick the
button below`, which becomes the token `Claude.aiClick` in the search index.

Ours keeps the compactness, adds proper block separation and `alt` text, and
strips the zero-width/non-breaking padding that marketing mail uses for preview
text.

### The recipe (validated)

Two passes, because one does not work:

```rust
// Pass 1 — drop everything that is not content, and mark block boundaries in
// the HTML itself.
let settings = Settings::new()
    .append_element_content_handler(element!(
        "script, style, head, noscript, template, svg, iframe, object",
        |el| { el.remove(); Ok(()) }))
    .append_element_content_handler(element!(
        "p, div, br, tr, li, h1, h2, h3, h4, h5, h6, table, thead, tbody, \
         blockquote, section, article, header, footer, hr, td, th, ul, ol, pre",
        |el| { el.before("\u{0001}", ContentType::Text); Ok(()) }))
    .append_element_content_handler(element!("img", |el| {
        let alt = el.get_attribute("alt").unwrap_or_default().trim().to_string();
        if alt.is_empty() { el.remove(); } else { el.replace(&format!(" {alt} "), ContentType::Text); }
        Ok(())
    }));

// Pass 2 — collect text only. Entities are decoded here.
// Then replace \u{0001} with \n and normalise.
```

**Why two passes.** `Element::remove()` drops an element's inner content from
the *output*, but a `text!()` handler in the same pass still receives those
chunks — `TextChunk::removed()` describes the chunk itself, not its ancestors.
A single-pass version silently pours the contents of every `<style>` block into
`body_text`. That bug was found by looking at the output; it does not announce
itself.

**Normalisation**, in this order: map `\u{00A0} \u{200B} \u{200C} \u{200D}
\u{FEFF} \u{034F}` to spaces (marketing mail pads preview text with hundreds of
them), collapse whitespace runs per line, drop runs of blank lines to at most
one, trim.

**Link targets are dropped, link text is kept.** URLs are noise for both the
index and the model, and `body_html` still holds the real links for the
"View original" view.

## Trap 1 — `body_html` is never null if you ask `mail-parser` for it

`msg.body_html(0)` **synthesises HTML from the plain-text part** when no
`text/html` part exists. Use it naively and `body_html` is non-null for every
message, so the iOS "View original" affordance appears on plain-text mail.

`API.md` §2 says `body_html` is null exactly when there is no `text/html` part.
Detect it properly:

```rust
let has_real_html = msg.html_body.iter()
    .filter_map(|id| msg.part(*id))
    .any(|p| p.content_type().and_then(|c| c.subtype())
              .map(|s| s.eq_ignore_ascii_case("html")).unwrap_or(false));
```

The same mirror applies to `body_text(0)`, which synthesises text from HTML.
That one is convenient, but we override it with our own extractor for the
quality reasons above.

## Trap 2 — 8-bit bytes in headers are destroyed before you can see them

Headers are supposed to be ASCII. Senders put raw 8-bit bytes there anyway.
`mail-parser` reads the header block as UTF-8 and replaces every invalid byte
with `U+FFFD`, so the original byte is gone and nothing downstream can recover
it: `Don’t miss out` arrives as `Don�t miss out`.

Transcode the **header block only**, before parsing:

```rust
fn sanitize_headers(raw: &[u8]) -> Cow<[u8]> {
    let split = find_header_end(raw);          // first \r\n\r\n OR \n\n, whichever comes first
    let (head, body) = raw.split_at(split);
    if std::str::from_utf8(head).is_ok() { return Cow::Borrowed(raw); }   // the common case
    // windows-1252 for every byte ≥ 0x80, then append the body untouched
}
```

- **windows-1252, not latin-1.** Senders that declare `iso-8859-1`
  overwhelmingly mean cp1252, and 11 of 60 real messages here declare
  `iso-8859-1`. Decoding `0x92` as latin-1 gives a C1 control character; as
  cp1252 it gives the right single quote. Every mail client does this.
- **Header block only.** Parts carry their own charsets and transfer encodings;
  transcoding the body would corrupt base64 and quoted-printable payloads.
- **Only when the block is not valid UTF-8.** Valid UTF-8 always wins, which is
  what makes raw-UTF-8 headers (also common) keep working.
- `find_header_end` must accept `\n\n` as well as `\r\n\r\n`. Gmail's
  `format=raw` is not guaranteed pure CRLF, and a CRLF-only split loses the
  header/body boundary entirely.

## Trap 3 — duplicate `Subject` takes the last one

`msg.subject()` returns the **last** `Subject` header when a message carries
more than one. Every mail client shows the first, and a second `Subject` is a
header-injection trick to make one client disagree with another about what the
user is reading. Take the first, deliberately:

```rust
msg.headers().iter()
   .find(|h| h.name().eq_ignore_ascii_case("subject"))
   .and_then(|h| h.value().as_text())
```

## What P2 must also do

- **Never panic.** A parse failure writes a metadata-only row plus an
  `audit_log` entry and the sync continues. Case 22 (malformed base64) and case
  14 (unparseable date) are the tests for this.
- **`body_text` is never null.** Empty is legal (case 19); null is not.
- **Fall back to Gmail's `internalDate`** when the `Date` header is missing or
  unparseable — the sync layer always has it.
- **Rewrite `cid:` references** in `body_html` to
  `/v1/messages/{gmail_id}/attachments/{att_id}` before storing. The same
  `lol_html` pass that cleans the HTML can do it.
- **Populate the `attachments` table** at parse time — name, mime, size,
  content-id, inline flag. Bytes are never stored; the proxy endpoint refetches
  from Gmail on demand.

## Running the corpus

```
python3 backend/testdata/mime/generate.py   # rebuild the .eml files
python3 backend/testdata/mime/verify.py     # check the fixtures are well-formed
backend/testdata/fetch_live.sh 60           # refresh the real-mail sample (gitignored)
```

The Rust test reads `expected.json`, parses each sibling `.eml`, and asserts
every field. A panic is a failure, never a skip. Each case's `note` field says
what it defends against — put it in the failure message.
