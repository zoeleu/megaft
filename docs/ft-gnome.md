# ft_gnome

Your 42 logtime and upcoming evaluations in the GNOME top bar, with a small server that handles
42 sign-in, captcha and per-device access.

This project merges three earlier ones:

| Earlier project | What it gave this one |
| --- | --- |
| `ftgnome` (extension skeleton) | UUID `ftgnome@zoe.dev.br`, the logtime idea, the prefs toggles |
| `fortytwo-gnome-api` (Bun + Hono) | stack, Turnstile → 42 OAuth flow, AES-GCM token encryption |
| `next42eval@42.school` | GNOME 42 patterns (legacy imports, Soup 2.4/3 compat), evaluation display |

```
ft-gnome/
├── server/                  Bun + Hono + bun:sqlite
│   ├── src/
│   │   ├── index.ts         entry point
│   │   ├── app.ts           routes + security headers (CSP, HSTS, …)
│   │   ├── routes/web.tsx   login (Turnstile → 42 OAuth), /link, /devices dashboard
│   │   ├── routes/api.ts    device flow + bearer API for the extension
│   │   ├── services/        devices (tokens, rotation), users (42 tokens), summary
│   │   └── …                config, crypto, db (migrations), intra client, views
│   ├── test/                bun test (full flows with a fake 42 API)
│   └── scripts/fake-server.ts   dev server with fake 42 + always-pass captcha
├── extension/
│   ├── ftgnome@zoe.dev.br/  the GNOME Shell 42 extension (plain GJS, no build step)
│   └── test-client.js       gjs integration test for lib/http.js + lib/client.js
└── Makefile                 pack / install / test
```

## How sign-in works

The extension never sees your 42 tokens or the 42 app secret. It gets its **own** revocable
token through a device-authorisation flow (the "enter this code on your TV" pattern, RFC 8628):

```
 GNOME extension                     ft_gnome server                    Browser / 42
 ───────────────                     ───────────────                    ────────────
 "Link this computer…"
   POST /api/v1/device/code  ───────▶ creates request, returns
                                      user_code BCDF-GHJK + device_code
   opens browser ────────────────────────────────────────────────────▶ /link?code=BCDF-GHJK
                                                                        not signed in → /login
                                                                        Turnstile captcha ✔
                                      verifies captcha (siteverify) ◀── POST /login
                                      state cookie + DB row ──────────▶ 42 OAuth authorize
                                      /auth/callback: checks state,  ◀─ code
                                      exchanges code, stores 42 tokens
                                      encrypted, starts web session
                                                                        "Approve this device?"
                                      marks request approved        ◀── POST /link (CSRF token)
   polls POST /device/token  ───────▶ issues access + refresh token
   stores them in GNOME Keyring
   GET /api/v1/summary (Bearer) ────▶ calls 42 with the user's token, caches 60s
```

### Security measures

**Accounts and sessions**
- Turnstile is checked server-side before any OAuth redirect. The token is sent in the POST body,
  never in the URL, and checked with the client IP and an idempotency key.
- The OAuth `state` is random, used once, expires after 10 minutes and is tied to the browser by
  an HttpOnly cookie, which blocks login-CSRF.
- Redirect targets after login (`next`) must be same-site paths, so the login page can't be used
  as an open redirect.
- A new web session ID is created at every login. The cookie is HttpOnly and SameSite=Lax, and
  `__Host-` + Secure on https.
- Every form that changes something needs a CSRF token tied to the session, plus a same-origin
  check using `Sec-Fetch-Site` (falling back to `Origin`).

**Device tokens**
- Access tokens last 1 hour. Refresh tokens last 60 days and **rotate on every use**. If an old
  refresh token is used again, the device is revoked automatically, which catches stolen tokens.
- Only SHA-256 hashes of tokens are stored, so a database leak gives no usable credentials.
  Tokens carry `ftga_` / `ftgr_` / `ftgd_` prefixes so secret scanners can spot leaks.
- User codes are 8 consonants (no vowels, no look-alike characters, about 2.6×10¹⁰ combinations),
  expire after 10 minutes and are rate-limited. Polling too fast gets `slow_down`.
- The approval page shows the device name, platform, time and the IP that asked, with a phishing
  warning ("only approve if you started this").

