#!/usr/bin/env python3
"""Generate every wire-contract fixture in `docs/contract/` from one world state.

Run from anywhere:

    python3 docs/contract/generate.py && python3 docs/contract/validate.py

Why a generator and not hand-written JSON: hand-authored fixtures drift. A run
ends up simultaneously `pending_approval` and `done`, a draft is attributed to
an agent that lacks `draft_reply`, a feed item points at a note id that no file
defines. Nothing forces them to agree. Here one Python object graph -- the
WORLD section below -- feeds every file, so a cross-reference cannot disagree
with itself; it is the same string.

Rules this file obeys:

* **No randomness, no clocks.** Every id is a literal or a `uuid5` derivation,
  every timestamp is a constant derived from one base day. Two runs produce
  byte-identical output. `validate.py` greps this file for `random`,
  `uuid.uuid4`, `datetime.now`, `time.time` and friends, and fails if any
  appear.
* **Python 3.9 compatible, standard library only.** The system interpreter is
  3.9.6; CI also runs 3.14. Output must be identical on both.
* **`docs/API.md` is the specification.** Field sets, nullability, enums and
  pagination come from there, not from what looked reasonable here.

See `README.md` for the file table and `validate.py` for the invariants.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))

# --------------------------------------------------------------------------
# Deterministic identifiers
# --------------------------------------------------------------------------

# Frozen in backend/crates/nade-agent-sdk/src/ids.rs as
#   Uuid::from_u128(0x6e_61_64_65_5f_65_66_66_65_63_74_5f_6e_73_76_31)
# which is the sixteen ASCII bytes of "nade_effect_nsv1", i.e.
#   6e616465-5f65-6666-6563-745f6e737631
# Deriving it from the bytes rather than pasting the hex means the Python and
# the Rust cannot silently diverge: they are two spellings of one constant.
EFFECT_NAMESPACE = uuid.UUID(bytes=b"nade_effect_nsv1")


def effect_id(run_id, seq):
    """`uuid5(EFFECT_NAMESPACE, "<run-uuid-hyphenated>:<seq>")`.

    Mirrors `nade_agent_sdk::effect_id`. The run id is always the hyphenated
    lowercase form, so `"<uuid>:<seq>"` is fixed width up to the colon and
    cannot be ambiguous.
    """
    return str(uuid.uuid5(EFFECT_NAMESPACE, "%s:%d" % (run_id, seq)))


def args_hash(args):
    """`sha256:<hex>` over the canonical JSON encoding of a tool's arguments.

    Mirrors `nade_agent_sdk::args_hash`. `serde_json::Value` is backed by a
    `BTreeMap` (the crate deliberately does not enable `preserve_order`), so
    object keys serialise sorted, with no spaces, and non-ASCII as raw UTF-8.
    """
    canonical = json.dumps(args, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def compact(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


# The four v1 tools' real fingerprints, as `nade_agent_sdk::tool_fingerprint`
# computes them over `{name, description, version, schema}`.
#
# These are copied from the build, exactly like `EFFECT_GOLDEN` in validate.py
# is copied from the SDK's own golden-vector test. Python cannot compute them:
# the inputs are Rust string literals, and `check_generator_is_deterministic`
# forbids this file from importing `subprocess` to go and ask.
#
# Refresh with:
#   cargo test -p nade-server print_tool_fingerprints -- --ignored --nocapture
#
# They are held in place from the Rust side by
# `agents::tools::tests::the_real_fingerprints_match_the_contracts_golden_table`,
# so a tool whose description or schema changes fails there - in the language
# that owns the hash - rather than drifting silently past this file.
TOOL_FINGERPRINTS = {
    "draft_reply": "sha256:2f0eb322bcb3d5ad33f7498d8239d85f730015213c73b03c003bf058b9b46e2f",
    "read_thread": "sha256:572e586bdbf66002e29529db5deb0d792b29bb667e763ee2c8dc4a57f51370f8",
    "search_mail": "sha256:7c5acf3f12e2386148f59b7930d467608bee79dc6b340e4e2f6b6471578cc4f1",
    "write_note": "sha256:839c157ed05e2127a38a0ea05eeebb05cd524ee265bd2dfdd9aaa3655d64c203",
}


def tool_fingerprint(tool):
    """`sha256:<hex>` over a tool's interface, as the engine records it.

    The real value, not a stand-in: P4 built the four tools, so the hashes a
    live build records are known and are the ones written here.
    """
    try:
        return TOOL_FINGERPRINTS[tool]
    except KeyError:
        raise SystemExit(
            "no fingerprint for tool %r; add it to TOOL_FINGERPRINTS (see the "
            "refresh command above)" % tool)


def cursor(ts, id_):
    """An opaque keyset cursor: base64url of `{"ts": ..., "id": ...}`, unpadded.

    Clients must not parse this. It is base64url only so it is URL-safe.
    """
    raw = json.dumps({"ts": ts, "id": id_}, separators=(",", ":")).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def snippet_of(body_text):
    """API.md section 2: whitespace-collapsed, at most 200 characters."""
    return " ".join(body_text.split())[:200]


def plus_days(ts, days):
    """Add whole days to a `YYYY-MM-DDTHH:MM:SSZ` string without a clock.

    Deliberately arithmetic on the calendar rather than `datetime.now()`
    anything: this file must never read a clock. 2026 is not a leap year.
    """
    month_lengths = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    year = int(ts[0:4])
    month = int(ts[5:7])
    day = int(ts[8:10])
    for _ in range(days):
        day += 1
        if day > month_lengths[month - 1]:
            day = 1
            month += 1
            if month > 12:
                month = 1
                year += 1
    return "%04d-%02d-%02d%s" % (year, month, day, ts[10:])


# ==========================================================================
# WORLD -- everything below is derived from this section and nothing else.
# ==========================================================================

ACCOUNT_EMAIL = "jatinsethi98@gmail.com"
APP_VERSION = "1.0"
SERVER_VERSION = "0.1.0"

# `nade_` + 64 lowercase hex (API.md section 0). Example value only: a real
# device token exists exactly once, in the pair response, and is never stored.
PAIR_TOKEN = "nade_" + "cfa3abd9e2174b0d8f6c5a1e93b7d420615c8ae7f9d3b26041a8c7e5d90b3f16"

# v1's entire tool set (API.md section 5.1).
V1_TOOLS = ["search_mail", "read_thread", "write_note", "draft_reply"]

# -- Mailboxes -------------------------------------------------------------
#
# Raw Gmail `labels.list` output, in Gmail's own order, before the server maps
# and filters it. Gmail's system labels come back with `name == id`
# ("CATEGORY_PERSONAL"), which is not a thing to show a person, and the raw
# list contains flags and places v1 does not sync. Both the mapping and the
# filtering happen below so they are visible rather than assumed.
RAW_LABELS = [
    # (label id, Gmail's name, Gmail's type, unread threads, total threads)
    ("CHAT", "CHAT", "system", 0, 0),
    ("SENT", "SENT", "system", 0, 37),
    ("INBOX", "INBOX", "system", 12, 201),
    ("IMPORTANT", "IMPORTANT", "system", 9, 154),
    ("TRASH", "TRASH", "system", 0, 18),
    ("DRAFT", "DRAFT", "system", 0, 4),
    ("SPAM", "SPAM", "system", 31, 62),
    ("STARRED", "STARRED", "system", 0, 9),
    ("UNREAD", "UNREAD", "system", 47, 47),
    ("YELLOW_STAR", "YELLOW_STAR", "system", 0, 2),
    ("CATEGORY_FORUMS", "CATEGORY_FORUMS", "system", 0, 3),
    ("CATEGORY_UPDATES", "CATEGORY_UPDATES", "system", 6, 88),
    ("CATEGORY_PERSONAL", "CATEGORY_PERSONAL", "system", 4, 63),
    ("CATEGORY_PROMOTIONS", "CATEGORY_PROMOTIONS", "system", 2, 41),
    ("CATEGORY_SOCIAL", "CATEGORY_SOCIAL", "system", 0, 12),
    # User labels, in the arbitrary order Gmail returns them. Names pass
    # through verbatim, awkward brackets and all.
    #
    # Real user label ids come in two shapes -- short (`Label_24`) for labels
    # made early, and long (`Label_1901000275082506782`) for ones made since.
    # Both are here on purpose: nothing downstream may assume a length.
    ("Label_1901000275082506782", "Subscriptions", "user", 5, 31),
    ("Label_2", "[Gmail]All Mail", "user", 0, 1284),
    ("Label_12", "To Reply", "user", 3, 14),
    ("Label_37", "BnbLeverage", "user", 0, 47),
    ("Label_25", "[Notion]", "user", 0, 8),
    ("Label_18", "Awaiting Reply", "user", 0, 6),
    ("Label_1901000275082506791", "Notes", "user", 1, 22),
]

LABEL_SUBSCRIPTIONS = "Label_1901000275082506782"
LABEL_TO_REPLY = "Label_12"

# API.md section 2: only these system labels are exposed, with these display
# names, in this order. Everything else Gmail calls a system label is a flag
# or a place v1 does not sync.
SYSTEM_LABEL_WHITELIST = [
    ("INBOX", "Inbox"),
    ("CATEGORY_PERSONAL", "Primary"),
    ("CATEGORY_UPDATES", "Updates"),
    ("CATEGORY_SOCIAL", "Social"),
    ("CATEGORY_PROMOTIONS", "Promotions"),
    ("CATEGORY_FORUMS", "Forums"),
    ("STARRED", "Starred"),
    ("SENT", "Sent"),
]

HIDDEN_LABEL_PREFIX = "[Gmail]"

# -- Threads and messages --------------------------------------------------
#
# Gmail ids are 16 lowercase hex characters. A thread's id is the id of its
# first message, which is why T1 and M1 share one.

T1 = "18f2a1b3c4d5e6f7"  # Kettle -- the thread three runs touch
T2 = "18f2705f4e3d2c1b"  # Foehn -- HTML-only, no text/plain part
T3 = "18f28c5d6e7f8a9b"  # Northbound design review
T4 = "18f2554a3b2c1d0e"  # Halcyon -- the skipped approval's thread
T5 = "18f29e0a1b2c3d4e"  # Chase statement
T6 = "18f1d8e7c6b5a409"  # Vale Robotics -- the expired approval's thread
T7 = "18f1f0a2b3c4d5e6"  # the oldest synced thread; no subject, no snippet

M1 = T1                  # first message of T1
M2 = "18f2b0c1d2e3f405"
M3 = "18f2c4d5e6f70819"
M_T2 = T2
M_T4 = T4
M_T6 = T6

ATT_PDF = (
    "ANGjdJ_qk9mR0xT2wPmC5nQ8vYbH3sLzF1eKp7uXcRtN6gWdA0iJlOoYzSxVhBmU"
    "-4fEc2QaTrPnKvDgIyLwZjXsMeHbNqUfCkRt9pB1oS5dGvA"
)
ATT_LOGO = (
    "ANGjdJ_7hTqE0wRm2YvC9nLpX4sBd6KfJ1aUgZoQ3iMtNbHxVeSwPyDrClkFuOjG"
    "-8zAtIeRnWq5MvXcYbLdKsHfTgJpNaUoEiZrQwBmSxVyCk"
)

# -- Agents ----------------------------------------------------------------
#
# UUID v4 literals: version nibble 4, variant nibble 8. Readable on purpose --
# these are fixtures, not secrets. Anything an agent writes is uuid5 instead,
# derived below.

AGENT_A = "a0000001-0000-4000-8000-000000000001"  # published, mail-triggered
AGENT_B = "a0000002-0000-4000-8000-000000000002"  # published, scheduled
AGENT_C = "a0000003-0000-4000-8000-000000000003"  # draft
AGENT_D = "a0000004-0000-4000-8000-000000000004"  # draft, failed to compile

RUN_PENDING = "b0000001-0000-4000-8000-000000000001"
RUN_DRAFTED = "b0000002-0000-4000-8000-000000000002"
RUN_NOTED = "b0000003-0000-4000-8000-000000000003"
RUN_SKIPPED = "b0000004-0000-4000-8000-000000000004"
RUN_FAILED = "b0000005-0000-4000-8000-000000000005"
RUN_EXPIRED = "b0000006-0000-4000-8000-000000000006"
RUN_PENDING_DRAFT = "b0000007-0000-4000-8000-000000000007"

FEED_LIVE = "c0000001-0000-4000-8000-000000000001"
FEED_RESOLVED = "c0000002-0000-4000-8000-000000000002"
FEED_SKIPPED = "c0000003-0000-4000-8000-000000000003"
FEED_EXPIRED = "c0000004-0000-4000-8000-000000000004"
FEED_INFO = "c0000005-0000-4000-8000-000000000005"
FEED_EDITABLE = "c0000006-0000-4000-8000-000000000006"

APPROVAL_TOKEN_LIVE = "f0000001-0000-4000-8000-000000000001"
APPROVAL_TOKEN_EDITABLE = "f0000002-0000-4000-8000-000000000002"

# The only note NADE writes outside a run: the one the server puts in place
# when the account is first connected. No run, so no `run_id`, so no
# `uuid5(run, seq)` to derive from -- hence a v4 id, per API.md section 6.1.
NOTE_WELCOME = "e0000001-0000-4000-8000-000000000001"

# -- The clock, frozen -----------------------------------------------------
#
# One base day, 2026-08-16/17, with every other instant written out relative
# to it in prose below. Ordering satisfies causality: a run starts before its
# journal entries, which precede its feed item, which precedes its resolution.

APPROVAL_TTL_DAYS = 7  # API.md section 7

TS = {
    # account setup
    "welcome_mail": "2026-07-18T00:00:00Z",
    "welcome_note": "2026-07-18T00:00:04Z",
    # 9 Aug -- the approval nobody answered
    "t6_mail": "2026-08-09T11:04:10Z",
    "r6_start": "2026-08-09T11:04:22Z",
    "r6_think": "2026-08-09T11:04:25Z",
    "r6_read_done": "2026-08-09T11:04:28Z",
    "r6_think2": "2026-08-09T11:04:30Z",
    "r6_ask": "2026-08-09T11:04:31Z",
    "r6_sweep": "2026-08-16T12:00:00Z",  # hourly expiry cron
    # 15 Aug -- the approval that was skipped
    "t4_mail": "2026-08-15T18:19:40Z",
    "r4_start": "2026-08-15T18:20:03Z",
    "r4_think": "2026-08-15T18:20:06Z",
    "r4_read_done": "2026-08-15T18:20:08Z",
    "r4_think2": "2026-08-15T18:20:10Z",
    "r4_ask": "2026-08-15T18:20:11Z",
    "r4_skip": "2026-08-15T19:41:52Z",
    # 16 Aug
    "t5_mail": "2026-08-16T07:40:11Z",
    "m1": "2026-08-16T09:12:04Z",
    "r5_start": "2026-08-16T09:12:06Z",
    "r5_think": "2026-08-16T09:12:09Z",
    "r5_step_error": "2026-08-16T09:12:30Z",
    "r5_cancel": "2026-08-16T09:12:31Z",
    "t2_mail": "2026-08-16T12:55:31Z",
    "m2": "2026-08-16T14:31:09Z",
    "r2_start": "2026-08-16T14:38:20Z",
    "r2_think": "2026-08-16T14:38:24Z",
    "r2_read_done": "2026-08-16T14:38:26Z",
    "r2_think2": "2026-08-16T14:38:35Z",
    "r2_ask": "2026-08-16T14:38:37Z",
    "r3_start": "2026-08-16T15:00:02Z",
    "r3_think": "2026-08-16T15:00:04Z",
    "r3_search_done": "2026-08-16T15:00:06Z",
    "r3_think2": "2026-08-16T15:00:07Z",
    "r3_write": "2026-08-16T15:00:08Z",
    "r3_done": "2026-08-16T15:00:09Z",
    "t3_mail": "2026-08-16T18:03:52Z",
    "r2_granted": "2026-08-16T18:07:44Z",
    "r2_write": "2026-08-16T18:07:45Z",
    "r2_done": "2026-08-16T18:07:46Z",
    # 17 Aug
    "d1_patched": "2026-08-17T07:41:12Z",
    "m3": "2026-08-17T08:22:40Z",
    "r1_start": "2026-08-17T08:22:43Z",
    "r1_think": "2026-08-17T08:22:47Z",
    "r1_read_done": "2026-08-17T08:22:52Z",
    "r1_think2": "2026-08-17T08:22:59Z",
    "r1_ask": "2026-08-17T08:23:01Z",
    "r7_start": "2026-08-17T09:05:11Z",
    "r7_think": "2026-08-17T09:05:15Z",
    "r7_read_done": "2026-08-17T09:05:17Z",
    "r7_think2": "2026-08-17T09:05:26Z",
    "r7_ask": "2026-08-17T09:05:28Z",
}


# --------------------------------------------------------------------------
# Mailboxes, derived
# --------------------------------------------------------------------------

def build_mailboxes():
    by_id = dict((row[0], row) for row in RAW_LABELS)
    out = []
    for label_id, display in SYSTEM_LABEL_WHITELIST:
        _, _, _, unread, total = by_id[label_id]
        out.append({
            "id": label_id,
            "name": display,
            "kind": "system",
            "unread": unread,
            "total": total,
        })
    user = [row for row in RAW_LABELS if row[2] == "user"]
    # API.md section 2: labels whose name starts with "[Gmail]" are hidden --
    # they are Gmail's own internal views, not mailboxes.
    user = [row for row in user if not row[1].startswith(HIDDEN_LABEL_PREFIX)]
    # "User labels follow, ordered by name", name passed through verbatim.
    user.sort(key=lambda row: row[1])
    for label_id, name, _, unread, total in user:
        out.append({
            "id": label_id,
            "name": name,
            "kind": "user",
            "unread": unread,
            "total": total,
        })
    return out


MAILBOXES = build_mailboxes()


# --------------------------------------------------------------------------
# Messages and threads
# --------------------------------------------------------------------------

M1_TEXT = (
    "Hi Jatin — I came across your work on the Northbound reader and wanted to "
    "check whether you're open to a conversation. We're hiring a staff designer to "
    "own the composer end to end.\n\nThe role outline is attached.\n\nPriya"
)
M1_HTML = (
    "<div><p>Hi Jatin — I came across your work on the Northbound reader and "
    "wanted to check whether you're open to a conversation. We're hiring a staff "
    "designer to own the composer end to end.</p>"
    "<p>The role outline is attached.</p>"
    '<p><img src="/v1/messages/%s/attachments/%s" alt="Kettle" width="96"></p>'
    "<p>Priya</p></div>"
) % (M1, ATT_LOGO)

M2_TEXT = (
    "Adding Dan, who leads the composer team.\n\nJatin — when are you free for a "
    "30-minute intro? Tuesday or Thursday afternoon both work on our side.\n\nPriya"
)

M3_TEXT = (
    "Two things to lock in: a 30-minute intro with our head of design, and a "
    "portfolio session the week after. Let me know which afternoon suits and I'll "
    "send the invites.\n\nPriya"
)
M3_HTML = (
    "<div><p>Two things to lock in: a 30-minute intro with our head of design, and "
    "a portfolio session the week after. Let me know which afternoon suits and I'll "
    "send the invites.</p><p>Priya</p></div>"
)

# T2 has no text/plain part at all. `body_text` is synthesised from the HTML
# (API.md section 2: it is guaranteed non-null), `body_html` is the original.
T2_TEXT = (
    "Guten Tag, Ihre Bestellung Nr. 88-2041 wurde versandt und trifft "
    "voraussichtlich am Dienstag ein. Sendungsverfolgung: FN-2041-88."
)
T2_HTML = (
    "<html><body><h1>Ihre Bestellung ist unterwegs \U0001f4e6</h1>"
    "<p>Guten Tag, Ihre Bestellung Nr. 88-2041 wurde versandt und trifft "
    "voraussichtlich am Dienstag ein.</p>"
    "<p>Sendungsverfolgung: FN-2041-88.</p></body></html>"
)

THREAD_DETAIL_T1 = {
    "id": T1,
    "subject": "Staff Product Designer at Kettle",
    "mailbox_name": "To Reply",
    "account_email": ACCOUNT_EMAIL,
    "messages": [
        {
            "gmail_id": M1,
            "from_name": "Priya Raghavan",
            "from_email": "priya@kettle.com",
            "to": [ACCOUNT_EMAIL],
            "cc": [],
            "ts": TS["m1"],
            "body_text": M1_TEXT,
            "body_html": M1_HTML,
            "label_ids": ["INBOX", "CATEGORY_PERSONAL", LABEL_TO_REPLY],
            "attachments": [
                {
                    "id": ATT_PDF,
                    "name": "Kettle — Staff Designer.pdf",
                    "mime": "application/pdf",
                    "size": 245760,
                    "inline": False,
                },
                {
                    "id": ATT_LOGO,
                    "name": "kettle-logo.png",
                    "mime": "image/png",
                    "size": 8194,
                    "inline": True,
                },
            ],
        },
        {
            # No text/html part: `body_html` is null, and is NOT a rendering of
            # the plain text. "View original" keys off exactly this.
            "gmail_id": M2,
            "from_name": "Priya Raghavan",
            "from_email": "priya@kettle.com",
            "to": [ACCOUNT_EMAIL],
            "cc": ["dan@kettle.com"],
            "ts": TS["m2"],
            "body_text": M2_TEXT,
            "body_html": None,
            "label_ids": ["INBOX", "CATEGORY_PERSONAL", LABEL_TO_REPLY],
            "attachments": [],
        },
        {
            "gmail_id": M3,
            "from_name": "Priya Raghavan",
            "from_email": "priya@kettle.com",
            "to": [ACCOUNT_EMAIL],
            "cc": [],
            "ts": TS["m3"],
            "body_text": M3_TEXT,
            "body_html": M3_HTML,
            "label_ids": ["INBOX", "UNREAD", "CATEGORY_PERSONAL", LABEL_TO_REPLY],
            "attachments": [],
        },
    ],
    "agent_cards": [],  # filled in below, once the runs exist
    "partial": False,
}

THREAD_DETAIL_T2 = {
    "id": T2,
    "subject": "Föhn — Ihre Bestellung ist unterwegs \U0001f4e6",
    "mailbox_name": "Subscriptions",
    "account_email": ACCOUNT_EMAIL,
    "messages": [
        {
            "gmail_id": M_T2,
            "from_name": "Föhn Versand",
            "from_email": "versand@foehn.de",
            # Undisclosed recipients: the header carried no To at all.
            "to": [],
            "cc": [],
            "ts": TS["t2_mail"],
            "body_text": T2_TEXT,
            "body_html": T2_HTML,
            "label_ids": ["INBOX", "CATEGORY_UPDATES", LABEL_SUBSCRIPTIONS],
            "attachments": [],
        }
    ],
    "agent_cards": [],
    "partial": False,
}


def thread_row(thread_id, subject, snippet, from_name, from_email, ts, unread,
               msg_count, agent_note):
    return {
        "id": thread_id,
        "subject": subject,
        "snippet": snippet,
        "from_name": from_name,
        "from_email": from_email,
        "ts": ts,
        "unread": unread,
        "msg_count": msg_count,
        "agent_note": agent_note,
    }


# `from_name`/`from_email`/`ts` are the NEWEST message's, matching what the row
# shows. For the two threads that also have a detail fixture, all four derived
# fields come straight off that detail so the two files cannot drift.
ROW_T1 = thread_row(
    T1, THREAD_DETAIL_T1["subject"], snippet_of(M3_TEXT),
    "Priya Raghavan", "priya@kettle.com", TS["m3"],
    True, len(THREAD_DETAIL_T1["messages"]),
    "Job Search Tracker · two next steps to approve",
)
ROW_T2 = thread_row(
    T2, THREAD_DETAIL_T2["subject"], snippet_of(T2_TEXT),
    "Föhn Versand", "versand@foehn.de", TS["t2_mail"],
    False, len(THREAD_DETAIL_T2["messages"]), None,
)
ROW_T3 = thread_row(
    T3, "Re: Design review — Thursday",
    "Works for me. I'll bring the composer flows and the two open questions from "
    "Monday.",
    "Kamran Ali", "kamran@northbound.co", TS["t3_mail"], False, 4,
    "Reply Drafter · a draft reply to approve",
)
ROW_T4 = thread_row(
    T4, "Senior Product Designer — Halcyon",
    "We're building out the design team and your name came up twice this week. "
    "Would you be open to a 45-minute screen?",
    # No display name on the header: "" rather than null. The UI decides
    # whether to fall back to the address; the server does not invent one.
    "", "talent@halcyon.io", TS["t4_mail"], False, 1, None,
)
ROW_T5 = thread_row(
    T5, "Your August statement is ready",
    "Your statement for the period ending 31 July is now available to view.",
    "Chase", "no.reply.alerts@chase.com", TS["t5_mail"], True, 1, None,
)
ROW_T6 = thread_row(
    T6, "Design Lead — Vale Robotics",
    "Following up on the systems interview and the paid trial week we discussed "
    "— are either of those still of interest?",
    "Nadia Okonkwo", "nadia@valerobotics.com", TS["t6_mail"], False, 2, None,
)
ROW_T7 = thread_row(
    # Mail with no subject at all, and a body that collapsed to nothing.
    T7, "", "", "", "hello@nade.local", TS["welcome_mail"], False, 1, None,
)

ALL_THREAD_ROWS = [ROW_T1, ROW_T2, ROW_T3, ROW_T4, ROW_T5, ROW_T6, ROW_T7]


# --------------------------------------------------------------------------
# Agents
# --------------------------------------------------------------------------

def filters(label_ids, newer_than_days, from_domains=None, from_contains=None,
            subject_contains=None, body_contains=None, has_attachment=None):
    """The deterministic SQL prefilter. All optional, all ANDed, none omitted."""
    return {
        "from_domains": from_domains or [],
        "from_contains": from_contains or [],
        "subject_contains": subject_contains or [],
        "body_contains": body_contains or [],
        "label_ids": label_ids,
        "has_attachment": has_attachment,
        "newer_than_days": newer_than_days,
    }


AGENTS = [
    {
        "id": AGENT_A,
        "name": "Job Search Tracker",
        "nl_definition": (
            "When a recruiter emails about a tech role, pull out the next steps "
            "and save them to a note. Ask me before you save."
        ),
        "status": "published",
        "trigger_summary": "On new mail",
        "schedule": None,
        "last_run_at": TS["r1_start"],
        "approval_required": True,
        "allowed_tools": ["search_mail", "read_thread", "write_note"],
        "compile_error": None,
        "when_span": "a recruiter emails about a tech role",
        "do_span": "pull out the next steps and save them to a note",
        "trailing": "Ask me before you save.",
        "spec": {
            "version": 1,
            "trigger": {
                "kind": "mail",
                "filters": filters(["INBOX"], 30),
                "semantic": (
                    "The sender is a recruiter, hiring manager, or in-house talent "
                    "partner writing about a specific engineering, design, or "
                    "product role."
                ),
            },
            "instruction": (
                "Read the thread. Extract every concrete next step the sender "
                "proposes -- calls, take-homes, intro meetings, deadlines -- with "
                "its date if one is given. Write them to a note titled after the "
                "company. If there are no concrete next steps, do nothing."
            ),
            "tools": ["search_mail", "read_thread", "write_note"],
            "output": {"kind": "note", "title_template": "{company} — next steps"},
        },
    },
    {
        "id": AGENT_B,
        "name": "Morning To-Do",
        "nl_definition": (
            "Every weekday at 8:00, build today's to-do list from the mail I "
            "haven't replied to and save it as a note."
        ),
        "status": "published",
        "trigger_summary": "Every weekday at 08:00",
        "schedule": {
            "freq": "week",
            "interval": 1,
            "byweekday": ["mon", "tue", "wed", "thu", "fri"],
            # freq=month only; null means "the anchor's day". Present, per
            # API.md section 0: nothing is ever omitted.
            "bymonthday": None,
            "at": "08:00",
            "tz": "America/Phoenix",
            "ends": {"kind": "never", "date": None, "count": None},
            "runs_done": 42,
        },
        "last_run_at": TS["r3_start"],
        "approval_required": False,
        "allowed_tools": ["search_mail", "read_thread", "write_note"],
        "compile_error": None,
        "when_span": "every weekday at 08:00",
        "do_span": "save today's to-do list as a note",
        # No trailing clause: this agent asks for nothing, so there is
        # nothing to say in the muted line after the sentence.
        "trailing": None,
        "spec": {
            "version": 1,
            "trigger": {
                "kind": "schedule",
                "filters": filters([LABEL_TO_REPLY], 7),
                "semantic": None,
            },
            "instruction": (
                "Search for threads in To Reply that Jatin has not answered. For "
                "each, write one line naming the person and what they are waiting "
                "for. Save the lot as a single note titled with today's date."
            ),
            "tools": ["search_mail", "read_thread", "write_note"],
            "output": {"kind": "note", "title_template": "To-do — {date}"},
        },
    },
    {
        "id": AGENT_C,
        "name": "Reply Drafter",
        "nl_definition": (
            "When someone asks when I'm free, draft a reply offering my "
            "availability. Always ask me first."
        ),
        # A newly created agent is always a draft, and a draft never runs on its
        # trigger. It can still be run once by hand from the builder, which is
        # what `trigger_kind: "manual"` on RUN_DRAFTED is.
        "status": "draft",
        "trigger_summary": "On new mail",
        "schedule": None,
        "last_run_at": TS["r7_start"],
        "approval_required": True,
        "allowed_tools": ["read_thread", "draft_reply"],
        "compile_error": None,
        "when_span": "someone asks when I'm free",
        "do_span": "draft a reply offering my availability",
        "trailing": "Always ask me first.",
        "spec": {
            "version": 1,
            "trigger": {
                "kind": "mail",
                "filters": filters(["INBOX"], 30),
                "semantic": (
                    "The sender is asking when Jatin is available, or proposing "
                    "times to meet."
                ),
            },
            "instruction": (
                "Read the thread. Draft a reply that names the times Jatin has "
                "already said work, in his register: short, no filler, no "
                "exclamation marks. Never send it."
            ),
            "tools": ["read_thread", "draft_reply"],
            "output": {"kind": "draft", "title_template": None},
        },
    },
    {
        "id": AGENT_D,
        "name": "Flight Watcher",
        "nl_definition": "do the thing with the stuff when it happens",
        # Compilation failed, so the agent is still created as a draft with
        # `spec: null` and `compile_error` set -- the user's sentence is never
        # lost (API.md section 5).
        "status": "draft",
        "trigger_summary": "Not set",
        "schedule": None,
        "last_run_at": None,
        "approval_required": True,
        "allowed_tools": [],
        "compile_error": (
            "I couldn't work out what should set this off or what it should do. "
            "Try naming the mail it should react to and what it should write."
        ),
        "when_span": None,
        "do_span": None,
        "trailing": None,
        "spec": None,
    },
]

AGENT_BY_ID = dict((a["id"], a) for a in AGENTS)

AGENT_LIST_FIELDS = [
    "id", "name", "nl_definition", "status", "trigger_summary", "schedule",
    "last_run_at", "approval_required",
]
# The full object only. `when_span`/`do_span`/`trailing` are the same three
# fields the `agent_draft` SSE event carries: the builder renders
# "When {when_span}, {do_span}." with both spans underlined and tappable, so a
# saved agent has to arrive with them or the screen behaves differently
# depending on how you reached it. They are derived from `spec` at compile
# time, so they are null exactly when `spec` is.
AGENT_FULL_FIELDS = AGENT_LIST_FIELDS + [
    "allowed_tools", "compile_error", "when_span", "do_span", "trailing", "spec",
]


def agent_list_row(agent):
    return dict((k, agent[k]) for k in AGENT_LIST_FIELDS)


def agent_full(agent):
    return dict((k, agent[k]) for k in AGENT_FULL_FIELDS)


# --------------------------------------------------------------------------
# Runs and journals
# --------------------------------------------------------------------------

class Journal(object):
    """Appends journal entries exactly as the SDK engine writes them.

    The vocabulary and every payload shape are transcribed from
    `backend/crates/nade-agent-sdk/src/journal.rs` (API.md section 6.1):
    `run_journal` is written ONLY by the engine, through the host's Journal
    driver, so these fixtures are engine output, never host output. Fields the
    SDK omits when absent (serde `skip_serializing_if`) are omitted here too --
    the one place API.md section 0's "nothing is ever omitted" rule does not
    apply. Key order follows the Rust struct declarations.

    `seq` starts at 1 and increases by one with no gaps (API.md section 6.1),
    enforced here by construction rather than typed out -- `(run_id, seq)` is a
    primary key, so two entries sharing a seq was never possible.

    A *step* is identified by the seq of the entry that **opened** it: the
    `step_started` for an ungated call, the `approval_requested` for a gated
    one. `step_started` and `step_done` carry that seq as `step_seq`, so a
    step is correlated by pointer rather than by guessing at the nearest
    preceding open with the same tool -- which is ambiguous the moment an
    agent calls a tool twice, which is ordinary. A tool failure is a
    `step_done` with `is_error: true`; there is no separate failure kind.

    `effect_id` derives from the opening seq and nothing else, so a gated step
    keeps the id minted at `approval_requested` through approval and execution.
    """

    def __init__(self, run_id):
        self.run_id = run_id
        self.entries = []
        self.turns = 0
        self.tokens_in = 0
        self.tokens_out = 0
        self.executed = set()   # step_seqs whose execution has started

    def add(self, kind, payload, at):
        seq = len(self.entries) + 1
        self.entries.append({
            "seq": seq,
            "kind": kind,
            "payload": payload,
            "created_at": at,
        })
        return seq

    def effect(self, seq):
        return effect_id(self.run_id, seq)

    def start(self, system, user_text, at):
        """`run_started`: the run's input plus the journal-format stamp."""
        self.add("run_started", {
            "input": {
                "system": system,
                "messages": [{"role": "user", "text": user_text}],
            },
            "journal_format": 1,
        }, at)

    def think(self, text, tokens_in, tokens_out, at, calls=()):
        """`model_response`: one model turn.

        `calls` is a list of `(call_id, tool, args)`. `tool_calls` is omitted
        when the turn requested none, and `stop_reason` follows: `tool_use`
        with calls, `end_turn` without. There is no per-entry model name --
        that is a post-v1 SDK field (API.md section 6.1).
        """
        self.turns += 1
        self.tokens_in += tokens_in
        self.tokens_out += tokens_out
        payload = {"turn": self.turns, "text": text}
        if calls:
            payload["tool_calls"] = [
                {"id": call_id, "name": tool, "arguments": tool_args}
                for call_id, tool, tool_args in calls
            ]
        payload["stop_reason"] = "tool_use" if calls else "end_turn"
        payload["usage"] = {"input_tokens": tokens_in,
                            "output_tokens": tokens_out}
        self.add("model_response", payload, at)

    def open_step(self, call_id, tool, tool_args, at):
        """An ungated call: the `step_started` entry opens its own step."""
        seq = len(self.entries) + 1
        self.executed.add(seq)
        self.add("step_started", {
            "step_seq": seq,
            "call_id": call_id,
            "tool": tool,
            "args": tool_args,
            "args_hash": args_hash(tool_args),
            "effect_id": self.effect(seq),
            "attempt": 1,
            "opened_at": at,
            "tool_fingerprint": tool_fingerprint(tool),
            "replay_policy": "retry",
        }, at)
        return seq

    def request_approval(self, call_id, tool, tool_args, at):
        """Open a gated step. The id minted here is what the feed item
        publishes, before the effect exists.

        `expires_at` is absent on purpose: NADE runs the engine with
        `approval_ttl = None` -- the host's 7-day sweep and the approve tx's
        410 own expiry (PLAN P4), and double enforcement would expire a timely
        approval whose resume job lagged. The feed item, its token and its
        deadline are host rows -- the journal never names them.
        """
        seq = len(self.entries) + 1
        self.add("approval_requested", {
            "step_seq": seq,
            "call_id": call_id,
            "tool": tool,
            "args": tool_args,
            "args_hash": args_hash(tool_args),
            "effect_id": self.effect(seq),
            "requested_at": at,
            "tool_fingerprint": tool_fingerprint(tool),
            "replay_policy": "retry",
        }, at)
        return seq

    def resolve(self, step_seq, decision, at):
        """`approval_resolved`: how the gate was settled, written by
        `Engine::resume` when the host's job delivers the human's answer --
        never by the approve/skip transaction itself. `reason` is omitted: it
        appears only when the engine itself decided (`ttl_expired`), which a
        host that owns expiry never triggers."""
        self.add("approval_resolved", {
            "step_seq": step_seq,
            "decision": decision,
            "resolved_at": at,
        }, at)

    def execute_gated_step(self, call_id, tool, tool_args, step_seq, at):
        """Run the step an earlier `approval_requested` opened, keeping its
        id, its args and its `opened_at` (the approval's `requested_at`)."""
        gate = self.entries[step_seq - 1]
        self.executed.add(step_seq)
        self.add("step_started", {
            "step_seq": step_seq,
            "call_id": call_id,
            "tool": tool,
            "args": tool_args,
            "args_hash": args_hash(tool_args),
            "effect_id": self.effect(step_seq),
            "attempt": 1,
            "opened_at": gate["payload"]["requested_at"],
            "tool_fingerprint": tool_fingerprint(tool),
            "replay_policy": "retry",
        }, at)

    def close_step(self, call_id, tool, result, step_seq, at,
                   is_error=False, truncated=False):
        self.add("step_done", {
            "step_seq": step_seq,
            "call_id": call_id,
            "tool": tool,
            "result": result,
            "is_error": is_error,
            "truncated": truncated,
        }, at)

    def end(self, status, at, output=None, reason=None):
        """`run_ended`: the terminal state, recorded once, always last.

        `steps` and `usage` are the run's own totals, accumulated here the
        same way the engine accumulates them, so they cannot disagree with
        the entries above. `output` (the model's closing prose) rides only on
        `done`; `reason` only on `failed`.
        """
        payload = {"status": status}
        if output is not None:
            payload["output"] = output
        if reason is not None:
            payload["reason"] = reason
        payload["steps"] = len(self.executed)
        payload["usage"] = {"input_tokens": self.tokens_in,
                            "output_tokens": self.tokens_out}
        self.add("run_ended", payload, at)


