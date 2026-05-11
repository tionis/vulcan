pub(crate) const DEFAULT_THEME_CSS: &str = r"
:root {
  color-scheme: light dark;
  --bg: #faf8f3;
  --bg-strong: #f2efe7;
  --bg-elevated: rgba(250, 248, 243, 0.97);
  --surface: #f6f4ee;
  --surface-strong: #fffdf8;
  --surface-soft: rgba(39, 49, 61, 0.035);
  --text: #27313d;
  --muted: #617184;
  --accent: #356ca5;
  --accent-strong: #244f7a;
  --accent-soft: rgba(53, 108, 165, 0.12);
  --border: rgba(39, 49, 61, 0.1);
  --border-strong: rgba(39, 49, 61, 0.18);
  --shadow: 0 1px 2px rgba(39, 49, 61, 0.06), 0 8px 24px rgba(39, 49, 61, 0.04);
  --code-bg: rgba(39, 49, 61, 0.06);
  --link: #245d99;
  --left-rail-width: 20rem;
  --right-rail-width: 19.5rem;
  --content-max: 48rem;
  --radius-lg: 0.85rem;
  --radius-md: 0.65rem;
  --rail-gap: 1.4rem;
}

@media (prefers-color-scheme: dark) {
  :root {
    --bg: #16181d;
    --bg-strong: #111319;
    --bg-elevated: rgba(19, 22, 28, 0.96);
    --surface: #1b2028;
    --surface-strong: #202632;
    --surface-soft: rgba(255, 255, 255, 0.04);
    --text: #e8edf6;
    --muted: #a8b3c3;
    --accent: #8fc1ff;
    --accent-strong: #bdd8ff;
    --accent-soft: rgba(143, 193, 255, 0.14);
    --border: rgba(232, 237, 246, 0.1);
    --border-strong: rgba(232, 237, 246, 0.18);
    --shadow: 0 1px 2px rgba(0, 0, 0, 0.2), 0 12px 32px rgba(0, 0, 0, 0.22);
    --code-bg: rgba(232, 237, 246, 0.08);
    --link: #98c7ff;
  }
}

html[data-theme='light'] {
  color-scheme: light;
  --bg: #faf8f3;
  --bg-strong: #f2efe7;
  --bg-elevated: rgba(250, 248, 243, 0.97);
  --surface: #f6f4ee;
  --surface-strong: #fffdf8;
  --surface-soft: rgba(39, 49, 61, 0.035);
  --text: #27313d;
  --muted: #617184;
  --accent: #356ca5;
  --accent-strong: #244f7a;
  --accent-soft: rgba(53, 108, 165, 0.12);
  --border: rgba(39, 49, 61, 0.1);
  --border-strong: rgba(39, 49, 61, 0.18);
  --shadow: 0 1px 2px rgba(39, 49, 61, 0.06), 0 8px 24px rgba(39, 49, 61, 0.04);
  --code-bg: rgba(39, 49, 61, 0.06);
  --link: #245d99;
}

html[data-theme='dark'] {
  color-scheme: dark;
  --bg: #16181d;
  --bg-strong: #111319;
  --bg-elevated: rgba(19, 22, 28, 0.96);
  --surface: #1b2028;
  --surface-strong: #202632;
  --surface-soft: rgba(255, 255, 255, 0.04);
  --text: #e8edf6;
  --muted: #a8b3c3;
  --accent: #8fc1ff;
  --accent-strong: #bdd8ff;
  --accent-soft: rgba(143, 193, 255, 0.14);
  --border: rgba(232, 237, 246, 0.1);
  --border-strong: rgba(232, 237, 246, 0.18);
  --shadow: 0 1px 2px rgba(0, 0, 0, 0.2), 0 12px 32px rgba(0, 0, 0, 0.22);
  --code-bg: rgba(232, 237, 246, 0.08);
  --link: #98c7ff;
}

html { scroll-padding-top: 2rem; }
* { box-sizing: border-box; }

body {
  margin: 0;
  min-height: 100vh;
  font-family: 'IBM Plex Sans', 'Avenir Next', 'Segoe UI', sans-serif;
  background: var(--bg);
  color: var(--text);
  line-height: 1.55;
}

a {
  color: var(--link);
  text-decoration-thickness: 0.08em;
  text-underline-offset: 0.18em;
}

a:hover { color: var(--accent-strong); }

img, video {
  max-width: 100%;
  height: auto;
}

code, pre {
  font-family: 'IBM Plex Mono', 'SFMono-Regular', monospace;
  background: var(--code-bg);
}

pre {
  padding: 1rem 1.1rem;
  border-radius: var(--radius-md);
  overflow: auto;
  border: 1px solid var(--border);
}

.site-shell {
  max-width: 1680px;
  margin: 0 auto;
  padding: 0 1.25rem 3rem;
}

.site-skip-link {
  position: absolute;
  left: 1rem;
  top: 0.8rem;
  padding: 0.65rem 0.9rem;
  border-radius: 999px;
  background: var(--accent);
  color: #fff;
  text-decoration: none;
  transform: translateY(-180%);
  transition: transform 0.18s ease;
  z-index: 10000;
}

.site-skip-link:focus { transform: translateY(0); }

.site-brand-card,
.site-listing,
.site-search-card,
.site-graph-card,
.site-panel,
.site-search-dialog-panel,
.site-mobile-dock {
  background: var(--surface-strong);
  border: 1px solid var(--border);
  box-shadow: var(--shadow);
}

.site-mobile-dock {
  display: none;
  position: fixed;
  left: 50%;
  bottom: 1rem;
  z-index: 80;
  gap: 0.5rem;
  align-items: center;
  justify-content: center;
  padding: 0.55rem;
  border-radius: 999px;
  backdrop-filter: blur(12px);
  transform: translateX(-50%);
}

.site-brand-mark {
  width: 2.1rem;
  height: 2.1rem;
  border-radius: 0.65rem;
  object-fit: cover;
  background: rgba(255, 255, 255, 0.4);
  border: 1px solid var(--border);
}

.site-toolbar-actions,
.site-rail-controls,
.site-module-toolbar,
.site-palette-group {
  display: flex;
  gap: 0.45rem;
  align-items: center;
  flex-wrap: wrap;
}

.site-control-button,
.site-palette-button,
.site-module-toggle,
.site-toolbar-toggle,
.site-search-launch,
.site-search-dialog-header button,
.site-explorer-folder-toggle,
.site-explorer-folder-label,
.site-panel-heading {
  appearance: none;
  border: 1px solid var(--border);
  background: var(--surface-strong);
  color: var(--text);
  border-radius: 0.8rem;
  padding: 0.48rem 0.7rem;
  text-decoration: none;
  cursor: pointer;
  font: inherit;
  font-size: 0.9rem;
  line-height: 1.3;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  transition: border-color 0.18s ease, background 0.18s ease, color 0.18s ease, transform 0.18s ease;
}

