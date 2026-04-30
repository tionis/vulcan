# Static Site Theme Reference

Copy this directory into a vault as `.vulcan/site/themes/reference/` and set:

```toml
[site.profiles.public]
theme = "reference"
```

Theme directories can override a fixed set of shell regions without introducing a separate
templating runtime:

- `head.html` — appended inside `<head>` after Vulcan's built-in CSS/JS includes.
- `header.html` — replaces the default site header.
- `nav.html` — replaces the default nav region when `header.html` is not present.
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
- `{{theme_toggle}}`
- `{{site_logo}}`

`header.html` supersedes `nav.html`; otherwise `nav.html` can replace only the navigation strip
inside the default header. Extra per-profile `extra_css` / `extra_js` still load after theme assets,
so local preview tweaks can override the shared theme without editing the theme directory itself.
