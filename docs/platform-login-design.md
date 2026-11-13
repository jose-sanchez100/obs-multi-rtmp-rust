# Design: platform accounts (YouTube and Facebook login)

**Goal:** work like Restream. Connect a YouTube channel and a Facebook Page once; before going
live, type a title and description in one place; pressing **Start** creates the broadcast on
each platform, gets its ingest address and stream key, and starts streaming. Pressing **Stop**
ends the broadcasts. Nobody has to open YouTube Studio or Facebook's Live Producer, or copy a
stream key.

Today the plugin needs a fixed stream key per target, which falls short on both platforms:

- **Facebook:** a Page's persistent stream key only works after someone opens the Page and
  creates a live video in Live Producer. That manual step is what this feature removes.
- **YouTube:** the default stream key goes live with whatever title and description were set
  last; changing them means opening YouTube Studio.

## Why OBS's own login cannot be reused

OBS's account integration (YouTube, Twitch, Restream) lives in the OBS frontend (`UI/auth-*`,
`UI/youtube-api-*`), not in `obs-frontend-api`, so plugins cannot reach its tokens or its
OAuth clients. The client IDs and secrets are injected into official OBS builds at build time.
OBS has no Facebook login at all. The plugin needs its own login and its own API calls.

## Platform flows

### Facebook Page

1. **Login** (once): Facebook Login with `pages_show_list`, `pages_read_engagement`,
   `pages_manage_posts` and `publish_video`. Exchange the short-lived user token for a
   long-lived one (about 60 days), then read `/me/accounts` to list the Pages the user
   administers and their Page tokens. A Page token derived from a long-lived user token has
   no expiry date, but it can still be invalidated (password change, permission removed), so
   the plugin must handle "reconnect needed".
2. **Start:** `POST /{page-id}/live_videos` with `status=LIVE_NOW`, `title`, `description`
   (and optionally privacy). The response has the LiveVideo `id` and `secure_stream_url`
   (`rtmps://…/rtmp/<key>`). The plugin splits that into server and key and starts the
   output. The video goes live as soon as data arrives; no Live Producer step.
3. **While live:** `POST /{live-video-id}` with a new `title`/`description` updates them.
4. **Stop:** stop the output, then `POST /{live-video-id}` with `end_live_video=true`, which
   turns the broadcast into a saved video.

### YouTube channel

1. **Login** (once): Google OAuth for installed apps (authorization code + PKCE, loopback
   redirect on `127.0.0.1`), scope `https://www.googleapis.com/auth/youtube`, offline access
   for a refresh token.
2. **Stream key:** `liveStreams.list(mine=true)` lists the channel's reusable streams; the
   user picks one in the target (normally the one whose key they use today). Its
   `cdn.ingestionInfo` gives `ingestionAddress`, `backupIngestionAddress` and `streamName`
   (the key), so a Live + Backup target in the Rust engine gets both ingests from one call.
   The key does not change between broadcasts.
3. **Start:** `liveBroadcasts.insert` with `snippet.title`, `snippet.description`,
   `snippet.scheduledStartTime` (now), `status.privacyStatus`, and
   `contentDetails.enableAutoStart=true` / `enableAutoStop=true`; then
   `liveBroadcasts.bind` to the chosen stream; then start the output. With auto-start,
   YouTube moves the broadcast to live once the stream is active. Without it, the plugin polls
   `liveStreams.list` until `status.streamStatus` is `active` and calls
   `liveBroadcasts.transition(live)`.
4. **While live:** `liveBroadcasts.update` changes the title and description.
5. **Stop:** stop the output; auto-stop completes the broadcast (or
   `liveBroadcasts.transition(complete)`).

Quota: the default allocation is 10,000 units a day; `insert` and `update` cost 50 units each,
so one start costs on the order of 100–150 units. That is dozens of starts a day per project,
plenty for one church, but not for a shared OAuth client used by many people (see below).

## Changes in the plugin

### Start becomes two-phase

Today `Core::start` → `Runtime::start` (`plugin/src/target.rs`) builds the service from the
target's fixed `service_param` and starts the output immediately, on the UI thread. For an
account target:

1. `Runtime::start` sets a new **Preparing** state (shown in the dock) and hands the API calls
   to a worker thread. No network call runs on the UI thread or under the core lock.
2. The worker creates the broadcast, then posts the result back with `with_core_or_retry`
   (`plugin/src/core.rs`), including the generation, so a Stop pressed meanwhile wins.
3. On the UI thread, the server and key from the API replace `service_param` for this run
   only (they are never written to `obs-multi-rtmp.json`), and the existing `start_inner`
   path starts the output.
