#!/usr/bin/env python3
"""Generate the MIME parser conformance corpus.

The point of this corpus is that the ground truth is defined FIRST, in plain
Python values, and then encoded into RFC-822 bytes by Python's standard
library. Our Rust parser must recover the ground truth from those bytes. The
truth never passes through our own parser, so the test is not circular.

Run:  python3 backend/testdata/mime/generate.py
Emits: *.eml  and  expected.json  in this directory.

Every case here is a real thing that arrives in a real inbox and a real way a
naive parser breaks. Do not delete a case because it looks exotic; each one is
a bug someone shipped.
"""

import base64
import json
import os
import quopri

HERE = os.path.dirname(os.path.abspath(__file__))
CRLF = "\r\n"

cases = {}


def emit(name, raw, expect):
    """Store one case. `raw` is the exact bytes on the wire."""
    if isinstance(raw, str):
        raw = raw.replace("\n", CRLF).encode("utf-8", "surrogateescape")
    path = os.path.join(HERE, name + ".eml")
    with open(path, "wb") as f:
        f.write(raw)
    cases[name + ".eml"] = expect


def b64(s, charset="utf-8"):
    return base64.b64encode(s.encode(charset)).decode("ascii")


def qp(s, charset="utf-8"):
    return quopri.encodestring(s.encode(charset)).decode("ascii")


# ── 01 the trivial case ──────────────────────────────────────────────────────
emit(
    "01-plain-ascii",
    """From: Priya Raghavan <priya@kettle.com>
To: jatinsethi98@gmail.com
Subject: Staff Product Designer at Kettle
Date: Sat, 16 Aug 2026 09:12:04 +0000
Message-ID: <case01@kettle.com>
Content-Type: text/plain; charset="us-ascii"

Hi Jatin - are you open to a conversation?
""",
    {
        "subject": "Staff Product Designer at Kettle",
        "from_name": "Priya Raghavan",
        "from_email": "priya@kettle.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-16T09:12:04Z",
        "body_text_contains": ["Hi Jatin - are you open to a conversation?"],
        "body_html_present": False,
        "attachments": [],
    },
)

# ── 02 RFC 2047 base64 encoded-word subject ─────────────────────────────────
subj02 = "Ihre Bestellung ist unterwegs"
emit(
    "02-rfc2047-b64-subject",
    f"""From: =?UTF-8?B?{b64("Föhn Versand")}?= <versand@foehn.de>
To: jatinsethi98@gmail.com
Subject: =?UTF-8?B?{b64(subj02)}?=
Date: Fri, 14 Aug 2026 20:55:31 +0200
Content-Type: text/plain; charset="utf-8"

Ihre Bestellung Nr. 88-2041 wurde versandt.
""",
    {
        "subject": subj02,
        "from_name": "Föhn Versand",
        "from_email": "versand@foehn.de",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-14T18:55:31Z",
        "body_text_contains": ["Ihre Bestellung Nr. 88-2041"],
        "body_html_present": False,
        "attachments": [],
    },
)

# ── 03 RFC 2047 Q encoding — underscore means space, not underscore ─────────
emit(
    "03-rfc2047-q-subject",
    """From: support@example.com
To: jatinsethi98@gmail.com
Subject: =?UTF-8?Q?Don=E2=80=99t_forget_your_caf=C3=A9_booking?=
Date: Mon, 3 Aug 2026 07:00:00 -0700
Content-Type: text/plain; charset="utf-8"

See you there.
""",
    {
        "subject": "Don’t forget your café booking",
        "from_name": "",
        "from_email": "support@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-03T14:00:00Z",
        "body_text_contains": ["See you there."],
        "body_html_present": False,
        "attachments": [],
    },
)

