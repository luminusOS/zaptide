# Task 7: Dependency Audit

**Date:** 2026-09-25
**Feature flag:** `native-shell`
**Cargo tree command:** `cargo tree -d --features native-shell`

## 1. Forbidden UI Dependencies (check-native-ui-deps.sh)

### Current status: FAILING (expected)

The script correctly identifies forbidden dependencies that must be removed
before Task 7 completion.

### Forbidden packages in Cargo.toml

| Package | Version | Role |
|---------|---------|------|
| `egui` | 0.36 | Immediate-mode UI framework |
| `eframe` | 0.36 | egui application shell (window, event loop) |
| `egui_extras` | 0.36 | egui extensions (file, image, SVG, WebP, GIF) |

### Source files with `use egui` imports

16 source files import egui directly:

- `src/animation.rs`, `src/bidi.rs`, `src/emoji.rs`, `src/markup.rs`,
  `src/qr.rs`, `src/theme.rs`, `src/util.rs`
- `src/ui/mod.rs`, `src/ui/chats.rs`, `src/ui/conversation.rs`,
  `src/ui/dialogs.rs`, `src/ui/keys.rs`, `src/ui/login.rs`,
  `src/ui/picker.rs`, `src/ui/polls.rs`, `src/ui/settings.rs`,
  `src/ui/update.rs`, `src/ui/widgets.rs`
- `src/demo/tour.rs`, `src/theme/custom.rs`

### What must change for the script to pass

1. Remove `egui`, `eframe`, `egui_extras` from `[dependencies]` in Cargo.toml
2. Replace all `use egui` imports with native GTK4/libadwaita/Relm4 equivalents
3. Remove egui-specific rendering code (`src/markup.rs`, `src/emoji.rs`,
   `src/animation.rs` egui paths) and replace with GTK rendering
4. Update `src/ui/` modules to use Relm4 components instead of egui views
5. Remove `egui_glow`, `epaint`, `ecolor`, `emath`, `egui-winit` from the
   dependency tree (these are transitive through eframe/egui)

## 2. Duplicate Runtime Stacks (cargo tree -d)

### GTK4 / GLib / libadwaita / Relm4 stacks

**Zero duplicate runtime stacks.**

| Crate | Version | Duplicates |
|-------|---------|------------|
| `glib` | 0.22.10 | None |
| `gtk4` | 0.11.5 | None |
| `libadwaita` | 0.9.2 | None |
| `relm4` | 0.11.0 | None |
| `gdk4` | 0.11.5 | None |
| `gsk4` | 0.11.5 | None |
| `pango` | 0.22.9 | None |
| `gio` | 0.22.10 | None |
| `cairo-rs` | 0.22.9 | None |
| `gdk-pixbuf` | 0.22.0 | None |
| `graphene-rs` | 0.22.8 | None |

All GTK/GNOME platform crates resolve to a single version. The native migration
stack is internally consistent.

### Duplicate Wayland/low-level crates

| Crate | Versions | Root cause |
|-------|----------|------------|
| `calloop` | 0.13.0, 0.14.4 | eframe/winit uses 0.13; zbus/async-io uses 0.14 |
| `calloop-wayland-source` | 0.3.0, 0.4.1 | Follows calloop split |
| `smithay-client-toolkit` | 0.19.2, 0.20.0 | eframe/winit uses 0.19; rfd/wayland-backend uses 0.20 |
| `rustix` | 0.38.44, 1.1.4 | eframe/winit/calloop 0.13 use 0.38; zbus/async-io uses 1.1 |

### Other notable duplicates

| Crate | Versions | Root cause |
|-------|----------|------------|
| `hashbrown` | 0.15.5, 0.16.1, 0.17.1 | Different consumers pin different versions |
| `darling` | 0.21.3, 0.24.1 | diesel_derives uses 0.21; bon-macros uses 0.24 |
| `syn` | 2.0.119, 3.0.4 | Most proc-macros use 2.x; bon/async-trait/serde use 3.x |
| `thiserror` | 1.0.69, 2.0.20 | calloop/mp4/sctk 0.19 use 1.x; wacore/rodio/buffa use 2.x |
| `getrandom` | 0.2.17, 0.3.4, 0.4.3 | ring uses 0.2; ahash uses 0.3; rand uses 0.4 |
| `png` | 0.17.16, 0.18.1 | eframe uses 0.17; image and dev-dependencies use 0.18 |
| `system-deps` | 7.0.8, 9.0.0 | libadwaita-sys uses 7.x; gtk4-sys uses 9.x |
| `toml` | 0.9.12, 1.1.4 | diesel_migrations uses 0.9; system-deps 9.x uses 1.1 |
| `phf` | 0.11.3, 0.13.1 | mime_guess2 build-dep uses 0.11; emojis uses 0.13 |
| `kurbo` | 0.11.3, 0.13.1 | resvg/usvg uses 0.11; vello/peniko uses 0.13 |
| `libwebp-sys2` | 0.1.11, 0.2.0 | image uses 0.1.11; webp-animation wraps 0.2.0 |
| `miniz_oxide` | 0.8.9, 0.9.1 | png 0.17 uses 0.8; flate2 uses 0.9 |

## 3. Recommendations

### Wayland duplicates (calloop, smithay-client-toolkit, rustix)

**Root cause:** The egui/eframe/winit stack pins older versions of these crates
while the D-Bus stack (zbus/async-io) has moved to newer versions. These splits
exist because winit 0.30.x depends on calloop 0.13 and rustix 0.38.

**Recommendation:** **Accept for now, resolve with egui removal.** Once Task 7
removes eframe/egui, the winit/calloop 0.13 path disappears entirely. The
native-shell stack (GTK4/Relm4) uses GLib's own event loop, not calloop. The
remaining calloop 0.14 usage comes from zbus and will be the sole version.

### GTK/GLib/libadwaita/Relm4

**Recommendation:** **No action needed.** Zero duplicates. The GNOME platform
stack is fully consolidated at a single version.

### Other duplicates (hashbrown, syn, darling, thiserror, etc.)

**Recommendation:** **Accept.** These are compile-time or utility crates with
no runtime cost from duplication. Version splits come from upstream dependency
policies (diesel, bon, whatsapp-rust) and are not actionable from ZapTide's
Cargo.toml. Pinning would create fragile overrides.

### system-deps split (7.x vs 9.x)

**Root cause:** `libadwaita-sys` 0.9.2 uses `system-deps` 7.x while `gtk4-sys`
0.11.5 uses `system-deps` 9.x. This is a build-time only dependency with no
runtime impact.

**Recommendation:** **Accept.** Will resolve when libadwaita-sys updates.

## 4. Summary

- **Forbidden UI deps:** 3 packages (egui, eframe, egui_extras) in Cargo.toml,
  16+ source files with egui imports. Expected to fail until Task 7 removes them.
- **GTK runtime stack duplicates:** Zero
- **GLib runtime stack duplicates:** Zero
- **libadwaita runtime stack duplicates:** Zero
- **Relm4 runtime stack duplicates:** Zero
- **Wayland duplicates:** 3 crate families (calloop, smithay-client-toolkit,
  rustix), caused by eframe/winit pinning older versions. Resolves with egui
  removal.
- **Other duplicates:** Compile-time/utility crates, accepted as upstream policy.
