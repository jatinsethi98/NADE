#!/usr/bin/env bash
#
# The test bench: one command from a clean checkout to a paired app showing
# real mail, on the simulator or on a phone.
#
#   scripts/bench.sh                      1g, the Mail tab root
#   scripts/bench.sh --screen 1e          the inbox list
#   scripts/bench.sh --screen 1f          a real thread (newest, picked for you)
#   scripts/bench.sh --screen 1k          settings
#   scripts/bench.sh --fresh              reinstall, so pairing runs again
#   scripts/bench.sh --lan                what to type on a physical iPhone
#
# Why a script and not a list of commands in a README: the pairing code is
# single-use with a ten-minute TTL, so every reinstall needs a fresh one, and a
# bench that needs a human to retype a secret is a bench that gets skipped.
# `-NADEPairCode` is DEBUG-only and drives the same `MailSync.pair` the Settings
# sheet drives, so what this exercises is the shipping path.
#
# It deliberately does NOT seed fixtures. `--NADESeed` builds the P3 screens
# whose endpoints do not exist yet; this points at the live server, which is the
# only thing that can tell you whether a phase actually landed.

set -euo pipefail
cd "$(dirname "$0")/.."

source "$(dirname "$0")/lib.sh"

APP_ID=fsaas.NADE
SIM="${BENCH_SIM:-iPhone 17 Pro}"
PORT="${NADE_PORT:-8080}"
BASE="http://127.0.0.1:$PORT"
SCREEN=""; MAILBOX=""; THREAD=""; FRESH=0; BUILD=1; LAN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --screen)  SCREEN="$2"; shift 2 ;;
        --mailbox) MAILBOX="$2"; shift 2 ;;
        --thread)  THREAD="$2"; shift 2 ;;
        --sim)     SIM="$2"; shift 2 ;;
        --fresh)   FRESH=1; shift ;;
        --no-build) BUILD=0; shift ;;
        --lan)     LAN=1; shift ;;
        -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

export PATH="$HOME/.cargo/bin:$PATH"

# ---------------------------------------------------------------- backend --

# Started detached rather than assumed: the most common way to look at a blank
# app is to have forgotten this.
if ! curl -fsS --max-time 3 "$BASE/v1/healthz" >/dev/null 2>&1; then
    echo "backend: not running, starting it"
    mkdir -p backend/.bench
    ( cd backend && NADE_ENV=dev nohup cargo run -q -p nade-server \
        >../backend/.bench/server.log 2>&1 & )
    for _ in $(seq 1 120); do
        curl -fsS --max-time 2 "$BASE/v1/healthz" >/dev/null 2>&1 && break
        sleep 1
    done
    if ! curl -fsS --max-time 3 "$BASE/v1/healthz" >/dev/null 2>&1; then
        echo "backend: never became healthy - see backend/.bench/server.log" >&2
        exit 1
    fi
fi
echo "backend: $(curl -fsS "$BASE/v1/healthz")"

# A paired device is not the same as a linked mailbox. Saying which is missing
# beats an empty mailbox list that could mean either.
if ! curl -fsS -H 'Authorization: Bearer '"${NADE_TOKEN:-dev-bench-token}" \
        "$BASE/v1/me" 2>/dev/null | grep -q '"status":"ok"'; then
    echo
    echo "  No Gmail account is linked to this server yet. Run:"
    echo "      cd backend && just gmail-connect"
    echo "  and approve the consent screen it prints, then re-run this."
    echo
fi

if [ "$LAN" = 1 ]; then
    IP="$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true)"
    HOSTNAME_LOCAL="$(scutil --get LocalHostName 2>/dev/null || true).local"
    echo
    echo "On the iPhone, in NADE > Mail > Settings > Server, type one of:"
    [ -n "$IP" ] && echo "    http://$IP:$PORT"
    echo "    http://$HOSTNAME_LOCAL:$PORT"
    echo
    echo "Then Pair this device, with this code:"
    echo "    $(cd backend && cargo run -q -p nade-server -- --print-pair-code 2>&1 \
            | grep -Eo '[0-9]{6}' | head -1)"
    echo
    echo "Both need the phone on the same Wi-Fi as this Mac, and the server"
    echo "bound to 0.0.0.0 (NADE_BIND in backend/.env). iOS raises a local-network"
    echo "prompt the first time - allow it, or the requests fail with no error."
    exit 0
fi

# --------------------------------------------------------------------- app --