# ── 04 legacy charset in an encoded word ────────────────────────────────────
emit(
    "04-rfc2047-iso8859-1",
    ("""From: =?ISO-8859-1?Q?J=FCrgen_M=FCller?= <juergen@example.de>
To: jatinsethi98@gmail.com
Subject: =?ISO-8859-1?Q?Gr=FC=DFe_aus_M=FCnchen?=
Date: Tue, 4 Aug 2026 12:00:00 +0000
Content-Type: text/plain; charset="iso-8859-1"

Sch\xf6ne Gr\xfc\xdfe.
""").replace("\n", CRLF).encode("latin-1"),
    {
        "subject": "Grüße aus München",
        "from_name": "Jürgen Müller",
        "from_email": "juergen@example.de",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-04T12:00:00Z",
        "body_text_contains": ["Schöne Grüße."],
        "body_html_present": False,
        "attachments": [],
    },
)

# ── 05 adjacent encoded words join with NO space between them ───────────────
# RFC 2047 §6.2: linear whitespace between two encoded words is not displayed.
emit(
    "05-rfc2047-adjacent-words",
    f"""From: news@example.com
To: jatinsethi98@gmail.com
Subject: =?UTF-8?B?{b64("Quarterly ")}?= =?UTF-8?B?{b64("résumé review")}?=
Date: Wed, 5 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="utf-8"

Body.
""",
    {
        "subject": "Quarterly résumé review",
        "from_name": "",
        "from_email": "news@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-05T09:00:00Z",
        "body_text_contains": ["Body."],
        "body_html_present": False,
        "attachments": [],
        "note": "RFC 2047 6.2 — whitespace between adjacent encoded words is dropped.",
    },
)

# ── 06 quoted-printable body with soft line breaks ──────────────────────────
body06 = (
    "Hi — your invoice for €41.90 is attached. This line is deliberately "
    "long enough that quoted-printable must fold it with a soft break, which the "
    "parser has to rejoin without inserting a newline.\r\n"
)
emit(
    "06-quoted-printable-body",
    f"""From: billing@example.com
To: jatinsethi98@gmail.com
Subject: Invoice
Date: Thu, 6 Aug 2026 10:30:00 +0000
Content-Type: text/plain; charset="utf-8"
Content-Transfer-Encoding: quoted-printable

{qp(body06)}""",
    {
        "subject": "Invoice",
        "from_name": "",
        "from_email": "billing@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-06T10:30:00Z",
        "body_text_contains": [
            "your invoice for €41.90",
            "rejoin without inserting a newline",
        ],
        "body_html_present": False,
        "attachments": [],
        "note": "Soft line breaks (=\\r\\n) must vanish, not become newlines.",
    },
)

# ── 07 base64 body ──────────────────────────────────────────────────────────
body07 = "Base64 body with an em dash — and a CJK run: 東京で会いましょう。\n"
emit(
    "07-base64-body",
    f"""From: tokyo@example.jp
To: jatinsethi98@gmail.com
Subject: Base64
Date: Fri, 7 Aug 2026 08:00:00 +0900
Content-Type: text/plain; charset="utf-8"
Content-Transfer-Encoding: base64

{b64(body07)}
""",
    {
        "subject": "Base64",
        "from_name": "",
        "from_email": "tokyo@example.jp",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-06T23:00:00Z",
        "body_text_contains": ["em dash —", "東京で会いましょう"],
        "body_html_present": False,
        "attachments": [],
    },
)

# ── 08 multipart/alternative — plain text wins ──────────────────────────────
emit(
    "08-multipart-alternative",
    """From: Kamran Ali <kamran@northbound.co>
To: jatinsethi98@gmail.com
Cc: design@northbound.co, Priya <priya@kettle.com>
Subject: Re: Design review
Date: Sat, 15 Aug 2026 18:03:52 +0000
Content-Type: multipart/alternative; boundary="BOUND08"

--BOUND08
Content-Type: text/plain; charset="utf-8"

Works for me. I will bring the composer flows.

--BOUND08
Content-Type: text/html; charset="utf-8"

<html><body><p>Works for me. I will bring the <b>composer flows</b>.</p></body></html>

--BOUND08--
""",
    {
        "subject": "Re: Design review",
        "from_name": "Kamran Ali",
        "from_email": "kamran@northbound.co",
        "to": ["jatinsethi98@gmail.com"],
        "cc": ["design@northbound.co", "priya@kettle.com"],
        "date_utc": "2026-08-15T18:03:52Z",
        "body_text_contains": ["Works for me. I will bring the composer flows."],
        "body_html_present": True,
        "attachments": [],
        "note": "text/plain is preferred for body_text; body_html keeps the HTML part.",
    },
)

