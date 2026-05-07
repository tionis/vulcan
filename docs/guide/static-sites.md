# Static Sites

Vulcan can publish a vault as a static website with profile-scoped filtering, a shared HTML renderer,
and a local preview server.

## Core commands

- `vulcan site profiles` lists effective site profiles and note counts.
- `vulcan site doctor --profile public` reports publish-facing issues such as unpublished link targets
  and route collisions.
- `vulcan site build --profile public` writes the site output to the configured `output_dir`.
- `vulcan site build --profile public --watch` keeps rebuilding after vault changes.
- `vulcan site build --profile public --strict` fails when publish diagnostics would leak into a CI build.
- `vulcan site serve --profile public --watch` builds once, serves the output on loopback, and bumps a
  live-reload version when rebuilds succeed.
- `vulcan site serve --profile public --watch --strict` keeps the last good output in place when a watched
  rebuild would introduce publish diagnostics.

`--fail-on-warning` is an alias for `--strict` on `site build` and `site serve`.

## Site profiles

Site profiles live under `[site.profiles.<name>]` in `.vulcan/config.toml`.

Example:

```toml
[site.profiles.public]
title = "Public Notes"
page_title_template = "{page} | {site}"
base_url = "https://notes.example.com"
deploy_path = "/garden"
output_dir = ".vulcan/site/public"
home = "Home"
search = true
graph = true
rss = true
include_paths = ["Home.md"]
exclude_folders = ["Templates/**", "Archive/**"]
exclude_tags = ["private", "draft"]
theme = "reference"
link_policy = "warn"
dataview_js = "off"
raw_html = "sanitize"
logo = "site/logo.png"

[site.profiles.public.asset_policy]
mode = "copy_referenced"
include_folders = ["site/shared/**"]
```

Important rules:

- The vault stays the source of truth. The output directory is disposable.
- Profiles publish by omission. Excluded notes do not appear in HTML, search, graph assets, hover
  previews, feeds, or copied assets.
- `content_transforms`, link policy, and asset policy reuse the same publication model as export
  profiles.
- `deploy_path` is optional and distinct from `base_url`. Use it when the built site will be hosted
  under a subpath such as `/garden/` or `/wiki/`.
- Site profiles can be layered through `.vulcan/config.local.toml` when a local preview needs a
  different `output_dir`, `base_url`, or filter set.
- Static mode has no runtime auth layer. “Private pages” means excluded at build time, never hidden in
  emitted HTML or JSON.
- `page_title_template` controls the browser `<title>` tag. Supported placeholders are `{page}`,
  `{site}`, and `{profile}`.
- `raw_html` controls how literal HTML blocks/inline HTML are published: `passthrough`, `sanitize`,
  or `strip`. `sanitize` keeps the content visible while removing unsafe markup; `strip` removes the
  raw tags and reports diagnostics in the rendered note.
- `dataview_js = "static"` runs DataviewJS blocks in deterministic publish mode. Static rendering
  rejects wall-clock access (`Date.now()`, `new Date()`, `VulcanDateTime.now()`, `today`-style
  daily helpers), network calls, host exec/shell helpers, and any filesystem writes. Unsupported
  blocks stay visible in the output as DataviewJS error callouts instead of disappearing silently.

Per-note publish metadata can be overridden in frontmatter, including `title`, `slug`, `description`,
`canonical_url`, and `summary_image`.

Example:

```yaml
---
site:
  profiles:
    public:
      title: Launch Page
      slug: launch
      description: Public-facing summary for the launch page.
      canonical_url: https://notes.example.com/start/
      summary_image: site/social/launch.png
---
```

## Renderer contract

The static site builder uses the same vault-aware HTML renderer as:

- `vulcan note get --mode html`
- `vulcan render --mode html`
- future WebUI and wiki note pages

That renderer understands Vulcan's note model instead of treating files as plain CommonMark. It keeps
wikilinks, embeds, callouts, task lists, headings, backlinks, and publication diagnostics on one path.
The shared contracts exposed by the builder are `RenderContext`, `RenderedNote`, `RenderedEmbed`, and
`SiteRoute`; later note pages should reuse those structures instead of defining parallel HTML/page models.

