#!/bin/bash
#
# P2 A6 — a Release build must not ship the DEBUG fixture world.
#
# This is a script and not a test on purpose. XCTest runs inside the simulator
# against whatever configuration was built, and that is always Debug — the same
# limitation IOS_DECISIONS D36 works around by parsing `project.pbxproj`. Only a
# build can answer a question about Release.
#
# The obvious form of this check does not work:
#
#     find "$DIR/NADE.app" -name '*.json'    # → exits 0 whether it finds 0 or 10
#
# `find` succeeds when it finds nothing *and* when it finds everything, so a
# comment saying "→ calendar.json only" is documentation, not a gate. This
# resolves BUILT_PRODUCTS_DIR from the build system rather than guessing at a
# path, and exits non-zero on any fixture that is not the calendar stub.

set -euo pipefail

cd "$(dirname "$0")/.."

source "$(dirname "$0")/lib.sh"

echo "building $SCHEME for Release…"
xcodebuild -project "$PROJECT" -scheme "$SCHEME" \
    -configuration Release -sdk iphonesimulator \
    -destination "$DESTINATION" build >/dev/null

PRODUCTS=$(products_dir Release)

APP="$PRODUCTS/$SCHEME.app"
if [ ! -d "$APP" ]; then
    echo "FAIL: no Release app at $APP — the check never looked at anything" >&2
    exit 1
fi

# `calendar.json` is the one fixture v1 genuinely ships: DESIGN.md §1j makes the
# Calendar tab a stub rendered from it, and PLAN.md's parity map says so.
# `-not -path '*/PlugIns/*'` matters: a Debug .app embeds NADETests.xctest,
# which carries all 59 contract fixtures by design (IOS_DECISIONS D4). Counting
# those would make the cross-check below pass for entirely the wrong reason —
# and would make this look like it had inspected the app when it had inspected
# the tests.
UNEXPECTED=$(find "$APP" -name '*.json' ! -name 'calendar.json' -not -path '*/PlugIns/*' | sed "s|$APP/||" || true)

if [ -n "$UNEXPECTED" ]; then
    echo "FAIL: a Release build carries fixture mail:" >&2
    echo "$UNEXPECTED" | sed 's/^/  /' >&2
    echo "" >&2
    echo "EXCLUDED_SOURCE_FILE_NAMES on the app target's Release configuration" >&2
    echo "is what keeps NADE/Fixtures/mail out of the product." >&2
    exit 1
fi

# The other half: the check must be able to see a fixture if one were there.
# A Debug build carries all nine, so if *that* comes back clean the search
# itself is broken and the Release pass above proved nothing.
DEBUG_PRODUCTS=$(products_dir Debug)
DEBUG_APP="$DEBUG_PRODUCTS/$SCHEME.app"

if [ -d "$DEBUG_APP" ]; then
    DEBUG_FIXTURES=$(find "$DEBUG_APP" -name '*.json' ! -name 'calendar.json' -not -path '*/PlugIns/*' | wc -l | tr -d ' ')
    if [ "$DEBUG_FIXTURES" -eq 0 ]; then
        echo "FAIL: the Debug build carries no fixtures either, so this check" >&2
        echo "cannot distinguish 'excluded from Release' from 'never built'." >&2
        exit 1
    fi
    echo "ok: Debug carries $DEBUG_FIXTURES fixtures, Release carries none"
else
    echo "ok: Release carries no fixture mail (no Debug build present to cross-check)"
fi