# ── 09 HTML only — body_text must be synthesised, never empty ───────────────
emit(
    "09-html-only",
    """From: newsletter@example.com
To: jatinsethi98@gmail.com
Subject: HTML only
Date: Sun, 9 Aug 2026 06:00:00 +0000
Content-Type: text/html; charset="utf-8"

<html><head><style>p{color:red}</style><script>var x=1;</script></head>
<body><h1>Weekly digest</h1><p>Three&nbsp;stories this week.</p>
<p>Read&nbsp;more &rarr;</p></body></html>
""",
    {
        "subject": "HTML only",
        "from_name": "",
        "from_email": "newsletter@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-09T06:00:00Z",
        "body_text_contains": ["Weekly digest", "Three stories this week"],
        "body_text_excludes": ["color:red", "var x=1", "&nbsp;", "<p>"],
        "body_html_present": True,
        "attachments": [],
        "note": "PLAN.md guarantees body_text non-empty via html2text. Script and style content must NOT leak into it, and entities must be decoded.",
    },
)

# ── 10 multipart/related with an inline cid: image ──────────────────────────
png = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)
emit(
    "10-multipart-related-cid",
    """From: shop@example.com
To: jatinsethi98@gmail.com
Subject: Your order
Date: Mon, 10 Aug 2026 11:00:00 +0000
Content-Type: multipart/related; boundary="BOUND10"; type="text/html"

--BOUND10
Content-Type: text/html; charset="utf-8"

<html><body><p>Thanks!</p><img src="cid:logo@example.com" alt="logo"></body></html>

--BOUND10
Content-Type: image/png
Content-Transfer-Encoding: base64
Content-ID: <logo@example.com>
Content-Disposition: inline; filename="logo.png"

"""
    + base64.b64encode(png).decode("ascii")
    + """

--BOUND10--
""",
    {
        "subject": "Your order",
        "from_name": "",
        "from_email": "shop@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-10T11:00:00Z",
        "body_text_contains": ["Thanks!"],
        "body_html_present": True,
        "attachments": [
            {"name": "logo.png", "mime": "image/png", "inline": True, "content_id": "logo@example.com"}
        ],
        "note": "cid: references must be rewritten to the attachment proxy URL; the inline part is still an attachment row.",
    },
)

# ── 11 multipart/mixed with a real attachment ───────────────────────────────
pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n"
emit(
    "11-multipart-mixed-attachment",
    """From: Priya Raghavan <priya@kettle.com>
To: jatinsethi98@gmail.com
Subject: Role outline
Date: Sat, 16 Aug 2026 09:12:04 +0000
Content-Type: multipart/mixed; boundary="BOUND11"

--BOUND11
Content-Type: text/plain; charset="utf-8"

The outline is attached.

--BOUND11
Content-Type: application/pdf; name="=?UTF-8?B?"""
    + b64("Kettle — Staff Designer.pdf")
    + """?="
Content-Transfer-Encoding: base64
Content-Disposition: attachment; filename="=?UTF-8?B?"""
    + b64("Kettle — Staff Designer.pdf")
    + """?="

"""
    + base64.b64encode(pdf).decode("ascii")
    + """

--BOUND11--
""",
    {
        "subject": "Role outline",
        "from_name": "Priya Raghavan",
        "from_email": "priya@kettle.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-16T09:12:04Z",
        "body_text_contains": ["The outline is attached."],
        "body_html_present": False,
        "attachments": [
            {"name": "Kettle — Staff Designer.pdf", "mime": "application/pdf", "inline": False}
        ],
        "note": "Attachment filename is itself RFC 2047 encoded — a very common real-world case.",
    },
)

