# Packaging

[`native-packages.yaml`](native-packages.yaml) is the packaging configuration:
it pins the shared CLI and nFPM versions and declares Linux amd64/arm64 inputs,
RPM contents and dependencies.
Application assets stay in `packaging/`.

ZapTide uses the `zaptide` binary and an isolated package identity. It does not
provide, conflict with, replace, or migrate ZapFast or FastsApp packages. The
GitHub repository is `luminusOS/zaptide`, so source archives extract into
`zaptide-VERSION`. No stable ZapTide packages have been published yet.

```sh
gem install native-packages --version 0.6.0
native-packages validate
native-packages doctor --target linux-amd64 --target linux-arm64
native-packages build --release v1.2.3 --target linux-amd64 --target linux-arm64
```

Replace `v1.2.3` with an existing stable application release. Local use also
requires nFPM 2.47.0, `bsdtar` and `readelf`. CI installs its tooling. To
package local release archives, put every configured input under `dist/`, then run
`native-packages build --version 1.2.3 --target linux-amd64 --target linux-arm64`. Outputs go to
`dist/packages/1.2.3`; use `--output` for a fresh destination when rebuilding.

Stable tags run the existing native build jobs first. After binaries and
`checksums.txt` are published, the shared workflow verifies their hashes,
builds the configured packages, and attaches them to the GitHub release.
Package checksums are separate from the original binary checksums. PR validation never publishes.

Review or publish an existing build with the same installed CLI:

```sh
native-packages publish --from dist/packages/1.2.3 --to github
native-packages repositories
native-packages status --offline
```

