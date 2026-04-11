Recipes:

- Find active project notes:
  `vulcan ls --where 'status = active and tags contains "#project"'`
- Show only matching paths:
  `vulcan query --format paths 'from notes where owner = "eric"'`
- Patch one exact string safely:
  `vulcan note patch Daily/2026-04-03.md 'Old text' 'New text' --check`
- Preview a rename before writing:
  `vulcan refactor rename-property status phase --dry-run`
- Search the web, then save notes yourself:
  `vulcan web search "sqlite wal tuning" --output json`
- Make a public JSON export that strips internal callouts:
  `vulcan export json 'from notes where file.path starts_with "Projects/"' --exclude-callout internal -o exports/public.json`
- Redact email addresses during export without changing the vault:
  `vulcan export json 'from notes where file.path starts_with "People/"' --replace-rule regex '[A-Za-z0-9._%+-]+@example\.com' redacted -o exports/people.json`

See also: `help getting-started`, `help filters`, `help refactor`, `vulcan export --help`.
