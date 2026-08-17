#!/usr/bin/env python3
"""Verify the corpus fixtures against expected.json using Python's stdlib parser.

This is a check on the *fixtures*, not on our Rust parser. If Python's email
module cannot recover the ground truth from a .eml we generated, the fixture is
malformed and any Rust test built on it would be testing nonsense.

A handful of cases are expected to diverge here, on purpose — they encode
behaviour a spec-literal parser gets wrong and a real mail client gets right.
Those are listed in ALLOWED_DIVERGENCE with the reason. Everything else must
match exactly.

Run: python3 backend/testdata/mime/verify.py
"""

import email
import email.policy
import json
import os
import sys
from datetime import timezone

HERE = os.path.dirname(os.path.abspath(__file__))

# Cases where stdlib is expected to disagree with the ground truth, and why.
ALLOWED_DIVERGENCE = {
    "09-html-only.eml": {
        "body_text_contains",
        "body_text_excludes",
    },  # stdlib does no html2text; that is our parser's job
    "10-multipart-related-cid.eml": {
        "body_text_contains",
    },  # same reason as 09 — the only text part is text/html
    "12-cp1252-mislabelled.eml": {
        "subject",
        "body_text_contains",
    },  # stdlib obeys the (wrong) iso-8859-1 label; mail clients substitute cp1252
    "22-bad-base64.eml": {"body_text_contains"},  # degraded decode, content undefined
    "26-huge-body.eml": {"body_text_min_length"},  # checked separately below
}


def raw_header(raw: bytes, name: str):
    """First occurrence of a header, unfolded, straight off the wire bytes.

    Deliberately policy-free: nothing here can throw on malformed content.
    """
    head = raw.split(b"\r\n\r\n", 1)[0].split(b"\n\n", 1)[0]
    text = head.decode("utf-8", errors="replace").replace("\r\n", "\n")
    lines = text.split("\n")
    want = name.lower() + ":"
    for i, line in enumerate(lines):
        if line.lower().startswith(want):
            value = line[len(want):]
            j = i + 1
            while j < len(lines) and lines[j][:1] in (" ", "\t"):
                value += " " + lines[j].strip()
                j += 1
            return value.strip()
    return None


def addr_list(msg, header):
    out = []
    for name, addr in email.utils.getaddresses(msg.get_all(header, [])):
        if addr:
            out.append(addr.lower())
    return out


def best_bodies(msg):
    """Return (text, html) preferring text/plain, ignoring attachment parts."""
    text, html = None, None
    if not msg.is_multipart():
        ct = msg.get_content_type()
        payload = msg.get_payload(decode=True) or b""
        charset = msg.get_content_charset() or "utf-8"
        try:
            decoded = payload.decode(charset, errors="replace")
        except LookupError:
            decoded = payload.decode("utf-8", errors="replace")
        if ct == "text/html":
            html = decoded
        else:
            text = decoded
        return text, html

    for part in msg.walk():
        if part.is_multipart():
            continue
        disp = (part.get_content_disposition() or "").lower()
        if disp == "attachment":
            continue
        ct = part.get_content_type()
        if ct not in ("text/plain", "text/html"):
            continue
        payload = part.get_payload(decode=True) or b""
        charset = part.get_content_charset() or "utf-8"
        try:
            decoded = payload.decode(charset, errors="replace")
        except LookupError:
            decoded = payload.decode("utf-8", errors="replace")
        if ct == "text/plain" and text is None:
            text = decoded
        elif ct == "text/html" and html is None:
            html = decoded
    return text, html


def attachments(msg):
    out = []
    if not msg.is_multipart():
        return out
    for part in msg.walk():
        if part.is_multipart():
            continue
        disp = (part.get_content_disposition() or "").lower()
        fn = part.get_filename()
        cid = part.get("Content-ID")
        if disp in ("attachment", "inline") and (fn or cid):
            out.append(
                {
                    "name": fn or "",
                    "mime": part.get_content_type(),
                    "inline": disp == "inline",
                }
            )
    return out