# ── 12 windows-1252 smart quotes mislabelled as iso-8859-1 ──────────────────
emit(
    "12-cp1252-mislabelled",
    ("""From: marketing@example.com
To: jatinsethi98@gmail.com
Subject: Don\x92t miss out
Date: Tue, 11 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="iso-8859-1"

We\x92ve got \x93news\x94 for you \x97 read on.
""").replace("\n", CRLF).encode("latin-1"),
    {
        "subject": "Don’t miss out",
        "from_name": "",
        "from_email": "marketing@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-11T09:00:00Z",
        "body_text_contains": ["We’ve got “news” for you — read on."],
        "body_html_present": False,
        "attachments": [],
        "note": "Senders label cp1252 as iso-8859-1 constantly. Decoding 0x92 as latin-1 gives an unprintable control char; treating iso-8859-1 as cp1252 is what every mail client does. If the parser is strict here it produces mojibake on a large fraction of real mail.",
    },
)

# ── 13 no Date header at all ────────────────────────────────────────────────
emit(
    "13-no-date-header",
    """From: nodate@example.com
To: jatinsethi98@gmail.com
Subject: No date
Content-Type: text/plain; charset="utf-8"

This message has no Date header.
""",
    {
        "subject": "No date",
        "from_name": "",
        "from_email": "nodate@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": None,
        "body_text_contains": ["This message has no Date header."],
        "body_html_present": False,
        "attachments": [],
        "note": "Must not panic. Fall back to Gmail's internalDate, which the sync layer always has.",
    },
)

# ── 14 unparseable Date ─────────────────────────────────────────────────────
emit(
    "14-malformed-date",
    """From: baddate@example.com
To: jatinsethi98@gmail.com
Subject: Bad date
Date: Yesterday afternoon, probably
Content-Type: text/plain; charset="utf-8"

Garbage date header.
""",
    {
        "subject": "Bad date",
        "from_name": "",
        "from_email": "baddate@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": None,
        "body_text_contains": ["Garbage date header."],
        "body_html_present": False,
        "attachments": [],
        "note": "Jarvis panicked here. Never panic on header content.",
    },
)

# ── 15 bare address, no display name ────────────────────────────────────────
emit(
    "15-bare-address",
    """From: alerts@chase.com
To: jatinsethi98@gmail.com
Subject: Your statement is ready
Date: Sun, 16 Aug 2026 07:40:11 +0000
Content-Type: text/plain; charset="utf-8"

Statement available.
""",
    {
        "subject": "Your statement is ready",
        "from_name": "",
        "from_email": "alerts@chase.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-16T07:40:11Z",
        "body_text_contains": ["Statement available."],
        "body_html_present": False,
        "attachments": [],
        "note": "from_name is empty, NOT the local part. The UI decides what to show.",
    },
)

# ── 16 group syntax and an empty To ─────────────────────────────────────────
emit(
    "16-group-address",
    """From: bulk@example.com
To: undisclosed-recipients:;
Cc:
Subject: Bulk mail
Date: Wed, 12 Aug 2026 15:00:00 +0000
Content-Type: text/plain; charset="utf-8"

You are one of many.
""",
    {
        "subject": "Bulk mail",
        "from_name": "",
        "from_email": "bulk@example.com",
        "to": [],
        "cc": [],
        "date_utc": "2026-08-12T15:00:00Z",
        "body_text_contains": ["You are one of many."],
        "body_html_present": False,
        "attachments": [],
        "note": "Group syntax with no members, and a present-but-empty Cc. Both must yield empty lists, not errors and not a phantom recipient.",
    },
)

# ── 17 raw 8-bit UTF-8, no transfer encoding ────────────────────────────────
emit(
    "17-8bit-utf8",
    """From: Ana Ruiz <ana@example.es>
To: jatinsethi98@gmail.com
Subject: Reunión mañana
Date: Thu, 13 Aug 2026 08:15:00 +0200
Content-Type: text/plain; charset="utf-8"
Content-Transfer-Encoding: 8bit

¿Nos vemos a las nueve? Ángela también viene.
""",
    {
        "subject": "Reunión mañana",
        "from_name": "Ana Ruiz",
        "from_email": "ana@example.es",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-13T06:15:00Z",
        "body_text_contains": ["¿Nos vemos a las nueve? Ángela también viene."],
        "body_html_present": False,
        "attachments": [],
        "note": "Unencoded 8-bit header value AND body. The Subject here is raw UTF-8, not an encoded word — also legal in practice.",
    },
)

