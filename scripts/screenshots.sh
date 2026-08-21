#!/bin/bash
#
# The P2 screenshot set — `docs/DESIGN.md` §5: "Every screen lands with a
# `simctl` screenshot in `docs/screens/<id>.png` beside the design render, from
# the phase it first exists."
#
# Checked in, unlike P1's (IOS_DECISIONS D25 kept that one in a scratchpad
# because it was three calls in a loop). This one is not: it pins a clock, a
# time zone and a status bar, and every one of those is load-bearing. A shot set
# that cannot be regenerated the same way twice is not evidence of anything.
#
#   -NADENow    the fixture world is frozen but `listTime` is a function of now,
#               so without it today's shot says "Yesterday" and next week's says
#               "17 Aug".
#   TZ=UTC      "today" is the *device's* calendar day, so the row times shift
#               with the machine's region unless it is pinned.
#   status_bar  a real clock and a stray notification banner both changed a P1
#               screenshot (D45).

set -euo pipefail
cd "$(dirname "$0")/.."

source "$(dirname "$0")/lib.sh"

APP_ID=fsaas.NADE
NOW=2026-08-17T12:00:00Z
OUT=docs/screens
AX5=UICTContentSizeCategoryAccessibilityXXXL

DEVICES=("iPhone 17 Pro:p2" "iPhone SE (3rd generation):p2-se")

# P3's screens. Their own list because they live on the Ask tab and take
# different arguments; the loop below shoots both sets per device.
#
# Four fields, `name|screen|query|agent`, and **not** a pre-quoted argument
# string: `UserDefaults` reads `-key value` pairs out of argv, so a query that
# word-splits sets `NADEQuery` to its first word and leaves the rest as stray
# arguments. Every 1a shot would have rendered the same "What" stream.
P3_SHOTS=(
    "2a|2a||"
    "2a-focus|2a-focus||"
    "1b-agents|1b||"
    "1c-builder|1c||"
    "1c-compile-failed|1c||a0000004-0000-4000-8000-000000000004"
    "1a-answer|1a|What did I miss from Stripe this week?|"
    "1a-results|1a|from:priya@acme.com|"
    "1a-draft|1a|When a recruiter emails, save the next steps|"
)

shoot() {  # device prefix name args...
    local device="$1" prefix="$2" name="$3"; shift 3
    xcrun simctl terminate "$device" "$APP_ID" >/dev/null 2>&1 || true
    SIMCTL_CHILD_TZ=UTC xcrun simctl launch "$device" "$APP_ID" \
        -NADEGallery 0 -NADENow "$NOW" "$@" >/dev/null
    sleep 3
    xcrun simctl io "$device" screenshot --type=png "$OUT/$prefix-$name.png" >/dev/null 2>&1
    echo "  $prefix-$name.png"
}

PRODUCTS=$(products_dir Debug)

for entry in "${DEVICES[@]}"; do
    device="${entry%%:*}"
    prefix="${entry##*:}"
    echo "$device →"

    xcrun simctl boot "$device" >/dev/null 2>&1 || true
    xcrun simctl bootstatus "$device" -b >/dev/null 2>&1 || true
    xcrun simctl install "$device" "$PRODUCTS/NADE.app"
    xcrun simctl status_bar "$device" override \
        --time 9:41 --batteryState charged --batteryLevel 100 \
        --cellularMode active --cellularBars 4 --wifiBars 3 >/dev/null 2>&1 || true

    shoot "$device" "$prefix" 1g-mailboxes      -NADESeed fixtures -NADEScreen 1g
    shoot "$device" "$prefix" 1g-needs-gmail    -NADESeed empty    -NADEScreen 1g
    shoot "$device" "$prefix" 1e-inbox          -NADESeed fixtures -NADEScreen 1e -NADEMailbox INBOX
    shoot "$device" "$prefix" 1e-label          -NADESeed fixtures -NADEScreen 1e -NADEMailbox Label_12
    shoot "$device" "$prefix" 1e-empty          -NADESeed fixtures -NADEScreen 1e -NADEMailbox CATEGORY_SOCIAL
    shoot "$device" "$prefix" 1e-offline        -NADESeed offline  -NADEScreen 1e -NADEMailbox INBOX
    shoot "$device" "$prefix" 1f-thread         -NADESeed fixtures -NADEScreen 1f -NADEThread 18f2a1b3c4d5e6f7
    shoot "$device" "$prefix" 1f-partial        -NADESeed fixtures -NADEScreen 1f -NADEMailbox Label_12 -NADEThread 18f1d8e7c6b5a409
    shoot "$device" "$prefix" 1k-settings       -NADESeed fixtures -NADEScreen 1k
    shoot "$device" "$prefix" 1e-ax5            -NADESeed fixtures -NADEScreen 1e -NADEMailbox INBOX \
        -UIPreferredContentSizeCategoryName "$AX5"
    shoot "$device" "$prefix" 1f-ax5            -NADESeed fixtures -NADEScreen 1f -NADEThread 18f2a1b3c4d5e6f7 \
        -UIPreferredContentSizeCategoryName "$AX5"

    for entry in "${P3_SHOTS[@]}"; do
        IFS='|' read -r name screen query agent <<<"$entry"
        args=(-NADESeed fixtures -NADEScreen "$screen")
        [ -n "$query" ] && args+=(-NADEQuery "$query")
        [ -n "$agent" ] && args+=(-NADEAgent "$agent")
        shoot "$device" "${prefix/p2/p3}" "$name" "${args[@]}"
    done

    xcrun simctl status_bar "$device" clear >/dev/null 2>&1 || true
done

echo
echo "checking for duplicates…"
# D41's lesson: a shot that is byte-identical to another one is a shot that was
# never taken. `p1-gallery-13-motion.png` was a copy of `-12-ax5b.png` because
# the scroll clamped at the end of the content, and nothing noticed.
DUPES=$(shasum -a 256 "$OUT"/p2-*.png "$OUT"/p3-*.png | awk '{ print $1 }' | sort | uniq -d)
if [ -n "$DUPES" ]; then
    echo "FAIL: byte-identical screenshots:" >&2
    for hash in $DUPES; do shasum -a 256 "$OUT"/p2-*.png "$OUT"/p3-*.png | grep "^$hash" >&2; done
    exit 1
fi
echo "ok: $(ls "$OUT"/p2-*.png "$OUT"/p3-*.png | wc -l | tr -d ' ') screenshots, all distinct"
