# Packaging

ZapTide is distributed as a Flatpak. There are no native distribution packages.

ZapTide needs libadwaita 1.10, which ships with GNOME 51. Linux release
binaries build in a Fedora 45 container, the first with libadwaita 1.10.

## Flatpak

`packaging/flatpak/dev.luminusos.ZapTide.yml` builds from the local checkout,
fetching Cargo dependencies during the build. The adjacent bundle manifest
reuses the Linux release binary. Both grant Wayland/X11, GPU, audio, network
and keyring access; attachments chosen by the user use portals. No
home-directory permission is granted. `--persist=.local/state` keeps the
archive and session on Flatpak versions without `XDG_STATE_HOME`.

Build and install locally with Flatpak Builder (GNOME 51 runtime and SDK):

```sh
flatpak-builder --user --install --force-clean --install-deps-from=flathub build-dir packaging/flatpak/dev.luminusos.ZapTide.yml
```

To create a distributable bundle instead, export the build to a local repo:

```sh
mkdir -p target/flatpak
flatpak-builder --user --force-clean --install-deps-from=flathub --repo=target/flatpak/repo target/flatpak/build packaging/flatpak/dev.luminusos.ZapTide.yml
flatpak build-bundle target/flatpak/repo target/flatpak/zaptide-dev.flatpak dev.luminusos.ZapTide
```

To generate a pinned offline Flathub checkout, Python needs `aiohttp`,
`tomlkit` and `PyYAML`:

```sh
packaging/flatpak/flathub.sh vX.Y.Z /path/to/flathub-checkout
flatpak-builder --user --install --force-clean build-dir /path/to/flathub-checkout/dev.luminusos.ZapTide.yml
```

Flathub submission is a separate, manual step.
[Flathub's requirements](https://docs.flathub.org/docs/for-app-authors/requirements#generative-ai-policy)
prohibit AI agents from submitting or writing submission interactions and
require disclosure of generated material.

The release workflow builds the Linux binaries once, packages the x86_64 one
as a Flatpak bundle with `dev.luminusos.ZapTide.bundle.yml`, and lists every
file in `checksums.txt`.

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
