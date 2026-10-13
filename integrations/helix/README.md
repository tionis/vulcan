# Vulcan for Helix

[Helix](https://helix-editor.com) has no plugin system, so Vulcan's Helix
integration is configuration: the `vulcan lsp` language server for Markdown
notes in a vault, and keybindings that run the server's commands. Vault
semantics stay in Vulcan. Tested with Helix 25.07.

## Install

```sh
vulcan editor helix install --dry-run   # show the changes as a diff
vulcan editor helix install             # merge them into ~/.config/helix/
```

This merges two sets of entries into Helix's own files (`~/.config/helix/`,
or `%AppData%\helix` on Windows), editing them in place so your comments,
layout, and settings stay:

- into [`languages.toml`](languages.toml): the `vulcan` language server,
  started only in vaults (`required-root-patterns`); and Vulcan added first to
  the Markdown entry's servers, with the vault markers added to its roots. An
  existing Markdown entry is extended, and servers already listed (or Helix's
  defaults, `marksman` and `markdown-oxide`) stay.
- into [`config.toml`](config.toml): keybindings under `space v`.

Keys you bound yourself are kept, and so are Vulcan's entries you changed or
removed later; `--force` takes them over. The first change to each file keeps
the previous version as `<file>.vulcan-backup`, and `.vulcan-helix.json`
records what Vulcan wrote, so running `install` again (which `vulcan
self-update` does) refreshes only that, and `vulcan editor helix uninstall`
removes only that. `vulcan editor helix status` shows each entry's state.
Run `:config-reload` in Helix afterwards.

To merge by hand instead, `vulcan editor helix print-config` prints both files
(`--only languages` or `--only keys` for one).

## What you get

From the language server, through Helix's own keys:

- go to definition on links, embeds, `#heading` and `#^block` subpaths (`gd`),
  references (`gr`), hover previews (`space k`)
- completion after `[[`, `#`, `^`, and in frontmatter
- diagnostics for unresolved and ambiguous links (`space d`)
- link-safe rename of notes, headings, and block ids (`space r`); renaming a
  link to a note moves the note, and an open note's buffer follows
- code actions (`space a`): create a missing note, pick an ambiguous link's
  target, convert link style, complete or reopen a task, extract a heading's
  section into a new note
- due-date inlay hints on open tasks (with `display-inlay-hints = true` under
  `[editor.lsp]` in `config.toml`; Helix hides them by default)
- document and workspace symbols (`space s`, `space S`)

From the keybindings:

| Keys | Does |
| --- | --- |
| `space v d`, `w`, `m` | Open today's daily, weekly, or monthly note, creating it from its template |
| `space v x` | Complete or reopen the task on the cursor line |
| `space v e` | Extract the section of the heading on the cursor line into a new note |
| `space v c` | Append the selection to the inbox note |
| `space v s` | Rescan the vault |

Any other server command runs from `:lsp-workspace-command`, with JSON
arguments, for example:

```text
:lsp-workspace-command vulcan.note.move {"from": "%{buffer_name}", "to": "archive/Note.md"}
:lsp-workspace-command vulcan.periodic.open {"period": "daily", "date": "2026-10-13"}
```

## Limitations

- Start Helix in the vault (or in a Git repository holding it): Helix looks
  for root markers only up to its workspace, and `%{buffer_name}` is relative
  to the directory Helix was started in.
- Query results (Dataview, Tasks, Bases) are not drawn: Helix has no way for
  a server to add virtual text. Use `vulcan note get --evaluate` or the TUIs.
- No code lenses, semantic highlighting, or workspace-wide diagnostics, which
  Helix does not support; `vulcan doctor` lists problems in every note.
- The capture binding sends the selection to `vulcan inbox -` (stdin), so it
  uses the CLI's vault discovery from Helix's working directory.
