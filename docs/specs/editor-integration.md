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
than mixing two store clocks; Vulcan's own `vulcan/*` requests answer anyway, since clients redraw
on `vulcan/indexChanged` and some (yegappan/lsp) show every error to the user. Scans that change nothing, such as the watcher's first pass and its
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

**Position encoding.** The server takes the first of the client's `general.positionEncodings` it
supports (`utf-8`, `utf-16`, or `utf-32`), since clients list them by preference and some only
handle their first choice everywhere (yegappan/lsp applies workspace edits as UTF-32), falling back
to the mandatory `utf-16`. `LineIndex` stores line starts plus per-line
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
- **Unsaved buffers are rewritten from their own text.** Plans are computed from files on disk.
  An open buffer whose text matches its file gets the planned edits; one with unsaved changes gets
  edits recomputed from its text (links to the moved note get their new target with the same link
  formatter as the CLI move; links to a renamed heading or block get the new subpath; the heading
  or block label itself is renamed in its own note), which also covers links that exist only in
  unsaved text. Its file is left to the editor's next save. Only a moved note with unsaved changes
  is refused with `conflict`, since the editor must rename its file.
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
| Success of `textDocument/rename` | an empty `WorkspaceEdit`, sent once the plan is checked and before the `workspace/applyEdit` that carries the change: some clients (Helix) wait for the rename's answer without handling requests. A failure after that (the editor rejects the edit) is a `window/showMessage` error, with the vault restored as usual |
| Success of `vulcan.note.move` | `{ dryRun, action, report, disk: [paths], editor: [paths], moved }` |