The Flatpak build steps remain responsible for their native artifacts. Additional nFPM formats require suitable platform
inputs and dependencies; adding a format does not port the application.
See the [shared CLI documentation](https://github.com/crmne/native-packages/tree/v0.6.0)
for commands and supported formats.

To upgrade the tool, change `tool.version` in `native-packages.yaml`, the matching immutable workflow reference, and any release-job gem installation
pin together. Applications need no packaging Gemfile, lockfile or Ruby wrapper.

Linux releases build in a Fedora 45 container, the first with libadwaita 1.10.
The RPM declares runtime-loaded Wayland, X11 and EGL libraries as well as ALSA
and its PulseAudio plugin. Packaging runs for an explicit published ZapTide
version through manual dispatch or a release workflow call. A clean Fedora
container installs and removes the package, checks GUI libraries loaded with
`dlopen`, and verifies desktop assets. Run the same check locally with
`bash packaging/test-install.sh fedora:45 /path/to/native-packages-output`.

## Flatpak

`packaging/flatpak/dev.luminusos.ZapTide.yml` builds from the local checkout,
fetching Cargo dependencies during the build. The adjacent bundle
manifest reuses the Linux release binary, as in Spotifast. Both grant Wayland/X11,
GPU, audio, network and keyring access; attachments chosen by the user use
portals. No home-directory permission is granted. `--persist=.local/state` keeps
the archive and session on Flatpak versions without `XDG_STATE_HOME`.

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

Flathub submission/review is a separate publication step; the manifest alone does
not make ZapTide available in Flathub. A maintainer must submit it manually:
[Flathub's requirements](https://docs.flathub.org/docs/for-app-authors/requirements#generative-ai-policy)
prohibit AI agents from submitting or writing submission interactions and require
disclosure of generated material. Review the manifests and these changes before
submitting. The manifests use the GNOME 51 runtime, the first with libadwaita 1.10; the CI builder
container is 25.08 and installs the runtime and SDK named by the manifest. The GitHub release job includes the bundle
in `checksums.txt`. No existing release files are replaced by this change.

## Supported distributions

ZapTide needs libadwaita 1.10, which ships with GNOME 51.

- **Fedora 45+**.
- **Arch Linux**: rolling release, always has latest GTK and libadwaita.
- **Flatpak**: GNOME 51 runtime, for every other distribution.
- **Ubuntu and Debian**: not until they package libadwaita 1.10; use the Flatpak.

## Build dependencies

| Dependency | Fedora | Arch | Notes |
|------------|--------|------|-------|
| Rust 1.75+ | `rust cargo` | `rust` | Stable channel |
| GTK 4.14+ | `gtk4-devel` | `gtk4` | |
| libadwaita 1.10+ | `libadwaita-devel` | `libadwaita` | GNOME 51 |
| ALSA | `alsa-lib-devel` | `alsa-lib` | Required for audio |
| GStreamer | `gstreamer1-devel` | `gstreamer` | Linux audio playback; base and good plugins at runtime |
| cmake | `cmake` | `cmake` | Builds libopus via opusic-sys |
| libseccomp, fontconfig | `libseccomp-devel fontconfig-devel` | `libseccomp fontconfig` | glycin, which loads photos; needs `glycin-loaders` and `bubblewrap` at runtime |
| gettext | `gettext` | `gettext` | Compiles i18n catalogs |
| glib2-devel | `glib2-devel` | `glib2` | Provides glib-compile-schemas |
| desktop-file-utils | `desktop-file-utils` | `desktop-file-utils` | desktop-file-validate |
| appstream | `appstream` | `appstream` | appstreamcli for metainfo |

SQLCipher, OpenSSL, openh264, and libopus are bundled via their respective `-sys` crates and compiled from source. No system packages required beyond cmake for libopus.

## Build instructions

### Install dependencies

**Fedora:**

```sh
sudo dnf install rust cargo gtk4-devel libadwaita-devel alsa-lib-devel gstreamer1-devel gstreamer1-plugins-base gstreamer1-plugins-good cmake gettext glib2-devel desktop-file-utils appstream
```

**Arch:**

```sh
sudo pacman -S rust gtk4 libadwaita alsa-lib gstreamer gst-plugins-base gst-plugins-good cmake gettext glib2 desktop-file-utils appstream
```

### Build and test

```sh
cargo build --release --locked --features native-shell
cargo test --locked --features native-shell
```

### Manual installation

```sh
sudo install -Dm755 target/release/zaptide /usr/local/bin/zaptide
sudo install -Dm644 data/dev.luminusos.ZapTide.desktop /usr/share/applications/
sudo install -Dm644 data/dev.luminusos.ZapTide.metainfo.xml /usr/share/metainfo/
sudo install -Dm644 data/dev.luminusos.ZapTide.gschema.xml /usr/share/glib-2.0/schemas/
sudo glib-compile-schemas /usr/share/glib-2.0/schemas/
sudo install -Dm644 packaging/icons/zaptide.svg /usr/share/icons/hicolor/scalable/apps/dev.luminusos.ZapTide.svg
```

For automated RPM packaging, use `native-packages` as described above.

## Flatpak sandbox limitations

- **Microphone**: requires `--socket=pulseaudio` (already in manifest).
- **Background mode**: requires `--talk-name=org.freedesktop.portal.Background` (add if not present).
- **Tray icon**: GNOME does not support StatusNotifierItem by default; requires optional extension (TopIcons Plus or similar).
- **Native builds**: no sandbox limitations beyond standard XDG directory access.

Flatpak grants Wayland/X11, GPU, audio, network, keyring access. No home-directory permission is granted; `--persist=.local/state` keeps archive and session on versions without `XDG_STATE_HOME`.

## Data locations

| Type | Path | Notes |
|------|------|-------|
| Config | `$XDG_CONFIG_HOME/zaptide/` | Typically `~/.config/zaptide/` |
| Cache | `$XDG_CACHE_HOME/zaptide/` | Typically `~/.cache/zaptide/` |
| Logs | `$XDG_STATE_HOME/zaptide/` | Typically `~/.local/state/zaptide/` |
| Flatpak | `$HOME/.var/app/dev.luminusos.ZapTide/` | All data under this prefix |

ZapTide uses an isolated XDG namespace. Never reads ZapFast, FastsApp, or FastWhatsApp data. See `src/paths.rs`.

## Privacy model

- Archive encrypted with SQLCipher; key stored in OS keyring (not config file).
- No telemetry, no hosted backend, no analytics.
- Notifications are content-free by default (no message preview).
- Logs redact message content, phone numbers, keys, and QR payloads.
- ZapTide's keyring service and single-instance wire identity are separate from ZapFast.

## Relationship to ZapFast

ZapTide is a fork of ZapFast (commit `0d8cc506`). It preserves:

- whatsapp-rust protocol implementation
- Encrypted archive (SQLCipher)
- Backend worker architecture
- Message history and media handling

Replaced:

- ZapFast's egui/eframe interface with native GTK4/libadwaita/Relm4
- ZapFast's package identity and XDG namespace (isolated; no data migration)

ZapTide does not provide, conflict with, replace, or migrate ZapFast packages.

## Packaging scripts

| Script | Purpose |
|--------|---------|
| `packaging/build-metadata.sh` | Generates `.desktop`, metainfo, compiles schemas |
| `packaging/check.sh` | Validates metadata, checks for forbidden dependencies |
| `packaging/smoke-test.sh` | Builds and launches synthetic smoke session |
| `packaging/test-install.sh` | Tests RPM installation in a clean Fedora container |
| `scripts/check-native-ui-deps.sh` | Scans for forbidden UI frameworks (egui, webkit, etc.) |
| `scripts/perf-native-ui.sh` | Performance measurement script |
| `packaging/flatpak/flathub.sh` | Generates pinned Flathub checkout from release tag |

Run `bash packaging/check.sh` before submitting releases to validate metadata and dependencies.
