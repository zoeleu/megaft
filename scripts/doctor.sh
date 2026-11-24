#!/usr/bin/env bash
# Checks that Rust's toolchain, registry and build output stay off $HOME (PLAN §3).
set -u
fail=0
ok()  { printf '  \033[32mok\033[0m   %s\n' "$1"; }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=1; }
on_sgoinfre() { case "$(readlink -f "$1" 2>/dev/null || echo "$1")" in /sgoinfre/*) return 0;; *) return 1;; esac; }

echo "megaft doctor"
for v in RUSTUP_HOME CARGO_HOME CARGO_TARGET_DIR; do
    val="${!v:-}"
    if [ -z "$val" ]; then bad "$v is not set (source ~/.zshenv / fish conf.d/rust.fish)";
    elif on_sgoinfre "$val"; then ok "$v=$val";
    else bad "$v=$val is not on sgoinfre"; fi
done

root="$(cd "$(dirname "$0")/.." && pwd)"
if [ -L "$root/target" ] && on_sgoinfre "$root/target"; then ok "./target -> $(readlink "$root/target")";
elif [ -e "$root/target" ]; then bad "./target is a real directory in \$HOME (run: rm -rf target && make bootstrap)";
else bad "./target symlink missing (run: make bootstrap)"; fi

# Any real target/ dir next to a Cargo.toml anywhere under $HOME?
stray=$(find "$HOME" -xdev -maxdepth 6 -type d -name target -not -path '*/.git/*' -not -path '*/node_modules/*' \
        -execdir test -f Cargo.toml \; -print 2>/dev/null)
if [ -n "$stray" ]; then
    printf '  \033[33mwarn\033[0m %s\n' "build output in \$HOME from other projects (delete it; CARGO_TARGET_DIR now prevents new ones):"
    echo "$stray" | sed 's/^/         /'
else ok "no target/ directories in \$HOME"; fi

want=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/rust-toolchain.toml")
have=$(cd "$root" && rustc --version 2>/dev/null | awk '{print $2}')
if [ "$want" = "$have" ]; then ok "rustc $have (pinned)"; else bad "rustc '$have' but rust-toolchain.toml pins '$want'"; fi

if systemctl --user is-system-running >/dev/null 2>&1 || [ "$(systemctl --user is-system-running 2>/dev/null)" = degraded ]; then
    ok "systemd user manager reachable"
else
    printf '  \033[33mwarn\033[0m %s\n' "systemd user manager not reachable (megaftd falls back to plain D-Bus activation)"
fi
exit $fail
