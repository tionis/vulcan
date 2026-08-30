# Vulcan Companion for Obsidian

This is a thin client for Vulcan's authenticated loopback companion protocol. It displays the
selected wiki's synchronization state, requests finite sync jobs, can debounce completed Obsidian
vault writes into sync triggers, and provides a dry-run-first UI for choosing a preserved conflict
side. It never invokes Git, moves Vulcan refs, or implements its own synchronization state machine.

## Current installation

Copy the release artifacts `manifest.json`, bundled `main.js`, and `styles.css` into:

```text
<vault>/.obsidian/plugins/vulcan-companion/
```

Enable **Vulcan Companion** in Obsidian's community plugin settings. Obsidian 1.11.4 or newer is
required because the bearer credential uses the native device-local `SecretStorage` API. The
plugin works on desktop and mobile; Android still needs a reachable Vulcan daemon running in
Termux or a later native bridge. One-shot `vulcan sync run` remains available without this plugin
or the daemon.

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
stream is unavailable.

## Conflict review

Run **Vulcan Companion: Review synchronization conflicts**, select a preserved conflict, and choose
which side to preview. The first request is always `dry_run: true`. The preview modal exposes a
separate warning-styled apply button, and Vulcan reruns its ordinary stale-input, recovery, lease,
whole-tree, and worktree checks before accepting anything. The plugin never selects a side by
default and does not expose an arbitrary Git command.

## Tests

Install the pinned development dependencies, build the self-contained desktop/mobile bundle, and
run the protocol tests:

```sh
cd integrations/obsidian-vulcan
npm ci
npm run check
npm test
```