## Theme contract

`theme = "default"` uses the built-in CSS/JS shell. Any other theme name resolves to either:

- `.vulcan/site/themes/<theme>/`
- a vault-relative directory path when `theme` already looks like a path

Optional files in that directory:

- `theme.css`
- `theme.js`
- `head.html`
- `toolbar.html`
- `header.html`
- `nav.html`
- `left_rail.html`
- `right_rail.html`
- `footer.html`
- `note_before.html`
- `note_after.html`

This is intentionally a small fixed compatibility surface, not a general template language. Theme
partials can use stable placeholder tokens:

- `{{site_title}}`
- `{{page_title}}`
- `{{page_description}}`
- `{{profile_name}}`
- `{{home_href}}`
- `{{deploy_path}}`
- `{{canonical_url}}`
- `{{current_note_path}}`
- `{{nav}}`
- `{{search_button}}`
- `{{palette_controls}}`
- `{{reader_mode_toggle}}`
- `{{theme_toggle}}`
- `{{explorer}}`
- `{{toolbar}}`
- `{{left_rail}}`
- `{{right_rail}}`
- `{{site_logo}}`

`theme_toggle` is kept as a compatibility alias for the built-in palette controls. `toolbar.html`
wraps or replaces the built-in mobile utility dock, `header.html` inserts extra chrome between that
shell control region and the main layout, and `left_rail.html` / `right_rail.html` can replace the
default explorer and page-panel rails while still receiving the same stable tokens. `header.html` still supersedes `nav.html`;
otherwise `nav.html` only replaces the primary-nav strip inside the default left rail. `theme.css`
and `theme.js` are copied into published assets and loaded after Vulcan's built-in shell assets. A
reference bundle lives at
`docs/examples/static-site-theme-reference/`.

## Shell and module config

The built-in shell now has first-class profile-scoped config for layout chrome, explorer behavior,
and right-rail modules:

```toml
[site.profiles.public]
title = "Public Notes"
home = "Home"
search = true
graph = true
backlinks = true

[site.profiles.public.shell]
reader_mode = true
default_palette = "system" # system | light | dark
left_rail = true
right_rail = true

[site.profiles.public.navigation]
explorer = true
folder_click = "collapse"  # collapse | link
default_folder_state = "collapsed"
use_saved_state = true
show_home = true
show_recent = true
show_folders = true
show_tags = true
show_graph = true

[site.profiles.public.modules]
toc = true
graph = true
backlinks = true
outgoing_links = true
```

Current defaults and semantics:

- `shell.reader_mode = true` enables the built-in reader-mode toggle; the active state persists in
  browser storage per site profile
- `shell.default_palette` chooses the default `system` / `light` / `dark` palette before any stored
  user preference is applied
- `navigation.folder_click = "collapse"` is the built-in default and makes folder rows behave like a
  collapsible explorer; when a folder note such as `Guides/index.md` or `Guides/Guides.md` exists,
  the built-in shell also renders a separate `Open` affordance for that folder landing page
- `navigation.folder_click = "link"` makes folder rows navigate directly to folder notes or generated
  folder listing pages instead of using the row label as the collapse toggle
- `navigation.default_folder_state` seeds the explorer's initial collapse state; the built-in shell
  restores saved folder open/closed state when `use_saved_state = true`
- `modules.*` control which right-rail panels are available in the built-in shell; the shell still
  only renders modules whose source data exists on the current page, so TOC stays hidden when a page
  has no headings and local graph stays hidden when graph export is disabled

## Math and Mermaid contract

Vulcan keeps math and Mermaid responsibilities split cleanly between the shared server renderer and
an optional browser runtime.

Server-side output:

- inline and display math stay in the HTML as TeX text wrapped in stable `data-site-math="inline"`
  or `data-site-math="display"` markers
- Mermaid fences stay readable as `<pre><code class="language-mermaid">...</code></pre>` with
  `data-site-mermaid-source="true"` on the `<pre>` wrapper

Built-in shell behavior:

- `assets/vulcan-site.js` does not bundle KaTeX or Mermaid on its own
- on `DOMContentLoaded`, it dispatches `vulcan-site:math` and `vulcan-site:mermaid`
  `CustomEvent`s with the matched nodes in `event.detail.nodes`
