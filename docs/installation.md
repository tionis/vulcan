# Installing Vulcan

Vulcan is distributed as versioned, checksummed archives. Installing the CLI does not register a
wiki or enable the synchronization daemon. Git must be installed separately for the current Git
sync backend.

## Direct archive

Download the archive for your platform together with `SHA256SUMS`, verify its SHA-256 digest, and
extract it. Each archive has one top-level directory containing `vulcan` (`vulcan.exe` on Windows),
shell completions, the `vulcan(1)` man page, README, installation notes, and license files.

The supported target names are:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Place the executable in a stable path on `PATH`, such as `~/.local/bin/vulcan`. A stable path matters
when a native daemon service refers to it across upgrades. Replace the executable atomically, then
run `vulcan daemon install` again after an upgrade to refresh the native service definition.

## Checksum-verifying installers

The POSIX installer supports Linux and macOS and defaults to `~/.local`:

```sh
curl -fsSL https://raw.githubusercontent.com/tionis/vulcan/v0.1.0/scripts/install.sh | \
  sh -s -- --version 0.1.0 --dry-run
```

Review the plan, remove `--dry-run`, and ensure `~/.local/bin` is on `PATH`. Pass an explicit
`--prefix`; for example `--prefix /usr/local`, only when a system-wide installation is intended and
the invoking user has the required permission.

On Windows, download `scripts/install.ps1` from the matching tag and review:

```powershell
.\install.ps1 -Version 0.1.0 -DryRun
.\install.ps1 -Version 0.1.0 -AddToPath
```

The default Windows prefix is `%LOCALAPPDATA%\Programs\Vulcan`. `-AddToPath` is explicit because it
changes the persistent user environment. Both installers validate OS/architecture and SHA-256 before
replacing the executable, and neither writes daemon configuration or starts a service.

Each release also publishes a Homebrew formula and WinGet portable-manifest set generated from the
same verified artifact manifest. These files are publication inputs for the package repositories;
they do not create a second binary or configuration layout. The Homebrew formula supports Linux and
macOS and defines an optional service using Homebrew's stable `opt_bin` path. Installing the formula
does not start it; `brew services start vulcan` is a separate explicit action.

## Daemon service

Review and install the user service explicitly:

```sh
vulcan daemon install --dry-run
vulcan daemon install
vulcan daemon status
```

Linux uses `systemd --user`, macOS uses a LaunchAgent in `~/Library/LaunchAgents`, and Windows uses
a limited per-user Task Scheduler logon task. To remove only the service projection:

```sh
vulcan daemon uninstall --dry-run
vulcan daemon uninstall
```

Service removal and package removal preserve registered wikis, vault files, credentials, journals,
conflict records, and other device state. Remove those separately only when you deliberately want to
discard them.

Optional daemon-provider secrets may be stored as literal `NAME=value` records in
`$XDG_CONFIG_HOME/vulcan/daemon.env` (normally `~/.config/vulcan/daemon.env`). On Linux and macOS,
set mode `0600`. Inherited environment variables take precedence. The file does not support shell
expansion or commands.

## Development fallback

From a source checkout with the pinned Rust toolchain installed:

```sh
cargo install --locked --path vulcan-cli
```

This builds the current checkout and is intended as a development or fallback installation path.

## Upgrade, rollback, and removal

Run the same installer with the new version. The executable is staged and replaced at its stable
path. Then preview and refresh the native service so it cannot retain an obsolete executable path:

```sh
vulcan daemon install --dry-run
vulcan daemon install
vulcan daemon status
```

To roll back, run the installer with the prior supported version and refresh the service again. To
remove Vulcan, first run `vulcan daemon uninstall`, then remove the installed binary, man page, and
completion files from the selected prefix (or uninstall through the package manager). This preserves
all user and vault state by design.

Release archives are currently checksum-verifiable but unsigned. Production macOS notarization,
Developer-ID signing, Windows Authenticode signing, and signed provenance remain separate release
gates; contributor builds must not be represented as signed production releases.