# ── 18 nested multipart ─────────────────────────────────────────────────────
emit(
    "18-nested-multipart",
    """From: complex@example.com
To: jatinsethi98@gmail.com
Subject: Nested
Date: Fri, 14 Aug 2026 09:00:00 +0000
Content-Type: multipart/mixed; boundary="OUTER"

--OUTER
Content-Type: multipart/alternative; boundary="INNER"

--INNER
Content-Type: text/plain; charset="utf-8"

Plain part inside the alternative.

--INNER
Content-Type: text/html; charset="utf-8"

<p>HTML part inside the alternative.</p>

--INNER--

--OUTER
Content-Type: text/plain; charset="utf-8"
Content-Disposition: attachment; filename="notes.txt"

Attached plain text.

--OUTER--
""",
    {
        "subject": "Nested",
        "from_name": "",
        "from_email": "complex@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-14T09:00:00Z",
        "body_text_contains": ["Plain part inside the alternative."],
        "body_text_excludes": ["Attached plain text."],
        "body_html_present": True,
        "attachments": [{"name": "notes.txt", "mime": "text/plain", "inline": False}],
        "note": "A text/plain part with Content-Disposition: attachment is an ATTACHMENT, not the body. Getting this wrong appends attachment text to every body.",
    },
)

# ── 19 headers only, no body ────────────────────────────────────────────────
emit(
    "19-empty-body",
    """From: empty@example.com
To: jatinsethi98@gmail.com
Subject: (no body)
Date: Sat, 15 Aug 2026 00:00:00 +0000
Content-Type: text/plain; charset="utf-8"

""",
    {
        "subject": "(no body)",
        "from_name": "",
        "from_email": "empty@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-15T00:00:00Z",
        "body_text_contains": [],
        "body_text_is_empty": True,
        "body_html_present": False,
        "attachments": [],
        "note": "Empty body is legal. It must round-trip as an empty string, not null, and must not fail the non-empty-body_text guarantee by throwing.",
    },
)

# ── 20 folded headers ───────────────────────────────────────────────────────
emit(
    "20-folded-headers",
    """From: Long Sender Name Indeed
 <long@example.com>
To: jatinsethi98@gmail.com,
 second@example.com,
 third@example.com
Subject: A subject that has been folded
 across two physical lines
Date: Sun, 16 Aug 2026 12:00:00 +0000
Content-Type: text/plain; charset="utf-8"

Folded headers above.
""",
    {
        "subject": "A subject that has been folded across two physical lines",
        "from_name": "Long Sender Name Indeed",
        "from_email": "long@example.com",
        "to": ["jatinsethi98@gmail.com", "second@example.com", "third@example.com"],
        "cc": [],
        "date_utc": "2026-08-16T12:00:00Z",
        "body_text_contains": ["Folded headers above."],
        "body_html_present": False,
        "attachments": [],
        "note": "Unfolding joins continuation lines with a single space.",
    },
)

# ── 21 emoji in the subject, via base64 encoded word ────────────────────────
emit(
    "21-emoji-subject",
    f"""From: google@example.com
To: jatinsethi98@gmail.com
Subject: =?UTF-8?B?{b64("Your journey starts now 🚀")}?=
Date: Mon, 17 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="utf-8"

Welcome aboard.
""",
    {
        "subject": "Your journey starts now 🚀",
        "from_name": "",
        "from_email": "google@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-17T09:00:00Z",
        "body_text_contains": ["Welcome aboard."],
        "body_html_present": False,
        "attachments": [],
        "note": "Astral-plane codepoint. Any code that indexes by byte or by UTF-16 unit will slice it in half.",
    },
)

