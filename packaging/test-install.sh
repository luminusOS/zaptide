#!/usr/bin/env bash
# Usage: bash packaging/test-install.sh fedora:45 native-packages-output
# The compiler runs on the host so it cannot supply missing runtime libraries.
set -euo pipefail
image=${1:?Supply a Fedora container image}
packages=$(realpath "${2:?Supply a native-packages output directory}")
case "$(uname -m)" in
  x86_64) target=linux-amd64 ;;
  aarch64) target=linux-arm64 ;;
  *) echo 'Unsupported test architecture' >&2; exit 1 ;;
esac
case "$image" in
  fedora:*) ;;
  *) echo 'Unsupported test distribution' >&2; exit 1 ;;
esac
package_dir="$packages/packages/$target/rpm"
test -d "$package_dir"
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
checks=$(mktemp -d)
trap 'rm -rf -- "$checks"' EXIT
cc -std=c99 -Wall -Wextra -Werror "$script_dir/check-runtime-libs.c" -ldl -o "$checks/check-runtime-libs"
docker run --rm \
  --volume "$package_dir:/packages:ro" \
  --volume "$checks:/checks:ro" \
  "$image" sh -ec '
    set -- /packages/*.rpm
    test "$#" -eq 1
    test -f "$1"
    mkdir -p /root/.config/zaptide
    printf "%s\n" "preserve-existing-settings" > /root/.config/zaptide/fixture
    dnf install -y --setopt=install_weak_deps=False "$1"
    rpm -q zaptide
    zaptide --version
    /checks/check-runtime-libs
    test -s /usr/share/applications/dev.luminusos.ZapTide.desktop
    test -s /usr/share/icons/hicolor/scalable/apps/zaptide.svg
    grep -qx "Icon=zaptide" /usr/share/applications/dev.luminusos.ZapTide.desktop
    grep -qx "StartupWMClass=dev.luminusos.ZapTide" /usr/share/applications/dev.luminusos.ZapTide.desktop
    dnf remove -y zaptide
    test ! -e /usr/bin/zaptide
    test ! -e /usr/share/applications/dev.luminusos.ZapTide.desktop
    test ! -e /usr/share/icons/hicolor/scalable/apps/zaptide.svg
    test "$(cat /root/.config/zaptide/fixture)" = preserve-existing-settings
  '
