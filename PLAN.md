# megaft — plan

One repository for:

- **`ftgnome@zoe.dev.br`**: the GNOME Shell 42 extension (today in `~/projects/ft-gnome/extension`).
- **`megaftd`**: a Rust background service that does the long-running work and talks to the extension
  over D-Bus.
- **evalfinder**: rewritten in Rust as a **GPUI** desktop app and a **ratatui** TUI. Both can run on their
  own, or hand jobs to `megaftd` so a booking keeps running after the window closes.

Hard rule: **no `target/` in `$HOME`.** The toolchain, the build output and the registry live on sgoinfre.

## Status (2026-11-24)

| Phase | State | Notes |
| --- | --- | --- |
| 0 — bootstrap + spikes | **done** | Rust 1.100.0 pinned, on sgoinfre. Spike A (GPUI on X11/Intel Vulkan), B (systemd + D-Bus activation), C (headless Chrome login, 5.8 s) all pass. |
| 1 — move things in | **done** | `extension/` and `server/` from ft-gnome; server tests 20/20, gjs client test passes. Python evalfinder in `legacy/`. |
| 2 — megaft-core | **done** | Live dry run matches the Python version slot for slot. Unit, property and fixture tests. |
| 3 — megaftd + proto | **done** | Installed as a systemd user unit; activation through `SystemdService=` works; jobs survive restarts; objects match `megaft1.xml` (test). |
| 4 — ratatui TUI | **done** | `evalfinder` (TUI) + `book` / `status` / `login` / `import-cookies` subcommands; tested in tmux with both backends. |
| 5 — GPUI app | **done** | `evalfinder-gui`; full flow tested on the desktop with the daemon backend. |
| 6 — extension integration | **code done, needs a GNOME Shell test** | `lib/megaftd.js` passes `extension/test-megaftd.js` under gjs 1.72; the indicator/menu part only runs inside GNOME Shell and hasn't been tried there yet. D2 (daemon owns ft_gnome tokens) not started. |
| 7 — retire Python | **pending** | Waiting for a real password login (`evalfinder login --fresh --user <login>`) to be confirmed. |

`make check` (doctor, lint inheritance, fmt, clippy `-D warnings` on every crate, all tests) passes.

### Where the implementation differs from the plan below

- **Build dir:** one shared `CARGO_TARGET_DIR=/sgoinfre/zleullie/cargo-target` for every Rust project (set in
  fish/zsh/profile), and `./target` is a symlink to it — instead of a per-repo `megaft/target`.
- **xkbcommon-x11 shim:** done by `crates/evalfinder-gui/build.rs` (a symlink in `OUT_DIR`), so no absolute path
  is committed and no `syslibs/` dir is needed.
- **GPUI:** `gpui-component` 0.11 is built against `gpui-pre` 0.3.14, so that's what is pinned (plus
  `gpui-pre-platform` for the X11/Wayland backends). No fallback framework (decision D4).
- **zbus:** uses its tokio backend everywhere (its own runtime ran handlers off-tokio and panicked in
  reqwest). The GPUI app owns a tokio runtime and makes all backend/D-Bus calls from it.
- **Backend choice:** made once at app start (megaftd if installed, `--local` to force in-process) instead of a
  per-job "run in background" toggle, which would have needed two separately logged-in sessions.
- **Session renewal in megaftd:** headless only (a background service never pops up a window). Apps can still
  open a visible login window.
- **No sandboxing** in the systemd unit (by request), and credentials are never saved: the password lives in
  memory only, the session cookies in `~/.local/state/megaft/session.json` (0600, home).
- **Install:** `make install` (daemon + CLI/TUI, ~15 MB each in `~/.local/bin`) and `make install-gui`
  separately, because the GPUI release build is much heavier.

---

## 1. What exists today

| Thing | Where | State |
| --- | --- | --- |
| Extension (GJS, legacy imports, Soup 2.4/3, libsecret, device-link flow) | `~/projects/ft-gnome/extension/ftgnome@zoe.dev.br` | Working, no build step |
| ft_gnome server (Bun + Hono + SQLite, Turnstile → 42 OAuth, device tokens) | `~/projects/ft-gnome/server` | Working, has tests |
| evalfinder (Python: urllib + undetected-chromedriver + Textual) | `~/projects/evalfinder` | Working; booked a real evaluation |
| Older merged projects | `ftgnome`, `fortytwo-gnome-api`, `next42eval@42.school` | Already folded into `ft-gnome` |