- if those events are not canceled and a compatible global runtime is present, the shell enhances
  the page in place with `window.katex.render(...)` or `window.mermaid.run(...)`
- if no runtime is present, the server-rendered fallback remains visible instead of disappearing

Custom themes or external frontends can either:

- provide global KaTeX/Mermaid runtimes and let the built-in shell enhance automatically
- listen for `vulcan-site:math` or `vulcan-site:mermaid`, call `event.preventDefault()`, and take
  over rendering themselves

Because `theme.js` loads after the built-in shell, these enhancement hooks fire on
`DOMContentLoaded` so custom themes still have time to register listeners first.

## Preview server

`vulcan site serve` is loopback-only and daemon-independent. It serves built files directly from the
site output directory and exposes a local live-reload endpoint at:

- `/__vulcan_site/live-reload.json`
- `/__vulcan_site/live-reload.events`

When a profile sets `deploy_path`, the built HTML, manifests, RSS links, canonical metadata, and
preview live-reload endpoint all use that prefix. The loopback preview still serves `/` for convenience
and also serves the prefixed routes such as `/garden/`, `/garden/__vulcan_site/live-reload.json`, and
`/garden/__vulcan_site/live-reload.events`.

When `--watch` is enabled, the server watches the vault, `.vulcan/config.toml`, and referenced site
assets through the normal vault watcher. Successful rebuilds track which generated files actually
changed or were deleted, only rewrite outputs whose bytes differ, and bump the live-reload version.
Failed rebuilds keep the previous output available and surface the last error through the live-reload
payload so the browser overlay can show it. The payload also includes publish diagnostics plus
`changed_files` / `deleted_files` so downstream tooling can react to the watched build.

When strict mode is enabled, Vulcan preflights the rebuild in `--dry-run` mode first. If publish
warnings or errors are detected, the running preview keeps serving the last good output and reports the
problem through `last_error`.

## Incremental builds

Site builds now persist note-render state under `.vulcan/site-state/`. When the same profile writes
to the same `output_dir` and its planned routes stay stable, Vulcan reuses unchanged rendered note
pages instead of recompiling the whole published subset on every rebuild. `site serve --watch` uses
the same cache automatically, so the live preview loop benefits from the same incremental note-page
reuse as one-shot `site build`.

The current cache invalidates and rerenders:

- notes whose own source file changed
- notes whose resolved link or embed state changed after some other vault edit
- backlink, tag-listing, folder-listing, and embed dependents of changed notes
- notes with `dataview`, `tasks`, `.base`, or static `dataviewjs` output when other vault notes change
- notes that depend on changed `.base` files

Vulcan still falls back to a full note-page rerender when:

- vault config or site-profile config changes
- the published note set changes
- route planning changes for the selected notes
- site `content_transforms` are active

Aggregate outputs such as the home page, folder/tag listings, recent pages, manifests, and feeds
are still regenerated conservatively from the current rendered-note set. Copied assets now keep a
small per-profile metadata cache under `.vulcan/site-assets/`, so rebuilds can skip vault asset
copies when both the source file and the published output copy still match the last successful
build. The main incremental wins today are skipping unchanged note-page HTML compilation and
avoiding redundant large-asset rereads while keeping the overall publication output deterministic.

## Output shape

The builder currently emits a profile-scoped site with:

- note pages under `/notes/.../` or `/<deploy_path>/notes/.../`
- `index.html`
- folder listings
- tag listings
- recent notes
- `assets/route-manifest.json`
- `assets/navigation-tree.json`
- `assets/search-index.json` — note-level search documents plus BM25-style term postings for the
  built-in client-side full-text search; the index is derived from source note text so incremental
  rebuilds do not need to strip rendered HTML again
- `assets/graph.json` — published nodes/edges reused by the graph page, local graph modules, and
  frontend-bundle consumers
- `assets/hover-previews.json`
- `assets/recent-notes.json`
- `assets/related-notes.json`
- `sitemap.xml` when `base_url` is set
- `rss.xml` when RSS is enabled and `base_url` is set

