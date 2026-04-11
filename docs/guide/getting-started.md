Start with `vulcan index init`, then index the vault with `vulcan index scan`.

Core workflows:

- `vulcan search <query>` for content-oriented lookup.
- `vulcan query '<dsl>'` for metadata and graph-aware filtering.
- `vulcan note get|create|append|patch` for precise note edits.
- `vulcan export ...` for publication output; `markdown`, `json`, `epub`, and `zip` can apply publication transforms without modifying source notes.
- `vulcan doctor` to surface unresolved links and parser diagnostics.

Automation conventions:

- Prefer `--output json` for scripts and external harnesses.
- Use `--dry-run` before bulk or destructive mutations.
- Note names may be ambiguous; pass a full relative path when precision matters.
- When you need repeatable public exports, prefer `export profile create` for the profile-wide settings and `export profile rule ...` for the ordered transform rules stored in `.vulcan/config.toml`.

See also: `help examples`, `help filters`, `help query-dsl`, `vulcan export --help`.
