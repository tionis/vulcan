# Static Site Theme Reference

Copy this directory into a vault as `.vulcan/site/themes/reference/` and set:

```toml
[site.profiles.public]
theme = "reference"
```

Theme directories can override a fixed set of shell regions without introducing a separate
templating runtime:

- `head.html` — appended inside `<head>` after Vulcan's built-in CSS/JS includes.
- `toolbar.html` — wraps or replaces the sticky top toolbar.
- `header.html` — replaces the default site header.
- `nav.html` — replaces the default nav region when `header.html` is not present.
- `left_rail.html` — wraps or replaces the default explorer/search rail.
- `right_rail.html` — wraps or replaces the default TOC/backlinks/graph rail.
- `footer.html` — replaces the default footer.
- `note_before.html` — inserted before rendered note bodies on note/home pages.
- `note_after.html` — inserted after rendered note bodies on note/home pages.
- `theme.css` — copied into the published assets and loaded after `assets/vulcan-site.css`.
- `theme.js` — copied into the published assets and loaded after `assets/vulcan-site.js`.

Supported placeholder tokens:

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

`theme_toggle` is kept as a compatibility alias for the built-in palette controls. `header.html`
still supersedes `nav.html`; otherwise `nav.html` can replace only the primary-nav strip inside the
default left rail. The new `toolbar.html`, `left_rail.html`, and `right_rail.html` partials receive
the built-in shell markup through `{{toolbar}}`, `{{left_rail}}`, and `{{right_rail}}` so a custom
theme can progressively wrap the defaults instead of rewriting everything at once.

Extra per-profile `extra_css` / `extra_js` still load after theme assets, so local preview tweaks
can override the shared theme without editing the theme directory itself.
