# config

Inspect and import effective Vulcan configuration

Notes:
  `config show` merges built-in defaults with `.vulcan/config.toml` and `.vulcan/config.local.toml` when present.
  `config list` is derived from the schema registry used by config mutation commands and the settings TUI.
  `config edit` requires an interactive terminal, is schema-driven, and can edit shared or local overrides before saving.
  `config set` and `config unset` accept `--target <shared|local>`; quote strings when the shell would otherwise strip them.
  Use `config alias ...`, `config permissions profile ...`, `plugin set`, and `export profile ...` when a dedicated command is available.
  Import commands preserve unrelated config sections and overwrite the mapped target keys.
  Import flags: --preview/--dry-run, --apply, --target <shared|local>, --no-commit
  Use `config import --all` to apply every detected importer in registry order.
  Use `config import --list` to inspect detectable sources without writing.
  When git auto-commit is enabled for mutations, config edits, config CRUD, plugin config changes, and config imports participate like other mutating commands.

Examples:
  vulcan config show
  vulcan config list web
  vulcan config show periodic.daily
  vulcan config get periodic.daily.template
  vulcan config edit
  vulcan config set periodic.daily.template "Templates/Daily"
  vulcan config set web.search.backend brave --target local
  vulcan config unset web.search.backend --target local
  vulcan config alias set ship "query --where 'status = shipped'"
  vulcan config permissions profile create agent --clone readonly
  vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'
  vulcan config import core --preview
  vulcan config import core --apply
  vulcan config import dataview
  vulcan config import kanban
  vulcan config import --all --preview
  vulcan config import --list
  vulcan config import periodic-notes
  vulcan config import quickadd
  vulcan config import tasknotes --preview
  vulcan config import tasks --preview
  vulcan config import templater --target local
  vulcan --output json config get web.search.backend
  vulcan --output json config list plugins
  vulcan --output json config show web.search
  vulcan --output json config import tasks

## Subcommands

    show             Show the effective merged Vulcan config
    list             List supported config keys from the schema registry
    get              Read a single effective config value
    edit             Open the interactive settings editor
    set              Write a single config value with schema validation
    unset            Remove one config override and prune empty tables
    alias            Manage command aliases under [aliases]
      list             List effective command aliases
      set              Create or update one command alias
      delete           Delete one command alias override
    permissions      Manage config-backed permission profiles
      profile          Run or manage named permission profiles
        list             List effective permission profiles
        show             Show one effective permission profile
        create           Create one permission profile
        set              Set one permission profile field
        delete           Delete one permission profile override
    import           Import compatible Obsidian plugin settings
      core             Import Obsidian core settings
      dataview         Import Obsidian Dataview plugin settings
      templater        Import Obsidian Templater plugin settings
      quickadd         Import Obsidian QuickAdd plugin settings
      kanban           Import Obsidian Kanban plugin settings
      periodic-notes   Import Obsidian Daily Notes and Periodic Notes settings
      tasknotes        Import Obsidian TaskNotes plugin settings
      tasks            Import Obsidian Tasks plugin settings

## Generated Config Reference

Derived from Vulcan's config descriptor registry. `config set`, `config unset`, `config list`, the settings TUI, and this help surface share the same supported key metadata.

Precedence: `.vulcan/config.local.toml` > `.vulcan/config.toml` > `.obsidian/*` imports > built-in defaults.

Prefer dedicated commands when available: `config alias ...`, `config permissions profile ...`, `plugin set ...`, and `export profile ...`.

Manual editing is still supported. Use `.vulcan/config.toml` for shared defaults you want to sync, and `.vulcan/config.local.toml` for machine-local overrides such as developer-specific paths, API env-var names, or temporary experiments.

Typical TOML blocks:

```toml
[aliases]
ship = "query --where 'status = shipped'"

[permissions.profiles.agent]
read = "all"
network = { allow = true, domains = ["docs.example.com"] }

[plugins.lint]
enabled = true
path = ".vulcan/plugins/lint.js"
events = ["on_note_write", "on_pre_commit"]
sandbox = "strict"
permission_profile = "agent"

[web.search]
backend = "brave"
api_key_env = "BRAVE_API_KEY"
```

### General

Top-level vault configuration not covered by a more specific section.

- `assistant.prompts_folder` — type: `string`; target: `shared|local`; default: `AI/Prompts`
  Edit `assistant.prompts_folder` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set assistant.prompts_folder <value>`
- `assistant.skills_folder` — type: `string`; target: `shared|local`; default: `.agents/skills`
  Edit `assistant.skills_folder` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set assistant.skills_folder <value>`
- `assistant.tools_folder` — type: `string`; target: `shared|local`; default: `.agents/tools`
  Edit `assistant.tools_folder` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set assistant.tools_folder <value>`
- `chunking.overlap` — type: `integer`; target: `shared|local`; default: `0`
  Edit `chunking.overlap` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set chunking.overlap <value>`
- `chunking.strategy` — type: `string`; target: `shared|local`; default: `heading`
  Edit `chunking.strategy` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set chunking.strategy <value>`
- `chunking.target_size` — type: `integer`; target: `shared|local`; default: `4000`
  Edit `chunking.target_size` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set chunking.target_size <value>`