.site-control-button:hover,
.site-palette-button:hover,
.site-module-toggle:hover,
.site-toolbar-toggle:hover,
.site-search-launch:hover,
.site-search-dialog-header button:hover,
.site-explorer-folder-toggle:hover,
.site-explorer-folder-label:hover,
.site-panel-heading:hover {
  border-color: var(--border-strong);
  background: var(--surface-soft);
}

.site-palette-button[aria-pressed='true'],
.site-module-toggle[aria-pressed='true'],
.site-control-button[aria-pressed='true'],
.site-toolbar-toggle[aria-expanded='true'] {
  background: var(--accent-soft);
  border-color: var(--accent);
  color: var(--accent-strong);
}

.site-layout {
  display: grid;
  gap: var(--rail-gap);
  grid-template-columns: minmax(15rem, var(--left-rail-width)) minmax(0, var(--content-max)) minmax(15rem, var(--right-rail-width));
  align-items: start;
  justify-content: center;
}

body[data-left-rail-state='closed'][data-right-rail-state='closed'] .site-layout,
body[data-left-rail-enabled='false'][data-right-rail-enabled='false'] .site-layout,
body[data-reader-mode='true'] .site-layout {
  grid-template-columns: minmax(0, 1fr);
}

body[data-left-rail-enabled='false'][data-right-rail-enabled='true'][data-right-rail-state='open'] .site-layout,
body[data-left-rail-state='closed'][data-right-rail-enabled='true'][data-right-rail-state='open'] .site-layout {
  grid-template-columns: minmax(0, 1fr) var(--right-rail-width);
}

body[data-left-rail-enabled='true'][data-left-rail-state='open'][data-right-rail-enabled='false'] .site-layout,
body[data-left-rail-enabled='true'][data-left-rail-state='open'][data-right-rail-state='closed'] .site-layout {
  grid-template-columns: var(--left-rail-width) minmax(0, 1fr);
}

.site-left-rail,
.site-right-rail {
  position: sticky;
  top: 0;
  align-self: start;
  max-height: 100vh;
  overflow: auto;
  padding: 0;
  border: 0;
  background: transparent;
  box-shadow: none;
}

.site-left-rail.is-disabled,
.site-right-rail.is-disabled,
body[data-left-rail-state='closed'] .site-left-rail,
body[data-right-rail-state='closed'] .site-right-rail,
body[data-reader-mode='true'] .site-left-rail,
body[data-reader-mode='true'] .site-right-rail {
  display: none;
}

.site-content { min-width: 0; }

.site-main,
.site-listing,
.site-search-card,
.site-graph-card,
.site-panel,
.site-brand-card,
.site-mobile-dock {
  border-radius: var(--radius-lg);
}

.site-main {
  max-width: 100%;
  margin: 0;
  padding: 2.5rem 0 2rem;
  background: transparent;
  border: 0;
  box-shadow: none;
}

.site-main > :first-child { margin-top: 0; }
.site-main > :last-child { margin-bottom: 0; }

.site-main h1,
.site-main h2,
.site-main h3,
.site-main h4,
.site-main h5,
.site-main h6 {
  font-family: 'IBM Plex Sans', 'Avenir Next', 'Segoe UI', sans-serif;
  line-height: 1.18;
  letter-spacing: -0.015em;
}

.site-main p,
.site-main li,
.site-main blockquote {
  font-size: 0.98rem;
}

.site-breadcrumbs,
.site-meta,
.site-footer,
.site-empty {
  color: var(--muted);
}

.site-breadcrumbs {
  display: flex;
  gap: 0.45rem;
  flex-wrap: wrap;
  font-size: 0.95rem;
  margin-bottom: 1rem;
}

.site-brand-card,
.site-panel,
.site-listing,
.site-search-card,
.site-graph-card {
  padding: 0.9rem 1rem;
}

.site-brand-card {
  display: flex;
  gap: 0.8rem;
  align-items: center;
  padding: 0;
  background: transparent;
  border: 0;
  box-shadow: none;
}

.site-brand-title {
  margin: 0;
  font-size: 1rem;
  font-weight: 700;
  line-height: 1.25;
}

.site-brand-title a {
  color: inherit;
  text-decoration: none;
}

.site-rail-shell {
  display: grid;
  gap: 1rem;
  padding: 2.5rem 0 1.5rem;
}

.site-rail-header {
  display: grid;
  gap: 0.7rem;
}

.site-rail-controls {
  display: grid;
  gap: 0.65rem;
}

.site-rail-button-row {
  display: flex;
  gap: 0.45rem;
  flex-wrap: wrap;
}

.site-search-launch {
  justify-content: flex-start;
  width: 100%;
}

.site-palette-group {
  width: 100%;
}

.site-palette-button {
  flex: 1 1 0;
}

.site-mobile-dock .site-search-launch,
.site-mobile-dock .site-control-button {
  width: auto;
}

.site-primary-nav {
  display: grid;
  gap: 0.15rem;
}

.site-nav-link {
  display: block;
  padding: 0.42rem 0.55rem;
  border-radius: 0.55rem;
  color: inherit;
  text-decoration: none;
  border: 1px solid transparent;
  font-size: 0.92rem;
}

.site-nav-link:hover {
  background: var(--accent-soft);
  border-color: var(--border-strong);
}

.site-rail-section-title {
  font-size: 0.74rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
  margin-bottom: 0.45rem;
}

.site-explorer-panel {
  padding: 0;
  border: 0;
  border-radius: 0;
  background: transparent;
}

.site-explorer-tree,
.site-explorer-children,
.site-panel-list,
.site-local-graph-list,
.site-search-results {
  list-style: none;
  padding: 0;
  margin: 0;
}

.site-explorer-tree,
.site-explorer-children {
  display: grid;
  gap: 0;
}

.site-explorer-children {
  margin-left: 0.95rem;
  padding-left: 0.65rem;
  border-left: 1px solid var(--border);
}

.site-explorer-folder-row {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  gap: 0.15rem;
  align-items: center;
}

.site-explorer-folder-toggle {
  width: 1.1rem;
  min-width: 1.1rem;
  padding: 0.15rem 0;
  border: 0;
  background: transparent;
  color: var(--muted);
  border-radius: 0.4rem;
  font-size: 0.76rem;
}

.site-explorer-folder-toggle[aria-expanded='true'] {
  transform: rotate(90deg);
}

.site-explorer-folder-open,
.site-explorer-folder-link,
.site-explorer-link,
.site-explorer-folder-label {
  min-width: 0;
  display: block;
  padding: 0.3rem 0.45rem;
  border-radius: 0.5rem;
  color: inherit;
  text-decoration: none;
  font-size: 0.9rem;
  line-height: 1.35;
}

.site-explorer-folder-label {
  border: 0;
  background: transparent;
  text-align: left;
}

