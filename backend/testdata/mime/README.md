# MIME parser conformance corpus

26 hand-built `.eml` files plus `expected.json`, the ground truth our Rust
parser (P2, `mail/parse.rs`) must recover from them.

## Why this exists rather than a dump of real mail

The plan originally called for 237 real message bodies as golden files. Those
turned out to be the *output* of a parser we already know is buggy — no RFC 2047,
no quoted-printable, panics on bad dates — so comparing against them would have
encoded the bugs as the specification. Worse, they are personal mail and would
have had to live in git.

So the truth is defined first, as plain Python values in `generate.py`, and then
encoded into RFC-822 bytes by Python's standard library. The truth never passes
through any parser of ours. Every case is something that actually arrives in an
inbox and a real way naive parsers break.

Real mail still gets tested — at P2, against the live account, but as a
*smoke* (nothing panics, `body_text` is never empty, every message round-trips)
rather than as golden output. Those messages stay out of git.

## Files

| File | Purpose |
|---|---|
| `generate.py` | Defines the ground truth and emits the `.eml` files. Re-run to regenerate; output is deterministic. |
| `verify.py` | Checks the fixtures against `expected.json` using Python's stdlib parser. Guards against a malformed fixture making a Rust test meaningless. |
| `expected.json` | What the parser must produce, per file. |
| `NN-*.eml` | The cases. |

Both scripts pass under Python 3.9 and 3.14 with identical results.

## The five intentional divergences

`verify.py` uses Python's stdlib as an independent check, but three cases
encode behaviour the stdlib deliberately does *not* implement. These are
listed in `ALLOWED_DIVERGENCE` and are exactly where our parser must be better
than a spec-literal one:

- **09, 10** — the only text part is `text/html`, so `body_text` has to be
  synthesised. `PLAN.md` guarantees `body_text` is non-empty; html2text is how.
  Script and style contents must not leak in, and entities must be decoded.
- **12** — the sender labels Windows-1252 bytes as `iso-8859-1`. A literal
  decode turns `0x92` into a C1 control character and the reader sees `Don�t`.
  Every real mail client treats a declared `iso-8859-1` as `windows-1252`. So
  must we, or a large fraction of ordinary marketing mail renders as mojibake.

## What each case is defending against

| # | Case | The bug it catches |
|---|---|---|
| 01 | plain ascii | baseline |
| 02 | RFC 2047 base64 subject | raw `=?UTF-8?B?…?=` shown to the user |
| 03 | RFC 2047 Q encoding | `_` rendered as underscore instead of space |
| 04 | iso-8859-1 encoded word | legacy charset ignored |
| 05 | adjacent encoded words | spurious space between them (RFC 2047 §6.2) |
| 06 | quoted-printable | soft line breaks becoming real newlines |
| 07 | base64 body | multi-byte characters split across decode chunks |
| 08 | multipart/alternative | picking HTML when plain text exists |
| 09 | html only | empty `body_text`, or CSS/JS leaking into it |
| 10 | multipart/related + `cid:` | inline image lost, or `cid:` left unrewritten |
| 11 | attachment, RFC 2047 filename | mojibake filenames |
| 12 | cp1252 mislabelled | mojibake on ordinary marketing mail |
| 13 | no Date header | crash, or a bogus epoch timestamp |
| 14 | malformed Date | **panic** — Jarvis did exactly this |
| 15 | bare address | inventing a display name from the local part |
| 16 | group syntax, empty Cc | phantom recipient, or a parse error |
| 17 | raw 8-bit UTF-8 | assuming headers are always ASCII |
| 18 | nested multipart | attachment text appended to the body |
| 19 | empty body | null vs empty string, failed non-empty guarantee |
| 20 | folded headers | truncation at the fold |
| 21 | emoji subject | slicing an astral codepoint in half |
| 22 | malformed base64 | aborting the whole sync over one bad message |
| 23 | unknown charset | error instead of a fallback |
| 24 | duplicate Subject | non-deterministic choice |
| 25 | mixed CRLF/LF | losing the header/body boundary |
| 26 | 200 KB body | silent truncation; the `fts` column caps at 100k, the body does not |

## What real mail in this account actually looks like

Profiled from 60 raw messages pulled out of the live 30-day window on
2026-08-17 (`backend/testdata/fetch_live.sh`, output gitignored):

| Dimension | Distribution |
|---|---|
| Top-level content type | `multipart/alternative` 33 · **`text/html` 23** · `multipart/mixed` 3 · `multipart/related` 1 |
| Transfer encoding | quoted-printable 73 · 7bit 16 · base64 8 |
| Declared charset | utf-8 majority · **iso-8859-1 11** · us-ascii 3 |
| RFC 2047 subjects | 11 of 60 |

Two numbers change how this corpus should be read:

1. **38% of real messages are `text/html` with no plain-text part at all.**
   Case 09 is not an exotic fallback — html2text is a primary path, and the
   quality of that conversion is the quality of the mail list snippet, the
   search index, and everything an agent reads. Treat it accordingly.
2. **`iso-8859-1` really is declared in this inbox.** Case 12's cp1252
   substitution is not a theoretical nicety; without it a chunk of ordinary
   mail renders with replacement characters.

The 30-day window holds ~500 messages, comfortably under the
`MAX_SYNC_MESSAGES=2000` dev cap, so a full test sync is cheap.

## How the Rust test should use it

Read `expected.json`, iterate its keys, parse the sibling `.eml`, and assert
each field. Treat a panic as a failure, not a skip. `body_text_contains` is a
substring assertion (whitespace normalisation is fine); `body_text_excludes`
must not appear at all. The `note` field on a case explains what it is for —
put it in the failure message so a future reader knows why the case exists.
