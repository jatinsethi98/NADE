#!/usr/bin/env python3
"""Check every fixture in `docs/contract/` against `docs/API.md`.

    python3 docs/contract/generate.py && python3 docs/contract/validate.py

Exits non-zero, listing every violation, on any failure. This is the file that
stops the last round of bugs coming back: a run that was simultaneously
`pending_approval` and `done`, a draft attributed to an agent without
`draft_reply`, a feed item pointing at a note id no file defined, "deterministic"
effect ids that were actually uuid4.

The expected shapes below are **written out by hand from API.md**, not derived
from the generated output. That is the point: a schema inferred from the thing
it is checking validates nothing. When API.md changes, this file changes with
it, and `generate.py` is what has to catch up.

Python 3.9 compatible, standard library only.
"""

from __future__ import annotations

import ast
import datetime
import hashlib
import json
import os
import re
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))

ERRORS = []


def fail(where, message):
    ERRORS.append("%s: %s" % (where, message))


# ==========================================================================
# 1. The frozen effect-id namespace
# ==========================================================================

# Two independent spellings of one constant. `ids.rs` writes it as
# `Uuid::from_u128(0x6e_61_64_65_...)`; API.md section 6.1 writes it as hex;
# both are the ASCII bytes of "nade_effect_nsv1". If any of the three drift
# apart, every journal ever written stops replaying onto the same effects.
NAMESPACE_FROM_BYTES = uuid.UUID(bytes=b"nade_effect_nsv1")
NAMESPACE_FROM_HEX = uuid.UUID("6e616465-5f65-6666-6563-745f6e737631")
NAMESPACE_FROM_U128 = uuid.UUID(int=0x6E61_6465_5F65_6666_6563_745F_6E73_7631)

# Copied from durable-agent `effect_id_golden_vector` (tests/happy.rs).
EFFECT_GOLDEN = [
    ("00000000-0000-0000-0000-000000000000", 1, "e7293919-5143-5626-903c-d82c4d5be3c8"),
    ("00000000-0000-0000-0000-000000000000", 3, "1ac9540d-d1eb-5c36-8fcf-58ce0a96f997"),
    ("00000000-0000-0000-0000-000000000000", 5, "01ba250f-ac9e-5afd-9b06-f18ce4fd5af0"),
]


def effect_id(run_id, seq):
    return str(uuid.uuid5(NAMESPACE_FROM_BYTES, "%s:%d" % (run_id, seq)))


def check_namespace():
    where = "effect namespace"
    if NAMESPACE_FROM_BYTES != NAMESPACE_FROM_HEX:
        fail(where, "bytes(b'nade_effect_nsv1')=%s != API.md hex %s"
             % (NAMESPACE_FROM_BYTES, NAMESPACE_FROM_HEX))
    if NAMESPACE_FROM_BYTES != NAMESPACE_FROM_U128:
        fail(where, "bytes form != ids.rs Uuid::from_u128 form")
    for run_id, seq, expected in EFFECT_GOLDEN:
        got = effect_id(run_id, seq)
        if got != expected:
            fail(where, "effect_id(%s, %d) = %s, Rust says %s"
                 % (run_id, seq, got, expected))


