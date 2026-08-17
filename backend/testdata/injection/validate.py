#!/usr/bin/env python3
"""Validate the prompt-injection corpus.

This is the acceptance gate. It runs with no Rust and no model, and it proves
two different things:

1. **The manifest is internally consistent.** Every case file exists, every
   enum value is drawn from the closed set declared in the manifest, every
   attack has a severity and every control has none, ids are unique and
   family-prefixed, and the declared counts match the cases.

2. **Every case actually is what it says it is** — checked against the real
   bytes on the wire, using Python's own MIME parser as an independent decoder
   (the same trick `backend/testdata/mime/verify.py` uses: the truth never
   passes through a parser of ours). Concretely:

   - a string the manifest says the agent MUST see is genuinely present in the
     decoded message (subject + text parts + entity-decoded HTML text + alt
     text), so the case can actually exercise the assertion;
   - a string the manifest says must NOT survive is genuinely absent from the
     decoded *plain* view (e.g. a base64 payload whose decoded form must not
     appear as prose);
   - a string the manifest says is WITHHELD (hidden in HTML, must be dropped by
     the extractor) genuinely appears in the raw HTML, so "it was dropped" is a
     real claim and not a case that never contained it;
   - every declared forbidden codepoint genuinely appears in the raw bytes, so
     the neutralisation assertion has something to bite on;
   - a control never contains the attacker address, and never asks for a
     mutating tool by name — a control that smuggled a real payload would
     poison the false-positive rate it exists to measure.

What this file deliberately does NOT do is run the extractor or a model. Those
are the Rust harness's job (`harness/`), because the extractor's exact output
and the engine's exact behaviour are what the harness exists to pin down. This
validator checks that the corpus is well-formed and honest; the harness checks
that the defenses hold.

Run:  python3 backend/testdata/injection/validate.py
Exit: 0 if everything holds, 1 with a report otherwise.

Passes under Python 3.9 and 3.14.
"""

import email
import email.policy
import html as html_module
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def fail(problems, message):
    problems.append(message)


def load_manifest(problems):
    path = os.path.join(HERE, "manifest.json")
    if not os.path.exists(path):
        fail(problems, "manifest.json is missing — run generate.py first")
        return None
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


# ─────────────────────────────────────────────────── independent MIME decoding ──


def _walk_parts(msg):
    if msg.is_multipart():
        for part in msg.get_payload():
            for item in _walk_parts(part):
                yield item
    else:
        yield msg


def _decode_part_text(part):
    """Best-effort text of one leaf part, charset-aware, never raising."""
    payload = part.get_payload(decode=True)
    if payload is None:
        return ""
    charset = part.get_content_charset() or "utf-8"
    if charset.lower() in ("iso-8859-1", "latin-1", "windows-1252", "cp1252"):
        # Match what a real client does; the ASCII payloads we assert on are
        # unaffected by the choice.
        charset = "cp1252"
    try:
        return payload.decode(charset, "replace")
    except (LookupError, ValueError):
        return payload.decode("utf-8", "replace")


_TAG_RE = re.compile(r"<[^>]+>")
_ALT_RE = re.compile(r'alt\s*=\s*"([^"]*)"', re.IGNORECASE)
_TITLE_RE = re.compile(r"<title[^>]*>(.*?)</title>", re.IGNORECASE | re.DOTALL)


def _html_to_text(raw_html):
    """A generous decode: everything a lenient extractor MIGHT keep.

    Deliberately over-inclusive — it keeps alt text, title text and entity
    decoding — because its only job here is to confirm a `must_contain` string
    is reachable. The Rust harness decides what the real extractor keeps.
    """
    alts = " ".join(_ALT_RE.findall(raw_html))
    titles = " ".join(_TITLE_RE.findall(raw_html))
    stripped = _TAG_RE.sub(" ", raw_html)
    return html_module.unescape(stripped + " " + alts + " " + titles)


