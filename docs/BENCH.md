# The test bench

One command from a clean checkout to a paired app showing real mail. It exists
because every phase from here on has to be *looked at* on a device, and a bench
that needs a human to retype a ten-minute secret is a bench that gets skipped.

```sh
scripts/bench.sh                      # 1g, the Mail tab root
scripts/bench.sh --screen 1e          # the inbox list
scripts/bench.sh --screen 1f          # a real thread (newest, picked for you)
scripts/bench.sh --screen 1k          # settings
scripts/bench.sh --fresh              # reinstall + wipe the keychain, so pairing runs again
scripts/bench.sh --lan                # what to type on a physical iPhone
scripts/bench.sh --no-build           # skip xcodebuild
scripts/bench.sh --sim "iPhone SE (3rd generation)"
```

It starts the backend if it is not already up, builds Debug, installs on the
simulator, mints a pairing code and pairs the app with it, then deep-links to
the screen you named.

**It never seeds fixtures.** `-NADESeed` builds the P3 screens whose endpoints
do not exist yet; the bench points at the live server, which is the only thing
that can tell you whether a phase actually landed.

## What it needed from the app

Two DEBUG-only launch arguments, both in `NADE/NADEApp.swift`:

- **`-NADEScreen` now works on a live launch.** It was reachable only from the
  seeded path, so the bench could deep-link into the fixture world but not into
  the real one — backwards, since the live screens are the ones a phase gate has
  to look at. A live launch still keeps the shipping default tab unless the
  argument is actually passed.
- **`-NADEPairCode <six digits>`** pairs on launch, through the same
  `MailSync.pair` the Settings sheet drives, so what the bench exercises is the
  shipping path. It declines to spend a second code when the device is already
  paired, and prints to the console when a code is refused.

`--fresh` also runs `simctl keychain reset`. Uninstalling does not clear the
simulator keychain, and the device token lives there — without the reset,
`--fresh` reinstalls an app that is still paired and a run meant to exercise
first contact silently exercises nothing.

After a `--fresh`, pairing happens on a throwaway launch before the screen you
asked for is opened. Pairing is asynchronous: a deep-linked screen renders
before the token exists, fails its own page load, and keeps that error. 1g and
1k rebuild from the account observation and recover; a *pushed* list (1e, 1f)
holds the failure in its own model until something reloads it. The product
cannot reach that state on its own — from 1g there is nothing to tap into while
unpaired — so it is a race the deep link creates and the script fixes, not a bug
in the screens. The wait is on `backend/secrets/pair-code.json` disappearing:
that `unlink` is what makes single-use atomic across processes (backend
`DECISIONS.md` D4), so it is the pairing having landed rather than a guess at
how long a round trip takes.

## The one-time setup behind it

`backend/.env` (gitignored, written from `.env.example`) binds the server to
`0.0.0.0:8080` so a phone on the same Wi-Fi can reach it, and sets
`NADE_TOKEN=dev-bench-token` as the dev bearer so the bench can curl without
pairing. Both are inert outside `NADE_ENV=dev`.

Linking a Gmail account is still a human consent click, ~weekly, because the
OAuth app is in Testing mode and refresh tokens die every 7 days (PHASE0 H5):

```sh
cd backend && just gmail-connect     # prints a URL; approve it in a browser
```

The bench says so explicitly when `/v1/me` is not `ok`, rather than showing an
empty mailbox list that could mean either "not linked" or "no mail".

## On a physical iPhone

`scripts/bench.sh --lan` prints the LAN URL and a pairing code. On the phone:
**Mail → Settings → Server**, type the URL, then **Pair this device**.

Both machines must be on the same Wi-Fi, and iOS raises a local-network
permission prompt the first time — allow it, or requests fail with no error.
`NADE/Info.plist` already carries `NSAllowsLocalNetworking` and
`NSLocalNetworkUsageDescription` for exactly this.

Installing on hardware needs a cable and the personal signing team already in
the project (`DEVELOPMENT_TEAM = T6BTHTJ6Y8`). A free team's build expires after
7 days; the Apple Developer Program ($99/yr) is what buys TestFlight, 90-day
installs, and the push entitlement P6 needs (PHASE0 H9).

## How current the mail is

With no tunnel, **push is off** — the webhook is fail-closed without
`NADE_PUSH_SA_EMAIL` and `NADE_PUSH_AUDIENCE`, deliberately, because `aud` alone
is forgeable. What keeps mail current instead is `NADE_POLL_INTERVAL_MINS=30`:
the backend re-reads history at most 30 minutes after a change. The app then
picks it up on its next foreground refresh; there is no pull-to-refresh.

So the honest MVP expectation is **up to ~30 minutes**, not seconds. To get the
≤60 s that P3's acceptance criterion actually names, run a tunnel:

```sh
cd backend && just tunnel     # quick tunnel + re-aims the Pub/Sub subscription
```

and paste the printed `https://<host>/v1/auth/gmail/callback` into the Google
OAuth client's redirect URIs — the one step the script cannot do (PHASE0 H3).
The hostname changes every session, which is the price of not owning a
Cloudflare-managed domain.

## The two legs simctl cannot set

Liquid Glass adapts to **Reduce Transparency** and **Increase Contrast**, and
neither is reachable from `xcrun simctl ui` the way `appearance` and
`content_size` are. `scripts/screenshots.sh` therefore does not shoot them, and
the gallery cannot fake them either — both are read-only environment values, the
same problem `GalleryView`'s Reduce Motion section already documents.

Nor is the back door: `xcrun simctl spawn <device> defaults write
com.apple.Accessibility ReduceTransparencyEnabled -bool true` writes the key and
reads it back, and the app relaunches with its glass entirely unchanged — posting
`ReduceTransparencyChangedNotification` does not help either. Tried on iOS 26.5,
recorded here so it is not tried twice.

They are looked at by hand, and the glass chrome is the reason to bother:

1. In the simulator, **Settings → Accessibility → Display & Text Size**.
2. Turn on **Reduce Transparency**, then **Increase Contrast**, one at a time.
3. Re-shoot the two places the material carries type over moving content —
   `scripts/bench.sh --screen 1e` scrolled, and the gallery's glass section
   (`-NADEGallery 1 -NADEGallerySection glass`).

What you are checking is legibility, not fidelity: the system is expected to
replace the material with something flatter, and the design's own contrast has
none to spare — the Classical accent-to-ground pair is tuned to 3:1, "enough for
icons, large text and interface chrome, **not for body copy**".
