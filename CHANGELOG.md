# Changelog

## 0.2.1 — 2026-09-06

Vulcan 0.2.1 is the first published stable release after 0.1.0. It contains all changes described
for 0.2.0 below and makes Android release builds compatible with both the current NDK 29 `lib/`
host-library layout and the legacy `lib64/` layout.

## 0.2.0 — 2026-09-06 (tagged, not published)

Vulcan 0.2.0 advances the project from a local vault CLI into an experimental multi-device
information hub. It remains pre-alpha; keep independent backups and review mutations with
`--dry-run`.

### Highlights

- Added finite Git-backed device synchronization with recoverable checkpoints, deterministic
  conflict artifacts, reviewed resolutions, semantic proposals, and notification-driven pulls.
- Added the multi-vault daemon, authenticated HTTP and MCP surfaces, native user-service
  definitions, background watching, and scheduled synchronization.
- Added Android/Termux support with detached Git storage, platform preflight diagnostics, a focused
  clone workflow, scheduler helpers, and a full-featured aarch64 Android release artifact.
- Added bidirectional, conflict-aware Outline workflows, named integration routes, MDAF import
  foundations, EPUB and static-site publication, and durable reconciliation state outside the
  rebuildable cache.
- Expanded Dataview, Bases, TaskNotes, Kanban, periodic-note, template, plugin, custom-tool, and
  agent-skill compatibility.
- Added immutable stable and bounded rolling release channels, channel-scoped signed update
  metadata, checksummed portable archives, Debian packages, installers, and self-update support.

### Upgrade notes

- Git 2.38 or newer is required for the Git synchronization backend.
- `v0.2.1` is the stable update-signing trust bootstrap. Install it once from a manually verified
  checksum, archive, or package when upgrading from `v0.1.0`; later stable updates can be verified
  through the embedded `stable-2026-09` identity.
- Synchronization state, credentials, journals, and publisher mappings are durable device state and
  are intentionally stored outside the rebuildable SQLite cache.