READ_T1_ARGS = {"thread_id": T1}
READ_T3_ARGS = {"thread_id": T3}
READ_T4_ARGS = {"thread_id": T4}
READ_T6_ARGS = {"thread_id": T6}

# The system prompt the host assembles for a run is the agent's compiled
# instruction. What exactly P4 wraps around it is the host's business; the
# journal records whatever it was, verbatim, in `run_started.input`.
A_INSTRUCTION = AGENT_BY_ID[AGENT_A]["spec"]["instruction"]
B_INSTRUCTION = AGENT_BY_ID[AGENT_B]["spec"]["instruction"]
C_INSTRUCTION = AGENT_BY_ID[AGENT_C]["spec"]["instruction"]

KETTLE_MAIL = (
    'New mail from priya@kettle.com on the thread '
    '"Staff Product Designer at Kettle".'
)


# -- RUN_FAILED: Agent A on M1, killed by Gmail -----------------------------
#
# The tool's upstream died: `step_done` records the structured error
# (`is_error: true`), and the host -- not the engine -- then ended the run
# loudly with `Engine::cancel`, which appends `run_ended {status: "failed",
# reason: {cap: "cancelled"}}`. That is the loud-fail path PLAN P4 mandates
# for a run that cannot make progress; a cap breach would have written
# `cap_breached` first instead.

