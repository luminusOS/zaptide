#!/usr/bin/env bash
set -euo pipefail

# Task 7 step 3: Scan for forbidden non-native UI dependencies.
# Exit 0 if clean, 1 if forbidden deps found.

FORBIDDEN_PKGS=(
    "egui"
    "eframe"
    "egui_extras"
    "gtk-egui-area"
    "webkit2gtk"
    "webkitgtk"
    "cef"
    "chromium"
    "wry"
    "web-view"
    "tauri"
    "electron"
    "nwjs"
)

FORBIDDEN_IMPORTS=(
    "use egui"
    "use eframe"
    "gtk_egui_area"
)

# Directories and files to exclude from source scanning
EXCLUDE_DIRS=(
    ".git"
    "target"
    "docs"
)

EXCLUDE_FILES=()

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

FOUND_ISSUES=0
FOUND_ITEMS=()

echo "Scanning for forbidden UI dependencies..."
echo ""

# Function to check if a path should be excluded from source scanning
should_exclude() {
    local path="$1"
    for exclude_dir in "${EXCLUDE_DIRS[@]}"; do
        if [[ "$path" == *"/$exclude_dir/"* ]] || [[ "$path" == "$exclude_dir/"* ]]; then
            return 0
        fi
    done
    for exclude_file in "${EXCLUDE_FILES[@]}"; do
        if [[ "$(basename "$path")" == "$exclude_file" ]]; then
            return 0
        fi
    done
    return 1
}

# 1. Scan manifest files for forbidden package names
echo "Checking Cargo.toml, Cargo.lock, and packaging manifests..."
MANIFEST_FILES=(
    "Cargo.toml"
    "Cargo.lock"
)

# Add any JSON files in packaging/
if [ -d "packaging" ]; then
    while IFS= read -r -d '' file; do
        MANIFEST_FILES+=("$file")
    done < <(find packaging -name "*.json" -print0 2>/dev/null || true)
fi

for manifest in "${MANIFEST_FILES[@]}"; do
    if [ -f "$manifest" ]; then
        for pkg in "${FORBIDDEN_PKGS[@]}"; do
            # Match package name as a dependency (with quotes or as a key)
            if grep -qE "(^[[:space:]]*\"?${pkg}\"?[[:space:]]*=|\"name\"[[:space:]]*:[[:space:]]*\"${pkg}\")" "$manifest"; then
                FOUND_ITEMS+=("Package '$pkg' found in $manifest")
                FOUND_ISSUES=1
            fi
        done
    fi
done

# 2. Run cargo tree and check for forbidden packages in the dependency tree
echo "Running cargo tree..."
if command -v cargo &> /dev/null; then
    TREE_FILE=$(mktemp)
    cargo tree --all-features 2>/dev/null > "$TREE_FILE" || true
    if [ -s "$TREE_FILE" ]; then
        for pkg in "${FORBIDDEN_PKGS[@]}"; do
            # Match exact package name followed by version (avoids partial matches)
            if grep -qE " ${pkg} v[0-9]" "$TREE_FILE"; then
                FOUND_ITEMS+=("Package '$pkg' found in cargo tree dependency tree")
                FOUND_ISSUES=1
            fi
        done
    fi
    rm -f "$TREE_FILE"
fi

# 3. Scan source files for forbidden imports
echo "Scanning source files for forbidden imports..."
while IFS= read -r -d '' file; do
    if ! should_exclude "$file"; then
        for import in "${FORBIDDEN_IMPORTS[@]}"; do
            if grep -q "$import" "$file"; then
                FOUND_ITEMS+=("Import '$import' found in $file")
                FOUND_ISSUES=1
            fi
        done
    fi
done < <(find . -type f \( -name "*.rs" -o -name "*.toml" -o -name "*.json" \) -print0 2>/dev/null)

# Report results
echo ""
if [ $FOUND_ISSUES -eq 1 ]; then
    echo "ERROR: Forbidden UI dependencies found:"
    echo ""
    for item in "${FOUND_ITEMS[@]}"; do
        echo "  - $item"
    done
    echo ""
    echo "These dependencies must be removed before Task 7 can be completed."
    echo "See docs/zaptide/evidence/task-7-dependency-audit.md for details."
    exit 1
else
    echo "SUCCESS: No forbidden UI dependencies found."
    exit 0
fi
