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
