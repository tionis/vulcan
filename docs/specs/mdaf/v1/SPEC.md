# Markdown Artifact Format (MDAF) version 1

MDAF is an immutable, extractor- and source-format-neutral package for one primary Markdown document and the evidence needed to reinterpret or materialize it later. Inputs may be PDFs, images, audio, video, web pages, ebooks, office documents, structured data, plain text, compound media, or formats not yet anticipated. `Markdown` describes the normalized primary output, not the source. A conforming artifact is either a directory whose name ends in `.mdaf` or a ZIP file whose name ends in `.mdaf`. Both representations expose the same root members and have the same logical identity.

MDAF deliberately does not define OCR, PDF conversion, table extraction, or a universal document-block ontology. Producers normalize only the information consumers share today and retain complete native responses as opaque declared members. Consumers must never select behavior from a producer, tool, model, asset filename, or extension namespace.

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

Member paths use UTF-8, `/` separators, Unicode NFC, and relative POSIX syntax. Empty components, `.`, `..`, absolute paths, backslashes, control characters, Windows drive/UNC prefixes, case-fold-equivalent duplicates, and Unicode-normalization-equivalent duplicates are invalid. Directory readers reject symlinks and special files. ZIP readers reject encrypted members, symlink modes, duplicate normalized paths, more than 100,000 entries, any non-asset member larger than 512 MiB, any asset larger than 2 GiB, total declared or expanded content above 8 GiB, or an expansion ratio above 1,000:1.

Readers validate declared sizes before extraction, stream bytes through bounded readers, and do not write outside an isolated staging directory. ZIP timestamps, compression method, ordering, and permissions do not affect logical identity.

## Manifest and member roles

`info.json` conforms to `info.schema.json`. Version 1 fixes the primary paths to `text.md` and `provenance.json`. Optional normalized sidecars use their fixed root names. Other members are declared with one of these roles:

- `asset`: path below `assets/`;
- `rendition`: path below `renditions/`;
- `source`: path below `sources/`;
- `environment`: path below `environments/`;
- `extension`: path below `extensions/<reverse-domain-namespace>/`.

The primary Markdown media type is `text/markdown`. `markdown.variant` and `markdown.features` describe syntax without changing the MDAF contract. Sources have stable artifact-local IDs, media types, canonical BLAKE3 digests, optional alternate algorithm-tagged digests, and optional embedded member paths. Alternate digests preserve upstream identities without weakening or replacing the canonical digest. Portable core fields must not contain credentials, signed URLs, authorization headers, or absolute local paths.

## Logical identity

The logical artifact identity is independent of directory versus ZIP serialization. MDAF v1 uses the default 256-bit BLAKE3 output for all canonical digests. Digest values are lowercase hexadecimal prefixed by `blake3:`. For every regular member including `info.json`, compute its canonical digest. Sort records by normalized UTF-8 path bytes. Serialize each record as compact JSON with keys in this exact order and a trailing LF:

```json
{"path":"text.md","size":123,"digest":"blake3:<64 lowercase hex>"}
```

The artifact identity is the canonical BLAKE3 digest of the concatenated UTF-8 records. Strings use JSON escaping with no ASCII-only conversion. The specification fixtures provide a test vector. `info.json.derived_from` contains canonical logical identities of immutable parents; it is lineage, not an instruction to fetch them. A derivative remains self-contained and carries forward evidence needed for future processing.

## Normalized source map

`source-map.json` conforms to `source-map.schema.json` and binds to the canonical digest of `text.md`. All document ranges are zero-based, half-open UTF-8 byte ranges whose endpoints are character boundaries.

A mapping connects a Markdown span to a source locator and may carry confidence and a namespaced method. A reference connects authored Markdown text to a target locator. Mappings may overlap, may be partial, and may repeat the same Markdown span for multiple sources. Producers decide which inferred records are reliable enough to publish; consumers preserve confidence and method but do not rerun extraction.

A locator names exactly one declared source and contains an ordered list of selectors. An empty selector list denotes the complete source. Otherwise selectors are conjunctive refinements: an `interval` selecting page 12 followed by a `rectangle` selects that rectangle on page 12. Order records the natural outside-in refinement and is preserved, but does not change the selected segment. Half-open ranges include their start and exclude their end.

MDAF v1 defines these normalized selectors:

