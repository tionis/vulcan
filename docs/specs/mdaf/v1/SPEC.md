# Markdown Artifact Format (MDAF) version 1

MDAF is an immutable, extractor- and source-format-neutral package for one primary Markdown document and the evidence needed to reinterpret or materialize it later. Inputs may be PDFs, images, audio, video, web pages, ebooks, office documents, structured data, plain text, compound media, or formats not yet anticipated. `Markdown` describes the normalized primary output, not the source. A conforming artifact is either a directory whose name ends in `.mdaf` or a ZIP file whose name ends in `.mdaf`. Both representations expose the same root members and have the same logical identity.

MDAF deliberately does not define OCR, PDF conversion, table extraction, or a universal document-block ontology. Producers normalize only the information consumers share today and retain complete native responses as opaque declared members. Consumers must never select behavior from a producer, tool, model, asset filename, or extension namespace.

MDAF v1 is a [Container Core v1](../../container-core/v1/SPEC.md) format with manifest `info.json` and suffix `.mdaf` for both the directory and the ZIP form. Container Core defines the shared path, archive-safety, digest, logical-identity, source, locator, selector, and provenance rules. This document states only what MDAF adds. The rules were moved there unchanged, so existing artifacts and their identities remain valid. Use a [Markdown Wiki Package v2](../../wiki-package/v2/SPEC.md) for a wiki tree built from MDAF or other sources.

## Root layout

```text
info.json          required manifest
text.md            required primary Markdown
provenance.json    required activity graph
assets/            optional files referenced by text.md
source-map.json    optional normalized source selectors and references
outline.json       optional aligned alternative hierarchy
renditions/        optional complete native or alternate outputs
sources/           optional embedded source documents
environments/      optional locks, inventories, or SBOMs
extensions/        optional reverse-domain-namespaced producer data
```

Every regular file except `info.json` must appear exactly once in `info.json.members`. Empty directories have no meaning. Unknown files at the root are invalid. `renditions/`, `environments/`, and `extensions/` are opaque to Vulcan after path, size, and digest validation.

## Paths and archive safety