None of these is a git repository yet. The ft-gnome folder isn't either.

What evalfinder knows, and the Rust port has to keep (it's all intra **web** endpoints, because API v2
can't book as a student):

- `GET /projects/<slug>/slots?team_id=` returns the HTML page. It holds `data-index-url`,
  `data-update-url`, `data-duration` and the CSRF token. **The page's team id is the one to use**, not
  the one API v2 gives.
- `GET /projects/<slug>/slots.json?team_id=&start=&end=` returns blocks of 15-minute slot ids,
  comma-separated.
- `POST /projects/<slug>/slots/<id1,id2>.json?team_id=` with `_method=put`, `start` and `end`, plus
  `X-CSRF-Token`, books the slot.
- `/<slug>/mine` lists the evaluations already booked (`data-scale-team` and `data-long-date`).
- `/` lists the open teams with their status ("Evaluation needed").
- The `cf_clearance` cookie only works with the user-agent of the browser that got it.

## 2. Constraints found on this machine

| Finding | Consequence |
| --- | --- |
| System Rust is **1.75** (Dec 2023). No rustup. | Install rustup into sgoinfre. GPUI and several of the lints need a recent stable (≥ 1.95 for the lint set below). |
| systemd **249** user manager is running (it wasn't reachable earlier in the session; `make doctor` checks it). The session bus is `dbus.service` under it. | `megaftd` is a **systemd user service** (`Type=dbus`). The D-Bus activation file points at it with `SystemdService=`, so the first call starts it through systemd. Jobs survive logout-to-login because the unit is enabled. Fallback if the user manager is down: plain D-Bus activation (`Exec=`). |
| GNOME Shell **42.9**, **X11** session | The extension stays legacy `imports.*` (no ESM). GPUI uses its X11 backend here. |
| Vulkan: Intel ADL-S (`intel_icd`) + lavapipe | GPUI's renderer will work. lavapipe is a CPU fallback. |
| `libxkbcommon-x11.so.0` exists, but the unversioned `.so` (dev package) doesn't, and there's no sudo | Linking GPUI will fail on `-lxkbcommon-x11`. Fix it with a shim: put a `libxkbcommon-x11.so` symlink in `/sgoinfre/zleullie/megaft/syslibs` and pass `-L` from the repo's Cargo config. |
| Home: 4.4 GB free of 11 GB. sgoinfre: NFS, 391 GB free | Everything big goes to sgoinfre. NFS makes builds slower. |
| `~/.cargo` is already a symlink to `~/sgoinfre/.cargo` | The registry is already off home. Keep it that way. |
| sgoinfre has had exploits | Put **nothing secret** on sgoinfre: no cookies, no tokens, no passwords. Installed binaries go to `~/.local/bin` (small, stripped), so a tampered sgoinfre can't swap the program that holds your intra session. The toolchain and build cache stay on sgoinfre. Accepted risk, see §9. |

## 3. Storage layout: keep `target/` out of home

```
/sgoinfre/zleullie/
├── .cargo/                    CARGO_HOME (already used through ~/.cargo)
├── .rustup/                   RUSTUP_HOME (new)
└── megaft/
    ├── target/                CARGO_TARGET_DIR for this repo
    └── syslibs/libxkbcommon-x11.so -> /usr/lib/x86_64-linux-gnu/libxkbcommon-x11.so.0
```

Three layers, so no single mistake fills home:

1. **Environment.** Set it in both fish (the login shell) and zsh/bash (the shell tools spawn):
   ```fish
   # ~/.config/fish/conf.d/rust.fish
   set -gx RUSTUP_HOME /sgoinfre/zleullie/.rustup
   set -gx CARGO_HOME  /sgoinfre/zleullie/.cargo
   set -gx CARGO_TARGET_DIR /sgoinfre/zleullie/cargo-target   # default for every Rust project
   fish_add_path $CARGO_HOME/bin
   ```
   The same three exports go in `~/.zshenv` and `~/.profile`.
2. **A per-repo symlink.** `make bootstrap` creates `megaft/target -> /sgoinfre/zleullie/megaft/target`
   (gitignored). An editor started from the GNOME dock doesn't read the shell profile. Without
   `CARGO_TARGET_DIR`, Cargo then writes `./target`, which is the symlink, so the output still lands on
   sgoinfre. rust-analyzer gets the same treatment through `rust-analyzer.cargo.targetDir`.
3. **A check.** `make doctor` fails if any real `target/` directory sits under `$HOME` next to a
   `Cargo.toml`. It also checks that `RUSTUP_HOME`, `CARGO_HOME` and the target directory resolve to
   sgoinfre, and that rustc matches `rust-toolchain.toml`. It runs as part of `make check`.

`make clean-cache` removes `/sgoinfre/zleullie/megaft/target`. GPUI builds are several GB.

## 4. Architecture

```
                         ┌──────────────── session D-Bus (per user) ────────────────┐
 GNOME Shell 42          │                                                          │
 ftgnome@zoe.dev.br ─────┤  br.dev.zoe.MegaFt1  ◀── activated on first call ──┐     │
  (GJS, Gio.DBusProxy)   │        │                                           │     │
                         │   megaftd (tokio + zbus) ──────────────────────────┘     │
 evalfinder GPUI app ────┤        ├─ intra web client (cookies, CSRF, slots, book)  │
 evalfinder TUI ─────────┘        ├─ booking jobs (keep running, saved to disk)     │
     │                            ├─ Chrome login (chromiumoxide, throwaway profile)│
     │                            └─ ft_gnome server client (device tokens) [phase 6]
     └─ "local mode": the same megaft-core in-process when the daemon isn't wanted
```

- **One engine, two ways to run it.** The GUI and the TUI talk to a `Backend` trait. `LocalBackend`
  runs `megaft-core` in-process. `DaemonBackend` is a zbus proxy. Each app has a "Run in background"
  toggle; when it's on, the job is handed to `megaftd` and survives closing the window.
- **Starting the daemon.** Two small files in home, installed by `make install`:
  ```ini
  # ~/.config/systemd/user/megaftd.service
  [Unit]
  Description=megaft background service (42 evaluations, GNOME extension backend)
  After=graphical-session.target

  [Service]
  Type=dbus
  BusName=br.dev.zoe.MegaFt1
  ExecStart=%h/.local/bin/megaftd
  NotifyAccess=main
  Restart=on-failure
  RestartSec=5

  [Install]
  WantedBy=default.target
  ```
  ```ini
  # ~/.local/share/dbus-1/services/br.dev.zoe.MegaFt1.service
  [D-BUS Service]
  Name=br.dev.zoe.MegaFt1
  Exec=/bin/false
  SystemdService=megaftd.service
  ```
  The first call from the extension or an app starts it through systemd. When enabled, it also
  starts at login, so saved jobs resume on their own. Logs go to the journal:
  `journalctl --user -u megaftd -f`. With no jobs and no clients for 10 minutes it exits, and the
  next call starts it again.
- **Credentials follow what we settled on for evalfinder.** The password lives only in the daemon's
  memory (`secrecy::SecretString`, zeroized) and is never written. The intra session cookies go in
  `~/.local/state/megaft/session.json` (home, 0600, not sgoinfre). The ft_gnome device tokens stay in
  GNOME Keyring through the Secret Service API.

## 5. Repository layout

```
megaft/
├── Cargo.toml                 workspace, [workspace.lints], [workspace.dependencies], profiles
├── Cargo.lock
├── rust-toolchain.toml        exact pinned version (≥ 1.95) + clippy, rustfmt, rust-src
├── clippy.toml                test allowances (§7)
├── rustfmt.toml
├── .cargo/config.toml         -L shim for xkbcommon-x11; no absolute target-dir (see §3)
├── Makefile                   bootstrap / doctor / check / test / install / pack-extension
├── crates/
│   ├── megaft-core/           intra web client + slot logic (port of evalfinder.py), no UI
│   ├── megaft-browser/        Chrome login / cookie refresh via chromiumoxide (feature-gated)
│   ├── megaft-proto/          D-Bus interfaces (zbus proxies + zvariant types) + XML for GJS
│   ├── megaftd/               the daemon binary
│   ├── evalfinder-app/        UI-independent state machine: login → setup → run (shared by both UIs)
│   ├── evalfinder-gui/        GPUI app (+ gpui-component for inputs, select, table)
│   └── evalfinder-tui/        ratatui + crossterm; binary `evalfinder`, also a headless CLI
├── extension/
│   └── ftgnome@zoe.dev.br/    moved from ft-gnome; gains lib/megaftd.js (D-Bus client)
├── server/                    ft_gnome Bun server, moved as is (decision D1)
├── dist/                      packed extension zip (gitignored)
└── legacy/evalfinder-py/      Python evalfinder, kept until the Rust version matches it, then deleted
```

### Crates in detail

**megaft-core** (async, tokio, reqwest + rustls + cookie_store)
- `IntraWeb`: cookie jar, UA pinning, `whoami`, `open_projects`, `booking_page(slug)` (team id,
  duration, URLs, CSRF), `slots(range)`, `book(ids)`, `upcoming_evaluations(slug)`.
- `slots::candidates(...)` and `Window`/`Avoid` stay **pure functions** with property tests
  (`proptest`): no overlap with existing evaluations ± buffer, no overlap between the job's own
  bookings, slots within the window.
- `BookingJob`: the poll loop as a `Stream<Item = JobEvent>` (Log, Booked, Refused, Finished).
  Cancellation goes through a `CancellationToken`. Back-off on 429 and jitter as in Python.
- Errors: `thiserror` in the library, `anyhow` only in binaries. HTML parsing uses the `scraper` crate
  instead of the Python regexes. Fixtures come from the real pages with personal data stripped.
- The `cast_*` lints are on in this crate (§7), because slot and time arithmetic is where they matter.

**megaft-browser**: chromiumoxide driving the system `google-chrome` (158 here) with a throwaway
profile under `$XDG_RUNTIME_DIR` that is deleted on drop. It fills the Keycloak form from an
in-memory `SecretString`. If that fails it opens a visible window. It returns cookies and the UA.
Feature `browser`, so the TUI can be built without it.

**megaft-proto**: the single source of truth for the D-Bus API. `#[zbus::proxy]` traits for clients,
plus an exported introspection XML checked in at `extension/ftgnome@zoe.dev.br/dbus/megaft1.xml`. A
test fails if the checked-in XML differs from the generated one, so the GJS side can't fall out of date.

**megaftd**: tokio + zbus server, run as the systemd user unit above. Jobs are saved (with no secrets)
to `~/.local/state/megaft/jobs.json` and resumed at start-up. Logs: `tracing` → `tracing-journald`
when `JOURNAL_STREAM` is set (under systemd), otherwise stderr. Readiness is the bus name being
taken (`Type=dbus`). It also sends `sd_notify` `STATUS=` lines ("2 jobs running"), which
`systemctl --user status megaftd` shows; this needs `NotifyAccess=main` in the unit.

**evalfinder-app**: the screens as plain state (`Login`, `Setup { projects, count, hours, dry_run }`,
`Run { progress, booked, log }`), driven by `Backend`. The GUI and the TUI only draw it, so behaviour is
written and tested once.

**evalfinder-gui**: GPUI. Needs `gpui-component` (or similar) for text inputs, select and table,
because GPUI itself has no text input. The app owns a dedicated tokio runtime thread for `LocalBackend`,
because GPUI has its own executor. zbus is built **without** the tokio feature here.

**evalfinder-tui**: ratatui + crossterm, the same three screens as the Textual app. `evalfinder book
codexion --hours 4 --count 2 [--dry-run] [--daemon]` runs headless for scripts.

## 6. D-Bus API (v1)

Bus name `br.dev.zoe.MegaFt1`, base path `/br/dev/zoe/MegaFt1`. Session bus only.

```
br.dev.zoe.MegaFt1.Session            /br/dev/zoe/MegaFt1
  Login(s login, s password)          -> (s who)          password kept in memory only
  LoginInteractive()                  -> (s who)          opens Chrome
  UseSavedSession()                   -> (s who)
  Logout()
  prop  State  s  ("logged-out" | "logging-in" | "ready" | "expired")
  prop  Who    s

br.dev.zoe.MegaFt1.Evaluations        /br/dev/zoe/MegaFt1
  OpenProjects()                      -> a(sss)  slug, name, status
  Upcoming(s slug)                    -> ax      unix starts
  CreateJob(s slug, u count, x deadline_unix, u buffer_min, b dry_run) -> (o job)
  ListJobs()                          -> ao
  signal JobAdded(o) / JobRemoved(o)

br.dev.zoe.MegaFt1.Job                /br/dev/zoe/MegaFt1/jobs/<n>
  Cancel()
  prop  State   s   ("running" | "done" | "deadline" | "cancelled" | "error")
  prop  Booked  a(xx)
  prop  Spec    (suxub)
  signal Log(x ts, s line)
  signal Booked(x start, x end)

br.dev.zoe.MegaFt1.Summary            (phase 6, for the top bar)
  prop  Logtime a{sv}, NextEvaluations a(...)  — from the ft_gnome server or the intra
```

The extension reads `Job` objects to show "🎯 Codexion 1/2" in the top bar with a cancel item, and sends
a GNOME notification on `Booked`.

## 7. Strict Clippy

Follows emschwartz.me/your-clippy-config-should-be-stricter: individual lints at `warn`, and
`-D warnings` when checking.

```toml
# Cargo.toml
[workspace.lints.rust]
unsafe_code = "forbid"          # nothing here needs unsafe; GPUI's own unsafe lives in its crate
missing_debug_implementations = "warn"
unused_qualifications = "warn"

[workspace.lints.clippy]
# Don't panic
string_slice = "warn"
indexing_slicing = "warn"
unwrap_used = "warn"
expect_used = "warn"            # optional in the post; on here (UI + daemon must not crash)
panic = "warn"
todo = "warn"
unimplemented = "warn"
unreachable = "warn"
get_unwrap = "warn"
unwrap_in_result = "warn"
unchecked_time_subtraction = "warn"
panic_in_result_fn = "warn"
# Don't fail silently
let_underscore_future = "warn"
let_underscore_must_use = "warn"
unused_result_ok = "warn"
map_err_ignore = "warn"
assertions_on_result_states = "warn"
# Async
await_holding_lock = "warn"
await_holding_refcell_ref = "warn"
if_let_mutex = "warn"
large_futures = "warn"
# Memory / unsafe (tripwires, since unsafe_code is forbidden)
mem_forget = "warn"
undocumented_unsafe_blocks = "warn"
multiple_unsafe_ops_per_block = "warn"
unnecessary_safety_doc = "warn"
unnecessary_safety_comment = "warn"
# Numbers
float_cmp = "warn"
float_cmp_const = "warn"
lossy_float_literal = "warn"
cast_sign_loss = "warn"
invalid_upcast_comparisons = "warn"
# Easy to avoid
rc_mutex = "warn"
debug_assert_with_mut_call = "warn"
iter_not_returning_iterator = "warn"
expl_impl_clone_on_copy = "warn"
infallible_try_from = "warn"
dbg_macro = "warn"
# No silent escape hatches: #[expect(lint, reason = "...")] only
allow_attributes = "warn"
allow_attributes_without_reason = "warn"
```

```toml
# clippy.toml
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-dbg-in-tests = true
```

- Every member crate has `[lints] workspace = true`. `make check` greps every `crates/*/Cargo.toml` and
  fails if one doesn't, because stable Cargo doesn't warn about this.
- Inheriting the workspace lints rules out per-crate overrides in Cargo.toml. So `megaft-core` turns on
  the optional numeric lints at its crate root: `#![warn(clippy::cast_possible_truncation,
  clippy::cast_possible_wrap, clippy::cast_precision_loss)]`. They stay off in the GPUI crate, where
  pixel math would produce mostly noise. `arithmetic_side_effects` stays off everywhere, because the
  post measured about 85 % noise.
- `make check` = `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features -- -D
  warnings` + `cargo test --workspace` + the proto XML drift test + the lint-inheritance check +
  `make doctor`. The extension side keeps its gjs test and adds `eslint` for the GJS (optional).
- The toolchain is pinned in `rust-toolchain.toml`. A new Rust release can't break `-D warnings` until
  we bump it on purpose.

## 8. Milestones

Each phase ends with `make check` passing and something you can run.

**Phase 0 — bootstrap and risk spikes (do first, ~1 evening)**
1. Install rustup into `/sgoinfre/zleullie/.rustup` (`--no-modify-path`), set up the env files from §3,
   and pin stable.
2. `git init` megaft, workspace skeleton, lints, Makefile, `make doctor`.
3. **Spike A: GPUI.** A window with a text input on this machine (X11, Intel Vulkan), with the
   xkbcommon-x11 shim. Measure the build size and time on NFS. *If this fails, stop and report; there is no
   fallback framework.*
4. **Spike B: systemd + D-Bus activation.** A minimal zbus service as a `Type=dbus` user unit, started
   through `SystemdService=` by a call from `gdbus` and from a GJS snippet inside GNOME Shell 42. Also
   check that it starts at login once enabled, and that it falls back to plain `Exec=` activation when
   the user manager is down.
5. **Spike C: chromiumoxide login.** Headless Keycloak login + cf_clearance + UA, matching what
   undetected-chromedriver does today.

**Phase 1 — move things in.** Copy `ft-gnome/extension` and `ft-gnome/server` as they are, keeping
their Makefile targets as `make ext-*` and `make server-*`. Move the Python evalfinder to `legacy/`.
Everything still works as before.

**Phase 2 — megaft-core.** Port the intra client and the slot logic with fixtures and property tests.
A `cargo run -p evalfinder-tui -- book … --dry-run` matching the Python dry-run is the parity check.

**Phase 3 — megaftd + megaft-proto.** D-Bus API, jobs, job persistence, systemd unit + activation
file, idle exit. `make install` puts the stripped binaries in `~/.local/bin`, installs both unit files,
and runs `systemctl --user daemon-reload` and `enable megaftd`. `make uninstall` reverses it.

**Phase 4 — ratatui TUI.** The three screens, Local and Daemon backends, the headless CLI.

**Phase 5 — GPUI app.** The same screens through `evalfinder-app`. A `.desktop` file in
`~/.local/share/applications`.

**Phase 6 — extension integration.** `lib/megaftd.js` (a `Gio.DBusProxy` built from the checked-in
XML): job status in the top bar, Booked notifications, cancel. Optional: move the ft_gnome device-token
handling from the extension into `megaftd` (decision D2).

**Phase 7 — retire Python.** Delete `legacy/` once the TUI and GUI cover everything the Textual app did.

## 9. Risks

- **GPUI changes fast and isn't 1.0.** Pin an exact version or git revision. Upgrades happen on purpose,
  never through a loose `cargo update`. gpui-component is pinned the same way.
- **Undocumented intra endpoints.** Keep the HTML parsing in one module, with fixtures. When a parse
  fails, record the page with personal data stripped so it can be fixed quickly.
- **Cloudflare.** So far plain HTTPS with the browser's cookies and UA has worked. chromiumoxide isn't
  "undetected" by default; Spike C decides whether we need stealth flags or a visible-window fallback
  for every login.
- **NFS builds are slow, and sgoinfre is 91 % full overall.** Use `[profile.dev] debug = "line-tables-only"`,
  incremental builds, and `make clean-cache`. A local `/tmp` target directory would be faster but gets
  wiped and is shared with other users, so it stays off the table unless you say otherwise.
- **Something running as you could be tampered with on sgoinfre (toolchain, build cache).** Binaries
  are installed to home and carry no secrets on sgoinfre, but a poisoned toolchain would still build
  poisoned binaries. Optional mitigation: `make doctor` checks `rustup` component hashes.
- **Polling politeness.** Keep the 15–20 s jittered interval and the 429 back-off. Run at most one job
  per project, enforced by the daemon.

## 10. Decisions needed

| # | Question | Recommendation |
| --- | --- | --- |
| D1 | Does the Bun `server/` move into megaft? | **Yes, as is.** The extension's device-link flow depends on it, and a monorepo keeps the API and its client in step. A Rust rewrite isn't planned. |
| D2 | Should `megaftd` own the ft_gnome device tokens and 42 summary, making the extension a pure D-Bus UI? | **Later (phase 6+).** It simplifies the extension, but its current Soup/keyring code works and is tested. |
| D3 | Bus name / UUID domain | `br.dev.zoe.MegaFt1`, which matches the existing `@zoe.dev.br` UUID. |
| D4 | GPUI fallback if Spike A fails | **None.** GPUI only; report back if it doesn't work. |
| D5 | Is a git remote wanted (GitHub `zoeleu/megaft`)? | Your call. Nothing is pushed until you say so. |
