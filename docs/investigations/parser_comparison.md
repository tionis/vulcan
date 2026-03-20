# Investigation: Markdown Parser Comparison

**Date:** 20 March 2026
**Purpose:** Evaluate alternatives to pulldown-cmark for the Vulcan parser pipeline, including parsers in other languages, since the parser is the most critical component of the system.

## Evaluation criteria

Ranked by importance to Vulcan:

1. **Byte-accurate source offsets** — Required for move-safe link rewriting. Without this, the rewrite engine must re-render entire documents or use fragile regex substitution.
2. **Wikilink support** (native or extensible) — Must handle `[[target]]`, `[[target|display]]`, `![[embed]]`, ideally with heading/block subpath awareness.
3. **Streaming/incremental processing** — For large vaults, materializing a full AST per file is more expensive than streaming events. Matters for rewrite-heavy operations across many files.
4. **Obsidian Flavored Markdown coverage** — Callouts, frontmatter, footnotes, math, GFM tables/strikethrough/task lists.
5. **Extensibility** — Can we add our Obsidian semantic pass cleanly?
6. **Correctness** — CommonMark compliance, edge case handling, test suite quality.
7. **Performance** — Parse speed and memory usage.
8. **Language fit** — How well does it integrate with a Rust binary? FFI overhead, build complexity, binary size.

---

## Candidates

### 1. pulldown-cmark (Rust) — CURRENT CHOICE

**Version:** 0.13.1
**Language:** Rust (native)
**Model:** Event stream (SAX-like), not AST

| Criterion | Rating | Notes |
|---|---|---|
| Byte offsets | Excellent | `into_offset_iter()` returns `(Event, Range<usize>)` for every event. Byte-accurate. |
| Wikilinks | Good | `LinkType::WikiLink { has_pothole }` since 0.13.0. No native subpath splitting or embed classification — requires ~200-300 line semantic pass. |
| Streaming | Excellent | Event stream by design. No AST allocation. Can process a file by collecting only the spans that matter. |
| OFM coverage | Good | GFM (tables, strikethrough, tasks), math (`ENABLE_MATH`), footnotes, YAML frontmatter, callouts (GFM blockquote tags). No `%%comments%%` or `==highlights==` — handled in semantic pass. |
| Extensibility | Moderate | No plugin system. Extension happens by post-processing the event stream, which is sufficient for our needs but not as clean as a proper extension API. |
| Correctness | Excellent | CommonMark spec-compliant. Well-tested. |
| Performance | Excellent | Fastest Rust Markdown parser. Zero-copy where possible. Low memory. |
| Language fit | Native | Pure Rust, no FFI. |

**Strengths:** Best offset story for rewriting. Streaming model is ideal for "collect links, patch, move on" workflows. Fastest option. Zero integration overhead.

**Weaknesses:** No plugin system — all OFM-specific handling is our code. No AST if we ever need tree-level analysis (e.g., "find the list that precedes this block ref paragraph"). The event stream requires manual state tracking for context-dependent operations.

---

### 2. comrak (Rust)

**Version:** 0.51
**Language:** Rust (native)
**Model:** Arena-allocated AST with parent/sibling/child pointers (`typed_arena` + `RefCell`)

| Criterion | Rating | Notes |
|---|---|---|
| Byte offsets | Good | `sourcepos` on AST nodes (line/column + byte offsets). Available on all nodes. |
| Wikilinks | Good | Native support with both title-before-pipe and title-after-pipe variants. No evidence of subpath or embed classification. |
| Streaming | Poor | Full AST must be materialized before processing. For rewrite-only operations, this allocates an entire tree per file when we only need a handful of link spans. |
| OFM coverage | Excellent | Widest extension set of any parser: GFM, math, footnotes, frontmatter, alerts (callouts), wikilinks, superscript, underline, spoiler, greentext, description lists. |
| Extensibility | Good | Extension options are toggle flags. Custom renderers via trait. No arbitrary syntax extension API, but the built-in set covers more ground than pulldown-cmark. |
| Correctness | Excellent | CommonMark 0.31.2. Port of cmark-gfm (the C reference). |
| Performance | Good | Faster than most, but slower than pulldown-cmark due to AST allocation. |
| Language fit | Native | Pure Rust, no FFI. |