if [ "$BUILD" = 1 ]; then
    echo "build: xcodebuild Debug"
    xcodebuild -project "$PROJECT" -scheme "$SCHEME" -configuration Debug \
        -destination "platform=iOS Simulator,name=$SIM" build \
        >/tmp/nade-bench-build.log 2>&1 \
        || { echo "build failed - tail of /tmp/nade-bench-build.log:" >&2
             tail -30 /tmp/nade-bench-build.log >&2; exit 1; }
fi

PRODUCTS="$(DESTINATION="platform=iOS Simulator,name=$SIM" products_dir Debug)"
[ -d "$PRODUCTS/NADE.app" ] || { echo "no NADE.app in $PRODUCTS" >&2; exit 1; }

xcrun simctl boot "$SIM" >/dev/null 2>&1 || true
xcrun simctl bootstatus "$SIM" -b >/dev/null 2>&1 || true
open -a Simulator

xcrun simctl terminate "$SIM" "$APP_ID" >/dev/null 2>&1 || true
if [ "$FRESH" = 1 ]; then
    xcrun simctl uninstall "$SIM" "$APP_ID" >/dev/null 2>&1 || true
    # Uninstalling does NOT clear the simulator keychain, and the device token
    # lives there. Without this, --fresh reinstalls an app that is still paired,
    # `-NADEPairCode` correctly declines to spend a second code, and a run meant
    # to exercise first-contact silently exercises nothing.
    xcrun simctl keychain "$SIM" reset >/dev/null 2>&1 || true
fi
xcrun simctl install "$SIM" "$PRODUCTS/NADE.app"

# 1f needs a thread that exists. The fixture default does not, on a live server,
# so the screen would come up empty and look like a bug in the thread view.
if [ "$SCREEN" = "1f" ] && [ -z "$THREAD" ]; then
    THREAD="$(curl -fsS -H 'Authorization: Bearer '"${NADE_TOKEN:-dev-bench-token}" \
        "$BASE/v1/mailboxes/${MAILBOX:-INBOX}/threads?limit=1" 2>/dev/null \
        | python3 -c 'import json,sys
try:
    t = json.load(sys.stdin)["threads"]
    print(t[0]["id"] if t else "")
except Exception:
    print("")' )"
    [ -n "$THREAD" ] && echo "thread: $THREAD (newest)" \
        || echo "thread: none found - is the mailbox empty?" >&2
fi

ARGS=()
[ -n "$SCREEN" ]  && ARGS+=(-NADEScreen "$SCREEN")
[ -n "$MAILBOX" ] && ARGS+=(-NADEMailbox "$MAILBOX")
[ -n "$THREAD" ]  && ARGS+=(-NADEThread "$THREAD")

# Minted last, so its ten-minute clock starts as late as possible. A no-op when
# the device is already paired - the app checks before spending it.
CODE="$(cd backend && cargo run -q -p nade-server -- --print-pair-code 2>&1 \
        | grep -Eo '[0-9]{6}' | head -1)"
[ -n "$CODE" ] && ARGS+=(-NADEPairCode "$CODE")

# Pair on a throwaway launch first, then open the screen you asked for.
#
# Pairing is asynchronous, so a deep-linked screen renders before the token
# exists, fails its own page load, and keeps that error: 1g and 1k rebuild
# themselves from the account observation and recover, but a *pushed* list (1e,
# 1f) holds the failure in its own model and only a reload clears it. That is a
# race this script creates by deep-linking into a state the product cannot
# reach on its own - from 1g there is nothing to tap into while unpaired - so it
# is fixed here rather than in the app.
#
# Only when --fresh guaranteed the keychain is empty. Otherwise the device is
# almost certainly still paired, the code is declined as a no-op, and paying two
# launches on every iteration would be the wrong trade.
if [ "$FRESH" = 1 ] && [ -n "$CODE" ]; then
    xcrun simctl launch "$SIM" "$APP_ID" -NADEPairCode "$CODE" >/dev/null
    # The code file is unlinked the moment the code is spent - that `unlink` is
    # what makes single-use atomic across processes (backend DECISIONS D4) - so
    # its disappearance is the pairing having actually landed, not a guess at
    # how long a round trip takes.
    CODE_FILE="${NADE_PAIR_CODE_FILE:-backend/secrets/pair-code.json}"
    for _ in $(seq 1 40); do
        [ -e "$CODE_FILE" ] || break
        sleep 0.25
    done
    [ -e "$CODE_FILE" ] && echo "warning: pairing code was never spent" >&2
    sleep 1   # the Keychain write and the first refresh follow the unlink
    xcrun simctl terminate "$SIM" "$APP_ID" >/dev/null 2>&1 || true
fi

xcrun simctl launch "$SIM" "$APP_ID" "${ARGS[@]}" >/dev/null
echo "launched: $SIM${SCREEN:+ at $SCREEN}"
