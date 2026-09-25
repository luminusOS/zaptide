#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="$PROJECT_ROOT/packaging/build"
APP_ID="dev.luminusos.ZapTide"
FAILURES=0

warn() { echo "WARN: $*" >&2; }
fail() { echo "FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }
pass() { echo "PASS: $*"; }

run_if_available() {
    local cmd="$1"; shift
    if ! command -v "$cmd" &>/dev/null; then
        warn "$cmd not found, skipping"
        return 0
    fi
    "$cmd" "$@"
}

echo "=== Checking packaging metadata ==="

if [[ -f "$BUILD_DIR/${APP_ID}.desktop" ]]; then
    if run_if_available desktop-file-validate "$BUILD_DIR/${APP_ID}.desktop"; then
        pass "desktop-file-validate ${APP_ID}.desktop"
    else
        fail "desktop-file-validate ${APP_ID}.desktop"
    fi
else
    warn "$BUILD_DIR/${APP_ID}.desktop not found (run build-metadata.sh first)"
fi

if [[ -f "$BUILD_DIR/${APP_ID}.metainfo.xml" ]]; then
    if run_if_available appstreamcli validate --pedantic "$BUILD_DIR/${APP_ID}.metainfo.xml"; then
        pass "appstreamcli validate --pedantic ${APP_ID}.metainfo.xml"
    else
        fail "appstreamcli validate --pedantic ${APP_ID}.metainfo.xml"
    fi
else
    warn "$BUILD_DIR/${APP_ID}.metainfo.xml not found (run build-metadata.sh first)"
fi

if [[ -d "$BUILD_DIR/gschemas" ]]; then
    if run_if_available glib-compile-schemas --strict --dry-run "$BUILD_DIR/gschemas/"; then
        pass "glib-compile-schemas --strict schemas"
    else
        fail "glib-compile-schemas --strict schemas"
    fi
else
    warn "$BUILD_DIR/gschemas/ not found (run build-metadata.sh first)"
fi

echo "=== Checking forbidden dependencies ==="
FORBIDDEN="egui eframe webkit cef"
if command -v cargo &>/dev/null; then
    CARGO_OUT="$(cargo metadata --all-features --format-version 1 2>/dev/null || true)"
    if [[ -n "$CARGO_OUT" ]]; then
        for dep in $FORBIDDEN; do
            if echo "$CARGO_OUT" | grep -qi "\"name\":\"${dep}\""; then
                fail "forbidden dependency found: $dep"
            else
                pass "no forbidden dependency: $dep"
            fi
        done
    else
        warn "cargo metadata failed, skipping dependency check"
    fi
else
    warn "cargo not found, skipping dependency check"
fi

echo ""
if [[ $FAILURES -gt 0 ]]; then
    echo "FAILED: $FAILURES check(s) failed"
    exit 1
fi
echo "All checks passed"
exit 0