**Who moves the file depends on whether it is open.** A closed note is moved by Vulcan: its delete
and create are part of the journaled batch, with a rename hint so the document keeps its
identity. A note open in the editor is moved by the editor through a `RenameFile` operation at the
end of the same `workspace/applyEdit` (after any edits to it, addressed by its old URI), because
that is the only standard way for an LSP client to re-point its buffer; the server then indexes
the rename with a hint. The server creates missing destination folders first (Vim's `rename()`, used by
yegappan/lsp, does not), and checks on disk that the rename happened: an editor that answers
`applied` without moving the file gets the same rollback as one that rejects the edit. Editors that announce
renames to servers while applying one (Helix sends `willRenameFiles` and waits for the answer) get
an immediate empty answer for the rename the server asked for, and its `didRenameFiles` is ignored,
since the writer thread, which would otherwise handle them, is waiting for that same edit. (An earlier draft had the server always move the file and rely on a plugin
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
journaled note write. Completing sets the first completed status and a `✅` date; reopening sets
the first todo status and drops it. The next occurrence of a `🔁` task moves its reference date
(due, else scheduled, else start) to the rule's next date after it, or after the completion date
for `when done` rules, and moves the other dates by the same number of days; it drops any block id
and goes above the completed task unless `tasks.recurrence_on_completion = "next-line"`. A task
with no dates completes without a new occurrence.

**Dry run.** Every `vulcan.*` command accepts `{ "dryRun": true }` and returns its plan without
changing anything: the equivalent CLI dry-run report under `report`, plus the paths it would change
on disk and in the editor.

**Auto-commit.** Edits applied by the editor are ordinary saves; the LSP never commits them. Disk
batches follow `[git]` config, but the LSP defaults `autoCommit` to `false` (the `--no-commit`
analog). Users can opt in through `initializationOptions.autoCommit`.

**Permissions.** `vulcan lsp --permissions <profile>` (the global flag) runs the session under a
permission profile. Every planned rename, move, toggle, creation, and template insertion is checked
before it is shown or applied: each path it reads or writes must be readable and writable, because
dry-run reports quote what they read. `vulcan/render` reads only what the profile grants
(`EvaluatedMarkdownOptions.guard`), `vulcan.scan` needs the `index` grant, and periodic and
template rendering use the profile. Navigation, completion, and diagnostics treat unreadable notes
as nonexistent: they neither resolve nor complete (a link to one is reported as unresolved, like a
link to a missing note), hover and definition do not read them, references omit backlinks from
them, and tag and property completion count only readable notes. Documents the editor itself has
open stay readable to it. The startup and on-save index refresh keeps the
cache usable regardless of `index`, as the CLI's automatic refresh does.

**DataviewJS and code execution.** The strict `static` sandbox (computation and cache-backed
reads; no raw file reads, network, writes, or host execution; deterministic) cannot harm the vault
or the device, so it is the floor for every vault. Full evaluation (`on`: the vault's configured
`JsRuntimeSandbox` tier and permission profile, still never writing) follows `vulcan trust`, the
same per-user trust list that gates startup scripts and plugins. Without a client preference
(`initializationOptions.dataviewJs`, or a request's `dataviewJs`), the vault's
`dataview.render_dataview_js` applies, and without that a trusted vault renders with `on` and any
other vault with `static`. A preference or setting is honored, except that `on` falls back to
`static` for an untrusted vault (vault content and editors cannot grant trust), and `off` turns
`DataviewJS` off. The CLI, MCP `note_get`, browse previews, and evaluated exports follow the same
rule (`vulcan_core::vault_dataview_js_policy`); only an explicit CLI `--dataviewjs` flag is taken
as given, since the person at the terminal chose it for that command.
Builds without the `js_runtime` feature render the block as a notice. Template insertion renders
with template mutations disabled and follows the existing template trust rules.

## Capabilities

### MVP

- `textDocument/didOpen`, `didChange` (incremental), `didSave`, `didClose`; `positionEncoding`.
- `textDocument/definition` on wikilinks, Markdown links, embeds, and `#heading`/`^block` subpaths.
- `textDocument/references`: backlinks plus overlay deltas; on a heading, links with that subpath.
- `textDocument/completion`: after `[[` (notes and aliases, with `#`/`^` sub-completion from the
  target's outline); `#tags`; frontmatter property keys; inline-field keys inside `[` or `(`
  (inserting `key:: `; bare keys at a line start are not completed, which would fire on every
  word). Uses `textEdit` with `filterText`.
- `textDocument/hover`: first lines or the targeted section of a link target, plus a properties
  summary (reusing `build_note_info_report`).
- `textDocument/publishDiagnostics` for **open buffers only**: parse diagnostics and link problems
  (`Unresolved`, `Ambiguous` with candidates, `OutsideVault`), using the same categories as
  `DoctorReport`. Debounced; incomplete `[[` tokens are skipped while typing. Workspace-wide issues
  stay in `vulcan doctor --output json`, which `vulcan.nvim` loads into the quickfix list.
- `textDocument/documentSymbol` (outline sections, with the section id in `detail`) and
  `workspace/symbol` (note paths and aliases).
- `textDocument/rename`, `workspace/willRenameFiles`, `workspace/didRenameFiles`.
- `textDocument/codeAction`: create a note from an unresolved link, pick a disambiguation
  candidate, convert link style, toggle a task, and (on a heading) extract its section into a new
  note (`refactor.extract`). Actions are computed eagerly, so no
  `codeAction/resolve` is needed: buffer-only actions carry a versioned edit, and actions that
  create files or toggle tasks are `vulcan.*` commands run on the writer thread.
- `workspace/executeCommand`: `vulcan.scan`, `vulcan.note.create`, `vulcan.note.move`,
  `vulcan.task.toggle`, `vulcan.periodic.open`, `vulcan.template.insert`, `vulcan.capture`,
  `vulcan.section.extract`.

### Beyond the MVP

- `textDocument/codeLens`: a lens on the first line counting the notes that link here, running the
  client-side command `vulcan.showBacklinks` (vulcan.nvim maps it to references). The server asks
  clients that support it to refresh lenses and inlay hints after every index change.
- `textDocument/inlayHint`: how far off each open task's due date is (`due today`, `due in 3 days`,
  `overdue by 2 days`), from a Tasks `📅` marker or a `due::` field, using the local date.
- `[editor]` in `.vulcan/config.toml` (or the device-local `config.local.toml`, which wins):
  `diagnose_unsupported_syntax`, `diagnostic_delay_ms` (200), `code_lens`, `inlay_hints`, and
  `semantic_tokens`, all on by default; a feature that is off is not advertised.
- `textDocument/semanticTokens/full`: token types `wikiLink`, `markdownLink`, `embed`, `tag`,
  `inlineField` (the key only), and `blockId`, with modifiers `unresolved` and `ambiguous` once the
  vault is indexed. External links are left to the syntax highlighter, and a link wrapped across lines becomes one token per line.
  After an index change, clients that support it are asked to refresh. vulcan.nvim links each type
  to a treesitter group by default and dims unresolved links (`DiagnosticUnnecessary`).
- `workspace/diagnostic`: the same checks as for open documents, run on closed notes the index
  reports problems in (read from disk, skipping notes the permission profile hides). Open documents
  stay push-only; `textDocument/diagnostic` returns an empty report so clients do not show them
  twice. A note reported before and now clean or open gets an empty report. Because Neovim re-pulls
  the whole vault on `workspace/diagnostic/refresh`, the server sends that refresh only after the
  client has pulled workspace diagnostics once.
- Extract section (`vulcan.section.extract` with `{ "path", "position", "dryRun"? }`, offered as a
  `refactor.extract` code action on a heading line): the heading's section, up to the next heading
  of the same or a higher level, becomes a note named after the heading in the source note's
  folder, without its heading line, and a link to it replaces the section. Links anywhere to the
  heading become links to the new note; links to its subheadings and block ids keep their subpath
  on the new note; same-note links inside the section that point outside it gain the source note's
  name. Open buffers (including unsaved ones) get versioned edits, and the new note and closed
  files are one journaled batch. An existing note at the destination is refused. This is planned in
  the editor layer rather than over `vulcan refactor split`, which decomposes whole notes from the
  saved file and cannot see unsaved buffers.
- Minimal-diff `TextEdit`s (one per changed line).

### Later

mdbase schema-enum value completion (after the MDB items land); heading and block subpath
diagnostics (owned by OBS.2 in `vulcan-core`; the LSP only maps what the parser and resolver
report); daemon-backed store.

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
               "vulcan.periodic.open", "vulcan.template.insert", "vulcan.capture",
               "vulcan.section.extract"]
}
```

Clients check `protocolVersion` before using custom methods. Breaking changes bump it; additive
fields do not.

| Method | Params | Result |
| --- | --- | --- |
| `vulcan/render` | `{ textDocument, range?, dataviewJs? }` | `{ regions: [{ range, kind, source, markdown }], diagnostics, dataviewJs }`: every inline expression, `dataview`/`dataviewjs`/`tasks` block, and `.base` embed intersecting `range` (or the whole document), evaluated against the open buffer. `kind` is `inline` or `block`; `source` is `inline`, `dataview`, `dataviewjs`, `tasks`, `base`, or `notice` (disabled or failed regions); `markdown` is the result as `vulcan render` would splice it in; `dataviewJs` is the policy actually used. Clients choose which regions to show. |
| `vulcan/outline` | `{ textDocument }` | `NoteOutline`, exactly as `vulcan note outline --output json` |
| `vulcan/backlinks` | `{ textDocument }` | `BacklinksReport`, exactly as `vulcan note backlinks --output json` (keeps `context`, `matched_by`) |
| `vulcan/status` | `{}` | `{ cache: "fresh" \| "stale" \| "indexing", lastScan, backlinkCount?, daemon: "attached" \| "absent", watcher: "lease" \| "polling" \| "client" \| "off" }` |
| notification `vulcan/indexChanged` | `{ paths, summary, renames? }` (`ScanSummary`; `renames` lists `{ from, to }` vault paths of moves the server planned) | Sent after every scan the server observes |

Errors use LSP `ResponseError` with `data: { kind, detail }`, reusing the companion protocol kinds:
`invalid_request`, `not_found`, `permission_denied`, `conflict`, `internal`.

## Configuration

- **`initializationOptions`** (client and transport concerns only): `vault` override,
  `permissions` profile, `dataviewJs` preference, `autoCommit`, `watcher` (`own` | `client` | `off`),
  `diagnostics` (`onChange` | `onSave`), `completion.linkStyle`, and `showDocument` (treat the
  client as handling `window/showDocument` although it does not advertise it, as Helix does).
- **Command arguments for editors without a plugin layer.** A command's document argument (`path`,
  `from`) may be a file URI, a vault path, or a filesystem path (absolute, `~/`, or relative to the
  vault root), and a position may be given as 1-based `lineNumber` and `columnNumber` (characters)
  instead of an LSP `position` (`vulcan.task.toggle` also takes `lineNumber` for `line`). Helix
  keybindings fill them from `%{buffer_name}`, `%{cursor_line}`, and `%{cursor_column}`.
- **Vault semantics** stay in `.vulcan/config.toml` and the device-local `.vulcan/config.local.toml`
  (higher precedence). Their `[editor]` section holds editor defaults (see "Beyond the MVP");
  `DataviewJS` rendering follows `vulcan trust` instead of configuration.
- **Vault discovery.** The MVP runs one process per vault; Neovim already starts one client per
  `root_dir`. Root markers are `.vulcan/`, `.vulcan.toml`, and `.obsidian/`. The server resolves the
  folder with `discover_vault_root`, so 9.39 repository pointers and MkDocs layouts work unchanged.
  When the initial `workspaceFolders` resolve to several vaults, the process multiplexes: each
  vault runs an unchanged single-vault server (its own session, writer, and watcher lease) on an
  in-memory connection. Document messages route to the vault holding the URI (the longest root
  containing it; files outside every vault go to the first). `workspace/symbol`,
  `workspace/diagnostic` (with `previousResultIds` split per vault), `workspace/willRenameFiles`,
  `vulcan.scan`, and `shutdown` fan out and merge; other commands go to the vault of their
  `path`/`from`/`uri` argument, else the first vault, and requests to the vault of their
  `textDocument`. vulcan.nvim passes the current buffer as `uri` to `vulcan.periodic.open` and
  `vulcan.capture`, and as `textDocument` to `vulcan/status`, so those act on the buffer's vault. Requests the vault servers send the client get
  fresh ids, and their progress tokens and registration ids get a `#<n>` suffix so vaults never
  collide. Only then does the server advertise `workspace.workspaceFolders.changeNotifications`, and
  `didChangeWorkspaceFolders` starts and stops vault servers (the last one stays). A single vault is
  served directly, as before.

