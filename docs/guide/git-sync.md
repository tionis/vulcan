# Git-backed device synchronization

Vulcan can synchronize the complete canonical vault through a dedicated Git live branch. Each `vulcan sync run` is a finite, direct transaction: it captures the current worktree without touching the normal Git index, reconciles the configured live ref, applies an accepted tree only after preserving local bytes, and refreshes an existing cache. The daemon schedules the same workflow but is not required.

This is device/file-tree synchronization. It replicates the whole vault rather than selecting or translating notes for an external wiki.

## Ordinary Linux and Windows setup

Preview a clone and device-local registration first:

```sh
vulcan --output json vault clone <remote> <vault-path> --id personal --dry-run
```

Then create it and run a finite sync:

```sh
vulcan vault clone <remote> <vault-path> --id personal
vulcan sync doctor personal
vulcan sync run personal
```

The default colocated layout keeps `.git/` beside the vault. `sync run` does not start the daemon. It also works against an unregistered repository selected with `--vault <path>`.

## Android and Termux

Android shared storage is suitable for the Obsidian-visible files but not for Git's private repository machinery. Keep the worktree in shared storage and the detached Git directory inside Termux-private storage.

After granting Termux storage access and installing Git, preview the layout:

```sh
termux-setup-storage
pkg install git
vulcan --output json vault clone <remote> /storage/emulated/0/Documents/Personal \
  --id personal \
  --git-dir ~/.local/share/vulcan/git/personal.git \
  --platform android-shared \
  --dry-run
```

Apply the same command without `--dry-run`, then use one-shot commands whenever synchronization is wanted:

```sh
vulcan sync status personal
vulcan sync doctor personal
vulcan sync run personal
```

These commands work from an interactive Termux shell, a Termux shortcut, or an explicitly configured scheduler. They neither require nor implicitly start the Vulcan daemon. Android lifecycle and battery integration are packaging concerns layered over this same finite command.

The `android-shared` profile records the filesystem limitations instead of pretending it behaves like native Linux storage:

- executable bits are not representable;
- symlinks are checked out as link files;
- case-folding, Unicode-normalization, Windows-reserved-name, and path-length hazards are diagnosed before unsafe publication or application;
- case-only renames may require an intermediate path.

Do not place the detached Git directory in shared storage. Uninstalling Termux can remove its private objects and refs while leaving the visible vault behind. If that happens, preserve the vault and use `vulcan vault recover-git personal <remote> --dry-run` before applying recovery. Vulcan captures the surviving worktree first, but unpushed objects that existed only in the lost private directory cannot be reconstructed.

## Conflicts and recovery

A conflicting sync preserves the immutable candidates and may publish a safe materialization. The accepted remote bytes remain at the original path, the competing local bytes appear below `.sync-conflicts/<conflict-id>/local/`, and clean paths continue synchronizing. The hidden directory is not indexed as notes.

Inspect and resolve through Vulcan rather than editing its refs or conflict-copy structure manually:

```sh
vulcan sync conflicts
vulcan sync conflicts <conflict-id>
vulcan sync resolve <conflict-id> --side local --dry-run
vulcan sync resolve <conflict-id> --side local
```

Complete-file, patch, editor, and reviewed agent-proposal modes are also available. Successful resolution removes the hidden conflict directory atomically while retaining the original Git objects and durable conflict record.

If a finite cycle is interrupted or the network is unavailable, rerun `vulcan sync run`. Device-local journals and Git refs retain the captured state; do not delete them or replace the vault with a fresh clone as a recovery shortcut.
