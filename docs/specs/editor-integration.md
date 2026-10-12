# Editor integration: `vulcan lsp` and `vulcan.nvim`

**Status:** Design contract for Roadmap Phase 9.40 (not yet implemented)

This document defines how terminal editors (Neovim, Vim, Helix, Emacs, Zed, and any other Language
Server Protocol client) get live vault semantics from Vulcan. The goal is that a Vim or Neovim
user can navigate, complete, diagnose, refactor, and render an Obsidian-style vault with the same
correctness guarantees the CLI already provides, without Obsidian and without a running daemon.

It covers two deliverables:

- `vulcan lsp`: an editor-neutral, synchronous stdio language server over `vulcan-core` and
  `vulcan-app`.
- `vulcan.nvim`: a thin Neovim plugin for UI the LSP cannot express (pickers, periodic notes,
  capture, TUI launchers, rendered query blocks).

Graph visualization belongs to the Phase 13.3 WebUI, and canvas and Excalidraw editing belong to
Phase 18. Editors open those views in the browser rather than emulating them.

## Principles

1. **The vault stays canonical.** The language server never writes unsaved buffer text into
   `cache.db`. The cache is only refreshed from bytes on disk, through the normal scan path.
2. **No editor-side semantics.** Link parsing, resolution, task state, template rendering, periodic
   date math, and frontmatter editing live in `vulcan-core`/`vulcan-app`. Editor code asks the
   server or `--output json`.
3. **Daemon-independent.** `vulcan lsp` is a client-owned stdio transport like `vulcan mcp` stdio.
   It works with no daemon running and must not depend on unified hosting (10.7). A later daemon
   attachment (10.5 client routing) is an optimization, not a requirement.
4. **Open buffers belong to the editor.** The server never writes a file that is open in the client
   behind the client's back. Edits to open files are always returned as versioned
   `TextDocumentEdit`s.
5. **Safe defaults for untrusted vaults.** Opening a vault in an editor must never execute
   DataviewJS, templates, or other code without explicit device-local opt-in.

## Crate placement

| Layer | Owns |
| --- | --- |
| `vulcan-core` | `LineIndex` (byte ↔ line/column in UTF-8, UTF-16, and UTF-32); "semantic element at byte offset" lookup over `ParsedDocument` (links, embeds, tags, headings, block ids, inline fields, property keys); overlay-aware resolution that builds `ResolverDocument`s from cache identities plus overlay documents and calls `resolve_link`; a public dynamic-region API returning byte ranges (today `dynamic_content::evaluate_dynamic_regions` is crate-private). |
| `vulcan-app::editor` (new, transport-neutral, sibling of the `mcp_*` modules) | `EditorSession` (vault paths, a shared `NoteStoreSession`, overlay map); `completion` (typed candidates with kind, insert text, detail); `diagnostics` (overlay parse plus resolution into typed diagnostics with byte ranges); `navigation` (definition, references, hover payloads); `edits` (planned changes split by open/closed file); `render` (regions). |
| `vulcan-lsp` (new library crate) | `lsp-server` message loop, `lsp-types` serialization, position-encoding negotiation, request routing, worker threads, watcher lease, `WorkspaceEdit` assembly. The only crate that knows LSP. |
| `vulcan-cli` | The `vulcan lsp` subcommand (clap and dispatch only) calling `vulcan_lsp::run_stdio`, plus `describe`/`help` entries. |

Dependency rules, enforced in `vulcan-cli/tests/boundary_guard.rs`:

- `vulcan-lsp` depends on `vulcan-core` and `vulcan-app`, never on `vulcan-cli` or `vulcan-daemon`.
- `vulcan-app` and `vulcan-core` must not depend on `lsp-types` or `lsp-server`.

`vulcan-app::editor` is intended for reuse by the Phase 14.1 WebUI editor (wikilink and tag
autocomplete, diagnostics) and by MCP `completion/complete`.

Prerequisite boundary moves:

- Checkbox and task toggling (`run_note_checkbox_command`, `vulcan-cli/src/commands/note.rs`) moves
  to `vulcan-app` as a plan-only workflow: source text in, replacement text and report out. The
  existing `tasks complete` path is rebuilt on it.
- `collect_complete_candidates` (`vulcan-app/src/browse.rs`), which returns bare strings for shell
  completion, becomes a thin projection of `editor::completion`.