_j5 = Journal(RUN_FAILED)
_j5.start(A_INSTRUCTION, KETTLE_MAIL, TS["r5_start"])
_j5.think("I'll read the thread first.", 1786, 88, TS["r5_think"],
          calls=[("call_1_1", "read_thread", READ_T1_ARGS)])
_seq = _j5.open_step("call_1_1", "read_thread", READ_T1_ARGS, TS["r5_think"])
_j5.close_step("call_1_1", "read_thread", {
    "error": "Gmail returned 503 for messages.get after 5 attempts.",
}, _seq, TS["r5_step_error"], is_error=True)
_j5.end("failed", TS["r5_cancel"], reason={
    "cap": "cancelled",
    "detail": "Gmail was unavailable, so the thread could not be read.",
})

# -- RUN_DRAFTED: Agent C, run once by hand, approved, wrote a draft --------

_j2 = Journal(RUN_DRAFTED)
_j2.start(C_INSTRUCTION,
          'Run once now, on the thread "Staff Product Designer at Kettle".',
          TS["r2_start"])
_j2.think("Reading the thread before drafting.", 1104, 61, TS["r2_think"],
          calls=[("call_1_1", "read_thread", READ_T1_ARGS)])
_seq = _j2.open_step("call_1_1", "read_thread", READ_T1_ARGS, TS["r2_think"])
_j2.close_step("call_1_1", "read_thread", {
    "thread_id": T1,
    "subject": "Staff Product Designer at Kettle",
    "message_count": 2,
    "latest": {
        "gmail_id": M2,
        "from_email": "priya@kettle.com",
        "ts": TS["m2"],
        "body_text": snippet_of(M2_TEXT),
    },
}, _seq, TS["r2_read_done"])

