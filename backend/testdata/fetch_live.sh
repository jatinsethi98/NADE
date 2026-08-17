#!/usr/bin/env bash
# Pull a sample of real messages out of the live Gmail account as raw RFC-822,
# for offline smoke tests. Output goes to backend/testdata/live/, which is
# gitignored — this is personal mail and never belongs in the repository.
#
#   backend/testdata/fetch_live.sh [count]      (default 60)
#
# The conformance corpus in backend/testdata/mime/ is what specifies the
# parser. This sample only answers a different question: does the parser
# survive contact with reality — no panics, no empty bodies, no lost headers.

set -euo pipefail

COUNT="${1:-60}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$HERE/live"
CRED="${NADE_GMAIL_CREDENTIALS:-$HOME/Library/Mobile Documents/com~apple~CloudDocs/Desktop/current_projects/tools/credentials.json}"
TOKEN_FILE="${NADE_GMAIL_TOKEN:-$DEST/token.json}"

if [ ! -f "$CRED" ]; then
  echo "no OAuth client at: $CRED" >&2
  echo "set NADE_GMAIL_CREDENTIALS to point at one" >&2
  exit 1
fi
if [ ! -f "$TOKEN_FILE" ]; then
  echo "no refresh token at: $TOKEN_FILE" >&2
  echo "set NADE_GMAIL_TOKEN, or run the backend's OAuth flow and export the token there" >&2
  exit 1
fi

CID=$(jq -r '.installed.client_id // .web.client_id' "$CRED")
CSEC=$(jq -r '.installed.client_secret // .web.client_secret' "$CRED")
RT=$(jq -r '.refresh_token' "$TOKEN_FILE")

AT=$(curl -s -X POST https://oauth2.googleapis.com/token \
  -d "client_id=$CID" -d "client_secret=$CSEC" \
  -d "refresh_token=$RT" -d "grant_type=refresh_token" | jq -r '.access_token // empty')

if [ -z "$AT" ]; then
  echo "token refresh failed — the OAuth app is in Testing mode, so refresh tokens die after 7 days." >&2
  echo "re-consent and refresh the token file, then run this again." >&2
  exit 1
fi

mkdir -p "$DEST/raw"
curl -s -H "Authorization: Bearer $AT" \
  "https://gmail.googleapis.com/gmail/v1/users/me/messages?q=newer_than%3A30d&maxResults=500" \
  > "$DEST/list.json"

jq -r '.messages[].id' "$DEST/list.json" | head -"$COUNT" > "$DEST/ids.txt"

n=0
while IFS= read -r id; do
  [ -z "$id" ] && continue
  out="$DEST/raw/${id}.eml"
  curl -s -H "Authorization: Bearer $AT" \
    "https://gmail.googleapis.com/gmail/v1/users/me/messages/${id}?format=raw" \
    | jq -r '.raw // empty' | base64 -d > "$out" 2>/dev/null || true
  if [ -s "$out" ]; then n=$((n + 1)); else rm -f "$out"; fi
done < "$DEST/ids.txt"

echo "fetched $n raw messages into $DEST/raw"
