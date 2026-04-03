Start with `vulcan init`, then index the vault with `vulcan scan`.

Core workflows:

- `vulcan search <query>` for content-oriented lookup.
- `vulcan query '<dsl>'` for metadata and graph-aware filtering.
- `vulcan note get|create|append|patch` for precise note edits.
- `vulcan doctor` to surface unresolved links and parser diagnostics.

Automation conventions:

- Prefer `--output json` for scripts and external harnesses.
- Use `--dry-run` before bulk or destructive mutations.
- Note names may be ambiguous; pass a full relative path when precision matters.

See also: `help examples`, `help filters`, `help query-dsl`.