DRAFT_ARGS = {
    "thread_id": T1,
    "to": ["priya@kettle.com"],
    "subject": "Re: Staff Product Designer at Kettle",
    "body_text": (
        "Hi Priya,\n\nThanks for the introduction to Dan. Tuesday or Thursday "
        "afternoon both work for me — Tuesday after 2pm is easiest.\n\nBest,"
        "\nJatin"
    ),
}
_j2.think("Priya asked which afternoon works. I'll draft a reply naming Tuesday.",
          2418, 174, TS["r2_think2"],
          calls=[("call_2_1", "draft_reply", DRAFT_ARGS)])
# The gated step's identity is the seq of this `approval_requested` entry, not
# of the `step_started` that eventually executes it. That is what lets the
# server publish `data.draft_id` in the feed item before the draft exists.
# The feed item itself is a host row: the journal never names it.
R2_GATE_SEQ = _j2.request_approval("call_2_1", "draft_reply", DRAFT_ARGS,
                                   TS["r2_ask"])
DRAFT_ID = _j2.effect(R2_GATE_SEQ)
_j2.resolve(R2_GATE_SEQ, "approve", TS["r2_granted"])
# Executes under the id the approval minted: re-running this step upserts the
# same row rather than creating a second draft.
_j2.execute_gated_step("call_2_1", "draft_reply", DRAFT_ARGS, R2_GATE_SEQ,
                       TS["r2_write"])
