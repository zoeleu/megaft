#!/usr/bin/env bash
# Every workspace member must have `[lints] workspace = true`.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
fail=0
for t in "$root"/crates/*/Cargo.toml; do
    if ! awk '/^\[lints\]/{l=1;next} /^\[/{l=0} l && /^workspace *= *true/{f=1} END{exit !f}' "$t"; then
        echo "missing [lints] workspace = true: ${t#$root/}"; fail=1
    fi
done
[ $fail -eq 0 ] && echo "lint inheritance: all crates opt in"
exit $fail
