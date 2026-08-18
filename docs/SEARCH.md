# Search and storage — Gmail is the index, we are the cache

A decision record. Supersedes the retrieval design in `PLAN.md` §Agent runtime
("Ask retrieval") and the `fts` column in the schema.

## What we were doing, and the number that killed it

`messages` carried a generated `tsvector` over subject, sender and body text,
maintained on every write, queried with `websearch_to_tsquery`. It is a
perfectly good index. It indexes **0.78% of the mailbox** — ~500 messages
inside the 30-day sync window, out of 63,866.

That is not a small index. It is a *wrong* one, and wrong in the worst
direction: a search for an invoice from March returns **zero results**, which is
indistinguishable from "no such mail exists". The user is not told their
archive was never indexed. They are told, silently, that their memory is wrong.

Widening the window does not fix it either. Syncing all 63,866 messages costs
~35–45 minutes of quota-paced fetching, gigabytes of body text, and a
permanently growing store — to rebuild an index Google already maintains, keeps
current, and serves in one call.

## What comparable apps actually do

Two families, and NADE is firmly in the second.

**Full local store** — Apple Mail, Outlook desktop, Thunderbird. They sync
*everything* over IMAP and index it locally (Spotlight, Windows Search,
Thunderbird's gloda). This makes sense when you already hold every byte,
which they do and we deliberately do not.

**Windowed cache, provider search** — Superhuman, Spark, Shortwave, Missive.
They cache a recent window for instant display, and delegate search to the
provider's API, hydrating results from the cache and fetching the misses. Their
search covers the whole mailbox from day one, on a store a thousandth the size.

## The decision

**Gmail is the search index. Postgres is a cache of what we have looked at.**

- `GET /search` and the agents' `search_mail` tool both go to
  `users.messages.list?q=…`. Gmail returns ids over the **entire** mailbox.
- We hydrate those ids from the `messages` table, fetch the misses from Gmail
  in a batch, store them, and return the thread rows.
- A fetched-on-demand message stays cached, so opening a thread makes it fast
  forever after.
- The `fts` column, its GIN index and the 100,000-character truncation are all
  removed. Nothing maintains a second index.

### What the cache is for

Display, not recall. It holds the 30-day window plus anything ever opened or
touched by an agent, so the mail list, thread view and agent tools read from
local rows at local speed. It is allowed to be incomplete, and the API says so:
a thread list is a view of the window, never a claim about the mailbox.

Eviction is out of scope for v1 — 500 messages is nothing. When it matters, the
rule is least-recently-shown, never touching anything an agent has referenced.

### What this buys

- Search covers 63,866 messages instead of 500, immediately.
- No write amplification, no GIN index, no truncation at 100k characters.
- No index staleness, no reindex-on-parser-change, no divergence between what
  Gmail thinks a message says and what we do.
- Google's ranking, operators and typo tolerance for free.
- The `href` question disappears. Nothing we extract is an index input, so
  whether a URL lands in `body_text` is purely a display and prompt-size
  question — which is why bare-URL-only lines are dropped and nothing more
  elaborate is needed.

### What it costs, stated honestly

- **A search is a network round trip** — ~200–400 ms, and 5 quota units against
  a 250/second ceiling. Irrelevant at one user; worth a per-account cache of
  recent queries if it ever is not.
- **Offline search degrades.** The server returns an error; the iOS app falls
  back to filtering the rows GRDB already holds, and **says so** rather than
  presenting a partial result as complete.
- **Gmail's query language has traps we measured.** A malformed query returns
  an empty 200 — never a 400 — so an empty result is ambiguous between "no
  matches" and "your query was nonsense". `q=label:` takes a label *name* and
  silently matches nothing when handed an id, which is exactly what a program
  has to hand. Both matter most for the agents' `search_mail`, where a model
  writes the query.

### Therefore, required with this change

`search_mail` and `GET /search` must **validate and normalise** the query
before sending it: reject unknown operators rather than passing them through to
a silent empty result, translate a label id to its name, and reject the units
Gmail does not support (`w`, bare numbers) instead of letting them match
nothing. When a query is rejected, say why — an agent that receives "unknown
operator `label:Label_25`; use the label name" can correct itself, and one that
receives an empty list cannot.

## Not doing

- **No embeddings, no vector store, no RAG.** Gmail's keyword search plus
  recency is the retrieval layer. The prior art for this project coupled an
  embedder into the ingest path and lost 13% of its data to it.
- **No attachment content indexing.** Attachment *metadata* is stored; bytes
  are never stored and never parsed for text.
- **No second index of any kind.** If something needs finding, it is found
  through Gmail or through a plain SQL predicate over the cache.