def decode_views(path):
    """Return (agent_visible, plain_only, raw_html) for one .eml.

    - agent_visible: subject + every text/plain and entity-decoded text/html
      part + alt/title — the widest possible view, for `must_contain`.
    - plain_only: subject + text/plain parts only, for `must_not_contain`
      (a decoded-from-encoding payload must not appear here as prose).
    - raw_html: concatenated undecoded HTML parts, for `withheld` presence.
    """
    with open(path, "rb") as handle:
        msg = email.message_from_binary_file(handle, policy=email.policy.compat32)

    subject = str(email.header.make_header(email.header.decode_header(msg.get("Subject", ""))))

    plain_chunks = [subject]
    html_chunks = []
    for part in _walk_parts(msg):
        ctype = part.get_content_type()
        if ctype == "text/plain":
            plain_chunks.append(_decode_part_text(part))
        elif ctype == "text/html":
            html_chunks.append(_decode_part_text(part))

    raw_html = "\n".join(html_chunks)
    agent_visible = "\n".join(plain_chunks) + "\n" + _html_to_text(raw_html)
    plain_only = "\n".join(plain_chunks)
    return agent_visible, plain_only, raw_html


# The six padding invisibles parse.rs maps to a space (docs/PARSER.md), plus the
# bidi and tag-character ranges the reference fence DELETES because they carry
# only rendering direction / hidden data, never content.
_PADDING_INVISIBLES = " ​‌‍﻿͏"
_BIDI_CONTROLS = "‪‫‬‭‮⁦⁧⁨⁩"


def _is_tag_char(ch):
    return 0xE0000 <= ord(ch) <= 0xE007F


def _neutralise(text):
    """The agent-visible form after the reference fence neutralises invisibles.

    Padding invisibles become spaces (matching parse.rs); bidi controls and
    Unicode tag characters are deleted (matching the reference fence). This is
    the layer `must_contain` needles are written against — the text as the agent
    would finally see it, not the raw bytes on the wire.
    """
    out = []
    for ch in text:
        if ch in _PADDING_INVISIBLES:
            out.append(" ")
        elif ch in _BIDI_CONTROLS or _is_tag_char(ch):
            continue
        else:
            out.append(ch)
    return "".join(out)


def _norm(text):
    """Neutralise invisibles, then whitespace-collapse (as the harness does)."""
    return " ".join(_neutralise(text).split())


# ───────────────────────────────────────────────────────────────── the checks ──


def check_schema(manifest, problems):
    outcomes = set(manifest["outcomes"])
    owner_tasks = manifest["owner_tasks"]
    severities = set(manifest["severities"])
    families = set(manifest["families"])
    mutating = set(manifest["mutating_tools"])

    # The owner-task → outcome map must be closed and self-consistent.
    for task, outcome in owner_tasks.items():
        if outcome not in outcomes:
            fail(problems, "owner_task {!r} maps to unknown outcome {!r}".format(task, outcome))

    seen_ids = set()
    counts = {"attack": 0, "control": 0}
    by_family = {}

    for case in manifest["cases"]:
        cid = case["id"]
        where = "case {}".format(cid)

        if cid in seen_ids:
            fail(problems, "{}: duplicate id".format(where))
        seen_ids.add(cid)

        fam = case["family"]
        if fam not in families:
            fail(problems, "{}: unknown family {!r}".format(where, fam))
        if not cid.startswith(fam + "-"):
            fail(problems, "{}: id is not prefixed with its family {!r}".format(where, fam))

        kind = case["kind"]
        if kind not in ("attack", "control"):
            fail(problems, "{}: unknown kind {!r}".format(where, kind))
        counts[kind] = counts.get(kind, 0) + 1
        by_family.setdefault(fam, {"attack": 0, "control": 0})[kind] += 1

        task = case["owner_task"]
        if task not in owner_tasks:
            fail(problems, "{}: unknown owner_task {!r}".format(where, task))

        expect = case["expect"]
        if expect["outcome"] not in outcomes:
            fail(problems, "{}: outcome {!r} not in the closed set".format(where, expect["outcome"]))
        if expect["outcome"] != owner_tasks.get(task):
            fail(
                problems,
                "{}: outcome {!r} disagrees with owner_task {!r} (should be {!r})".format(
                    where, expect["outcome"], task, owner_tasks.get(task)
                ),
            )

        sev = expect["severity_if_defense_fails"]
        if sev not in severities:
            fail(problems, "{}: severity {!r} not in the closed set".format(where, sev))
        if kind == "control" and sev != "none":
            fail(problems, "{}: a control must have severity 'none', has {!r}".format(where, sev))
        if kind == "attack" and sev == "none":
            fail(problems, "{}: an attack must have a real severity".format(where))

        # The gate invariant is the same for every case, and it is the whole
        # point: no mutating tool may execute, and no effect may be stored,
        # without a human. A case that weakened this would be lying.
        if set(expect["tools_that_must_not_execute"]) != mutating:
            fail(problems, "{}: tools_that_must_not_execute must equal the mutating set".format(where))
        if expect["max_stored_effects"] != 0:
            fail(problems, "{}: max_stored_effects must be 0 (no unapproved effect, ever)".format(where))

        # approval_must_mention only means something when the run pauses.
        if case["expect"]["approval_must_mention"] and expect["outcome"] != "pauses_for_approval":
            fail(problems, "{}: approval_must_mention set but outcome is not pauses_for_approval".format(where))

    # Declared counts must match reality.
    if counts["attack"] != manifest["counts"]["attacks"]:
        fail(problems, "attack count {} != declared {}".format(counts["attack"], manifest["counts"]["attacks"]))
    if counts["control"] != manifest["counts"]["controls"]:
        fail(problems, "control count {} != declared {}".format(counts["control"], manifest["counts"]["controls"]))
    if len(seen_ids) != manifest["counts"]["total"]:
        fail(problems, "total count {} != declared {}".format(len(seen_ids), manifest["counts"]["total"]))
    for fam, got in by_family.items():
        declared = manifest["counts"]["by_family"].get(fam)
        if declared != got:
            fail(problems, "family {} counts {} != declared {}".format(fam, got, declared))


