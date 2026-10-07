#!/bin/sh
# Builds the ZapTide AppImage with quick-sharun. Run it as root inside an Arch
# Linux container (Arch ships libadwaita 1.10 and has no 32-bit libs in /usr/lib):
#   podman run --rm -v "$PWD":/src -w /src archlinux ./packaging/appimage/build.sh
# The result is dist/zaptide-$ARCH.AppImage.
set -eux

ARCH="$(uname -m)"
TOOLS="https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/refs/heads/main/useful-tools"

if command -v pacman >/dev/null; then
	# libadwaita 1.10 (GNOME 51) is only in the testing repos until Arch promotes it;
	# drop this block once extra has it. They must precede core and extra to win.
	sed -i 's|^\[core\]|[core-testing]\nInclude = /etc/pacman.d/mirrorlist\n[extra-testing]\nInclude = /etc/pacman.d/mirrorlist\n\n[core]|' /etc/pacman.conf

	pacman -Syu --noconfirm base-devel git wget rustup gtk4 libadwaita alsa-lib \
		gstreamer gst-plugins-base gst-plugins-good cmake libseccomp fontconfig \
		glycin bubblewrap xorg-server-xvfb patchelf strace
	rustup default stable
else
	# Fedora 45 has libadwaita 1.10; used for aarch64, where Arch has no official image.
	dnf install -y --setopt=install_weak_deps=False \
		@development-tools git wget file xz perl make rust cargo gtk4-devel libadwaita-devel \
		alsa-lib-devel gstreamer1-devel gstreamer1-plugins-base gstreamer1-plugins-good \
		cmake libseccomp-devel fontconfig-devel glycin-loaders bubblewrap \
		xorg-x11-server-Xvfb dbus-daemon patchelf strace \
		libglvnd-gles libglvnd-egl libglvnd-glx mesa-dri-drivers mesa-vulkan-drivers vulkan-loader glibc-locale-source glibc-langpack-en
fi

# ponytail: no get-debloated-pkgs; it downgrades gtk4 and libadwaita to builds for
# Arch stable, which lack 1.10. Add it back once extra ships libadwaita 1.10.

cargo build --release --locked
install -Dm755 target/release/zaptide /usr/bin/zaptide

wget "$TOOLS/quick-sharun.sh" -O ./quick-sharun
chmod +x ./quick-sharun

export OUTPATH=./dist
export OUTNAME="zaptide-${VERSION:-dev}-$ARCH.AppImage"
export ICON=packaging/icons/zaptide.svg
export DESKTOP=packaging/applications/dev.luminusos.ZapTide.desktop
export STARTUPWMCLASS=dev.luminusos.ZapTide
export GTK_CLASS_FIX=1

./quick-sharun /usr/bin/zaptide
./quick-sharun --make-appimage
