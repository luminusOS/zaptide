# Packaging

ZapTide releases include Flatpak bundles and AppImages for x86_64 and aarch64.
Native distribution packages are not available.

ZapTide needs libadwaita 1.10 (GNOME 51). Linux release binaries build in
Fedora 45, the first Fedora version to include it.

## Flatpak

`packaging/flatpak/dev.luminusos.ZapTide.yml` builds this checkout and fetches
Cargo dependencies. The bundle manifest beside it packages the Linux release
binary. Both grant Wayland/X11, GPU, audio, network and keyring access.
User-selected attachments go through portals; neither manifest grants access
to the home directory. `--persist=.local/state` keeps the archive and session
on Flatpak versions without `XDG_STATE_HOME`.

Build and install locally with Flatpak Builder (GNOME 51 runtime and SDK):

```sh
flatpak-builder --user --install --force-clean --install-deps-from=flathub build-dir packaging/flatpak/dev.luminusos.ZapTide.yml
```

For a distributable bundle, export the build to a local repository:

```sh
mkdir -p target/flatpak
flatpak-builder --user --force-clean --install-deps-from=flathub --repo=target/flatpak/repo target/flatpak/build packaging/flatpak/dev.luminusos.ZapTide.yml
flatpak build-bundle target/flatpak/repo target/flatpak/zaptide-dev.flatpak dev.luminusos.ZapTide
```

Generating a pinned offline Flathub checkout needs Python's `aiohttp`,
`tomlkit` and `PyYAML`:

```sh
packaging/flatpak/flathub.sh vX.Y.Z /path/to/flathub-checkout
flatpak-builder --user --install --force-clean build-dir /path/to/flathub-checkout/dev.luminusos.ZapTide.yml
```

Flathub submission is a separate, manual step.
[Flathub's requirements](https://docs.flathub.org/docs/for-app-authors/requirements#generative-ai-policy)
prohibit AI agents from submitting or writing submission interactions and
require disclosure of generated material.

The release workflow builds a native binary for each architecture, packages
both with `dev.luminusos.ZapTide.bundle.yml`, and lists every file in
`checksums.txt`.

## AppImage

The x86_64 AppImage is built with
[quick-sharun](https://github.com/pkgforge-dev/Anylinux-AppImages) in an Arch
Linux container. It bundles glibc, GTK, libadwaita, GStreamer, the glycin
loaders and bubblewrap, so it needs nothing from the host distribution. Arch
stable has libadwaita 1.9, so `packaging/appimage/build.sh` enables the Arch
testing repositories until 1.10 reaches `extra`. To build it locally:

```sh
podman run --rm -v "$PWD":/src -w /src -e VERSION=dev archlinux ./packaging/appimage/build.sh
```

The result is `dist/zaptide-dev-x86_64.AppImage`. The aarch64 AppImage uses the
same script in a Fedora 45 container, because Arch has no official ARM64 image
and Arch Linux ARM ships libadwaita 1.9. `build.sh` picks the `dnf` path when
`pacman` is absent:

```sh
podman run --rm -v "$PWD":/src -w /src -e VERSION=dev registry.fedoraproject.org/fedora:45 ./packaging/appimage/build.sh
```

## Build dependencies

| Dependency | Fedora | Notes |
|------------|--------|-------|
| GTK 4.14+, libadwaita 1.10+ | `gtk4-devel libadwaita-devel` | GNOME 51 |
| ALSA | `alsa-lib-devel` | Audio |
| GStreamer | `gstreamer1-devel` | Playback; base and good plugins at runtime |
| cmake | `cmake` | Builds libopus via opusic-sys |
| libseccomp, fontconfig | `libseccomp-devel fontconfig-devel` | glycin; needs `glycin-loaders` and `bubblewrap` at runtime |
| Perl | `perl` | Bundled OpenSSL |

SQLCipher, OpenSSL and libopus are compiled from source by their `-sys` crates.

## Flatpak limitations

- **Tray icon**: GNOME shows StatusNotifierItem only with an extension such as AppIndicator.
