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
both with `dev.luminusos.ZapTide.bundle.yml`, and lists each package file in
`checksums.txt`. The same run builds `flatpak-release-manifest.json`, which
records the tag, source revision, workflow run and the two bundle artifact IDs,
sizes and SHA-256 hashes, and `flatpak-export-report.json`, which records the
verified Flatpak refs, OSTree commits and runtimes for both architectures. Both
stay as artifacts of the workflow run and are not attached to the GitHub
release. The manifest follows the versioned distribution contract; the report
is separate from that schema. The Luminus Flatpak repository does not read them:
it rebuilds the same record from `checksums.txt`, the downloaded bundles and the
verified run.
Final tags export on the `stable` branch, while `-rcN` and `-rc.N` tags use
`testing`.

## AppImage

The x86_64 AppImage is built with
[quick-sharun](https://github.com/pkgforge-dev/Anylinux-AppImages) in an Arch
Linux container. It bundles glibc, GTK, libadwaita, the Adwaita icon theme,
GStreamer, the glycin loaders and bubblewrap, so it needs nothing from the host
distribution. ZapTide uses the bundled icons whatever icon theme the desktop is
set to, because GTK would otherwise look only in that theme. Arch
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

The AppDir follows the AppImage convention: the application lives under `usr/`
(`usr/bin`, `usr/lib`, `usr/share`). sharun's `AppRun` also looks for those
directories beside itself, so `build.sh` leaves a link to each one at the top
level. The build fails if `usr/bin/zaptide` is missing.

Each AppImage embeds the update information
`gh-releases-zsync|luminusOS|zaptide|latest|zaptide-*-<arch>.AppImage.zsync`, and
the build writes the matching `.zsync` file next to it. The release publishes both,
so [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) and
`appimageupdatetool` download only the changed blocks:

```sh
appimageupdatetool zaptide.AppImage
```

## Build dependencies

| Dependency | Fedora | Notes |
|------------|--------|-------|
| GTK 4.14+, libadwaita 1.10+ | `gtk4-devel libadwaita-devel` | GNOME 51 |
| ALSA | `alsa-lib-devel` | Audio |
| GStreamer 1.20+ | `gstreamer1-devel`, `gstreamer1-plugins-base-devel` | Playback and calls; base and good plugins at runtime. Video calls also need an H.264 encoder (`gstreamer1-plugin-openh264` or x264) and `gst-libav` |
| cmake | `cmake` | Builds libopus via opusic-sys |
| libseccomp, fontconfig | `libseccomp-devel fontconfig-devel` | glycin; needs `glycin-loaders` and `bubblewrap` at runtime |
| Perl | `perl` | Bundled OpenSSL |

SQLCipher, OpenSSL and libopus are compiled from source by their `-sys` crates.

Calls add the `rtc-*` crates (DTLS, SCTP and data channels), which are still 0.21 beta releases, and `rkyv`. Their licences are MIT or Apache-2.0. The AppImage does not yet check for the H.264 encoder, `v4l2src` or the autodetect source and sink plugins that calls use.

## Flatpak limitations

- **Calls**: voice calls and the ring use the PulseAudio socket. The ring looks for the freedesktop sound theme in the data directories and falls back to a tone it makes, so it works without that theme. Notification buttons go through the notification portal; the desktop's do-not-disturb setting is read through GSettings and may not be visible in the sandbox. Video calls are not supported: the manifest grants no camera access (`--device=all` or the camera portal through PipeWire) and no H.264 codec extension (`org.freedesktop.Platform.openh264`). Neither was added because they widen the sandbox and nothing here can test them.

- **Tray icon**: GNOME shows StatusNotifierItem only with an extension such as AppIndicator.
