---
name: conversation-export
title: Conversation Export
description: Normalize pasted chat transcripts and save them as vault Markdown callout notes.
license: UNLICENSED
compatibility:
  - vulcan
allowed-tools:
  - note_create
metadata:
  vulcan:
    commands:
      - id: export
        script: scripts/export-conversation.js
        sandbox: fs
        packs: [custom]
        expose: true
        input_schema:
          type: object
          properties:
            title:
              type: string
            transcript:
              type: string
            source:
              type: string
            date:
              type: string
            target_folder:
              type: string
            dry_run:
              type: boolean
        output_schema:
          type: object
          required: [path, title, source, message_count, roles]
          properties:
            path:
              type: string
            title:
              type: string
            source:
              type: string
            message_count:
              type: integer
            roles:
              type: array
              items:
                type: string
            markdown:
              type: string
---

# Conversation Export

Use this skill when a user wants to save a chat transcript into the vault as an Obsidian-readable Markdown note.

The `export` command accepts pasted plain text, JSON arrays, or JSONL-style message logs. It writes a note under `AI/Conversations/` by default using `[!user]`, `[!assistant]`, `[!system]`, and `[!tool]` callouts with frontmatter describing the source and message count.

Prefer this skill over a bespoke note edit when the task is primarily conversation archival. Use `dry_run: true` when the user wants to preview the normalized Markdown before writing it.