- `refactor::rename_heading` and `rename_block_ref` gain plan-only variants that return
  `OrdinaryWriteChange` batches, as `plan_ordinary_note_move` already does.

## Process and threading model

`vulcan lsp` is synchronous: rust-analyzer's `lsp-server` and `lsp-types` crates over crossbeam
channels, with no Tokio runtime. This keeps `vulcan-core` synchronous and matches the
unified-hosting contract that stdio surfaces are client-owned and never resident-owned.

Threads:

1. **Main loop.** Receives messages. Handles `didOpen`, `didChange`, and `didClose` inline, since
   overlay updates are cheap. Routes requests to the reader pool or the writer.
2. **Reader pool (three threads).** Each request is answered from a snapshot of the
   `EditorSession`: cloning shares open documents until their next change and shares the cached
   vault identities and resolver, which are reused while the store clock is unchanged and rebuilt
   when it advances. Readers never take the vault write lock. (Routing note queries through a
   retained `NoteStoreSession` is a later optimization; the editor requests read identities,
   backlinks, and catalogs, not hydrated note records.)
3. **Single writer thread.** The only caller of `acquire_write_lock`. Runs targeted scans and disk
   mutations serially, so a CLI `move` holding the lock never stalls completion or hover.
4. **Watcher thread** (only while holding the watch lease, see Freshness). Uses
   `watch_vault_until`, which already coalesces events and scans.

Cancellation: `$/cancelRequest` sets a per-request flag checked before a request starts and before
its answer is sent. Cancelled requests answer `RequestCanceled` (-32800). If a scan that changed
files commits while a request is in flight, the request answers `ContentModified` (-32801) rather
than mixing two store clocks. Scans that change nothing, such as the watcher's first pass and its
periodic safety rescans, do not invalidate requests.

Lock and read ordering follow `mutation-coordination.md`: canonicalize the vault root before
building `VaultPaths`; gate reads through `ordinary_write::acquire_consistent_ordinary_read`; gate
mutations through the same pending-batch check MCP uses
(`mcp_dispatch::acquire_request_ordinary_write_gate`); keep SQLite transactions innermost and
short. WAL readers keep seeing the previous complete snapshot during a scan.

## Freshness

- **Startup.** `prove_note_store_fresh` decides whether the writer runs an incremental refresh. The
  LSP is a long-lived interactive surface and follows `[scan].browse_mode` semantics (background
  scan). Progress is reported through `$/progress`.
- **`didChange`.** Overlay only. Never scans, never writes the cache.
- **`didSave`.** The writer scans the saved path (`scan_vault_paths_unlocked`) under the write lock,
  then republishes diagnostics for all open documents, because backlink-derived state may change.
- **External changes.** Configured by `watcher`:
  - `own` (default): the process tries to take an advisory OS lock on `.vulcan/editor-watch.lock`
    (the same exclusive-lock pattern and sync exclusion as `write.lock`). Exactly one LSP process
    per vault holds it. That process runs the watcher and scans. Other LSP processes on the same
    vault skip watching and poll the store clock (`note_store_clock`) about every two seconds,
    republishing diagnostics when it advances. Because it is an OS lock, a crashed holder never
    leaves a stale lease.
  - `client`: rely on `workspace/didChangeWatchedFiles` registration, then scan the reported paths.
  - `off`: never scan for external changes.
  - Planned: if a live daemon keeps the vault's index fresh, no LSP process takes the lease, and
    every process behaves like a non-holder (clock polling). This waits for a daemon
    index-ownership probe; today's daemon watchers only trigger sync.
- **Client-initiated renames** (file explorers such as oil.nvim or neo-tree): `didRenameFiles` is
  passed as a rename hint to `scan_vault_paths_unlocked_with_renames`, so document identity
  survives. The server registers for file (not folder) renames; a folder rename falls back to
  ordinary discovery.
- **Diagnostics** are published at once for an opened document, 200 ms after the last edit for a
  changed one, and for every open document after an index change.
- **Progress.** When the client supports `window.workDoneProgress`, the startup refresh is
  reported as `$/progress` under the token `vulcan/index`, once the client has acknowledged it.

## Overlay semantics

