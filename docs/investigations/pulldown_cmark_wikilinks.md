# Investigation: pulldown-cmark Wikilink Support

**Date:** 20 March 2026
**pulldown-cmark version:** 0.13.1 (latest), wikilinks added in 0.13.0
**Status:** Usable with known gaps that require a supplementary pass

## Summary

pulldown-cmark's wikilink support covers the basic Obsidian link patterns but does **not** handle heading subpaths, block references, or embed detection natively. These must be handled by a small post-processing pass in Vulcan.

## How wikilinks are represented

Wikilinks are emitted as standard `Tag::Link` / `Tag::Image` events with `LinkType::WikiLink { has_pothole: bool }`:

```rust
// [[Note Name]] produces:
Event::Start(Tag::Link {
    link_type: LinkType::WikiLink { has_pothole: false },
    dest_url: "Note Name",
    title: "",
    id: "",
})
Event::Text("Note Name")
Event::End(TagEnd::Link)

// [[Note Name|Display Text]] produces:
Event::Start(Tag::Link {
    link_type: LinkType::WikiLink { has_pothole: true },
    dest_url: "Note Name",
    title: "",
    id: "",
})
Event::Text("Display Text")
Event::End(TagEnd::Link)

// ![[image.png]] produces:
Event::Start(Tag::Image {
    link_type: LinkType::WikiLink { has_pothole: false },
    dest_url: "image.png",
    title: "",
    id: "",
})
Event::Text("image.png")
Event::End(TagEnd::Image)
```

The `has_pothole` field indicates whether the pipe separator was present (pulldown-cmark uses "pothole" as its term for piped wikilinks).

## What works out of the box

| Pattern | Supported | Notes |
|---|---|---|
| `[[Note Name]]` | Yes | Basic wikilink |
| `[[Note Name\|Display Text]]` | Yes | `has_pothole: true`, display text in inner events |
| `[[folder/Note Name]]` | Yes | Path preserved in `dest_url` |
| `![[image.png]]` | Yes | Emitted as `Tag::Image` with `WikiLink` link type |
| `![[image.png\|alt text]]` | Yes | Alt text as display content |
| `into_offset_iter()` byte ranges | Yes | Source ranges returned for all events including wikilinks |

## What does NOT work out of the box

| Pattern | Status | Impact |
|---|---|---|
| `[[Note#Heading]]` | Partial | The `#Heading` is included in `dest_url` as a raw string. pulldown-cmark does NOT split it into target + heading subpath. Vulcan must parse `dest_url` to extract the `#` fragment. |
| `[[Note#^block-id]]` | Partial | Same as above — `#^block-id` is part of `dest_url`. Vulcan must detect the `#^` prefix. |
| `![[Note]]` (note embed) | Misclassified | `![[Note]]` for embedding a note (not an image) is emitted as `Tag::Image`. Vulcan must distinguish note embeds from image embeds by checking the file extension or absence of extension. |
| `[[Note#Heading\|Display]]` | Partial | Works (alias + subpath), but subpath extraction from `dest_url` is still manual. |
| Pipe in href | Impossible | A literal `|` cannot appear in the link destination. This matches Obsidian's behavior so it's not a practical issue. |
| Inline HTML links | Not tracked | `<a href="...">` and `<img src="...">` in raw HTML are emitted as `Event::Html` / `Event::InlineHtml` — pulldown-cmark does not produce `Tag::Link` events for them. These links are invisible to the link graph. Rare in Obsidian vaults but should surface as a `doctor` diagnostic. |
| `%%comments%%` | Not recognized | Obsidian hides text between `%%` markers in preview. pulldown-cmark has no concept of this and emits `%%` and content as regular `Text` events. **Must strip in the semantic pass** — otherwise private comments leak into chunks, FTS index, and search results. |
| `==highlights==` | Not recognized | Obsidian renders text between `==` as highlighted. pulldown-cmark emits literal `==` markers as text. Content is still searchable (low impact), but the markers are noise. Strip `==` markers in the semantic pass for cleaner indexing. |
| Nested tags (`#tag/subtag`) | Not recognized | Obsidian supports hierarchical tags with `/` separators. Inline tag extraction must match `#[a-zA-Z0-9/_-]+` (including slashes), not just `#[a-zA-Z0-9-]+`. |
| `obsidian://` URIs | N/A | Can appear in standard Markdown links. Should be recognized and recorded as external links — do not attempt vault-path resolution. |

## Recommended Vulcan approach

1. **Use pulldown-cmark as-is** for tokenization and offset extraction. The `into_offset_iter()` + `WikiLink` link type gives us exactly what we need for span-based patching.

