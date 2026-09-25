# Upstream relationship

ZapTide is a fork of [ZapFast](https://github.com/crmne/zapfast). The fork started from ZapFast
commit `0d8cc506da2fe1b40f9d89aa31776ced02f6b1b4` on 2026-09-21. This is newer than the planning
snapshot `e18dea5d34a399436b4149b87233f41e751b08a7` and includes upstream fixes through pull request
117.

ZapFast remains the source for protocol, archive, backend, model, and egui UI improvements. ZapTide
owns its Linux/GNOME shell, product identity, packaging, data namespace, and GTK integration.

## Data boundary

ZapTide uses its own XDG directories, keyring service, process identity, and single-instance wire
protocol. It never opens, moves, copies, or deletes ZapFast data automatically. Users link ZapTide
as a separate companion device.

Any future importer needs a separate reviewed plan covering explicit consent, archive and device
store compatibility, keyring mapping, atomic copy, collision handling, rollback, and retry behavior.

## Syncing ZapFast changes

Add the source repository as an `upstream` remote in a local checkout:

```sh
git remote add upstream https://github.com/crmne/zapfast.git
git fetch upstream
```

Before importing changes:

1. Review the full upstream range and classify changes by protocol/core, egui UI, platform shell,
   packaging, and product identity.
2. Import protocol/core and egui fixes with merge or cherry-pick, preserving upstream authorship.
3. Resolve shell, path, keyring, updater, and packaging changes against ZapTide policy instead of
   accepting ZapFast identity or migration behavior.
4. Search the resulting diff for `ZapFast`, `zapfast`, `FastsApp`, `fastsapp`, `crmne/zapfast`, and
   `rocks.zapfast.ZapFast`. Historical references in this file are expected; runtime references need
   explicit review.
5. Run formatting, clippy, all target tests, all feature tests, documentation, and data-isolation
   tests before pushing.

Record every sync in the merge or commit message with the imported upstream range and any skipped
commits.

## Importing ZapFast Upstream Fixes

ZapTide forks ZapFast at commit 0d8cc506. Upstream fixes to the whatsapp-rust
protocol library, encrypted archive, backend worker, and domain models can be
imported without restoring egui UI code.

### Process

1. Cherry-pick the ZapFast commit(s) that fix protocol/archive/backend issues
2. Resolve conflicts in `src/backend/worker.rs`, `src/archive.rs`, `src/model.rs`
3. DO NOT import changes to `src/ui/`, `src/app.rs`, `src/main.rs`, or `src/theme.rs`
   (these are egui-specific and have been replaced by native GTK4/Relm4)
4. Run `scripts/check-native-ui-deps.sh` to verify no egui dependencies were reintroduced
5. Run full CI suite
6. If CI passes, merge; if not, resolve without adding egui back

### Files that should NEVER be imported from ZapFast upstream

- `src/ui/*` (replaced by `src/application.rs`, `src/native_*.rs`)
- `src/app.rs` (egui App struct, replaced by Relm4 NativeApplication)
- `src/main.rs` (eframe::run_native, replaced by RelmApp)
- `src/theme.rs` (egui colors, replaced by `src/native_theme.rs`)
- `src/demo.rs` (egui layout tests, replaced by native tests)
- `src/macos.rs` (egui chrome, native uses libadwaita)
- `Cargo.toml` egui/eframe dependencies (forbidden by CI check)

### Files that CAN be imported from ZapFast upstream

- `src/backend.rs` (protocol bridge)
- `src/backend/worker.rs` (protocol worker)
- `src/archive.rs` (encrypted archive)
- `src/model.rs` (domain models)
- `src/voice.rs` (audio codec)
- `src/audio.rs` (playback/recording)
- `src/markup.rs` (text formatting)
- `src/emoji.rs` (emoji rendering)
- `src/updates.rs` (release management)