_j2.close_step("call_2_1", "draft_reply", {
    "draft_id": DRAFT_ID,
    "to": ["priya@kettle.com"],
    "subject": "Re: Staff Product Designer at Kettle",
}, R2_GATE_SEQ, TS["r2_write"])
_j2.think("Drafted a reply to Priya proposing Tuesday or Thursday afternoon.",
          2691, 42, TS["r2_done"])
_j2.end("done", TS["r2_done"],
        output="Drafted a reply to Priya proposing Tuesday or Thursday afternoon.")

# -- RUN_NOTED: Agent B on schedule, no approval needed, wrote a note -------

_j3 = Journal(RUN_NOTED)
_j3.start(B_INSTRUCTION, "Scheduled run for Sunday 16 August.", TS["r3_start"])
SEARCH_ARGS = {"q": "label:\"To Reply\" newer_than:7d", "limit": 25}
_j3.think("Looking for threads in To Reply that are still waiting.", 942, 54,
          TS["r3_think"], calls=[("call_1_1", "search_mail", SEARCH_ARGS)])
_seq = _j3.open_step("call_1_1", "search_mail", SEARCH_ARGS, TS["r3_think"])
_j3.close_step("call_1_1", "search_mail", {
    "count": 6,
    "threads": [
        {"id": T1, "subject": THREAD_DETAIL_T1["subject"]},
        {"id": T3, "subject": ROW_T3["subject"]},
        {"id": T4, "subject": ROW_T4["subject"]},
    ],
}, _seq, TS["r3_search_done"])

NOTE_TITLE = "To-do — 16 Aug"
NOTE_BODY = (
    "# To-do — 16 Aug\n"
    "\n"
    "- [ ] Reply to Priya Raghavan about the Kettle intro\n"
    "- [ ] Get Kamran the composer flows before Thursday\n"
    "- [ ] Answer the Halcyon screen, or decline it\n"
    "- [ ] Chase Vale Robotics for the trial-week dates\n"
    "- [ ] Pay the August statement\n"
    "- [ ] Close the Föhn return window on Tuesday\n"
)
NOTE_ARGS = {"title": NOTE_TITLE, "body_md": NOTE_BODY, "thread_id": None}
_j3.think("Six threads. Writing the list.", 3104, 231, TS["r3_think2"],
          calls=[("call_2_1", "write_note", NOTE_ARGS)])
# Agent B does not require approval, so this step opens itself and its id comes
# from its own seq.
R3_WRITE_SEQ = _j3.open_step("call_2_1", "write_note", NOTE_ARGS, TS["r3_write"])
NOTE_ID = _j3.effect(R3_WRITE_SEQ)
_j3.close_step("call_2_1", "write_note", {
    "note_id": NOTE_ID,
    "title": NOTE_TITLE,
}, R3_WRITE_SEQ, TS["r3_write"])
_j3.think("Six threads still waiting on a reply. Saved as a note.", 3389, 38,
          TS["r3_done"])
_j3.end("done", TS["r3_done"],
        output="Six threads still waiting on a reply. Saved as a note.")

# -- RUN_PENDING: Agent A on M3, waiting on the live write_note approval ----

PENDING_NOTE_ARGS = {
    "title": "Kettle — next steps",
    "body_md": (
        "# Kettle — next steps\n"
        "\n"
        "- [ ] 30-minute intro with the head of design\n"
        "- [ ] Portfolio session the week after\n"
    ),
    "thread_id": T1,
}

_j1 = Journal(RUN_PENDING)
_j1.start(A_INSTRUCTION, KETTLE_MAIL, TS["r1_start"])
_j1.think("I'll read the thread first.", 1840, 96, TS["r1_think"],
          calls=[("call_1_1", "read_thread", READ_T1_ARGS)])
_seq = _j1.open_step("call_1_1", "read_thread", READ_T1_ARGS, TS["r1_think"])
# A long thread: the result was capped, and says so in the value rather than
# being silently trimmed.
# A result the engine had to cap. The envelope is the SDK's own
# (`nade_agent_sdk::tool::cap_result`), not a marker of our invention: the
# engine *replaces* an oversized result rather than trimming inside it, so a
# fixture carrying "…[truncated N bytes]" described output no build produces -
# the same defect class as the pre-P4 journal vocabulary, and caught the same
# way, by transcribing what the SDK actually writes.
_j1.close_step("call_1_1", "read_thread", {
    "nade_truncated": True,
    "reason": "tool result exceeded the configured size cap",
    "original_bytes": 17580,
    "limit_bytes": 16384,
    "preview": (
        "{\"thread_id\":\"" + T1 + "\",\"subject\":\"Staff Product Designer at "
        "Kettle\",\"message_count\":3,\"latest\":{\"from_email\":\"priya@kettle.com"
    ),
}, _seq, TS["r1_read_done"], truncated=True)
_j1.think("Two concrete next steps. Both need approval before I save them.",
          3210, 184, TS["r1_think2"],
          calls=[("call_2_1", "write_note", PENDING_NOTE_ARGS)])
R1_GATE_SEQ = _j1.request_approval("call_2_1", "write_note", PENDING_NOTE_ARGS,
                                   TS["r1_ask"])
PENDING_NOTE_ID = _j1.effect(R1_GATE_SEQ)

# -- RUN_PENDING_DRAFT: Agent C, waiting on the live draft_reply approval ---
#
# The second live approval, and the only one whose pending action is
# `draft_reply` -- which is what makes its card render
# ["approve", "edit", "skip"]. Edit means "approve, then edit the draft it
# created", the only edit path v1 has, so without this item the phone would
# have to draw an Edit button it had never decoded.

PENDING_DRAFT_ARGS = {
    "thread_id": T3,
    "to": ["kamran@northbound.co"],
    "subject": "Re: Design review — Thursday",
    "body_text": (
        "Hi Kamran,\n\nThursday works. I'll bring the composer flows and "
        "answers to the two open questions from Monday.\n\nJatin"
    ),
}

_j7 = Journal(RUN_PENDING_DRAFT)
_j7.start(C_INSTRUCTION,
          'Run once now, on the thread "Re: Design review — Thursday".',
          TS["r7_start"])
_j7.think("Reading the design review thread.", 1188, 58, TS["r7_think"],
          calls=[("call_1_1", "read_thread", READ_T3_ARGS)])
_seq = _j7.open_step("call_1_1", "read_thread", READ_T3_ARGS, TS["r7_think"])
_j7.close_step("call_1_1", "read_thread", {
    "thread_id": T3,
    "subject": ROW_T3["subject"],
    "message_count": 4,
    "latest": {
        "from_email": "kamran@northbound.co",
        "ts": TS["t3_mail"],
        "body_text": ROW_T3["snippet"],
    },
}, _seq, TS["r7_read_done"])
_j7.think("Kamran needs a time. Asking before I save the draft.", 2530, 166,
          TS["r7_think2"],
          calls=[("call_2_1", "draft_reply", PENDING_DRAFT_ARGS)])
R7_GATE_SEQ = _j7.request_approval("call_2_1", "draft_reply",
                                   PENDING_DRAFT_ARGS, TS["r7_ask"])
PENDING_DRAFT_ID = _j7.effect(R7_GATE_SEQ)

# -- RUN_SKIPPED: Agent A on M4, approval skipped ---------------------------

SKIPPED_NOTE_ARGS = {
    "title": "Halcyon — next steps",
    "body_md": (
        "# Halcyon — next steps\n"
        "\n"
        "- [ ] 45-minute screen with the design team\n"
    ),
    "thread_id": T4,
}

_j4 = Journal(RUN_SKIPPED)
_j4.start(A_INSTRUCTION,
          'New mail from talent@halcyon.io on the thread '
          '"Senior Product Designer — Halcyon".',
          TS["r4_start"])
_j4.think("Reading the Halcyon thread.", 1502, 71, TS["r4_think"],
          calls=[("call_1_1", "read_thread", READ_T4_ARGS)])