Container Core [member paths](../../container-core/v1/SPEC.md#member-paths) and [archive safety](../../container-core/v1/SPEC.md#archive-safety) apply. Non-asset members are limited to 512 MiB, and MDAF control JSON is limited to 32 MiB.

## Manifest and member roles

`info.json` conforms to `info.schema.json`. Version 1 fixes the primary paths to `text.md` and `provenance.json`. Optional normalized sidecars use their fixed root names. Other members are declared with one of these roles:

- `asset`: path below `assets/`;
- `rendition`: path below `renditions/`;
- `source`: path below `sources/`;
- `environment`: path below `environments/`;
- `extension`: path below `extensions/<reverse-domain-namespace>/`.

The primary Markdown media type is `text/markdown`. `markdown.variant` and `markdown.features` describe syntax without changing the MDAF contract. Sources have stable artifact-local IDs, media types, canonical BLAKE3 digests, optional alternate algorithm-tagged digests, and optional embedded member paths. Alternate digests preserve upstream identities without weakening or replacing the canonical digest. Portable core fields must not contain credentials, signed URLs, authorization headers, or absolute local paths.

## Logical identity

The logical identity is the Container Core [logical identity](../../container-core/v1/SPEC.md#logical-identity) over every regular member, including `info.json`. The specification fixtures provide a test vector. `info.json.derived_from` contains canonical logical identities of immutable parents; it is lineage, not an instruction to fetch them. A derivative remains self-contained and carries forward the evidence needed for future processing.

## Normalized source map

`source-map.json` conforms to `source-map.schema.json` and binds to the canonical digest of `text.md`. All document ranges are zero-based, half-open UTF-8 byte ranges whose endpoints are character boundaries.

A mapping connects a Markdown span to a source locator and may carry confidence and a namespaced method. A reference connects authored Markdown text to a target locator. Mappings may overlap, may be partial, and may repeat the same Markdown span for multiple sources. Producers decide which inferred records are reliable enough to publish; consumers preserve confidence and method but do not rerun extraction.

Locators and selectors follow Container Core [locators and selectors](../../container-core/v1/SPEC.md#locators-and-selectors): `interval`, `rectangle`, `polygon`, `grid`, `text-quote`, `fragment`, and namespaced `extension`. Unknown future source formats therefore require neither a new MDAF version nor a Vulcan code branch. They use the closest lossless normalized selectors and keep any richer native locator in an extension selector or rendition.

Source-reference resolution is conservative. A target selector must be matched by a compatible mapping selector for the same declared source; all target selectors must overlap or identify the same segment. Ambiguous or unsupported matches remain authored Markdown and produce a diagnostic rather than an inferred link.

A reference's document span may identify plain prose, not only an existing Markdown link. A consumer may wrap an exactly placed plain-text reference in a link after resolving its target. It must preserve code, existing link syntax, and ambiguous or overlapping placements. A coarse mapping that overlaps multiple materialized notes represents all those candidates: using only the note containing the mapping's first byte invents unsupported precision. Display labels do not change numeric selector coordinates and must not be inferred from a filename or a presumed page-number offset. Producers should leave externally qualified citations and unproven targets unbound.

## Alternative outline

`outline.json` conforms to `outline.schema.json` and binds to `text.md`. Nodes form one ordered forest with stable IDs, parent IDs, levels, titles, heading spans, section spans, and optional source locators. Section spans must be ordered and either disjoint or properly nested; a heading span lies inside its section. Selecting the outline as import authority requires complete valid alignment. Markdown headings remain the default authority, and consumers never merge authorities silently.

An outline title is routing metadata; it need not equal an authored Markdown heading or create a Markdown fragment with that name. A producer may combine a split chapter title or describe a front-matter section without changing primary Markdown. Existing authored heading/HTML-anchor targets remain valid and must route to their owning output note even when omitted from the selected outline.

Consumers must honor declared section boundaries or reject an unsupported alignment before mutation. Vulcan's heading decomposition currently accepts outlines whose section starts equal heading starts, whose parents match the preceding level stack, and whose section ends equal the next node at the same or a shallower level (or the document end). Bounded tails or gaps require a different materialization algorithm; they must not be silently expanded. Levels two and three are a useful producer convention for major sections and nested topics, but their meaning is not prescribed by MDAF and does not select an import authority automatically.

## Native evidence and extensions

When materializing an explicitly selected outline, consumers may render authored
ATX heading markers at note-relative outline levels without changing the heading
text or its fragment identity. Synthetic boundaries in prose do not authorize
replacing prose with titles. Source spans continue to reference immutable primary
bytes, and link placements must account for both growing and shrinking markers.

Complete extractor responses belong below `renditions/<namespace>/` and are declared with their real media types and schemas when known. They may contain provider-native block trees, bounding boxes, polygons, masks, timestamps, tracks, frames, page Markdown, tables, hyperlinks, DOM trees, or binary databases. MDAF does not rewrite or interpret them.

Native responses are retained byte-for-byte after mandatory secret filtering. A redaction creates a provenance record naming the field location, reason, and original-field digest when safe to compute. Assets may use arbitrary names; only declared roles and Markdown-relative references carry meaning.

## Provenance

`provenance.json` is a Container Core [provenance](../../container-core/v1/SPEC.md#provenance) activity graph. The bundled `provenance.schema.json` is identical to Container Core's apart from its `$id` and `title`. Every generated member names the one activity that produced it. Full dependency locks, runtime descriptions, hardware inventories, SPDX documents, or CycloneDX documents are optional environment members. Unknown exact versions or revisions remain explicit `unavailable` values with diagnostics.

## Consumer behavior

Structural validity, complete source-byte coverage and successful generated-link
checks do not establish semantic topic ownership. Consumers should distinguish
these mechanical checks from semantic review and identify unusually large notes
throughout the output, not only the root. Size measurements should distinguish
owned primary bytes from generated navigation and metadata. These review aids
are consumer reports, not additional required MDAF fields or a schema revision.

Materialization granularity is independent of outline authority. Consumers may
retain small descendant sections inline in their nearest materialized ancestor,
provided source coverage stays complete and non-overlapping, relative hierarchy
is preserved, and the policy is explicit and reproducible. Such coarsening does
not rewrite the immutable outline or assert that a small section is erroneous.
Repeated reference text is not intrinsically ambiguous when normalized source
spans identify distinct occurrences and their safe output placements correspond.
Repeated heading labels may use unique enclosing-section context; unresolved
targets must not be chosen by first-match ordering.

Materialized explicit relative links retain their source-directory meaning.
An explicit `./` or `../` target must not be silently redirected to a same-named
note through fuzzy filename or alias matching when its intended path is missing.

Producers should distinguish structural alignment from confidence in inferred
boundaries. Conflicting contents-page and authored-title evidence must be
reconciled or reported as unresolved; an inferred page offset is not itself
proof of an individual section boundary. Inferred hierarchy alignment must not
be relabeled as an observed source label. These are v1 semantic clarifications,
not new required fields or a schema revision.

Consumers validate schemas, members, hashes, normalized semantics, and provenance relationships before mutation. Unknown namespaced extensions and native renditions are accepted and ignored. A new extractor requires only a producer adapter that emits the normalized core and declares its native evidence; it never requires a Vulcan code branch or an MDAF version change.

Vulcan imports an artifact into a required vault-relative destination. The output Markdown tree becomes canonical vault content. The artifact itself remains external. Vulcan may materialize normalized source ranges and uniquely resolvable source references, but it never projects opaque native evidence into notes or the rebuildable cache.

Import reports retain validation warnings alongside materialization diagnostics. Root display metadata uses the artifact title when available, with existing canonical frontmatter preserved; the virtual primary filename `text.md` is not the book title. Consumers should surface unusually large root remainders so a structurally valid artifact is not mistaken for a useful hierarchy. Improving hierarchy or metadata creates a new immutable derivative with explicit transformation provenance, not an in-place artifact edit. These clarifications use the existing v1 fields and require no schema version change.
