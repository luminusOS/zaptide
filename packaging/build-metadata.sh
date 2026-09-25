#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="$PROJECT_ROOT/data"
BUILD_DIR="$PROJECT_ROOT/packaging/build"
CHECK_ONLY=false

APP_ID="dev.luminusos.ZapTide"
VERSION="${ZAPTIDE_VERSION:-0.14.0}"

usage() {
    cat <<EOF
Usage: $(basename "$0") [--check]

Builds packaging metadata from templates in data/.

Options:
  --check    Validate templates without writing output files

Output:
  packaging/build/${APP_ID}.desktop
  packaging/build/${APP_ID}.metainfo.xml
  packaging/build/gschemas/gschemas.compiled

Environment:
  ZAPTIDE_VERSION    Override version string (default: from Cargo.toml)
EOF
}

die() { echo "ERROR: $*" >&2; exit 1; }
warn() { echo "WARN: $*" >&2; }

require_cmd() {
    if ! command -v "$1" &>/dev/null; then
        if [[ "${CHECK_ONLY}" == "true" ]]; then
            warn "$1 not found, skipping"
            return 1
        fi
        die "$1 required but not found"
    fi
    return 0
}

substitute() {
    local input="$1" output="$2"
    if command -v envsubst &>/dev/null; then
        export APP_ID VERSION
        envsubst '${APP_ID} ${VERSION}' < "$input" > "$output"
    elif command -v sed &>/dev/null; then
        sed -e "s|\${APP_ID}|${APP_ID}|g" -e "s|\${VERSION}|${VERSION}|g" "$input" > "$output"
    else
        cp "$input" "$output"
    fi
}

validate_desktop() {
    local file="$1"
    if require_cmd desktop-file-validate; then
        if ! desktop-file-validate "$file"; then
            die "desktop-file-validate failed on $file"
        fi
        echo "OK: desktop-file-validate $file"
    fi
}

validate_metainfo() {
    local file="$1"
    if require_cmd appstreamcli; then
        if ! appstreamcli validate --pedantic "$file"; then
            die "appstreamcli validate failed on $file"
        fi
        echo "OK: appstreamcli validate --pedantic $file"
    fi
}

validate_schema() {
    local schema_dir="$1"
    if require_cmd glib-compile-schemas; then
        if ! glib-compile-schemas --strict --dry-run "$schema_dir" 2>&1; then
            die "glib-compile-schemas --strict --dry-run failed on $schema_dir"
        fi
        echo "OK: glib-compile-schemas --strict --dry-run $schema_dir"
    fi
}

if [[ "${1:-}" == "--check" ]]; then
    CHECK_ONLY=true
    shift
fi

if [[ "${1:-}" == "--help" ]] || [[ "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi

for f in "$DATA_DIR/${APP_ID}.desktop.in" \
         "$DATA_DIR/${APP_ID}.metainfo.xml.in" \
         "$DATA_DIR/${APP_ID}.gschema.xml"; do
    [[ -f "$f" ]] || die "template not found: $f"
done

if [[ "$CHECK_ONLY" == "true" ]]; then
    echo "=== Validating templates (check-only mode) ==="
    TMPDIR_CHECK="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR_CHECK"' EXIT

    substitute "$DATA_DIR/${APP_ID}.desktop.in" "$TMPDIR_CHECK/${APP_ID}.desktop"
    validate_desktop "$TMPDIR_CHECK/${APP_ID}.desktop"

    substitute "$DATA_DIR/${APP_ID}.metainfo.xml.in" "$TMPDIR_CHECK/${APP_ID}.metainfo.xml"
    validate_metainfo "$TMPDIR_CHECK/${APP_ID}.metainfo.xml"

    mkdir -p "$TMPDIR_CHECK/gschemas"
    cp "$DATA_DIR/${APP_ID}.gschema.xml" "$TMPDIR_CHECK/gschemas/"
    validate_schema "$TMPDIR_CHECK/gschemas"

    echo "=== All checks passed ==="
    exit 0
fi

echo "=== Building packaging metadata ==="
mkdir -p "$BUILD_DIR/gschemas"

substitute "$DATA_DIR/${APP_ID}.desktop.in" "$BUILD_DIR/${APP_ID}.desktop"
echo "Generated: $BUILD_DIR/${APP_ID}.desktop"

substitute "$DATA_DIR/${APP_ID}.metainfo.xml.in" "$BUILD_DIR/${APP_ID}.metainfo.xml"
echo "Generated: $BUILD_DIR/${APP_ID}.metainfo.xml"

cp "$DATA_DIR/${APP_ID}.gschema.xml" "$BUILD_DIR/gschemas/"
if require_cmd glib-compile-schemas; then
    glib-compile-schemas "$BUILD_DIR/gschemas/"
    echo "Generated: $BUILD_DIR/gschemas/gschemas.compiled"
else
    warn "glib-compile-schemas unavailable, skipping schema compilation"
fi

echo "=== Build complete ==="