**Strengths:** Richest OFM coverage out of the box — alerts/callouts, wikilinks, math, footnotes, frontmatter all native. If we needed full AST analysis (e.g., transformations, structural queries), this would be the better choice.

**Weaknesses:** AST materialization is wasteful for our primary use case (collect link spans → patch). For a vault with 5000 notes where a rename touches 200 files, we'd allocate and discard 200 full ASTs just to extract a few link spans. The `RefCell`-heavy arena model is also less ergonomic than pulldown-cmark's simple iterator.

---

### 3. goldmark (Go)

**Version:** actively maintained (434+ commits)
**Language:** Go
**Model:** AST with segment-based source references

| Criterion | Rating | Notes |
|---|---|---|
| Byte offsets | Good | AST nodes store `text.Segment` (Start/End byte offsets). Inline nodes have single segments; block nodes have `Lines()` returning multiple segments. Offsets are into the original source. |
| Wikilinks | Good (via extension) | `goldmark-wikilink` extension provides `Node` with `Target`, `Fragment` (heading), `Embed` bool, and pipe display text. No block ref (`#^`) support. |
| Streaming | Poor | Full AST. Extensible via transformers but no event-stream mode. |
| OFM coverage | Moderate | GFM (tables, strikethrough, tasks), definition lists, footnotes, typographer. No native math, frontmatter, callouts, or wikilinks — all via community extensions. |
| Extensibility | Excellent | Best extension model of any parser. Can add custom AST nodes, block/inline parsers, AST transformers, and renderers. This is goldmark's standout feature. |
| Correctness | Excellent | 100% CommonMark compliant. Well-structured. |
| Performance | Good | Efficient for Go, lower memory than many alternatives. |
| Language fit | Poor | Go — would require either rewriting Vulcan in Go, or CGo FFI from Rust, or running as a sidecar process. None of these are practical for a core pipeline component. |

**Strengths:** Best extension architecture. If Vulcan were a Go project, goldmark would be the clear choice. The wikilink extension already parses fragments (headings) and embeds into structured AST nodes, reducing our semantic pass.

**Weaknesses:** Language mismatch is a dealbreaker. CGo FFI from Rust adds build complexity, runtime overhead, and debugging pain. A sidecar process adds IPC latency and deployment complexity. The extension quality is excellent but we can't use it.

---

### 4. remark / unified (TypeScript/JavaScript)

**Version:** actively maintained, 150+ plugins
**Language:** TypeScript/JavaScript
**Model:** mdast (Markdown Abstract Syntax Tree) conforming to unist

| Criterion | Rating | Notes |
|---|---|---|
| Byte offsets | Good | Positions on all AST nodes (line/column/offset). Full unist positional info. |
| Wikilinks | Moderate | `remark-wiki-link` / `micromark-extension-wiki-link` add `wikiLink` nodes. Community-maintained, not as mature as goldmark's extension. |
| Streaming | Poor | Full AST (mdast). |
| OFM coverage | Good (via plugins) | GFM (`remark-gfm`), math (`remark-math`), frontmatter (`remark-frontmatter`). Callouts require custom plugin. |
| Extensibility | Excellent | Plugin-based architecture with 150+ community plugins. Micromark for tokenization, mdast for AST, rehype for HTML. Very composable. |
| Correctness | Good | CommonMark compliant. micromark is the JS equivalent of markdown-rs. |
| Performance | Poor | JavaScript/Node.js. Orders of magnitude slower than Rust for batch processing of large vaults. |
| Language fit | Poor | TypeScript — requires Node.js runtime or WASM compilation. Binary distribution is painful (pkg, nexe, or bundled V8). Not viable for a single-binary CLI. |

