# Vulcan for Helix

[Helix](https://helix-editor.com) has no plugin system, so Vulcan's Helix
integration is configuration: the `vulcan lsp` language server for Markdown
notes in a vault, and keybindings that run the server's commands. Vault
semantics stay in Vulcan. Tested with Helix 25.07.

## Install

Print both files and merge them into `~/.config/helix/` (`%AppData%\helix`
on Windows):

```sh
vulcan editor helix print-config                    # both, each under a heading comment
vulcan editor helix print-config --only languages   # just languages.toml
vulcan editor helix print-config --only keys        # just config.toml
```

- [`languages.toml`](languages.toml) registers the server, starts it only in
  vaults (`required-root-patterns`), makes a note's vault its workspace root,
  and lists it for Markdown. To keep another Markdown server, list it too:
  `language-servers = ["vulcan", "marksman"]`.
- [`config.toml`](config.toml) adds keybindings under `space v`.

If you already have a `[[language]]` entry for Markdown, merge the keys into
it rather than adding a second one.

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
