# megaft

42 tooling for the GNOME desktop, in one repository:

| Part | What it is |
| --- | --- |
| **evalfinder** | Finds and books 42 evaluation slots: a GPUI desktop app (`evalfinder-gui`), a ratatui TUI and a headless CLI (`evalfinder`). |
| **megaftd** | A background service (systemd user unit, D-Bus `br.dev.zoe.MegaFt1`) that runs booking jobs so they keep going after you close the app. |
| **ft_gnome** | A GNOME Shell 42 extension: logtime and upcoming evaluations in the top bar, plus megaftd's booking jobs. |
| **server** | The ft_gnome server (Bun + Hono): 42 OAuth, captcha and per-device tokens for the extension. |

See [PLAN.md](PLAN.md) for the design, the status of each part, and the reasoning behind it.

> evalfinder uses the intra's own web endpoints (the public API can't book as a student). Use it on your own
> account, keep the polling interval reasonable (default 15 s + jitter), and expect it to break if the intra
> changes.

## Layout

```
crates/
  megaft-core/      intra web client + slot logic (no UI)
  megaft-browser/   Chrome login (chromiumoxide) → session cookies
  megaft-proto/     D-Bus API: megaft1.xml + client proxies
  megaftd/          the background service
  evalfinder-app/   screens, validation, Local/Daemon backends (shared by both UIs)
  evalfinder-gui/   GPUI app
  evalfinder-tui/   ratatui TUI + CLI (binary: evalfinder)
extension/          GNOME Shell 42 extension (plain GJS, legacy imports)
server/             ft_gnome server (Bun)
dist-files/         systemd unit, D-Bus activation file, .desktop entry
legacy/             the Python evalfinder, until the Rust one has fully replaced it
```

## Building

On 42 machines `$HOME` is small, so the toolchain, the registry and **all build output live on sgoinfre**.
`make doctor` checks this; `make check` runs it too.

```bash
# once: rustup into sgoinfre (see PLAN.md §3), then
export RUSTUP_HOME=/sgoinfre/$USER/.rustup CARGO_HOME=/sgoinfre/$USER/.cargo CARGO_TARGET_DIR=/sgoinfre/$USER/cargo-target
make bootstrap      # ./target -> sgoinfre symlink + doctor
make check          # fmt + clippy -D warnings + tests
make install        # megaftd + evalfinder into ~/.local/bin, enable the systemd unit
make install-gui    # evalfinder-gui (heavier build)
make ext-install    # the GNOME extension
```

Requirements: the pinned Rust in `rust-toolchain.toml`, Google Chrome or Chromium (for logging in), Vulkan
(GPUI), and for the server/extension tests `bun` and `gjs`.

## Using evalfinder

```bash
evalfinder                     # TUI: log in → pick project, count, hours → watch it book
evalfinder-gui                 # same, as a desktop app
evalfinder status              # who's logged in, open projects
evalfinder book codexion --hours 4 --count 2 [--dry-run]
evalfinder login [--visible] [--fresh --user <login>]
```

When megaftd is installed, jobs run there and keep going after you close the app (`--local` runs them
in-process instead). Follow them with `journalctl --user -u megaftd -f` or in the GNOME top bar.

**Credentials are never saved.** The password is only held in memory (by the app, or by megaftd after
`Login`). The intra session cookies are kept in `~/.local/state/megaft/session.json` (mode 0600, in
`$HOME`, never on sgoinfre).

## Lints

Strict Clippy, following [Your Clippy config should be stricter](https://emschwartz.me/your-clippy-config-should-be-stricter/):
no `unwrap`/`expect`/indexing/panics outside tests, no silently ignored results, `#[expect(…, reason = …)]`
instead of `#[allow]`, `unsafe_code = "forbid"`. Checked with `-D warnings` on a pinned toolchain; every
crate must opt in with `[lints] workspace = true` (`make lint-inheritance`).