**Strengths:** Richest ecosystem. If you need an obscure Markdown extension, there's probably a remark plugin for it. The `remark-stringify` roundtrip (parse → transform → serialize) is excellent.

**Weaknesses:** Performance and distribution. Parsing 5000 notes in Node.js is slow. Distributing as a single binary requires bundling a JS runtime. The entire architecture is optimized for web/build tooling, not for a local CLI.

---

### 5. markdown-it (JavaScript)

**Version:** 14.x, actively maintained
**Language:** JavaScript
**Model:** Token stream (flat array, not tree)

| Criterion | Rating | Notes |
|---|---|---|
| Byte offsets | Poor | Only line-level source maps (`[line_begin, line_end]`). No byte offsets. Character-level positions require patching via community plugins and are not first-class. |
| Wikilinks | Moderate | Community plugins (`markdown-it-wikilinks-plus`). Multiple forks of varying quality. |
| Streaming | Moderate | Token array, not full AST. But tokens must all be generated before processing. |
| OFM coverage | Good (via plugins) | Extensive plugin ecosystem. Tables, footnotes, math, front matter all available. |
| Extensibility | Good | Rule-based plugin system. Can add/replace/modify parsing rules. |
| Correctness | Good | CommonMark compliant. |
| Performance | Poor | JavaScript. |
| Language fit | Poor | Same issues as remark — requires JS runtime. |

**Strengths:** Simple, fast-for-JS, good plugin model.

**Weaknesses:** No byte-level offsets is a dealbreaker for our rewrite engine. Line-level granularity is insufficient for patching individual link spans within a line. Language mismatch compounds the problem.

---

### 6. markdown-rs (Rust)

**Version:** 1.0.0 (released April 2025)
**Language:** Rust (native, `#![no_std]` + alloc)
**Model:** State machine → concrete tokens → mdast AST or HTML

| Criterion | Rating | Notes |
|---|---|---|
| Byte offsets | Excellent | "Every byte is accounted for, with positional info." mdast nodes include `line:column (byte_offset)` positions. |
| Wikilinks | Not supported | No wikilink extension. Supports GFM, MDX, math, frontmatter. No plugin system to add custom syntax. |
| Streaming | Moderate | Internal state machine produces tokens, but the public API is `to_html()` or `to_mdast()` — full AST or rendered output, no event stream. |
| OFM coverage | Moderate | GFM, MDX, math, frontmatter. No callouts, no wikilinks. |
| Extensibility | Poor | No plugin or extension API. The supported extensions are compiled in. |
| Correctness | Excellent | Goes beyond CommonMark compliance — follows cmark reference parser behavior. 2300+ tests. 100% code coverage. |
| Performance | Good | Rust, competitive with pulldown-cmark. |
| Language fit | Native | Pure Rust, `#![no_std]`. |

**Strengths:** Most thorough correctness story. Sibling of micromark (JS), so if we ever need JS interop for the same parse semantics, the mapping is direct.

**Weaknesses:** No wikilinks and no way to add them without forking. This is a hard blocker. The parser was designed for MDX workflows, not Obsidian/wiki-style content. The `to_mdast()` API also means full AST materialization — no event stream.

---

## Comparison matrix

