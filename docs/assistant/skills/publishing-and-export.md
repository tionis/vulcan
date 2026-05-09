---
name: publishing-and-export
description: Build static sites, export vault content, render Markdown, and package selected notes. Use when the user asks about site builds, export profiles, EPUB/ZIP/SQLite/JSON/CSV output, render diagnostics, publish filters, or route/link policy.
version: 1
tools:
  - site
  - export
  - render
  - config_show
  - help
require_confirmation: false
---

# Publishing and Export

## When to Use This Skill

Use this skill when the user wants vault content rendered, packaged, published, or diagnosed for a
static output target.

## Recommended Flow

- Use `vulcan render` for one Markdown file or stdin.
- Use `vulcan export ...` for one-off artifacts such as Markdown, JSON, CSV, graph, EPUB, ZIP, SQLite, search index, or frontend bundle outputs.
- Use export profiles for repeatable export settings.
- Use `vulcan site build --profile <name>` for static sites and `vulcan site doctor` for publish diagnostics.
- Inspect link policy, route collisions, asset policy, publish filters, and hidden-content transforms before changing output.

## Guardrails

- Exports and sites should be reproducible from vault source plus config.
- Do not silently publish private or hidden sections; check include/exclude filters and content transforms.
- Broken links should be handled by configured link policy, not by ad hoc deletion.
- For site work, prefer profile edits over command lines that cannot be repeated.

## Example Moves

- Build a public static site profile and diagnose unpublished links.
- Export a selected set of notes to EPUB while excluding private callouts.
- Render one note to HTML to inspect markdown/parser behavior.
