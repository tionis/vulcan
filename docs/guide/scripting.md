Vulcan has two scripting-adjacent surfaces today:

- DataviewJS evaluation for indexed `dataviewjs` blocks.
- Templater-compatible JavaScript execution for template rendering.

These run inside a restricted QuickJS sandbox. They are suitable for note-local automation and computed views, not arbitrary shell access.

External harnesses can also treat the CLI itself as a tool surface:

- `vulcan describe --format openai-tools`
- `vulcan describe --format mcp`
- `vulcan help --output json <topic>`

The general-purpose `vulcan run` scripting surface from Phase 9.18.5 is the next layer on top of the same runtime foundations.

See also: `help sandbox`, `help js`, `help describe`.
