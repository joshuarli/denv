#!/bin/bash
set -euo pipefail

# Noop latency benchmark: denv vs direnv.

if ! command -v hyperfine &>/dev/null; then
    echo "error: hyperfine required — brew install hyperfine" >&2
    exit 1
fi

PROJ="$(cd "$(dirname "$0")" && pwd)"
cargo build --release --quiet --manifest-path="$PROJ/Cargo.toml"
BIN="$PROJ/target/release/denv"

DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT
DIR=$(cd "$DIR" && pwd -P)

echo 'export FOO=bar' > "$DIR/.envrc"
cd "$DIR"

# --- denv setup ---
export __DENV_PID=$$
export __DENV_SHELL=fish
"$BIN" allow >/dev/null 2>&1

MTIME=$(stat -f %m .envrc)
export __DENV_STATE="$MTIME 0 $DIR"

OUT=$("$BIN" export fish 2>/dev/null)
if [ -n "$OUT" ]; then
    echo "error: denv noop produced output: $OUT" >&2
    exit 1
fi

CMDS=(-n baseline "/usr/bin/true" -n denv "$BIN export fish")

# --- direnv (optional) ---
if command -v direnv &>/dev/null; then
    DIRENV_BIN=$(command -v direnv)
    direnv allow 2>/dev/null
    eval "$(direnv export bash 2>/dev/null)" || true

    OUT=$(direnv export fish 2>/dev/null) || true
    if [ -z "$OUT" ]; then
        CMDS+=(-n direnv "$DIRENV_BIN export fish")
    else
        echo "warning: direnv noop produced output, skipping" >&2
    fi
else
    echo "note: direnv not found, benchmarking denv only" >&2
fi

hyperfine --warmup 500 --shell=none "${CMDS[@]}"