- `embedding.api_key_env` — type: `string`; target: `shared|local`; default: `OPENAI_API_KEY`
  Edit `embedding.api_key_env` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.api_key_env <value>`
- `embedding.base_url` — type: `string`; target: `shared|local`; default: `http://localhost:11434/v1`
  Edit `embedding.base_url` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.base_url <value>`
- `embedding.cache_key` — type: `string`; target: `shared|local`; default: `openai-compatible:text-embedding-3-small`
  Edit `embedding.cache_key` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.cache_key <value>`
- `embedding.max_batch_size` — type: `integer`; target: `shared|local`; default: `32`
  Edit `embedding.max_batch_size` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.max_batch_size <value>`
- `embedding.max_concurrency` — type: `integer`; target: `shared|local`; default: `4`
  Edit `embedding.max_concurrency` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.max_concurrency <value>`
- `embedding.max_input_tokens` — type: `integer`; target: `shared|local`; default: `8192`
  Edit `embedding.max_input_tokens` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.max_input_tokens <value>`
- `embedding.model` — type: `string`; target: `shared|local`; default: `text-embedding-3-small`
  Edit `embedding.model` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.model <value>`
- `embedding.normalized` — type: `boolean`; target: `shared|local`; default: `true`
  Edit `embedding.normalized` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.normalized <value>`
- `embedding.provider` — type: `string`; target: `shared|local`; default: `openai-compatible`
  Edit `embedding.provider` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set embedding.provider <value>`
- `extraction.args` — type: `array`; target: `shared|local`; default: `[5 items]`
  Edit `extraction.args` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set extraction.args <value>`
- `extraction.command` — type: `string`; target: `shared|local`; default: `sh`
  Edit `extraction.command` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set extraction.command <value>`
- `extraction.extensions` — type: `array`; target: `shared|local`; default: `[5 items]`
  Edit `extraction.extensions` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set extraction.extensions <value>`
- `extraction.max_output_bytes` — type: `integer`; target: `shared|local`; default: `262144`
  Edit `extraction.max_output_bytes` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set extraction.max_output_bytes <value>`
- `git.auto_commit` — type: `boolean`; target: `shared|local`; default: `false`
  Edit `git.auto_commit` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set git.auto_commit <value>`
- `git.exclude` — type: `array`; target: `shared|local`; default: `[0 items]`
  Edit `git.exclude` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set git.exclude <value>`
- `git.message` — type: `string`; target: `shared|local`; default: `vulcan {action}: {files}`
  Edit `git.message` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set git.message <value>`
- `git.scope` — type: `string`; target: `shared|local`; default: `vulcan-only`
  Edit `git.scope` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set git.scope <value>`
- `git.trigger` — type: `string`; target: `shared|local`; default: `mutation`
  Edit `git.trigger` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set git.trigger <value>`
- `inbox.format` — type: `string`; target: `shared|local`; default: `- {text}`
  Edit `inbox.format` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set inbox.format <value>`
- `inbox.heading` — type: `string`; target: `shared|local`; default: `## Inbox`
  Edit `inbox.heading` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set inbox.heading <value>`
- `inbox.path` — type: `string`; target: `shared|local`; default: `Inbox.md`
  Edit `inbox.path` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set inbox.path <value>`
- `inbox.timestamp` — type: `boolean`; target: `shared|local`; default: `true`
  Edit `inbox.timestamp` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set inbox.timestamp <value>`
- `quickadd.capture_choices` — type: `array`; target: `shared|local`; default: `[0 items]`
  Edit `quickadd.capture_choices` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set quickadd.capture_choices <value>`
- `quickadd.global_variables` — type: `object`; target: `shared|local`; default: `{2 keys}`
  Edit `quickadd.global_variables` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set quickadd.global_variables <value>`
- `quickadd.template_choices` — type: `array`; target: `shared|local`; default: `[0 items]`
  Edit `quickadd.template_choices` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set quickadd.template_choices <value>`
- `quickadd.template_folder` — type: `string`; target: `shared|local`; default: `QuickAdd/Templates`
  Edit `quickadd.template_folder` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set quickadd.template_folder <value>`
- `scan.browse_mode` — type: `enum`; target: `shared|local`; default: `background`; values: `off`, `blocking`, `background`
  Edit `scan.browse_mode` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set scan.browse_mode <value>`
- `scan.default_mode` — type: `enum`; target: `shared|local`; default: `blocking`; values: `off`, `blocking`, `background`
  Edit `scan.default_mode` in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
  Example: `vulcan config set scan.default_mode <value>`

### Links

Link formatting, resolution rules, attachment paths, and Markdown compatibility.

- `attachment_folder` — type: `string`; target: `shared|local`; default: `.`
  Override the preferred folder for new attachments.
  Example: `vulcan config set attachment_folder <value>`
- `link_resolution` — type: `string`; target: `shared|local`; default: `shortest`
  Choose whether new links resolve relative to the current file or the vault root.
  Example: `vulcan config set link_resolution <value>`
- `link_style` — type: `string`; target: `shared|local`; default: `wikilink`
  Select wikilink or Markdown link formatting for generated links.
  Example: `vulcan config set link_style <value>`