| | pulldown-cmark | comrak | goldmark | remark | markdown-it | markdown-rs |
|---|---|---|---|---|---|---|
| **Language** | Rust | Rust | Go | TypeScript | JavaScript | Rust |
| **Byte offsets** | ✅ Excellent | ✅ Good | ✅ Good | ✅ Good | ❌ Line-only | ✅ Excellent |
| **Wikilinks** | ✅ Native | ✅ Native | ✅ Extension | ⚠️ Plugin | ⚠️ Plugin | ❌ None |
| **Streaming** | ✅ Event stream | ❌ Full AST | ❌ Full AST | ❌ Full AST | ⚠️ Token array | ❌ Full AST |
| **OFM coverage** | ✅ Good | ✅ Best | ⚠️ Moderate | ✅ Good | ✅ Good | ⚠️ Moderate |
| **Extensibility** | ⚠️ Post-process | ✅ Options | ✅ Best | ✅ Plugins | ✅ Rules | ❌ None |
| **Correctness** | ✅ Excellent | ✅ Excellent | ✅ Excellent | ✅ Good | ✅ Good | ✅ Best |
| **Performance** | ✅ Fastest | ✅ Fast | ✅ Fast (Go) | ❌ Slow | ❌ Slow | ✅ Fast |
| **Rust integration** | ✅ Native | ✅ Native | ❌ FFI/sidecar | ❌ Runtime | ❌ Runtime | ✅ Native |

## Eliminated candidates

- **markdown-it**: No byte-level offsets. Dealbreaker for rewrite engine.
- **remark/unified**: Performance and distribution story incompatible with single-binary CLI.
- **markdown-rs**: No wikilinks, no extension API. Would require forking.
- **goldmark**: Excellent parser, wrong language. FFI/sidecar overhead unjustifiable for core pipeline.

## Final two: pulldown-cmark vs comrak

Both are native Rust, both have good byte offsets, both support wikilinks natively. The decision comes down to the processing model.

### When to choose comrak

- If the project needed **full AST analysis** — structural queries, tree transformations, or rendering to multiple output formats.
- If the project needed **the widest built-in OFM coverage** — comrak has alerts, spoiler text, superscript, description lists, and more without any custom code.
- If **rewriting was rare** and most operations were read-only queries over the graph.

### When to choose pulldown-cmark

- If the project's **hottest path is rewriting links across many files** — the event stream avoids allocating a full AST per file when we only need a handful of link spans.
- If **memory efficiency matters for large vaults** — streaming one event at a time uses less memory than materializing a tree.
- If the project can tolerate **~200-300 lines of semantic pass code** to handle the OFM gaps (comments, highlights, subpath splitting, embed classification).

### Verdict: pulldown-cmark remains the right choice

Vulcan's most performance-sensitive operation is move-safe rewriting: when a file is renamed, every inbound link (potentially across hundreds of files) must be re-parsed to extract the target link span, patched, and written back. This is a "scan many files, touch a few bytes each" workload where the event stream model wins decisively.

The OFM gaps (comments, highlights, tags, subpath splitting) are real but bounded — they add ~200-300 lines to the semantic pass and don't require any changes to pulldown-cmark itself. comrak would eliminate some of these gaps but would add AST allocation overhead on every file touch.

If Vulcan later adds features that need full AST analysis (e.g., structural document transformations, rich rendering), comrak could be introduced as a secondary parser for those specific code paths, while pulldown-cmark remains the primary parser for indexing and rewriting. The two are not mutually exclusive.

## Sources

- [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) — [wikilinks spec](https://pulldown-cmark.github.io/pulldown-cmark/specs/wikilinks.html) — [LinkType docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/enum.LinkType.html)
- [comrak](https://github.com/kivikakk/comrak) — [docs.rs](https://docs.rs/comrak)
- [goldmark](https://github.com/yuin/goldmark) — [ast package](https://pkg.go.dev/github.com/yuin/goldmark/ast) — [goldmark-wikilink](https://pkg.go.dev/go.abhg.dev/goldmark/wikilink)
- [remark/unified](https://github.com/remarkjs/remark) — [remark-wiki-link](https://github.com/landakram/remark-wiki-link)
- [markdown-it](https://github.com/markdown-it/markdown-it) — [source position issue](https://github.com/markdown-it/markdown-it/issues/821)
- [markdown-rs](https://github.com/wooorm/markdown-rs)
