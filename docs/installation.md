# Installing Vulcan

Vulcan is distributed as versioned, checksummed archives. Installing the CLI does not register a
wiki or enable the synchronization daemon. Git 2.38 or newer must be installed separately for the
current Git sync backend.

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

### Update channels and portable self-update

Portable/manual installations can follow one of two update channels:

- `stable` is the default immutable release stream.
- `main` is an explicitly selected rolling development prerelease. It is replaced in place and is
  not a supported rollback boundary.

Check without changing the executable, then download, verify, and preview the complete update:

```sh
vulcan self-update check
vulcan self-update apply --dry-run
vulcan self-update apply
```

The updater requires HTTPS, validates the channel and target, requires a newer semantic version,
verifies signed metadata against trusted keys, checks the archive's exact size and SHA-256 digest,
and atomically replaces the running executable. `--allow-downgrade` is the explicit exception for a
reinstall or rollback. Restart a running daemon after applying an update.

Current builds embed the separate `stable-2026-09` identity with stable-only authority. After the
first post-bootstrap stable release is published and signed, the ordinary commands above verify it
without an exception. A binary from before that trust bootstrap cannot authenticate the first
signed stable descriptor; install that one release from a manually verified checksum/archive or
package. Do not normalize `--allow-unsigned` as the stable update path. The currently published
`v0.1.0` release predates the update-channel descriptor, so portable stable self-update remains
unavailable until the first post-bootstrap stable release; install a current checksummed
archive/package manually in the meantime.

Rolling descriptors are signed by the dedicated `main-2026-09` identity after the automated build
completes. Release binaries embed its public key with `main`-only authority, so a portable binary
can opt into the authenticated rolling stream without weakening stable-channel trust:

```sh
vulcan self-update check --channel main
vulcan self-update apply --channel main --dry-run
vulcan self-update apply --channel main
```

A binary built before the `main-2026-09` public key was embedded cannot verify that signature. Give
that old binary one explicitly accepted `--allow-unsigned` bootstrap update or install a checksummed
current archive manually; subsequent rolling updates verify normally. If a newly published rolling
descriptor is still in its short unsigned handoff window, wait for the hosted signing workflow
instead of normalizing `--allow-unsigned` as the ongoing update path.

The fixed rolling release page and direct assets are at
`https://github.com/tionis/vulcan/releases/tag/rolling-main`. It is checked at most daily, publishes
only a new `main` commit whose push CI passed, and keeps one prerelease instead of accumulating
nightly releases.

The browser can download any archive or Debian package from that page. With GitHub CLI, for example:

```sh
gh release download rolling-main --repo tionis/vulcan \
  --pattern 'vulcan-*-x86_64-unknown-linux-gnu.tar.gz' --pattern SHA256SUMS
gh release download rolling-main --repo tionis/vulcan \
  --pattern 'vulcan_*-1_amd64.deb' --pattern SHA256SUMS
```

Select the corresponding target/architecture pattern on macOS, Windows, or arm64 Linux, then verify
the downloaded entry in `SHA256SUMS` before installing it.

Do not use `self-update` for an APT, Homebrew, WinGet, or other package-managed installation. Use the
package manager so its ownership database, auxiliary files, updates, and removal remain coherent.
The complete protocol and future registry mapping are specified in
[`docs/specs/update-channels.md`](specs/update-channels.md).

## Debian package

GitHub releases also contain `vulcan_<version>-1_amd64.deb` and
`vulcan_<version>-1_arm64.deb`. Download the package for the architecture reported by
`dpkg --print-architecture` together with `SHA256SUMS`, verify the exact package entry, and install
it through APT so dependencies are checked:

```sh
grep ' vulcan_<version>-1_amd64.deb$' SHA256SUMS | sha256sum -c -
sudo apt install ./vulcan_<version>-1_amd64.deb
```

The package installs `vulcan` at `/usr/bin/vulcan` plus the man page, Bash/Fish/Zsh completions,
documentation, and licenses. It depends on Git and the standard GNU/Linux runtime libraries. It
does not register a wiki or install/start the user daemon service; review `vulcan daemon install
--dry-run` separately when background synchronization is wanted.

There is not yet a signed APT repository, so `apt update` cannot discover new Vulcan versions.
Upgrade by downloading and installing a newer release package. `sudo apt remove vulcan` removes
only packaged files and preserves user configuration, registered wikis, credentials, journals,
conflicts, and vault content.

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

Each release also contains generated Homebrew formula and WinGet portable-manifest publication
inputs from the same verified artifact manifest. They are not yet published to a tap or the central
WinGet repository. The prospective Homebrew formula supports Linux and macOS and defines an optional
service using Homebrew's stable `opt_bin` path.

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

For a portable installation, use `vulcan self-update apply` once signed channels are available, or
run the same installer with the new version. The executable is staged and replaced at its stable
path. Then preview and refresh the native service so it cannot retain an obsolete executable path:

```sh
vulcan daemon install --dry-run
vulcan daemon install
vulcan daemon status
```

To roll back, run the installer with the prior supported version and refresh the service again. To
remove Vulcan, first run `vulcan daemon uninstall`, then remove the installed binary, man page, and
completion files from the selected prefix (or uninstall through the package manager). A portable
self-update installation may also leave a hidden `.<binary>.vulcan-update.lock` and, on platforms
that lock a running executable, a reported `.<binary>.vulcan-update-backup-<id>` beside the binary;
remove those only after every Vulcan process has stopped. This preserves all user and vault state by
design.

Release archives are currently checksum-verifiable but unsigned. Production macOS notarization,
Developer-ID signing, Windows Authenticode signing, and signed provenance remain separate release
gates; contributor builds must not be represented as signed production releases.