def args_hash(args):
    canonical = json.dumps(args, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return "sha256:" + hashlib.sha256(canonical.encode("utf-8")).hexdigest()


# ==========================================================================
# 2. Primitive formats (API.md section 0)
# ==========================================================================

TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
GMAIL_ID_RE = re.compile(r"^[0-9a-f]{16}$")
MAILBOX_ID_RE = re.compile(r"^(INBOX|SENT|STARRED|CATEGORY_[A-Z]+|Label_\d+)$")
# A message's `label_ids` is Gmail's raw membership, so it also carries the
# flags that are not mailboxes -- UNREAD, IMPORTANT and friends.
LABEL_ID_RE = re.compile(r"^([A-Z][A-Z_]*|Label_\d+)$")
ATTACHMENT_ID_RE = re.compile(r"^[A-Za-z0-9_-]{20,}$")
CURSOR_RE = re.compile(r"^[A-Za-z0-9_-]+$")
BEARER_RE = re.compile(r"^nade_[0-9a-f]{64}$")
HHMM_RE = re.compile(r"^([01]\d|2[0-3]):[0-5]\d$")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")


def parse_ts(value):
    """Second-precision UTC only: no offsets, no fractional seconds, always Z."""
    if not isinstance(value, str) or not TS_RE.match(value):
        return None
    try:
        return datetime.datetime(
            int(value[0:4]), int(value[5:7]), int(value[8:10]),
            int(value[11:13]), int(value[14:16]), int(value[17:19]),
        )
    except ValueError:
        return None


# ==========================================================================
# 3. A tiny shape language, so every expectation below is explicit
# ==========================================================================

def NUL(spec):
    """`spec|null` -- always present, may be null."""
    return ("null", spec)


def OPT(spec):
    """A key the writer omits when absent, never sets to null.

    Journal payloads only: they are the SDK engine's own serialization,
    byte-faithful, with serde `skip_serializing_if` on optional fields -- the
    one place API.md section 0's "nothing is ever omitted" rule does not
    apply (API.md section 6.1). Everywhere else, absence stays a violation.
    """
    return ("opt", spec)


def ARR(spec):
    return ("arr", spec)


def OBJ(pairs):
    """Exact key set: a missing key and an extra key are both violations.

    A key whose spec is wrapped in `OPT` may be absent; when present it is
    checked against the wrapped spec.
    """
    return ("obj", pairs)


def ENUM(values):
    return ("enum", values)


def UNION(discriminator, options):
    return ("union", discriminator, options)


def check_shape(value, spec, path):
    if isinstance(spec, str):
        check_primitive(value, spec, path)
        return
    kind = spec[0]
    if kind == "null":
        if value is None:
            return
        check_shape(value, spec[1], path)
    elif kind == "opt":
        check_shape(value, spec[1], path)
    elif kind == "arr":
        if not isinstance(value, list):
            fail(path, "expected an array, got %s" % type_name(value))
            return
        for index, item in enumerate(value):
            check_shape(item, spec[1], "%s[%d]" % (path, index))
    elif kind == "obj":
        if not isinstance(value, dict):
            fail(path, "expected an object, got %s" % type_name(value))
            return
        expected = set(spec[1].keys())
        required = set(key for key, inner in spec[1].items()
                       if not (isinstance(inner, tuple) and inner[0] == "opt"))
        actual = set(value.keys())
        for key in sorted(required - actual):
            fail(path, "missing field %r" % key)
        for key in sorted(actual - expected):
            fail(path, "unexpected field %r (API.md does not declare it)" % key)
        for key in spec[1]:
            if key in value:
                check_shape(value[key], spec[1][key], "%s.%s" % (path, key))
    elif kind == "enum":
        if value not in spec[1]:
            fail(path, "%r is not one of %s" % (value, spec[1]))
    elif kind == "union":
        discriminator, options = spec[1], spec[2]
        if not isinstance(value, dict):
            fail(path, "expected an object, got %s" % type_name(value))
            return
        tag = value.get(discriminator)
        if tag not in options:
            fail(path, "%s=%r has no declared shape (expected one of %s)"
                 % (discriminator, tag, sorted(options)))
            return
        check_shape(value, options[tag], path)
    else:
        raise AssertionError("unknown spec %r" % (spec,))


def type_name(value):
    if value is None:
        return "null"
    return {bool: "bool", int: "int", float: "number", str: "string",
            list: "array", dict: "object"}.get(type(value), type(value).__name__)


def check_primitive(value, kind, path):
    if kind == "any":
        return
    if kind == "str":
        if not isinstance(value, str):
            fail(path, "expected a string, got %s" % type_name(value))
    elif kind == "bool":
        if not isinstance(value, bool):
            fail(path, "expected a bool, got %s" % type_name(value))
    elif kind == "int":
        if isinstance(value, bool) or not isinstance(value, int):
            fail(path, "expected an int, got %s" % type_name(value))
    elif kind == "num":
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            fail(path, "expected a number, got %s" % type_name(value))
    elif kind == "ts":
        if parse_ts(value) is None:
            fail(path, "%r is not YYYY-MM-DDTHH:MM:SSZ" % (value,))
    elif kind == "uuid":
        if not isinstance(value, str) or not UUID_RE.match(value):
            fail(path, "%r is not a lowercase hyphenated UUID" % (value,))
    elif kind == "gmailid":
        if not isinstance(value, str) or not GMAIL_ID_RE.match(value):
            fail(path, "%r is not a Gmail id (16 lowercase hex)" % (value,))
    elif kind == "mailboxid":
        if not isinstance(value, str) or not MAILBOX_ID_RE.match(value):
            fail(path, "%r is not a Gmail label id" % (value,))
    elif kind == "labelid":
        if not isinstance(value, str) or not LABEL_ID_RE.match(value):
            fail(path, "%r is not a Gmail label id" % (value,))
    elif kind == "attid":
        if not isinstance(value, str) or not ATTACHMENT_ID_RE.match(value):
            fail(path, "%r is not an opaque Gmail attachment id" % (value,))
    elif kind == "cursor":
        if not isinstance(value, str) or not CURSOR_RE.match(value):
            fail(path, "%r is not an opaque base64url cursor" % (value,))
    elif kind == "email":
        if not isinstance(value, str) or "@" not in value:
            fail(path, "%r is not an address" % (value,))
    elif kind == "hhmm":
        if not isinstance(value, str) or not HHMM_RE.match(value):
            fail(path, "%r is not HH:MM" % (value,))
    elif kind == "sha256":
        if not isinstance(value, str) or not SHA256_RE.match(value):
            fail(path, "%r is not sha256:<64 hex>" % (value,))
    elif kind == "date":
        if not isinstance(value, str) or not DATE_RE.match(value):
            fail(path, "%r is not YYYY-MM-DD" % (value,))
    else:
        raise AssertionError("unknown primitive %r" % kind)


# ==========================================================================
# 4. The shapes, transcribed from API.md
# ==========================================================================

RUN_STATUSES = ["queued", "running", "pending_approval", "waiting", "done",
                "failed", "expired", "skipped"]
TRIGGER_KINDS = ["mail", "schedule", "manual"]
V1_TOOLS = ["search_mail", "read_thread", "write_note", "draft_reply"]
ERROR_CODES = ["bad_request", "unauthorized", "forbidden", "not_found", "conflict",
               "token_consumed", "gone", "approval_expired", "payload_too_large",
               "rate_limited", "needs_reauth", "upstream_unavailable", "internal"]

# API.md section 2 -- one row of a thread list. Shared by /threads, /search and
# the `results` SSE frame.
THREAD_ROW = OBJ({
    "id": "gmailid",
    "subject": "str",                 # "" when the mail has none, never null
    "snippet": "str",                 # <= 200 chars, whitespace-collapsed
    "from_name": "str",               # "" when the sender had no display name
    "from_email": "str",
    "ts": "ts",
    "unread": "bool",
    "msg_count": "int",
    "agent_note": NUL("str"),
})

ATTACHMENT = OBJ({
    "id": "attid",
    "name": "str",
    "mime": "str",
    "size": "int",
    "inline": "bool",
})

MESSAGE = OBJ({
    # There is no `id` field on a message. `gmail_id` is the identity; the
    # database's bigint primary key is internal and never appears on the wire.
    "gmail_id": "gmailid",
    "from_name": "str",
    "from_email": "str",
    "to": ARR("email"),               # may be empty (undisclosed-recipients)
    "cc": ARR("email"),
    "ts": "ts",
    "body_text": "str",               # never null; may be ""
    "body_html": NUL("str"),          # null when NO text/html part exists
    "label_ids": ARR("labelid"),
    "attachments": ARR(ATTACHMENT),
})

AGENT_CARD = OBJ({
    "run_id": "uuid",
    "agent_name": "str",
    "status": ENUM(RUN_STATUSES),
    "summary": "str",
    "feed_item_id": NUL("uuid"),
})

THREAD_DETAIL = OBJ({
    "id": "gmailid",
    "subject": "str",
    "mailbox_name": "str",
    "account_email": "email",
    "messages": ARR(MESSAGE),
    "agent_cards": ARR(AGENT_CARD),
    # True when the cache holds only part of the conversation and Gmail could
    # not be reached for the rest. The window is a cache, not the mailbox
    # (docs/SEARCH.md); a fragment must never be served as the whole thread.
    "partial": "bool",
})

NOTE_ROW = OBJ({
    "id": "uuid",
    "run_id": NUL("uuid"),
    "thread_id": NUL("gmailid"),
    "title": "str",
    "agent_name": NUL("str"),
    "unread": "bool",
    "updated_at": "ts",
})

NOTE_DETAIL = OBJ({
    "id": "uuid",
    "title": "str",
    "body_md": "str",
    "run_id": NUL("uuid"),
    "thread_id": NUL("gmailid"),
    "agent_name": NUL("str"),
    "unread": "bool",
    "updated_at": "ts",
})

DRAFT = OBJ({
    "id": "uuid",
    "run_id": NUL("uuid"),
    "thread_id": NUL("gmailid"),
    "to": ARR("email"),
    "subject": "str",
    "body_text": "str",
    "created_at": "ts",
    "updated_at": "ts",
})

SCHEDULE = OBJ({
    "freq": ENUM(["day", "week", "month"]),
    "interval": "int",
    "byweekday": ARR(ENUM(["mon", "tue", "wed", "thu", "fri", "sat", "sun"])),
    "bymonthday": NUL("int"),
    "at": "hhmm",
    "tz": "str",
    "ends": OBJ({
        "kind": ENUM(["never", "on", "after"]),
        "date": NUL("date"),
        "count": NUL("int"),
    }),
    "runs_done": "int",
})

SPEC = OBJ({
    "version": "int",
    "trigger": OBJ({
        "kind": ENUM(TRIGGER_KINDS),
        "filters": OBJ({
            "from_domains": ARR("str"),
            "from_contains": ARR("str"),
            "subject_contains": ARR("str"),
            "body_contains": ARR("str"),
            "label_ids": ARR("labelid"),
            "has_attachment": NUL("bool"),
            "newer_than_days": "int",
        }),
        "semantic": NUL("str"),
    }),
    "instruction": "str",
    "tools": ARR(ENUM(V1_TOOLS)),
    "output": OBJ({
        "kind": ENUM(["note", "draft", "none"]),
        "title_template": NUL("str"),
    }),
})

AGENT_LIST_ROW_FIELDS = {
    "id": "uuid",
    "name": "str",
    "nl_definition": "str",
    "status": ENUM(["draft", "published", "paused"]),
    "trigger_summary": "str",
    "schedule": NUL(SCHEDULE),
    "last_run_at": NUL("ts"),
    "approval_required": "bool",
}
AGENT_ROW = OBJ(dict(AGENT_LIST_ROW_FIELDS))

_agent_full = dict(AGENT_LIST_ROW_FIELDS)
_agent_full.update({
    "allowed_tools": ARR(ENUM(V1_TOOLS)),
    "compile_error": NUL("str"),
    # The three the builder underlines. Derived from `spec` at compile time,
    # so null exactly when `spec` is. Full object only -- never on a list row.
    "when_span": NUL("str"),
    "do_span": NUL("str"),
    "trailing": NUL("str"),
    "spec": NUL(SPEC),
})
AGENT_FULL = OBJ(_agent_full)

ASK_REQUEST = OBJ({
    "query": "str",
    "thread_id": NUL("gmailid"),
    # Forces the route instead of letting the server classify. null is normal.
    "route_hint": NUL(ENUM(["answer", "results", "agent_draft"])),
})

RUN_ROW = OBJ({
    "id": "uuid",
    "agent_id": "uuid",
    "agent_name": "str",
    "trigger_kind": ENUM(TRIGGER_KINDS),
    "status": ENUM(RUN_STATUSES),
    "summary": NUL("str"),
    "created_at": "ts",
    "updated_at": "ts",
})

# API.md section 6.1, one row per journal kind. The vocabulary and every
# payload shape are the SDK engine's own, transcribed from
# the `durable-agent` crate’s `journal.rs` -- `run_journal` is written
# ONLY by the engine, through the host's Journal driver, so a host-invented
# kind or field here is a contract violation. The key set is exact; `OPT`
# marks the fields serde omits when absent, which is the one licensed
# exception to "nothing is ever omitted".

# The statuses `run_ended` can record: `RunStatus::is_terminal`. `queued` and
# `running` are host states a journal never carries.
TERMINAL_RUN_STATUSES = ["done", "failed", "skipped", "expired"]

TOKEN_USAGE = OBJ({"input_tokens": "int", "output_tokens": "int"})

# `durable_agent::ToolCall`, as the engine journals it: ids already
# normalised, `context` absent (it is attached at dispatch, after journaling).
TOOL_CALL = OBJ({"id": "str", "name": ENUM(V1_TOOLS), "arguments": "any"})

# `durable_agent::Message`, tagged by `role`.
SDK_MESSAGE = UNION("role", {
    "system": OBJ({"role": ENUM(["system"]), "text": "str"}),
    "user": OBJ({"role": ENUM(["user"]), "text": "str"}),
    "assistant": OBJ({"role": ENUM(["assistant"]), "text": OPT("str"),
                      "tool_calls": OPT(ARR(TOOL_CALL))}),
    "tool": OBJ({"role": ENUM(["tool"]), "call_id": "str", "name": "str",
                 "result": "any", "is_error": OPT("bool")}),
})

# `durable_agent::FailureReason`, internally tagged on `cap`.
FAILURE_REASON = UNION("cap", {
    "step_cap_exceeded": OBJ({"cap": ENUM(["step_cap_exceeded"]),
                              "limit": "int", "taken": "int"}),
    "token_budget_exceeded": OBJ({"cap": ENUM(["token_budget_exceeded"]),
                                  "limit": "int", "spent": "int"}),
    "ambiguous_effect": OBJ({"cap": ENUM(["ambiguous_effect"]),
                             "tool": ENUM(V1_TOOLS), "step_seq": "int",
                             "effect_id": "uuid"}),
    "cancelled": OBJ({"cap": ENUM(["cancelled"]), "detail": "str"}),
})

JOURNAL_PAYLOADS = {
    "run_started": OBJ({
        "input": OBJ({"system": OPT("str"), "messages": ARR(SDK_MESSAGE)}),
        "journal_format": "int",
    }),
    # No per-entry model name: post-v1 SDK field, not a second vocabulary.
    "model_response": OBJ({
        "turn": "int",
        "text": OPT("str"),
        "tool_calls": OPT(ARR(TOOL_CALL)),
        "stop_reason": ENUM(["end_turn", "tool_use", "max_tokens",
                             "stop_sequence", "content_filter"]),
        "usage": TOKEN_USAGE,
    }),
    # `step_seq` points at the entry that OPENED this step -- the `step_started`
    # itself for an ungated call, the `approval_requested` for a gated one.
    # Correlating by "nearest preceding open with the same tool" is ambiguous
    # the moment an agent calls a tool twice, which is ordinary.
    "step_started": OBJ({
        "step_seq": "int",
        "call_id": "str",
        "tool": ENUM(V1_TOOLS),
        "args": "any",
        "args_hash": "sha256",
        "effect_id": "uuid",
        "attempt": "int",
        "opened_at": "ts",
        "tool_fingerprint": OPT("sha256"),
        "replay_policy": ENUM(["retry", "halt"]),
    }),
    # A tool failure is `is_error: true`; there is no separate failure kind.
    "step_done": OBJ({
        "step_seq": "int",
        "call_id": "str",
        "tool": ENUM(V1_TOOLS),
        "result": "any",
        "is_error": "bool",
        "truncated": "bool",
        "wake_at": OPT("ts"),
    }),
    # No feed_item_id and no summary: the feed item is a host row raised from
    # the engine's ApprovalRequest outcome, and the journal never names it.
    # `expires_at` is FORBIDDEN, not optional: NADE runs `approval_ttl = None`
    # (the host sweep and the approve tx's 410 own expiry), so a fixture that
    # carries it is describing an engine configured wrong. The SDK type has the
    # field; this file checks NADE's fixtures, which are narrower than the SDK
    # grammar on purpose (four tools, no replay attempts) — a live journal may
    # legally contain what a fixture must not.
    "approval_requested": OBJ({
        "step_seq": "int",
        "call_id": "str",
        "tool": ENUM(V1_TOOLS),
        "args": "any",
        "args_hash": "sha256",
        "effect_id": "uuid",
        "requested_at": "ts",
        "tool_fingerprint": OPT("sha256"),
        "replay_policy": ENUM(["retry", "halt"]),
    }),
    "approval_resolved": OBJ({
        "step_seq": "int",
        "decision": ENUM(["approve", "skip", "expire"]),
        "resolved_at": "ts",
        "reason": OPT(ENUM(["ttl_expired"])),
    }),
    "run_waiting": OBJ({"step_seq": "int", "wake_at": "ts"}),
    "run_woken": OBJ({"step_seq": "int", "woken_at": "ts"}),
    "cap_breached": OBJ({"reason": FAILURE_REASON}),
    "run_ended": OBJ({
        "status": ENUM(TERMINAL_RUN_STATUSES),
        "output": OPT("str"),
        "reason": OPT(FAILURE_REASON),
        "steps": "int",
        "usage": TOKEN_USAGE,
    }),
}

JOURNAL_ENTRY = OBJ({
    "seq": "int",
    "kind": ENUM(sorted(JOURNAL_PAYLOADS.keys())),
    "payload": "any",                 # checked per kind in check_journal
    "created_at": "ts",
})

RUN_DETAIL = OBJ({
    "id": "uuid",
    "agent_id": "uuid",
    "agent_name": "str",
    "status": ENUM(RUN_STATUSES),
    "trigger_kind": ENUM(TRIGGER_KINDS),
    "trigger_ref": NUL("str"),
    "error": NUL("str"),
    "journal": ARR(JOURNAL_ENTRY),
})

# API.md section 7.1 -- `data`, typed by action.
FEED_DATA = UNION("action", {
    "write_note": OBJ({
        "action": ENUM(["write_note"]),
        "action_label": "str",
        "note_title": "str",
        "note_id": "uuid",
        "thread_id": NUL("gmailid"),
    }),
    "draft_reply": OBJ({
        "action": ENUM(["draft_reply"]),
        "action_label": "str",
        "draft_id": "uuid",
        # Nullable, because the tool's own `thread_id` is optional
        # ("Optional: the thread this draft replies to") and the card is raised
        # from `approval_requested` -- before any dispatch could have supplied
        # one. API.md section 3 already types a draft row's thread_id the same
        # way; this was the outlier.
        "thread_id": NUL("gmailid"),
        "to": ARR("email"),
        "subject": "str",
        "never_messaged": "bool",
    }),
    "none": OBJ({
        "action": ENUM(["none"]),
        "note_id": NUL("uuid"),
        # An agent with `approval_required = false` and `draft_reply` in its
        # tools writes its draft with no card to approve, and the `info` card
        # that follows had no field to name what it made.
        "draft_id": NUL("uuid"),
        "thread_id": NUL("gmailid"),
    }),
})

FEED_ITEM = OBJ({
    "id": "uuid",
    "kind": ENUM(["approval", "info"]),
    "title": "str",
    "body": "str",
    "status": ENUM(["new", "resolved", "skipped", "expired"]),
    "run_id": NUL("uuid"),
    "actions": ARR(ENUM(["approve", "edit", "skip"])),
    "approval_token": NUL("uuid"),
    "approval_expires_at": NUL("ts"),
    "resolved_note": NUL("str"),
    "data": NUL(FEED_DATA),
    "created_at": "ts",
})

ERROR_ENVELOPE = OBJ({
    "error": OBJ({"code": ENUM(ERROR_CODES), "message": "str"}),
})

# Endpoint -> shape. Also the definitive file list: a file here that is not on
# disk, or on disk and not here, is a failure.
FIXTURE_SHAPES = {
    # 1. Auth and identity
    "pair.json": OBJ({"token": "str"}),
    "gmail_link.json": OBJ({"url": "str", "expires_at": "ts"}),
    "me.json": OBJ({"email": "email", "status": ENUM(["ok", "needs_reauth"])}),
    "me_needs_reauth.json": OBJ({"email": "email",
                                 "status": ENUM(["ok", "needs_reauth"])}),
    # 2. Mail
    "mailboxes.json": OBJ({"mailboxes": ARR(OBJ({
        "id": "mailboxid",
        "name": "str",
        "kind": ENUM(["system", "user"]),
        "unread": "int",
        "total": "int",
    }))}),
    "threads.json": OBJ({"threads": ARR(THREAD_ROW), "next_cursor": NUL("cursor")}),
    "threads_last_page.json": OBJ({"threads": ARR(THREAD_ROW),
                                   "next_cursor": NUL("cursor")}),
    "threads_empty.json": OBJ({"threads": ARR(THREAD_ROW),
                               "next_cursor": NUL("cursor")}),
    "thread.json": THREAD_DETAIL,
    "thread_html_only.json": THREAD_DETAIL,
    "thread_partial.json": THREAD_DETAIL,
    "search.json": OBJ({"threads": ARR(THREAD_ROW), "next_cursor": NUL("cursor")}),
    "search_empty.json": OBJ({"threads": ARR(THREAD_ROW),
                              "next_cursor": NUL("cursor")}),
    # 3. Notes and drafts
    "notes.json": OBJ({"notes": ARR(NOTE_ROW), "next_cursor": NUL("cursor")}),
    "notes_empty.json": OBJ({"notes": ARR(NOTE_ROW), "next_cursor": NUL("cursor")}),
    "note.json": NOTE_DETAIL,
    "drafts.json": OBJ({"drafts": ARR(DRAFT), "next_cursor": NUL("cursor")}),
    "drafts_empty.json": OBJ({"drafts": ARR(DRAFT), "next_cursor": NUL("cursor")}),
    "draft.json": DRAFT,
    # 4. Ask (request bodies)
    "ask_request.json": ASK_REQUEST,
    "ask_request_route_hint.json": ASK_REQUEST,
    # 5. Agents
    "agents.json": OBJ({"agents": ARR(AGENT_ROW)}),
    "agents_empty.json": OBJ({"agents": ARR(AGENT_ROW)}),
    "agent.json": AGENT_FULL,
    "agent_scheduled.json": AGENT_FULL,
    "agent_draft.json": AGENT_FULL,
    "agent_compile_failed.json": AGENT_FULL,
    # 6. Runs
    "runs.json": OBJ({"runs": ARR(RUN_ROW), "next_cursor": NUL("cursor")}),
    "runs_empty.json": OBJ({"runs": ARR(RUN_ROW), "next_cursor": NUL("cursor")}),
    "run.json": RUN_DETAIL,
    "run_done.json": RUN_DETAIL,
    "run_failed.json": RUN_DETAIL,
    "run_pending_draft.json": RUN_DETAIL,
    "run_skipped.json": RUN_DETAIL,
    "run_expired.json": RUN_DETAIL,
    # 7. Feed
    "feed.json": OBJ({"items": ARR(FEED_ITEM), "next_cursor": NUL("cursor"),
                      "new_count": "int"}),
    "feed_empty.json": OBJ({"items": ARR(FEED_ITEM), "next_cursor": NUL("cursor"),
                            "new_count": "int"}),
    "feed_item.json": FEED_ITEM,
    "feed_item_info.json": FEED_ITEM,
    "feed_item_editable.json": FEED_ITEM,
    "approve.json": OBJ({"run_id": "uuid", "status": ENUM(RUN_STATUSES)}),
    "skip.json": OBJ({"status": ENUM(["skipped"])}),
    "seen.json": OBJ({"new_count": "int"}),
    # 8. Settings
    "settings.json": OBJ({
        "approval_required_default": "bool",
        "sync_window_days": "int",
        "spend_ceiling_usd_per_day": "num",
        "app_version": "str",
        "disclosure": "str",
    }),
    # 10. Health
    "healthz.json": OBJ({"status": "str", "db": ENUM(["ok", "down"]),
                         "version": "str"}),
    "healthz_db_down.json": OBJ({"status": "str", "db": ENUM(["ok", "down"]),
                                 "version": "str"}),
}
for _code in ERROR_CODES:
    FIXTURE_SHAPES["error_%s.json" % _code] = ERROR_ENVELOPE

SSE_FIXTURES = ["ask_answer.sse", "ask_results.sse", "ask_agent_draft.sse",
                "ask_error.sse"]

# `GET /agents` does not carry `allowed_tools`, so the tool constraint can only
# be checked against a full agent object. Every agent that has a run needs one.
AGENT_FULL_FIXTURES = ["agent.json", "agent_scheduled.json", "agent_draft.json",
                       "agent_compile_failed.json"]

RUN_DETAIL_FIXTURES = ["run.json", "run_done.json", "run_failed.json",
                       "run_pending_draft.json", "run_skipped.json",
                       "run_expired.json"]

# API.md section 5: `GET /agents` is ordered oldest first by `created_at`.
# `created_at` is not on the wire, so the order is transcribed here rather than
# inferred from the file it is checking.
AGENTS_OLDEST_FIRST = ["Job Search Tracker", "Morning To-Do", "Reply Drafter",
                       "Flight Watcher"]

# API.md section 0: a response carries `next_cursor` if and only if the endpoint
# is paginated. Bounded collections must have no cursor field at all, because
# inventing one implies a page boundary that will never exist.
PAGINATED = {
    "threads.json", "threads_last_page.json", "threads_empty.json",
    "search.json", "search_empty.json",
    "notes.json", "notes_empty.json",
    "drafts.json", "drafts_empty.json",
    "feed.json", "feed_empty.json",
    "runs.json", "runs_empty.json",
}
UNPAGINATED = {"mailboxes.json", "agents.json", "agents_empty.json", "settings.json"}

# API.md section 2, in this order, with these display names. Everything else
# Gmail calls a system label is a flag or a place v1 does not sync.
SYSTEM_MAILBOXES = [
    ("INBOX", "Inbox"),
    ("CATEGORY_PERSONAL", "Primary"),
    ("CATEGORY_UPDATES", "Updates"),
    ("CATEGORY_SOCIAL", "Social"),
    ("CATEGORY_PROMOTIONS", "Promotions"),
    ("CATEGORY_FORUMS", "Forums"),
    ("STARRED", "Starred"),
    ("SENT", "Sent"),
]
NEVER_A_MAILBOX = ["UNREAD", "IMPORTANT", "CHAT", "YELLOW_STAR", "SPAM", "TRASH",
                   "DRAFT"]

# v1 takes no outbound action, and the UI is not allowed to imply otherwise.
OUTBOUND_VERBS = ["send", "sends", "sending", "sent", "forward", "forwards",
                  "forwarding", "forwarded", "archive", "archives", "archiving",
                  "archived", "delete", "deletes", "deleting", "deleted", "trash",
                  "reply", "replies", "email", "emails", "mail", "post", "posts",
                  "publish", "share", "shares", "unsubscribe", "rsvp", "accept",
                  "decline", "book", "pay", "schedule"]

ACCOUNT_EMAIL = "jatinsethi98@gmail.com"
APPROVAL_TTL_DAYS = 7


# ==========================================================================
# 5. Load
# ==========================================================================

def load_all():
    raw = {}
    docs = {}
    on_disk = set()
    for name in sorted(os.listdir(HERE)):
        if not (name.endswith(".json") or name.endswith(".sse")):
            continue
        on_disk.add(name)
        with open(os.path.join(HERE, name), "rb") as handle:
            data = handle.read()
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError as error:
            fail(name, "not valid UTF-8: %s" % error)
            continue
        if "\r" in text:
            fail(name, "contains a carriage return; the wire is LF only")
        raw[name] = text
        if name.endswith(".json"):
            try:
                docs[name] = json.loads(text)
            except ValueError as error:
                fail(name, "is not valid JSON: %s" % error)

    expected = set(FIXTURE_SHAPES) | set(SSE_FIXTURES)
    for name in sorted(expected - on_disk):
        fail(name, "is missing; API.md section 11 requires it")
    for name in sorted(on_disk - expected):
        fail(name, "is not a fixture API.md declares -- delete it or add it to "
                   "FIXTURE_SHAPES")
    return raw, docs


def check_canonical_formatting(raw, docs):
    """The files are generated. Re-serialising must reproduce them byte for byte.

    This is what makes "never hand-edit the JSON" enforceable rather than
    aspirational: any manual reflow, key reorder or stray whitespace fails here.
    """
    for name in sorted(docs):
        canonical = json.dumps(docs[name], indent=2, ensure_ascii=False) + "\n"
        if raw[name] != canonical:
            fail(name, "is not canonically formatted (2-space indent, UTF-8 "
                       "verbatim, one trailing newline). Run generate.py; do not "
                       "hand-edit.")


# ==========================================================================
# 6. Shape and convention checks
# ==========================================================================

def check_shapes(docs):
    for name in sorted(docs):
        if name in FIXTURE_SHAPES:
            check_shape(docs[name], FIXTURE_SHAPES[name], name)


def check_pagination(docs):
    for name in sorted(docs):
        body = docs[name]
        if not isinstance(body, dict):
            continue
        if name in PAGINATED:
            if "next_cursor" not in body:
                fail(name, "is a paginated endpoint and must carry next_cursor")
            # An empty collection returns an empty array AND next_cursor: null.
            for key in ("threads", "notes", "drafts", "items", "runs"):
                if body.get(key) == [] and body.get("next_cursor") is not None:
                    fail(name, "is empty, so next_cursor must be null")
        elif name in UNPAGINATED and "next_cursor" in body:
            fail(name, "is a bounded collection and must have no cursor field at "
                       "all (API.md section 0)")


def check_formats(docs):
    where = "pair.json"
    token = docs.get("pair.json", {}).get("token")
    if not isinstance(token, str) or not BEARER_RE.match(token):
        fail(where, "token must be 'nade_' + 64 lowercase hex")
    elif len(token) != 69:
        fail(where, "token must be 69 characters, got %d" % len(token))

    for name in ("me.json", "me_needs_reauth.json"):
        if docs.get(name, {}).get("email") != ACCOUNT_EMAIL:
            fail(name, "email must be the single account, %s" % ACCOUNT_EMAIL)
    if docs.get("me.json", {}).get("status") != "ok":
        fail("me.json", "must show status ok")
    if docs.get("me_needs_reauth.json", {}).get("status") != "needs_reauth":
        fail("me_needs_reauth.json", "must show status needs_reauth")

    if docs.get("healthz.json", {}).get("db") != "ok":
        fail("healthz.json", "must report db ok")
    if docs.get("healthz_db_down.json", {}).get("db") != "down":
        fail("healthz_db_down.json", "must report db down")


def check_errors(docs):
    for code in ERROR_CODES:
        name = "error_%s.json" % code
        body = docs.get(name)
        if not isinstance(body, dict):
            continue
        envelope = body.get("error", {})
        if envelope.get("code") != code:
            fail(name, "code is %r but the filename says %r"
                 % (envelope.get("code"), code))
        message = envelope.get("message", "")
        if not message or not message[0].isupper() or not message.rstrip().endswith("."):
            fail(name, "message must be a complete end-user sentence: %r" % message)
        for marker in ("Error:", "panic", "::", "unwrap", "/src/", "sqlx", "Traceback"):
            if marker in message:
                fail(name, "message leaks internals (%r)" % marker)


def check_mailboxes(docs):
    boxes = docs.get("mailboxes.json", {}).get("mailboxes")
    if not isinstance(boxes, list):
        return
    where = "mailboxes.json"
    system = [b for b in boxes if b.get("kind") == "system"]
    user = [b for b in boxes if b.get("kind") == "user"]

    got = [(b["id"], b["name"]) for b in system]
    if got != SYSTEM_MAILBOXES:
        fail(where, "system labels must be exactly %s, got %s"
             % (SYSTEM_MAILBOXES, got))
    # System first, then user: order in the array is the order on screen.
    if boxes[:len(system)] != system:
        fail(where, "system labels must come before user labels")

    for box in boxes:
        if box["id"] in NEVER_A_MAILBOX:
            fail(where, "%s is a Gmail flag, not a mailbox; it must be filtered out"
                 % box["id"])
        if box["name"].startswith("[Gmail]"):
            fail(where, "%r is one of Gmail's own internal views and must be hidden"
                 % box["name"])
        if box["unread"] < 0 or box["total"] < 0 or box["unread"] > box["total"]:
            fail(where, "%s has unread=%d total=%d"
                 % (box["id"], box["unread"], box["total"]))

    names = [b["name"] for b in user]
    if names != sorted(names):
        fail(where, "user labels must be ordered by name, got %s" % names)

    # Real user label ids come in two shapes -- `Label_24` and
    # `Label_1901000275082506782`. Both must be present so nothing downstream
    # assumes a length.
    widths = set(len(b["id"]) - len("Label_") for b in user)
    if not any(w <= 4 for w in widths) or not any(w >= 12 for w in widths):
        fail(where, "user label ids must cover both the short and the long form; "
                    "got id widths %s" % sorted(widths))


# ==========================================================================
# 7. The fixture world: every id, gathered once
# ==========================================================================

class World(object):
    def __init__(self, docs):
        self.docs = docs
        self.agents = {}
        self.runs = {}
        self.run_details = {}
        self.feed_items = {}
        self.notes = {}
        self.drafts = {}
        self.threads = {}
        self.messages = {}
        self.attachments = set()
        self.mailbox_names = {}

        self.full_agents = set()
        for agent in docs.get("agents.json", {}).get("agents", []):
            self.agents[agent["id"]] = agent
        for name in AGENT_FULL_FIXTURES:
            agent = docs.get(name)
            if isinstance(agent, dict) and "id" in agent:
                self.agents.setdefault(agent["id"], agent)
                self.agents[agent["id"]] = dict(self.agents[agent["id"]], **agent)
                self.full_agents.add(agent["id"])

        for run in docs.get("runs.json", {}).get("runs", []):
            self.runs[run["id"]] = run
        for name in RUN_DETAIL_FIXTURES:
            detail = docs.get(name)
            if isinstance(detail, dict) and "id" in detail:
                self.run_details[detail["id"]] = (name, detail)

        for item in docs.get("feed.json", {}).get("items", []):
            self.feed_items[item["id"]] = item
        for name in ("feed_item.json", "feed_item_info.json"):
            item = docs.get(name)
            if isinstance(item, dict) and "id" in item:
                self.feed_items.setdefault(item["id"], item)

        for note in docs.get("notes.json", {}).get("notes", []):
            self.notes[note["id"]] = note
        detail = docs.get("note.json")
        if isinstance(detail, dict) and "id" in detail:
            self.notes.setdefault(detail["id"], detail)

        for draft in docs.get("drafts.json", {}).get("drafts", []):
            self.drafts[draft["id"]] = draft
        detail = docs.get("draft.json")
        if isinstance(detail, dict) and "id" in detail:
            self.drafts.setdefault(detail["id"], detail)

        for name in ("threads.json", "threads_last_page.json", "search.json"):
            for row in docs.get(name, {}).get("threads", []):
                self.threads.setdefault(row["id"], row)
        for name in ("thread.json", "thread_html_only.json", "thread_partial.json"):
            detail = docs.get(name)
            if isinstance(detail, dict) and "id" in detail:
                self.threads.setdefault(detail["id"], None)
                for message in detail.get("messages", []):
                    self.messages[message["gmail_id"]] = (detail["id"], message)
                    for att in message.get("attachments", []):
                        self.attachments.add((message["gmail_id"], att["id"]))

        # A thread's id is the id of its first message, so every thread id is
        # also a valid message id even where no detail fixture spells it out.
        for thread_id in list(self.threads):
            self.messages.setdefault(thread_id, (thread_id, None))

        for box in docs.get("mailboxes.json", {}).get("mailboxes", []):
            self.mailbox_names[box["id"]] = box["name"]

    def agent_of_run(self, run_id):
        run = self.runs.get(run_id)
        if run is None:
            return None
        return self.agents.get(run["agent_id"])


def not_yet_created_effects(world):
    """Effect ids that legitimately resolve to no row, with the reason.

    An approval publishes `data.note_id` / `data.draft_id` *before* the effect
    exists -- that is the whole point of deriving the id rather than minting it
    after the fact, and it is what lets the client deep-link straight after
    approving. If the approval is never granted, the row is never written, so
    the id stays dangling forever and that is correct.

    Nothing is whitelisted by value: each id is recomputed from its run and the
    seq of the `approval_requested` entry, so a typo cannot hide in here.
    """
    allowed = {}
    for item in world.feed_items.values():
        if item["kind"] != "approval" or item["status"] == "resolved":
            continue
        run_id = item.get("run_id")
        run = world.runs.get(run_id)
        if run is None or run["status"] == "done":
            continue
        data = item.get("data") or {}
        eid = data.get("note_id") or data.get("draft_id")
        if eid is None:
            continue
        reason = ("run %s is %s, so the approved effect was never written"
                  % (run_id, run["status"]))
        allowed[eid] = (run_id, reason)
    return allowed


# ==========================================================================
# 8. Referential integrity
# ==========================================================================

def check_references(world):
    docs = world.docs
    pending = not_yet_created_effects(world)

    def known_note(value):
        return value in world.notes or value in pending

    def known_draft(value):
        return value in world.drafts or value in pending

    for name in sorted(docs):
        body = docs[name]
        for path, key, value in walk(body, name):
            if value is None:
                continue
            if key in ("run_id",) and isinstance(value, str):
                if value not in world.runs:
                    fail(path, "run_id %s is in no run list" % value)
            elif key == "agent_id" and isinstance(value, str):
                if value not in world.agents:
                    fail(path, "agent_id %s is in no agent list" % value)
            elif key == "feed_item_id" and isinstance(value, str):
                if value not in world.feed_items:
                    fail(path, "feed_item_id %s is in no feed" % value)
            elif key == "thread_id" and isinstance(value, str):
                if value not in world.threads:
                    fail(path, "thread_id %s appears in no thread list or detail"
                         % value)
            elif key == "gmail_id" and isinstance(value, str):
                if value not in world.messages:
                    fail(path, "gmail_id %s appears in no thread detail" % value)
            elif key == "note_id" and isinstance(value, str):
                if not known_note(value):
                    fail(path, "note_id %s is neither a note nor a pending effect"
                         % value)
            elif key == "draft_id" and isinstance(value, str):
                if not known_draft(value):
                    fail(path, "draft_id %s is neither a draft nor a pending effect"
                         % value)

    # Every pending id must be exactly what the protocol would compute.
    for eid, (run_id, reason) in sorted(pending.items()):
        detail = world.run_details.get(run_id)
        if detail is None:
            fail("feed", "%s is published as a pending effect of %s, but that run "
                         "has no detail fixture, so the id is corroborated by "
                         "nothing" % (eid, run_id))
            continue
        gate = [e for e in detail[1]["journal"] if e["kind"] == "approval_requested"]
        if not gate:
            fail("feed", "%s claims to be a pending effect of %s but that run "
                         "never requested approval" % (eid, run_id))
            continue
        expected = effect_id(run_id, gate[-1]["seq"])
        if eid != expected:
            fail("feed", "pending effect %s should be %s (%s)"
                 % (eid, expected, reason))

    # `cid:` URLs are rewritten server-side before the response is built, so
    # every attachment URL inside body_html must resolve.
    url_re = re.compile(r"/v1/messages/([0-9a-f]{16})/attachments/([A-Za-z0-9_-]+)")
    for name in ("thread.json", "thread_html_only.json", "thread_partial.json"):
        detail = docs.get(name)
        if not isinstance(detail, dict):
            continue
        for message in detail.get("messages", []):
            html = message.get("body_html") or ""
            for gmail_id, att_id in url_re.findall(html):
                if (gmail_id, att_id) not in world.attachments:
                    fail(name, "body_html links attachment %s on %s, which the "
                               "message does not carry" % (att_id, gmail_id))


def walk(node, path):
    """Yield (path, key, value) for every scalar and list-of-scalars in a doc."""
    if isinstance(node, dict):
        for key, value in node.items():
            child = "%s.%s" % (path, key)
            if isinstance(value, (dict, list)):
                yield from walk(value, child)
            else:
                yield child, key, value
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from walk(value, "%s[%d]" % (path, index))


# ==========================================================================
# 9. Journal protocol
# ==========================================================================

# How a run's wire status is read off its journal's last entry. `run_ended`
# carries the terminal status itself; the two parked states are implied by
# what the run is parked on. Anything else settles nothing and no fixture may
# end on it.
def status_from_journal(journal):
    last = journal[-1]
    if last["kind"] == "run_ended":
        payload = last["payload"]
        return payload.get("status") if isinstance(payload, dict) else None
    if last["kind"] == "approval_requested":
        return "pending_approval"
    if last["kind"] == "run_waiting":
        return "waiting"
    return None


# The four v1 tools' real `tool_fingerprint` values, restated here the same way
# every other shape in this file is: independently, so a generator that started
# varying them - or a fixture edited by hand - fails rather than agreeing with
# itself. Copied from the build:
#   cargo test -p nade-server print_tool_fingerprints -- --ignored --nocapture
# and held in place from the Rust side by
# `agents::tools::tests::the_real_fingerprints_match_the_contracts_golden_table`.
TOOL_FINGERPRINT_GOLDEN = {
    "draft_reply": "sha256:2f0eb322bcb3d5ad33f7498d8239d85f730015213c73b03c003bf058b9b46e2f",
    "read_thread": "sha256:572e586bdbf66002e29529db5deb0d792b29bb667e763ee2c8dc4a57f51370f8",
    "search_mail": "sha256:7c5acf3f12e2386148f59b7930d467608bee79dc6b340e4e2f6b6471578cc4f1",
    "write_note": "sha256:839c157ed05e2127a38a0ea05eeebb05cd524ee265bd2dfdd9aaa3655d64c203",
}


def check_tool_fingerprints(world):
    """Every journalled fingerprint is the real one for its tool.

    `tool_fingerprint` hashes a tool's *interface* - name, description, version
    and schema - and the engine checks it before every dispatch, including after
    an approval, so "a step opened under one build is never executed under
    another". A fixture carrying a value no build reproduces would describe a
    journal that could never replay.
    """
    seen = {}
    for _run_id, (name, detail) in sorted(world.run_details.items()):
        for entry in detail["journal"]:
            payload = entry.get("payload", {})
            fingerprint = payload.get("tool_fingerprint")
            if fingerprint is None:
                continue
            tool = payload.get("tool")
            expected = TOOL_FINGERPRINT_GOLDEN.get(tool)
            if expected is None:
                fail(name, "journal seq %s names tool %r, which has no known "
                           "fingerprint" % (entry.get("seq"), tool))
            elif fingerprint != expected:
                fail(name, "journal seq %s records %s's fingerprint as %s; the "
                           "real one is %s"
                     % (entry.get("seq"), tool, fingerprint, expected))
            seen.setdefault(tool, set()).add(fingerprint)

    for tool, values in sorted(seen.items()):
        if len(values) > 1:
            fail("journals", "%s has %d different fingerprints across the "
                             "corpus; one build has one" % (tool, len(values)))


def check_journals(world):
    for run_id, (name, detail) in sorted(world.run_details.items()):
        journal = detail["journal"]
        if not journal:
            fail(name, "a run always has at least a run_started entry")
            continue

        # seq starts at 1 and increases by one with no gaps.
        for index, entry in enumerate(journal):
            if entry["seq"] != index + 1:
                fail("%s.journal[%d]" % (name, index),
                     "seq is %d, expected %d (starts at 1, no gaps)"
                     % (entry["seq"], index + 1))

        # Payload shape, per kind, and no `seq` inside it: the outer seq is the
        # step identity and there is no second copy of it to fall out of sync.
        for index, entry in enumerate(journal):
            path = "%s.journal[%d](%s)" % (name, index, entry["kind"])
            shape = JOURNAL_PAYLOADS.get(entry["kind"])
            if shape is None:
                fail(path, "unknown journal kind")
                continue
            check_shape(entry["payload"], shape, path + ".payload")
            if isinstance(entry["payload"], dict) and "seq" in entry["payload"]:
                fail(path, "payload carries its own seq; the outer seq is the step "
                           "identity (API.md section 6.1)")

        # run_started is first, is the only one, and stamps the format this
        # build writes. The trigger is NOT here: trigger_kind and trigger_ref
        # are host columns on the run row -- the journal records the engine's
        # input, not the host's dispatch decision.
        if journal[0]["kind"] != "run_started":
            fail(name, "journal must open with run_started, got %s"
                 % journal[0]["kind"])
        else:
            payload = journal[0]["payload"]
            if isinstance(payload, dict) and payload.get("journal_format") != 1:
                fail(name, "run_started.journal_format is %r; this contract "
                           "pins format 1" % payload.get("journal_format"))
        for entry in journal[1:]:
            if entry["kind"] == "run_started":
                fail(name, "run_started is recorded once, at seq 1; seq %d "
                           "repeats it" % entry["seq"])

        # run_ended is recorded once and nothing follows it.
        ended = [e["seq"] for e in journal if e["kind"] == "run_ended"]
        if len(ended) > 1:
            fail(name, "run_ended is recorded once, got seqs %s" % ended)
        elif ended and ended[0] != journal[-1]["seq"]:
            fail(name, "%d entr(ies) follow run_ended; it is always last"
                 % (journal[-1]["seq"] - ended[0]))

        # Timestamps never go backwards.
        previous = None
        for index, entry in enumerate(journal):
            current = parse_ts(entry["created_at"])
            if previous is not None and current is not None and current < previous:
                fail("%s.journal[%d]" % (name, index),
                     "created_at %s goes backwards" % entry["created_at"])
            previous = current

        check_step_pairing(name, run_id, journal)
        check_effect_ids(name, run_id, journal)
        check_run_status(name, detail, journal, world)


def check_step_pairing(name, run_id, journal):
    """Steps correlate by `step_seq`, not by position.

    `step_seq` names the entry that opened the step: its own seq for an
    ungated `step_started` and for every `approval_requested` (a gate opens
    its own step), or the earlier `approval_requested` for the `step_started`
    that executes an approved gate. Every started step is closed exactly once,
    by a `step_done` carrying the same `step_seq` at a later seq -- a failed
    tool closes with `is_error: true`; there is no separate failure kind. An
    approval is resolved at most once, and its step executes only on
    `decision: "approve"`.
    """
    by_seq = dict((e["seq"], e) for e in journal)
    started = {}                      # step_seq -> the step_started entry
    closed = {}                       # step_seq -> the closing step_done
    resolved = {}                     # step_seq -> the approval_resolved entry

    def payload_of(entry, *keys):
        # A malformed payload was already reported by the shape check; skip it
        # rather than crashing, so the rest of the report still arrives.
        if not isinstance(entry["payload"], dict):
            return None
        if any(key not in entry["payload"] for key in keys):
            return None
        step_seq = entry["payload"].get("step_seq")
        if not isinstance(step_seq, int) or isinstance(step_seq, bool):
            return None
        return entry["payload"]

    for entry in journal:
        kind = entry["kind"]
        if kind == "approval_requested":
            payload = payload_of(entry, "step_seq", "tool")
            if payload is None:
                continue
            # An approval opens its own step; its step_seq is its own seq.
            if payload["step_seq"] != entry["seq"]:
                fail(name, "seq %d (approval_requested) has step_seq %d; a "
                           "gate opens its own step" % (entry["seq"],
                                                        payload["step_seq"]))
            continue

        if kind == "approval_resolved":
            payload = payload_of(entry, "step_seq", "decision")
            if payload is None:
                continue
            step_seq = payload["step_seq"]
            opener = by_seq.get(step_seq)
            if opener is None or opener["kind"] != "approval_requested":
                fail(name, "seq %d resolves step %d, which is not an "
                           "approval_requested" % (entry["seq"], step_seq))
                continue
            if step_seq in resolved:
                fail(name, "step %d is resolved twice (seq %d and seq %d)"
                     % (step_seq, resolved[step_seq]["seq"], entry["seq"]))
            resolved[step_seq] = entry
            continue

        if kind not in ("step_started", "step_done"):
            continue
        payload = payload_of(entry, "step_seq", "tool")
        if payload is None:
            continue
        step_seq = payload["step_seq"]
        opener = by_seq.get(step_seq)
        if opener is None:
            fail(name, "seq %d has step_seq %d, which is not an entry in this run"
                 % (entry["seq"], step_seq))
            continue
        if step_seq > entry["seq"]:
            fail(name, "seq %d has step_seq %d, which is later than itself"
                 % (entry["seq"], step_seq))
            continue

        if kind == "step_started":
            gate = None
            if step_seq == entry["seq"]:
                pass                  # ungated: the step opens itself
            elif opener["kind"] != "approval_requested":
                fail(name, "seq %d has step_seq %d, which is a %s; a step is "
                           "opened by its own step_started or by an "
                           "approval_requested"
                     % (entry["seq"], step_seq, opener["kind"]))
            else:
                gate = opener["payload"] if isinstance(opener["payload"], dict) else {}
            if gate is not None:
                # The executing entry re-records the gate's facts verbatim: a
                # step never executes as anything but what the human was shown.
                for field in ("tool", "call_id", "args", "args_hash", "effect_id"):
                    if payload.get(field) != gate.get(field):
                        fail(name, "seq %d executes gate %d but its %s differs "
                                   "from the approval's"
                             % (entry["seq"], step_seq, field))
                if payload.get("opened_at") != gate.get("requested_at"):
                    fail(name, "seq %d opened_at %r; a gated step opens when "
                               "the approval was requested (%r)"
                         % (entry["seq"], payload.get("opened_at"),
                            gate.get("requested_at")))
                if step_seq not in resolved:
                    fail(name, "seq %d executes gate %d before any "
                               "approval_resolved" % (entry["seq"], step_seq))
                elif resolved[step_seq]["payload"].get("decision") != "approve":
                    fail(name, "seq %d executes gate %d whose decision was %r"
                         % (entry["seq"], step_seq,
                            resolved[step_seq]["payload"].get("decision")))
            # journal.rs: no step claims to have opened after the entry
            # recording it was made.
            opened = parse_ts(payload.get("opened_at", ""))
            created = parse_ts(entry["created_at"])
            if opened and created and opened > created:
                fail(name, "seq %d claims to have opened at %s, after the entry "
                           "recording it (%s)"
                     % (entry["seq"], payload["opened_at"], entry["created_at"]))
            if step_seq in started:
                fail(name, "step %d is started twice (seq %d and seq %d)"
                     % (step_seq, started[step_seq]["seq"], entry["seq"]))
            started[step_seq] = entry
        else:
            if step_seq not in started:
                fail(name, "seq %d closes step %d, which was never started"
                     % (entry["seq"], step_seq))
                continue
            if step_seq in closed:
                fail(name, "step %d is closed twice (seq %d and seq %d)"
                     % (step_seq, closed[step_seq]["seq"], entry["seq"]))
            if entry["seq"] <= started[step_seq]["seq"]:
                fail(name, "seq %d must close step %d at a *later* seq"
                     % (entry["seq"], step_seq))
            for field in ("tool", "call_id"):
                if payload.get(field) != started[step_seq]["payload"].get(field):
                    fail(name, "seq %d closes step %d but its %s is %r, not %r"
                         % (entry["seq"], step_seq, field, payload.get(field),
                            started[step_seq]["payload"].get(field)))
            closed[step_seq] = entry

    for step_seq, entry in sorted(started.items()):
        if step_seq not in closed:
            fail(name, "step %d (%s, started at seq %d) was never closed"
                 % (step_seq, entry["payload"]["tool"], entry["seq"]))

    # A settled gate that was not approved never executes.
    for step_seq, entry in sorted(resolved.items()):
        decision = entry["payload"].get("decision")
        if decision in ("skip", "expire") and step_seq in started:
            fail(name, "gate %d was settled as %r yet a step_started executes it"
                 % (step_seq, decision))


def check_effect_ids(name, run_id, journal):
    """`effect_id` derives from the opening seq and nothing else.

    So a gated step keeps the id minted at `approval_requested` through
    approval and execution, which is what makes re-execution after a crash
    upsert the same row instead of writing a second one.
    """
    for entry in journal:
        kind = entry["kind"]
        payload = entry["payload"]
        # Malformed payloads are the shape check's business, not this one's.
        if not isinstance(payload, dict):
            continue
        required = {"approval_requested": ("effect_id", "args"),
                    "step_started": ("step_seq", "effect_id", "args"),
                    "step_done": ("result", "truncated")}.get(kind)
        if required and any(key not in payload for key in required):
            continue
        if kind == "approval_requested":
            # An approval opens its own step, so its id comes from its own seq.
            expected = effect_id(run_id, entry["seq"])
            if payload.get("effect_id") != expected:
                fail(name, "seq %d effect_id %s should be %s"
                     % (entry["seq"], payload.get("effect_id"), expected))
            recomputed = args_hash(payload["args"])
            if payload.get("args_hash") != recomputed:
                fail(name, "seq %d args_hash %s should be %s"
                     % (entry["seq"], payload.get("args_hash"), recomputed))
        elif kind == "step_started":
            step_seq = payload["step_seq"]
            expected = effect_id(run_id, step_seq)
            if payload.get("effect_id") != expected:
                fail(name, "seq %d effect_id %s should be %s -- uuid5 of "
                           "step_seq %d, not of the entry's own seq"
                     % (entry["seq"], payload.get("effect_id"), expected,
                        step_seq))
            recomputed = args_hash(payload["args"])
            if payload.get("args_hash") != recomputed:
                fail(name, "seq %d args_hash %s should be %s"
                     % (entry["seq"], payload.get("args_hash"), recomputed))
        elif kind == "step_done":
            # The SDK **replaces** an oversized result with its own envelope
            # rather than trimming inside it, so the evidence of a cap is
            # `nade_truncated`, not a marker string. The fixture used to carry
            # "…[truncated N bytes]", which no build produces - a rule that
            # could never have been satisfied by a real journal.
            result = payload["result"]
            marker = isinstance(result, dict) and result.get("nade_truncated") is True
            if payload["truncated"] and not marker:
                fail(name, "seq %d is truncated but carries no "
                           "SDK's `nade_truncated` envelope" % entry["seq"])
            if marker and not payload["truncated"]:
                fail(name, "seq %d carries a truncation marker but truncated=false"
                     % entry["seq"])


def check_run_status(name, detail, journal, world):
    """A run's status agrees with its journal's last entry. This is the exact
    bug that produced a run which was simultaneously pending and completed."""
    expected = status_from_journal(journal)
    if expected is None:
        fail(name, "journal ends on %r, which settles nothing; a run's last "
                   "entry must fix its status" % journal[-1]["kind"])
    elif detail["status"] != expected:
        fail(name, "status is %r but the journal ends on %s, which means %r"
             % (detail["status"], journal[-1]["kind"], expected))

    if (detail["error"] is not None) != (detail["status"] == "failed"):
        fail(name, "error is %r for status %r; error is set exactly when a run "
                   "failed" % (detail["error"], detail["status"]))

    # The list row and the detail describe one run.
    row = world.runs.get(detail["id"])
    if row is not None:
        for field in ("agent_id", "agent_name", "trigger_kind", "status"):
            if row[field] != detail[field]:
                fail(name, "%s is %r here and %r in runs.json"
                     % (field, detail[field], row[field]))
        first = parse_ts(journal[0]["created_at"])
        created = parse_ts(row["created_at"])
        updated = parse_ts(row["updated_at"])
        if created and first and created > first:
            fail(name, "run created_at %s is after its first journal entry %s"
                 % (row["created_at"], journal[0]["created_at"]))
        last_ts = parse_ts(journal[-1]["created_at"])
        if updated and last_ts and updated < last_ts:
            fail(name, "run updated_at %s is before its last journal entry %s"
                 % (row["updated_at"], journal[-1]["created_at"]))

    agent = world.agents.get(detail["agent_id"])
    if agent is not None:
        if agent["name"] != detail["agent_name"]:
            fail(name, "agent_name %r != agent %s's name %r"
                 % (detail["agent_name"], agent["id"], agent["name"]))
        # Host-enforced allowed_tools at dispatch, regardless of what the spec
        # says: a tool a run called that the agent may not call is the bug that
        # attributed a draft to an agent without draft_reply.
        for entry in journal:
            tool = entry["payload"].get("tool") if isinstance(entry["payload"], dict) else None
            if tool is None:
                continue
            if tool not in agent.get("allowed_tools", []):
                fail(name, "seq %d calls %s, which is not in %s's allowed_tools %s"
                     % (entry["seq"], tool, agent["name"],
                        agent.get("allowed_tools", [])))

    check_engine_accounting(name, journal)


def check_engine_accounting(name, journal):
    """The facts the engine derives must agree with the entries they summarise.

    journal.rs's replay validation, applied to the fixtures: model turns count
    1..n in order; an opening entry's tool and arguments are the ones the
    model actually asked for, resolved by `call_id` to the nearest preceding
    `model_response`; and `run_ended`'s `steps` and `usage` are the totals of
    what the journal above them records.
    """
    turns = 0
    tokens_in = 0
    tokens_out = 0
    current_calls = {}                # call_id -> {"name", "arguments"}
    executed = set()                  # step_seqs whose execution started

    for entry in journal:
        kind = entry["kind"]
        payload = entry["payload"]
        if not isinstance(payload, dict):
            continue
        if kind == "model_response":
            turns += 1
            if payload.get("turn") != turns:
                fail(name, "seq %d is model turn %r; turns count 1..n in order"
                     % (entry["seq"], payload.get("turn")))
            usage = payload.get("usage") or {}
            if isinstance(usage, dict):
                tokens_in += usage.get("input_tokens", 0) or 0
                tokens_out += usage.get("output_tokens", 0) or 0
            current_calls = dict(
                (call.get("id"), call)
                for call in payload.get("tool_calls", [])
                if isinstance(call, dict)
            )
        elif kind in ("step_started", "approval_requested"):
            if kind == "step_started":
                step_seq = payload.get("step_seq")
                if isinstance(step_seq, int):
                    executed.add(step_seq)
            call = current_calls.get(payload.get("call_id"))
            if call is None:
                fail(name, "seq %d answers call %r, which the current model "
                           "turn never made" % (entry["seq"],
                                                payload.get("call_id")))
                continue
            if call.get("name") != payload.get("tool"):
                fail(name, "seq %d runs %r but the model called %r"
                     % (entry["seq"], payload.get("tool"), call.get("name")))
            if call.get("arguments") != payload.get("args"):
                fail(name, "seq %d's args differ from the arguments the model "
                           "gave call %r" % (entry["seq"], payload.get("call_id")))
        elif kind == "run_ended":
            usage = payload.get("usage") or {}
            recorded = (usage.get("input_tokens"), usage.get("output_tokens"))
            if recorded != (tokens_in, tokens_out):
                fail(name, "run_ended.usage %r; the model turns above total "
                           "(%d, %d)" % (usage, tokens_in, tokens_out))
            if payload.get("steps") != len(executed):
                fail(name, "run_ended.steps is %r but %d step(s) started "
                           "executing" % (payload.get("steps"), len(executed)))
            status = payload.get("status")
            if (status == "failed") != ("reason" in payload):
                fail(name, "run_ended carries reason=%r for status %r; reason "
                           "rides exactly on failed"
                     % (payload.get("reason"), status))
            if status != "done" and "output" in payload:
                fail(name, "run_ended carries output for status %r; the "
                           "model's closing prose rides only on done" % status)


# ==========================================================================
# 10. Runs, agents, notes, drafts
# ==========================================================================

def check_runs(world):
    for run_id, row in sorted(world.runs.items()):
        agent = world.agents.get(row["agent_id"])
        if agent is not None and agent["name"] != row["agent_name"]:
            fail("runs.json", "%s says agent_name %r, agent %s is %r"
                 % (run_id, row["agent_name"], agent["id"], agent["name"]))
        created = parse_ts(row["created_at"])
        updated = parse_ts(row["updated_at"])
        if created and updated and updated < created:
            fail("runs.json", "%s updated_at is before created_at" % run_id)

    order = [r["created_at"] for r in world.docs.get("runs.json", {}).get("runs", [])]
    if order != sorted(order, reverse=True):
        fail("runs.json", "runs are newest first (API.md section 6)")


def check_agents(world):
    # Every agent with a run needs a full-object fixture, or the tool
    # constraint below silently passes for exactly the agents that write things.
    for run in world.runs.values():
        if run["agent_id"] not in world.full_agents:
            fail("agents", "run %s belongs to agent %s, which has no full-object "
                           "fixture, so its allowed_tools cannot be checked"
                 % (run["id"], run["agent_id"]))

    for agent_id, agent in sorted(world.agents.items()):
        where = "agent %s" % agent["name"]
        if agent_id in world.full_agents:
            # Derived from spec at compile time, so null exactly when spec is.
            spans = [agent.get("when_span"), agent.get("do_span")]
            if agent.get("spec") is None:
                for field in ("when_span", "do_span", "trailing"):
                    if agent.get(field) is not None:
                        fail(where, "%s is %r but spec is null; the spans are "
                                    "derived from spec"
                             % (field, agent.get(field)))
            elif any(span is None for span in spans):
                fail(where, "has a spec but when_span/do_span are %r; the builder "
                            "renders 'When {when_span}, {do_span}.' and cannot "
                            "without them" % (spans,))
        # Creation compiles nl_definition into spec. If compilation fails the
        # agent is still created as a draft with spec null and compile_error set.
        if (agent.get("spec") is None) != (agent.get("compile_error") is not None):
            fail(where, "spec is %s and compile_error is %r; exactly one of "
                        "those is how a compile failure is recorded"
                 % ("null" if agent.get("spec") is None else "set",
                    agent.get("compile_error")))
        if agent.get("spec") is None and agent.get("status") != "draft":
            fail(where, "failed to compile, so it must still be a draft")
        spec = agent.get("spec")
        allowed = agent.get("allowed_tools", [])
        if spec is not None:
            extra = [t for t in spec["tools"] if t not in allowed]
            if extra:
                fail(where, "spec.tools %s is not a subset of allowed_tools %s"
                     % (extra, allowed))
            if (spec["trigger"]["kind"] == "schedule") != (agent["schedule"] is not None):
                fail(where, "trigger kind %r and schedule %s disagree"
                     % (spec["trigger"]["kind"],
                        "null" if agent["schedule"] is None else "set"))
        schedule = agent.get("schedule")
        if schedule is not None:
            if schedule["interval"] < 1:
                fail(where, "schedule interval must be >= 1")
            if schedule["freq"] != "week" and schedule["byweekday"]:
                fail(where, "byweekday is freq=week only")
            if schedule["freq"] != "month" and schedule["bymonthday"] is not None:
                fail(where, "bymonthday is freq=month only")
            day = schedule["bymonthday"]
            if day is not None and not (day == -1 or 1 <= day <= 28):
                fail(where, "bymonthday %r must be 1..28 or -1" % day)
            ends = schedule["ends"]
            if ends["kind"] == "on" and ends["date"] is None:
                fail(where, "ends.kind=on needs a date")
            if ends["kind"] == "after" and ends["count"] is None:
                fail(where, "ends.kind=after needs a count")
            if ends["kind"] == "never" and (ends["date"] or ends["count"]):
                fail(where, "ends.kind=never carries neither date nor count")

        # last_run_at is the newest run this agent has.
        mine = [r for r in world.runs.values() if r["agent_id"] == agent_id]
        if mine:
            newest = max(r["created_at"] for r in mine)
            if agent["last_run_at"] != newest:
                fail(where, "last_run_at %r but its newest run started %r"
                     % (agent["last_run_at"], newest))
        elif agent["last_run_at"] is not None:
            fail(where, "last_run_at is set but the agent has no runs")

    order = [a["name"] for a in world.docs.get("agents.json", {}).get("agents", [])]
    if order and order != AGENTS_OLDEST_FIRST:
        fail("agents.json", "is ordered oldest first by created_at (API.md "
                            "section 5): expected %s, got %s"
             % (AGENTS_OLDEST_FIRST, order))

    # The list row and the full object describe one agent.
    for name in AGENT_FULL_FIXTURES:
        full = world.docs.get(name)
        if not isinstance(full, dict):
            continue
        rows = [a for a in world.docs.get("agents.json", {}).get("agents", [])
                if a["id"] == full["id"]]
        if not rows:
            fail(name, "agent %s does not appear in agents.json" % full["id"])
            continue
        for key, value in rows[0].items():
            if full.get(key) != value:
                fail(name, "%s is %r here and %r in agents.json"
                     % (key, full.get(key), value))


def check_notes_and_drafts(world):
    for kind, rows, tool in (("note", world.notes, "write_note"),
                             ("draft", world.drafts, "draft_reply")):
        for row_id, row in sorted(rows.items()):
            where = "%s %s" % (kind, row_id)
            version = uuid.UUID(row_id).version
            run_id = row.get("run_id")
            if run_id is None:
                # Nothing wrote it in a run, so there is no (run, seq) to derive
                # from: a v4 id is the only correct answer.
                if version != 4:
                    fail(where, "has no run, so its id must be uuid4, got v%s"
                         % version)
                if kind == "note" and row.get("agent_name") is not None:
                    fail(where, "has no run, so agent_name must be null")
                continue

            if version != 5:
                fail(where, "was written by run %s, so its id must be the uuid5 "
                            "effect id, got v%s" % (run_id, version))
            run = world.runs.get(run_id)
            if run is None:
                continue
            # The invariant the old fixtures broke: a row exists only if the run
            # that wrote it finished.
            if run["status"] != "done":
                fail(where, "belongs to run %s, which is %r; only a run that "
                            "reached done has written its effect"
                     % (run_id, run["status"]))
            agent = world.agents.get(run["agent_id"])
            if agent is not None and tool not in agent.get("allowed_tools", []):
                fail(where, "was produced by %s, which cannot call %s "
                            "(allowed_tools %s)"
                     % (agent["name"], tool, agent.get("allowed_tools", [])))
            if kind == "note" and agent is not None:
                if row.get("agent_name") != agent["name"]:
                    fail(where, "agent_name %r != the run's agent %r"
                         % (row.get("agent_name"), agent["name"]))

            detail = world.run_details.get(run_id)
            if detail is not None:
                writes = [e for e in detail[1]["journal"]
                          if e["kind"] == "step_started"
                          and e["payload"]["tool"] == tool]
                if not writes:
                    fail(where, "run %s has no %s step" % (run_id, tool))
                elif row_id != writes[-1]["payload"]["effect_id"]:
                    fail(where, "id %s != the %s step's effect_id %s"
                         % (row_id, tool, writes[-1]["payload"]["effect_id"]))

            created = row.get("created_at")
            if created and parse_ts(created) and parse_ts(run["created_at"]):
                if parse_ts(created) < parse_ts(run["created_at"]):
                    fail(where, "was created before the run that wrote it")

    # Reading a note marks it read, so the detail always reports unread: false.
    detail = world.docs.get("note.json")
    if isinstance(detail, dict) and detail.get("unread") is not False:
        fail("note.json", "reports the state *after* the read, so unread is "
                          "always false")

    # PATCH returns the full draft. Only the patched fields may have moved.
    listed = world.docs.get("drafts.json", {}).get("drafts", [])
    patched = world.docs.get("draft.json")
    if isinstance(patched, dict) and listed:
        matching = [d for d in listed if d["id"] == patched["id"]]
        if not matching:
            fail("draft.json", "draft %s is not in drafts.json" % patched["id"])
        else:
            before = matching[0]
            for field in ("id", "run_id", "thread_id", "created_at"):
                if before[field] != patched[field]:
                    fail("draft.json", "%s changed across a PATCH (%r -> %r)"
                         % (field, before[field], patched[field]))
            if parse_ts(patched["updated_at"]) < parse_ts(before["updated_at"]):
                fail("draft.json", "updated_at went backwards across a PATCH")
            if parse_ts(patched["updated_at"]) < parse_ts(patched["created_at"]):
                fail("draft.json", "updated_at is before created_at")
    for draft in listed:
        for address in draft["to"]:
            if "@" not in address:
                fail("drafts.json", "%r is not an address" % address)

    order = [n["updated_at"] for n in world.docs.get("notes.json", {}).get("notes", [])]
    if order != sorted(order, reverse=True):
        fail("notes.json", "notes are newest first")


# ==========================================================================
# 11. Feed
# ==========================================================================

def add_days(ts, days):
    stamp = parse_ts(ts)
    if stamp is None:
        return None
    return (stamp + datetime.timedelta(days=days)).strftime("%Y-%m-%dT%H:%M:%SZ")


def check_feed(world):
    items = world.docs.get("feed.json", {}).get("items", [])
    where = "feed.json"

    counted = len([i for i in items if i["status"] == "new"])
    declared = world.docs.get("feed.json", {}).get("new_count")
    if declared != counted:
        fail(where, "new_count is %r but %d items have status new"
             % (declared, counted))

    order = [i["created_at"] for i in items]
    if order != sorted(order, reverse=True):
        fail(where, "feed items are newest first")

    for item in world.feed_items.values():
        check_feed_item(world, item)

    # The deep-link fixtures are the same rows as in the list, byte for byte:
    # GET /feed/{id} returns "the same item shape".
    for name in ("feed_item.json", "feed_item_info.json"):
        single = world.docs.get(name)
        if not isinstance(single, dict):
            continue
        matching = [i for i in items if i["id"] == single["id"]]
        if not matching:
            fail(name, "item %s does not appear in feed.json" % single["id"])
        elif matching[0] != single:
            fail(name, "differs from the same item in feed.json")
    single = world.docs.get("feed_item_info.json")
    if isinstance(single, dict) and single.get("kind") != "info":
        fail("feed_item_info.json", "must be an info item")

    # The card that renders an Edit button. Without a live `draft_reply`
    # approval in the corpus, iOS would have to draw a button it had never
    # decoded, so this fixture's whole job is to be that case.
    editable = world.docs.get("feed_item_editable.json")
    if isinstance(editable, dict):
        if editable.get("actions") != ["approve", "edit", "skip"]:
            fail("feed_item_editable.json", "must be the Edit case: actions "
                                            "['approve', 'edit', 'skip'], got %s"
                 % editable.get("actions"))
        if editable.get("status") != "new" or editable.get("kind") != "approval":
            fail("feed_item_editable.json", "must be a live approval")
        if (editable.get("data") or {}).get("action") != "draft_reply":
            fail("feed_item_editable.json", "Edit only exists for draft_reply")

    # Every branch of the section 7 rule must actually appear somewhere, or a
    # client can ship without having decoded one of them.
    seen_actions = set(tuple(i["actions"]) for i in items)
    for required in [(), ("approve", "skip"), ("approve", "edit", "skip")]:
        if required not in seen_actions:
            fail(where, "no item exercises actions %s; all three branches of "
                        "API.md section 7 must be covered" % list(required))

    # `never_messaged` drives a red flag on the card, so both states must ship.
    flags = set((i["data"] or {}).get("never_messaged") for i in items
                if (i["data"] or {}).get("action") == "draft_reply")
    if flags != {True, False}:
        fail(where, "draft_reply cards must cover never_messaged true and false, "
                    "got %s" % sorted(flags, key=str))

    # POST /feed/seen resolves info items only, so what is left new is exactly
    # the approvals.
    seen = world.docs.get("seen.json", {}).get("new_count")
    remaining = len([i for i in items if i["status"] == "new" and i["kind"] != "info"])
    if seen != remaining:
        fail("seen.json", "new_count is %r; marking the info items seen leaves %d "
                          "new approvals" % (seen, remaining))

    # POST /feed/{id}/approve moves the run pending_approval -> queued. The run
    # it names must be one whose approval was actually granted.
    approve = world.docs.get("approve.json")
    if isinstance(approve, dict):
        if approve.get("status") != "queued":
            fail("approve.json", "an approved run is queued, not %r"
                 % approve.get("status"))
        detail = world.run_details.get(approve.get("run_id"))
        if detail is None:
            fail("approve.json", "run %s has no detail fixture to corroborate it"
                 % approve.get("run_id"))
        else:
            granted = [e for e in detail[1]["journal"]
                       if e["kind"] == "approval_resolved"
                       and isinstance(e["payload"], dict)
                       and e["payload"].get("decision") == "approve"]
            if not granted:
                fail("approve.json", "run %s never recorded an approval_resolved "
                                     "with decision approve" % approve["run_id"])


def check_feed_item(world, item):
    where = "feed item %s" % item["id"]
    kind, status = item["kind"], item["status"]

    # approval_token is non-null only while status=new AND kind=approval.
    should_have_token = (kind == "approval" and status == "new")
    if (item["approval_token"] is not None) != should_have_token:
        fail(where, "approval_token is %s for kind=%s status=%s"
             % ("set" if item["approval_token"] else "null", kind, status))

    # An approval carries its deadline for its whole life; an info item has no
    # approval, so nothing to expire.
    if (item["approval_expires_at"] is not None) != (kind == "approval"):
        fail(where, "approval_expires_at is %s for kind=%s"
             % ("set" if item["approval_expires_at"] else "null", kind))
    if kind == "approval":
        expected = add_days(item["created_at"], APPROVAL_TTL_DAYS)
        if item["approval_expires_at"] != expected:
            fail(where, "approvals expire %d days after creation: expected %s, "
                        "got %s" % (APPROVAL_TTL_DAYS, expected,
                                    item["approval_expires_at"]))

    # The italic line under a settled card. Nothing to say while it is still
    # asking, and an info item asks nothing.
    should_have_note = (kind == "approval" and status != "new")
    if (item["resolved_note"] is not None) != should_have_note:
        fail(where, "resolved_note is %s for kind=%s status=%s"
             % ("set" if item["resolved_note"] else "null", kind, status))

    action = (item["data"] or {}).get("action")
    if kind == "info" and action != "none":
        fail(where, "an info item's data.action is 'none', got %r" % action)
    if kind == "approval" and action not in ("write_note", "draft_reply"):
        fail(where, "an approval's data.action must name the pending tool, got %r"
             % action)

    # API.md section 7: exactly the buttons to render, in order.
    if kind != "approval" or status != "new":
        expected = []
    elif action == "draft_reply":
        # Edit means "approve, then edit the draft it created" -- the only edit
        # path v1 has.
        expected = ["approve", "edit", "skip"]
    else:
        # There is no note editor in v1, so no Edit button.
        expected = ["approve", "skip"]
    if item["actions"] != expected:
        fail(where, "actions %s; API.md section 7 says %s for kind=%s status=%s "
                    "action=%s" % (item["actions"], expected, kind, status, action))

    # The card's button label. It says "Save note" or "Save draft" and never
    # anything outbound: v1 takes no outbound action.
    label = (item["data"] or {}).get("action_label")
    if kind == "approval":
        if not label:
            fail(where, "an approval card needs an action_label to put on the "
                        "button")
        elif label not in ("Save note", "Save draft"):
            fail(where, "action_label %r is not one of the two v1 verbs" % label)

    run_id = item.get("run_id")
    run = world.runs.get(run_id) if run_id else None
    if run is not None:
        agent = world.agents.get(run["agent_id"])
        if agent is not None:
            if item["title"] != agent["name"]:
                fail(where, "title %r is not the agent's name %r"
                     % (item["title"], agent["name"]))
            if action in agent.get("allowed_tools", []) or action == "none":
                pass
            else:
                fail(where, "asks to run %s, which is not in %s's allowed_tools %s"
                     % (action, agent["name"], agent.get("allowed_tools", [])))
        # A feed item cannot predate the run that raised it.
        if parse_ts(item["created_at"]) < parse_ts(run["created_at"]):
            fail(where, "was created before its run")
        # The card and its run must not contradict each other **about this
        # decision**. Three of the four are exact; `resolved` is not, and
        # cannot be:
        #
        #   * `new` means the run is parked on this very card;
        #   * `skipped` and `expired` are decisions that end the run, so the
        #     run carries them too;
        #   * `resolved` means "go ahead" was recorded. Where the run got to
        #     afterwards is its own business - it may still be `queued`, it may
        #     be `done`, it may have **paused again** on a second gated call
        #     (`max_steps` is 12 and an agent may hold both `write_note` and
        #     `draft_reply`), and it may have failed on a later step. The one
        #     thing it may not be is `skipped` or `expired`, which would mean
        #     two different answers to one question.
        if kind == "approval":
            exact = {"new": "pending_approval", "skipped": "skipped",
                     "expired": "expired"}
            if status in exact and run["status"] != exact[status]:
                fail(where, "is %r but its run is %r; the two move together"
                     % (status, run["status"]))
            if status == "resolved" and run["status"] in ("skipped", "expired"):
                fail(where, "is resolved but its run is %r; one decision cannot "
                            "be both" % run["status"])
        # The journal never names the feed item -- the item is a host row
        # raised from the engine's ApprovalRequest outcome (API.md section
        # 6.1), so the tie runs the other way: the run's gate must exist, the
        # item is stamped with the moment the engine asked, and the effect id
        # the item publishes is the one the gate minted.
        detail = world.run_details.get(run_id)
        if detail is not None and kind == "approval":
            gates = [e for e in detail[1]["journal"]
                     if e["kind"] == "approval_requested"]
            if not gates:
                fail(where, "run %s never requested an approval" % run_id)
            else:
                gate = gates[-1]
                if gate["created_at"] != item["created_at"]:
                    fail(where, "created_at %s but the approval was requested "
                                "at %s" % (item["created_at"],
                                           gate["created_at"]))
                payload = gate["payload"] if isinstance(gate["payload"], dict) else {}
                data = item["data"] or {}
                eid = data.get("note_id") or data.get("draft_id")
                if eid is not None and payload.get("effect_id") != eid:
                    fail(where, "publishes effect %s but the run's gate minted "
                                "%s" % (eid, payload.get("effect_id")))
                if data.get("action") != payload.get("tool"):
                    fail(where, "data.action %r but the gate holds %r"
                         % (data.get("action"), payload.get("tool")))


def check_no_outbound_copy(world):
    """No card may imply NADE sends, forwards or files anything. v1 does not."""
    word = re.compile(r"[a-z']+")
    for item in world.feed_items.values():
        for field in ("action_label",):
            label = (item["data"] or {}).get(field)
            if not label:
                continue
            for token in word.findall(label.lower()):
                if token in OUTBOUND_VERBS:
                    fail("feed item %s" % item["id"],
                         "%s %r uses the outbound verb %r; v1 takes no outbound "
                         "action" % (field, label, token))
        # `body` is the model's own prose, and `2a` renders it at 14 pt as the
        # card's content. A compromised model writing "Sent your reply to ..."
        # on the home screen is the failure `backend/testdata/injection` exists
        # to make impossible, and the server screens it at write time
        # (`agents::feed::promises_an_outbound_action`); this is the fixture
        # side of the same rule. "reply" is exempt for the same reason it is
        # exempt below: drafting one is what the product does.
        for token in word.findall((item["body"] or "").lower()):
            if token in OUTBOUND_VERBS and token != "reply":
                fail("feed item %s" % item["id"],
                     "body %r uses the outbound verb %r; v1 takes no outbound "
                     "action" % (item["body"], token))
        for token in word.findall((item["resolved_note"] or "").lower()):
            if token in OUTBOUND_VERBS and token != "reply":
                fail("feed item %s" % item["id"],
                     "resolved_note %r uses the outbound verb %r"
                     % (item["resolved_note"], token))


# ==========================================================================
# 12. Mail consistency
# ==========================================================================

def collapse(text):
    return " ".join(text.split())[:200]


def check_mail(world):
    docs = world.docs
    for name in ("thread.json", "thread_html_only.json", "thread_partial.json"):
        detail = docs.get(name)
        if not isinstance(detail, dict):
            continue
        messages = detail["messages"]
        if not messages:
            fail(name, "a thread detail always has at least one message")
            continue
        # Oldest first.
        stamps = [m["ts"] for m in messages]
        if stamps != sorted(stamps):
            fail(name, "messages are ordered oldest first")
        if detail["account_email"] != ACCOUNT_EMAIL:
            fail(name, "account_email must be %s" % ACCOUNT_EMAIL)
        # The label the thread is filed under, for the footer.
        filed = [box_id for box_id, box_name in world.mailbox_names.items()
                 if box_name == detail["mailbox_name"]]
        if not filed:
            fail(name, "mailbox_name %r is not a mailbox" % detail["mailbox_name"])
        elif not any(box_id in m["label_ids"] for box_id in filed
                     for m in messages):
            fail(name, "mailbox_name %r but no message carries that label"
                 % detail["mailbox_name"])

        # Every label a message claims membership of is either a mailbox the
        # server exposes or one of Gmail's flags. A label id in neither set is
        # a dangling reference.
        for message in messages:
            for label_id in message["label_ids"]:
                if label_id in world.mailbox_names or label_id in NEVER_A_MAILBOX:
                    continue
                fail(name, "%s carries label %s, which is neither a mailbox nor a "
                           "Gmail flag" % (message["gmail_id"], label_id))

        row = world.threads.get(detail["id"])
        if row:
            newest = messages[-1]
            # `partial` means the detail could not be completed, so the rows a
            # client is reading have gaps (API.md §2). The list row is still the
            # truth about the *thread*; the detail is a subset of it. Equality
            # is therefore the wrong assertion in that state, and asserting it
            # anyway is how a validator quietly forbids the one case the
            # contract says clients must handle. Both relations still bind in
            # the only direction that can catch a real defect.
            if detail["partial"]:
                if len(messages) > row["msg_count"]:
                    fail(name, "partial detail carries %d messages but the list "
                               "row counts only %d"
                         % (len(messages), row["msg_count"]))
                if newest["ts"] > row["ts"]:
                    fail(name, "partial detail's newest message %s is later than "
                               "the list row's ts %s"
                         % (newest["ts"], row["ts"]))
            else:
                if row["ts"] != newest["ts"]:
                    fail(name, "list ts %s but the newest message is %s"
                         % (row["ts"], newest["ts"]))
                if row["msg_count"] != len(messages):
                    fail(name, "list msg_count %d but the detail has %d messages"
                         % (row["msg_count"], len(messages)))
                for field in ("from_name", "from_email"):
                    if row[field] != newest[field]:
                        fail(name, "list %s %r is not the newest message's %r"
                             % (field, row[field], newest[field]))
            if row["subject"] != detail["subject"]:
                fail(name, "list subject %r != detail subject %r"
                     % (row["subject"], detail["subject"]))
            # unread is true when ANY message in the thread is unread.
            any_unread = any("UNREAD" in m["label_ids"] for m in messages)
            if row["unread"] != any_unread:
                fail(name, "list unread=%s but UNREAD on a message is %s"
                     % (row["unread"], any_unread))
            if row["snippet"] != collapse(newest["body_text"]):
                fail(name, "list snippet is not the newest body_text, collapsed "
                           "and capped at 200")

    for name in ("threads.json", "threads_last_page.json", "search.json"):
        rows = docs.get(name, {}).get("threads", [])
        stamps = [r["ts"] for r in rows]
        if stamps != sorted(stamps, reverse=True):
            fail(name, "threads are sorted by ts descending")
        for row in rows:
            if len(row["snippet"]) > 200:
                fail(name, "%s snippet is %d chars, cap is 200"
                     % (row["id"], len(row["snippet"])))
            if row["snippet"] != collapse(row["snippet"]):
                fail(name, "%s snippet is not whitespace-collapsed" % row["id"])
            if row["msg_count"] < 1:
                fail(name, "%s has msg_count %d" % (row["id"], row["msg_count"]))

    # One thread, one row: the same thread in two lists must be the same object.
    seen = {}
    for name in ("threads.json", "threads_last_page.json", "search.json"):
        for row in docs.get(name, {}).get("threads", []):
            if row["id"] in seen and seen[row["id"]][1] != row:
                fail(name, "thread %s differs from the copy in %s"
                     % (row["id"], seen[row["id"]][0]))
            seen[row["id"]] = (name, row)

    # agent_note is the summary of the most recent run still wanting something.
    for row in world.threads.values():
        if not row or row.get("agent_note") is None:
            continue
        wanting = [i for i in world.feed_items.values()
                   if i["status"] == "new"
                   and (i["data"] or {}).get("thread_id") == row["id"]]
        if not wanting:
            fail("thread %s" % row["id"],
                 "carries an agent_note (%r) but no run on it still wants "
                 "anything" % row["agent_note"])


def check_timestamps_are_causal(world):
    """Nothing in the world happens before the mail that caused it."""
    reply_triggered = False
    for run_id, (name, detail) in sorted(world.run_details.items()):
        ref = detail["trigger_ref"]
        if detail["trigger_kind"] != "mail" or ref is None:
            continue
        entry = world.messages.get(ref)
        if entry is None or entry[1] is None:
            continue
        thread_id = entry[0]
        # `trigger_ref` on a mail trigger is the MESSAGE id, not the thread id.
        # On a one-message thread the two are the same string, so that is only
        # visible when at least one run fires on a reply.
        if ref != thread_id:
            reply_triggered = True
        message_ts = parse_ts(entry[1]["ts"])
        run = world.runs.get(run_id)
        if run and message_ts and parse_ts(run["created_at"]) < message_ts:
            fail(name, "was triggered by %s at %s but started earlier, at %s"
                 % (ref, entry[1]["ts"], run["created_at"]))

    if not reply_triggered:
        fail("runs", "every mail trigger fires on a thread-opening message, "
                     "where trigger_ref and the thread id are the same string. "
                     "At least one run must fire on a reply, or the contract "
                     "that trigger_ref is the MESSAGE id is untested.")


# ==========================================================================
# 13. SSE streams
# ==========================================================================

SSE_EVENT_SHAPES = {
    "route": OBJ({"kind": ENUM(["answer", "results", "agent_draft"])}),
    "token": OBJ({"text": "str"}),
    "results": OBJ({"threads": ARR(THREAD_ROW)}),
    "draft": OBJ({
        "name": "str",
        "nl_definition": "str",
        "when_span": "str",
        "do_span": "str",
        "trailing": NUL("str"),
        "tool": ENUM(V1_TOOLS),
        "approval_required": "bool",
        "status": ENUM(["draft"]),
    }),
    "done": OBJ({"sources": ARR(OBJ({"gmail_id": "gmailid", "subject": "str"}))}),
    "error": OBJ({"code": ENUM(ERROR_CODES), "message": "str"}),
}


def parse_sse(name, text):
    if not text.endswith("\n\n"):
        fail(name, "must end with the blank line that terminates the last event")
        return []
    events = []
    for index, block in enumerate(text.split("\n\n")):
        if block == "":
            continue
        lines = block.split("\n")
        if len(lines) != 2:
            fail(name, "event %d is not an `event:`/`data:` pair: %r" % (index, block))
            continue
        if not lines[0].startswith("event: ") or not lines[1].startswith("data: "):
            fail(name, "event %d is malformed: %r" % (index, block))
            continue
        try:
            payload = json.loads(lines[1][len("data: "):])
        except ValueError as error:
            fail(name, "event %d data: is not valid JSON: %s" % (index, error))
            continue
        events.append((lines[0][len("event: "):], payload))
    return events


def check_streams(world, raw):
    for name in SSE_FIXTURES:
        text = raw.get(name)
        if text is None:
            continue
        events = parse_sse(name, text)
        if not events:
            continue

        # The first event is always route.
        if events[0][0] != "route":
            fail(name, "opens with %r; the first event is always route"
                 % events[0][0])
        route_kind = events[0][1].get("kind")

        for index, (event, payload) in enumerate(events):
            shape = SSE_EVENT_SHAPES.get(event)
            if shape is None:
                fail(name, "event %d is %r, which API.md section 4 does not define"
                     % (index, event))
                continue
            check_shape(payload, shape, "%s[%d](%s)" % (name, index, event))

        # Exactly one terminal event: done or error, never both, nothing after.
        terminals = [i for i, (e, _) in enumerate(events) if e in ("done", "error")]
        if len(terminals) != 1:
            fail(name, "has %d terminal events; a stream ends with exactly one"
                 % len(terminals))
        elif terminals[0] != len(events) - 1:
            fail(name, "has %d event(s) after its terminal event"
                 % (len(events) - 1 - terminals[0]))

        kinds = [e for e, _ in events]
        if kinds.count("route") != 1:
            fail(name, "carries %d route events; there is exactly one"
                 % kinds.count("route"))

        if route_kind == "results":
            if "results" not in kinds:
                fail(name, "route says results but no results event arrives")
            else:
                hits = kinds.index("results")
                # The design renders a prose lead-in above the hits.
                if "token" not in kinds[:hits]:
                    fail(name, "must carry token events before its results event")
                if any(k == "token" for k in kinds[hits:]):
                    fail(name, "tokens must all precede the results event")
                for row in events[hits][1]["threads"]:
                    if row["id"] not in world.threads:
                        fail(name, "results names thread %s, which is in no thread "
                                   "list" % row["id"])
                    else:
                        listed = world.threads[row["id"]]
                        if listed and listed != row:
                            fail(name, "thread %s differs from the list fixture"
                                 % row["id"])
                if "next_cursor" in events[hits][1]:
                    fail(name, "an SSE frame is not a page and carries no cursor")
        elif "results" in kinds:
            fail(name, "route says %r but a results event arrives" % route_kind)

        if route_kind == "agent_draft":
            drafts = [p for e, p in events if e == "draft"]
            if not drafts:
                fail(name, "route says agent_draft but no draft event arrives")
            for draft in drafts:
                # The client renders "When {when_span}, {do_span}." with both
                # underlined, so both must actually be in the sentence it saves.
                for field in ("when_span", "do_span"):
                    if draft[field] not in draft["nl_definition"]:
                        fail(name, "%s %r is not a span of nl_definition"
                             % (field, draft[field]))
                if draft["status"] != "draft":
                    fail(name, "a proposed agent is always a draft")
        elif "draft" in kinds:
            fail(name, "route says %r but a draft event arrives" % route_kind)

        for event, payload in events:
            if event == "done":
                for source in payload["sources"]:
                    if source["gmail_id"] not in world.messages:
                        fail(name, "cites %s, which appears in no thread"
                             % source["gmail_id"])

        if name == "ask_error.sse":
            if kinds[-1] != "error":
                fail(name, "must end on an error event")
            if "done" in kinds:
                fail(name, "done does not follow error")


# ==========================================================================
# 14. The generator itself
# ==========================================================================

# A module that imports none of these cannot read a clock or an RNG, which is
# the whole of "no randomness, no clocks". Checked against the parsed syntax
# tree rather than the raw text, so prose that merely *names* a banned call --
# this file does, and so does generate.py's docstring -- is not a violation.
GENERATOR_IMPORT_ALLOWLIST = {"__future__", "base64", "hashlib", "json", "os",
                              "re", "sys", "uuid"}
BANNED_CALLS = {"uuid4", "uuid1", "urandom", "now", "utcnow", "today", "time",
                "monotonic", "random", "choice", "shuffle", "sample", "randint"}


def check_generator_is_deterministic():
    path = os.path.join(HERE, "generate.py")
    if not os.path.exists(path):
        fail("generate.py", "is missing; the fixtures are generated, not written")
        return
    with open(path, encoding="utf-8") as handle:
        source = handle.read()
    try:
        tree = ast.parse(source)
    except SyntaxError as error:
        fail("generate.py", "does not parse: %s" % error)
        return

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".")[0]
                if root not in GENERATOR_IMPORT_ALLOWLIST:
                    fail("generate.py", "imports %r; the same input must always "
                                        "produce byte-identical output, so only "
                                        "%s are available"
                         % (alias.name, sorted(GENERATOR_IMPORT_ALLOWLIST)))
        elif isinstance(node, ast.ImportFrom):
            root = (node.module or "").split(".")[0]
            if root not in GENERATOR_IMPORT_ALLOWLIST:
                fail("generate.py", "imports from %r, which is not deterministic"
                     % node.module)
        elif isinstance(node, ast.Call):
            target = node.func
            name = None
            if isinstance(target, ast.Attribute):
                name = target.attr
            elif isinstance(target, ast.Name):
                name = target.id
            if name in BANNED_CALLS:
                fail("generate.py", "calls %s(); the same input must always "
                                    "produce byte-identical output" % name)


# ==========================================================================

def main():
    check_namespace()
    raw, docs = load_all()
    check_canonical_formatting(raw, docs)
    check_shapes(docs)
    check_pagination(docs)
    check_formats(docs)
    check_errors(docs)
    check_mailboxes(docs)

    world = World(docs)
    check_references(world)
    check_journals(world)
    check_tool_fingerprints(world)
    check_runs(world)
    check_agents(world)
    check_notes_and_drafts(world)
    check_feed(world)
    check_no_outbound_copy(world)
    check_mail(world)
    check_timestamps_are_causal(world)
    check_streams(world, raw)
    check_generator_is_deterministic()

    if ERRORS:
        sys.stderr.write("%d contract violation(s):\n\n" % len(ERRORS))
        for message in ERRORS:
            sys.stderr.write("  %s\n" % message)
        sys.stderr.write("\nFix generate.py -- never the JSON.\n")
        return 1

    sys.stdout.write(
        "ok: %d JSON fixtures + %d SSE streams agree with docs/API.md\n"
        % (len(docs), len(SSE_FIXTURES))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