The default theme now uses a Quartz-like three-region shell: a persistent left rail with search,
primary navigation, and a folder-note-aware explorer; a centered reading surface; and a right rail
for TOC, local graph, backlinks, and outgoing links. It includes `system` / `light` / `dark`
palette controls, a reader mode that hides most chrome, persisted rail/module state, keyboard-first
search (`/`), a skip link plus landmarked page shell, a global mobile-friendly search dialog backed
by a note-level BM25-style full-text index with prefix lookup, client-rendered local/global graph
views backed by the shared published graph asset, profile-scoped `extra_css` / `extra_js`, favicon
injection, and logo rendering from the site profile. The default desktop shell no longer uses a
wide top bar; search, palette, reader-mode, and panel controls live in the left rail, while a
compact mobile utility dock handles rail toggles on narrow screens. The default explorer now
behaves closer to Obsidian/Quartz: folder rows collapse by default, folder notes stay available via
an explicit `Open` affordance, and the built-in shell remembers rail, module, and folder state
across full-page navigations. When a deploy path is configured, the default shell, manifests, and
preview server all emit prefix-aware URLs so the built output can be hosted under that subpath
unchanged.

## Frontend bundle mode

External frontend tools should consume Vulcan's publication output, not replace its vault semantics.
The bundle flow therefore reuses an existing `site` profile instead of defining a second publish
configuration surface.

Example:

```toml
[site.profiles.public]
title = "Public Notes"
deploy_path = "/garden"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true

[export.profiles.public_bundle]
format = "frontend-bundle"
path = "exports/public-bundle"
site_profile = "public"
pretty = true
```

Core commands:

- `vulcan export profile run public_bundle`
- `vulcan export profile serve public_bundle --port 4174`

`export profile run` writes a deterministic bundle directory. `export profile serve` keeps rebuilding
that directory on watched changes and exposes local live-reload endpoints at:

- `/__vulcan_bundle/live-reload.json`
- `/__vulcan_bundle/live-reload.events`

The bundle layout is intentionally stable and typed. The root contract now carries shell, navigation,
and module defaults mirrored from the selected site profile, and the shared artifacts include the
published explorer tree used by the built-in shell:

- `frontend-bundle.json` — versioned root contract with profile metadata, note index entries, route
  information, diagnostics, and artifact paths
- `notes/.../index.json` — per-note documents with rendered `body_html`, headings, embeds, outgoing
  links, backlinks, breadcrumbs, and render diagnostics
- `assets/route-manifest.json`
- `assets/navigation-tree.json`
- `assets/hover-previews.json`
- `assets/recent-notes.json`
- `assets/related-notes.json`
- `assets/search-index.json` when search is enabled
- `assets/graph.json` when graph export is enabled
- `assets/invalidation.json`
- `schema/frontend-bundle.schema.json`
- `schema/frontend-bundle.d.ts`

One-shot bundle builds keep `assets/invalidation.json` deterministic so repeated builds of the same
vault/config stay byte-identical. During `export profile serve`, the JSON/SSE live-reload payload
carries the current `changed_files`, `deleted_files`, `changed_routes`, `deleted_routes`,
`changed_assets`, and `deleted_assets` sets so Vite/Astro/Next-style dev servers can trigger HMR or
full reloads without re-walking the bundle directory.

Reference example files live under `docs/examples/frontend-bundle-reference/`.

## Diagnostics and automation

Use `--output json` with `site profiles`, `site doctor`, and `site build` when another tool or LLM
needs structured results.

Use `vulcan site build --strict` in CI or release checks when publish warnings should fail the build.
That check runs through the same route planner, publication filters, and renderer as the real build.

Typical workflow:

1. `vulcan site profiles`
2. `vulcan site doctor --profile public`
3. `vulcan site build --profile public --strict`
4. `vulcan site serve --profile public --watch`
5. promote any repeated local-only preview tweaks into the shared site profile once they stabilize

## Deferred

The first static-site release intentionally does not try to solve everything. Still deferred:

- comments or auth-backed “private pages”
- analytics integrations
- stacked pages or SPA routing
- full browser-side DataviewJS parity
- arbitrary client-side plugin execution
