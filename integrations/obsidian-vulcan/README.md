# Vulcan Companion for Obsidian

This is a thin client for Vulcan's authenticated loopback companion protocol. It displays the
selected wiki's synchronization state, requests finite sync jobs, can debounce completed Obsidian
vault writes into sync triggers, and provides a dry-run-first UI for choosing a preserved conflict
side. It never invokes Git, moves Vulcan refs, or implements its own synchronization state machine.

## Installation

Install or update the companion directly from the Vulcan binary. The wiki must already be registered
and must contain a real `.obsidian/` directory:

```sh
vulcan daemon companion install personal --dry-run
vulcan daemon companion install personal
```

Omit `personal` to infer the registration from global `--vault`. The command embeds and writes
`manifest.json`, bundled `main.js`, and `styles.css` into:

```text
<vault>/.obsidian/plugins/vulcan-companion/
```

On first install it also seeds `data.json` with only the non-secret configured daemon endpoint and
registered wiki ID. Updates preserve existing plugin settings and unrelated files. Enable **Vulcan
Companion** in Obsidian's community plugin settings. Obsidian 1.11.4 or newer is required because
the bearer credential uses the native device-local `SecretStorage` API. The plugin works on desktop
and mobile; Android still needs a reachable Vulcan daemon running in Termux or a later native
bridge. One-shot `vulcan sync run` remains available without this plugin or the daemon.

The companion follows the registered directory profile. For `files-only`, it supports sync status,
sync requests, and review of preserved file conflicts. The profile disables indexing, scripts,
semantic history, and agent conflict resolution. Full-tree materialization is the current sync
contract; selecting files-only does not enable sparse checkout or partial clone. Unattended
files-only sync applies conservative Git repository checks, but Vulcan cannot lock out external
Git processes, so branch switches and Git operations must stay clear while a job runs.

## Pairing with the local daemon

Start Vulcan, inspect the endpoint, and explicitly reveal the device credential:

```sh
vulcan daemon start --detach
vulcan daemon companion --output json
vulcan daemon companion --reveal-token --output json
```

In the plugin settings, copy `base_url`, the registered wiki ID, and the revealed token. The token
is written only to Obsidian `SecretStorage`; `data.json` contains non-secret endpoint, wiki, and
trigger preferences. Do not place the revealed JSON or token in a note, synchronized plugin
settings, logs, shell history, or source control. Obsidian plugins share the trust boundary of the
Obsidian application, so install only trusted plugins on a device that holds this bearer token.

The daemon must remain loopback-only and its registered permission profile remains authoritative.
The WebSocket sends deduplicated snapshots; a 30-second HTTP refresh remains as recovery when the
stream is unavailable. By default, the companion shows one Obsidian notice for each failed daemon
job or retained failed transaction. Notices include a bounded error summary, while the status dialog
shows the failed job ID, category, retryability, and full retained error. **Network failure notices**
can instead be disabled, delayed until a chosen number of distinct failed jobs, or delayed until a
network failure remains unresolved for a chosen number of minutes. Count and duration policies show
one notice per continuous network outage and reset after recovery. Duration starts when the companion
first observes the failure; a restart resets its timer. Other failures still notify immediately.
Temporary companion connection loss changes the status bar to offline and is not treated as a sync
failure. Disable **Notify on failed synchronization** to suppress every failure notice.

## Conflict review

Run **Vulcan Companion: Review synchronization conflicts**, select a preserved conflict, and choose
which side to preview. The first request is always `dry_run: true`. The preview modal exposes a
separate warning-styled apply button, and Vulcan reruns its ordinary stale-input, recovery, lease,
whole-tree, and worktree checks before accepting anything. The plugin never selects a side by
default and does not expose an arbitrary Git command.

## Tests

Install the pinned development dependencies, build the self-contained desktop/mobile bundle (which
also refreshes the assets embedded by `vulcan-app`), and run the protocol tests:

```sh
cd integrations/obsidian-vulcan
npm ci
npm run check
npm test
```