# ── 22 malformed base64 body ────────────────────────────────────────────────
emit(
    "22-bad-base64",
    """From: broken@example.com
To: jatinsethi98@gmail.com
Subject: Broken encoding
Date: Tue, 18 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="utf-8"
Content-Transfer-Encoding: base64

VGhpcyBpcyBmaW5l!!!! not base64 at all @@@@
""",
    {
        "subject": "Broken encoding",
        "from_name": "",
        "from_email": "broken@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-18T09:00:00Z",
        "body_text_contains": [],
        "body_html_present": False,
        "attachments": [],
        "note": "Must degrade gracefully — decode what it can or fall back to the raw text — and must never panic or abort the whole sync. PLAN.md: parse failure yields a metadata row plus an audit entry, never an aborted sync.",
    },
)

# ── 23 unknown charset ──────────────────────────────────────────────────────
emit(
    "23-unknown-charset",
    """From: mystery@example.com
To: jatinsethi98@gmail.com
Subject: Unknown charset
Date: Wed, 19 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="x-nonexistent-charset"

Plain ASCII survives regardless.
""",
    {
        "subject": "Unknown charset",
        "from_name": "",
        "from_email": "mystery@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-19T09:00:00Z",
        "body_text_contains": ["Plain ASCII survives regardless."],
        "body_html_present": False,
        "attachments": [],
        "note": "Unknown charset falls back to UTF-8 then to lossy latin-1. Never an error.",
    },
)

# ── 24 duplicate Subject headers ────────────────────────────────────────────
emit(
    "24-duplicate-headers",
    """From: dup@example.com
To: jatinsethi98@gmail.com
Subject: First subject
Subject: Second subject
Date: Thu, 20 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="utf-8"

Two subject headers.
""",
    {
        "subject": "First subject",
        "from_name": "",
        "from_email": "dup@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-20T09:00:00Z",
        "body_text_contains": ["Two subject headers."],
        "body_html_present": False,
        "attachments": [],
        "note": "First occurrence wins, matching every mail client. Deterministic either way, but pick one and test it.",
    },
)

# ── 25 CRLF vs bare LF line endings mixed ───────────────────────────────────
emit(
    "25-mixed-line-endings",
    (
        "From: mixed@example.com\r\n"
        "To: jatinsethi98@gmail.com\n"
        "Subject: Mixed endings\r\n"
        "Date: Fri, 21 Aug 2026 09:00:00 +0000\n"
        'Content-Type: text/plain; charset="utf-8"\r\n'
        "\r\n"
        "Line one.\n"
        "Line two.\r\n"
    ).encode("utf-8"),
    {
        "subject": "Mixed endings",
        "from_name": "",
        "from_email": "mixed@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-21T09:00:00Z",
        "body_text_contains": ["Line one.", "Line two."],
        "body_html_present": False,
        "attachments": [],
        "note": "Gmail's format=raw is not guaranteed to be pure CRLF. A parser that only splits on CRLF loses the header/body boundary.",
    },
)

# ── 26 very long single line — the size-cap boundary ────────────────────────
long_line = "A" * 200_000
emit(
    "26-huge-body",
    f"""From: huge@example.com
To: jatinsethi98@gmail.com
Subject: Huge body
Date: Sat, 22 Aug 2026 09:00:00 +0000
Content-Type: text/plain; charset="utf-8"

{long_line}
""",
    {
        "subject": "Huge body",
        "from_name": "",
        "from_email": "huge@example.com",
        "to": ["jatinsethi98@gmail.com"],
        "cc": [],
        "date_utc": "2026-08-22T09:00:00Z",
        "body_text_min_length": 100_000,
        "body_html_present": False,
        "attachments": [],
        "note": "The messages.fts generated column truncates at 100k chars. body_text itself is not truncated at parse time; anything that caps it must do so explicitly and say so.",
    },
)

with open(os.path.join(HERE, "expected.json"), "w", encoding="utf-8") as f:
    json.dump(dict(sorted(cases.items())), f, indent=2, ensure_ascii=False)
    f.write("\n")

print(f"wrote {len(cases)} cases + expected.json to {HERE}")
