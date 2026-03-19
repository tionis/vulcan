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

## Recommended Vulcan approach

1. **Use pulldown-cmark as-is** for tokenization and offset extraction. The `into_offset_iter()` + `WikiLink` link type gives us exactly what we need for span-based patching.

2. **Add a thin Obsidian semantic pass** that post-processes the event stream:
   - For every `WikiLink` event, split `dest_url` on `#` to extract `(target_path, heading_or_block_subpath)`.
   - If the subpath starts with `^`, classify it as a block reference; otherwise, it's a heading reference.
   - For `Tag::Image` with `WikiLink` link type, check if `dest_url` looks like a note (no image extension) to distinguish note embeds from image embeds.
   - Extract inline tags (`#tag`) from `Text` events — these are NOT recognized by pulldown-cmark at all.

3. **Do not fork pulldown-cmark.** The gaps are small and stable — subpath splitting and embed classification are straightforward string operations. A fork would create maintenance burden for minimal gain.

## Risk assessment

**Low risk.** The gaps are well-defined and can be addressed with <100 lines of post-processing. The core value — byte-accurate source ranges for every link token — works correctly. The `has_pothole` field even tells us whether to preserve pipe syntax during rewrites.

## Sources

- [pulldown-cmark wikilinks spec](https://pulldown-cmark.github.io/pulldown-cmark/specs/wikilinks.html)
- [LinkType::WikiLink docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/enum.LinkType.html)
- [Tag enum docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/enum.Tag.html)
- [pulldown-cmark releases](https://github.com/pulldown-cmark/pulldown-cmark/releases)