_seq = _j4.open_step("call_1_1", "read_thread", READ_T4_ARGS, TS["r4_think"])
_j4.close_step("call_1_1", "read_thread", {
    "thread_id": T4,
    "subject": ROW_T4["subject"],
    "message_count": 1,
    "latest": {
        "gmail_id": M_T4,
        "from_email": "talent@halcyon.io",
        "ts": TS["t4_mail"],
        "body_text": ROW_T4["snippet"],
    },
}, _seq, TS["r4_read_done"])
_j4.think("One next step. Asking before I save.", 2044, 118, TS["r4_think2"],
          calls=[("call_2_1", "write_note", SKIPPED_NOTE_ARGS)])
R4_GATE_SEQ = _j4.request_approval("call_2_1", "write_note", SKIPPED_NOTE_ARGS,
                                   TS["r4_ask"])
SKIPPED_NOTE_ID = _j4.effect(R4_GATE_SEQ)
# The human said no. `Engine::resume` records the decision and ends the run;
# the gated step never executes, so the note is never written.
_j4.resolve(R4_GATE_SEQ, "skip", TS["r4_skip"])
_j4.end("skipped", TS["r4_skip"])

# -- RUN_EXPIRED: Agent A on M6, approval never answered --------------------

EXPIRED_NOTE_ARGS = {
    "title": "Vale Robotics — next steps",
    "body_md": (
        "# Vale Robotics — next steps\n"
        "\n"
        "- [ ] Systems interview\n"
        "- [ ] Paid trial week\n"
    ),
    "thread_id": T6,
}

_j6 = Journal(RUN_EXPIRED)
_j6.start(A_INSTRUCTION,
          'New mail from nadia@valerobotics.com on the thread '
          '"Design Lead — Vale Robotics".',
          TS["r6_start"])
_j6.think("Reading the Vale Robotics thread.", 1611, 79, TS["r6_think"],
          calls=[("call_1_1", "read_thread", READ_T6_ARGS)])
_seq = _j6.open_step("call_1_1", "read_thread", READ_T6_ARGS, TS["r6_think"])
_j6.close_step("call_1_1", "read_thread", {
    "thread_id": T6,
    "subject": ROW_T6["subject"],
    "message_count": 2,
    "latest": {
        "gmail_id": M_T6,
        "from_email": "nadia@valerobotics.com",
        "ts": TS["t6_mail"],
        "body_text": ROW_T6["snippet"],
    },
}, _seq, TS["r6_read_done"])
_j6.think("Two next steps. Asking before I save.", 2260, 141, TS["r6_think2"],
          calls=[("call_2_1", "write_note", EXPIRED_NOTE_ARGS)])
R6_GATE_SEQ = _j6.request_approval("call_2_1", "write_note", EXPIRED_NOTE_ARGS,
                                   TS["r6_ask"])
EXPIRED_NOTE_ID = _j6.effect(R6_GATE_SEQ)
# The host's hourly sweep found the 7-day deadline passed and delivered
# `Resolution::Expire`; the engine records it as any other resolution. No
# `reason` field: `ttl_expired` appears only when the engine's own TTL check
# converted a late approve, and NADE runs with `approval_ttl = None`.
_j6.resolve(R6_GATE_SEQ, "expire", TS["r6_sweep"])
_j6.end("expired", TS["r6_sweep"])


RUNS = [
    {
        "id": RUN_PENDING,
        "agent_id": AGENT_A,
        "trigger_kind": "mail",
        "trigger_ref": M3,
        "status": "pending_approval",
        # No summary yet: the run has not produced one, and will not until the
        # approval it is blocked on is answered.
        "summary": None,
        "error": None,
        "created_at": TS["r1_start"],
        "updated_at": TS["r1_ask"],
        "journal": _j1.entries,
    },
    {
        "id": RUN_DRAFTED,
        "agent_id": AGENT_C,
        "trigger_kind": "manual",
        "trigger_ref": None,
        "status": "done",
        "summary": "Drafted a reply to Priya proposing Tuesday or Thursday afternoon.",
        "error": None,
        "created_at": TS["r2_start"],
        "updated_at": TS["r2_done"],
        "journal": _j2.entries,
    },
    {
        "id": RUN_NOTED,
        "agent_id": AGENT_B,
        "trigger_kind": "schedule",
        "trigger_ref": None,
        "status": "done",
        "summary": "Six threads still waiting on a reply. Saved as a note.",
        "error": None,
        "created_at": TS["r3_start"],
        "updated_at": TS["r3_done"],
        "journal": _j3.entries,
    },
    {
        "id": RUN_SKIPPED,
        "agent_id": AGENT_A,
        "trigger_kind": "mail",
        "trigger_ref": M_T4,
        "status": "skipped",
        "summary": "One next step found — a 45-minute screen with Halcyon.",
        "error": None,
        "created_at": TS["r4_start"],
        "updated_at": TS["r4_skip"],
        "journal": _j4.entries,
    },
    {
        "id": RUN_FAILED,
        "agent_id": AGENT_A,
        "trigger_kind": "mail",
        "trigger_ref": M1,
        "status": "failed",
        "summary": None,
        "error": "Gmail was unavailable, so the thread could not be read.",
        "created_at": TS["r5_start"],
        "updated_at": TS["r5_cancel"],
        "journal": _j5.entries,
    },
    {
        "id": RUN_PENDING_DRAFT,
        "agent_id": AGENT_C,
        "trigger_kind": "manual",
        "trigger_ref": None,
        "status": "pending_approval",
        "summary": None,
        "error": None,
        "created_at": TS["r7_start"],
        "updated_at": TS["r7_ask"],
        "journal": _j7.entries,
    },
    {
        "id": RUN_EXPIRED,
        "agent_id": AGENT_A,
        "trigger_kind": "mail",
        "trigger_ref": M_T6,
        "status": "expired",
        "summary": "Two next steps found — a systems interview and a paid trial week.",
        "error": None,
        "created_at": TS["r6_start"],
        "updated_at": TS["r6_sweep"],
        "journal": _j6.entries,
    },
]

RUN_BY_ID = dict((r["id"], r) for r in RUNS)


def run_list_row(run):
    return {
        "id": run["id"],
        "agent_id": run["agent_id"],
        "agent_name": AGENT_BY_ID[run["agent_id"]]["name"],
        "trigger_kind": run["trigger_kind"],
        "status": run["status"],
        "summary": run["summary"],
        "created_at": run["created_at"],
        "updated_at": run["updated_at"],
    }


def run_detail(run):
    return {
        "id": run["id"],
        "agent_id": run["agent_id"],
        "agent_name": AGENT_BY_ID[run["agent_id"]]["name"],
        "status": run["status"],
        "trigger_kind": run["trigger_kind"],
        "trigger_ref": run["trigger_ref"],
        "error": run["error"],
        "journal": run["journal"],
    }


# Newest first, which is what the Settings run log shows.
RUNS_NEWEST_FIRST = [RUN_PENDING_DRAFT, RUN_PENDING, RUN_NOTED, RUN_DRAFTED,
                     RUN_FAILED, RUN_SKIPPED, RUN_EXPIRED]

# Agent cards on the Kettle thread, newest run first. `feed_item_id` is null
# for the failed run: it never asked for anything, so no card to tap through to.
THREAD_DETAIL_T1["agent_cards"] = [
    {
        "run_id": RUN_PENDING,
        "agent_name": AGENT_BY_ID[AGENT_A]["name"],
        "status": "pending_approval",
        "summary": "Two next steps found — a 30-minute intro and a portfolio session.",
        "feed_item_id": FEED_LIVE,
    },
    {
        "run_id": RUN_DRAFTED,
        "agent_name": AGENT_BY_ID[AGENT_C]["name"],
        "status": "done",
        "summary": "Drafted a reply to Priya proposing Tuesday or Thursday afternoon.",
        "feed_item_id": FEED_RESOLVED,
    },
    {
        "run_id": RUN_FAILED,
        "agent_name": AGENT_BY_ID[AGENT_A]["name"],
        "status": "failed",
        "summary": "Couldn't finish — Gmail was unavailable.",
        "feed_item_id": None,
    },
]


# The one thread whose detail is *incomplete*. `docs/API.md` §2 says `partial`
# is true when the windowed cache could not finish completing a thread — Gmail
# unreachable, throttled, or a credential that needs re-auth — and that clients
# "must surface it rather than render the rows as a complete thread". Until P2
# there was no fixture for it at all, so neither side had ever serialised or
# decoded the case, on either lane.
#
# T6 is the honest carrier: it is the old thread that reached the cache through
# a *search* hit, which is exactly the cause API.md describes ("a search that
# matched one message of a ten-message conversation cached exactly that one").
# Its row says `msg_count: 2`; this detail carries one message and says why.
# The pair is the point — a client that derives one from the other gets it
# wrong, and `msg_count` is deliberately not `len(messages)` here.
THREAD_DETAIL_T6_PARTIAL = {
    "id": T6,
    "subject": ROW_T6["subject"],
    "mailbox_name": "To Reply",
    "account_email": ACCOUNT_EMAIL,
    "messages": [
        {
            "gmail_id": M_T6,
            "from_name": ROW_T6["from_name"],
            "from_email": ROW_T6["from_email"],
            "to": [ACCOUNT_EMAIL],
            "cc": [],
            "ts": TS["t6_mail"],
            "body_text": ROW_T6["snippet"],
            "body_html": None,
            "label_ids": ["INBOX", "CATEGORY_PERSONAL", LABEL_TO_REPLY],
            "attachments": [],
        }
    ],
    # The expired approval's own run. It is the only fixture anywhere that
    # renders DESIGN.md's `expired` status string on an agent card.
    "agent_cards": [
        {
            "run_id": RUN_EXPIRED,
            "agent_name": AGENT_BY_ID[AGENT_A]["name"],
            "status": "expired",
            "summary": "Two next steps found, but the approval was never answered.",
            "feed_item_id": FEED_EXPIRED,
        }
    ],
    "partial": True,
}



# --------------------------------------------------------------------------
# Notes and drafts
#
# The invariant the old fixtures broke: a note or draft row exists only if the
# run that wrote it reached `done`. RUN_PENDING, RUN_SKIPPED and RUN_EXPIRED
# never wrote theirs, so PENDING_NOTE_ID, SKIPPED_NOTE_ID and EXPIRED_NOTE_ID
# appear in feed items and journals but in no list. Their ids are still exact,
# which is the whole point of deriving them.
# --------------------------------------------------------------------------