def main():
    with open(os.path.join(HERE, "expected.json"), encoding="utf-8") as f:
        expected = json.load(f)

    failures = []
    divergences = []

    for name, exp in expected.items():
        path = os.path.join(HERE, name)
        if not os.path.exists(path):
            failures.append(f"{name}: fixture file missing")
            continue
        with open(path, "rb") as f:
            raw = f.read()

        # A parse that throws is always a fixture bug.
        try:
            msg = email.message_from_bytes(raw, policy=email.policy.default)
        except Exception as e:  # noqa: BLE001
            failures.append(f"{name}: stdlib failed to parse: {e!r}")
            continue

        skip = ALLOWED_DIVERGENCE.get(name, set())

        def check(field, got, want):
            if field in skip:
                if got != want:
                    divergences.append(f"{name}.{field}: stdlib={got!r} truth={want!r}")
                return
            if got != want:
                failures.append(f"{name}.{field}: stdlib={got!r} expected={want!r}")

        check("subject", str(msg.get("Subject") or ""), exp["subject"])

        froms = email.utils.getaddresses(msg.get_all("From", []))
        got_name = froms[0][0] if froms else ""
        got_addr = froms[0][1].lower() if froms else ""
        check("from_name", got_name, exp["from_name"])
        check("from_email", got_addr, exp["from_email"])
        check("to", addr_list(msg, "To"), [a.lower() for a in exp["to"]])
        check("cc", addr_list(msg, "Cc"), [a.lower() for a in exp["cc"]])

        # Date is read off the raw bytes, not through the policy. policy.default
        # RAISES on an unparseable Date (case 14) the moment you touch the
        # header — which is itself the bug this corpus exists to catch, so the
        # verifier must not depend on the very behaviour under test.
        want_date = exp.get("date_utc")
        raw_date = raw_header(raw, "Date")
        got_date = None
        if raw_date is not None:
            try:
                dt = email.utils.parsedate_to_datetime(raw_date)
                if dt is not None:
                    if dt.tzinfo is None:
                        dt = dt.replace(tzinfo=timezone.utc)
                    got_date = dt.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            except (TypeError, ValueError):
                got_date = None
        check("date_utc", got_date, want_date)

        text, html = best_bodies(msg)
        text = text or ""

        if exp.get("body_text_is_empty"):
            if text.strip() != "":
                failures.append(f"{name}.body_text: expected empty, got {text[:60]!r}")
        for needle in exp.get("body_text_contains", []):
            if "body_text_contains" in skip:
                if needle not in text:
                    divergences.append(f"{name}: stdlib body lacks {needle!r}")
            elif needle not in text:
                failures.append(f"{name}: body_text missing {needle!r}; got {text[:120]!r}")
        for needle in exp.get("body_text_excludes", []):
            if "body_text_excludes" in skip:
                continue
            if needle in text:
                failures.append(f"{name}: body_text should not contain {needle!r}")
        if "body_text_min_length" in exp and "body_text_min_length" not in skip:
            if len(text) < exp["body_text_min_length"]:
                failures.append(
                    f"{name}: body_text len {len(text)} < {exp['body_text_min_length']}"
                )

        check("body_html_present", html is not None, exp["body_html_present"])

        got_atts = attachments(msg)
        want_atts = exp.get("attachments", [])
        if len(got_atts) != len(want_atts):
            failures.append(
                f"{name}.attachments: stdlib found {[a['name'] for a in got_atts]}, "
                f"expected {[a['name'] for a in want_atts]}"
            )
        else:
            for got, want in zip(got_atts, want_atts):
                for k in ("name", "mime", "inline"):
                    if got[k] != want[k]:
                        failures.append(
                            f"{name}.attachment.{k}: stdlib={got[k]!r} expected={want[k]!r}"
                        )

    # 26 needs its own check: the huge body is in the payload, length verified raw.
    huge = os.path.join(HERE, "26-huge-body.eml")
    if os.path.exists(huge) and os.path.getsize(huge) < 100_000:
        failures.append("26-huge-body.eml is not actually huge")

    print(f"cases: {len(expected)}")
    if divergences:
        print(f"\nintentional divergences ({len(divergences)}) — documented, not failures:")
        for d in divergences:
            print(f"  · {d}")
    if failures:
        print(f"\nFIXTURE BUGS ({len(failures)}):")
        for f_ in failures:
            print(f"  ✗ {f_}")
        sys.exit(1)
    print("\nall fixtures well-formed: stdlib recovers the ground truth")


if __name__ == "__main__":
    main()