4. On failure, the dock shows the platform's error message. An optional per-target "fall back
   to the saved stream key" setting lets the target still stream (without a new title).

Stop runs the output stop first, then the "end broadcast" call on the worker.
Sync start with the main OBS stream still works; the API calls add a second or two.

### Shared broadcast info

The dock follows Restream's channel list:

- An **Update titles** button in the dock header opens one form: title, description, and
  YouTube privacy (public/unlisted/private). Every account target uses it, with an optional
  per-target override (for example a different description on Facebook). Saving it while
  targets are live sends the update calls.
- Each account target's row shows the platform icon, the channel or Page name, the title it
  will use (or is live with), and for YouTube a privacy badge. The row keeps the existing
  enable checkbox, which plays the role of Restream's per-channel switch, and an **Edit**
  link to the per-target override.
- The existing **Start all** / **Stop all** buttons start or stop every enabled target.

The form keeps the last values, and a short list of templates (for example "Sunday service")
is a later addition.

### Target editor

New protocols next to the existing ones:

- **YouTube (account):** account, reusable stream (key), Live + Backup toggle.
- **Facebook (account):** account, Page.

Encoders, scene, audio tracks and the Rust session engine options are the same as for the
"RTMP/RTMPS (Rust session engine)" protocol, since the transport is the same; only where the
server and key come from changes.

### Accounts and secrets

- **Accounts** section in the Advanced menu: Connect / Disconnect for YouTube and Facebook,
  and the name of each connected account.
- Tokens go to the operating system's credential store (Windows Credential Manager, macOS
  Keychain, Secret Service on Linux), keyed by platform and account ID. The profile's
  `obs-multi-rtmp.json` stores only the account ID and display name, so copying or sharing a
  profile never leaks a token.
- Login opens the system browser; the plugin listens on a loopback port for the redirect.

### New dependencies

- An HTTPS client using the `rustls` the engine already has (for example `ureq` with rustls),
  plus `serde_json` (already used).
- A credential-store crate (for example `keyring`).

## OAuth apps: the real constraint

Both platforms require an app registered by whoever ships the login, and both restrict apps
that have not been reviewed:

| | Without review | For anyone to use it |
|---|---|---|
| **YouTube** | App in "Testing": up to 100 test users, and refresh tokens expire **7 days** after consent (weekly re-login) | Publish the app; the `youtube` scope is sensitive, so Google requires verification (privacy policy, domain, demo video) |
| **Facebook** | Only people with a role on the app (admin, developer, tester) | App Review for `publish_video` and the Live Video API; Meta's docs also mention business verification |

Plan for that:

1. **Bring your own app:** the Accounts section has fields for a Google OAuth client ID
   (desktop type) and a Meta app ID. A user who registers their own apps (the README gets a
   step-by-step guide) uses the feature for their own channel and Page with no review:
   on Facebook as the app's admin; on YouTube by publishing the app unverified (consent
   screen shows an "unverified app" warning, but tokens no longer expire weekly) or keeping it
   in Testing and reconnecting weekly.
2. **Built-in apps, later:** if the project registers and gets its own apps reviewed, ship
   their IDs as defaults so most users just click Connect. The bring-your-own fields remain
   for anyone who runs out of quota.

## Open questions to check during the prototype

- **Facebook desktop login:** which redirect Facebook accepts for a desktop app (a loopback
  `http://127.0.0.1:<port>` URI, or the `login_success.html` page with the token read from the
  browser) and whether "Login for Devices" allows the page and video permissions.
- **Facebook app type:** Business apps replaced Development/Live mode with access levels;
  confirm that an app admin can use `publish_video` on their own Page without review.
- **Facebook backup ingest:** whether a Page live video can expose a backup RTMPS URL like
  YouTube's.
- **YouTube auto-start timing** with a reused bound stream: how long after data arrives the
  broadcast goes live, and whether polling plus `transition` is more predictable.
- **Ending on crash:** if OBS exits without Stop, Facebook ends the video after the stream
  stops and YouTube's auto-stop completes the broadcast; confirm both, and add a cleanup of
  stale broadcasts on next start if not.

## Phases

1. **Facebook Page** (bring-your-own app): login, Page picker, create/update/end live video,
   Preparing state in the dock, credential store. This removes the Live Producer step.
2. **YouTube** (bring-your-own client ID): login, stream picker, insert/bind/auto-start,
   update while live, Live + Backup from the stream's ingestion info.
3. **Broadcast info** shared form with per-target overrides, then templates. Twitch could
   join here (its API sets the channel's title and category; the stream key is fixed).
4. Built-in OAuth apps, if the project goes through Google verification and Meta App Review.
