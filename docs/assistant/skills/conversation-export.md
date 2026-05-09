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
        cli:
          aliases: [conversation-export]
          args:
            - flag: title
              action: string
              field: title
              description: Conversation title.
            - flag: source
              action: choice
              field: source
              choices: [chatgpt, codex, claude, gemini, pi, other]
              description: Conversation source label.
            - flag: date
              action: string
              field: date
              completion: daily-date
              description: Conversation date.
            - flag: target-folder
              action: string
              field: target_folder
              completion: vault-path
              description: Vault folder for exported conversation notes.
            - flag: dry-run
              action: boolean
              field: dry_run
              description: Preview normalized Markdown without writing a note.
            - flag: transcript
              action: string
              field: transcript
              description: Raw transcript text.
            - flag: transcript-file
              action: string_file
              field: transcript
              description: Read raw transcript text from a file or stdin.
            - flag: messages-file
              action: json_file
              field: messages
              description: Read structured message JSON from a file or stdin.
            - flag: user
              action: append_message
              role: user
              description: Append a user turn to messages.
            - flag: assistant
              action: append_message
              role: assistant
              description: Append an assistant turn to messages.
            - flag: system
              action: append_message
              role: system
              description: Append a system turn to messages.
        input_schema:
          type: object
          required: [title]
          anyOf:
            - required: [transcript]
            - required: [messages]
            - required: [turns]
          additionalProperties: false
          properties:
            title:
              type: string
              minLength: 1
            transcript:
              type: string
              minLength: 1
            messages:
              type: array
              minItems: 1
              items:
                type: object
                required: [role]
                anyOf:
                  - required: [content]
                  - required: [thinking]
                  - required: [reasoning]
                  - required: [tool_uses]
                  - required: [tool_results]
                additionalProperties: false
                properties:
                  role:
                    type: string
                    enum: [user, human, assistant, system, tool]
                  content:
                    anyOf:
                      - type: string
                      - type: array
                        items:
                          anyOf:
                            - type: string
                            - type: object
                              required: [type]
                              anyOf:
                                - required: [text]
                                - required: [content]
                                - required: [value]
                                - required: [input]
                                - required: [output]
                                - required: [result]
                              additionalProperties: false
                              properties:
                                type:
                                  type: string
                                  enum: [text, thinking, reasoning, tool_use, tool-call, tool_call, tool_result, tool-output, tool_output]
                                text:
                                  type: string
                                content: {}
                                value: {}
                                name:
                                  type: string
                                tool:
                                  type: string
                                function:
                                  type: string
                                function_name:
                                  type: string
                                id:
                                  type: string
                                call_id:
                                  type: string
                                tool_call_id:
                                  type: string
                                input: {}
                                arguments: {}
                                args: {}
                                params: {}
                                output: {}
                                result: {}
                                error: {}
                      - type: object
                  text:
                    type: string
                  message:
                    type: string
                  output: {}
                  thinking:
                    type: string
                  reasoning:
                    type: string
                  thoughts:
                    type: string
                  tool_uses:
                    type: array
                    items:
                      type: object
                      required: [name]
                      additionalProperties: false
                      properties:
                        name:
                          type: string
                        tool:
                          type: string
                        function:
                          type: string
                        function_name:
                          type: string
                        id:
                          type: string
                        call_id:
                          type: string
                        tool_call_id:
                          type: string
                        input: {}
                        arguments: {}
                        args: {}
                        params: {}
                        output: {}
                        result: {}
                        error: {}
                  toolUses:
                    type: array
                  tools:
                    type: array
                  tool_results:
                    type: array
                    items:
                      type: object
                      required: [name]
                      anyOf:
                        - required: [output]
                        - required: [result]
                        - required: [content]
                        - required: [error]
                      additionalProperties: false
                      properties:
                        name:
                          type: string
                        tool:
                          type: string
                        function:
                          type: string
                        function_name:
                          type: string
                        id:
                          type: string
                        call_id:
                          type: string
                        tool_call_id:
                          type: string
                        output: {}
                        result: {}
                        content: {}
                        error: {}
                  toolResults:
                    type: array
            turns:
              type: array
              minItems: 1
              items:
                type: object
                required: [role]
                anyOf:
                  - required: [content]
                  - required: [thinking]
                  - required: [reasoning]
                  - required: [tool_uses]
                  - required: [tool_results]
                additionalProperties: false
                properties:
                  role:
                    type: string
                    enum: [user, human, assistant, system, tool]
                  content:
                    anyOf:
                      - type: string
                      - type: array
                      - type: object
                  text:
                    type: string
                  message:
                    type: string
                  output: {}
                  thinking:
                    type: string
                  reasoning:
                    type: string
                  thoughts:
                    type: string
                  tool_uses:
                    type: array
                    items:
                      type: object
                      required: [name]
                      additionalProperties: true
                      properties:
                        name:
                          type: string
                  toolUses:
                    type: array
                  tools:
                    type: array
                  tool_results:
                    type: array
                    items:
                      type: object
                      required: [name]
                      additionalProperties: true
                      properties:
                        name:
                          type: string
                  toolResults:
                    type: array
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
        examples:
          - name: dry-run-cli
            description: Preview a short two-turn ChatGPT transcript without writing a note.
            cli_args:
              - --title
              - Example Chat
              - --source
              - chatgpt
              - --date
              - "2026-05-09"
              - --dry-run
              - --user
              - Hello
              - --assistant
              - Hi there.
---

# Conversation Export

Use this skill when a user wants to save a chat transcript into the vault as an Obsidian-readable Markdown note.

The `export` command accepts pasted plain text, JSON arrays, JSONL-style message logs, or structured `messages`/`turns` arrays. Structured turns may include `role`, `content`, `thinking`/`reasoning`, `tool_uses`, `tool_results`, or typed `content` parts such as `text`, `thinking`, `tool_use`, and `tool_result`. It writes a note under `AI/Conversations/` by default using `[!user]`, `[!assistant]`, `[!system]`, `[!tool]`, and nested `[!thinking]` callouts with frontmatter describing the source and message count.

Prefer this skill over a bespoke note edit when the task is primarily conversation archival. Use `dry_run: true` when the user wants to preview the normalized Markdown before writing it.
