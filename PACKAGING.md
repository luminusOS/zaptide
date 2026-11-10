# Packaging

ZapTide releases include Flatpak bundles and AppImages for x86_64 and aarch64.
Native distribution packages are not available.

The application is `GPL-3.0-or-later`. `packaging/install-licenses.sh` installs
the GPL text, the inherited `LICENSE-MIT`, `NOTICE` and vendored notices in every
distribution format. Keep those notices intact. Binary distribution also
requires corresponding source under the applicable licences, including local
patches and build scripts. These notices alone do not cover every dependency
or bundled native library; review their source and notice obligations before
publishing packages.

ZapTide needs libadwaita 1.10 (GNOME 51). Release binaries build in Fedora 45.

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

The release workflow builds native binaries for both architectures and packages
them with `dev.luminusos.ZapTide.bundle.yml`. Package hashes go in `checksums.txt`.
Two reports stay in the workflow artifacts, not the GitHub release:

- `flatpak-release-manifest.json`: tag, source revision, workflow run, bundle
  artifact IDs, sizes and SHA-256 hashes, following the versioned distribution
  contract.
- `flatpak-export-report.json`: verified Flatpak refs, OSTree commits and
  runtimes for both architectures, separate from the manifest schema.

The Luminus Flatpak repository rebuilds the record from `checksums.txt`,
downloaded bundles and the verified run; it does not consume these reports.
Final tags export on `stable`; `-rcN` and `-rc.N` tags use `testing`.

## AppImage

The x86_64 AppImage is built with
[quick-sharun](https://github.com/pkgforge-dev/Anylinux-AppImages) in an Arch
Linux container. It bundles glibc, GTK, libadwaita, the Adwaita icon theme,
GStreamer, the glycin loaders and bubblewrap. ZapTide uses its bundled icons
regardless of the desktop's icon theme; GTK would otherwise search that theme. Arch
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

`build.sh` fetches quick-sharun from a fixed commit and stops unless the file
matches `QUICK_SHARUN_SHA256`, so a bump changes `QUICK_SHARUN_URL` and the hash
together. The script then downloads sharun, appimagetool, cross-libc-dlopen and
onelf at the release versions it names; those are pinned by version but not
hashed. Its self-updater hook, the only part that would fetch the latest
AppImageUpdate, is not enabled because `build.sh` sets no `ADD_HOOKS`. Tag
builds restore no CI caches, so a release never reuses build output from
another branch.

The CI and release workflows name their containers by digest, keeping the tag
for readers. Dependabot does not update `container:` digests, so re-pin them
before a release. Each command prints the digest of the multi-architecture
index, which replaces the `sha256:` part of every reference to that image:

```sh
skopeo inspect --raw docker://registry.fedoraproject.org/fedora:45 | sha256sum
skopeo inspect --raw docker://docker.io/library/archlinux:latest | sha256sum
skopeo inspect --raw docker://ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-25.08 | sha256sum
```

The Arch image has only an x86_64 manifest and is used only by the x86_64
AppImage job.

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
| GStreamer 1.20+ | `gstreamer1-devel`, `gstreamer1-plugins-base-devel` | Voice notes (recording, playback, waveforms) and calls; base and good plugins at runtime. Video calls also need an H.264 encoder (`gstreamer1-plugin-openh264` or x264) and `gst-libav` |
| cmake | `cmake` | Builds libopus via opusic-sys |
| libseccomp, fontconfig | `libseccomp-devel fontconfig-devel` | glycin; needs `glycin-loaders` and `bubblewrap` at runtime |
| Perl | `perl` | Bundled OpenSSL |

SQLCipher, OpenSSL and libopus are compiled from source by their `-sys` crates.

Calls use the `rtc-*` crates (DTLS, SCTP and data channels), still at 0.21 beta,
and `rkyv`, under MIT or Apache-2.0 licences. AppImage packaging does not yet
check for an H.264 encoder, `v4l2src` or the autodetect source and sink plugins.

GTK's emoji chooser needs a colour emoji font: `google-noto-emoji-color-fonts`
on Fedora or `noto-fonts-emoji` on Arch. Without it, emoji may be monochrome
or boxes. Arch needs the `gstreamer` headers for audio builds.

## Flatpak permissions and limitations

### Calls

Voice calls and ringing use the PulseAudio socket. Ringing uses the freedesktop
sound theme or a generated tone. Notification buttons use the notification
portal. GSettings may not expose desktop Do Not Disturb inside the sandbox.

Video uses `org.freedesktop.portal.Camera`. ZapTide detects the sandbox through
`/.flatpak-info` or a non-empty `FLATPAK_ID`, requests access, opens the portal's
PipeWire remote and runs `pipewiresrc` on its camera node. This needs no device,
filesystem or bus-name permission in the manifest; do not add `--device=all`.

GNOME 51 includes the GStreamer PipeWire plugin, but no H.264 encoder in the
base runtime. ZapTide uses `x264enc` from `org.freedesktop.Platform.codecs-extra`,
which the runtime declares and Flatpak installs when a remote carries it, not
`org.freedesktop.Platform.openh264`. `avdec_h264` was present in a sandbox with
`codecs-extra`. The missing-extension case has not been tested; it reports a
missing encoder or decoder. The camera portal route was checked against a real
session portal outside a sandbox, not inside a built Flatpak.

### Keyring

Inside Flatpak, oo7 keeps archive keys in
`~/.var/app/dev.luminusos.ZapTide/data/keyrings/default.keyring`. The Secret
portal stores the per-app secret that encrypts this file in the host keyring.
Backups and moves need both. Desktops without a Secret portal backend use the
host Secret Service directly. Outside Flatpak, keys are found in any Secret
Service collection and created in the default collection only when absent;
nothing is migrated.

On first start after an upgrade from a pre-oo7 build, ZapTide copies each
missing key from the host Secret Service, reads the copy back and leaves the
original untouched. Both manifests retain `--talk-name=org.freedesktop.secrets`
to read those legacy keys. Remove it only after deliberately closing the
migration window; desktops without a Secret portal will then need one.

Synthetic credential and migration tests do not prove real-daemon or Flatpak
migration acceptance. That still needs a user-operated test account and the
applicable capture checks. Closing superseded implementation PRs is not a
substitute for those checks.

### Tray

GNOME shows StatusNotifierItem only with an extension such as AppIndicator.

## Releases

Cut releases for a feature or a batch of fixes worth announcing, not every
commit. A regression in a just-released version can ship immediately. Agents
need explicit authorization for commits, pushes, tags and publication.

1. Bump `version` in `Cargo.toml` and update `Cargo.lock` with a build. Run the
   full checks in [CONTRIBUTING.md](CONTRIBUTING.md#verify-your-change), then
   commit and push before tagging so the binaries report the right version.
2. Tag `vX.Y.Z` and push the tag. Wait for every build, package and `checksums.txt`.
   A pushed tag alone does not complete a release.
3. Replace generated GitHub notes with written release notes after the artifacts
   are ready. Verify all media links before publishing.

Before writing notes, read the previous two stable releases and match their
style: a short plain-language summary, `New` and `Fixed` sections with bold
user-facing results, a `Thanks` section and a full-changelog link comparing
the previous tag. Credit contributors on the relevant item with issue or PR
numbers, and acknowledge reporters separately. Include screenshots or short
videos of the main features when useful, using synthetic data only. Describe
known limitations and do not use em dashes.
