# Packaging

ZapTide releases include a Flatpak bundle and an x86_64 AppImage. Native
distribution packages are not available.

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

The release workflow builds Linux binaries once, packages the x86_64 binary
with `dev.luminusos.ZapTide.bundle.yml`, and lists every file in
`checksums.txt`.

## AppImage

The x86_64 AppImage uses the Fedora 45 release build environment. It bundles
the GTK and libadwaita libraries linked by ZapTide. To build it locally,
install `cargo-appimage` 2.4.0 and `appimagetool`, then run:

```sh
APPIMAGE_EXTRACT_AND_RUN=1 cargo appimage --locked
```

The result is `target/appimage/zaptide.AppImage` (or under `CARGO_TARGET_DIR`
when set). Media playback and image loading also need GStreamer plugins,
glycin loaders and bubblewrap on the host.

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