## `vulcan.nvim`

Location: in-repo `integrations/vulcan.nvim/`, next to `integrations/obsidian-vulcan/`, so the
plugin and `protocolVersion` move in lockstep.

Distribution: the binary carries the plugin (`vulcan_app::nvim_plugin`, every file except
`test/`, checked against the directory by a test) and installs it with `vulcan editor nvim
install`. The default target is Neovim's package directory, `stdpath('data')` (honoring
`NVIM_APPNAME`, `XDG_DATA_HOME`, and `LOCALAPPDATA` on Windows) plus
`site/pack/vulcan/start/vulcan.nvim`, which Neovim loads without a plugin manager; `--dir` installs
anywhere, for example where a lazy.nvim `dir =` spec points (lazy resets `packpath`, so it needs the
`dir` spec). The install generates `doc/tags`, since Neovim's package loading does not run
`:helptags`. A `.vulcan-managed.json` marker records the version and each file's BLAKE3 hash, so
`status` reports `missing`, `current`, `outdated` (another version or build), `modified` (edited or
deleted files), or `unmanaged` (a checkout or a plugin manager's copy). `install` refreshes
`missing`, `current`, and `outdated` copies and removes files the bundle dropped; `modified` and
`unmanaged` copies, and `uninstall` of them, need `--force`. `self-update` runs the new binary's
installer for an unedited managed copy in the default directory, and `:checkhealth vulcan` compares
the loaded copy with the binary. A mirror repository for git-based plugin managers is future work.

Scope:

- LSP registration through `vim.lsp.config`/`vim.lsp.start` with the root markers above.
- `:checkhealth vulcan`: binary version, `protocolVersion`, watcher mode, cache status.
- Pickers over `vim.ui.select`, so whichever `ui.select` provider is configured (telescope,
  fzf-lua, snacks) presents them, over `vulcan ls`, `search`, `tags`, and `template list` JSON
  output. Backlinks use the standard `textDocument/references`.
- `:Vulcan` subcommands wrapping server commands: `daily`/`weekly`/`monthly`/`periodic`,
  `template`, `task`, `move`, `scan`, plus `render`, `doctor`, and `status`. QuickAdd-style capture
  is not wrapped yet.
- Floating-terminal launchers for `vulcan browse`, `bases tui`, and `kanban show`.
- `vulcan/render` results drawn as virtual text: block results as virtual lines below the block,
  inline values at the end of the line, refreshed on `BufEnter`, `BufWritePost`, `InsertLeave`, and
  `vulcan/indexChanged`.
- Buffer re-pointing after moves needs no plugin code: a moved open note is renamed by the editor
  (`RenameFile`), which Neovim applies to the buffer.
- `vulcan doctor --output json` into the quickfix list, at each problem's line (`line`, from the
  link's or diagnostic's byte offset).