The overlay is a map from document URI to `{ version, text, line_index, parsed, vault_rel_path,
resolution_clock }`. `parse_document(source, &config)` is pure. Re-parsing on every change is fine
for note-sized files; only diagnostic publication is debounced.

| Computed from buffer text | Computed from the cache |
| --- | --- |
| Outline and document symbols; parse diagnostics (broken frontmatter, unsupported syntax, type mismatches); the document's own outgoing links and their resolution; local inline-field and property keys; task lines; dynamic-content regions | Note identities (path, filename, aliases); backlinks; tag and property catalogs; hover content for closed notes; anything else cross-document |

Merged resolution:

- Resolver documents are the cache identities, with each open buffer's identity replaced by its
  overlay (aliases from `parsed.aliases`, path from the URI).
- Unsaved new buffers are added with synthetic ids `overlay:<uri>`. A link to a note that exists
  only as an unsaved buffer resolves (no false "unresolved"), and definition jumps to that buffer.
  Closing it without saving makes the link unresolved again.
- Backlinks to an overlay-only note: the cache has none, so the server adds deltas from the other
  open buffers' overlay links. Global graph queries are not overlay-corrected.
- A buffer outside every vault (`relative_to_vault` returns `None`) gets no diagnostics and only
  generic completion.

**Position encoding.** The server picks from the client's `general.positionEncodings`, preferring
`utf-8` and falling back to the mandatory `utf-16`. `LineIndex` stores line starts plus per-line
UTF-16 lengths. CLI JSON columns (for example `LineContext.column`) are 1-based Unicode scalar
counts, which match neither encoding. LSP positions are always converted from byte offsets, never
from CLI JSON columns.

Parser offsets: `RawLink`, `RawHeading`, and `RawTag` carry a start offset only. The end is
`start + raw_text.len()`. Links originating in frontmatter (`OriginContext`) must be verified to use
document-absolute offsets before they are surfaced as ranges.

## Mutation contract

Each file in a planned change is handled by whether it is **open** in the client (dirty or clean),
never by operation type:

- **Open file.** A versioned `TextDocumentEdit` inside a `workspace/applyEdit`. The version check
  provides the stale-input guarantee that preimage checks provide on disk. Edits are minimal: one
  per changed line when the line count is unchanged (the usual case for link rewrites), otherwise
  one edit spanning the changed region.