2. **Add a thin Obsidian semantic pass** that post-processes the event stream:
   - For every `WikiLink` event, split `dest_url` on `#` to extract `(target_path, heading_or_block_subpath)`.
   - If the subpath starts with `^`, classify it as a block reference; otherwise, it's a heading reference.
   - For `Tag::Image` with `WikiLink` link type, check if `dest_url` looks like a note (no image extension) to distinguish note embeds from image embeds. Embed subpaths (`![[Note#Heading]]`, `![[Note#^block]]`) compose with the same splitting logic — no special handling needed.
   - Extract inline tags (`#tag` and `#tag/subtag/deep`) from `Text` events — these are NOT recognized by pulldown-cmark at all. Match `#[a-zA-Z0-9/_-]+` to support nested tag hierarchies.
   - **Strip `%%comment%%` blocks:** Scan `Text` events for `%%` delimiters and remove everything between them (including the delimiters). This must happen before chunk content is stored, otherwise private comments pollute the FTS index and search results. Handle both single-line (`%%inline comment%%`) and multi-line comments spanning multiple `Text` events.
   - **Strip `==highlight==` markers:** Remove literal `==` from text content for cleaner indexing. The highlighted text itself should still be indexed.
   - **Classify `obsidian://` URIs:** In standard `Tag::Link` events, check if `dest_url` starts with `obsidian://` and record as an external link rather than attempting vault-path resolution.
   - **Block ref extraction:** In Obsidian, a block ID is a standalone paragraph containing only `^identifier`, placed *after* the block it labels. For example, a block ID for a list appears as a bare `^my-list-id` paragraph following the list, not inline within a list item. pulldown-cmark will emit this as `Tag::Paragraph` > `Text("^my-list-id")`. The extraction pass should:
     1. Detect paragraphs whose text content matches `^[a-zA-Z0-9-]+` exactly.
     2. Record the block ID and associate it with the *preceding* block-level element (the list, blockquote, code block, table, or paragraph that came before it in the event stream).
     3. Store the byte offset range of both the block ID paragraph and the referenced block, so that embed resolution can identify the correct content span.
     This means the `block_refs` table should store both the block ID location and the start/end offsets of the content block it labels.

3. **Enable all relevant pulldown-cmark options:**
   - `ENABLE_WIKILINKS` — wikilinks
   - `ENABLE_GFM` — tables, strikethrough, task lists
   - `ENABLE_MATH` — inline and display math (prevents `$...$` from being misinterpreted as text delimiters)
   - `ENABLE_FOOTNOTES` — footnotes can contain links that must be in the graph
   - `ENABLE_YAML_STYLE_METADATA_BLOCKS` — lets pulldown-cmark recognize frontmatter so it emits a `MetadataBlock` event rather than treating `---` as a thematic break. Vulcan should extract raw frontmatter from this event for lossless preservation, avoiding the need to pre-strip frontmatter before parsing.

4. **Do not fork pulldown-cmark.** The gaps are small and stable — subpath splitting, comment stripping, and embed classification are straightforward string operations. A fork would create maintenance burden for minimal gain.

## Risk assessment

**Low risk.** The gaps are well-defined and the semantic pass is estimated at ~200-300 lines covering: subpath splitting, embed classification, block ref extraction, comment stripping, highlight marker removal, inline tag extraction, nested tag support, and `obsidian://` URI detection. The core value — byte-accurate source ranges for every link token — works correctly. The `has_pothole` field even tells us whether to preserve pipe syntax during rewrites.

## Deferred: features that only matter for rendering/export

- **Embedded search results:** Obsidian supports ` ```query ` fenced code blocks that evaluate a search query and inline the results at render time. pulldown-cmark emits these as normal `Tag::CodeBlock` with info string `"query"`. No indexing or resolution needed — the content is dynamic. If Vulcan later adds rendering, export, or hydration features, these blocks would need evaluation against the FTS index.
- **Mermaid diagrams:** ` ```mermaid ` fenced code blocks. Emitted as normal `Tag::CodeBlock`. Render-only concern.
- **LaTeX math:** MathJax inline (`$...$`) and display (`$$...$$`) formulas. pulldown-cmark 0.13 has `ENABLE_MATH` which emits `Event::InlineMath` and `Event::DisplayMath`, so the parser already handles these. No indexing relevance, but the events are available if rendering is added later.
- **MultiMarkdown 6 tables:** Not in standard Obsidian; requires a community plugin. pulldown-cmark supports GFM tables but not MMD6 extensions (colspan, rowspan, etc.). Out of scope per §3 (no plugin-specific syntax), but worth noting if export fidelity becomes a goal.

## Out of scope: common plugin syntax

These are explicitly out of scope per §3 (no plugin-specific syntax extensions). They are listed here because they are extremely common in real vaults and worth understanding for triage purposes.

- **Dataview:** `` ```dataview `` code blocks and inline expressions (`= this.property`). pulldown-cmark emits code blocks as `Tag::CodeBlock` and inline expressions as regular text. Both pass through harmlessly as inert chunk content. No special handling needed.
- **Templater:** `<% %>` template tags. pulldown-cmark may emit these as `Event::InlineHtml` or `Event::Html` depending on context. Harmless — templates are typically resolved before the note is saved, so they rarely appear in vault content at rest. If they do, they're just noise in the index.

Neither requires special-casing in the parser. If `doctor` ever grows a "plugin syntax detected" advisory diagnostic, these would be the first candidates.

## Sources

- [pulldown-cmark wikilinks spec](https://pulldown-cmark.github.io/pulldown-cmark/specs/wikilinks.html)
- [LinkType::WikiLink docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/enum.LinkType.html)
- [Tag enum docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/enum.Tag.html)
- [pulldown-cmark releases](https://github.com/pulldown-cmark/pulldown-cmark/releases)