- `strict_line_breaks` — type: `boolean`; target: `shared|local`; default: `false`
  Mirror Obsidian's strict line break behavior when rendering Markdown.
  Example: `vulcan config set strict_line_breaks <value>`

### Properties

Typed frontmatter and property parsing overrides.

- `property_types.<name>` — type: `string`; target: `shared|local`
  Explicit type overrides for frontmatter properties discovered in the vault.
  Example: `vulcan config set property_types.<name> <value>`

### Templates

Template folders, triggers, and Templater-compatible defaults.

- `templates.auto_jump_to_cursor` — type: `boolean`; target: `shared|local`; default: `false`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.auto_jump_to_cursor <value>`
- `templates.command_timeout` — type: `integer`; target: `shared|local`; default: `5`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.command_timeout <value>`
- `templates.date_format` — type: `string`; target: `shared|local`; default: `YYYY-MM-DD`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.date_format <value>`
- `templates.enable_file_templates` — type: `boolean`; target: `shared|local`; default: `false`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.enable_file_templates <value>`
- `templates.enable_folder_templates` — type: `boolean`; target: `shared|local`; default: `true`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.enable_folder_templates <value>`
- `templates.enable_system_commands` — type: `boolean`; target: `shared|local`; default: `false`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.enable_system_commands <value>`
- `templates.enabled_templates_hotkeys` — type: `array`; target: `shared|local`; default: `[0 items]`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.enabled_templates_hotkeys <value>`
- `templates.file_templates` — type: `array`; target: `shared|local`; default: `[0 items]`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.file_templates <value>`
- `templates.folder_templates` — type: `array`; target: `shared|local`; default: `[0 items]`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.folder_templates <value>`
- `templates.intellisense_render` — type: `integer`; target: `shared|local`; default: `1`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.intellisense_render <value>`
- `templates.obsidian_folder` — type: `string`; target: `shared|local`; default: `Shared Templates`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.obsidian_folder <value>`
- `templates.shell_path` — type: `string`; target: `shared|local`; default: `/bin/bash`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.shell_path <value>`
- `templates.startup_templates` — type: `array`; target: `shared|local`; default: `[0 items]`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.startup_templates <value>`
- `templates.syntax_highlighting` — type: `boolean`; target: `shared|local`; default: `true`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.syntax_highlighting <value>`
- `templates.syntax_highlighting_mobile` — type: `boolean`; target: `shared|local`; default: `false`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.syntax_highlighting_mobile <value>`
- `templates.templater_folder` — type: `string`; target: `shared|local`; default: `Templates`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.templater_folder <value>`
- `templates.templates_pairs` — type: `array`; target: `shared|local`; default: `[0 items]`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.templates_pairs <value>`
- `templates.time_format` — type: `string`; target: `shared|local`; default: `HH:mm`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.time_format <value>`
- `templates.trigger_on_file_creation` — type: `boolean`; target: `shared|local`; default: `false`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.trigger_on_file_creation <value>`
- `templates.user_scripts_folder` — type: `string`; target: `shared|local`; default: `Scripts`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.user_scripts_folder <value>`
- `templates.web_allowlist` — type: `array`; target: `shared|local`; default: `[0 items]`
  Template discovery, file triggers, folder mappings, and shell integration.
  Example: `vulcan config set templates.web_allowlist <value>`

### Periodic Notes

Daily, weekly, monthly, quarterly, and yearly note generation settings.