.site-explorer-folder-open {
  padding-inline: 0.5rem;
  font-size: 0.78rem;
  color: var(--muted);
}

.site-explorer-folder-link:hover,
.site-explorer-link:hover,
.site-explorer-folder-open:hover,
.site-explorer-folder-label:hover,
.site-explorer-link.is-active,
.site-explorer-folder-link.is-active,
.site-explorer-folder-open.is-active,
.site-explorer-folder-label.is-active {
  background: var(--accent-soft);
}

.site-module-toolbar {
  position: sticky;
  top: 0;
  z-index: 2;
  margin-bottom: 0.8rem;
  padding-bottom: 0.4rem;
  background: var(--bg);
}

.site-panel {
  overflow: hidden;
  margin-bottom: 0.75rem;
}

.site-panel.is-hidden { display: none; }

.site-panel-heading {
  width: 100%;
  border-radius: 0;
  border: 0;
  border-bottom: 1px solid var(--border);
  background: transparent;
  padding: 0.68rem 0.82rem;
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-weight: 700;
}

.site-panel.is-collapsed .site-panel-body { display: none; }
.site-panel.is-collapsed .site-panel-chevron { transform: rotate(-90deg); }

.site-panel-body {
  padding: 0.75rem 0.82rem 0.85rem;
}

.site-panel-list {
  display: grid;
  gap: 0.65rem;
}

.site-panel-list li {
  padding-bottom: 0.65rem;
  border-bottom: 1px solid var(--border);
}

.site-panel-list li:last-child {
  border-bottom: 0;
  padding-bottom: 0;
}

.site-listing,
.site-search-card,
.site-graph-card {
  max-width: min(100%, var(--content-max));
  margin: 0 auto;
}

.site-card-grid {
  display: grid;
  gap: 1rem;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
}

.site-card {
  border: 1px solid var(--border);
  border-radius: 0.9rem;
  padding: 0.95rem;
  background: var(--surface);
}

.site-card h3 {
  margin-top: 0;
  margin-bottom: 0.6rem;
}

.site-card p {
  margin: 0;
  color: var(--muted);
}

.site-explorer-filter {
  width: 100%;
  margin-bottom: 0.75rem;
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 0.45rem 0.7rem;
  background: var(--surface);
  color: var(--text);
}

.site-listing-hero {
  display: grid;
  gap: 0.8rem;
  padding-bottom: 1.25rem;
  margin-bottom: 1.25rem;
  border-bottom: 1px solid var(--border);
}

.site-listing-kicker {
  margin: 0;
  color: var(--muted);
  font-size: 0.82rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
}

.site-listing-hero h2 {
  margin: 0;
  font-size: clamp(1.35rem, 2.4vw, 2rem);
}

.site-listing-meta {
  display: flex;
  gap: 0.5rem;
  flex-wrap: wrap;
  color: var(--muted);
  font-size: 0.92rem;
}

.site-listing-stat {
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 0.25rem 0.65rem;
  background: var(--surface);
}

.site-search-input {
  width: 100%;
  padding: 0.82rem 0.95rem;
  border-radius: 0.8rem;
  border: 1px solid var(--border);
  background: var(--surface-strong);
  color: var(--text);
  font: inherit;
}

.site-search-dialog[hidden] { display: none; }

.site-search-dialog {
  position: fixed;
  inset: 0;
  z-index: 9998;
  padding: 1.1rem;
  background: rgba(16, 18, 19, 0.48);
  backdrop-filter: blur(12px);
}

.site-search-dialog-panel {
  max-width: 46rem;
  margin: 4rem auto 0;
  border-radius: var(--radius-lg);
  padding: 1rem;
  background: var(--bg-elevated);
}

.site-search-dialog-header {
  display: flex;
  gap: 1rem;
  align-items: center;
  justify-content: space-between;
}

.site-search-results {
  margin-top: 1rem;
  display: grid;
  gap: 0.8rem;
}

.site-search-results li {
  border: 1px solid var(--border);
  border-radius: 0.8rem;
  padding: 0.85rem 0.95rem;
  background: var(--surface);
}

.site-search-results a {
  font-weight: 700;
  text-decoration: none;
}

.site-search-results mark {
  background: var(--accent-soft);
  color: inherit;
  padding: 0 0.15em;
  border-radius: 0.2rem;
}

.site-graph-stage {
  position: relative;
  min-height: 14rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface-soft);
  overflow: hidden;
}

.site-graph-stage.is-global {
  min-height: 34rem;
}

.site-graph-stage svg {
  display: block;
  width: 100%;
  height: 100%;
}

.site-graph-empty,
.site-graph-caption {
  color: var(--muted);
  font-size: 0.88rem;
}

.site-graph-empty {
  margin: 0;
  padding: 0.85rem 1rem;
}

.site-graph-caption {
  margin: 0.7rem 0 0;
}

.site-graph-edge {
  stroke: var(--border-strong);
  stroke-width: 1.1;
  stroke-opacity: 0.88;
}

.site-graph-node circle {
  fill: var(--surface-strong);
  stroke: var(--accent);
  stroke-width: 1.4;
}

.site-graph-node.is-current circle {
  fill: var(--accent);
  stroke: var(--accent-strong);
}

.site-graph-node text {
  fill: var(--text);
  font-size: 11px;
  paint-order: stroke;
  stroke: var(--bg-elevated);
  stroke-width: 3px;
  stroke-linejoin: round;
}

.site-graph-node.is-current text {
  fill: #fff;
  font-weight: 700;
}