- `interval`: an ordered numeric range with an open unit such as `byte`, `unicode-scalar`, `line`, `page`, `slide`, `frame`, `sample`, `millisecond`, or `second`; optional origin and display labels preserve numbering conventions without changing the numeric range;
- `rectangle`: an `x`, `y`, `width`, and `height` region in an open unit; `pixel`, `percent`, and `normalized` have their ordinary top-left-origin media meaning, with percent bounded by 100 and normalized values bounded by 1;
- `polygon`: three or more non-degenerate points in an open spatial unit, for regions that a rectangle cannot represent accurately;
- `grid`: zero-based, half-open row and column ranges plus an optional sheet name, for spreadsheets, tables, matrices, and similar media;
- `text-quote`: exact text with optional prefix and suffix context, providing a content-stable complement to positional intervals;
- `fragment`: a media-defined fragment value and optional public `conforms_to` specification identifier, for HTML IDs, EPUB CFI, CSV fragments, track identifiers, or another established addressing scheme;
- `extension`: reverse-domain-namespaced opaque JSON for a selector that cannot be represented without loss in the normalized core.

Numbers must be finite. Intervals, rectangles, grids, and polygons must be non-empty. Consumers validate normalized selectors but never infer their meaning from a source media type. Unknown future source formats therefore require neither a new MDAF version nor a Vulcan code branch; they use the closest lossless normalized selectors and retain any richer native locator in an extension or rendition.

Source-reference resolution is conservative. A target selector must be matched by a compatible mapping selector for the same declared source; all target selectors must overlap or identify the same segment. Ambiguous or unsupported matches remain authored Markdown and produce a diagnostic rather than an inferred link.

## Alternative outline

`outline.json` conforms to `outline.schema.json` and binds to `text.md`. Nodes form one ordered forest with stable IDs, parent IDs, levels, titles, heading spans, section spans, and optional source locators. Section spans must be ordered and either disjoint or properly nested; a heading span lies inside its section. Selecting the outline as import authority requires complete valid alignment. Markdown headings remain the default authority, and consumers never merge authorities silently.

## Native evidence and extensions

Complete extractor responses belong below `renditions/<namespace>/` and are declared with their real media types and schemas when known. They may contain provider-native block trees, bounding boxes, polygons, masks, timestamps, tracks, frames, page Markdown, tables, hyperlinks, DOM trees, or binary databases. MDAF does not rewrite or interpret them.

Native responses are retained byte-for-byte after mandatory secret filtering. A redaction creates a provenance record naming the field location, reason, and original-field digest when safe to compute. Assets may use arbitrary names; only declared roles and Markdown-relative references carry meaning.

## Provenance

`provenance.json` conforms to `provenance.schema.json`. It is an activity DAG. Every generated member names one producing activity. Each activity records inputs, outputs, dependencies, sanitized output-affecting parameters and their digest, and every directly participating transformation tool and model.

Tools require name and version; build revisions are included when available. Models record provider, identifier, returned identifier, and revision or checksum when exposed. A mutable or unresolved model alias is explicitly marked and produces a reproducibility warning, never invented provenance. Full dependency locks, runtime descriptions, hardware inventories, SPDX documents, or CycloneDX documents are optional environment members.

`parameters_digest` is the canonical BLAKE3 digest of `parameters` serialized as compact UTF-8 JSON: object keys are sorted recursively by Unicode scalar value, arrays retain their order, strings use normal JSON escaping without ASCII-only conversion, numbers use their JSON lexical representation, and no whitespace or trailing newline is emitted. Producers should prefer strings for values whose numeric lexical form is itself significant.

Transport secrets, credentials, signed URLs, and private endpoint topology are forbidden. Unknown exact versions or revisions remain explicit `unavailable` values with diagnostics.

## Consumer behavior

Consumers validate schemas, members, hashes, normalized semantics, and provenance relationships before mutation. Unknown namespaced extensions and native renditions are accepted and ignored. A new extractor requires only a producer adapter that emits the normalized core and declares its native evidence; it never requires a Vulcan code branch or an MDAF version change.

Vulcan imports an artifact into a required vault-relative destination. The output Markdown tree becomes canonical vault content. The artifact itself remains external. Vulcan may materialize normalized source ranges and uniquely resolvable source references, but it never projects opaque native evidence into notes or the rebuildable cache.