- Interop notes found in testing: Neovim sends an empty `initializationOptions` table as `[]`, so
  the server treats any non-object as absent; and Neovim lists `resourceOperations` without
  `documentChanges`, so the server treats listed resource operations as `documentChanges`
  support.
- Recommended companions instead of reimplementation: render-markdown.nvim or markview.nvim for live
  preview, image.nvim or snacks.image for images.

Must not contain: link parsing, path or alias resolution, frontmatter editing, task-state logic,
template rendering, periodic date math, or direct `cache.db` access.

Plain Vim, Helix, and Emacs get a guide (`docs/guide/editors.md`): `yegappan/lsp` or `vim-lsp` plus
fzf.vim recipes, a Helix `languages.toml` entry, and an Eglot entry.

The plugin's headless smoke test (`integrations/vulcan.nvim/test/smoke.lua`) drives a real
Neovim against the built binary: attaching, rendering, the CLI wrapper, doctor, task toggling with
recurrence, link rename, moving the open note, periodic notes, and `:checkhealth`.
`cargo test -p vulcan-cli --test nvim_plugin` runs it when Neovim 0.10+ is installed (set
`VULCAN_REQUIRE_NVIM` to fail instead of skipping). Linux CI installs a checksum-pinned Neovim
0.10.4, the oldest supported release, and sets `VULCAN_REQUIRE_NVIM`.