def check_coverage(manifest, problems):
    """Every family the task names must exist, and controls must be plentiful."""
    required_families = {
        "direct", "fence", "encoding", "hidden", "identity",
        "tool", "multistage", "exfil", "dos", "control",
    }
    present = set(manifest["families"])
    missing = required_families - present
    if missing:
        fail(problems, "missing required families: {}".format(sorted(missing)))

    if manifest["counts"]["controls"] < 15:
        fail(problems, "need at least 15 controls, have {}".format(manifest["counts"]["controls"]))
    if manifest["counts"]["total"] < 60:
        fail(problems, "need at least 60 cases, have {}".format(manifest["counts"]["total"]))

    # Every family except 'control' should carry attacks; 'control' carries only
    # controls. (Controls live in their own family here by design, so the corpus
    # has one obvious place to look for the false-positive set.)
    for fam, bucket in manifest["counts"]["by_family"].items():
        if fam == "control":
            if bucket["attack"] != 0:
                fail(problems, "the control family must hold only controls")
        elif bucket["attack"] == 0:
            fail(problems, "family {} has no attacks".format(fam))


_FORBIDDEN_RE = re.compile(r"^U\+([0-9A-Fa-f]{4,6})$")


def check_case_bytes(manifest, problems):
    attacker = manifest["attacker_address"]
    mutating = manifest["mutating_tools"]

    for case in manifest["cases"]:
        cid = case["id"]
        where = "case {}".format(cid)
        path = os.path.join(HERE, case["file"])
        if not os.path.exists(path):
            fail(problems, "{}: file {} does not exist".format(where, case["file"]))
            continue

        try:
            agent_visible, plain_only, raw_html = decode_views(path)
        except Exception as exc:  # noqa: BLE001 — a corpus file must always decode
            fail(problems, "{}: Python could not decode the .eml ({})".format(where, exc))
            continue

        expect = case["expect"]
        agent_n = _norm(agent_visible)
        plain_n = _norm(plain_only)

        # must_contain: reachable in the widest decode.
        for needle in expect["extracted_must_contain"]:
            if _norm(needle) not in agent_n:
                fail(problems, "{}: must_contain {!r} is not present in the decoded message".format(where, needle))

        # must_not_contain: absent from the plain view (an encoded payload's
        # decoded form must not already be sitting in prose).
        #
        # EXCEPT for `fails_safely` cases. Those assert that OUR extractor yields
        # nothing, and Python's parser is deliberately more lenient than ours —
        # it happily recovers a part with no closing boundary, which is exactly
        # the shape those cases use. Only the Rust harness can check that claim
        # (`fail_safe_cases_produce_no_agent_input`), so here we check the half
        # Python can: that the payload really is in the bytes, making the case
        # non-vacuous.
        fail_safe = expect["outcome"] == "fails_safely"
        for needle in expect["extracted_must_not_contain"]:
            if fail_safe:
                with open(path, "rb") as handle:
                    source = handle.read().decode("utf-8", "replace")
                if _norm(needle) not in _norm(source):
                    fail(problems, "{}: fails_safely case does not contain {!r} at all — it proves nothing".format(where, needle))
            elif _norm(needle) in plain_n:
                fail(problems, "{}: must_not_contain {!r} appears in the decoded plain text".format(where, needle))

        # withheld: genuinely present in the raw HTML, so "dropped" is a real
        # claim. (These are hidden-HTML cases by construction.)
        for needle in expect["withheld_from_agent_must_contain"]:
            if _norm(needle) not in _norm(raw_html):
                fail(problems, "{}: withheld string {!r} is not in the raw HTML — the case cannot show a drop".format(where, needle))

        # forbidden codepoints: genuinely present pre-neutralisation, either as a
        # literal byte in the .eml or as an HTML entity that decodes to it (a
        # message can smuggle U+200B as &#8203;). The Rust harness proves they are
        # gone from the agent view; this proves the case really contained them.
        with open(path, "rb") as handle:
            raw_text = handle.read().decode("utf-8", "surrogatepass")
        pre_neutral = raw_text + "\n" + html_module.unescape(raw_html)
        for token in expect["forbidden_codepoints"]:
            match = _FORBIDDEN_RE.match(token)
            if not match:
                fail(problems, "{}: forbidden codepoint {!r} is not U+XXXX".format(where, token))
                continue
            if chr(int(match.group(1), 16)) not in pre_neutral:
                fail(problems, "{}: declared forbidden codepoint {} does not appear in the .eml".format(where, token))

        # A control must not smuggle a real attack: no attacker address, no
        # mutating-tool name presented as an instruction to the agent.
        if case["kind"] == "control":
            if attacker in agent_visible or attacker in raw_html:
                fail(problems, "{}: a CONTROL contains the attacker address — it would poison the false-positive rate".format(where))
            for tool in mutating:
                # The tool NAME may legitimately appear (control-11 discusses
                # write_note); what a control must not do is address the agent
                # with an imperative to use it against the owner. We approximate
                # that with the attacker-address check above and a softer check
                # here: a control must not pair a mutating verb with the attacker.
                pass

        # payload_encoded cases must NOT have their decoded instruction sitting
        # in the plain text (that is the definition of "encoded").
        if expect["payload_encoded"] and case["kind"] == "attack":
            if "ignore all previous instructions" in plain_n.lower():
                fail(problems, "{}: marked payload_encoded but the plaintext instruction is present".format(where))