- **Unsaved buffers are refused.** Plans are computed from files on disk, so an affected open
  buffer whose text differs from its file answers `conflict` ("save or discard unsaved changes
  first") and nothing changes. Re-planning against buffer text is a later refinement.
- **Closed file.** A journaled on-disk batch through
  `apply_ordinary_write_batch_with_renames_and_preflight`, preimage-checked, on the writer thread.

**Ordering for mixed batches: disk first, then the editor, with a compensating inverse.** Disk is
the side the server can deterministically roll back; editor buffers are user-undoable but not
server-reversible once versions move.

1. The writer applies the closed-file batch (preimage-checked, journaled).
2. On success, the server sends `workspace/applyEdit` with the open-file edits.
3. If the client answers `applied: false`, rejects the request, or does not answer within two
   minutes, the server applies the inverse batch through the same API, using the planned bytes as
   preimages. The batch is all-or-nothing: when every file still holds the planned bytes the vault
   is restored, and when any was edited in the gap nothing is touched, following the rule that
   later edits always win, and the error names the affected files. The move journal's interruption
   recovery is not used for this deliberate undo.

Failure reporting (LSP `ResponseError.data = { kind, detail, ... }`):

| Outcome | Result |
| --- | --- |
| Disk step fails (preimage mismatch or lock) | `conflict` or `internal`; `detail` names the file; nothing changed |
| Editor rejects, inverse succeeds | `conflict`, "the editor rejected the edit; the vault was restored (paths)" |
| Editor rejects, inverse hits a preimage mismatch | `conflict`, names the changed paths and points to `vulcan doctor` |
| Client cannot apply versioned edits (or rename files, for an open moved note) | `conflict`; nothing changed |
| Success of `textDocument/rename` | an empty `WorkspaceEdit`: everything was applied through `workspace/applyEdit` |
| Success of `vulcan.note.move` | `{ dryRun, action, report, disk: [paths], editor: [paths], moved }` |

**Who moves the file depends on whether it is open.** A closed note is moved by Vulcan: its delete
and create are part of the journaled batch, with a rename hint so the document keeps its
identity. A note open in the editor is moved by the editor through a `RenameFile` operation at the
end of the same `workspace/applyEdit` (after any edits to it, addressed by its old URI), because
that is the only standard way for an LSP client to re-point its buffer; the server then indexes
the rename with a hint. (An earlier draft had the server always move the file and rely on a plugin
to re-point buffers; generic clients would keep editing, and could re-save, the old path.)

**Client-initiated renames** (`workspace/willRenameFiles`, sent by file explorers before they
rename): the server plans the move and answers with the edits for open documents, which the
client applies itself. Rewrites of closed files are held until `workspace/didRenameFiles` reports
that the rename happened; the server then indexes the rename with a hint first, and publishes the
held rewrites as a preimage-checked batch, the moved note's own rewrite addressed by its new
path.

Rename entry points:

- `textDocument/prepareRename` reports the renamed span and current name.
- `textDocument/rename` on a link's note target → note move: the new name renames it within its
  folder, a name containing `/` is a vault path, and a leading `/` names the vault root.
- `textDocument/rename` on a link's `#heading` or `#^block` part renames that heading or block in
  the target note.
- `workspace/executeCommand` `vulcan.note.move` with `{ from, to, dryRun? }` (`from` is a vault path
  or file URI) moves any note, including the one being edited.
- `textDocument/rename` on a heading → plan-only `rename_heading`.
- `textDocument/rename` on a `^block` id → plan-only `rename_block_ref`.

**Task toggling.** `vulcan.task.toggle` on an open file is a single-file versioned
`TextDocumentEdit` computed on the overlay text with the shared Tasks completion semantics: status
change, done date, and insertion of the next occurrence for recurring tasks according to
`TasksConfig`. It never writes to disk while the file is open. On a closed file it uses the
journaled note write.

**Dry run.** Code actions are the LSP-native dry run: the edit is computed lazily in
`codeAction/resolve`, and the plan summary (`MoveSummary`, `RefactorReport`) is carried in `data`.
Every `vulcan.*` command also accepts `{ "dryRun": true }` and returns the same report JSON as the
CLI's `--dry-run`.

**Auto-commit.** Edits applied by the editor are ordinary saves; the LSP never commits them. Disk
batches follow `[git]` config, but the LSP defaults `autoCommit` to `false` (the `--no-commit`
analog). Users can opt in through `initializationOptions.autoCommit`.

**Permissions.** `vulcan lsp --permissions <profile>` resolves a permission profile, and the
resulting guard is passed through every workflow, including `EvaluatedMarkdownOptions.guard`.
`executeCommand` uses the same app workflows as CLI and MCP, so profile path grants are enforced
there rather than in the LSP.

**DataviewJS and code execution.** `vulcan/render` defaults to `DataviewJsRenderPolicy::Off`.
`Static` requires `initializationOptions.dataviewJs = "static"`. `On` is honored only when the
device-local `.vulcan/config.local.toml` enables it, because opening an untrusted vault in an
editor must not run `dataviewjs`. The configured `JsRuntimeSandbox` tier still applies underneath.
Template insertion follows the existing template trust rules.

## Capabilities

### MVP

- `textDocument/didOpen`, `didChange` (incremental), `didSave`, `didClose`; `positionEncoding`.
- `textDocument/definition` on wikilinks, Markdown links, embeds, and `#heading`/`^block` subpaths.
- `textDocument/references`: backlinks plus overlay deltas; on a heading, links with that subpath.
- `textDocument/completion`: after `[[` (notes and aliases, with `#`/`^` sub-completion from the
  target's outline); `#tags`; frontmatter property keys; inline-field keys. Uses `textEdit` with
  `filterText`.
- `textDocument/hover`: first lines or the targeted section of a link target, plus a properties
  summary (reusing `build_note_info_report`).
- `textDocument/publishDiagnostics` for **open buffers only**: parse diagnostics and link problems
  (`Unresolved`, `Ambiguous` with candidates, `OutsideVault`), using the same categories as
  `DoctorReport`. Debounced; incomplete `[[` tokens are skipped while typing. Workspace-wide issues
  stay in `vulcan doctor --output json`, which `vulcan.nvim` loads into the quickfix list.
- `textDocument/documentSymbol` (outline sections, with the section id in `detail`) and
  `workspace/symbol` (note paths and aliases).
- `textDocument/rename`, `workspace/willRenameFiles`, `workspace/didRenameFiles`.
- `textDocument/codeAction` with `codeAction/resolve`: create a note from an unresolved link, pick a
  disambiguation candidate, convert link style (`LinkStylePreference`), toggle a task.
- `workspace/executeCommand`: `vulcan.scan`, `vulcan.note.create`, `vulcan.note.move`,
  `vulcan.task.toggle`, `vulcan.periodic.open`, `vulcan.template.insert`.

### Later

Inlay hints and code lens (backlink counts, due dates); mdbase schema-enum value completion (after
the MDB items land); heading and block subpath diagnostics (owned by OBS.2 in `vulcan-core`; the
LSP only maps what the parser and resolver report); extract-section (reusing the note split
refactor); semantic tokens; minimal-diff `TextEdit`s; multi-root workspaces; daemon-backed store;
pull-model `workspace/diagnostic`, only if users ask for it.

### Explicitly not exposed over LSP

Queries, Bases, Kanban, and search are not mirrored. Rendering Bases tables as virtual text is a
poor fit, so `vulcan.nvim` opens the Bases TUI in a floating terminal instead. Other queries and
search go through `vulcan ... --output json`.

## Custom protocol

Custom requests live in the `vulcan/` namespace. The server advertises them in
`InitializeResult.capabilities.experimental.vulcan`:

```json
{
  "protocolVersion": 1,
  "vault": { "root": "/path/to/vault", "cacheFresh": true },
  "commands": ["vulcan.scan", "vulcan.note.create", "vulcan.note.move", "vulcan.task.toggle",
               "vulcan.periodic.open", "vulcan.template.insert"]
}
```

Clients check `protocolVersion` before using custom methods. Breaking changes bump it; additive
fields do not.

| Method | Params | Result |
| --- | --- | --- |
| `vulcan/render` | `{ textDocument, range?, dataviewJs? }` | `{ markdown, evaluated, diagnostics, regions: [{ range, markdown }] }`. Top-level fields mirror `vulcan render --output json`; `regions` is new. MVP renders Dataview and Tasks blocks only. |
| `vulcan/outline` | `{ textDocument }` | `NoteOutline`, exactly as `vulcan note outline --output json` |
| `vulcan/backlinks` | `{ textDocument }` | `BacklinksReport`, exactly as `vulcan note backlinks --output json` (keeps `context`, `matched_by`) |
| `vulcan/status` | `{}` | `{ cache: "fresh" \| "stale" \| "indexing", lastScan, backlinkCount?, daemon: "attached" \| "absent", watcher: "lease" \| "polling" \| "client" \| "off" }` |
| notification `vulcan/indexChanged` | `{ paths, summary }` (`ScanSummary`) | Sent after every scan the server observes |

Errors use LSP `ResponseError` with `data: { kind, detail }`, reusing the companion protocol kinds:
`invalid_request`, `not_found`, `permission_denied`, `conflict`, `internal`.

## Configuration

- **`initializationOptions`** (client and transport concerns only): `vault` override,
  `permissions` profile, `dataviewJs` ceiling, `autoCommit`, `watcher` (`own` | `client` | `off`),
  `diagnostics` (`onChange` | `onSave`), `completion.linkStyle`.
- **Vault semantics** stay in `.vulcan/config.toml` and the device-local `.vulcan/config.local.toml`
  (higher precedence). A new `[editor]` section holds defaults the LSP reads, such as
  `dataview_js` and `diagnose_unsupported_syntax`. Settings that enable execution follow the
  existing trust rule: untrusted shared executable configuration is ignored.
- **Vault discovery.** The MVP runs one process per vault; Neovim already starts one client per
  `root_dir`. Root markers are `.vulcan/`, `.vulcan.toml`, and `.obsidian/`. The server resolves the
  folder with `discover_vault_root`, so 9.39 repository pointers and MkDocs layouts work unchanged.
  Multiple vaults in `workspaceFolders` (one `EditorSession` per folder) come later.

## `vulcan.nvim`

Location: in-repo `integrations/vulcan.nvim/`, next to `integrations/obsidian-vulcan/`, so the
plugin and `protocolVersion` move in lockstep. A mirror repository for plugin managers can be a CI
export later.

Scope:

- LSP registration through `vim.lsp.config`/`vim.lsp.start` with the root markers above.
- `:checkhealth vulcan`: binary version, `protocolVersion`, watcher mode, cache status.
- Pickers (telescope, fzf-lua, or snacks; whichever is installed) over `vulcan ls`, `search`,
  `tags`, backlinks (`vulcan/backlinks`), and `periodic list` JSON output.
- `:Vulcan daily|weekly|template|capture` commands wrapping the periodic, template insertion, and
  capture CLI or `executeCommand`.
- Floating-terminal launchers for `vulcan browse`, `bases`, and `kanban`.
- `vulcan/render` results shown as virtual lines under Dataview and Tasks fences.
- Buffer re-pointing on `moved: { from, to }` results.
- `vulcan doctor --output json` into the quickfix list.
- Recommended companions instead of reimplementation: render-markdown.nvim or markview.nvim for live
  preview, image.nvim or snacks.image for images.

Must not contain: link parsing, path or alias resolution, frontmatter editing, task-state logic,
template rendering, periodic date math, or direct `cache.db` access.

Plain Vim gets a guide (`docs/guide/editors.md`) with `yegappan/lsp` or `vim-lsp` plus fzf.vim
recipes over `--output json`.

Open item: `tp.file.cursor()` placement needs the template insertion report to return a cursor
offset, which the template API does not report today.

## Testing

- `vulcan-lsp/tests/`: an in-process `lsp_server::Connection::memory()` harness against
  `tests/fixtures/vaults/`, with JSON snapshots in the same style as `vulcan-cli/tests/snapshots/`:
  `initialize`; diagnostics on `broken-frontmatter` and `ambiguous-links`; definition and references
  on `basic`; completion lists.
- Parity: `vulcan/outline` equals `note outline --output json`; `vulcan/backlinks` equals
  `note backlinks --output json`.
- Rename parity: the edits for a move on `move-rewrite` equal `vulcan move --dry-run` applied to the
  files; move then move back restores the original bytes. Split test: one affected file open and
  one closed produce one `TextDocumentEdit` and one journaled disk change, and cache identity is
  preserved.
- Mutation failure paths: editor rejection restores disk; an external edit in the gap is left alone
  and reported in `left_changed`; a dirty moved buffer is refused.
- Overlay: a link to an unsaved new buffer resolves; closing it without saving makes it unresolved
  again; the `cache.db` hash is unchanged across `didChange`.
- Encoding: a CJK and emoji fixture checked under both negotiated UTF-8 and UTF-16 positions.
- Concurrency: an external write triggers a lease-holder scan and republished diagnostics; a second
  LSP process on the same vault polls and republishes without watching; a CLI `move` holding the
  write lock does not stall completion.
- Security: `dataviewjs` blocks are not executed by `vulcan/render` without device-local opt-in.
- `boundary_guard.rs` dependency-direction rules.
- `vulcan.nvim`: headless `nvim` smoke tests in `integrations/vulcan.nvim/test/`.

## Agent skill impact

- New bundled skill `editor-integration` (trigger: setting up Neovim, Vim, or another LSP client
  for a vault, or explaining editor diagnostics), registered in `BUNDLED_SKILL_FILES` with
  `metadata.vulcan.managed: true`, plus installed-payload and discovery tests.
- `diagnostics-and-repair`: editor diagnostics use the `doctor` categories.
- `note-operations` and `refactoring`: when a vault is open under `vulcan lsp`, agent disk edits stay
  safe (write lock, preimage checks) and the editor reloads; prefer `move`/`refactor` so link
  rewrites remain journaled.
- `docs/assistant/AGENTS.template.md`: the LSP never writes buffer state to the cache; files on disk
  stay canonical.
- `docs/cli.md` and `describe` gain `vulcan lsp`.

## Risks and open questions

- Diagnostics noise while typing: mitigated by debouncing and skipping partial tokens.
- Full-document `TextEdit`s can move cursors in some clients; acceptable for the MVP.
- `lsp-types` and `lsp-server` must fit MSRV 1.88; pin them in workspace dependencies.
- Frontmatter-origin link offsets and missing end offsets in `Raw*` types need verification before
  ranges ship.
- Two column conventions (CLI scalar columns and LSP encoded columns) must never be mixed.
- Template cursor reporting is not yet available.