- `periodic.daily.enabled` — type: `boolean`; target: `shared|local`; default: `true`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.enabled <value>`
- `periodic.daily.folder` — type: `string`; target: `shared|local`; default: `Journal/Daily`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.folder <value>`
- `periodic.daily.format` — type: `string`; target: `shared|local`; default: `YYYY-MM-DD`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.format <value>`
- `periodic.daily.interval` — type: `integer`; target: `shared|local`; default: `1`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.interval <value>`
- `periodic.daily.schedule_heading` — type: `string`; target: `shared|local`; default: `Schedule`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.schedule_heading <value>`
- `periodic.daily.start_of_week` — type: `string`; target: `shared|local`; default: `monday`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.start_of_week <value>`
- `periodic.daily.template` — type: `string`; target: `shared|local`; default: `daily`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.template <value>`
- `periodic.daily.unit` — type: `string`; target: `shared|local`; default: `days`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.daily.unit <value>`
- `periodic.monthly.enabled` — type: `boolean`; target: `shared|local`; default: `true`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.enabled <value>`
- `periodic.monthly.folder` — type: `string`; target: `shared|local`; default: `Journal/Monthly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.folder <value>`
- `periodic.monthly.format` — type: `string`; target: `shared|local`; default: `YYYY-MM`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.format <value>`
- `periodic.monthly.interval` — type: `integer`; target: `shared|local`; default: `1`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.interval <value>`
- `periodic.monthly.start_of_week` — type: `string`; target: `shared|local`; default: `monday`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.start_of_week <value>`
- `periodic.monthly.template` — type: `string`; target: `shared|local`; default: `monthly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.template <value>`
- `periodic.monthly.unit` — type: `string`; target: `shared|local`; default: `months`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.monthly.unit <value>`
- `periodic.quarterly.enabled` — type: `boolean`; target: `shared|local`; default: `false`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.enabled <value>`
- `periodic.quarterly.folder` — type: `string`; target: `shared|local`; default: `Journal/Quarterly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.folder <value>`
- `periodic.quarterly.format` — type: `string`; target: `shared|local`; default: `YYYY-[Q]Q`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.format <value>`
- `periodic.quarterly.interval` — type: `integer`; target: `shared|local`; default: `1`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.interval <value>`
- `periodic.quarterly.start_of_week` — type: `string`; target: `shared|local`; default: `monday`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.start_of_week <value>`
- `periodic.quarterly.template` — type: `string`; target: `shared|local`; default: `quarterly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.template <value>`
- `periodic.quarterly.unit` — type: `string`; target: `shared|local`; default: `quarters`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.quarterly.unit <value>`
- `periodic.sprint.anchor_date` — type: `string`; target: `shared|local`; default: `2026-01-05`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.anchor_date <value>`
- `periodic.sprint.enabled` — type: `boolean`; target: `shared|local`; default: `true`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.enabled <value>`
- `periodic.sprint.folder` — type: `string`; target: `shared|local`; default: `Journal/Sprints`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.folder <value>`
- `periodic.sprint.format` — type: `string`; target: `shared|local`; default: `YYYY-[Sprint]-MM-DD`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.format <value>`
- `periodic.sprint.interval` — type: `integer`; target: `shared|local`; default: `2`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.interval <value>`
- `periodic.sprint.template` — type: `string`; target: `shared|local`; default: `sprint`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.template <value>`
- `periodic.sprint.unit` — type: `string`; target: `shared|local`; default: `weeks`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.sprint.unit <value>`
- `periodic.weekly.enabled` — type: `boolean`; target: `shared|local`; default: `true`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.enabled <value>`
- `periodic.weekly.folder` — type: `string`; target: `shared|local`; default: `Journal/Weekly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.folder <value>`
- `periodic.weekly.format` — type: `string`; target: `shared|local`; default: `YYYY-[W]ww`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.format <value>`
- `periodic.weekly.interval` — type: `integer`; target: `shared|local`; default: `1`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.interval <value>`
- `periodic.weekly.start_of_week` — type: `string`; target: `shared|local`; default: `monday`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.start_of_week <value>`
- `periodic.weekly.template` — type: `string`; target: `shared|local`; default: `weekly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.template <value>`
- `periodic.weekly.unit` — type: `string`; target: `shared|local`; default: `weeks`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.weekly.unit <value>`
- `periodic.yearly.enabled` — type: `boolean`; target: `shared|local`; default: `false`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.enabled <value>`
- `periodic.yearly.folder` — type: `string`; target: `shared|local`; default: `Journal/Yearly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.folder <value>`
- `periodic.yearly.format` — type: `string`; target: `shared|local`; default: `YYYY`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.format <value>`
- `periodic.yearly.interval` — type: `integer`; target: `shared|local`; default: `1`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.interval <value>`
- `periodic.yearly.start_of_week` — type: `string`; target: `shared|local`; default: `monday`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.start_of_week <value>`
- `periodic.yearly.template` — type: `string`; target: `shared|local`; default: `yearly`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.template <value>`
- `periodic.yearly.unit` — type: `string`; target: `shared|local`; default: `years`
  Periodic note folder, filename format, template, cadence, and schedule heading.
  Example: `vulcan config set periodic.yearly.unit <value>`

### Tasks

Task query defaults, statuses, and recurrence behavior.

- `tasks.default_source` — type: `enum`; target: `shared|local`; default: `all`; values: `tasknotes`, `inline`, `all`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.default_source <value>`
- `tasks.global_filter` — type: `string`; target: `shared|local`; default: `#task`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.global_filter <value>`
- `tasks.global_query` — type: `string`; target: `shared|local`; default: `not done`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.global_query <value>`
- `tasks.recurrence_on_completion` — type: `enum`; target: `shared|local`; default: `next-line`; values: `same-line`, `next-line`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.recurrence_on_completion <value>`
- `tasks.remove_global_filter` — type: `boolean`; target: `shared|local`; default: `false`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.remove_global_filter <value>`
- `tasks.set_created_date` — type: `boolean`; target: `shared|local`; default: `false`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.set_created_date <value>`
- `tasks.statuses.cancelled` — type: `array`; target: `shared|local`; default: `[1 item]`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.statuses.cancelled <value>`
- `tasks.statuses.completed` — type: `array`; target: `shared|local`; default: `[2 items]`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.statuses.completed <value>`
- `tasks.statuses.definitions` — type: `array`; target: `shared|local`; default: `[0 items]`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.statuses.definitions <value>`
- `tasks.statuses.in_progress` — type: `array`; target: `shared|local`; default: `[1 item]`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.statuses.in_progress <value>`
- `tasks.statuses.non_task` — type: `array`; target: `shared|local`; default: `[0 items]`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.statuses.non_task <value>`
- `tasks.statuses.todo` — type: `array`; target: `shared|local`; default: `[1 item]`
  Task query defaults, status sets, created-date behavior, and recurrence settings.
  Example: `vulcan config set tasks.statuses.todo <value>`

### TaskNotes

TaskNotes folders, statuses, NLP, pomodoro, and saved views.

