#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")"
cargo build --quiet

BIN="$(pwd)/target/debug/denv"
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT

# Resolve symlinks (macOS /var -> /private/var) so __DENV_STATE matches cwd
DIR=$(cd "$DIR" && pwd -P)

# Set up noop fast path: .env exists, __DENV_STATE matches
echo "FOO=bar" > "$DIR/.env"
MTIME=$(stat -f %m "$DIR/.env")

export __DENV_PID=$$
export __DENV_SHELL=fish
export __DENV_STATE="0 $MTIME $DIR"

# Kill any stale trace sessions
sudo pkill -f fs_usage 2>/dev/null || true
sleep 0.3

TRACE=$(mktemp)
trap 'rm -rf "$DIR" "$TRACE"' EXIT

# Start fs_usage, then run denv, then let fs_usage timeout naturally
sudo fs_usage -w -t 3 denv > "$TRACE" 2>&1 &
FSPID=$!
sleep 0.5

(cd "$DIR" && "$BIN" export fish)

wait "$FSPID" 2>/dev/null || true

cat "$TRACE"
