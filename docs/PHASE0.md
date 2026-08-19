# Phase 0 — the human steps

PLAN.md §Phase 0 shrank once the orchestrator started self-installing tools.
This is what is actually left for Jatin, in order, with everything the
orchestrator can now do itself struck off.

Already done by the orchestrator: rustup/cargo, Google Cloud SDK
(`~/google-cloud-sdk/bin/gcloud`, bundled Python 3.14), the four OFL font faces,
the Cargo workspace, the contract fixtures. Homebrew and Codex CLI were already
on the machine.

---

## ~~H1 — gcloud login~~ ✅ done 2026-08-17

Account `jatinsethi98@gmail.com`, project `deliveriesapp-293223`
(number `730041371062`).

## ~~H2 — Gmail + Pub/Sub plumbing~~ ✅ done 2026-08-17, by the orchestrator

- `gmail.googleapis.com`, `pubsub.googleapis.com`, `iam.googleapis.com`,
  `iamcredentials.googleapis.com` all enabled.
- Topic `projects/deliveriesapp-293223/topics/gmail-events` created.
- `gmail-api-push@system.gserviceaccount.com` → `roles/pubsub.publisher`
  on that topic. Verified in the topic's IAM policy.

## ~~H4 — `cloudflared tunnel login`~~ ❌ dropped at P3

`PLAN.md`'s Phase 0 table defines H4 as a browser click for
`cloudflared tunnel login`. This file supersedes that table, and never had an
H4 section — so it sat between the two documents, owned by neither.

It is **dropped**, deliberately. P3 uses a *quick* tunnel
(`cloudflared tunnel --url`), which needs no login and no Cloudflare-managed
domain. The cost is an ephemeral `*.trycloudflare.com` hostname that changes
every session, so the Pub/Sub push subscription and the OAuth redirect URI have
to be re-pointed each time — which is what `just tunnel` does.

A named tunnel would give a stable hostname and remove that step, but it needs
both the login click and a domain whose DNS is on Cloudflare. There is no domain
before P8 (H10), so it would have blocked P3 on buying one.

## H6 — push subscription · **prerequisites done, subscription waits on P3**

Done already:
- Push identity created: **`nade-push@deliveriesapp-293223.iam.gserviceaccount.com`**
  → this is `PUSH_SA_EMAIL` in `backend/.env`.
- Pub/Sub service agent `service-730041371062@gcp-sa-pubsub.iam.gserviceaccount.com`
  confirmed to exist (holds `roles/pubsub.serviceAgent` on the project) and
  granted **`roles/iam.serviceAccountTokenCreator`** on the push identity —
  the grant PLAN.md finding C10 is about. Without it, authenticated push
  cannot mint OIDC tokens.

Still to do, at P3, by the orchestrator once cloudflared has a hostname:
create the push subscription on `gmail-events` targeting
`https://<tunnel>/v1/webhooks/gmail` with OIDC auth as `nade-push`.

`just tunnel` now does exactly this — create-or-update — every session, because
a quick tunnel's hostname does not survive a restart.

## ~~H3 — OAuth Web client~~ ✅ done 2026-08-17

`backend/secrets/web_client.json`, client id
`730041371062-4sbk548j2ch3h8h2dqtclhdpfb0jvgrh…`, type **web**, project
`deliveriesapp-293223`, redirect URI
`http://localhost:8080/v1/auth/gmail/callback` registered and **verified** —
probing Google's authorize endpoint now reaches the sign-in page instead of
`redirect_uri_mismatch`.

At P3, add `https://<tunnel>/v1/auth/gmail/callback` as a second redirect URI
once the tunnel hostname exists. **This is the one step `just tunnel` cannot
do**: `gcloud` does not expose redirect URIs for a web client created in the
console, so the script prints the URI and you paste it in. With a quick tunnel
it recurs whenever you need the Gmail link flow from a new tunnel. No need to re-download the JSON — redirect
URIs are validated server-side, not read from that file.

Still worth doing on that page: the old Desktop client
(`730041371062-41hi220o5lf53d7pr1msf6d57900s7s6…`) has its secret sitting in
iCloud Drive. Rotate it before anything ships publicly.

## H5 — consent clicks (recurring, ~weekly)

When the orchestrator opens the Gmail consent screen, approve it. The OAuth app
is in Testing mode, so refresh tokens die every 7 days; expect to redo this
about once a week until verification. The backend surfaces it as a
`needs_reauth` banner rather than failing silently.

## H7 — LLM keys (blocks P4)

Put in `/Users/jatinsethi/Projects/NADE/backend/.env` (gitignored):

```
ANTHROPIC_API_KEY=sk-ant-…
# optional, for a cheaper model behind an OpenAI-compatible endpoint:
OPENAI_COMPAT_BASE_URL=
OPENAI_COMPAT_API_KEY=
```

## H8 — send test mail (blocks P3 and P5, ~3 times)

When asked, send an email to jatinsethi98@gmail.com from anywhere. That is how
the push → sync → agent → feed loop gets proven end to end.

## H9 — *optional*, only for push on a physical phone (P6)

Apple Developer Program ($99/yr), App ID `fsaas.NADE` with Push Notifications,
an APNs `.p8` key → `backend/secrets/apns.p8`. **The simulator needs none of
this** — `xcrun simctl push` proves the whole approval flow without paying
anything. Skip until you want it on your actual phone.

## H10 — *P8 only*, deployment

A VPS, a DNS record, SSH access, and Docker Desktop (its first launch needs a
GUI click). Nothing before P8 touches this.
