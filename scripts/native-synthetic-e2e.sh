#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

HEADLESS=false
SKIP_SCREENSHOTS=false
for arg in "$@"; do
    case "$arg" in
        --headless) HEADLESS=true; SKIP_SCREENSHOTS=true ;;
        --no-screenshots) SKIP_SCREENSHOTS=true ;;
    esac
done

if [ "$HEADLESS" = true ]; then
    echo "Running native synthetic e2e in headless mode (no screenshot capture)"
    export ZAPTIDE_NATIVE_SYNTHETIC=1
    export RUST_LOG="${RUST_LOG:-info}"
    cargo build --locked --features demo
    exec cargo run --locked --features demo -- --headless
fi

export GTK_A11Y=atspi
export ZAPTIDE_NATIVE_SYNTHETIC=1
export RUST_LOG="${RUST_LOG:-info}"

cargo build --locked --features demo

if [ "$SKIP_SCREENSHOTS" = true ]; then
    echo "Running native synthetic e2e without screenshot capture"
    if command -v dbus-run-session >/dev/null; then
        exec dbus-run-session -- xvfb-run -a cargo run --locked --features demo
    fi
    exec xvfb-run -a cargo run --locked --features demo
fi

if command -v dbus-run-session >/dev/null; then
    exec dbus-run-session -- xvfb-run -a cargo run --locked --features demo -- --demo-shot
fi
exec xvfb-run -a cargo run --locked --features demo -- --demo-shot