.site-visually-hidden {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

.site-pill-list {
  display: flex;
  gap: 0.5rem;
  flex-wrap: wrap;
  list-style: none;
  padding: 0;
  margin: 0 0 1rem;
}

.site-pill-list a {
  display: inline-flex;
  border: 1px solid var(--border);
  padding: 0.35rem 0.75rem;
  border-radius: 999px;
  text-decoration: none;
  background: var(--surface-soft);
}

.site-inline-nav {
  display: flex;
  gap: 0.8rem;
  justify-content: space-between;
  margin-top: 2rem;
  flex-wrap: wrap;
}

.math.math-inline { white-space: nowrap; }
.math.math-display { display: block; overflow-x: auto; padding: 0.5rem 0; }

.site-main input[type='checkbox'] {
  width: 0.95rem;
  height: 0.95rem;
  margin-right: 0.55rem;
  accent-color: var(--accent);
  transform: translateY(0.08rem);
}

.site-main li:has(> input[type='checkbox']) {
  list-style: none;
  padding-left: 0;
}

.site-main li:has(> input[type='checkbox']:checked) {
  color: var(--muted);
  text-decoration: line-through;
  text-decoration-color: var(--border-strong);
}

.site-callout {
  margin-top: 1rem;
  border-left: 3px solid var(--accent);
  padding: 0.7rem 0.9rem;
  border-radius: 0.7rem;
  background: var(--surface);
  color: var(--muted);
}

.site-diagnostics {
  margin-top: 1rem;
  border-radius: 1rem;
  border: 1px solid rgba(178, 69, 54, 0.35);
  background: rgba(178, 69, 54, 0.08);
  padding: 1rem;
}

.site-footer {
  margin-top: 1.4rem;
  text-align: center;
  font-size: 0.95rem;
}

.site-live-overlay {
  position: fixed;
  right: 1rem;
  bottom: 1rem;
  z-index: 9999;
  background: rgba(20, 20, 20, 0.92);
  color: #fff;
  padding: 0.9rem 1rem;
  border-radius: 1rem;
  max-width: 24rem;
  box-shadow: 0 18px 36px rgba(0, 0, 0, 0.35);
}

body[data-reader-mode='true'] .site-footer,
body[data-reader-mode='true'] .site-primary-nav,
body[data-reader-mode='true'] .site-explorer-panel,
body[data-reader-mode='true'] .site-mobile-dock,
body[data-reader-mode='true'] [data-site-search-open] {
  display: none !important;
}

body[data-reader-mode='true'] .site-main {
  padding-top: 1rem;
}

body[data-reader-mode='true'] .site-content {
  width: min(100%, 46rem);
  margin: 0 auto;
}

body[data-reader-mode='true'] .site-main p,
body[data-reader-mode='true'] .site-main li {
  font-size: 1.02rem;
  line-height: 1.72;
}

a:focus-visible,
button:focus-visible,
input:focus-visible {
  outline: 2px solid var(--accent-strong);
  outline-offset: 3px;
}

@media (max-width: 1240px) {
  :root {
    --left-rail-width: 17.25rem;
    --right-rail-width: 17rem;
  }
}

@media (max-width: 960px) {
  .site-shell { padding-inline: 0.8rem; }

  .site-mobile-dock {
    display: flex;
  }

  .site-layout {
    grid-template-columns: 1fr !important;
  }

  .site-left-rail,
  .site-right-rail {
    display: block;
    position: fixed;
    top: 0;
    bottom: 0;
    width: min(18rem, calc(100vw - 1.5rem));
    max-height: none;
    padding: 0 0.8rem;
    background: var(--bg-elevated);
    box-shadow: 0 28px 60px rgba(0, 0, 0, 0.28);
    overflow: auto;
    z-index: 60;
    transition: transform 0.22s ease, opacity 0.22s ease;
    opacity: 0;
    pointer-events: none;
  }

  .site-left-rail {
    left: 0;
    transform: translateX(-110%);
  }

  .site-right-rail {
    right: 0;
    transform: translateX(110%);
  }

  body[data-left-rail-state='open'] .site-left-rail,
  body[data-right-rail-state='open'] .site-right-rail {
    opacity: 1;
    pointer-events: auto;
    transform: translateX(0);
  }

  .site-search-dialog { padding: 0.6rem; }

  .site-search-dialog-panel {
    margin-top: 0;
    min-height: calc(100vh - 1.2rem);
  }
}
";

pub(crate) const DEFAULT_THEME_JS: &str = r#"(() => {
  const root = document.documentElement;
  const body = document.body;
  const profileScope = `${body.dataset.siteProfile || 'site'}:${body.dataset.siteDeployPath || '/'}`;
  const storageKey = (name) => `vulcan-site:${profileScope}:${name}`;
  const readStorage = (key) => {
    try {
      return localStorage.getItem(key);
    } catch (_) {
      return null;
    }
  };
  const writeStorage = (key, value) => {
    try {
      localStorage.setItem(key, value);
    } catch (_) {}
  };
  const readJsonStorage = (key, fallback) => {
    const value = readStorage(key);
    if (!value) return fallback;
    try {
      return JSON.parse(value);
    } catch (_) {
      return fallback;
    }
  };

  const escapeHtml = (value) =>
    String(value).replace(/[&<>"']/g, (char) => ({
      '&': '&amp;',
      '<': '&lt;',
      '>': '&gt;',
      '"': '&quot;',
      "'": '&#39;',
    })[char] ?? char);
  const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const highlightText = (value, query) => {
    const terms = query.trim().split(/\s+/).filter(Boolean).map(escapeRegExp);
    if (!terms.length) return escapeHtml(value);
    const regex = new RegExp(`(${terms.join('|')})`, 'ig');
    return escapeHtml(value).replace(regex, '<mark>$1</mark>');
  };

  const prefersDark = window.matchMedia?.('(prefers-color-scheme: dark)');
  const prefersCompactLayout = window.matchMedia?.('(max-width: 960px)');
  const resolveTheme = (mode) =>
    mode === 'system' ? (prefersDark?.matches ? 'dark' : 'light') : mode;
  const syncThemeButtons = () => {
    const activeMode = root.dataset.themeMode || 'system';
    document.querySelectorAll('[data-theme-mode]').forEach((button) => {
      button.setAttribute(
        'aria-pressed',
        button.getAttribute('data-theme-mode') === activeMode ? 'true' : 'false'
      );
    });
  };
  const applyThemeMode = (mode, persist) => {
    root.dataset.themeMode = mode;
    root.dataset.theme = resolveTheme(mode);
    syncThemeButtons();
    if (persist) writeStorage(storageKey('theme-mode'), mode);
  };
  applyThemeMode(readStorage(storageKey('theme-mode')) || body.dataset.defaultPalette || 'system', false);
  if (prefersDark) {
    const syncSystemTheme = () => {
      if ((root.dataset.themeMode || 'system') === 'system') {
        applyThemeMode('system', false);
      }
    };
    if (prefersDark.addEventListener) prefersDark.addEventListener('change', syncSystemTheme);
    else if (prefersDark.addListener) prefersDark.addListener(syncSystemTheme);
  }

  // Persist shell state per profile/deploy path so navigation, modules, and reader mode survive
  // full-page navigations and live-preview reloads.
  const railEnabled = {
    left: body.dataset.leftRailEnabled === 'true',
    right: body.dataset.rightRailEnabled === 'true',
  };
  const railDatasetKey = (side) => (side === 'left' ? 'leftRailState' : 'rightRailState');
  const syncRailButtons = (side) => {
    const open = body.dataset[railDatasetKey(side)] === 'open';
    document.querySelectorAll(`[data-site-rail-toggle="${side}"]`).forEach((button) => {
      button.setAttribute('aria-expanded', open ? 'true' : 'false');
    });
  };
  const setRailState = (side, state, persist) => {
    body.dataset[railDatasetKey(side)] = railEnabled[side] ? state : 'closed';
    syncRailButtons(side);
    if (persist && railEnabled[side]) {
      writeStorage(storageKey(`rail:${side}`), body.dataset[railDatasetKey(side)]);
    }
  };
  const defaultRailState = (side) =>
    railEnabled[side] ? (prefersCompactLayout?.matches ? 'closed' : 'open') : 'closed';
  setRailState('left', readStorage(storageKey('rail:left')) || defaultRailState('left'), false);
  setRailState('right', readStorage(storageKey('rail:right')) || defaultRailState('right'), false);

  const readerModeAvailable = body.dataset.readerModeEnabled === 'true';
  const syncReaderModeButtons = () => {
    const active = body.dataset.readerMode === 'true';
    document.querySelectorAll('[data-reader-mode-toggle]').forEach((button) => {
      button.setAttribute('aria-pressed', active ? 'true' : 'false');
    });
  };
  const setReaderMode = (active, persist) => {
    body.dataset.readerMode = active && readerModeAvailable ? 'true' : 'false';
    syncReaderModeButtons();
    if (persist && readerModeAvailable) {
      writeStorage(storageKey('reader-mode'), body.dataset.readerMode);
    }
  };
  setReaderMode(readStorage(storageKey('reader-mode')) === 'true', false);

  const moduleButtons = new Map(
    [...document.querySelectorAll('[data-site-module-toggle]')].map((button) => [
      button.getAttribute('data-site-module-toggle'),
      button,
    ])
  );
  const modulePanels = new Map(
    [...document.querySelectorAll('[data-site-module]')].map((panel) => [
      panel.getAttribute('data-site-module'),
      panel,
    ])
  );
  let moduleState = readJsonStorage(storageKey('modules'), { hidden: {}, collapsed: {} });
  const persistModuleState = () => writeStorage(storageKey('modules'), JSON.stringify(moduleState));
  const applyModuleState = (id) => {
    const panel = modulePanels.get(id);
    const button = moduleButtons.get(id);
    const hidden = Boolean(moduleState.hidden?.[id]);
    const collapsed = !hidden && Boolean(moduleState.collapsed?.[id]);
    if (panel) {
      panel.classList.toggle('is-hidden', hidden);
      panel.classList.toggle('is-collapsed', collapsed);
      const heading = panel.querySelector(`[data-site-panel-toggle="${id}"]`);
      if (heading) {
        heading.setAttribute('aria-expanded', hidden ? 'false' : collapsed ? 'false' : 'true');
      }
    }
    if (button) button.setAttribute('aria-pressed', hidden ? 'false' : 'true');
  };
  for (const id of modulePanels.keys()) applyModuleState(id);

  const explorerTree = document.querySelector('.site-explorer-tree');
  const explorerUsesSavedState = explorerTree?.dataset.siteSavedState === 'true';
  const explorerFolderClick = explorerTree?.dataset.siteFolderClick || 'link';
  const explorerFolderToggles = new Map(
    [...document.querySelectorAll('[data-site-explorer-folder-toggle]')].map((toggle) => [
      toggle.getAttribute('data-site-explorer-folder-toggle'),
      toggle,
    ])
  );
  const explorerFolderBodies = new Map(
    [...document.querySelectorAll('[data-site-explorer-folder-body]')].map((panel) => [
      panel.getAttribute('data-site-explorer-folder-body'),
      panel,
    ])
  );
  let explorerState = readJsonStorage(storageKey('explorer-folders'), {});
  const persistExplorerState = () => {
    if (!explorerUsesSavedState) return;
    writeStorage(storageKey('explorer-folders'), JSON.stringify(explorerState));
  };
  const setExplorerFolderOpen = (folderPath, open, persist) => {
    const toggle = explorerFolderToggles.get(folderPath);
    const bodyNode = explorerFolderBodies.get(folderPath);
    if (!toggle || !bodyNode) return;
    toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
    bodyNode.hidden = !open;
    if (persist && explorerUsesSavedState) {
      explorerState[folderPath] = open;
      persistExplorerState();
    }
  };
  if (explorerTree) {
    explorerTree.querySelectorAll('[data-site-explorer-folder-toggle]').forEach((toggle) => {
      const folderPath = toggle.getAttribute('data-site-explorer-folder-toggle');
      if (!folderPath) return;
      const saved = explorerUsesSavedState ? explorerState[folderPath] : undefined;
      const expanded = typeof saved === 'boolean' ? saved : toggle.getAttribute('aria-expanded') === 'true';
      setExplorerFolderOpen(folderPath, expanded, false);
    });
    const savedScroll = Number(readStorage(storageKey('explorer-scroll')) || '0');
    if (savedScroll > 0) explorerTree.scrollTop = savedScroll;
    explorerTree.addEventListener('scroll', () => {
      if (explorerUsesSavedState) writeStorage(storageKey('explorer-scroll'), String(explorerTree.scrollTop));
    }, { passive: true });
  }

  const explorerFilter = document.querySelector('[data-site-explorer-filter]');
  const applyExplorerFilter = (query) => {
    if (!explorerTree) return;
    const needle = query.trim().toLocaleLowerCase();
    explorerTree.querySelectorAll('[data-site-explorer-filter-text]').forEach((item) => {
      item.hidden = Boolean(needle) && !item.getAttribute('data-site-explorer-filter-text').includes(needle);
    });
    explorerTree.querySelectorAll('[data-site-explorer-folder]').forEach((folder) => {
      const folderMatches = folder.getAttribute('data-site-explorer-filter-text')?.includes(needle);
      const matchingChildren = [...folder.querySelectorAll('[data-site-explorer-filter-text]')]
        .some((item) => !item.hidden);
      folder.hidden = Boolean(needle) && !folderMatches && !matchingChildren;
    });
  };
  if (explorerFilter) {
    explorerFilter.addEventListener('input', () => applyExplorerFilter(explorerFilter.value));
  }

  document.addEventListener('click', (event) => {
    const paletteButton = event.target.closest('[data-theme-mode]');
    if (paletteButton) {
      applyThemeMode(paletteButton.getAttribute('data-theme-mode') || 'system', true);
      return;
    }

    const railButton = event.target.closest('[data-site-rail-toggle]');
    if (railButton) {
      const side = railButton.getAttribute('data-site-rail-toggle');
      if (!side || !railEnabled[side]) return;
      const open = body.dataset[railDatasetKey(side)] === 'open';
      setRailState(side, open ? 'closed' : 'open', true);
      return;
    }

    const readerButton = event.target.closest('[data-reader-mode-toggle]');
    if (readerButton && readerModeAvailable) {
      setReaderMode(body.dataset.readerMode !== 'true', true);
      return;
    }

    const moduleButton = event.target.closest('[data-site-module-toggle]');
    if (moduleButton) {
      const id = moduleButton.getAttribute('data-site-module-toggle');
      if (!id || !modulePanels.has(id)) return;
      moduleState.hidden[id] = !Boolean(moduleState.hidden?.[id]);
      if (moduleState.hidden[id]) moduleState.collapsed[id] = false;
      applyModuleState(id);
      persistModuleState();
      return;
    }

    const panelToggle = event.target.closest('[data-site-panel-toggle]');
    if (panelToggle) {
      const id = panelToggle.getAttribute('data-site-panel-toggle');
      if (!id || !modulePanels.has(id) || moduleState.hidden?.[id]) return;
      moduleState.collapsed[id] = !Boolean(moduleState.collapsed?.[id]);
      applyModuleState(id);
      persistModuleState();
      return;
    }

    const folderToggle = event.target.closest('[data-site-explorer-folder-toggle]');
    if (folderToggle) {
      const folderPath = folderToggle.getAttribute('data-site-explorer-folder-toggle');
      if (!folderPath) return;
      const open = folderToggle.getAttribute('aria-expanded') === 'true';
      setExplorerFolderOpen(folderPath, !open, true);
      return;
    }

    const folderLabel = event.target.closest('[data-site-explorer-folder-label]');
    if (folderLabel && explorerFolderClick === 'collapse') {
      const folderPath = folderLabel.getAttribute('data-site-explorer-folder-label');
      if (!folderPath) return;
      const toggle = explorerFolderToggles.get(folderPath);
      const open = toggle?.getAttribute('aria-expanded') === 'true';
      setExplorerFolderOpen(folderPath, !open, true);
    }
  });

  const searchDialog = document.querySelector('[data-site-search-dialog]');
  const searchInput = searchDialog?.querySelector('[data-site-search-input]');
  const searchResults = searchDialog?.querySelector('[data-site-search-results]');
  const searchAsset = body.dataset.searchAsset;
  const openSearch = () => {
    if (!searchDialog || !searchInput) return;
    searchDialog.hidden = false;
    searchInput.focus();
    searchInput.select();
  };
  const closeSearch = () => {
    if (!searchDialog) return;
    searchDialog.hidden = true;
  };
  if (searchDialog) {
    document.querySelectorAll('[data-site-search-open]').forEach((button) => {
      button.addEventListener('click', openSearch);
    });
    document.querySelectorAll('[data-site-search-close]').forEach((button) => {
      button.addEventListener('click', closeSearch);
    });
    searchDialog.addEventListener('click', (event) => {
      if (event.target === searchDialog) closeSearch();
    });
  }
  const isSearchTokenChar = (char) => /[0-9]/.test(char) || char.toLowerCase() !== char.toUpperCase();
  const tokenizeSearchQuery = (value) => {
    const tokens = [];
    let current = '';
    for (const char of String(value).toLowerCase()) {
      if (isSearchTokenChar(char)) current += char;
      else if (current) {
        tokens.push(current);
        current = '';
      }
    }
    if (current) tokens.push(current);
    return tokens;
  };
  if (searchInput && searchResults && searchAsset) {
    let searchDocuments = [];
    let searchDocumentsById = new Map();
    let searchTerms = {};
    let searchTermList = [];
    let averageLength = 1;
    let searchReady = false;
    let searchFailed = false;
    fetch(searchAsset)
      .then((response) => (response.ok ? response.json() : null))
      .then((payload) => {
        searchDocuments = payload?.documents || [];
        searchDocumentsById = new Map(searchDocuments.map((document) => [document.id, document]));
        searchTerms = payload?.terms || {};
        searchTermList = Object.keys(searchTerms).sort();
        averageLength = payload?.average_length || 1;
        searchReady = true;
        renderSearchResults();
      })
      .catch(() => {
        searchFailed = true;
        renderSearchResults();
      });
    const searchHits = (query) => {
      const tokens = tokenizeSearchQuery(query);
      if (!tokens.length || !searchDocuments.length) return [];
      const lowerBound = (prefix) => {
        let low = 0;
        let high = searchTermList.length;
        while (low < high) {
          const middle = Math.floor((low + high) / 2);
          if (searchTermList[middle] < prefix) low = middle + 1;
          else high = middle;
        }
        return low;
      };
      const prefixMatches = new Map();
      const termsForToken = (token) => {
        if (searchTerms[token]) return [token];
        if (prefixMatches.has(token)) return prefixMatches.get(token);
        const matches = [];
        for (let index = lowerBound(token); index < searchTermList.length; index += 1) {
          const term = searchTermList[index];
          if (!term.startsWith(token) || matches.length >= 32) break;
          matches.push(term);
        }
        prefixMatches.set(token, matches);
        return matches;
      };
      const scores = new Map();
      const matchCounts = new Map();
      for (const token of tokens) {
        const candidateTerms = termsForToken(token);
        const matchedDocuments = new Set();
        for (const term of candidateTerms) {
          const postings = searchTerms[term];
          if (!Array.isArray(postings) || !postings.length) continue;
          const idf = Math.log(
            1 + (searchDocuments.length - postings.length + 0.5) / (postings.length + 0.5)
          );
          for (const posting of postings) {
            const document = searchDocumentsById.get(posting.id);
            if (!document) continue;
            const tf = Math.max(1, posting.tf || 1);
            const lengthRatio = averageLength > 0 ? document.length / averageLength : 1;
            const score =
              idf * ((tf * 2.2) / (tf + 1.2 * (0.25 + 0.75 * lengthRatio)));
            scores.set(posting.id, (scores.get(posting.id) || 0) + score);
            matchedDocuments.add(posting.id);
          }
        }
        for (const documentId of matchedDocuments) {
          matchCounts.set(documentId, (matchCounts.get(documentId) || 0) + 1);
        }
      }
      return [...scores.entries()]
        .sort((left, right) => {
          const rightMatches = matchCounts.get(right[0]) || 0;
          const leftMatches = matchCounts.get(left[0]) || 0;
          if (rightMatches !== leftMatches) return rightMatches - leftMatches;
          if (right[1] !== left[1]) return right[1] - left[1];
          const rightDocument = searchDocumentsById.get(right[0]);
          const leftDocument = searchDocumentsById.get(left[0]);
          return (leftDocument?.title || '').localeCompare(rightDocument?.title || '');
        })
        .slice(0, 20)
        .map(([documentId]) => searchDocumentsById.get(documentId))
        .filter(Boolean);
    };
    const renderSearchResults = () => {
      const rawQuery = searchInput.value.trim();
      if (!rawQuery) {
        searchResults.innerHTML = '<li>Type to search the published site.</li>';
        return;
      }
      if (!searchReady) {
        searchResults.innerHTML = searchFailed
          ? '<li>Search index could not be loaded.</li>'
          : '<li>Loading search index…</li>';
        return;
      }
      const hits = searchHits(rawQuery);
      if (!hits.length) {
        searchResults.innerHTML = '<li>No matching notes in the published subset.</li>';
        return;
      }
      searchResults.innerHTML = hits
        .map(
          (hit) =>
            `<li><a href="${hit.url}">${highlightText(hit.title, rawQuery)}</a><div>${highlightText(hit.excerpt || hit.preview || '', rawQuery)}</div></li>`
        )
        .join('');
    };
    searchInput.addEventListener('input', renderSearchResults);
    renderSearchResults();
  }

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      if (searchDialog && !searchDialog.hidden) {
        event.preventDefault();
        closeSearch();
        return;
      }
      if (prefersCompactLayout?.matches) {
        if (body.dataset.leftRailState === 'open') setRailState('left', 'closed', true);
        if (body.dataset.rightRailState === 'open') setRailState('right', 'closed', true);
      }
      return;
    }
    if (event.key === '/' && !event.metaKey && !event.ctrlKey && !event.altKey) {
      if (document.activeElement && /input|textarea/i.test(document.activeElement.tagName)) return;
      if (!searchDialog) return;
      event.preventDefault();
      openSearch();
    }
  });

  const graphAsset = body.dataset.graphAsset;
  const graphCanvases = [...document.querySelectorAll('[data-site-graph-canvas]')];
  const clamp = (value, min, max) => Math.min(Math.max(value, min), max);
  const degreesForGraph = (nodes, edges) => {
    const degrees = new Map(nodes.map((node) => [node.path, 0]));
    for (const edge of edges) {
      degrees.set(edge.source, (degrees.get(edge.source) || 0) + 1);
      degrees.set(edge.target, (degrees.get(edge.target) || 0) + 1);
    }
    return degrees;
  };
  const adjacencyForGraph = (nodes, edges) => {
    const adjacency = new Map(nodes.map((node) => [node.path, new Set()]));
    for (const edge of edges) {
      adjacency.get(edge.source)?.add(edge.target);
      adjacency.get(edge.target)?.add(edge.source);
    }
    return adjacency;
  };
  const buildGraphLayout = (nodes, edges, width, height, pinnedPath) => {
    // Prefer a deterministic ring layout over a client-side force simulation. It is cheaper for
    // large published graphs and avoids the "slow on every page load" cost of repeated O(n^2)
    // relaxation loops in the browser.
    const positions = new Map();
    if (!nodes.length) return positions;

    const center = { x: width / 2, y: height / 2 };
    const degrees = degreesForGraph(nodes, edges);
    const adjacency = adjacencyForGraph(nodes, edges);
    const primaryPath =
      (pinnedPath && adjacency.has(pinnedPath) ? pinnedPath : null) ||
      [...nodes].sort((left, right) => {
        const degreeDelta = (degrees.get(right.path) || 0) - (degrees.get(left.path) || 0);
        if (degreeDelta !== 0) return degreeDelta;
        return (left.title || left.path).localeCompare(right.title || right.path);
      })[0]?.path;

    const orderedPaths = [];
    const visited = new Set();
    if (primaryPath) {
      const queue = [primaryPath];
      visited.add(primaryPath);
      while (queue.length) {
        const path = queue.shift();
        if (!path) continue;
        orderedPaths.push(path);
        const neighbors = [...(adjacency.get(path) || [])].sort((left, right) => {
          const degreeDelta = (degrees.get(right) || 0) - (degrees.get(left) || 0);
          if (degreeDelta !== 0) return degreeDelta;
          return left.localeCompare(right);
        });
        for (const neighbor of neighbors) {
          if (visited.has(neighbor)) continue;
          visited.add(neighbor);
          queue.push(neighbor);
        }
      }
    }

    for (const node of [...nodes].sort((left, right) => {
      const degreeDelta = (degrees.get(right.path) || 0) - (degrees.get(left.path) || 0);
      if (degreeDelta !== 0) return degreeDelta;
      return (left.title || left.path).localeCompare(right.title || right.path);
    })) {
      if (!visited.has(node.path)) orderedPaths.push(node.path);
    }

    const rootPath = orderedPaths[0];
    if (!rootPath) return positions;
    positions.set(rootPath, center);

    let index = 1;
    let ring = 1;
    let capacity = pinnedPath ? 10 : 18;
    while (index < orderedPaths.length) {
      const count = Math.min(capacity, orderedPaths.length - index);
      const radius =
        Math.min(width, height) * (pinnedPath ? 0.16 + ring * 0.16 : 0.18 + ring * 0.12);
      const offset = ring * 0.32;
      for (let slot = 0; slot < count; slot += 1) {
        const path = orderedPaths[index + slot];
        const angle = -Math.PI / 2 + (slot / count) * Math.PI * 2 + offset;
        positions.set(path, {
          x: clamp(center.x + Math.cos(angle) * radius, 20, width - 20),
          y: clamp(center.y + Math.sin(angle) * radius, 20, height - 20),
        });
      }
      index += count;
      ring += 1;
      capacity += pinnedPath ? 8 : 12;
    }
    return positions;
  };
  const renderGraphCanvas = (canvas, nodes, edges, currentPath, globalMode) => {
    if (!nodes.length) {
      canvas.innerHTML = '<p class="site-graph-empty">No published graph data is available for this view.</p>';
      return;
    }
    const width = globalMode ? 980 : 360;
    const height = globalMode ? 560 : 260;
    const degrees = degreesForGraph(nodes, edges);
    const positions = buildGraphLayout(nodes, edges, width, height, currentPath);
    const labelLimit = globalMode ? 28 : nodes.length;
    const labeledPaths = new Set(
      [...nodes]
        .sort((left, right) => {
          const rightCurrent = right.path === currentPath ? 1 : 0;
          const leftCurrent = left.path === currentPath ? 1 : 0;
          if (rightCurrent !== leftCurrent) return rightCurrent - leftCurrent;
          return (degrees.get(right.path) || 0) - (degrees.get(left.path) || 0);
        })
        .slice(0, labelLimit)
        .map((node) => node.path)
    );
    const edgeMarkup = edges
      .map((edge) => {
        const source = positions.get(edge.source);
        const target = positions.get(edge.target);
        if (!source || !target) return '';
        return `<line class="site-graph-edge" x1="${source.x.toFixed(2)}" y1="${source.y.toFixed(2)}" x2="${target.x.toFixed(2)}" y2="${target.y.toFixed(2)}" />`;
      })
      .join('');
    const nodeMarkup = nodes
      .map((node) => {
        const position = positions.get(node.path);
        if (!position) return '';
        const isCurrent = node.path === currentPath;
        const radius = clamp(5 + (degrees.get(node.path) || 0) * 0.28, 5, isCurrent ? 11 : 9);
        const label = labeledPaths.has(node.path)
          ? `<text x="${(position.x + radius + 6).toFixed(2)}" y="${(position.y + 4).toFixed(2)}">${escapeHtml(node.title || node.path)}</text>`
          : '';
        return [
          `<a class="site-graph-node${isCurrent ? ' is-current' : ''}" href="${escapeHtml(node.url)}">`,
          `<title>${escapeHtml(node.title || node.path)}</title>`,
          `<circle cx="${position.x.toFixed(2)}" cy="${position.y.toFixed(2)}" r="${radius.toFixed(2)}" />`,
          label,
          '</a>',
        ].join('');
      })
      .join('');
    canvas.innerHTML = `<svg viewBox="0 0 ${width} ${height}" role="img" aria-label="Published note graph">${edgeMarkup}${nodeMarkup}</svg>`;
  };
  if (graphAsset && graphCanvases.length) {
    fetch(graphAsset)
      .then((response) => (response.ok ? response.json() : null))
      .then((payload) => {
        const nodes = payload?.nodes || [];
        const edges = payload?.edges || [];
        const nodeByPath = new Map(nodes.map((node) => [node.path, node]));
        const globalDegrees = degreesForGraph(nodes, edges);
        for (const canvas of graphCanvases) {
          const mode = canvas.getAttribute('data-site-graph-canvas') || 'local';
          if (mode === 'global') {
            const selectedNodes = [...nodes]
              .sort((left, right) => {
                const degreeDelta =
                  (globalDegrees.get(right.path) || 0) - (globalDegrees.get(left.path) || 0);
                if (degreeDelta !== 0) return degreeDelta;
                return (left.title || left.path).localeCompare(right.title || right.path);
              })
              .slice(0, 140);
            const selectedPaths = new Set(selectedNodes.map((node) => node.path));
            const selectedEdges = edges.filter(
              (edge) => selectedPaths.has(edge.source) && selectedPaths.has(edge.target)
            );
            renderGraphCanvas(canvas, selectedNodes, selectedEdges, null, true);
            continue;
          }

          const notePath = canvas.getAttribute('data-site-note-path') || body.dataset.currentNotePath;
          if (!notePath || !nodeByPath.has(notePath)) {
            canvas.innerHTML = '<p class="site-graph-empty">This note has no published graph node.</p>';
            continue;
          }
          const localPaths = new Set([notePath]);
          for (const edge of edges) {
            if (edge.source === notePath) localPaths.add(edge.target);
            if (edge.target === notePath) localPaths.add(edge.source);
          }
          const localNodes = [...localPaths]
            .map((path) => nodeByPath.get(path))
            .filter(Boolean);
          const localEdges = edges.filter(
            (edge) => localPaths.has(edge.source) && localPaths.has(edge.target)
          );
          renderGraphCanvas(canvas, localNodes, localEdges, notePath, false);
        }
      })
      .catch(() => {});
  }

  const dispatchRuntimeHook = (name, detail) => {
    const event = new CustomEvent(name, { cancelable: true, detail });
    document.dispatchEvent(event);
    return !event.defaultPrevented;
  };
  const enhanceMath = () => {
    const nodes = [...document.querySelectorAll('[data-site-math]')];
    if (!nodes.length) return;
    if (
      !dispatchRuntimeHook('vulcan-site:math', {
        nodes,
        runtimeAvailable: Boolean(window.katex?.render),
      })
    ) {
      return;
    }
    if (!window.katex?.render) return;
    for (const node of nodes) {
      const tex = node.textContent?.trim();
      if (!tex) continue;
      try {
        window.katex.render(tex, node, {
          displayMode: node.dataset.siteMath === 'display',
          throwOnError: false,
        });
        node.setAttribute('data-site-math-rendered', 'katex');
      } catch (_) {}
    }
  };
  const enhanceMermaid = () => {
    const sourceBlocks = [...document.querySelectorAll('[data-site-mermaid-source]')];
    if (!sourceBlocks.length) return;
    if (
      !dispatchRuntimeHook('vulcan-site:mermaid', {
        nodes: sourceBlocks,
        runtimeAvailable: Boolean(window.mermaid?.run),
      })
    ) {
      return;
    }
    if (!window.mermaid?.run) return;
    const hosts = [];
    for (const block of sourceBlocks) {
      const code = block.querySelector('code.language-mermaid');
      const source = code?.textContent?.trim();
      if (!source) continue;
      const host = document.createElement('div');
      host.className = 'mermaid';
      host.setAttribute('data-site-mermaid', 'true');
      host.textContent = source;
      block.replaceWith(host);
      hosts.push(host);
    }
    if (!hosts.length) return;
    try {
      window.mermaid.run({ nodes: hosts });
    } catch (_) {}
  };
  const runOptionalRuntimeEnhancements = () => {
    enhanceMath();
    enhanceMermaid();
  };
  if (document.readyState === 'complete') {
    runOptionalRuntimeEnhancements();
  } else {
    document.addEventListener('DOMContentLoaded', runOptionalRuntimeEnhancements, { once: true });
  }

  let liveVersion = null;
  const liveUrl = body.dataset.liveReloadUrl || '/__vulcan_site/live-reload.json';
  const liveSseUrl = body.dataset.liveReloadSseUrl || '';
  const overlayId = 'vulcan-site-live-overlay';
  const ensureOverlay = (message) => {
    let overlay = document.getElementById(overlayId);
    if (!overlay) {
      overlay = document.createElement('div');
      overlay.id = overlayId;
      overlay.className = 'site-live-overlay';
      document.body.appendChild(overlay);
    }
    overlay.innerHTML = message;
  };
  const clearOverlay = () => {
    const overlay = document.getElementById(overlayId);
    if (overlay) overlay.remove();
  };
  const formatDiagnosticsOverlay = (payload) => {
    if (payload.last_error) return escapeHtml(payload.last_error);
    if (!Array.isArray(payload.diagnostics) || !payload.diagnostics.length) return '';
    return payload.diagnostics
      .slice(0, 3)
      .map((diagnostic) => {
        const path = diagnostic.source_path ? ` <br><small>${escapeHtml(diagnostic.source_path)}</small>` : '';
        return `<strong>[${escapeHtml(diagnostic.level)}]</strong> ${escapeHtml(diagnostic.kind)} ${escapeHtml(diagnostic.message)}${path}`;
      })
      .join('<hr>');
  };
  const handleLivePayload = (payload) => {
    if (!payload) return;
    if (liveVersion === null) {
      liveVersion = payload.version;
    } else if (payload.version !== liveVersion) {
      window.location.reload();
      return;
    }
    const overlayMessage = formatDiagnosticsOverlay(payload);
    if (overlayMessage) ensureOverlay(overlayMessage);
    else clearOverlay();
  };
  const startPolling = () => {
    window.setInterval(() => {
      fetch(liveUrl, { cache: 'no-store' })
        .then((response) => (response.ok ? response.json() : null))
        .then(handleLivePayload)
        .catch(() => {});
    }, 1200);
  };
  if (window.EventSource && liveSseUrl) {
    const source = new EventSource(liveSseUrl);
    source.addEventListener('update', (event) => {
      try {
        handleLivePayload(JSON.parse(event.data));
      } catch (_) {}
    });
    source.onerror = () => {
      source.close();
      startPolling();
    };
  } else {
    startPolling();
  }
})();"#;