- `tasknotes.archive_folder` — type: `string`; target: `shared|local`; default: `TaskNotes/Archive`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.archive_folder <value>`
- `tasknotes.default_priority` — type: `string`; target: `shared|local`; default: `normal`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.default_priority <value>`
- `tasknotes.default_status` — type: `string`; target: `shared|local`; default: `open`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.default_status <value>`
- `tasknotes.enable_natural_language_input` — type: `boolean`; target: `shared|local`; default: `true`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.enable_natural_language_input <value>`
- `tasknotes.excluded_folders` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.excluded_folders <value>`
- `tasknotes.field_mapping.archive_tag` — type: `string`; target: `shared|local`; default: `archived`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.archive_tag <value>`
- `tasknotes.field_mapping.blocked_by` — type: `string`; target: `shared|local`; default: `blockedBy`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.blocked_by <value>`
- `tasknotes.field_mapping.complete_instances` — type: `string`; target: `shared|local`; default: `complete_instances`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.complete_instances <value>`
- `tasknotes.field_mapping.completed_date` — type: `string`; target: `shared|local`; default: `completedDate`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.completed_date <value>`
- `tasknotes.field_mapping.contexts` — type: `string`; target: `shared|local`; default: `contexts`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.contexts <value>`
- `tasknotes.field_mapping.date_created` — type: `string`; target: `shared|local`; default: `dateCreated`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.date_created <value>`
- `tasknotes.field_mapping.date_modified` — type: `string`; target: `shared|local`; default: `dateModified`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.date_modified <value>`
- `tasknotes.field_mapping.due` — type: `string`; target: `shared|local`; default: `due`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.due <value>`
- `tasknotes.field_mapping.pomodoros` — type: `string`; target: `shared|local`; default: `pomodoros`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.pomodoros <value>`
- `tasknotes.field_mapping.priority` — type: `string`; target: `shared|local`; default: `priority`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.priority <value>`
- `tasknotes.field_mapping.projects` — type: `string`; target: `shared|local`; default: `projects`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.projects <value>`
- `tasknotes.field_mapping.recurrence` — type: `string`; target: `shared|local`; default: `recurrence`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.recurrence <value>`
- `tasknotes.field_mapping.recurrence_anchor` — type: `string`; target: `shared|local`; default: `recurrence_anchor`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.recurrence_anchor <value>`
- `tasknotes.field_mapping.reminders` — type: `string`; target: `shared|local`; default: `reminders`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.reminders <value>`
- `tasknotes.field_mapping.scheduled` — type: `string`; target: `shared|local`; default: `scheduled`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.scheduled <value>`
- `tasknotes.field_mapping.skipped_instances` — type: `string`; target: `shared|local`; default: `skipped_instances`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.skipped_instances <value>`
- `tasknotes.field_mapping.status` — type: `string`; target: `shared|local`; default: `status`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.status <value>`
- `tasknotes.field_mapping.time_entries` — type: `string`; target: `shared|local`; default: `timeEntries`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.time_entries <value>`
- `tasknotes.field_mapping.time_estimate` — type: `string`; target: `shared|local`; default: `timeEstimate`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.time_estimate <value>`
- `tasknotes.field_mapping.title` — type: `string`; target: `shared|local`; default: `title`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.field_mapping.title <value>`
- `tasknotes.identification_method` — type: `string`; target: `shared|local`; default: `tag`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.identification_method <value>`
- `tasknotes.nlp_default_to_scheduled` — type: `boolean`; target: `shared|local`; default: `false`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.nlp_default_to_scheduled <value>`
- `tasknotes.nlp_language` — type: `string`; target: `shared|local`; default: `en`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.nlp_language <value>`
- `tasknotes.nlp_triggers` — type: `array`; target: `shared|local`; default: `[5 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.nlp_triggers <value>`
- `tasknotes.pomodoro.long_break` — type: `integer`; target: `shared|local`; default: `15`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.pomodoro.long_break <value>`
- `tasknotes.pomodoro.long_break_interval` — type: `integer`; target: `shared|local`; default: `4`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.pomodoro.long_break_interval <value>`
- `tasknotes.pomodoro.short_break` — type: `integer`; target: `shared|local`; default: `5`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.pomodoro.short_break <value>`
- `tasknotes.pomodoro.storage_location` — type: `string`; target: `shared|local`; default: `task`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.pomodoro.storage_location <value>`
- `tasknotes.pomodoro.work_duration` — type: `integer`; target: `shared|local`; default: `25`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.pomodoro.work_duration <value>`
- `tasknotes.priorities` — type: `array`; target: `shared|local`; default: `[4 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.priorities <value>`
- `tasknotes.saved_views` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.saved_views <value>`
- `tasknotes.statuses` — type: `array`; target: `shared|local`; default: `[4 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.statuses <value>`
- `tasknotes.task_creation_defaults.default_contexts` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_contexts <value>`
- `tasknotes.task_creation_defaults.default_due_date` — type: `string`; target: `shared|local`; default: `none`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_due_date <value>`
- `tasknotes.task_creation_defaults.default_projects` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_projects <value>`
- `tasknotes.task_creation_defaults.default_recurrence` — type: `string`; target: `shared|local`; default: `none`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_recurrence <value>`
- `tasknotes.task_creation_defaults.default_reminders` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_reminders <value>`
- `tasknotes.task_creation_defaults.default_scheduled_date` — type: `string`; target: `shared|local`; default: `none`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_scheduled_date <value>`
- `tasknotes.task_creation_defaults.default_tags` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_creation_defaults.default_tags <value>`
- `tasknotes.task_tag` — type: `string`; target: `shared|local`; default: `task`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.task_tag <value>`
- `tasknotes.tasks_folder` — type: `string`; target: `shared|local`; default: `TaskNotes/Tasks`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.tasks_folder <value>`
- `tasknotes.user_fields` — type: `array`; target: `shared|local`; default: `[0 items]`
  TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.
  Example: `vulcan config set tasknotes.user_fields <value>`

### Kanban

Kanban board formatting, archiving, and display preferences.

- `kanban.append_archive_date` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.append_archive_date <value>`
- `kanban.archive_date_format` — type: `string`; target: `shared|local`; default: `YYYY-MM-DD HH:mm`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.archive_date_format <value>`
- `kanban.archive_date_separator` — type: `string`; target: `shared|local`; default: ``
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.archive_date_separator <value>`
- `kanban.archive_with_date` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.archive_with_date <value>`
- `kanban.date_colors` — type: `array`; target: `shared|local`; default: `[0 items]`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.date_colors <value>`
- `kanban.date_display_format` — type: `string`; target: `shared|local`; default: `YYYY-MM-DD`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.date_display_format <value>`
- `kanban.date_format` — type: `string`; target: `shared|local`; default: `YYYY-MM-DD`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.date_format <value>`
- `kanban.date_picker_week_start` — type: `integer`; target: `shared|local`; default: `1`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.date_picker_week_start <value>`
- `kanban.date_time_display_format` — type: `string`; target: `shared|local`; default: `YYYY-MM-DD HH:mm`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.date_time_display_format <value>`
- `kanban.date_trigger` — type: `string`; target: `shared|local`; default: `@`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.date_trigger <value>`
- `kanban.full_list_lane_width` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.full_list_lane_width <value>`
- `kanban.hide_card_count` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.hide_card_count <value>`
- `kanban.hide_tags_display` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.hide_tags_display <value>`
- `kanban.hide_tags_in_title` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.hide_tags_in_title <value>`
- `kanban.inline_metadata_position` — type: `enum`; target: `shared|local`; default: `body`; values: `body`, `footer`, `metadata-table`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.inline_metadata_position <value>`
- `kanban.lane_width` — type: `integer`; target: `shared|local`; default: `272`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.lane_width <value>`
- `kanban.link_date_to_daily_note` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.link_date_to_daily_note <value>`
- `kanban.list_collapse` — type: `array`; target: `shared|local`; default: `[0 items]`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.list_collapse <value>`
- `kanban.max_archive_size` — type: `integer`; target: `shared|local`; default: `100`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.max_archive_size <value>`
- `kanban.metadata_keys` — type: `array`; target: `shared|local`; default: `[0 items]`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.metadata_keys <value>`
- `kanban.move_dates` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.move_dates <value>`
- `kanban.move_tags` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.move_tags <value>`
- `kanban.move_task_metadata` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.move_task_metadata <value>`
- `kanban.new_card_insertion_method` — type: `enum`; target: `shared|local`; default: `append`; values: `prepend`, `prepend-compact`, `append`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.new_card_insertion_method <value>`
- `kanban.new_line_trigger` — type: `enum`; target: `shared|local`; default: `shift-enter`; values: `enter`, `shift-enter`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.new_line_trigger <value>`
- `kanban.new_note_folder` — type: `string`; target: `shared|local`; default: `Cards`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.new_note_folder <value>`
- `kanban.new_note_template` — type: `string`; target: `shared|local`; default: `Kanban Card`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.new_note_template <value>`
- `kanban.show_add_list` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_add_list <value>`
- `kanban.show_archive_all` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_archive_all <value>`
- `kanban.show_board_settings` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_board_settings <value>`
- `kanban.show_checkboxes` — type: `boolean`; target: `shared|local`; default: `false`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_checkboxes <value>`
- `kanban.show_relative_date` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_relative_date <value>`
- `kanban.show_search` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_search <value>`
- `kanban.show_set_view` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_set_view <value>`
- `kanban.show_view_as_markdown` — type: `boolean`; target: `shared|local`; default: `true`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.show_view_as_markdown <value>`
- `kanban.table_sizing` — type: `object`; target: `shared|local`; default: `{2 keys}`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.table_sizing <value>`
- `kanban.tag_action` — type: `enum`; target: `shared|local`; default: `obsidian`; values: `kanban`, `obsidian`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.tag_action <value>`
- `kanban.tag_colors` — type: `array`; target: `shared|local`; default: `[0 items]`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.tag_colors <value>`
- `kanban.tag_sort` — type: `array`; target: `shared|local`; default: `[0 items]`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.tag_sort <value>`
- `kanban.time_format` — type: `string`; target: `shared|local`; default: `HH:mm`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.time_format <value>`
- `kanban.time_trigger` — type: `string`; target: `shared|local`; default: `@@`
  Kanban board metadata keys, archiving, layout, and card creation settings.
  Example: `vulcan config set kanban.time_trigger <value>`

### Dataview

Dataview compatibility flags, rendering behavior, and JS limits.

- `dataview.default_date_format` — type: `string`; target: `shared|local`; default: `MMMM dd, yyyy`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.default_date_format <value>`
- `dataview.default_datetime_format` — type: `string`; target: `shared|local`; default: `h:mm a - MMMM dd, yyyy`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.default_datetime_format <value>`
- `dataview.display_result_count` — type: `boolean`; target: `shared|local`; default: `true`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.display_result_count <value>`
- `dataview.enable_dataview_js` — type: `boolean`; target: `shared|local`; default: `true`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.enable_dataview_js <value>`
- `dataview.enable_inline_dataview_js` — type: `boolean`; target: `shared|local`; default: `false`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.enable_inline_dataview_js <value>`
- `dataview.group_column_name` — type: `string`; target: `shared|local`; default: `Group`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.group_column_name <value>`
- `dataview.inline_js_query_prefix` — type: `string`; target: `shared|local`; default: `$=`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.inline_js_query_prefix <value>`
- `dataview.inline_query_prefix` — type: `string`; target: `shared|local`; default: `=`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.inline_query_prefix <value>`
- `dataview.js_max_stack_size_bytes` — type: `integer`; target: `shared|local`; default: `262144`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.js_max_stack_size_bytes <value>`
- `dataview.js_memory_limit_bytes` — type: `integer`; target: `shared|local`; default: `16777216`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.js_memory_limit_bytes <value>`
- `dataview.js_timeout_seconds` — type: `integer`; target: `shared|local`; default: `30`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.js_timeout_seconds <value>`
- `dataview.max_recursive_render_depth` — type: `integer`; target: `shared|local`; default: `4`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.max_recursive_render_depth <value>`
- `dataview.primary_column_name` — type: `string`; target: `shared|local`; default: `File`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.primary_column_name <value>`
- `dataview.recursive_subtask_completion` — type: `boolean`; target: `shared|local`; default: `false`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.recursive_subtask_completion <value>`
- `dataview.task_completion_text` — type: `string`; target: `shared|local`; default: `completion`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.task_completion_text <value>`
- `dataview.task_completion_tracking` — type: `boolean`; target: `shared|local`; default: `false`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.task_completion_tracking <value>`
- `dataview.task_completion_use_emoji_shorthand` — type: `boolean`; target: `shared|local`; default: `false`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.task_completion_use_emoji_shorthand <value>`
- `dataview.timezone` — type: `string`; target: `shared|local`; default: `+02:00`
  Dataview rendering compatibility, inline query prefixes, and JS execution limits.
  Example: `vulcan config set dataview.timezone <value>`

### JS Runtime

Sandbox defaults, runtime memory limits, and script locations.

- `js_runtime.default_sandbox` — type: `enum`; target: `shared|local`; default: `strict`; values: `strict`, `fs`, `net`, `none`
  Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.
  Example: `vulcan config set js_runtime.default_sandbox <value>`
- `js_runtime.default_timeout_seconds` — type: `integer`; target: `shared|local`; default: `30`
  Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.
  Example: `vulcan config set js_runtime.default_timeout_seconds <value>`
- `js_runtime.memory_limit_mb` — type: `integer`; target: `shared|local`; default: `64`
  Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.
  Example: `vulcan config set js_runtime.memory_limit_mb <value>`
- `js_runtime.scripts_folder` — type: `string`; target: `shared|local`; default: `.vulcan/scripts`
  Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.
  Example: `vulcan config set js_runtime.scripts_folder <value>`
- `js_runtime.stack_limit_kb` — type: `integer`; target: `shared|local`; default: `256`
  Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.
  Example: `vulcan config set js_runtime.stack_limit_kb <value>`

### Web

Web search backend selection and API endpoint configuration.

- `web.search.backend` — type: `enum`; target: `shared|local`; default: `duckduckgo`; values: `disabled`, `duckduckgo`, `auto`, `kagi`, `exa`, `tavily`, `brave`, `ollama`
  Configure the preferred web search provider, API key env var, and base URL.
  Example: `vulcan config set web.search.backend <value>`
- `web.user_agent` — type: `string`; target: `shared|local`; default: `Vulcan/0.1 (+https://github.com/tionis/vulcan)`
  Shared web client settings such as the user agent used by fetch/search helpers.
  Example: `vulcan config set web.user_agent <value>`

### Plugins

Registered event-driven plugin settings for the current vault.

- `plugins.<name>` — type: `object`; target: `shared|local`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin set`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`
- `plugins.<name>.description` — type: `string`; target: `shared|local`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin set --description`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`
- `plugins.<name>.enabled` — type: `boolean`; target: `shared|local`; default: `true`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin enable`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`
- `plugins.<name>.events` — type: `array`; target: `shared|local`; default: `[0 items]`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin set --add-event`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`
- `plugins.<name>.path` — type: `string`; target: `shared|local`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin set --path`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`
- `plugins.<name>.permission_profile` — type: `string`; target: `shared|local`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin set --permission-profile`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`
- `plugins.<name>.sandbox` — type: `enum`; target: `shared|local`; values: `strict`, `fs`, `net`, `none`
  Per-plugin registration, hook subscription, sandbox, and permission profile settings.
  Preferred command: `vulcan plugin set --sandbox`
  Example: `vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict`

### Permissions

Static permission profiles used by plugins, MCP, and scripted callers.

- `permissions.profiles.<name>` — type: `object`; target: `shared|local`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile create`
  Example: `vulcan config permissions profile create agent --clone readonly`
- `permissions.profiles.<name>.config` — type: `enum`; target: `shared|local`; values: `none`, `read`, `write`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.cpu_limit_ms` — type: `integer`; target: `shared|local`; values: `unlimited`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.execute` — type: `enum`; target: `shared|local`; values: `allow`, `deny`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.git` — type: `enum`; target: `shared|local`; values: `allow`, `deny`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.index` — type: `enum`; target: `shared|local`; values: `allow`, `deny`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.memory_limit_mb` — type: `integer`; target: `shared|local`; values: `unlimited`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.network` — type: `flexible`; target: `shared|local`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.policy_hook` — type: `string`; target: `shared|local`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.read` — type: `flexible`; target: `shared|local`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.refactor` — type: `flexible`; target: `shared|local`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.shell` — type: `enum`; target: `shared|local`; values: `allow`, `deny`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.stack_limit_kb` — type: `integer`; target: `shared|local`; values: `unlimited`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`
- `permissions.profiles.<name>.write` — type: `flexible`; target: `shared|local`
  Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.
  Preferred command: `vulcan config permissions profile set`
  Example: `vulcan config permissions profile set agent network '{ allow = true, domains = ["example.com"] }'`

### Aliases

Custom top-level CLI command aliases expanded before clap parsing.

- `aliases.<name>` — type: `string`; target: `shared|local`
  Alias expansion for short custom commands like `today = "query --format count"`.
  Preferred command: `vulcan config alias set`
  Example: `vulcan config alias set ship "query --where 'status = shipped'"`
- `aliases.q` — type: `string`; target: `shared|local`; default: `query`
  Alias expansion for short custom commands like `today = "query --format count"`.
  Preferred command: `vulcan config alias set`
  Example: `vulcan config set aliases.q <value>`
- `aliases.t` — type: `string`; target: `shared|local`; default: `tasks list`
  Alias expansion for short custom commands like `today = "query --format count"`.
  Preferred command: `vulcan config alias set`
  Example: `vulcan config set aliases.t <value>`
- `aliases.today` — type: `string`; target: `shared|local`; default: `daily today`
  Alias expansion for short custom commands like `today = "query --format count"`.
  Preferred command: `vulcan config alias set`
  Example: `vulcan config set aliases.today <value>`

### Export Profiles

Named export profiles stored in config and managed by dedicated export commands.

- `export.profiles.<name>` — type: `object`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile create`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.author` — type: `string`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.backlinks` — type: `boolean`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.content_transforms` — type: `array`; target: `shared`; default: `[0 items]`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile rule add`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.format` — type: `enum`; target: `shared`; values: `markdown`, `json`, `csv`, `graph`, `epub`, `zip`, `sqlite`, `search-index`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.frontmatter` — type: `boolean`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.graph_format` — type: `enum`; target: `shared`; values: `json`, `dot`, `graphml`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.path` — type: `string`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.pretty` — type: `boolean`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.query` — type: `string`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.query_json` — type: `string`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.title` — type: `string`; target: `shared`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`
- `export.profiles.<name>.toc` — type: `enum`; target: `shared`; values: `tree`, `flat`
  Named export profile metadata; dedicated `export profile` commands are preferred for edits.
  Preferred command: `vulcan export profile set`
  Example: `vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub`

### Static Site

Static-site publication profiles, filters, route policies, and theme assets.

- `site.profiles.<name>` — type: `object`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public '{}'`
- `site.profiles.<name>.asset_policy.include_folders` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.asset_policy.mode` — type: `enum`; target: `shared|local`; values: `copy_referenced`, `error_on_missing`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.backlinks` — type: `boolean`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.base_url` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.content_transforms` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.dataview_js` — type: `enum`; target: `shared|local`; values: `off`, `static`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.deploy_path` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.exclude_folders` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.exclude_paths` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.exclude_tags` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.extra_css` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.extra_js` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.favicon` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.graph` — type: `boolean`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.home` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.include_folders` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.include_paths` — type: `array`; target: `shared|local`; default: `[0 items]`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.include_query` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.include_query_json` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.language` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.link_policy` — type: `enum`; target: `shared|local`; values: `error`, `warn`, `drop_link`, `render_plain_text`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.logo` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.output_dir` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.page_title_template` — type: `string`; target: `shared|local`
  Template for the HTML `<title>` tag on built pages. Supported placeholders: `{page}`, `{site}`, and `{profile}`.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.page_title_template '"{site} :: {page}"'`
- `site.profiles.<name>.rss` — type: `boolean`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.search` — type: `boolean`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.theme` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
- `site.profiles.<name>.title` — type: `string`; target: `shared|local`
  Static-site publication profile metadata, publish filters, theme assets, and route policy settings.
  Preferred command: `vulcan config set`
  Example: `vulcan config set site.profiles.public.title '"Public Notes"'`