**42 credentials**
- 42 tokens are encrypted with AES-256-GCM using a dedicated `ENCRYPTION_KEY`. The original
  project derived the key from the client secret. The ciphertext is bound to the user ID through
  AAD, so rows can't be swapped between users. 42 tokens are refreshed on the server, one refresh
  at a time per user.
- The 42 client secret goes in the POST body, not the query string as in the original
  `/callback`, so it doesn't end up in logs.
- 42 calls are spaced at least 550 ms apart to stay under 42's 2 requests/second limit, and
  summaries are cached per user.

**General hardening**
- Strict CSP with no inline scripts or styles, only `challenges.cloudflare.com` allowed. Also
  `frame-ancestors 'none'`, HSTS on https, `Referrer-Policy: same-origin`, and an 8 KB body limit
  on the API.
- Rate limits on login, callback, `/link`, device code and token endpoints, and per device on the
  API.
- Every link, approval, revocation and detected token reuse is written to an audit log shown on
  the dashboard.

### Device management
- **Web** (`/devices`): list linked devices (name, platform, linked date, last seen, last IP),
  rename them, revoke one or all, and see recent security activity.
- **Extension** (top bar → *Devices*): the same list, with a confirmation before revoking any
  device (or this one). *Unlink this computer…* revokes it on the server and clears the keyring.
- A revoked device notices on its next request, clears its credentials and offers to link again.

## GNOME 42 compatibility

GNOME 42 can't load ES-module extensions (`import … from 'resource://…'` needs GNOME 45+), and the
old `ftgnome` skeleton used them. This extension uses what GNOME 42 supports:

- Legacy `imports.*` with `init()` / `enable()` / `disable()`. Everything is created in
  `enable()` and removed in `disable()`, including timers, signals and pending polls.
- **libsoup 2.4** (`queue_message`) with an automatic switch to libsoup 3 (`send_and_read_async`).
- Prefs are built with GTK 4 + **libadwaita 1.1** widgets only (no `Adw.SwitchRow`/`EntryRow`,
  which need libadwaita 1.2–1.4). They go through `fillPreferencesWindow`.
- Tokens are stored in the **GNOME Keyring via libsecret**, never in GSettings/dconf. If
  libsecret is missing, they go in a file inside a 0700 directory.
- The server URL must be `https://`, except `http://localhost`. The client enforces this so tokens
  are never sent in clear text.

## Running it

### Server

```bash
cd server && bun install && cp .env.example .env
```

Then fill in `.env`:

1. Create a 42 app at https://profile.intra.42.fr/oauth/applications/new with the redirect URI
   **`${PUBLIC_URL}/auth/callback`**, then copy its UID and secret.
2. Create a Turnstile widget in the Cloudflare dashboard and copy its keys. The `.env.example`
   values are Cloudflare's always-pass test keys and only work for local testing.
3. Generate `ENCRYPTION_KEY` with `bun run gen-key`.
4. Behind Cloudflare or a reverse proxy, set `TRUST_PROXY` so rate limits and the audit log see
   real client IPs. Don't set it otherwise, because those headers can be faked.

```bash
bun run dev
```

In production, use `bun run start` behind https. The SQLite database goes in `data/`. It's a
single instance: rate limits and caches are kept in memory.

### Extension

```bash
make install
```

```bash
gnome-extensions enable ftgnome@zoe.dev.br
```

On Wayland, log out and back in first so GNOME Shell sees the new extension. Then set the server
URL in the extension's Settings and choose **42 → Link this computer…** in the top bar.

### Tests

```bash
make test
```

This runs the server tests, the typecheck and the gjs client test. For a local end-to-end run
without 42 credentials, use `make fake-server` and point the extension at `http://localhost:3999`.
That server has a fake 42 API, an always-pass captcha and a `/__test/approve` endpoint, so never
deploy it.

## Known limits
- If GNOME Shell crashes in the instant between the server rotating a refresh token and the keyring
  saving the new one, the next refresh looks like reuse and the device gets revoked. You just link
  it again. This is the usual trade-off with strict refresh-token rotation.
- Logtime comes from 42's `locations_stats`, which is grouped by 42's calendar days. "Today" and
  "this week" (from Monday) are added up in your local time.
- The evaluation queries use `/v2/me/scale_teams/as_corrector|as_corrected` (same as
  next42eval). If the filter is rejected, the server falls back to filtering itself.