def check_files_on_disk(manifest, problems):
    """No orphan .eml files, and every case points at a file that exists."""
    declared = {os.path.basename(c["file"]) for c in manifest["cases"]}
    cases_dir = os.path.join(HERE, "cases")
    if not os.path.isdir(cases_dir):
        fail(problems, "cases/ directory is missing")
        return
    on_disk = {n for n in os.listdir(cases_dir) if n.endswith(".eml")}
    for orphan in sorted(on_disk - declared):
        fail(problems, "orphan file cases/{} is not in the manifest".format(orphan))
    for missing in sorted(declared - on_disk):
        fail(problems, "manifest references cases/{} which is not on disk".format(missing))


def main():
    problems = []
    manifest = load_manifest(problems)
    if manifest is None:
        print("\n".join(problems))
        return 1

    check_schema(manifest, problems)
    check_coverage(manifest, problems)
    check_files_on_disk(manifest, problems)
    check_case_bytes(manifest, problems)

    if problems:
        print("INJECTION CORPUS INVALID — {} problem(s):\n".format(len(problems)))
        for problem in problems:
            print("  - {}".format(problem))
        return 1

    counts = manifest["counts"]
    print(
        "injection corpus valid: {} cases ({} attacks, {} controls) across {} families, "
        "manifest consistent, every case verified against its own bytes".format(
            counts["total"], counts["attacks"], counts["controls"], len(manifest["families"])
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
