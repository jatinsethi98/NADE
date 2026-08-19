#!/usr/bin/env bash
#
# A quick cloudflared tunnel, pointed at the running server, with the Pub/Sub
# push subscription re-aimed at it.
#
# "Quick" means no `cloudflared tunnel login`, no Cloudflare-managed domain, and
# a hostname that is different every session. That is the trade: zero human
# setup, but the push subscription and the OAuth redirect URI have to be
# re-pointed each time, which is what this script exists to do.
#
# It does NOT restart the server. Configuration is read once at boot
# (`Config::from_env`), so the new audience only takes effect after a restart -
# and that is printed as a step rather than a suggestion.

set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${NADE_PORT:-8080}"
BIND="${NADE_BIND:-127.0.0.1}"
SUBSCRIPTION="${NADE_PUSH_SUBSCRIPTION:-nade-gmail}"
TOPIC="${NADE_PUSH_TOPIC:-projects/deliveriesapp-293223/topics/gmail-events}"
PUSH_SA="${NADE_PUSH_SA_EMAIL:-nade-push@deliveriesapp-293223.iam.gserviceaccount.com}"
ENV_FILE="${NADE_ENV_FILE:-.env}"
LOG_DIR=".tunnel"
LOG="$LOG_DIR/cloudflared.log"

# A `just` recipe must not silently download a 39 MB binary. Say where it comes
# from and let a human decide.
if ! command -v cloudflared >/dev/null 2>&1; then
    cat >&2 <<'MSG'
cloudflared is not on PATH.

  brew install cloudflared
  # or a static binary:
  curl -L -o ~/.local/bin/cloudflared \
    https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-arm64
  chmod +x ~/.local/bin/cloudflared
MSG
    exit 1
fi

if ! curl -fsS "http://$BIND:$PORT/v1/healthz" >/dev/null 2>&1; then
    echo "No server on http://$BIND:$PORT - start one with \`just run\` first." >&2
    exit 1
fi

mkdir -p "$LOG_DIR"
: >"$LOG"

echo "starting a quick tunnel to http://127.0.0.1:$PORT ..."
cloudflared tunnel --no-autoupdate --url "http://127.0.0.1:$PORT" >>"$LOG" 2>&1 &
TUNNEL_PID=$!
trap 'kill "$TUNNEL_PID" 2>/dev/null || true' EXIT

HOST=""
for _ in $(seq 1 60); do
    HOST="$(grep -Eo 'https://[a-z0-9-]+\.trycloudflare\.com' "$LOG" | head -1 || true)"
    [ -n "$HOST" ] && break
    sleep 0.5
done

if [ -z "$HOST" ]; then
    echo "cloudflared never printed a hostname. Last 20 lines:" >&2
    tail -20 "$LOG" >&2
    exit 1
fi
echo "tunnel: $HOST"

# Prove the tunnel actually reaches us BEFORE pointing Pub/Sub at it. Otherwise
# a dead tunnel is discovered half an hour later as "no mail arrived", with
# nothing local to look at.
echo "checking the tunnel reaches this server ..."
for attempt in $(seq 1 20); do
    if curl -fsS "$HOST/v1/healthz" >/dev/null 2>&1; then
        break
    fi
    if [ "$attempt" -eq 20 ]; then
        echo "the tunnel is up but $HOST/v1/healthz does not answer." >&2
        exit 1
    fi
    sleep 1
done

ENDPOINT="$HOST/v1/webhooks/gmail"

if command -v gcloud >/dev/null 2>&1; then
    echo "pointing the push subscription at $ENDPOINT ..."
    if gcloud pubsub subscriptions describe "$SUBSCRIPTION" >/dev/null 2>&1; then
        gcloud pubsub subscriptions update "$SUBSCRIPTION" \
            --push-endpoint="$ENDPOINT" \
            --push-auth-service-account="$PUSH_SA" \
            --push-auth-token-audience="$ENDPOINT" >/dev/null
    else
        gcloud pubsub subscriptions create "$SUBSCRIPTION" \
            --topic="$TOPIC" \
            --ack-deadline=30 \
            --message-retention-duration=1h \
            --push-endpoint="$ENDPOINT" \
            --push-auth-service-account="$PUSH_SA" \
            --push-auth-token-audience="$ENDPOINT" >/dev/null
    fi
    echo "subscription $SUBSCRIPTION -> $ENDPOINT"
else
    echo "gcloud is not on PATH; point $SUBSCRIPTION at $ENDPOINT by hand." >&2
fi

# `backend/.env` is gitignored, and it is the only file this script writes.
touch "$ENV_FILE"
python3 - "$ENV_FILE" "$ENDPOINT" "$HOST" <<'PY'
import pathlib, sys

path, endpoint, host = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
wanted = {
    "NADE_PUSH_AUDIENCE": endpoint,
    "NADE_GMAIL_REDIRECT_URI": f"{host}/v1/auth/gmail/callback",
}
lines, seen = [], set()
for line in path.read_text().splitlines():
    key = line.split("=", 1)[0].strip() if "=" in line else ""
    if key in wanted:
        lines.append(f"{key}={wanted[key]}")
        seen.add(key)
    else:
        lines.append(line)
for key, value in wanted.items():
    if key not in seen:
        lines.append(f"{key}={value}")
path.write_text("\n".join(lines).rstrip() + "\n")
print(f"wrote NADE_PUSH_AUDIENCE and NADE_GMAIL_REDIRECT_URI to {path}")
PY

cat <<MSG

  ============================================================
  Two things this script cannot do for you:

  1. RESTART THE SERVER.
     Configuration is read once at boot, so until you restart,
     the audience check still uses the old hostname and every
     push answers 401.

  2. Add this redirect URI to the Google OAuth client, by hand:

       $HOST/v1/auth/gmail/callback

     gcloud does not expose redirect URIs for a web client
     created in the console (docs/PHASE0.md H3).

  Then: just gmail-connect
  ============================================================

Ctrl-C stops the tunnel.
MSG

wait "$TUNNEL_PID"