NOTES = [
    {
        "id": NOTE_ID,                      # uuid5: RUN_NOTED wrote it
        "run_id": RUN_NOTED,
        "thread_id": None,
        "title": NOTE_TITLE,
        "agent_name": AGENT_BY_ID[AGENT_B]["name"],
        "unread": True,
        "updated_at": TS["r3_write"],
        "body_md": NOTE_BODY,
    },
    {
        "id": NOTE_WELCOME,                 # uuid4: no run, so nothing to derive
        "run_id": None,
        "thread_id": None,
        "title": "Welcome to NADE",
        "agent_name": None,
        "unread": False,
        "updated_at": TS["welcome_note"],
        "body_md": (
            "# Welcome to NADE\n"
            "\n"
            "Your mail is syncing. Agents you publish write their notes here.\n"
            "\n"
            "Nothing leaves this server: NADE writes notes and prepares drafts, "
            "and never sends mail.\n"
        ),
    },
]

NOTE_LIST_FIELDS = ["id", "run_id", "thread_id", "title", "agent_name", "unread",
                    "updated_at"]
NOTE_DETAIL_FIELDS = ["id", "title", "body_md", "run_id", "thread_id", "agent_name",
                      "unread", "updated_at"]


def note_list_row(note):
    return dict((k, note[k]) for k in NOTE_LIST_FIELDS)


def note_detail(note):
    row = dict((k, note[k]) for k in NOTE_DETAIL_FIELDS)
    # Reading a note marks it read, so the detail reports the state *after*
    # the read -- always false, never the value the list showed.
    row["unread"] = False
    return row


DRAFT_BODY_AS_WRITTEN = DRAFT_ARGS["body_text"]
DRAFT_BODY_AFTER_PATCH = (
    "Hi Priya,\n\nThanks for the introduction to Dan. Tuesday after 2pm works best "
    "for me; Thursday afternoon is my backup.\n\nBest,\nJatin"
)

DRAFT_AS_WRITTEN = {
    "id": DRAFT_ID,                          # uuid5: RUN_DRAFTED wrote it
    "run_id": RUN_DRAFTED,
    "thread_id": T1,
    "to": ["priya@kettle.com"],
    "subject": "Re: Staff Product Designer at Kettle",
    "body_text": DRAFT_BODY_AS_WRITTEN,
    "created_at": TS["r2_write"],
    "updated_at": TS["r2_write"],
}

# `PATCH /drafts/{id}` with `{"body_text": ...}` -- the only field that moved.
DRAFT_AFTER_PATCH = dict(DRAFT_AS_WRITTEN)
DRAFT_AFTER_PATCH["body_text"] = DRAFT_BODY_AFTER_PATCH
DRAFT_AFTER_PATCH["updated_at"] = TS["d1_patched"]

DRAFTS = [DRAFT_AS_WRITTEN]


# --------------------------------------------------------------------------
# Feed
# --------------------------------------------------------------------------

def actions_for(kind, status, action):
    """API.md section 7, verbatim.

    Edit means "approve, then edit the draft it created", which is the only
    edit path v1 has -- so it appears for `draft_reply` and nowhere else.
    There is no note editor in v1.
    """
    if kind != "approval" or status != "new":
        return []
    if action == "draft_reply":
        return ["approve", "edit", "skip"]
    return ["approve", "skip"]


def feed_item(item_id, kind, title, body, status, run_id, action, data,
              created_at, approval_token=None, resolved_note=None):
    expires = plus_days(created_at, APPROVAL_TTL_DAYS) if kind == "approval" else None
    return {
        "id": item_id,
        "kind": kind,
        "title": title,
        "body": body,
        "status": status,
        "run_id": run_id,
        "actions": actions_for(kind, status, action),
        # Non-null only while status=new AND kind=approval.
        "approval_token": approval_token,
        "approval_expires_at": expires,
        "resolved_note": resolved_note,
        "data": data,
        "created_at": created_at,
    }


FEED_ITEM_LIVE = feed_item(
    FEED_LIVE, "approval", AGENT_BY_ID[AGENT_A]["name"],
    "Two next steps found — a 30-minute intro and a portfolio session. Save them "
    "as a note?",
    "new", RUN_PENDING, "write_note",
    {
        "action": "write_note",
        "action_label": "Save note",
        "note_title": "Kettle — next steps",
        # Deterministic, and computable before the note exists. Deep-linking
        # after approving therefore needs no round-trip to learn the new id.
        "note_id": PENDING_NOTE_ID,
        "thread_id": T1,
    },
    TS["r1_ask"], approval_token=APPROVAL_TOKEN_LIVE,
)

FEED_ITEM_INFO = feed_item(
    FEED_INFO, "info", AGENT_BY_ID[AGENT_B]["name"],
    "Six threads still waiting on a reply. Saved to “To-do — 16 Aug”.",
    "new", RUN_NOTED, "none",
    {"action": "none", "note_id": NOTE_ID, "thread_id": None},
    TS["r3_done"],
)

FEED_ITEM_RESOLVED = feed_item(
    FEED_RESOLVED, "approval", AGENT_BY_ID[AGENT_C]["name"],
    "Priya asked when you're free. Draft a reply proposing Tuesday or Thursday "
    "afternoon?",
    "resolved", RUN_DRAFTED, "draft_reply",
    {
        "action": "draft_reply",
        "action_label": "Save draft",
        "draft_id": DRAFT_ID,
        "thread_id": T1,
        "to": ["priya@kettle.com"],
        "subject": "Re: Staff Product Designer at Kettle",
        # You have never messaged this address, so the UI flags the recipient.
        "never_messaged": True,
    },
    TS["r2_ask"], resolved_note="Saved to Drafts.",
)

FEED_ITEM_SKIPPED = feed_item(
    FEED_SKIPPED, "approval", AGENT_BY_ID[AGENT_A]["name"],
    "One next step found — a 45-minute screen with Halcyon. Save it as a note?",
    "skipped", RUN_SKIPPED, "write_note",
    {
        "action": "write_note",
        "action_label": "Save note",
        "note_title": "Halcyon — next steps",
        "note_id": SKIPPED_NOTE_ID,
        "thread_id": T4,
    },
    TS["r4_ask"], resolved_note="Skipped — nothing was saved.",
)

FEED_ITEM_EXPIRED = feed_item(
    FEED_EXPIRED, "approval", AGENT_BY_ID[AGENT_A]["name"],
    "Two next steps found — a systems interview and a paid trial week. Save them "
    "as a note?",
    "expired", RUN_EXPIRED, "write_note",
    {
        "action": "write_note",
        "action_label": "Save note",
        "note_title": "Vale Robotics — next steps",
        "note_id": EXPIRED_NOTE_ID,
        "thread_id": T6,
    },
    TS["r6_ask"], resolved_note="Expired after 7 days — nothing was saved.",
)

FEED_ITEM_EDITABLE = feed_item(
    FEED_EDITABLE, "approval", AGENT_BY_ID[AGENT_C]["name"],
    "Kamran needs a time for Thursday. Draft a reply confirming it and listing "
    "the two open questions?",
    "new", RUN_PENDING_DRAFT, "draft_reply",
    {
        "action": "draft_reply",
        "action_label": "Save draft",
        # Computable now; the draft itself does not exist until this is
        # approved, so it is in no list.
        "draft_id": PENDING_DRAFT_ID,
        "thread_id": T3,
        "to": ["kamran@northbound.co"],
        "subject": "Re: Design review — Thursday",
        # A colleague already in the sent mail, so no red flag on the card.
        "never_messaged": False,
    },
    TS["r7_ask"], approval_token=APPROVAL_TOKEN_EDITABLE,
)

# Newest first.
FEED_ITEMS = [FEED_ITEM_EDITABLE, FEED_ITEM_LIVE, FEED_ITEM_INFO,
              FEED_ITEM_RESOLVED, FEED_ITEM_SKIPPED, FEED_ITEM_EXPIRED]


def new_count(items):
    """The badge. Approvals *and* unseen info items. Counted, never asserted."""
    return len([i for i in items if i["status"] == "new"])


# --------------------------------------------------------------------------
# Errors -- one per code in API.md section 0
#
# `message` is end-user-facing English: what happened and what to do. Never a
# stack trace, never an internal identifier. The five that the backend already
# hardcodes in `nade-server/src/error.rs` are reproduced verbatim, because a
# test byte-compares them.
# --------------------------------------------------------------------------

ERRORS = [
    ("bad_request", "The request could not be understood."),
    ("unauthorized", "Bearer token missing, unknown, or revoked."),
    ("forbidden", "This account is not allowed to do that."),
    ("not_found", "That does not exist."),
    ("conflict", "Something changed while you were working. Reload and try again."),
    ("token_consumed", "This approval was already recorded."),
    ("gone", "That is no longer available."),
    ("approval_expired",
     "This approval expired after 7 days. Run the agent again to get a fresh one."),
    ("payload_too_large", "That is too big to send. The limit is 25 MB."),
    ("rate_limited", "Too many attempts. Wait a minute and try again."),
    ("needs_reauth", "Gmail needs to be reconnected. Sign in again in Settings."),
    ("upstream_unavailable",
     "Gmail didn't respond. Nothing was lost — try again in a moment."),
    ("internal", "Something went wrong on the server."),
]


# --------------------------------------------------------------------------
# Ask streams
# --------------------------------------------------------------------------

ASK_DRAFT_NL = (
    "When a recruiter emails about a tech role, save the next steps as a note. "
    "Ask me first."
)
ASK_DRAFT_WHEN = "a recruiter emails about a tech role"
ASK_DRAFT_DO = "save the next steps as a note"


def sse(events):
    """Exact wire bytes: `event: <name>\\n data: <json>\\n`, blank line between.

    A single space after the colon, which is what the SSE grammar strips and
    what both clients already parse. The file ends with the blank line that
    terminates the last event.
    """
    out = []
    for name, payload in events:
        out.append("event: %s\ndata: %s\n\n" % (name, compact(payload)))
    return "".join(out)