`tp.file.cursor()` places the cursor: editor template insertion sets a reserved template variable
so the call emits a marker, removes it from the inserted text, and reports its position as
`report.cursor`; the plugin moves the cursor there. Outside editors the call stays a no-op.

## `vulcan.vim`

Vim 9 has no built-in LSP client, so `integrations/vulcan.vim/` is a Vim9script plugin over
[yegappan/lsp](https://github.com/yegappan/lsp) (Vim 9.0+), the most complete Vim client, with the
same shape and rules as `vulcan.nvim`: vault semantics stay in the server, and the plugin only
registers it, maps commands, and draws results. `vim-lsp` and other clients keep working through the
recipes in `docs/guide/editors.md`, without the plugin layer.

What yegappan/lsp provides (checked against its source, commit `58eac06`, 2026-09-05):

- `workspace.applyEdit` with `documentChanges` and `resourceOperations` (`rename`, `create`,
  `delete`), so versioned edits, renames, and moves are offered as for Neovim.
- Code lenses, inlay hints, code actions running `workspace/executeCommand` for unregistered
  commands, rename (without `prepareRename`), work-done progress, and UTF-8/16/32 positions.
- Pull diagnostics stored apart from pushed ones and merged per buffer, so the empty
  `textDocument/diagnostic` answer for open buffers does not hide pushed diagnostics.
- Hooks for the plugin layer: `lsp.Server().rpc_a(method, params, callback)` and
  `g:LspRequestCustom()` for `vulcan/*` requests, `customNotificationHandlers` (for
  `vulcan/indexChanged`), and `g:LspRegisterCmdHandler()` for client-side commands such as the
  backlink lens's `vulcan.showBacklinks`.
- No `window/showDocument`, `workspace/didChangeWatchedFiles`, or file-operation notifications; the
  server already works without them (it watches the vault itself and indexes editor-performed
  renames from its own rename hints).

Gaps the plugin or server closes:

- **Buffer re-pointing after moves.** yegappan applies a `RenameFile` by renaming the file on disk
  only; the buffer keeps the old name, so saving it would recreate the old file. The server adds
  `renames: [{ from, to }]` (vault paths) to `vulcan/indexChanged` for moves it planned, and the
  plugin re-points a buffer still named `from`: it writes the buffer to the new path (a moved open
  note never has unsaved changes, so it holds only the server's link rewrites), edits it there, and
  wipes the old buffer, so yegappan/lsp closes the old document and opens the new one (renaming the
  buffer with `:file` would leave the server believing the note closed). This is
  additive to protocol v1.
- **Change reporting.** yegappan reports buffer edits from a `listener_add()` callback, which runs
  on the next redraw; the plugin calls `listener_flush()` before its own requests so the server
  answers against the current text (the headless smoke test flushes in its wait loop, since a
  headless Vim never redraws).
- **Semantic tokens.** yegappan maps only standard token types to highlight groups, so Vulcan's
  custom types are not drawn. A follow-up can let a client ask for a legend of standard types
  through `initializationOptions`; until then links still get pushed diagnostics.

Scope, mirroring `vulcan.nvim`: `:Vulcan` subcommands (`notes`, `search`, `tags`, `backlinks`,
periodic notes, `template`, `task`, `capture`, `move`, `extract`, `scan`, `render`, `doctor`,
`problems` (the same as `doctor`, since yegappan/lsp has no workspace diagnostics), `status`, and
TUI launchers in a `:terminal` popup);
pickers through `popup_menu()`, or fzf.vim when installed; `vulcan/render` results drawn with text
properties (`text_align: 'after'` for inline values, `'below'` for block results, Vim 9.0.0067+);
`:Vulcan health` in place of `:checkhealth`; Vim help in `doc/vulcan.txt`.

Distribution: the installer generalizes from one Neovim bundle to a table of editor bundles that
share the marker, hash, `--force`, `self-update`, and health-check logic. `vulcan editor vim
install` writes `~/.vim/pack/vulcan/start/vulcan.vim` (or `$XDG_CONFIG_HOME/vim/...` when Vim uses
that directory, and `vimfiles` on Windows). yegappan/lsp itself stays the user's install; the plugin
reports its absence.

Testing: a headless smoke test (`vim -Nu NONE -es`) against the built binary with a pinned
yegappan/lsp commit, run by a `cargo test` wrapper that skips without Vim 9 and yegappan/lsp unless
`VULCAN_REQUIRE_VIM` is set; CI installs both.

## Other editors

Each editor gets the least layer its extension model allows; none of them adds vault semantics.

| Editor | Layer | Distribution |
| --- | --- | --- |
| Helix | Language server plus configuration (no plugin system yet): `languages.toml` and `space v` keybindings running `vulcan.*` commands; query results are not drawn. See "Helix" below. | `vulcan editor helix print-config` (`integrations/helix/`) |
| Emacs | Eglot configuration plus a small Elisp package for `:Vulcan`-style commands and drawing `vulcan/render` results as overlays. | File-based, so the same installer (`vulcan editor emacs install`) |
| Zed | An extension registering `vulcan lsp` for Markdown. | Zed's extension registry (needs publishing) |
| VS Code | An extension for commands and drawing `vulcan/render` results; the rest comes from the server. | Marketplace or Open VSX (needs publishing) |

File-based plugins join the installer's bundle table; registry-published extensions are separate
tracks, since publishing needs accounts and their copies can drift from the binary (the
`protocolVersion` check catches mismatches).

### Helix

Checked against Helix 25.07.1, its source and a real session (the end-to-end test drives it through
tmux). Its client advertises versioned `documentChanges` with create, rename, and delete,
`willRenameFiles`/`didRenameFiles`, dynamic watched-file registration, inlay hints, workspace
folders, and UTF-8 positions first; no code lenses, semantic tokens, pull diagnostics, or
`window/showDocument` (which it handles anyway). Findings and what closes them:

- **Renames block.** Helix waits for `textDocument/rename` with `block_on`, so the server answers it
  before sending the `workspace/applyEdit` that carries the change (see "Mutation contract").
- **It announces renames back.** Applying a `RenameFile`, Helix's `move_path` sends
  `willRenameFiles` and waits up to five seconds; the server answers the renames it asked for at
  once and ignores their `didRenameFiles`. Helix re-points the buffer itself.
- **`window/showDocument` is handled but not advertised**, so `languages.toml` sets the
  `showDocument` initialization option and new periodic notes open.
- **Commands from keybindings.** `:lsp-workspace-command <command> <json>` expands `%{buffer_name}`,
  `%{cursor_line}`, and `%{cursor_column}` inside the JSON, so bindings pass the note and cursor;
  the server accepts filesystem paths and 1-based `lineNumber`/`columnNumber` for that.
  `%{buffer_name}` is relative to Helix's working directory, so Helix should start in the vault.
- **Selections cannot go into JSON** (Helix has no escaping expansion; a line selection ends in a
  newline), so capture uses `:pipe-to vulcan inbox -`, which sends the selection on stdin rather
  than splicing it into a shell command.
- **Starting only in vaults.** `required-root-patterns = [".vulcan", ".vulcan.toml", ".obsidian"]`
  matches entry names in the root, and the Markdown language's `roots` make the vault the root.
  Helix searches for roots only up to its workspace (the Git repository or its working directory).
  When a server does not accept workspace-folder changes, Helix starts another instance for another
  root, so each vault gets its own server.

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