def ask_answer_stream():
    tokens = ["Priya", " proposed", " a 30-minute intro", " with the head of design,",
              " then a portfolio session the week after."]
    events = [("route", {"kind": "answer"})]
    for text in tokens:
        events.append(("token", {"text": text}))
    events.append(("done", {"sources": [
        {"gmail_id": M3, "subject": THREAD_DETAIL_T1["subject"]},
        {"gmail_id": M1, "subject": THREAD_DETAIL_T1["subject"]},
    ]}))
    return events


def ask_results_stream():
    # The design renders a prose lead-in above the hits, so the stream carries
    # both: tokens first, then the hits, in the threads-list shape with no
    # cursor -- an SSE frame is not a page.
    tokens = ["Four threads", " mention design", " in the last month. "]
    events = [("route", {"kind": "results"})]
    for text in tokens:
        events.append(("token", {"text": text}))
    events.append(("results", {"threads": [ROW_T1, ROW_T3, ROW_T4, ROW_T6]}))
    events.append(("done", {"sources": []}))
    return events


def ask_agent_draft_stream():
    return [
        ("route", {"kind": "agent_draft"}),
        ("draft", {
            "name": "Recruiter Next Steps",
            "nl_definition": ASK_DRAFT_NL,
            # Sent as separate fields rather than left for the client to find
            # in a sentence: the design underlines exactly these two spans.
            "when_span": ASK_DRAFT_WHEN,
            "do_span": ASK_DRAFT_DO,
            "trailing": "Asks before it saves.",
            "tool": "write_note",
            "approval_required": True,
            "status": "draft",
        }),
        ("done", {"sources": []}),
    ]


def ask_error_stream():
    # An error may follow partial tokens. The client keeps what it received and
    # shows the error beneath it. `done` does not follow `error`.
    return [
        ("route", {"kind": "answer"}),
        ("token", {"text": "Priya"}),
        ("token", {"text": " proposed"}),
        ("error", {
            "code": "upstream_unavailable",
            "message": "The model didn't respond. Try that again in a moment.",
        }),
    ]


# --------------------------------------------------------------------------
# Fixture manifest
# --------------------------------------------------------------------------

def build_fixtures():
    threads_page_one = [ROW_T1, ROW_T3, ROW_T2, ROW_T5, ROW_T4]
    last = threads_page_one[-1]

    files = []

    def add(name, value):
        files.append((name, value))

    # -- 1. Auth and identity ----------------------------------------------
    add("pair.json", {"token": PAIR_TOKEN})
    # `cap` is an example value, exactly like PAIR_TOKEN: a real one exists
    # once, is consumed by `GET /auth/gmail/start`, and is never stored.
    add("gmail_link.json", {
        "url": "http://localhost:8080/v1/auth/gmail/start?cap="
               "9f2c7a1e8b40d6539ac1e7f204b8d3617e5a09c4d82b6f13a7e05c9b41d8f26a",
        "expires_at": "2026-08-16T09:22:04Z",
    })
    add("me.json", {"email": ACCOUNT_EMAIL, "status": "ok"})
    add("me_needs_reauth.json", {"email": ACCOUNT_EMAIL, "status": "needs_reauth"})

    # -- 2. Mail -----------------------------------------------------------
    add("mailboxes.json", {"mailboxes": MAILBOXES})
    add("threads.json", {
        "threads": threads_page_one,
        "next_cursor": cursor(last["ts"], last["id"]),
    })
    add("threads_last_page.json", {"threads": [ROW_T7], "next_cursor": None})
    add("threads_empty.json", {"threads": [], "next_cursor": None})
    add("thread.json", THREAD_DETAIL_T1)
    add("thread_html_only.json", THREAD_DETAIL_T2)
    add("thread_partial.json", THREAD_DETAIL_T6_PARTIAL)
    add("search.json", {
        "threads": [ROW_T1, ROW_T3, ROW_T4, ROW_T6],
        "next_cursor": None,
    })
    add("search_empty.json", {"threads": [], "next_cursor": None})

    # -- 3. Notes and drafts -----------------------------------------------
    add("notes.json", {
        "notes": [note_list_row(n) for n in NOTES],
        "next_cursor": None,
    })
    add("notes_empty.json", {"notes": [], "next_cursor": None})
    add("note.json", note_detail(NOTES[0]))
    add("drafts.json", {"drafts": list(DRAFTS), "next_cursor": None})
    add("drafts_empty.json", {"drafts": [], "next_cursor": None})
    add("draft.json", DRAFT_AFTER_PATCH)

    # -- 4. Ask ------------------------------------------------------------
    #
    # The only request bodies in the corpus. Everything else here is a
    # response, but `route_hint` has nowhere else to be pinned: it exists only
    # on the way in, and the iOS side encodes it.
    add("ask_request.json", {
        "query": "what did Priya say about next steps?",
        "thread_id": T1,
        # Normally null: routing is decided server-side, heuristics first.
        "route_hint": None,
    })
    add("ask_request_route_hint.json", {
        "query": "when a recruiter emails about a tech role, save the next steps",
        "thread_id": None,
        # Forced, not classified. This is the results screen's "Make this an
        # agent" button, which is otherwise impossible to implement.
        "route_hint": "agent_draft",
    })

    # -- 5. Agents ---------------------------------------------------------
    add("agents.json", {"agents": [agent_list_row(a) for a in AGENTS]})
    add("agents_empty.json", {"agents": []})
    # `GET /agents` does not carry `allowed_tools`, so the full object is
    # fixtured for *every* agent that has runs -- otherwise nothing can check
    # that a run only called tools its agent is allowed to call, which is how a
    # draft ended up attributed to an agent without `draft_reply`.
    add("agent.json", agent_full(AGENT_BY_ID[AGENT_A]))
    add("agent_scheduled.json", agent_full(AGENT_BY_ID[AGENT_B]))
    add("agent_draft.json", agent_full(AGENT_BY_ID[AGENT_C]))
    add("agent_compile_failed.json", agent_full(AGENT_BY_ID[AGENT_D]))

    # -- 6. Runs -----------------------------------------------------------
    add("runs.json", {
        "runs": [run_list_row(RUN_BY_ID[r]) for r in RUNS_NEWEST_FIRST],
        "next_cursor": None,
    })
    add("runs_empty.json", {"runs": [], "next_cursor": None})
    add("run.json", run_detail(RUN_BY_ID[RUN_PENDING]))
    add("run_done.json", run_detail(RUN_BY_ID[RUN_DRAFTED]))
    add("run_failed.json", run_detail(RUN_BY_ID[RUN_FAILED]))
    # The second pending run: gated on draft_reply rather than write_note,
    # and the run whose still-unwritten draft id feed_item_editable.json
    # publishes.
    add("run_pending_draft.json", run_detail(RUN_BY_ID[RUN_PENDING_DRAFT]))
    # Every run whose feed item publishes an effect id needs a journal to
    # corroborate that id -- including the two whose effect will now never be
    # written. That is exactly the "feed item cites a note that does not exist"
    # bug, so the ids get checked rather than trusted.
    add("run_skipped.json", run_detail(RUN_BY_ID[RUN_SKIPPED]))
    add("run_expired.json", run_detail(RUN_BY_ID[RUN_EXPIRED]))

    # -- 7. Feed -----------------------------------------------------------
    add("feed.json", {
        "items": FEED_ITEMS,
        "next_cursor": None,
        "new_count": new_count(FEED_ITEMS),
    })
    add("feed_empty.json", {"items": [], "next_cursor": None, "new_count": 0})
    add("feed_item.json", FEED_ITEM_LIVE)
    add("feed_item_info.json", FEED_ITEM_INFO)
    # The only card that renders an Edit button: approve, then edit the draft
    # it created.
    add("feed_item_editable.json", FEED_ITEM_EDITABLE)
    # The response to approving FEED_RESOLVED: the run moved
    # pending_approval -> queued in the same transaction that resolved the item.
    add("approve.json", {"run_id": RUN_DRAFTED, "status": "queued"})
    add("skip.json", {"status": "skipped"})
    # After marking the one info item seen, only the live approval is still new.
    add("seen.json", {
        "new_count": len([i for i in FEED_ITEMS
                          if i["status"] == "new" and i["kind"] != "info"]),
    })

    # -- 8. Settings -------------------------------------------------------
    add("settings.json", {
        "approval_required_default": True,
        "sync_window_days": 30,
        "spend_ceiling_usd_per_day": 1.0,
        "app_version": APP_VERSION,
        "disclosure": (
            "Your mail syncs to your own server and is processed by the AI models "
            "you connect."
        ),
    })

    # -- 10. Health --------------------------------------------------------
    add("healthz.json", {"status": "ok", "db": "ok", "version": SERVER_VERSION})
    add("healthz_db_down.json",
        {"status": "degraded", "db": "down", "version": SERVER_VERSION})

    # -- 0. Errors ---------------------------------------------------------
    for code, message in ERRORS:
        add("error_%s.json" % code, {"error": {"code": code, "message": message}})

    return files


def build_streams():
    return [
        ("ask_answer.sse", ask_answer_stream()),
        ("ask_results.sse", ask_results_stream()),
        ("ask_agent_draft.sse", ask_agent_draft_stream()),
        ("ask_error.sse", ask_error_stream()),
    ]


# --------------------------------------------------------------------------
# Emit
# --------------------------------------------------------------------------

def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)


def main():
    fixtures = build_fixtures()
    streams = build_streams()

    written = set()
    for name, value in fixtures:
        # Two-space indent, UTF-8 as itself (em dashes stay em dashes), one
        # trailing newline. validate.py re-serialises and byte-compares, so a
        # hand edit that reflows the file fails there.
        text = json.dumps(value, indent=2, ensure_ascii=False) + "\n"
        write_text(os.path.join(HERE, name), text)
        written.add(name)

    for name, events in streams:
        write_text(os.path.join(HERE, name), sse(events))
        written.add(name)

    # Anything left over is a fixture that used to exist. Remove it, so the
    # directory is exactly what this file says it is and a rename cannot leave
    # a stale file behind for a client to keep decoding.
    removed = []
    for name in sorted(os.listdir(HERE)):
        if not (name.endswith(".json") or name.endswith(".sse")):
            continue
        if name not in written:
            os.remove(os.path.join(HERE, name))
            removed.append(name)

    sys.stdout.write("generated %d fixtures in %s\n" % (len(written), HERE))
    if removed:
        sys.stdout.write("removed %d stale file(s): %s\n"
                         % (len(removed), ", ".join(removed)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
